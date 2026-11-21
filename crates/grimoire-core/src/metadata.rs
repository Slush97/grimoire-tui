//! `~/.config/grimoire/mod-metadata.json`: what Grimoire knows about each
//! installed VPK, keyed by [`MetaKey`].
//!
//! The Electron client reads and writes this file too. So every change is a
//! read-modify-write of the raw JSON loaded fresh from disk: fields we don't
//! know about survive, and changes the other app made since our last scan
//! aren't overwritten by a stale copy.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};

use crate::library::MetaKey;
use crate::{fsx, Error, Result};

pub type MetadataMap = HashMap<String, ModMetadata>;

/// The fields remix reads. See docs/spec/on-disk-format.md.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ModMetadata {
    pub mod_name: Option<String>,
    pub author: Option<String>,
    pub game_banana_id: Option<u64>,
    pub game_banana_file_id: Option<u64>,
    pub category_name: Option<String>,
    pub variant_label: Option<String>,
    pub local_group_id: Option<String>,
    pub locker_hero: Option<String>,
    pub locker_hero_source: Option<String>,
    pub global_type: Option<String>,
    #[serde(deserialize_with = "null_as_default")]
    pub nsfw: bool,
    pub sha256: Option<String>,
    pub last_priority: Option<u32>,
    #[serde(deserialize_with = "null_as_default")]
    pub priority_mod: bool,
    pub thumbnail_url: Option<String>,
    pub source_file_name: Option<String>,
    /// The author's label for the GameBanana file, like "White colors".
    pub file_description: Option<String>,
}

/// The Electron client writes `null` for unset booleans.
fn null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: Deserializer<'de>,
    T: Default + Deserialize<'de>,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

/// The whole file as typed entries, for a scan. A missing file is empty.
pub(crate) fn load_map(path: &Path) -> Result<MetadataMap> {
    MetadataFile::load(path)?.typed()
}

/// The file as raw JSON, for read-modify-write. Load it right before a change,
/// never earlier.
#[derive(Debug)]
pub(crate) struct MetadataFile {
    path: PathBuf,
    entries: Map<String, Value>,
}

impl MetadataFile {
    pub(crate) fn load(path: &Path) -> Result<Self> {
        Ok(Self {
            path: path.to_owned(),
            entries: fsx::read_json(path)?.unwrap_or_default(),
        })
    }

    pub(crate) fn typed(&self) -> Result<MetadataMap> {
        serde_json::from_value(Value::Object(self.entries.clone())).map_err(|source| Error::Json {
            path: self.path.clone(),
            source,
        })
    }

    /// Sets one field, creating the entry if it doesn't exist yet.
    pub(crate) fn set(&mut self, key: &MetaKey, field: &str, value: Value) {
        let entry = self
            .entries
            .entry(key.as_str())
            .or_insert_with(|| Value::Object(Map::new()));
        if let Value::Object(fields) = entry {
            fields.insert(field.to_owned(), value);
        }
    }

    /// Sets every field `meta` has a value for. Unset options and `false`
    /// flags are left alone, so they never overwrite what's on disk.
    pub(crate) fn set_known(&mut self, key: &MetaKey, meta: &ModMetadata) {
        if let Ok(Value::Object(fields)) = serde_json::to_value(meta) {
            for (field, value) in fields {
                if !matches!(value, Value::Null | Value::Bool(false)) {
                    self.set(key, &field, value);
                }
            }
        }
    }

    /// Moves an entry to the key of the file's new location. Anything already
    /// at `to` described a file that's gone, so it's replaced.
    pub(crate) fn rekey(&mut self, from: &MetaKey, to: &MetaKey) {
        if from == to {
            return;
        }
        match self.entries.remove(from.as_str()) {
            Some(entry) => {
                self.entries.insert(to.as_str().to_owned(), entry);
            }
            None => {
                self.entries.remove(to.as_str());
            }
        }
    }

    /// Re-keys many entries at once. Sources all come out before any target
    /// goes in, so chained moves (a swap, a rotation) can't eat each other.
    pub(crate) fn rekey_all(&mut self, pairs: &[(&MetaKey, &MetaKey)]) {
        let mut moved = Vec::with_capacity(pairs.len());
        for (from, to) in pairs {
            if from == to {
                continue;
            }
            moved.push((to.as_str().to_owned(), self.entries.remove(from.as_str())));
        }
        for (to, entry) in moved {
            match entry {
                Some(entry) => {
                    self.entries.insert(to, entry);
                }
                None => {
                    self.entries.remove(&to);
                }
            }
        }
    }

    pub(crate) fn remove(&mut self, key: &MetaKey) {
        self.entries.remove(key.as_str());
    }

    pub(crate) fn save(&self) -> Result<()> {
        fsx::write_json(&self.path, &self.entries)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;

    use super::*;

    fn key(name: &str) -> MetaKey {
        MetaKey::for_path(Path::new(name))
    }

    #[test]
    fn missing_file_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load_map(&dir.path().join("mod-metadata.json"))
            .unwrap()
            .is_empty());
    }

    #[test]
    fn reads_known_fields_and_tolerates_nulls_and_locker_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mod-metadata.json");
        fs::write(
            &path,
            r#"{
                "cool_dir.vpk": {"modName":"Cool Skin","gameBananaId":42,"globalType":null,"nsfw":null,"imprinted":true},
                "locker:colors": {"modName":"Locker Ability Colors","lockerColors":{"colors":[]}}
            }"#,
        )
        .unwrap();

        let map = load_map(&path).unwrap();

        let cool = &map["cool_dir.vpk"];
        assert_eq!(cool.mod_name.as_deref(), Some("Cool Skin"));
        assert_eq!(cool.game_banana_id, Some(42));
        assert!(!cool.nsfw);
    }

    #[test]
    fn corrupt_file_is_an_error_not_an_empty_library() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mod-metadata.json");
        fs::write(&path, "{ nope").unwrap();
        assert!(matches!(load_map(&path), Err(Error::Json { .. })));
    }

    #[test]
    fn rekey_all_survives_a_swap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mod-metadata.json");
        fs::write(
            &path,
            r#"{"pak01_dir.vpk":{"modName":"One"},"pak02_dir.vpk":{"modName":"Two"}}"#,
        )
        .unwrap();

        let mut file = MetadataFile::load(&path).unwrap();
        let (a, b) = (key("pak01_dir.vpk"), key("pak02_dir.vpk"));
        file.rekey_all(&[(&a, &b), (&b, &a)]);
        file.save().unwrap();

        let raw: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            raw,
            json!({"pak01_dir.vpk":{"modName":"Two"},"pak02_dir.vpk":{"modName":"One"}})
        );
    }

    #[test]
    fn writes_keep_unknown_fields_and_rekey_moves_the_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mod-metadata.json");
        fs::write(
            &path,
            r#"{"pak03_dir.vpk":{"modName":"Cool","imprinted":true},"cool_dir.vpk":{"modName":"stale"}}"#,
        )
        .unwrap();

        let mut file = MetadataFile::load(&path).unwrap();
        file.set(&key("pak03_dir.vpk"), "lastPriority", json!(3));
        file.rekey(&key("pak03_dir.vpk"), &key("cool_dir.vpk"));
        file.save().unwrap();

        let raw: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            raw,
            json!({"cool_dir.vpk":{"modName":"Cool","imprinted":true,"lastPriority":3}})
        );
    }
}
