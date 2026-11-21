//! Building the installed-mod list from the filesystem.
//!
//! Scanning is strictly read-only. Repairs (name collisions, stale temp files)
//! are separate, explicit operations so that a refresh can never move a file.

use std::fs;
use std::path::Path;

use super::{temp_files, InstalledMod, MetaKey, ModList, Placement, FIRST_USER_PRIORITY_SLOT};
use crate::game::GameInstall;
use crate::metadata::MetadataMap;
use crate::{Error, Result};

/// Every installed mod, in load order: priority mods, then enabled mods by
/// root and slot, then disabled mods by name.
pub fn scan(install: &GameInstall, metadata: &MetadataMap) -> Result<ModList> {
    let mut mods = Vec::new();

    for (path, slot) in pak_files(&install.priority_dir())? {
        if slot >= FIRST_USER_PRIORITY_SLOT {
            mods.push(entry(&path, Placement::Priority { slot }, metadata));
        }
    }

    let roots = install.addon_roots()?;
    for root in &roots {
        for (path, slot) in pak_files(&root.path)? {
            let placement = Placement::Enabled {
                root: root.index,
                slot,
            };
            mods.push(entry(&path, placement, metadata));
        }
    }

    let mut disabled: Vec<_> = dir_vpks(&install.disabled_dir())?
        .into_iter()
        .map(|path| entry(&path, Placement::Disabled, metadata))
        .collect();
    disabled.sort_by_cached_key(|installed| installed.display_name().to_lowercase());

    let mut dirs = vec![install.priority_dir(), install.disabled_dir()];
    dirs.extend(roots.iter().map(|root| root.path.clone()));
    let leftovers = temp_files(&dirs)?;

    mods.sort_by_key(|installed| installed.placement);
    mods.extend(disabled);
    Ok(ModList { mods, leftovers })
}

fn entry(path: &Path, placement: Placement, metadata: &MetadataMap) -> InstalledMod {
    let key = MetaKey::for_path(path);
    InstalledMod {
        metadata: metadata.get(key.as_str()).cloned(),
        key,
        path: path.to_owned(),
        placement,
    }
}

/// `pakNN_dir.vpk` files in `dir` with their slot number.
fn pak_files(dir: &Path) -> Result<Vec<(std::path::PathBuf, u32)>> {
    Ok(dir_vpks(dir)?
        .into_iter()
        .filter_map(|path| {
            let slot = path.file_name()?.to_str().and_then(pak_slot)?;
            Some((path, slot))
        })
        .collect())
}

/// Every `*_dir.vpk` directly inside `dir`. Chunk files (`_000.vpk`) belong to
/// their `_dir.vpk` and are not mods on their own. A missing folder is empty.
fn dir_vpks(dir: &Path) -> Result<Vec<std::path::PathBuf>> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(Error::io(dir)(err)),
    };

    Ok(entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.to_ascii_lowercase().ends_with("_dir.vpk"))
        })
        .collect())
}

/// `pak07_dir.vpk` -> `Some(7)`.
fn pak_slot(file: &str) -> Option<u32> {
    let lower = file.to_ascii_lowercase();
    let digits = lower.strip_prefix("pak")?.strip_suffix("_dir.vpk")?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::ModMetadata;

    fn install_with(files: &[&str]) -> (tempfile::TempDir, GameInstall) {
        let dir = tempfile::tempdir().unwrap();
        let citadel = dir.path().join("game/citadel");
        fs::create_dir_all(citadel.join("addons")).unwrap();
        fs::write(citadel.join("gameinfo.gi"), "").unwrap();
        for file in files {
            let path = citadel.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "").unwrap();
        }
        let install = GameInstall::open(dir.path()).unwrap();
        (dir, install)
    }

    #[test]
    fn pak_slot_parses_only_pak_dir_files() {
        assert_eq!(pak_slot("pak07_dir.vpk"), Some(7));
        assert_eq!(pak_slot("PAK12_DIR.VPK"), Some(12));
        assert_eq!(pak_slot("pak07_000.vpk"), None);
        assert_eq!(pak_slot("cool_dir.vpk"), None);
    }

    #[test]
    fn scan_orders_priority_then_enabled_then_disabled() {
        let (_dir, install) = install_with(&[
            "addons/pak02_dir.vpk",
            "addons/pak01_dir.vpk",
            "addons/pak01_000.vpk",
            "addons2/pak01_dir.vpk",
            "grimoire/pak05_dir.vpk",
            "addons/.disabled/zeta_dir.vpk",
            "addons/.disabled/Alpha_dir.vpk",
        ]);

        let keys: Vec<String> = scan(&install, &MetadataMap::new())
            .unwrap()
            .mods
            .iter()
            .map(|m| m.key.to_string())
            .collect();

        assert_eq!(
            keys,
            [
                "grimoire/pak05_dir.vpk",
                "pak01_dir.vpk",
                "pak02_dir.vpk",
                "addons2/pak01_dir.vpk",
                "Alpha_dir.vpk",
                "zeta_dir.vpk",
            ]
        );
    }

    #[test]
    fn scan_skips_reserved_locker_slots() {
        let (_dir, install) = install_with(&["grimoire/pak01_dir.vpk", "grimoire/pak04_dir.vpk"]);
        assert!(scan(&install, &MetadataMap::new()).unwrap().mods.is_empty());
    }

    #[test]
    fn scan_reports_reorder_temps_as_leftovers_not_mods() {
        let (_dir, install) = install_with(&[
            "addons/pak01_dir.vpk",
            "addons/.grimoire-tmp-0-1-0-2",
            "addons/.grimoire-tmp-junk",
        ]);

        let list = scan(&install, &MetadataMap::new()).unwrap();

        assert_eq!(list.mods.len(), 1);
        assert_eq!(list.leftovers.len(), 1);
        assert!(list.leftovers[0].ends_with(".grimoire-tmp-0-1-0-2"));
    }

    #[test]
    fn scan_attaches_metadata_by_key() {
        let (_dir, install) = install_with(&["addons/pak01_dir.vpk"]);
        let metadata = MetadataMap::from([(
            "pak01_dir.vpk".to_owned(),
            ModMetadata {
                mod_name: Some("Cool Skin".into()),
                ..ModMetadata::default()
            },
        )]);

        let mods = scan(&install, &metadata).unwrap().mods;

        assert_eq!(mods[0].display_name(), "Cool Skin");
    }
}
