//! Pulls the VPKs out of a download. GameBanana mods come as zip, 7z or rar,
//! and now and then as a bare VPK.

use std::collections::HashSet;
use std::fmt::Display;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

const ZIP: &[u8] = b"PK\x03\x04";
const SEVEN_Z: &[u8] = b"7z\xbc\xaf\x27\x1c";
const RAR: &[u8] = b"Rar!\x1a\x07";
const VPK: &[u8] = &[0x34, 0x12, 0xaa, 0x55];

/// Unpacks every `.vpk` in `download` under `dest` and returns the mods in
/// it, sorted. `name` is the file's name on GameBanana, used for a bare VPK.
pub fn vpks(download: &Path, name: &str, dest: &Path) -> Result<Vec<PathBuf>, String> {
    let mut magic = [0; 8];
    let read = File::open(download)
        .and_then(|mut file| file.read(&mut magic))
        .map_err(text)?;
    let magic = &magic[..read];
    fs::create_dir_all(dest).map_err(text)?;

    if magic.starts_with(ZIP) {
        unzip(download, dest)
    } else if magic.starts_with(SEVEN_Z) {
        un7z(download, dest)
    } else if magic.starts_with(RAR) {
        unrar(download, dest)
    } else if magic.starts_with(VPK) {
        let stem = bare_vpk_stem(name);
        fs::copy(download, dest.join(format!("{stem}.vpk")))
            .map(drop)
            .map_err(text)
    } else {
        Err(format!("{name} isn't a zip, 7z, rar or VPK"))
    }
    .map_err(|err| format!("Couldn't unpack {name}: {err}"))?;

    let mut found = Vec::new();
    collect(dest, &mut found).map_err(text)?;
    found.sort();
    Ok(found)
}

fn unzip(file: &Path, dest: &Path) -> Result<(), String> {
    let mut zip = zip::ZipArchive::new(File::open(file).map_err(text)?).map_err(text)?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(text)?;
        if let Some(path) = target(dest, entry.name()) {
            write(&path, &mut entry).map_err(text)?;
        }
    }
    Ok(())
}

fn un7z(file: &Path, dest: &Path) -> Result<(), String> {
    let mut archive =
        sevenz_rust2::ArchiveReader::open(file, sevenz_rust2::Password::empty()).map_err(text)?;
    archive
        .for_each_entries(|entry, reader| {
            match target(dest, entry.name()).filter(|_| !entry.is_directory()) {
                Some(path) => write(&path, reader)?,
                // Solid blocks decode front to back, so skipped entries are read through.
                None => drop(io::copy(reader, &mut io::sink())?),
            }
            Ok(true)
        })
        .map_err(text)
}

fn unrar(file: &Path, dest: &Path) -> Result<(), String> {
    let mut archive = unrar::Archive::new(file)
        .open_for_processing()
        .map_err(text)?;
    while let Some(header) = archive.read_header().map_err(text)? {
        let name = header.entry().filename.to_string_lossy().into_owned();
        archive = match target(dest, &name).filter(|_| header.entry().is_file()) {
            Some(path) => {
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).map_err(text)?;
                }
                header.extract_to(&path).map_err(text)?
            }
            None => header.skip().map_err(text)?,
        };
    }
    Ok(())
}

/// Where an archive entry goes under `dest`: only `.vpk` files, and nothing
/// that climbs out with `..`.
fn target(dest: &Path, name: &str) -> Option<PathBuf> {
    if !name.to_ascii_lowercase().ends_with(".vpk") {
        return None;
    }
    let mut path = dest.to_path_buf();
    for part in name.split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => return None,
            part => path.push(part),
        }
    }
    Some(path)
}

fn write(path: &Path, from: &mut dyn Read) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    io::copy(from, &mut File::create(path)?).map(drop)
}

/// Every `_dir.vpk` under `dir`. A `.vpk` that isn't a `_dir.vpk` or one of
/// its numbered chunks is a single-file VPK, and gets the `_dir` name so it
/// counts as a mod.
fn collect(dir: &Path, found: &mut Vec<PathBuf>) -> io::Result<()> {
    let mut files = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect(&path, found)?;
        } else {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            let name = name.to_ascii_lowercase();
            files.push((path, name));
        }
    }
    let dir_stems: HashSet<&str> = files
        .iter()
        .filter_map(|(_, name)| name.strip_suffix("_dir.vpk"))
        .collect();
    for (path, name) in &files {
        if name.ends_with("_dir.vpk") {
            found.push(path.clone());
            continue;
        }
        let Some(stem) = name.strip_suffix(".vpk") else {
            continue;
        };
        let chunk_of = stem
            .rsplit_once('_')
            .filter(|(_, n)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            .map(|(owner, _)| owner);
        if chunk_of.is_some_and(|owner| dir_stems.contains(owner)) {
            continue;
        }
        let own_stem = path.file_stem().unwrap_or_default().to_string_lossy();
        let single = path.with_file_name(format!("{own_stem}_dir.vpk"));
        fs::rename(path, &single)?;
        found.push(single);
    }
    Ok(())
}

fn bare_vpk_stem(name: &str) -> &str {
    let base = name
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or("mod");
    let stem = base.rsplit_once('.').map_or(base, |(stem, _)| stem);
    if stem.is_empty() || matches!(stem, "." | "..") {
        "mod"
    } else {
        stem
    }
}

fn text(err: impl Display) -> String {
    err.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_keeps_vpks_inside_dest() {
        let dest = Path::new("/x");
        assert_eq!(
            target(dest, "Variant A\\pak01_dir.vpk"),
            Some(PathBuf::from("/x/Variant A/pak01_dir.vpk"))
        );
        assert_eq!(target(dest, "../../etc/evil.vpk"), None);
        assert_eq!(target(dest, "readme.txt"), None);
    }

    #[test]
    fn bare_vpk_stem_strips_paths_and_traversal() {
        assert_eq!(bare_vpk_stem("pak01_dir.vpk"), "pak01_dir");
        assert_eq!(bare_vpk_stem("../../evil.vpk"), "evil");
        assert_eq!(bare_vpk_stem("dir\\sub\\skin.vpk"), "skin");
        assert_eq!(bare_vpk_stem(".."), "mod");
        assert_eq!(bare_vpk_stem(".vpk"), "mod");
    }

    #[test]
    fn collect_names_single_files_and_skips_chunks() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["big_dir.vpk", "big_000.vpk", "lone.vpk", "a/pak01_dir.vpk"] {
            let path = dir.path().join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, "vpk").unwrap();
        }

        let mut found = Vec::new();
        collect(dir.path(), &mut found).unwrap();
        found.sort();

        let names: Vec<_> = found
            .iter()
            .map(|p| p.strip_prefix(dir.path()).unwrap().to_owned())
            .collect();
        assert_eq!(
            names,
            ["a/pak01_dir.vpk", "big_dir.vpk", "lone_dir.vpk"].map(PathBuf::from)
        );
    }
}
