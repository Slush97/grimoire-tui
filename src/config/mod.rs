//! The TUI's own config: `~/.config/grimoire-tui/config.toml`, with
//! theme files in `themes/` next to it.
//!
//! `default.toml` is the one source of truth for defaults. The user's file is
//! merged over it key by key, so a file written by an older version still
//! gets every newer key's default, and a user can keep just the lines they
//! changed.

pub mod keys;
pub mod theme;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ratatui::widgets::BorderType;
use serde::Deserialize;
use serde_json::Value;

use keys::Keymap;
use theme::{Colors, Theme};

pub const DEFAULT: &str = include_str!("default.toml");

#[derive(Debug, Clone)]
pub struct Config {
    pub theme_name: String,
    pub theme: Theme,
    pub keys: Keymap,
    pub ui: Ui,
    pub images: ImageProtocol,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ui {
    pub borders: Borders,
    pub details: bool,
    pub icons: Icons,
    pub browse_view: BrowseView,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BrowseView {
    List,
    Cards,
}

impl BrowseView {
    pub fn next(self) -> Self {
        match self {
            Self::List => Self::Cards,
            Self::Cards => Self::List,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::List => "list",
            Self::Cards => "cards",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Borders {
    Plain,
    Rounded,
    Double,
    Thick,
}

impl From<Borders> for BorderType {
    fn from(borders: Borders) -> Self {
        match borders {
            Borders::Plain => Self::Plain,
            Borders::Rounded => Self::Rounded,
            Borders::Double => Self::Double,
            Borders::Thick => Self::Thick,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Icons {
    Unicode,
    Ascii,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageProtocol {
    Auto,
    Kitty,
    Sixel,
    Iterm2,
    Halfblocks,
    Off,
}

impl ImageProtocol {
    const ALL: [Self; 6] = [
        Self::Auto,
        Self::Kitty,
        Self::Sixel,
        Self::Iterm2,
        Self::Halfblocks,
        Self::Off,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Kitty => "kitty",
            Self::Sixel => "sixel",
            Self::Iterm2 => "iterm2",
            Self::Halfblocks => "halfblocks",
            Self::Off => "off",
        }
    }

    pub fn next(self) -> Self {
        let at = Self::ALL.iter().position(|p| *p == self).unwrap_or(0);
        Self::ALL[(at + 1) % Self::ALL.len()]
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    theme: String,
    ui: Ui,
    images: Images,
    colors: Colors,
    keys: Keymap,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Images {
    protocol: ImageProtocol,
}

/// Where the config lives.
#[derive(Debug, Clone)]
pub struct Paths {
    pub file: PathBuf,
    pub themes: PathBuf,
}

impl Paths {
    pub fn for_user() -> Option<Self> {
        Some(Self::in_dir(&dirs::config_dir()?.join("grimoire-tui")))
    }

    pub fn in_dir(dir: &Path) -> Self {
        Self {
            file: dir.join("config.toml"),
            themes: dir.join("themes"),
        }
    }

    /// Writes the default file on the first run, so there's something to edit.
    pub fn ensure(&self) -> Result<(), String> {
        if self.file.exists() {
            return Ok(());
        }
        fs::create_dir_all(&self.themes)
            .and_then(|()| fs::write(&self.file, DEFAULT))
            .map_err(|err| format!("couldn't write {}: {err}", self.file.display()))
    }

    /// Changes when the config or its theme file is saved.
    pub fn stamp(&self, theme: &str) -> [Option<SystemTime>; 2] {
        let modified = |path: &Path| fs::metadata(path).and_then(|m| m.modified()).ok();
        [
            modified(&self.file),
            modified(&self.themes.join(format!("{theme}.toml"))),
        ]
    }

    /// Built-in themes, then the user's.
    pub fn theme_names(&self) -> Vec<String> {
        let mut user: Vec<String> = fs::read_dir(&self.themes)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let is_toml = path.extension().is_some_and(|ext| ext == "toml");
                is_toml.then(|| path.file_stem()?.to_str().map(str::to_owned))?
            })
            .filter(|name| !theme::BUILT_IN.iter().any(|(b, _)| b == name))
            .collect();
        user.sort();
        theme::BUILT_IN
            .iter()
            .map(|(name, _)| (*name).to_owned())
            .chain(user)
            .collect()
    }

    /// Sets one top-level or `[section]` value, keeping the rest of the file
    /// (comments included) as the user wrote it.
    pub fn save(
        &self,
        section: Option<&str>,
        key: &str,
        value: impl Into<toml_edit::Value>,
    ) -> Result<(), String> {
        let failed = |err: &dyn std::fmt::Display| format!("{}: {err}", self.file.display());
        let text = match fs::read_to_string(&self.file) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => DEFAULT.to_owned(),
            Err(err) => return Err(failed(&err)),
        };
        let mut doc: toml_edit::DocumentMut = text.parse().map_err(|err| failed(&err))?;
        let table = match section {
            Some(section) => doc
                .entry(section)
                .or_insert(toml_edit::table())
                .as_table_like_mut()
                .ok_or_else(|| failed(&format!("{section} isn't a table")))?,
            None => doc.as_table_mut(),
        };
        // Replacing the item in place keeps the comments above the key and
        // after the value; `insert` would drop them.
        match table.get_mut(key) {
            Some(item) => {
                let decor = item.as_value().map(|old| old.decor().clone());
                *item = toml_edit::value(value);
                if let (Some(decor), Some(new)) = (decor, item.as_value_mut()) {
                    *new.decor_mut() = decor;
                }
            }
            None => {
                table.insert(key, toml_edit::value(value));
            }
        }
        if let Some(dir) = self.file.parent() {
            fs::create_dir_all(dir).map_err(|err| failed(&err))?;
        }
        let temp = self.file.with_extension("toml.tmp");
        fs::write(&temp, doc.to_string())
            .and_then(|()| fs::rename(&temp, &self.file))
            .map_err(|err| failed(&err))
    }
}

impl Config {
    pub fn defaults() -> Self {
        let file: File = toml_edit::de::from_str(DEFAULT).expect("default.toml parses");
        Self::from_file(
            file,
            Theme::load("grimoire", Path::new("")).unwrap_or_default(),
        )
    }

    /// The user's file over the defaults. On a problem it says what's wrong
    /// and keeps going: a broken file gives all defaults, a broken theme
    /// gives the default theme.
    pub fn load(paths: &Paths) -> (Self, Option<String>) {
        let file = match read_merged(&paths.file) {
            Ok(file) => file,
            Err(err) => return (Self::defaults(), Some(err)),
        };
        let mut problem = None;
        let mut theme = Theme::load(&file.theme, &paths.themes).unwrap_or_else(|err| {
            problem = Some(err);
            Theme::load("grimoire", &paths.themes).unwrap_or_default()
        });
        theme.apply(&file.colors);
        (Self::from_file(file, theme), problem)
    }

    fn from_file(file: File, theme: Theme) -> Self {
        Self {
            theme_name: file.theme,
            theme,
            keys: file.keys,
            ui: file.ui,
            images: file.images.protocol,
        }
    }
}

fn read_merged(path: &Path) -> Result<File, String> {
    let failed = |err: &dyn std::fmt::Display| {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        format!("{name}: {}", brief(&err.to_string()))
    };
    let mut merged: Value = toml_edit::de::from_str(DEFAULT).expect("default.toml parses");
    match fs::read_to_string(path) {
        Ok(text) => {
            let user: Value = toml_edit::de::from_str(&text).map_err(|err| failed(&err))?;
            merge(&mut merged, user);
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(failed(&err)),
    }
    serde_json::from_value(merged).map_err(|err| failed(&err))
}

/// A parse error short enough for the footer: TOML's multi-line report
/// becomes "line 3, column 5: <what>", and serde's list of every allowed
/// name is dropped.
fn brief(err: &str) -> String {
    let lines: Vec<&str> = err
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.contains('|'))
        .collect();
    let text = match lines.as_slice() {
        [first, .., last] => {
            let at = first.strip_prefix("TOML parse error at ").unwrap_or(first);
            format!("{at}: {last}")
        }
        [only] => (*only).to_owned(),
        [] => err.to_owned(),
    };
    match text.find(", expected") {
        Some(cut) => text[..cut].to_owned(),
        None => text,
    }
}

/// Tables merge key by key; anything else in `over` replaces what's there.
fn merge(base: &mut Value, over: Value) {
    match (base, over) {
        (Value::Object(base), Value::Object(over)) => {
            for (key, value) in over {
                match base.get_mut(&key) {
                    Some(existing) => merge(existing, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (base, over) => *base = over,
    }
}

#[cfg(test)]
mod tests {
    use super::keys::{Action, Key};
    use super::*;

    #[test]
    fn defaults_bind_every_action_somewhere_sensible() {
        let config = Config::defaults();
        assert_eq!(config.theme_name, "grimoire");
        assert!(!config.keys.keys(Action::Quit).is_empty());
        assert_eq!(config.images, ImageProtocol::Auto);
    }

    #[test]
    fn user_file_overrides_only_what_it_sets() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::in_dir(dir.path());
        fs::write(
            &paths.file,
            "theme = \"paper\"\n[keys]\ndelete = \"x\"\n[colors]\naccent = \"red\"\n",
        )
        .unwrap();
        let (config, problem) = Config::load(&paths);
        assert_eq!(problem, None);
        assert_eq!(config.theme_name, "paper");
        assert_eq!(config.theme.accent, ratatui::style::Color::Red);
        assert_eq!(
            config.keys.keys(Action::Delete),
            ["x".parse::<Key>().unwrap()]
        );
        assert!(!config.keys.keys(Action::Toggle).is_empty());
        assert!(config.ui.details);
    }

    #[test]
    fn broken_file_falls_back_and_says_why() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::in_dir(dir.path());
        fs::write(&paths.file, "[ui]\nbordrs = \"plain\"\n").unwrap();
        let (config, problem) = Config::load(&paths);
        assert!(problem.unwrap().contains("bordrs"));
        assert_eq!(config.theme_name, "grimoire");
    }

    #[test]
    fn errors_are_short() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::in_dir(dir.path());
        fs::write(&paths.file, "[keys]\nbogus = \"z\"\n").unwrap();
        let problem = Config::load(&paths).1.unwrap();
        assert_eq!(problem, "config.toml: unknown variant `bogus`");
        fs::write(&paths.file, "theme = \n").unwrap();
        let problem = Config::load(&paths).1.unwrap();
        assert!(
            problem.starts_with("config.toml: line 1, column 9: "),
            "{problem}"
        );
    }

    #[test]
    fn save_keeps_comments_and_reloads() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::in_dir(dir.path());
        paths.ensure().unwrap();
        paths.save(None, "theme", "terminal").unwrap();
        paths.save(Some("images"), "protocol", "off").unwrap();
        paths.save(Some("ui"), "details", false).unwrap();
        let text = fs::read_to_string(&paths.file).unwrap();
        assert!(text.contains("# Grimoire TUI config."));
        assert!(text.contains("theme = \"terminal\""));
        assert!(text.contains("# How thumbnails are drawn"));
        let (config, problem) = Config::load(&paths);
        assert_eq!(problem, None);
        assert_eq!(config.theme_name, "terminal");
        assert_eq!(config.images, ImageProtocol::Off);
        assert!(!config.ui.details);
    }

    #[test]
    fn theme_names_list_user_files_after_built_ins() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::in_dir(dir.path());
        fs::create_dir_all(&paths.themes).unwrap();
        fs::write(paths.themes.join("zz.toml"), "").unwrap();
        fs::write(paths.themes.join("paper.toml"), "").unwrap();
        assert_eq!(paths.theme_names(), ["grimoire", "paper", "terminal", "zz"]);
    }
}
