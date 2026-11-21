//! App settings in `~/.config/grimoire-tui/settings.json`.
//!
//! The first run imports the Deadlock path from the Electron client if one is
//! configured (see `Backend::load`). Unknown fields are preserved on save so
//! other tools sharing the file never lose theirs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{fsx, Result};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub game_path: Option<PathBuf>,
    /// Enable a download as soon as it's installed. Off by default, like the
    /// Electron client's `autoEnableDownloads`.
    pub enable_downloads: bool,
    pub show_nsfw: bool,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Copy)]
pub enum SettingsChange {
    EnableDownloads(bool),
    ShowNsfw(bool),
}

impl Settings {
    /// `Ok(None)` on the first run, when there's no file yet.
    pub(crate) fn load(path: &Path) -> Result<Option<Self>> {
        fsx::read_json(path)
    }

    pub(crate) fn save(&self, path: &Path) -> Result<()> {
        fsx::write_json(path, self)
    }

    pub(crate) fn apply(&mut self, change: SettingsChange) {
        match change {
            SettingsChange::EnableDownloads(on) => self.enable_downloads = on,
            SettingsChange::ShowNsfw(on) => self.show_nsfw = on,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_disk_and_keeps_unknown_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        assert!(Settings::load(&path).unwrap().is_none());

        std::fs::write(&path, r#"{"showNsfw":true,"customFlag":123}"#).unwrap();
        let mut settings = Settings::load(&path).unwrap().unwrap();
        assert!(settings.show_nsfw);
        settings.apply(SettingsChange::EnableDownloads(true));
        settings.save(&path).unwrap();

        let reloaded = Settings::load(&path).unwrap().unwrap();
        assert_eq!(reloaded, settings);
        let raw: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(raw["customFlag"], 123);
    }
}
