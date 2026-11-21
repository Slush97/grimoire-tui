//! The user's installed mods, as they exist on disk right now.

pub(crate) mod mutate;
mod scan;

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use crate::game::{DISABLED_DIR, PRIORITY_DIR};
use crate::metadata::ModMetadata;
use crate::{Error, Result};

pub use scan::scan;

/// Slots per root: `pak01` to `pak99`.
pub(crate) const MAX_SLOT: u32 = 99;

/// Priority slots below this are reserved for Locker-generated VPKs and are
/// never user mods.
pub(crate) const FIRST_USER_PRIORITY_SLOT: u32 = 5;

/// One scan of the game folder.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModList {
    /// Load order: Global mods, then enabled mods by root and slot, then
    /// disabled mods by name.
    pub mods: Vec<InstalledMod>,
    /// Temp-named files left by a reorder that was interrupted. Only an
    /// explicit Repair touches them; a scan never does.
    pub leftovers: Vec<PathBuf>,
}

/// Every enabled non-Global mod, in load order. This is the list a reorder
/// takes, so every page that reorders starts from here.
pub fn enabled_order(mods: &[InstalledMod]) -> Vec<MetaKey> {
    mods.iter()
        .filter(|m| matches!(m.placement, Placement::Enabled { .. }))
        .map(|m| m.key.clone())
        .collect()
}

/// `order` with `key` moved to position `to` (0-based, clamped past the end).
/// `None` if `key` isn't in the order.
pub fn move_to(order: &[MetaKey], key: &MetaKey, to: usize) -> Option<Vec<MetaKey>> {
    let mut moved: Vec<MetaKey> = order.iter().filter(|k| *k != key).cloned().collect();
    if moved.len() == order.len() {
        return None;
    }
    moved.insert(to.min(moved.len()), key.clone());
    Some(moved)
}

/// `order` with `key` moved to just before `before`, so it loads first and
/// wins their conflicts. `None` if either isn't in the order.
pub fn move_before(order: &[MetaKey], key: &MetaKey, before: &MetaKey) -> Option<Vec<MetaKey>> {
    if !order.contains(before) {
        return None;
    }
    let mut moved = order.to_vec();
    let from = moved.iter().position(|k| k == key)?;
    let key = moved.remove(from);
    let to = moved.iter().position(|k| k == before)?;
    moved.insert(to, key);
    Some(moved)
}

/// Stable identity of an installed VPK, and the key into `mod-metadata.json`.
///
/// Mods in `addons/` and `.disabled/` key to their bare filename. Mods in an
/// overflow root key to `addonsN/<file>` and priority mods key to
/// `grimoire/<file>`, because every root has its own `pak01_dir.vpk` and the
/// keys must not collide.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MetaKey(String);

impl MetaKey {
    pub fn for_path(vpk: &Path) -> Self {
        let file = file_name(vpk);
        let parent = vpk
            .parent()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .unwrap_or_default();

        let keyed_by_folder =
            parent.eq_ignore_ascii_case(PRIORITY_DIR) || is_overflow_folder(parent);
        if keyed_by_folder {
            Self(format!("{parent}/{file}"))
        } else {
            Self(file.to_owned())
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for MetaKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where a mod sits, which decides whether and in what order it loads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Placement {
    /// In `grimoire/`: loads before everything else.
    Priority { slot: u32 },
    /// In `addons/` (root 0) or `addonsN/` (root N) as `pakNN_dir.vpk`.
    Enabled { root: u32, slot: u32 },
    /// In `addons/.disabled/`: not loaded.
    Disabled,
}

impl Placement {
    pub fn is_loaded(self) -> bool {
        !matches!(self, Self::Disabled)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledMod {
    pub key: MetaKey,
    pub path: PathBuf,
    pub placement: Placement,
    pub metadata: Option<ModMetadata>,
}

impl InstalledMod {
    /// The GameBanana name when we know it, otherwise the filename without
    /// the `_dir.vpk` suffix.
    pub fn display_name(&self) -> String {
        if let Some(name) = self.metadata.as_ref().and_then(|m| m.mod_name.as_deref()) {
            return name.to_owned();
        }
        let file = file_name(&self.path);
        file.strip_suffix("_dir.vpk").unwrap_or(file).to_owned()
    }

    /// What tells this mod apart from other files of the same GameBanana
    /// submission: the archive folder it came from, else the file's label or
    /// name on GameBanana.
    pub fn variant_name(&self) -> Option<&str> {
        let meta = self.metadata.as_ref()?;
        [
            &meta.variant_label,
            &meta.file_description,
            &meta.source_file_name,
        ]
        .into_iter()
        .flatten()
        .map(|name| name.trim())
        .find(|name| !name.is_empty())
    }

    /// Other installed files of the same GameBanana submission.
    pub fn variants<'a>(
        &'a self,
        mods: &'a [InstalledMod],
    ) -> impl Iterator<Item = &'a InstalledMod> {
        let id = self.metadata.as_ref().and_then(|m| m.game_banana_id);
        mods.iter().filter(move |m| {
            m.key != self.key
                && id.is_some()
                && m.metadata.as_ref().and_then(|m| m.game_banana_id) == id
        })
    }

    /// Identity that survives the mod moving between slots and folders:
    /// `gb:<id>:file:<fileId>`, else `sha256:<hash>` from the metadata. `None`
    /// when neither is recorded; callers that must have one hash the file.
    pub fn stable_id(&self) -> Option<String> {
        let meta = self.metadata.as_ref()?;
        match (meta.game_banana_id, meta.game_banana_file_id) {
            (Some(id), Some(file)) => Some(format!("gb:{id}:file:{file}")),
            _ => meta.sha256.as_ref().map(|hash| format!("sha256:{hash}")),
        }
    }
}

/// A `_dir.vpk` from outside the game folder, like a fresh download, and the
/// metadata to record for it. Chunk files next to it come along.
#[derive(Debug, Clone)]
pub struct Incoming {
    pub vpk: PathBuf,
    pub metadata: ModMetadata,
}

pub(crate) fn file_name(path: &Path) -> &str {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
}

/// Where a reorder temp file came from and where it belongs, parsed back from
/// its name, so a crash needs no other record to finish the job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TempMove {
    pub src_root: u32,
    pub src_slot: u32,
    pub dst_root: u32,
    pub dst_slot: u32,
    /// Chunk suffix (`000` in `pak03_000.vpk`), or `None` for the `_dir.vpk`.
    pub chunk: Option<String>,
}

impl TempMove {
    pub(crate) fn src_file(&self) -> String {
        member_file(self.src_slot, self.chunk.as_deref())
    }

    pub(crate) fn dst_file(&self) -> String {
        member_file(self.dst_slot, self.chunk.as_deref())
    }
}

fn member_file(slot: u32, chunk: Option<&str>) -> String {
    match chunk {
        None => format!("pak{slot:02}_dir.vpk"),
        Some(suffix) => format!("pak{slot:02}_{suffix}.vpk"),
    }
}

/// Reorder temp files never end in `_dir.vpk`, so the scan reports them as
/// leftovers instead of listing them as mods.
pub(crate) const REORDER_TMP_PREFIX: &str = ".grimoire-tmp-";

pub(crate) fn temp_name(m: &TempMove) -> String {
    let base = format!(
        "{REORDER_TMP_PREFIX}{}-{}-{}-{}",
        m.src_root, m.src_slot, m.dst_root, m.dst_slot
    );
    match &m.chunk {
        None => base,
        Some(suffix) => format!("{base}-{suffix}"),
    }
}

/// Parses our own temp names. Anything else under our prefix is none of our
/// business and stays invisible, so a stray file can never wedge the repair.
pub(crate) fn parse_temp_name(file: &str) -> Option<TempMove> {
    let rest = file.strip_prefix(REORDER_TMP_PREFIX)?;
    let mut parts = rest.split('-');
    let nums = [
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    ];
    let tail: Vec<&str> = parts.collect();
    let chunk = if tail.is_empty() {
        None
    } else {
        let joined = tail.join("-");
        (!joined.is_empty()).then_some(joined)
    };
    Some(TempMove {
        src_root: nums[0],
        src_slot: nums[1],
        dst_root: nums[2],
        dst_slot: nums[3],
        chunk,
    })
}

/// Parseable reorder temps in these dirs, sorted. A scan never touches them.
pub(crate) fn temp_files(dirs: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for dir in dirs {
        let entries = match fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(err) => return Err(Error::io(dir)(err)),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if parse_temp_name(file_name(&path)).is_some() {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

fn is_overflow_folder(name: &str) -> bool {
    name != DISABLED_DIR
        && name
            .strip_prefix("addons")
            .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_key_follows_the_legacy_rules() {
        let key = |p: &str| MetaKey::for_path(Path::new(p)).to_string();
        assert_eq!(key("/g/citadel/addons/pak03_dir.vpk"), "pak03_dir.vpk");
        assert_eq!(
            key("/g/citadel/addons/.disabled/cool_dir.vpk"),
            "cool_dir.vpk"
        );
        assert_eq!(
            key("/g/citadel/addons2/pak03_dir.vpk"),
            "addons2/pak03_dir.vpk"
        );
        assert_eq!(
            key("/g/citadel/grimoire/pak07_dir.vpk"),
            "grimoire/pak07_dir.vpk"
        );
    }

    #[test]
    fn display_name_prefers_metadata_then_strips_suffix() {
        let mut installed = InstalledMod {
            key: MetaKey("cool_dir.vpk".into()),
            path: "/x/.disabled/cool_dir.vpk".into(),
            placement: Placement::Disabled,
            metadata: None,
        };
        assert_eq!(installed.display_name(), "cool");

        installed.metadata = Some(ModMetadata {
            mod_name: Some("Cool Skin".into()),
            ..ModMetadata::default()
        });
        assert_eq!(installed.display_name(), "Cool Skin");
    }

    #[test]
    fn stable_id_prefers_gamebanana_then_sha256() {
        let with = |metadata: ModMetadata| InstalledMod {
            key: MetaKey("pak01_dir.vpk".into()),
            path: "/x/addons/pak01_dir.vpk".into(),
            placement: Placement::Enabled { root: 0, slot: 1 },
            metadata: Some(metadata),
        };
        let gb = ModMetadata {
            game_banana_id: Some(7),
            game_banana_file_id: Some(9),
            sha256: Some("ab".into()),
            ..ModMetadata::default()
        };
        assert_eq!(with(gb).stable_id().as_deref(), Some("gb:7:file:9"));

        let hashed = ModMetadata {
            game_banana_id: Some(7),
            sha256: Some("ab".into()),
            ..ModMetadata::default()
        };
        assert_eq!(with(hashed).stable_id().as_deref(), Some("sha256:ab"));
        assert_eq!(with(ModMetadata::default()).stable_id(), None);
    }

    #[test]
    fn temp_names_round_trip_and_reject_strays() {
        let dir_move = TempMove {
            src_root: 0,
            src_slot: 3,
            dst_root: 1,
            dst_slot: 7,
            chunk: None,
        };
        assert_eq!(temp_name(&dir_move), ".grimoire-tmp-0-3-1-7");
        assert_eq!(
            parse_temp_name(&temp_name(&dir_move)),
            Some(dir_move.clone())
        );

        let chunked = TempMove {
            chunk: Some("000".to_owned()),
            ..dir_move
        };
        assert_eq!(parse_temp_name(&temp_name(&chunked)), Some(chunked.clone()));
        assert_eq!(chunked.dst_file(), "pak07_000.vpk");

        assert_eq!(parse_temp_name(".grimoire-tmp-0-3"), None);
        assert_eq!(parse_temp_name(".grimoire-tmp-0-3-1-"), None);
        assert_eq!(parse_temp_name("pak03_dir.vpk"), None);
    }

    #[test]
    fn move_to_repositions_and_clamps() {
        let keys = |names: &[&str]| -> Vec<MetaKey> {
            names.iter().map(|n| MetaKey((*n).to_owned())).collect()
        };
        let order = keys(&["a", "b", "c"]);

        assert_eq!(
            move_to(&order, &MetaKey("c".into()), 0),
            Some(keys(&["c", "a", "b"]))
        );
        assert_eq!(
            move_to(&order, &MetaKey("a".into()), 99),
            Some(keys(&["b", "c", "a"]))
        );
        assert_eq!(move_to(&order, &MetaKey("x".into()), 0), None);
    }

    #[test]
    fn move_before_puts_the_key_right_in_front() {
        let keys = |names: &[&str]| -> Vec<MetaKey> {
            names.iter().map(|n| MetaKey((*n).to_owned())).collect()
        };
        let order = keys(&["a", "b", "c", "d"]);

        assert_eq!(
            move_before(&order, &MetaKey("d".into()), &MetaKey("b".into())),
            Some(keys(&["a", "d", "b", "c"]))
        );
        assert_eq!(
            move_before(&order, &MetaKey("a".into()), &MetaKey("c".into())),
            Some(keys(&["b", "a", "c", "d"]))
        );
        assert_eq!(
            move_before(&order, &MetaKey("x".into()), &MetaKey("c".into())),
            None
        );
    }
}
