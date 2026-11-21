//! Actions and the keys bound to them.
//!
//! The keymap is one flat table of action to keys. A page says which actions
//! mean something right now, most specific first, and a key resolves to the
//! first of those it's bound to. So `enter` can be `toggle` in Installed and
//! `details` in Browse without the config needing sections.

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde::{Deserialize, Deserializer};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Quit,
    Help,
    NextTab,
    PrevTab,
    InstalledTab,
    BrowseTab,
    SettingsTab,

    Up,
    Down,
    PageUp,
    PageDown,
    Top,
    Bottom,
    Confirm,
    Cancel,
    Search,
    OpenPage,
    Refresh,

    Toggle,
    Grab,
    MoveUp,
    MoveDown,
    Variants,
    Global,
    Delete,
    Show,
    OpenFolder,
    Repair,
    FixGameinfo,

    Details,
    Install,
    Sort,
    Category,
    Clear,
    View,
    Left,
    Right,

    Change,
    Detect,
    EditConfig,
}

impl Action {
    pub fn label(self) -> &'static str {
        match self {
            Self::Quit => "quit",
            Self::Help => "help",
            Self::NextTab => "next tab",
            Self::PrevTab => "previous tab",
            Self::InstalledTab => "go to Installed",
            Self::BrowseTab => "go to Browse",
            Self::SettingsTab => "go to Settings",
            Self::Up => "up",
            Self::Down => "down",
            Self::PageUp => "page up",
            Self::PageDown => "page down",
            Self::Top => "first",
            Self::Bottom => "last",
            Self::Confirm => "ok",
            Self::Cancel => "cancel",
            Self::Search => "search",
            Self::OpenPage => "open on GameBanana",
            Self::Refresh => "refresh",
            Self::Toggle => "on/off",
            Self::Grab => "move",
            Self::MoveUp => "load earlier",
            Self::MoveDown => "load later",
            Self::Variants => "variants",
            Self::Global => "global",
            Self::Delete => "delete",
            Self::Show => "show on/off/all",
            Self::OpenFolder => "open mods folder",
            Self::Repair => "finish a broken move",
            Self::FixGameinfo => "fix gameinfo.gi",
            Self::Details => "details",
            Self::Install => "install",
            Self::Sort => "sort",
            Self::Category => "category",
            Self::Clear => "clear filters",
            Self::View => "list or cards",
            Self::Left => "left",
            Self::Right => "right",
            Self::Change => "change",
            Self::Detect => "find Deadlock",
            Self::EditConfig => "edit config",
        }
    }
}

/// A key press with the modifiers that matter. Shift is folded into the
/// character, so `J` and `shift+j` are the same key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    code: KeyCode,
    mods: KeyModifiers,
}

impl Key {
    pub fn new(code: KeyCode, mods: KeyModifiers) -> Self {
        let mut mods = mods & (KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT);
        let code = match code {
            KeyCode::Char(c) => {
                if mods.contains(KeyModifiers::SHIFT) {
                    mods.remove(KeyModifiers::SHIFT);
                    KeyCode::Char(c.to_ascii_uppercase())
                } else {
                    KeyCode::Char(c)
                }
            }
            KeyCode::BackTab => {
                mods.remove(KeyModifiers::SHIFT);
                KeyCode::BackTab
            }
            KeyCode::Tab if mods.contains(KeyModifiers::SHIFT) => {
                mods.remove(KeyModifiers::SHIFT);
                KeyCode::BackTab
            }
            code => code,
        };
        Self { code, mods }
    }
}

impl From<KeyEvent> for Key {
    fn from(event: KeyEvent) -> Self {
        Self::new(event.code, event.modifiers)
    }
}

impl FromStr for Key {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        let (prefix, name) = if text == "+" {
            ("", "+")
        } else if let Some(prefix) = text.strip_suffix("++") {
            (prefix, "+")
        } else {
            text.rsplit_once('+').unwrap_or(("", text))
        };
        let mut mods = KeyModifiers::NONE;
        for part in prefix.split('+').filter(|part| !part.is_empty()) {
            mods |= match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => KeyModifiers::CONTROL,
                "alt" | "meta" => KeyModifiers::ALT,
                "shift" => KeyModifiers::SHIFT,
                _ => return Err(format!("unknown modifier \"{part}\" in \"{text}\"")),
            };
        }
        let mut chars = name.chars();
        let code = match (chars.next(), chars.next()) {
            (Some(c), None) => KeyCode::Char(c),
            _ => match name.to_ascii_lowercase().as_str() {
                "enter" | "return" => KeyCode::Enter,
                "esc" | "escape" => KeyCode::Esc,
                "tab" => KeyCode::Tab,
                "backtab" => KeyCode::BackTab,
                "space" => KeyCode::Char(' '),
                "backspace" => KeyCode::Backspace,
                "delete" | "del" => KeyCode::Delete,
                "insert" | "ins" => KeyCode::Insert,
                "up" => KeyCode::Up,
                "down" => KeyCode::Down,
                "left" => KeyCode::Left,
                "right" => KeyCode::Right,
                "home" => KeyCode::Home,
                "end" => KeyCode::End,
                "pageup" | "pgup" => KeyCode::PageUp,
                "pagedown" | "pgdn" => KeyCode::PageDown,
                lower => match lower.strip_prefix('f').and_then(|n| n.parse().ok()) {
                    Some(n @ 1..=12) => KeyCode::F(n),
                    _ => return Err(format!("unknown key \"{text}\"")),
                },
            },
        };
        Ok(Self::new(code, mods))
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.mods.contains(KeyModifiers::CONTROL) {
            f.write_str("ctrl+")?;
        }
        if self.mods.contains(KeyModifiers::ALT) {
            f.write_str("alt+")?;
        }
        if self.mods.contains(KeyModifiers::SHIFT) {
            f.write_str("shift+")?;
        }
        match self.code {
            KeyCode::Char(' ') => f.write_str("space"),
            KeyCode::Char(c) => write!(f, "{c}"),
            KeyCode::Enter => f.write_str("enter"),
            KeyCode::Esc => f.write_str("esc"),
            KeyCode::Tab => f.write_str("tab"),
            KeyCode::BackTab => f.write_str("shift+tab"),
            KeyCode::Backspace => f.write_str("bksp"),
            KeyCode::Delete => f.write_str("del"),
            KeyCode::Insert => f.write_str("ins"),
            KeyCode::Up => f.write_str("\u{2191}"),
            KeyCode::Down => f.write_str("\u{2193}"),
            KeyCode::Left => f.write_str("\u{2190}"),
            KeyCode::Right => f.write_str("\u{2192}"),
            KeyCode::Home => f.write_str("home"),
            KeyCode::End => f.write_str("end"),
            KeyCode::PageUp => f.write_str("pgup"),
            KeyCode::PageDown => f.write_str("pgdn"),
            KeyCode::F(n) => write!(f, "f{n}"),
            other => write!(f, "{other:?}"),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Keymap(HashMap<Action, Vec<Key>>);

impl Keymap {
    /// The first of `actions` that `key` is bound to.
    pub fn resolve(&self, key: Key, actions: &[Action]) -> Option<Action> {
        actions
            .iter()
            .copied()
            .find(|action| self.keys(*action).contains(&key))
    }

    pub fn keys(&self, action: Action) -> &[Key] {
        self.0.get(&action).map_or(&[], Vec::as_slice)
    }

    /// What to show for `action` in hints: its first key.
    pub fn hint(&self, action: Action) -> Option<Key> {
        self.keys(action).first().copied()
    }
}

impl<'de> Deserialize<'de> for Keymap {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum OneOrMany {
            One(String),
            Many(Vec<String>),
        }

        let raw = HashMap::<Action, OneOrMany>::deserialize(d)?;
        let mut map = HashMap::with_capacity(raw.len());
        for (action, keys) in raw {
            let keys = match keys {
                OneOrMany::One(key) => vec![key],
                OneOrMany::Many(keys) => keys,
            };
            let parsed = keys
                .iter()
                .filter(|key| !key.is_empty())
                .map(|key| key.parse())
                .collect::<Result<Vec<Key>, String>>()
                .map_err(serde::de::Error::custom)?;
            map.insert(action, parsed);
        }
        Ok(Self(map))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(text: &str) -> Key {
        text.parse().unwrap()
    }

    #[test]
    fn parses_names_and_modifiers() {
        assert_eq!(
            key("ctrl+d"),
            Key::new(KeyCode::Char('d'), KeyModifiers::CONTROL)
        );
        assert_eq!(key("shift+j"), key("J"));
        assert_eq!(key("shift+tab"), key("backtab"));
        assert_eq!(
            key("space"),
            Key::new(KeyCode::Char(' '), KeyModifiers::NONE)
        );
        assert_eq!(key("+"), Key::new(KeyCode::Char('+'), KeyModifiers::NONE));
        assert_eq!(
            key("ctrl++"),
            Key::new(KeyCode::Char('+'), KeyModifiers::CONTROL)
        );
        assert_eq!(key("F5"), Key::new(KeyCode::F(5), KeyModifiers::NONE));
        assert!("hyper+x".parse::<Key>().is_err());
        assert!("f13".parse::<Key>().is_err());
    }

    #[test]
    fn events_fold_shift_into_the_character() {
        let event = KeyEvent::new(KeyCode::Char('J'), KeyModifiers::SHIFT);
        assert_eq!(Key::from(event), key("J"));
        let event = KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT);
        assert_eq!(Key::from(event), key("backtab"));
    }

    #[test]
    fn resolve_takes_the_first_listed_action() {
        let map: Keymap = toml_edit::de::from_str(
            r#"
            toggle = ["space", "enter"]
            confirm = "enter"
            delete = ""
            "#,
        )
        .unwrap();
        let enter = key("enter");
        assert_eq!(
            map.resolve(enter, &[Action::Confirm, Action::Toggle]),
            Some(Action::Confirm)
        );
        assert_eq!(map.resolve(enter, &[Action::Toggle]), Some(Action::Toggle));
        assert!(map.keys(Action::Delete).is_empty());
    }
}
