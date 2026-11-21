//! Where things live inside a Deadlock install.
//!
//! The engine loads VPKs from the `SearchPaths` in `gameinfo.gi`. Grimoire
//! uses these folders under `game/citadel/`:
//!
//! - `addons/`: enabled mods named `pakNN_dir.vpk`. NN is the load slot, and
//!   a lower number wins file collisions.
//! - `addons1/` ... `addons10/`: overflow roots for mods past the base folder's
//!   99-slot budget. Each root has its own pakNN namespace.
//! - `grimoire/`: the priority root. It is searched before every addons root,
//!   so its mods win all collisions. Slots `pak01`-`pak04` are reserved for
//!   Locker-generated VPKs.
//! - `addons/.disabled/`: disabled mods, stored under free-form names.

use std::fs;
use std::path::{Path, PathBuf};

use crate::{Error, Result};

pub const PRIORITY_DIR: &str = "grimoire";
pub const DISABLED_DIR: &str = ".disabled";
pub const MAX_OVERFLOW_ROOTS: u32 = 10;

/// A validated Deadlock install. Holding one means `gameinfo.gi` existed when
/// it was opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameInstall {
    root: PathBuf,
}

/// One folder the engine loads `pakNN_dir.vpk` files from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddonRoot {
    /// 0 for `addons/`, N for `addonsN/`.
    pub index: u32,
    pub path: PathBuf,
}

impl GameInstall {
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let install = Self { root: root.into() };
        if install.gameinfo().is_file() {
            Ok(install)
        } else {
            Err(Error::NotAGameInstall(install.root))
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn citadel(&self) -> PathBuf {
        self.root.join("game").join("citadel")
    }

    pub fn gameinfo(&self) -> PathBuf {
        self.citadel().join("gameinfo.gi")
    }

    pub fn priority_dir(&self) -> PathBuf {
        self.citadel().join(PRIORITY_DIR)
    }

    pub fn disabled_dir(&self) -> PathBuf {
        self.citadel().join("addons").join(DISABLED_DIR)
    }

    /// The base `addons/` root followed by every overflow root on disk, in the
    /// order the engine searches them. Overflow roots are sorted numerically
    /// so `addons10` comes after `addons2`.
    pub fn addon_roots(&self) -> Result<Vec<AddonRoot>> {
        let citadel = self.citadel();
        let mut roots = vec![AddonRoot {
            index: 0,
            path: citadel.join("addons"),
        }];

        let entries = fs::read_dir(&citadel).map_err(Error::io(&citadel))?;
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(index) = name.to_str().and_then(overflow_index) else {
                continue;
            };
            if entry.path().is_dir() {
                roots.push(AddonRoot {
                    index,
                    path: entry.path(),
                });
            }
        }

        roots.sort_by_key(|root| root.index);
        Ok(roots)
    }
}

/// Whether Deadlock is running. Mod files must not move while the game has
/// them open. Under Proton the game process's argv[0] is a Windows path
/// ending in `deadlock.exe`.
pub fn is_running() -> bool {
    let Ok(entries) = fs::read_dir("/proc") else {
        return false;
    };
    entries.flatten().any(|entry| {
        let is_pid = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.bytes().all(|b| b.is_ascii_digit()));
        is_pid && fs::read(entry.path().join("cmdline")).is_ok_and(|cmd| is_deadlock(&cmd))
    })
}

fn is_deadlock(cmdline: &[u8]) -> bool {
    let program = cmdline.split(|&b| b == 0).next().unwrap_or_default();
    String::from_utf8_lossy(program)
        .to_ascii_lowercase()
        .replace('\\', "/")
        .ends_with("/deadlock.exe")
}

/// `addons3` -> `Some(3)`. The base `addons` folder and anything past
/// [`MAX_OVERFLOW_ROOTS`] are not overflow roots.
fn overflow_index(folder: &str) -> Option<u32> {
    let digits = folder.strip_prefix("addons")?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let index: u32 = digits.parse().ok()?;
    (1..=MAX_OVERFLOW_ROOTS).contains(&index).then_some(index)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_install() -> (tempfile::TempDir, GameInstall) {
        let dir = tempfile::tempdir().unwrap();
        let citadel = dir.path().join("game/citadel");
        fs::create_dir_all(citadel.join("addons")).unwrap();
        fs::write(citadel.join("gameinfo.gi"), "").unwrap();
        let install = GameInstall::open(dir.path()).unwrap();
        (dir, install)
    }

    #[test]
    fn open_rejects_folder_without_gameinfo() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            GameInstall::open(dir.path()),
            Err(Error::NotAGameInstall(_))
        ));
    }

    #[test]
    fn overflow_index_accepts_only_numbered_roots_in_range() {
        assert_eq!(overflow_index("addons1"), Some(1));
        assert_eq!(overflow_index("addons10"), Some(10));
        assert_eq!(overflow_index("addons"), None);
        assert_eq!(overflow_index("addons11"), None);
        assert_eq!(overflow_index("addons0"), None);
        assert_eq!(overflow_index("addonsx"), None);
    }

    #[test]
    fn is_deadlock_matches_only_the_game_binary() {
        assert!(is_deadlock(
            b"Z:\\home\\me\\Deadlock\\game\\bin\\win64\\deadlock.exe\0-steam\0"
        ));
        assert!(is_deadlock(b"C:/Deadlock/game/bin/win64/Deadlock.exe"));
        assert!(!is_deadlock(b"/usr/bin/grimoire-tui\0--path\0deadlock.exe"));
        assert!(!is_deadlock(b"/usr/bin/notdeadlock.exe"));
    }

    #[test]
    fn addon_roots_are_sorted_numerically() {
        let (_dir, install) = fake_install();
        for name in ["addons10", "addons2", "addons1"] {
            fs::create_dir(install.citadel().join(name)).unwrap();
        }

        let indexes: Vec<u32> = install
            .addon_roots()
            .unwrap()
            .iter()
            .map(|root| root.index)
            .collect();

        assert_eq!(indexes, [0, 1, 2, 10]);
    }
}
