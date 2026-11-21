//! Read-only access to the Electron client's settings, imported once on the
//! first run so users keep their setup.
//!
//! The Electron app keeps its data in `~/.config/grimoire/`. Its
//! `mod-metadata.json` isn't legacy: both apps read and write it, so it lives
//! in [`crate::metadata`]. We only read the settings fields we use and ignore
//! the rest, so new fields on that side never break parsing here.

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::fsx;
use crate::Result;

#[derive(Debug, Default, Clone, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct LegacySettings {
    pub deadlock_path: Option<PathBuf>,
    pub dev_mode: bool,
    pub dev_deadlock_path: Option<PathBuf>,
}

impl LegacySettings {
    /// A missing file yields defaults.
    pub(crate) fn load(dir: &Path) -> Result<Self> {
        Ok(fsx::read_json(&dir.join("settings.json"))?.unwrap_or_default())
    }

    /// The install the Electron app was pointed at, honoring its dev mode.
    pub fn active_game_path(&self) -> Option<&Path> {
        if self.dev_mode {
            self.dev_deadlock_path.as_deref()
        } else {
            self.deadlock_path.as_deref()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn missing_file_loads_as_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let settings = LegacySettings::load(dir.path()).unwrap();
        assert!(settings.deadlock_path.is_none());
    }

    #[test]
    fn reads_known_fields_and_ignores_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("settings.json"),
            r##"{"deadlockPath":"/games/Deadlock","accentColor":"#f97316"}"##,
        )
        .unwrap();

        let settings = LegacySettings::load(dir.path()).unwrap();

        assert_eq!(
            settings.active_game_path(),
            Some(Path::new("/games/Deadlock"))
        );
    }

    #[test]
    fn dev_mode_switches_the_active_path() {
        let settings = LegacySettings {
            deadlock_path: Some("/real".into()),
            dev_mode: true,
            dev_deadlock_path: Some("/dev".into()),
        };
        assert_eq!(settings.active_game_path(), Some(Path::new("/dev")));
    }
}
