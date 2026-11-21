//! Changing which mods load and in what order, per
//! docs/spec/on-disk-format.md. Only `Backend::handle` calls these, from the
//! one worker thread, so two of them never run at once.
//!
//! `metadata` is the path to `mod-metadata.json`. It is loaded fresh right
//! before each write and re-keyed in the same operation whenever a file moves,
//! so the Electron client never loses entries to a stale copy. `journal` is
//! remix's own crash note for reorders; see [`Journal`].

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{
    file_name, parse_temp_name, scan, temp_name, Incoming, InstalledMod, MetaKey, ModList,
    Placement, TempMove, FIRST_USER_PRIORITY_SLOT, MAX_SLOT,
};
use crate::game::{GameInstall, MAX_OVERFLOW_ROOTS};
use crate::metadata::{load_map, MetadataFile, ModMetadata};
use crate::{fsx, gameinfo, Error, Result};

pub(crate) fn enable(install: &GameInstall, metadata: &Path, key: &MetaKey) -> Result<()> {
    let list = fresh_scan(install, metadata)?;
    let target = find(&list, key)?;
    if target.placement.is_loaded() {
        return Err(Error::Refused(format!(
            "{} is already enabled.",
            target.display_name()
        )));
    }
    let from = file_name(&target.path).to_owned();
    let meta = target.metadata.clone().unwrap_or_default();
    let (dir, to) = if meta.priority_mod {
        let dir = install.priority_dir();
        let slot = free_priority_slot(&dir)?.ok_or_else(|| {
            Error::Refused("Global is full. Disable a Global mod first.".to_owned())
        })?;
        (dir, dir_file(slot))
    } else {
        let (root, slot) = normal_destination(install, meta.last_priority, Some(&from))?;
        (root_dir(install, root), dir_file(slot))
    };
    move_family(&install.disabled_dir(), &dir, &from, &to)?;
    rekey(metadata, key, &MetaKey::for_path(&dir.join(&to)))
}

pub(crate) fn disable(install: &GameInstall, metadata: &Path, key: &MetaKey) -> Result<()> {
    let list = fresh_scan(install, metadata)?;
    let target = find(&list, key)?;
    let (slot, src_dir) = match target.placement {
        Placement::Enabled { root, slot } => (slot, root_dir(install, root)),
        Placement::Priority { slot } => (slot, install.priority_dir()),
        Placement::Disabled => {
            return Err(Error::Refused(format!(
                "{} is already disabled.",
                target.display_name()
            )));
        }
    };
    let from = file_name(&target.path).to_owned();
    let meta = target.metadata.clone().unwrap_or_default();
    let to = unique_disabled_name(&install.disabled_dir(), &slug_for(&from, &meta))?;
    move_family(&src_dir, &install.disabled_dir(), &from, &to)?;

    let mut file = MetadataFile::load(metadata)?;
    let to_key = MetaKey::for_path(&install.disabled_dir().join(&to));
    file.rekey(key, &to_key);
    file.set(&to_key, "lastPriority", json!(slot));
    file.save()
}

/// Marks a mod Global (or not) and moves it if it's enabled. A disabled mod
/// only gets `priorityMod` set, and moves to `grimoire/` when enabled.
pub(crate) fn set_global(
    install: &GameInstall,
    metadata: &Path,
    key: &MetaKey,
    global: bool,
) -> Result<()> {
    let list = fresh_scan(install, metadata)?;
    let target = find(&list, key)?;
    let from = file_name(&target.path).to_owned();
    let meta = target.metadata.clone().unwrap_or_default();
    // The destination first: a full Global root refuses before anything moves.
    let action = match (target.placement, global) {
        (Placement::Disabled, _)
        | (Placement::Priority { .. }, true)
        | (Placement::Enabled { .. }, false) => None,
        (Placement::Enabled { root, .. }, true) => {
            let dir = install.priority_dir();
            let slot = free_priority_slot(&dir)?.ok_or_else(|| {
                Error::Refused("Global is full. Disable a Global mod first.".to_owned())
            })?;
            Some((root_dir(install, root), dir, dir_file(slot)))
        }
        (Placement::Priority { .. }, false) => {
            let (root, slot) = normal_destination(install, meta.last_priority, None)?;
            Some((
                install.priority_dir(),
                root_dir(install, root),
                dir_file(slot),
            ))
        }
    };

    let mut file = MetadataFile::load(metadata)?;
    file.set(key, "priorityMod", json!(global));
    if let Some((src_dir, dir, to)) = action {
        move_family(&src_dir, &dir, &from, &to)?;
        file.rekey(key, &MetaKey::for_path(&dir.join(&to)));
    }
    file.save()
}

/// Switches from one variant of a mod to another: `off` is disabled and `on`
/// takes the slot it freed, or goes Global if `off` was, so the mod keeps its
/// place in the load order.
pub(crate) fn swap(
    install: &GameInstall,
    metadata: &Path,
    off: &MetaKey,
    on: &MetaKey,
) -> Result<()> {
    let list = fresh_scan(install, metadata)?;
    let placement = find(&list, off)?.placement;
    find(&list, on)?;
    disable(install, metadata, off)?;
    let mut file = MetadataFile::load(metadata)?;
    match placement {
        Placement::Enabled { root: 0, slot } => file.set(on, "lastPriority", json!(slot)),
        Placement::Priority { .. } => file.set(on, "priorityMod", json!(true)),
        _ => {}
    }
    file.save()?;
    enable(install, metadata, on)
}

/// `order` must be exactly the enabled non-Global mods on disk, in their new
/// order. Anything else means the caller's snapshot is stale.
pub(crate) fn reorder(
    install: &GameInstall,
    metadata: &Path,
    journal: &Path,
    order: &[MetaKey],
) -> Result<()> {
    let list = fresh_scan(install, metadata)?;
    let current = super::enabled_order(&list.mods);
    if order.len() != current.len()
        || order.iter().collect::<HashSet<_>>() != current.iter().collect::<HashSet<_>>()
    {
        return Err(stale_order());
    }
    let by_key: HashMap<&MetaKey, &InstalledMod> = list.mods.iter().map(|m| (&m.key, m)).collect();
    let mut moves = Vec::new();
    for (i, key) in order.iter().enumerate() {
        let at = u32::try_from(i).map_err(|_| stale_order())?;
        let want = (at / MAX_SLOT, at % MAX_SLOT + 1);
        let Some(target) = by_key.get(key) else {
            return Err(stale_order());
        };
        let Placement::Enabled { root, slot } = target.placement else {
            return Err(stale_order());
        };
        if (root, slot) != want {
            moves.push(ReorderMove {
                key: key.clone(),
                from_name: file_name(&target.path).to_owned(),
                have: (root, slot),
                want,
            });
        }
    }
    if moves.is_empty() {
        return Ok(());
    }

    // New overflow roots exist, and gameinfo knows them, before any VPK moves.
    let mut existing: BTreeSet<u32> = install
        .addon_roots()?
        .iter()
        .map(|root| root.index)
        .collect();
    let mut created = Vec::new();
    for m in &moves {
        if !existing.contains(&m.want.0) {
            let dir = root_dir(install, m.want.0);
            fs::create_dir_all(&dir).map_err(Error::io(&dir))?;
            created.push(dir);
            existing.insert(m.want.0);
        }
    }
    if !created.is_empty() {
        if let Err(err) = gameinfo::repair(install) {
            for dir in &created {
                let _ = fs::remove_dir(dir);
            }
            return Err(err);
        }
    }

    let plan = Journal::for_moves(install, &moves)?;
    fsx::write_json(journal, &plan)?;
    for file in &plan.files {
        fs::rename(&file.from, &file.tmp).map_err(Error::io(&file.from))?;
    }
    for file in &plan.files {
        fs::rename(&file.tmp, &file.to).map_err(Error::io(&file.tmp))?;
    }
    let mut file = MetadataFile::load(metadata)?;
    let targets: Vec<MetaKey> = moves
        .iter()
        .map(|m| MetaKey::for_path(&root_dir(install, m.want.0).join(dir_file(m.want.1))))
        .collect();
    let pairs: Vec<(&MetaKey, &MetaKey)> = moves
        .iter()
        .zip(&targets)
        .map(|(m, to)| (&m.key, to))
        .collect();
    file.rekey_all(&pairs);
    file.save()?;
    match fs::remove_file(journal) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(Error::io(journal)(err)),
    }
}

pub(crate) fn delete(install: &GameInstall, metadata: &Path, keys: &[MetaKey]) -> Result<()> {
    if keys.is_empty() {
        return Ok(());
    }
    // No leftovers guard: deleting removes files, it never moves them.
    let list = scan(install, &load_map(metadata)?)?;
    let mut targets = Vec::with_capacity(keys.len());
    for key in keys {
        targets.push(find(&list, key)?);
    }
    for target in &targets {
        let dir = match target.placement {
            Placement::Enabled { root, .. } => root_dir(install, root),
            Placement::Priority { .. } => install.priority_dir(),
            Placement::Disabled => install.disabled_dir(),
        };
        for member in family(&dir, file_name(&target.path))? {
            match fs::remove_file(&member.path) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(Error::io(&member.path)(err)),
            }
        }
    }
    let mut file = MetadataFile::load(metadata)?;
    for key in keys {
        file.remove(key);
    }
    file.save()
}

/// Copies VPKs from outside the game folder into `.disabled/`, the way the
/// Electron client installs downloads, then enables them if asked.
pub(crate) fn install(
    install: &GameInstall,
    metadata: &Path,
    incoming: &[Incoming],
    enable_after: bool,
) -> Result<()> {
    fresh_scan(install, metadata)?;
    let disabled = install.disabled_dir();
    fs::create_dir_all(&disabled).map_err(Error::io(&disabled))?;
    let mut keys = Vec::with_capacity(incoming.len());
    for new in incoming {
        let to = unique_disabled_name(&disabled, &slug_for(file_name(&new.vpk), &new.metadata))?;
        copy_family(&new.vpk, &disabled, &to)?;
        let key = MetaKey::for_path(&disabled.join(&to));
        let mut file = MetadataFile::load(metadata)?;
        // Anything already at this key described a file that's gone.
        file.remove(&key);
        file.set_known(&key, &new.metadata);
        file.save()?;
        keys.push(key);
    }
    if enable_after {
        for key in &keys {
            enable(install, metadata, key)?;
        }
    }
    Ok(())
}

/// Copies a `_dir.vpk` and its chunks into `dst_dir` as `to`. Each file lands
/// under a `.part` name first and chunks go before the `_dir.vpk`, so a crash
/// never leaves a half-written mod that a scan would list.
fn copy_family(vpk: &Path, dst_dir: &Path, to: &str) -> Result<()> {
    let members = family(vpk.parent().unwrap_or(Path::new("")), file_name(vpk))?;
    if members.is_empty() {
        return Err(Error::io(vpk)(std::io::ErrorKind::NotFound.into()));
    }
    for member in members.iter().rev() {
        let name = match &member.chunk_suffix {
            None => to.to_owned(),
            Some(suffix) => format!("{}_{suffix}.vpk", dir_stem(to)),
        };
        let part = dst_dir.join(format!("{name}.part"));
        fs::copy(&member.path, &part).map_err(Error::io(&member.path))?;
        fs::rename(&part, dst_dir.join(&name)).map_err(Error::io(&part))?;
    }
    Ok(())
}

/// Finishes what an interrupted reorder left in [`ModList::leftovers`]. The
/// journal has the full plan; without it the temp names do, unless a file sits
/// where a leftover belongs, which only losing the journal mid-reorder causes.
pub(crate) fn repair(install: &GameInstall, metadata: &Path, journal: &Path) -> Result<()> {
    let plan: Option<Journal> = match fsx::read_json(journal) {
        Ok(plan) => plan,
        // Our own note is corrupt: the temp names still say where everything
        // belongs, so fall back to them instead of stranding the user.
        Err(Error::Json { .. }) => None,
        Err(err) => return Err(err),
    };
    if let Some(plan) = plan {
        complete_journal(metadata, journal, &plan)
    } else {
        complete_temps(install, metadata)
    }
}

fn complete_journal(metadata: &Path, journal: &Path, plan: &Journal) -> Result<()> {
    for file in &plan.files {
        if !file.from.exists() {
            continue;
        }
        if file.tmp.exists() {
            return Err(conflict(&file.tmp));
        }
        fs::rename(&file.from, &file.tmp).map_err(Error::io(&file.from))?;
    }
    for file in &plan.files {
        if !file.tmp.exists() {
            continue;
        }
        if file.to.exists() {
            return Err(conflict(&file.to));
        }
        fs::rename(&file.tmp, &file.to).map_err(Error::io(&file.tmp))?;
    }
    let mut file = MetadataFile::load(metadata)?;
    let rekeys = plan.rekeys();
    let pairs: Vec<(&MetaKey, &MetaKey)> = rekeys.iter().map(|(from, to)| (from, to)).collect();
    file.rekey_all(&pairs);
    file.save()?;
    match fs::remove_file(journal) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(Error::io(journal)(err)),
    }
}

fn complete_temps(install: &GameInstall, metadata: &Path) -> Result<()> {
    let list = scan(install, &load_map(metadata)?)?;
    if list.leftovers.is_empty() {
        return Ok(());
    }
    let mut temps = Vec::with_capacity(list.leftovers.len());
    for path in &list.leftovers {
        let Some(m) = parse_temp_name(file_name(path)) else {
            continue;
        };
        temps.push((path, m));
    }
    for (_, m) in &temps {
        let to = root_dir(install, m.dst_root).join(m.dst_file());
        if to.exists() {
            return Err(Error::Refused(
                "One leftover file's slot is taken and the reorder note is gone, so Grimoire can't place it. Rename each .grimoire-tmp file to the pak slot in its name, then rescan.".to_owned(),
            ));
        }
    }
    for (path, m) in &temps {
        let to = root_dir(install, m.dst_root).join(m.dst_file());
        fs::rename(path, &to).map_err(Error::io(path))?;
    }
    let mut file = MetadataFile::load(metadata)?;
    let keys: Vec<(MetaKey, MetaKey)> = temps
        .iter()
        .filter(|(_, m)| m.chunk.is_none())
        .map(|(_, m)| {
            (
                MetaKey::for_path(&root_dir(install, m.src_root).join(m.src_file())),
                MetaKey::for_path(&root_dir(install, m.dst_root).join(m.dst_file())),
            )
        })
        .collect();
    let pairs: Vec<(&MetaKey, &MetaKey)> = keys.iter().map(|(from, to)| (from, to)).collect();
    file.rekey_all(&pairs);
    file.save()
}

fn conflict(path: &Path) -> Error {
    Error::Refused(format!(
        "Repair found an unexpected file: {}. Move it out of the way and try again.",
        path.display()
    ))
}

/// The reorder plan, written to remix's own config dir before the first rename
/// and deleted after the metadata lands. Never in the game folder, so the
/// Electron client never sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Journal {
    files: Vec<JournalMove>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct JournalMove {
    from: PathBuf,
    tmp: PathBuf,
    to: PathBuf,
}

struct ReorderMove {
    key: MetaKey,
    /// The file's actual name on disk, which may not be canonical (`PAK7`).
    from_name: String,
    have: (u32, u32),
    want: (u32, u32),
}

impl Journal {
    fn for_moves(install: &GameInstall, moves: &[ReorderMove]) -> Result<Self> {
        let mut files = Vec::new();
        for m in moves {
            let from_dir = root_dir(install, m.have.0);
            let to_dir = root_dir(install, m.want.0);
            for member in family(&from_dir, &m.from_name)? {
                let tmp = from_dir.join(temp_name(&TempMove {
                    src_root: m.have.0,
                    src_slot: m.have.1,
                    dst_root: m.want.0,
                    dst_slot: m.want.1,
                    chunk: member.chunk_suffix.clone(),
                }));
                let to = match &member.chunk_suffix {
                    None => to_dir.join(dir_file(m.want.1)),
                    Some(suffix) => to_dir.join(format!("{}_{suffix}.vpk", pak_stem(m.want.1))),
                };
                files.push(JournalMove {
                    from: member.path,
                    tmp,
                    to,
                });
            }
        }
        Ok(Self { files })
    }

    /// Old and new meta keys for every moved `_dir.vpk`. Chunks have no entries.
    fn rekeys(&self) -> Vec<(MetaKey, MetaKey)> {
        self.files
            .iter()
            .filter(|file| {
                file_name(&file.from)
                    .to_ascii_lowercase()
                    .ends_with("_dir.vpk")
            })
            .map(|file| (MetaKey::for_path(&file.from), MetaKey::for_path(&file.to)))
            .collect()
    }
}

/// What the disk looks like right now. Refuses when an interrupted reorder
/// left temp files behind: those resolve first, so nothing builds on a
/// half-moved library.
fn fresh_scan(install: &GameInstall, metadata: &Path) -> Result<ModList> {
    let list = scan(install, &load_map(metadata)?)?;
    if !list.leftovers.is_empty() {
        return Err(Error::Refused(
            "An interrupted reorder left files behind. Press Repair first.".to_owned(),
        ));
    }
    Ok(list)
}

fn stale_order() -> Error {
    Error::Refused("The mod list changed. The new list is already on screen.".to_owned())
}

fn find(list: &ModList, key: &MetaKey) -> Result<InstalledMod> {
    list.mods
        .iter()
        .find(|m| &m.key == key)
        .cloned()
        .ok_or_else(|| {
            Error::Refused(
                "That mod moved since the list was made. The new list is already on screen."
                    .to_owned(),
            )
        })
}

fn rekey(metadata: &Path, from: &MetaKey, to: &MetaKey) -> Result<()> {
    let mut file = MetadataFile::load(metadata)?;
    file.rekey(from, to);
    file.save()
}

/// Where an enabled normal mod goes: its remembered slot, the lowest free base
/// slot, an existing overflow root, or a new overflow root, in that order.
fn normal_destination(
    install: &GameInstall,
    last_priority: Option<u32>,
    exclude: Option<&str>,
) -> Result<(u32, u32)> {
    let mut taken = taken_slots(&root_dir(install, 0), None)?;
    // Legacy pak-named files in `.disabled/` hold their base slots, so an
    // enable can't take a number a disabled mod still wears. The file being
    // enabled is leaving, so its own number frees up.
    taken.extend(taken_slots(&install.disabled_dir(), exclude)?);
    if let Some(slot) = last_priority {
        if (1..=MAX_SLOT).contains(&slot) && !taken.contains(&slot) {
            return Ok((0, slot));
        }
    }
    if let Some(slot) = (1..=MAX_SLOT).find(|slot| !taken.contains(slot)) {
        return Ok((0, slot));
    }
    for root in install.addon_roots()? {
        if root.index == 0 {
            continue;
        }
        let taken = taken_slots(&root.path, None)?;
        if let Some(slot) = (1..=MAX_SLOT).find(|slot| !taken.contains(slot)) {
            return Ok((root.index, slot));
        }
    }
    let existing: BTreeSet<u32> = install
        .addon_roots()?
        .iter()
        .map(|root| root.index)
        .collect();
    let index = (1..=MAX_OVERFLOW_ROOTS)
        .find(|index| !existing.contains(index))
        .ok_or_else(|| {
            Error::Refused(
                "Every mod folder is full. There is nowhere left to put this mod.".to_owned(),
            )
        })?;
    let dir = root_dir(install, index);
    fs::create_dir_all(&dir).map_err(Error::io(&dir))?;
    // The gameinfo line lands before the VPK does, so the engine never sees a
    // root it doesn't know about.
    if let Err(err) = gameinfo::repair(install) {
        let _ = fs::remove_dir(&dir);
        return Err(err);
    }
    Ok((index, 1))
}

fn free_priority_slot(dir: &Path) -> Result<Option<u32>> {
    let taken = taken_slots(dir, None)?;
    Ok((FIRST_USER_PRIORITY_SLOT..=MAX_SLOT).find(|slot| !taken.contains(slot)))
}

/// Slots taken in `dir`: any file named `pakNN_*`, chunks included. `except`
/// skips one file by name, for a file that is about to leave.
fn taken_slots(dir: &Path, except: Option<&str>) -> Result<BTreeSet<u32>> {
    let mut taken = BTreeSet::new();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(taken),
        Err(err) => return Err(Error::io(dir)(err)),
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if Some(name) == except {
            continue;
        }
        if let Some(slot) = pak_prefix_slot(name) {
            taken.insert(slot);
        }
    }
    Ok(taken)
}

/// `pak07_anything` to `Some(7)`. Case-insensitive; needs digits and then `_`.
fn pak_prefix_slot(file: &str) -> Option<u32> {
    let rest = file.to_ascii_lowercase();
    let rest = rest.strip_prefix("pak")?;
    let digits = rest.split('_').next()?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

fn root_dir(install: &GameInstall, index: u32) -> PathBuf {
    if index == 0 {
        install.citadel().join("addons")
    } else {
        install.citadel().join(format!("addons{index}"))
    }
}

fn dir_file(slot: u32) -> String {
    format!("pak{slot:02}_dir.vpk")
}

fn pak_stem(slot: u32) -> String {
    format!("pak{slot:02}")
}

/// `cool_dir.vpk` to `cool`. Callers only pass names the scan matched.
fn dir_stem(name: &str) -> &str {
    &name[..name.len() - "_dir.vpk".len()]
}

/// A `_dir.vpk` with the chunk files that move with it.
struct FamilyMember {
    path: PathBuf,
    /// `None` for the `_dir.vpk` itself, else the chunk suffix: `000` in
    /// `pak03_000.vpk`.
    chunk_suffix: Option<String>,
}

/// The `_dir.vpk` plus its `<stem>_*.vpk` siblings. Other `_dir.vpk` files are
/// never anyone's chunk, however their names start.
fn family(dir: &Path, dir_name: &str) -> Result<Vec<FamilyMember>> {
    let prefix = format!("{}_", dir_stem(dir_name));
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(Error::io(dir)(err)),
    };
    let mut members = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = file_name(&path);
        if name == dir_name {
            members.push(FamilyMember {
                path,
                chunk_suffix: None,
            });
        } else if let Some(suffix) = chunk_suffix(name, &prefix) {
            members.push(FamilyMember {
                path,
                chunk_suffix: Some(suffix),
            });
        }
    }
    members.sort_by(|a, b| a.chunk_suffix.cmp(&b.chunk_suffix));
    Ok(members)
}

/// The chunk suffix when `name` is one of `prefix`'s chunks: it starts with
/// the prefix, ends with `.vpk`, and isn't a `_dir.vpk` of its own. Degenerate
/// names with nothing between stay put.
fn chunk_suffix(name: &str, prefix: &str) -> Option<String> {
    if !Path::new(name)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("vpk"))
    {
        return None;
    }
    if name.to_ascii_lowercase().ends_with("_dir.vpk") {
        return None;
    }
    let stem = name.get(..prefix.len())?;
    if !stem.eq_ignore_ascii_case(prefix) {
        return None;
    }
    let suffix = name.get(prefix.len()..name.len() - ".vpk".len())?;
    (!suffix.is_empty()).then(|| suffix.to_owned())
}

fn move_family(src_dir: &Path, dst_dir: &Path, from: &str, to: &str) -> Result<()> {
    fs::create_dir_all(dst_dir).map_err(Error::io(dst_dir))?;
    for member in family(src_dir, from)? {
        let name = match &member.chunk_suffix {
            None => to.to_owned(),
            Some(suffix) => format!("{}_{suffix}.vpk", dir_stem(to)),
        };
        fs::rename(&member.path, dst_dir.join(&name)).map_err(Error::io(&member.path))?;
    }
    Ok(())
}

/// The `.disabled/` name for a mod: its own stem without any `pakNN_` prefix,
/// else the mod name, source file name, or variant label, else `mod`.
fn slug_for(file: &str, meta: &ModMetadata) -> String {
    let base = [
        Some(strip_pak_prefix(dir_stem(file))),
        meta.mod_name.as_deref(),
        meta.source_file_name.as_deref(),
        meta.variant_label.as_deref(),
    ]
    .into_iter()
    .flatten()
    .map(str::trim)
    .find(|base| !base.is_empty())
    .unwrap_or("mod");
    slugify(base)
}

/// `pak13_cool` to `cool`, and a bare `pak13` to nothing: a plain pak file has
/// no slug of its own, so the metadata fallbacks apply.
fn strip_pak_prefix(stem: &str) -> &str {
    let bytes = stem.as_bytes();
    if bytes.len() > 4 && bytes[..3].eq_ignore_ascii_case(b"pak") {
        let digits = bytes[3..].iter().take_while(|b| b.is_ascii_digit()).count();
        if digits > 0 {
            let rest = &stem[3 + digits..];
            return rest.strip_prefix('_').unwrap_or(rest);
        }
    }
    stem
}

fn slugify(base: &str) -> String {
    let mut slug = String::with_capacity(base.len());
    let mut separator = true;
    for c in base.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
            separator = false;
        } else if !separator {
            slug.push('_');
            separator = true;
        }
    }
    if separator {
        slug.pop();
    }
    slug.truncate(48);
    if slug
        .as_bytes()
        .get(..4)
        .is_some_and(|lead| lead[..3].eq_ignore_ascii_case(b"pak") && lead[3].is_ascii_digit())
    {
        slug.insert_str(0, "mod_");
    }
    if slug.is_empty() {
        slug.push_str("mod");
    }
    slug
}

/// `slug_dir.vpk`, or `slug_XXXX_dir.vpk` when taken. Names compare
/// case-insensitively, and the suffix counts up in hex until one is free.
fn unique_disabled_name(dir: &Path, slug: &str) -> Result<String> {
    let mut taken = HashSet::new();
    match fs::read_dir(dir) {
        Ok(entries) => {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    taken.insert(name.to_ascii_lowercase());
                }
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(Error::io(dir)(err)),
    }
    let mut candidate = format!("{slug}_dir.vpk");
    let mut n = 1;
    loop {
        if !taken.contains(&candidate) {
            return Ok(candidate);
        }
        candidate = format!("{slug}_{n:04x}_dir.vpk");
        n += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        _dir: tempfile::TempDir,
        install: GameInstall,
        metadata: PathBuf,
        journal: PathBuf,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let citadel = dir.path().join("game/citadel");
        fs::create_dir_all(citadel.join("addons/.disabled")).unwrap();
        fs::write(
            citadel.join("gameinfo.gi"),
            "\"GameInfo\"\n{\n\tFileSystem\n\t{\n\t\tSearchPaths\n\t\t{\n\t\t\tGame citadel/grimoire\n\t\t\tGame citadel/addons\n\t\t}\n\t}\n}\n",
        )
        .unwrap();
        Fixture {
            install: GameInstall::open(dir.path()).unwrap(),
            metadata: dir.path().join("config/mod-metadata.json"),
            journal: dir.path().join("config/reorder-journal.json"),
            _dir: dir,
        }
    }

    fn key(name: &str) -> MetaKey {
        MetaKey::for_path(Path::new(name))
    }

    impl Fixture {
        fn write(&self, rel: &str) {
            let path = self.install.citadel().join(rel);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "vpk").unwrap();
        }

        fn has(&self, rel: &str) -> bool {
            self.install.citadel().join(rel).exists()
        }

        fn mods(&self) -> ModList {
            scan(&self.install, &load_map(&self.metadata).unwrap()).unwrap()
        }

        fn entry(&self, key: &str) -> serde_json::Value {
            let raw: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&self.metadata).unwrap()).unwrap();
            raw[key].clone()
        }
    }

    #[test]
    fn enable_restores_the_remembered_slot_then_lowest_free() {
        let fx = fixture();
        fx.write("addons/pak01_dir.vpk");
        fx.write("addons/.disabled/cool_dir.vpk");
        fx.write("addons/.disabled/other_dir.vpk");
        fs::create_dir_all(fx.metadata.parent().unwrap()).unwrap();
        fs::write(
            &fx.metadata,
            r#"{"cool_dir.vpk":{"modName":"Cool","lastPriority":3},"other_dir.vpk":{"lastPriority":1}}"#,
        )
        .unwrap();

        enable(&fx.install, &fx.metadata, &key("cool_dir.vpk")).unwrap();
        enable(&fx.install, &fx.metadata, &key("other_dir.vpk")).unwrap();

        assert!(fx.has("addons/pak03_dir.vpk"));
        assert!(fx.has("addons/pak02_dir.vpk"));
        assert_eq!(fx.entry("pak03_dir.vpk")["modName"], "Cool");
        assert!(fx.entry("cool_dir.vpk").is_null());
    }

    #[test]
    fn enable_skips_legacy_pak_names_but_frees_its_own() {
        let fx = fixture();
        fx.write("addons/.disabled/pak02_dir.vpk");
        fx.write("addons/.disabled/enabling_dir.vpk");

        enable(&fx.install, &fx.metadata, &key("enabling_dir.vpk")).unwrap();
        assert!(fx.has("addons/pak01_dir.vpk"));

        enable(&fx.install, &fx.metadata, &key("pak02_dir.vpk")).unwrap();
        assert!(fx.has("addons/pak02_dir.vpk"));
    }

    #[test]
    fn enable_spills_into_overflow_and_then_a_new_root() {
        let fx = fixture();
        for slot in 1..=99 {
            fx.write(&format!("addons/pak{slot:02}_dir.vpk"));
        }
        fx.write("addons1/pak01_dir.vpk");
        fx.write("addons/.disabled/a_dir.vpk");
        fx.write("addons/.disabled/b_dir.vpk");

        enable(&fx.install, &fx.metadata, &key("a_dir.vpk")).unwrap();
        assert!(fx.has("addons1/pak02_dir.vpk"));

        for slot in 3..=99 {
            fx.write(&format!("addons1/pak{slot:02}_dir.vpk"));
        }
        enable(&fx.install, &fx.metadata, &key("b_dir.vpk")).unwrap();
        assert!(fx.has("addons2/pak01_dir.vpk"));
        let gameinfo = fs::read_to_string(fx.install.gameinfo()).unwrap();
        assert!(gameinfo.contains("citadel/addons2"));
        assert_eq!(fx.mods().mods.len(), 99 + 99 + 1);
    }

    #[test]
    fn enable_moves_chunks_and_refuses_when_loaded_or_gone() {
        let fx = fixture();
        fx.write("addons/pak01_dir.vpk");
        fx.write("addons/.disabled/cool_dir.vpk");
        fx.write("addons/.disabled/cool_000.vpk");

        enable(&fx.install, &fx.metadata, &key("cool_dir.vpk")).unwrap();
        assert!(fx.has("addons/pak02_dir.vpk"));
        assert!(fx.has("addons/pak02_000.vpk"));

        assert!(matches!(
            enable(&fx.install, &fx.metadata, &key("pak02_dir.vpk")),
            Err(Error::Refused(_))
        ));
        assert!(matches!(
            enable(&fx.install, &fx.metadata, &key("gone_dir.vpk")),
            Err(Error::Refused(_))
        ));
    }

    #[test]
    fn enable_sends_priority_mods_to_grimoire() {
        let fx = fixture();
        fx.write("grimoire/pak05_dir.vpk");
        fx.write("addons/.disabled/cool_dir.vpk");
        fx.write("addons/.disabled/full_dir.vpk");
        fs::create_dir_all(fx.metadata.parent().unwrap()).unwrap();
        fs::write(
            &fx.metadata,
            r#"{"cool_dir.vpk":{"priorityMod":true},"full_dir.vpk":{"priorityMod":true}}"#,
        )
        .unwrap();

        enable(&fx.install, &fx.metadata, &key("cool_dir.vpk")).unwrap();
        assert!(fx.has("grimoire/pak06_dir.vpk"));
        assert_eq!(fx.entry("grimoire/pak06_dir.vpk")["priorityMod"], true);

        for slot in 7..=99 {
            fx.write(&format!("grimoire/pak{slot:02}_dir.vpk"));
        }
        assert!(matches!(
            enable(&fx.install, &fx.metadata, &key("full_dir.vpk")),
            Err(Error::Refused(_))
        ));
        assert!(fx.has("addons/.disabled/full_dir.vpk"));
    }

    #[test]
    fn disable_slugifies_and_remembers_the_slot() {
        let fx = fixture();
        fx.write("addons/pak03_dir.vpk");
        fx.write("addons/pak03_000.vpk");
        fs::create_dir_all(fx.metadata.parent().unwrap()).unwrap();
        fs::write(
            &fx.metadata,
            r#"{"pak03_dir.vpk":{"modName":"Cool Skin (No Tail!)","imprinted":true}}"#,
        )
        .unwrap();

        disable(&fx.install, &fx.metadata, &key("pak03_dir.vpk")).unwrap();

        assert!(fx.has("addons/.disabled/cool_skin_no_tail_dir.vpk"));
        assert!(fx.has("addons/.disabled/cool_skin_no_tail_000.vpk"));
        assert!(!fx.has("addons/pak03_dir.vpk"));
        let entry = fx.entry("cool_skin_no_tail_dir.vpk");
        assert_eq!(entry["lastPriority"], 3);
        assert_eq!(entry["imprinted"], true);
    }

    #[test]
    fn swap_hands_the_slot_to_the_other_variant() {
        let fx = fixture();
        fx.write("addons/pak01_dir.vpk");
        fx.write("addons/pak02_dir.vpk");
        fx.write("addons/pak03_dir.vpk");
        fx.write("addons/.disabled/white_dir.vpk");
        fs::create_dir_all(fx.metadata.parent().unwrap()).unwrap();
        fs::write(
            &fx.metadata,
            r#"{"pak02_dir.vpk":{"modName":"Yamato","variantLabel":"Red"},"white_dir.vpk":{"modName":"Yamato","variantLabel":"White","lastPriority":9}}"#,
        )
        .unwrap();

        swap(
            &fx.install,
            &fx.metadata,
            &key("pak02_dir.vpk"),
            &key("white_dir.vpk"),
        )
        .unwrap();

        assert_eq!(fx.entry("pak02_dir.vpk")["variantLabel"], "White");
        assert_eq!(fx.entry("yamato_dir.vpk")["variantLabel"], "Red");
        assert!(!fx.has("addons/pak09_dir.vpk"));
    }

    #[test]
    fn install_copies_chunks_into_disabled_with_metadata() {
        let fx = fixture();
        let download = tempfile::tempdir().unwrap();
        fs::write(download.path().join("pak01_dir.vpk"), "vpk").unwrap();
        fs::write(download.path().join("pak01_000.vpk"), "chunk").unwrap();
        let new = Incoming {
            vpk: download.path().join("pak01_dir.vpk"),
            metadata: ModMetadata {
                mod_name: Some("Cool Skin".to_owned()),
                game_banana_id: Some(7),
                ..ModMetadata::default()
            },
        };

        install(&fx.install, &fx.metadata, &[new], false).unwrap();

        assert!(fx.has("addons/.disabled/cool_skin_dir.vpk"));
        assert!(fx.has("addons/.disabled/cool_skin_000.vpk"));
        assert!(download.path().join("pak01_dir.vpk").exists());
        let entry = fx.entry("cool_skin_dir.vpk");
        assert_eq!(entry["gameBananaId"], 7);
        assert!(entry["nsfw"].is_null());
    }

    #[test]
    fn install_can_enable_right_away() {
        let fx = fixture();
        fx.write("addons/pak01_dir.vpk");
        let download = tempfile::tempdir().unwrap();
        fs::write(download.path().join("Haze_dir.vpk"), "vpk").unwrap();
        let new = Incoming {
            vpk: download.path().join("Haze_dir.vpk"),
            metadata: ModMetadata {
                mod_name: Some("Haze".to_owned()),
                ..ModMetadata::default()
            },
        };

        install(&fx.install, &fx.metadata, &[new], true).unwrap();

        assert!(fx.has("addons/pak02_dir.vpk"));
        assert!(!fx.has("addons/.disabled/haze_dir.vpk"));
        assert_eq!(fx.entry("pak02_dir.vpk")["modName"], "Haze");
    }

    #[test]
    fn disable_dedups_and_escapes_pak_names() {
        let fx = fixture();
        fx.write("addons/pak01_dir.vpk");
        fx.write("addons/pak02_dir.vpk");
        fx.write("addons/.disabled/taken_dir.vpk");
        fs::create_dir_all(fx.metadata.parent().unwrap()).unwrap();
        fs::write(
            &fx.metadata,
            r#"{"pak01_dir.vpk":{"modName":"Taken"},"pak02_dir.vpk":{"modName":"Pak4 Ever"}}"#,
        )
        .unwrap();

        disable(&fx.install, &fx.metadata, &key("pak01_dir.vpk")).unwrap();
        disable(&fx.install, &fx.metadata, &key("pak02_dir.vpk")).unwrap();

        assert!(fx.has("addons/.disabled/taken_0001_dir.vpk"));
        assert!(fx.has("addons/.disabled/mod_pak4_ever_dir.vpk"));
    }

    #[test]
    fn set_global_moves_between_roots_and_flags_disabled() {
        let fx = fixture();
        fx.write("addons/pak01_dir.vpk");
        fx.write("addons/.disabled/cool_dir.vpk");

        set_global(&fx.install, &fx.metadata, &key("pak01_dir.vpk"), true).unwrap();
        assert!(fx.has("grimoire/pak05_dir.vpk"));
        assert_eq!(fx.entry("grimoire/pak05_dir.vpk")["priorityMod"], true);

        set_global(
            &fx.install,
            &fx.metadata,
            &key("grimoire/pak05_dir.vpk"),
            false,
        )
        .unwrap();
        assert!(fx.has("addons/pak01_dir.vpk"));
        assert_eq!(fx.entry("pak01_dir.vpk")["priorityMod"], false);

        set_global(&fx.install, &fx.metadata, &key("cool_dir.vpk"), true).unwrap();
        assert!(fx.has("addons/.disabled/cool_dir.vpk"));
        assert_eq!(fx.entry("cool_dir.vpk")["priorityMod"], true);
    }

    #[test]
    fn reorder_assigns_dense_slots_in_order() {
        let fx = fixture();
        fx.write("addons/pak01_dir.vpk");
        fx.write("addons/pak01_000.vpk");
        fx.write("addons/pak03_dir.vpk");
        fx.write("addons2/pak02_dir.vpk");
        fx.write("addons/.disabled/left_out_dir.vpk");
        fx.write("grimoire/pak05_dir.vpk");
        fs::create_dir_all(fx.metadata.parent().unwrap()).unwrap();
        fs::write(
            &fx.metadata,
            r#"{"addons2/pak02_dir.vpk":{"modName":"Traveler"}}"#,
        )
        .unwrap();

        reorder(
            &fx.install,
            &fx.metadata,
            &fx.journal,
            &[
                key("addons2/pak02_dir.vpk"),
                key("pak03_dir.vpk"),
                key("pak01_dir.vpk"),
            ],
        )
        .unwrap();

        assert!(fx.has("addons/pak01_dir.vpk"));
        assert!(fx.has("addons/pak02_dir.vpk"));
        assert!(fx.has("addons/pak03_dir.vpk"));
        assert!(fx.has("addons/pak03_000.vpk"));
        assert!(!fx.has("addons2/pak02_dir.vpk"));
        assert!(fx.has("addons/.disabled/left_out_dir.vpk"));
        assert!(fx.has("grimoire/pak05_dir.vpk"));
        assert_eq!(fx.entry("pak01_dir.vpk")["modName"], "Traveler");
        assert!(!fx.journal.exists());
        let names: Vec<String> = fx.mods().mods.iter().map(|m| m.key.to_string()).collect();
        assert_eq!(
            names,
            [
                "grimoire/pak05_dir.vpk",
                "pak01_dir.vpk",
                "pak02_dir.vpk",
                "pak03_dir.vpk",
                "left_out_dir.vpk",
            ]
        );
    }

    #[test]
    fn reorder_rejects_a_stale_list_and_keeps_a_noop_clean() {
        let fx = fixture();
        fx.write("addons/pak01_dir.vpk");
        fx.write("addons/pak02_dir.vpk");

        assert!(matches!(
            reorder(
                &fx.install,
                &fx.metadata,
                &fx.journal,
                &[key("pak01_dir.vpk")]
            ),
            Err(Error::Refused(_))
        ));
        assert!(matches!(
            reorder(
                &fx.install,
                &fx.metadata,
                &fx.journal,
                &[key("pak01_dir.vpk"), key("pak01_dir.vpk")]
            ),
            Err(Error::Refused(_))
        ));

        reorder(
            &fx.install,
            &fx.metadata,
            &fx.journal,
            &[key("pak01_dir.vpk"), key("pak02_dir.vpk")],
        )
        .unwrap();
        assert!(!fx.journal.exists());
        assert!(!fx.metadata.exists());
    }

    #[test]
    fn repair_completes_a_reorder_left_midway() {
        let fx = fixture();
        fx.write("addons/pak01_dir.vpk");
        fx.write("addons/pak02_dir.vpk");
        // A crash after phase one: everything in temp names, metadata untouched.
        let plan = Journal::for_moves(
            &fx.install,
            &[
                ReorderMove {
                    key: key("pak01_dir.vpk"),
                    from_name: "pak01_dir.vpk".to_owned(),
                    have: (0, 1),
                    want: (0, 2),
                },
                ReorderMove {
                    key: key("pak02_dir.vpk"),
                    from_name: "pak02_dir.vpk".to_owned(),
                    have: (0, 2),
                    want: (0, 1),
                },
            ],
        )
        .unwrap();
        fsx::write_json(&fx.journal, &plan).unwrap();
        for file in &plan.files {
            fs::rename(&file.from, &file.tmp).unwrap();
        }
        assert_eq!(fx.mods().leftovers.len(), 2);

        assert!(matches!(
            enable(&fx.install, &fx.metadata, &key("pak01_dir.vpk")),
            Err(Error::Refused(_))
        ));

        repair(&fx.install, &fx.metadata, &fx.journal).unwrap();

        assert!(fx.has("addons/pak01_dir.vpk"));
        assert!(fx.has("addons/pak02_dir.vpk"));
        assert!(fx.mods().leftovers.is_empty());
        assert!(!fx.journal.exists());
    }

    #[test]
    fn repair_without_a_journal_trusts_the_temp_names() {
        let fx = fixture();
        fx.write("addons/.grimoire-tmp-0-2-0-1");
        fs::create_dir_all(fx.metadata.parent().unwrap()).unwrap();
        fs::write(&fx.metadata, r#"{"pak02_dir.vpk":{"modName":"Traveler"}}"#).unwrap();

        repair(&fx.install, &fx.metadata, &fx.journal).unwrap();

        assert!(fx.has("addons/pak01_dir.vpk"));
        assert_eq!(fx.entry("pak01_dir.vpk")["modName"], "Traveler");

        repair(&fx.install, &fx.metadata, &fx.journal).unwrap();
    }

    #[test]
    fn delete_removes_files_chunks_and_metadata() {
        let fx = fixture();
        fx.write("addons/pak01_dir.vpk");
        fx.write("addons/pak01_000.vpk");
        fx.write("addons/.disabled/cool_dir.vpk");
        fs::create_dir_all(fx.metadata.parent().unwrap()).unwrap();
        fs::write(
            &fx.metadata,
            r#"{"pak01_dir.vpk":{"modName":"Gone"},"cool_dir.vpk":{}}"#,
        )
        .unwrap();

        delete(
            &fx.install,
            &fx.metadata,
            &[key("pak01_dir.vpk"), key("cool_dir.vpk")],
        )
        .unwrap();

        assert!(!fx.has("addons/pak01_dir.vpk"));
        assert!(!fx.has("addons/pak01_000.vpk"));
        assert!(!fx.has("addons/.disabled/cool_dir.vpk"));
        let raw: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&fx.metadata).unwrap()).unwrap();
        assert_eq!(raw, serde_json::json!({}));

        assert!(matches!(
            delete(&fx.install, &fx.metadata, &[key("pak01_dir.vpk")]),
            Err(Error::Refused(_))
        ));
    }

    #[test]
    fn slugify_collapses_trims_caps_and_escapes() {
        assert_eq!(slugify("Cool Skin (No Tail!)"), "cool_skin_no_tail");
        assert_eq!(slugify("  spaced  out  "), "spaced_out");
        assert_eq!(slugify("pak4 ever"), "mod_pak4_ever");
        assert_eq!(slugify("pakx"), "pakx");
        assert_eq!(slugify("!!!"), "mod");
        assert_eq!(slugify("a".repeat(60).as_str()).len(), 48);
    }

    #[test]
    fn family_excludes_other_dir_files() {
        let fx = fixture();
        fx.write("addons/pak03_dir.vpk");
        fx.write("addons/pak03_000.vpk");
        fx.write("addons/pak03_notes_dir.vpk");

        disable(&fx.install, &fx.metadata, &key("pak03_dir.vpk")).unwrap();

        assert!(!fx.has("addons/.disabled/pak03_notes_dir.vpk"));
        assert!(fx.has("addons/pak03_notes_dir.vpk"));
    }
}
