//! Colors. A theme is a dozen named roles; the pages ask for roles, never
//! for raw colors, so every theme works everywhere.
//!
//! Theme files are TOML with one line per role, plus an optional
//! `extends = "<theme>"` to start from another theme instead of `grimoire`.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use ratatui::style::{Color, Modifier, Style};
use serde::{Deserialize, Deserializer};

pub const BUILT_IN: [(&str, &str); 3] = [
    ("grimoire", include_str!("themes/grimoire.toml")),
    ("paper", include_str!("themes/paper.toml")),
    ("terminal", include_str!("themes/terminal.toml")),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Bg,
    Fg,
    Muted,
    Faint,
    Accent,
    AccentFg,
    Surface,
    Selection,
    Border,
    Ok,
    Warn,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// Behind everything. `reset` keeps the terminal's background.
    pub bg: Color,
    pub fg: Color,
    /// Secondary text: authors, labels, hints.
    pub muted: Color,
    /// Text that should recede: disabled mods, rules.
    pub faint: Color,
    pub accent: Color,
    /// Text drawn on top of `accent`.
    pub accent_fg: Color,
    /// Header bar and popups.
    pub surface: Color,
    /// The selected row.
    pub selection: Color,
    pub border: Color,
    pub ok: Color,
    pub warn: Color,
    pub error: Color,
}

impl Default for Theme {
    fn default() -> Self {
        Self {
            bg: Color::Reset,
            fg: Color::Reset,
            muted: Color::Gray,
            faint: Color::DarkGray,
            accent: Color::Yellow,
            accent_fg: Color::Black,
            surface: Color::Reset,
            selection: Color::DarkGray,
            border: Color::DarkGray,
            ok: Color::Green,
            warn: Color::Yellow,
            error: Color::Red,
        }
    }
}

impl Theme {
    pub fn set(&mut self, role: Role, color: Color) {
        let slot = match role {
            Role::Bg => &mut self.bg,
            Role::Fg => &mut self.fg,
            Role::Muted => &mut self.muted,
            Role::Faint => &mut self.faint,
            Role::Accent => &mut self.accent,
            Role::AccentFg => &mut self.accent_fg,
            Role::Surface => &mut self.surface,
            Role::Selection => &mut self.selection,
            Role::Border => &mut self.border,
            Role::Ok => &mut self.ok,
            Role::Warn => &mut self.warn,
            Role::Error => &mut self.error,
        };
        *slot = color;
    }

    pub fn apply(&mut self, colors: &Colors) {
        for (role, color) in &colors.0 {
            self.set(*role, *color);
        }
    }

    /// Loads a built-in theme or `<dir>/<name>.toml`.
    pub fn load(name: &str, dir: &Path) -> Result<Self, String> {
        Self::load_depth(name, dir, 0)
    }

    fn load_depth(name: &str, dir: &Path, depth: u8) -> Result<Self, String> {
        if depth > 4 {
            return Err(format!("theme \"{name}\" extends itself"));
        }
        let text = if let Some((_, text)) = BUILT_IN.iter().find(|(built_in, _)| *built_in == name)
        {
            (*text).to_owned()
        } else {
            let path = dir.join(format!("{name}.toml"));
            fs::read_to_string(&path)
                .map_err(|err| format!("theme \"{name}\": {}: {err}", path.display()))?
        };
        let file: ThemeFile =
            toml_edit::de::from_str(&text).map_err(|err| format!("theme \"{name}\": {err}"))?;
        let mut theme = match file.extends.as_deref() {
            Some(base) => Self::load_depth(base, dir, depth + 1)?,
            None if name == "grimoire" => Self::default(),
            None => Self::load_depth("grimoire", dir, depth + 1)?,
        };
        theme.apply(&file.colors);
        Ok(theme)
    }

    pub fn base(&self) -> Style {
        Style::new().fg(self.fg).bg(self.bg)
    }

    pub fn muted(&self) -> Style {
        Style::new().fg(self.muted)
    }

    pub fn faint(&self) -> Style {
        Style::new().fg(self.faint)
    }

    pub fn accent(&self) -> Style {
        Style::new().fg(self.accent)
    }

    pub fn key(&self) -> Style {
        Style::new().fg(self.accent).add_modifier(Modifier::BOLD)
    }

    pub fn selected(&self) -> Style {
        Style::new().bg(self.selection)
    }

    pub fn border(&self) -> Style {
        Style::new().fg(self.border)
    }

    pub fn title(&self) -> Style {
        Style::new().fg(self.fg).add_modifier(Modifier::BOLD)
    }

    pub fn popup(&self) -> Style {
        Style::new().fg(self.fg).bg(self.surface)
    }
}

/// Color overrides by role, from a theme file or the config's `[colors]`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Colors(HashMap<Role, Color>);

impl<'de> Deserialize<'de> for Colors {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = HashMap::<Role, String>::deserialize(d)?;
        raw.into_iter()
            .map(|(role, text)| {
                text.parse::<Color>()
                    .map(|color| (role, color))
                    .map_err(|_| serde::de::Error::custom(format!("\"{text}\" isn't a color")))
            })
            .collect::<Result<_, _>>()
            .map(Self)
    }
}

#[derive(Deserialize)]
struct ThemeFile {
    extends: Option<String>,
    #[serde(flatten)]
    colors: Colors,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_ins_parse() {
        for (name, _) in BUILT_IN {
            Theme::load(name, Path::new("/nonexistent")).unwrap();
        }
    }

    #[test]
    fn user_theme_extends_and_overrides() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("mine.toml"),
            "extends = \"paper\"\naccent = \"#8b5cf6\"\n",
        )
        .unwrap();
        let paper = Theme::load("paper", dir.path()).unwrap();
        let mine = Theme::load("mine", dir.path()).unwrap();
        assert_eq!(mine.accent, Color::Rgb(0x8b, 0x5c, 0xf6));
        assert_eq!(mine.bg, paper.bg);
    }

    #[test]
    fn bad_values_say_what_is_wrong() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("bad.toml"), "accent = \"orangeish\"\n").unwrap();
        let err = Theme::load("bad", dir.path()).unwrap_err();
        assert!(err.contains("orangeish"), "{err}");
        fs::write(dir.path().join("loop.toml"), "extends = \"loop\"\n").unwrap();
        assert!(Theme::load("loop", dir.path()).is_err());
    }
}
