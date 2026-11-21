//! Finding Deadlock (Steam app 1422450) through Steam's `libraryfolders.vdf`.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::game::GameInstall;
use crate::{Error, Result};

const DEADLOCK_APP_ID: &str = "1422450";

/// The Deadlock install folder, or `Refused` with a message for the user when
/// Steam doesn't know about one.
pub(crate) fn detect() -> Result<PathBuf> {
    detect_in_roots(&steam_roots())
}

fn steam_roots() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let candidates = [
        home.join(".steam/steam"),
        home.join(".local/share/Steam"),
        home.join(".var/app/com.valvesoftware.Steam/.steam/steam"),
        home.join("snap/steam/common/.local/share/Steam"),
    ];
    let mut roots = Vec::new();
    let mut seen = HashSet::new();
    for root in candidates {
        if !root.is_dir() {
            continue;
        }
        let key = fs::canonicalize(&root).unwrap_or_else(|_| root.clone());
        if seen.insert(key) {
            roots.push(root);
        }
    }
    roots
}

fn detect_in_roots(roots: &[PathBuf]) -> Result<PathBuf> {
    let mut seen = HashSet::new();
    let mut fallback = Vec::new();

    for steam_root in roots {
        if !steam_root.is_dir() {
            continue;
        }
        let mut libraries = read_libraries(steam_root);
        // Steam's own install dir is implicitly a library, even when the VDF
        // is missing or doesn't list it.
        libraries.insert(0, steam_root.clone());

        for library in libraries {
            let key = fs::canonicalize(&library).unwrap_or_else(|_| library.clone());
            if !seen.insert(key) {
                continue;
            }
            let steamapps = library.join("steamapps");
            let candidate = steamapps.join("common").join("Deadlock");
            let manifest = steamapps.join(format!("appmanifest_{DEADLOCK_APP_ID}.acf"));
            if manifest.is_file() && GameInstall::open(&candidate).is_ok() {
                return Ok(candidate);
            }
            fallback.push(candidate);
        }
    }

    for candidate in fallback {
        if GameInstall::open(&candidate).is_ok() {
            return Ok(candidate);
        }
    }

    Err(Error::Refused(
        "Couldn't find Deadlock in your Steam libraries. Set the folder manually.".to_owned(),
    ))
}

fn read_libraries(steam_root: &Path) -> Vec<PathBuf> {
    let vdf = steam_root.join("steamapps").join("libraryfolders.vdf");
    let Ok(content) = fs::read_to_string(vdf) else {
        return Vec::new();
    };
    parse_library_folders(&content)
}

/// Extracts every `"path" "<dir>"` entry from Steam's `libraryfolders.vdf`.
fn parse_library_folders(content: &str) -> Vec<PathBuf> {
    let mut libraries = Vec::new();
    for line in content.lines() {
        let mut rest = line.trim();
        if rest.starts_with("//") {
            continue;
        }
        let Some((key, after_key)) = read_vdf_string(rest) else {
            continue;
        };
        if !key.eq_ignore_ascii_case("path") {
            continue;
        }
        rest = after_key.trim_start();
        let Some((value, _)) = read_vdf_string(rest) else {
            continue;
        };
        if !value.is_empty() {
            libraries.push(PathBuf::from(value));
        }
    }
    libraries
}

/// Reads a leading double-quoted VDF string, unescaping `\\` and `\"`.
fn read_vdf_string(input: &str) -> Option<(String, &str)> {
    let rest = input.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.char_indices();
    while let Some((i, c)) = chars.next() {
        match c {
            '"' => return Some((out, &rest[i + 1..])),
            '\\' => match chars.next() {
                Some((_, escaped)) => out.push(escaped),
                None => return None,
            },
            _ => out.push(c),
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_install(root: &Path, with_manifest: bool) -> PathBuf {
        let install = root.join("steamapps/common/Deadlock");
        fs::create_dir_all(install.join("game/citadel/addons")).unwrap();
        fs::write(install.join("game/citadel/gameinfo.gi"), "").unwrap();
        if with_manifest {
            fs::write(
                root.join(format!("steamapps/appmanifest_{DEADLOCK_APP_ID}.acf")),
                "\"AppState\" {}",
            )
            .unwrap();
        }
        install
    }

    #[test]
    fn parses_libraryfolders_vdf_paths() {
        let vdf = r#"
"libraryfolders"
{
    "0"
    {
        "path"		"/home/me/.local/share/Steam"
        "label"		""
    }
    "1"
    {
        // "path" "/ignored/comment"
        "Path"  "/mnt/games/Steam\\Library"
    }
}
"#;
        assert_eq!(
            parse_library_folders(vdf),
            [
                PathBuf::from("/home/me/.local/share/Steam"),
                PathBuf::from("/mnt/games/Steam\\Library"),
            ]
        );
    }

    #[test]
    fn prefers_library_with_deadlock_appmanifest() {
        let dir = tempfile::tempdir().unwrap();
        let steam = dir.path().join("steam");
        let second = dir.path().join("second");
        fs::create_dir_all(steam.join("steamapps")).unwrap();
        // Unmanifested copy in the primary Steam dir.
        write_install(&steam, false);
        // Authoritative install in a secondary library.
        let expected = write_install(&second, true);
        fs::write(
            steam.join("steamapps/libraryfolders.vdf"),
            format!(
                "\"libraryfolders\"\n{{\n\t\"0\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}\n}}\n",
                second.display()
            ),
        )
        .unwrap();

        assert_eq!(detect_in_roots(&[steam]).unwrap(), expected);
    }

    #[test]
    fn falls_back_to_valid_install_without_manifest_and_skips_stale_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let stale = dir.path().join("stale");
        let real = dir.path().join("real");
        // Empty folder with no gameinfo.gi must be ignored.
        fs::create_dir_all(stale.join("steamapps/common/Deadlock/game/citadel")).unwrap();
        let expected = write_install(&real, false);

        assert_eq!(detect_in_roots(&[stale, real]).unwrap(), expected);
    }

    #[test]
    fn missing_install_returns_refused() {
        let dir = tempfile::tempdir().unwrap();
        let result = detect_in_roots(&[dir.path().to_path_buf()]);
        assert!(matches!(result, Err(Error::Refused(_))));
    }
}
