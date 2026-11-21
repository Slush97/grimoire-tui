//! The Settings tab. Game and mod settings live in `settings.json`; the
//! terminal's own look lives in `config.toml`, and changing it here writes
//! that file.

use std::path::PathBuf;

use grimoire_core::backend::{GameState, Request};
use grimoire_core::gameinfo::GameinfoStatus;
use grimoire_core::settings::SettingsChange;
use ratatui::crossterm::event::KeyEvent;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{HighlightSpacing, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;

use super::{navigate, Ctx, Effect, Field, Page};
use crate::config::keys::Action;
use crate::core::Job;
use crate::ui::{self, Edit, TextInput};

const ACTIONS: &[Action] = &[
    Action::Change,
    Action::Detect,
    Action::EditConfig,
    Action::Up,
    Action::Down,
    Action::PageUp,
    Action::PageDown,
    Action::Top,
    Action::Bottom,
];
const HINTS: &[(Action, &str)] = &[
    (Action::Change, "change"),
    (Action::Detect, "find Deadlock"),
    (Action::EditConfig, "edit config file"),
];

const LABEL_WIDTH: usize = 34;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    Folder,
    Gameinfo,
    EnableDownloads,
    ShowNsfw,
    Theme,
    Images,
    Details,
    BrowseView,
    ConfigFile,
}

const SECTIONS: [(&str, &[Row]); 3] = [
    ("Game", &[Row::Folder, Row::Gameinfo]),
    ("Mods", &[Row::EnableDownloads, Row::ShowNsfw]),
    (
        "Terminal",
        &[
            Row::Theme,
            Row::Images,
            Row::Details,
            Row::BrowseView,
            Row::ConfigFile,
        ],
    ),
];

fn rows() -> impl Iterator<Item = Row> {
    SECTIONS.iter().flat_map(|(_, rows)| rows.iter().copied())
}

#[derive(Default)]
pub struct Settings {
    selected: usize,
    list: ListState,
    path: TextInput,
}

impl Page for Settings {
    fn actions(&self) -> &'static [Action] {
        ACTIONS
    }

    fn hints(&self) -> &'static [(Action, &'static str)] {
        HINTS
    }

    fn modal(&self) -> bool {
        false
    }

    fn field(&self) -> Option<Field<'_>> {
        self.path.active().then_some(Field {
            label: "Deadlock folder",
            input: &self.path,
            hint: "enter save  esc cancel",
            inline: false,
        })
    }

    fn on_field_key(&mut self, key: KeyEvent, cx: &mut Ctx) {
        if let Edit::Submit(path) = self.path.on_key(key) {
            let path = expand_home(path.trim());
            cx.mutate(Job::new(Request::SetGamePath(path)).done("Deadlock folder saved."));
        }
    }

    fn on_action(&mut self, action: Action, cx: &mut Ctx) {
        let count = rows().count();
        if let Some(to) = navigate(action, self.selected, count, count) {
            self.selected = to;
            return;
        }
        match action {
            Action::Detect => {
                cx.mutate(Job::new(Request::DetectGamePath).done("Found Deadlock."));
            }
            Action::EditConfig => *cx.effect = Some(Effect::Edit(cx.paths.file.clone())),
            Action::Change => {
                if let Some(row) = rows().nth(self.selected) {
                    self.change(row, cx);
                }
            }
            _ => {}
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, cx: &mut Ctx) {
        let config = cx.config;
        let theme = &config.theme;
        let block = ui::panel(config, " Settings ");
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let [list_area, _, about_area] = Layout::vertical([
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(3),
        ])
        .areas(inner);

        let width = usize::from(list_area.width.saturating_sub(1));
        let mut items = Vec::new();
        let mut selected_row = 0;
        let mut index = 0;
        for (title, section) in SECTIONS {
            if !items.is_empty() {
                items.push(ListItem::new(Line::default()));
            }
            items.push(ListItem::new(ui::rule(
                config,
                title,
                None,
                list_area.width.saturating_sub(1),
            )));
            for row in section {
                if index == self.selected {
                    selected_row = items.len();
                }
                let (value, style) = value(*row, cx);
                let label = ui::cell(&format!(" {}", label(*row)), LABEL_WIDTH);
                let value = ui::fit(&value, width.saturating_sub(LABEL_WIDTH));
                items.push(ListItem::new(Line::from(vec![
                    Span::styled(label, theme.muted()),
                    Span::styled(value, style),
                ])));
                index += 1;
            }
        }
        self.list.select(Some(selected_row));
        let list = List::new(items)
            .highlight_style(theme.selected())
            .highlight_symbol(Line::from("\u{258c}").style(theme.accent()))
            .highlight_spacing(HighlightSpacing::Always);
        frame.render_stateful_widget(list, list_area, &mut self.list);

        if let Some(row) = rows().nth(self.selected) {
            let about = Paragraph::new(about(row, cx))
                .style(theme.faint())
                .wrap(Wrap { trim: true });
            frame.render_widget(about, about_area);
        }
    }
}

impl Settings {
    fn change(&mut self, row: Row, cx: &mut Ctx) {
        let settings = &cx.core.snapshot.settings;
        match row {
            Row::Folder => {
                let current = settings
                    .game_path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default();
                self.path.start(&current);
            }
            Row::Gameinfo => {
                let broken =
                    cx.core.snapshot.game().is_some_and(|game| {
                        matches!(&*game.gameinfo, Ok(GameinfoStatus::NeedsFix(_)))
                    });
                if broken {
                    cx.mutate(Job::new(Request::RepairGameinfo).done("Fixed gameinfo.gi."));
                } else {
                    cx.info("Nothing to fix here.");
                }
            }
            Row::EnableDownloads => {
                let change = SettingsChange::EnableDownloads(!settings.enable_downloads);
                cx.mutate(Job::new(Request::Settings(change)));
            }
            Row::ShowNsfw => {
                let change = SettingsChange::ShowNsfw(!settings.show_nsfw);
                cx.mutate(Job::new(Request::Settings(change)));
            }
            Row::Theme => {
                let names = cx.paths.theme_names();
                let at = names.iter().position(|name| *name == cx.config.theme_name);
                let next = &names[at.map_or(0, |at| (at + 1) % names.len())];
                save(cx, None, "theme", next.as_str());
            }
            Row::Images => {
                let next = cx.config.images.next();
                save(cx, Some("images"), "protocol", next.name());
            }
            Row::Details => save(cx, Some("ui"), "details", !cx.config.ui.details),
            Row::BrowseView => save(
                cx,
                Some("ui"),
                "browse_view",
                cx.config.ui.browse_view.next().name(),
            ),
            Row::ConfigFile => *cx.effect = Some(Effect::Edit(cx.paths.file.clone())),
        }
    }
}

fn save(cx: &mut Ctx, section: Option<&str>, key: &str, value: impl Into<toml_edit::Value>) {
    match cx.paths.save(section, key, value) {
        Ok(()) => *cx.effect = Some(Effect::ReloadConfig),
        Err(err) => cx.error(err),
    }
}

fn label(row: Row) -> &'static str {
    match row {
        Row::Folder => "Deadlock folder",
        Row::Gameinfo => "gameinfo.gi",
        Row::EnableDownloads => "Turn downloads on right away",
        Row::ShowNsfw => "Show NSFW mods",
        Row::Theme => "Theme",
        Row::Images => "Thumbnails",
        Row::Details => "Details pane",
        Row::BrowseView => "Browse view",
        Row::ConfigFile => "Config file",
    }
}

fn value(row: Row, cx: &Ctx) -> (String, Style) {
    let theme = &cx.config.theme;
    let settings = &cx.core.snapshot.settings;
    let normal = Style::new().fg(theme.fg);
    let on_off = |on: bool| {
        if on {
            ("on".to_owned(), Style::new().fg(theme.ok))
        } else {
            ("off".to_owned(), theme.muted())
        }
    };
    match row {
        Row::Folder => match &cx.core.snapshot.game {
            GameState::Ready(game) => (game.install.root().display().to_string(), normal),
            GameState::Invalid { path, reason } => (
                format!("{} ({reason})", path.display()),
                Style::new().fg(theme.error),
            ),
            GameState::NotSet => ("not set".to_owned(), Style::new().fg(theme.warn)),
        },
        Row::Gameinfo => match cx.core.snapshot.game().map(|game| &*game.gameinfo) {
            None => ("no game folder".to_owned(), theme.muted()),
            Some(Ok(GameinfoStatus::Configured)) => {
                ("set up".to_owned(), Style::new().fg(theme.ok))
            }
            Some(Ok(GameinfoStatus::NeedsFix(_))) => {
                ("needs fixing".to_owned(), Style::new().fg(theme.warn))
            }
            Some(Ok(GameinfoStatus::Missing)) => {
                ("missing".to_owned(), Style::new().fg(theme.error))
            }
            Some(Ok(GameinfoStatus::Unrepairable)) => {
                ("broken".to_owned(), Style::new().fg(theme.error))
            }
            Some(Err(err)) => (err.clone(), Style::new().fg(theme.error)),
        },
        Row::EnableDownloads => on_off(settings.enable_downloads),
        Row::ShowNsfw => on_off(settings.show_nsfw),
        Row::Theme => (cx.config.theme_name.clone(), normal),
        Row::Images => {
            let chosen = cx.config.images;
            let using = cx.thumbs.describe();
            if chosen.name() == using {
                (using.to_owned(), normal)
            } else {
                (format!("{} (using {using})", chosen.name()), normal)
            }
        }
        Row::Details => on_off(cx.config.ui.details),
        Row::BrowseView => (cx.config.ui.browse_view.name().to_owned(), normal),
        Row::ConfigFile => (tilde(&cx.paths.file), normal),
    }
}

fn about(row: Row, cx: &Ctx) -> String {
    match row {
        Row::Folder => format!(
            "The folder with game/citadel in it. {} changes it, {} looks in your Steam libraries.",
            cx.key(Action::Change),
            cx.key(Action::Detect)
        ),
        Row::Gameinfo => match cx.core.snapshot.game().map(|game| &*game.gameinfo) {
            Some(Ok(GameinfoStatus::NeedsFix(reason))) => {
                format!("{reason}. {} fixes it.", cx.key(Action::Change))
            }
            _ => "Tells Deadlock where to load mods from. Grimoire keeps it set up.".to_owned(),
        },
        Row::EnableDownloads => {
            "A mod you install from Browse turns on as soon as it's in, unless another variant of it already is."
                .to_owned()
        }
        Row::ShowNsfw => "Lists NSFW mods in Browse and shows their previews.".to_owned(),
        Row::Theme => format!(
            "grimoire, paper and terminal are built in. Put your own in {}.",
            tilde(&cx.paths.themes)
        ),
        Row::Images => "auto asks the terminal what it can draw. Previews are cached in ~/.cache/grimoire-tui/thumbnails.".to_owned(),
        Row::Details => "The pane beside Installed and Browse. It hides itself in narrow terminals.".to_owned(),
        Row::BrowseView => "Choose a list or thumbnail cards in Browse.".to_owned(),
        Row::ConfigFile => format!(
            "Keys, colors and the rest. {} opens it in $EDITOR, and saved changes show up right away.",
            cx.key(Action::EditConfig)
        ),
    }
}

fn tilde(path: &std::path::Path) -> String {
    match std::env::var_os("HOME").map(PathBuf::from) {
        Some(home) => match path.strip_prefix(&home) {
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => path.display().to_string(),
        },
        None => path.display().to_string(),
    }
}

fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(path),
    }
}
