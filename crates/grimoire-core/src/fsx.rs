//! File helpers every module shares: JSON reads that treat a missing file as
//! "nothing yet", and writes that can't leave a half-written file behind.

use std::ffi::OsString;
use std::fs;
use std::path::Path;

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::{Error, Result};

/// `Ok(None)` when the file doesn't exist. A file that exists but won't parse
/// is an error, because silently treating it as empty would lose user data.
pub(crate) fn read_json<T: DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(Error::io(path)(err)),
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|source| Error::Json {
            path: path.to_owned(),
            source,
        })
}

pub(crate) fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut text = serde_json::to_string_pretty(value).map_err(|source| Error::Json {
        path: path.to_owned(),
        source,
    })?;
    text.push('\n');
    write_atomic(path, text.as_bytes())
}

/// Writes `<path>.tmp` and renames it over `path`, creating the parent folder
/// if needed. Readers see the old file or the new one, never a partial one.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(Error::io(parent))?;
    }
    let mut tmp = OsString::from(path.as_os_str());
    tmp.push(".tmp");
    fs::write(&tmp, bytes).map_err(Error::io(&tmp))?;
    fs::rename(&tmp, path).map_err(Error::io(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_atomic_creates_parents_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/file.json");

        write_atomic(&path, b"one").unwrap();
        write_atomic(&path, b"two").unwrap();

        assert_eq!(fs::read_to_string(&path).unwrap(), "two");
        assert!(!dir.path().join("a/b/file.json.tmp").exists());
    }

    #[test]
    fn read_json_is_none_for_missing_and_error_for_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.json");
        assert!(read_json::<serde_json::Value>(&path).unwrap().is_none());

        fs::write(&path, "{ nope").unwrap();
        assert!(matches!(
            read_json::<serde_json::Value>(&path),
            Err(Error::Json { .. })
        ));
    }
}
