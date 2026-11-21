//! The tabs. A page draws from the latest snapshot plus its own UI state and
//! turns actions into jobs for the core or the network. Pages never talk to
//! each other.

pub mod browse;
pub mod installed;
pub mod settings;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Instant;

use ratatui::crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::Frame;

use crate::config::keys::Action;
use crate::config::{self, Config};
use crate::core::{Core, Job};
use crate::gamebanana::Client;
use crate::thumbs::Thumbs;
use crate::ui::{TextInput, Toast, ToastKind};

/// Everything a page may read or ask for.
pub struct Ctx<'a> {
    pub core: &'a mut Core,
    pub net: &'a Client,
    pub thumbs: &'a mut Thumbs,
    pub config: &'a Config,
    pub paths: &'a config::Paths,
    pub toast: &'a mut Option<Toast>,
    pub effect: &'a mut Option<Effect>,
    /// When the app started, to animate spinners.
    pub started: Instant,
}

/// Something only the app can do, asked for by a page.
pub enum Effect {
    ReloadConfig,
    /// Hand the terminal to `$EDITOR` for this file, then reload the config.
    Edit(PathBuf),
}

impl Ctx<'_> {
    pub fn info(&mut self, text: impl Into<String>) {
        *self.toast = Some(Toast::new(ToastKind::Info, text));
    }

    pub fn error(&mut self, text: impl Into<String>) {
        *self.toast = Some(Toast::new(ToastKind::Error, text));
    }

    /// Sends a write to the core, unless one is still running: the page's
    /// view of the disk is out of date until it lands.
    pub fn mutate(&mut self, job: Job) -> bool {
        if self.core.busy() {
            self.info("Still working on the last change.");
            return false;
        }
        self.core.send(job);
        true
    }

    /// The key to mention in a message about `action`.
    pub fn key(&self, action: Action) -> String {
        self.config
            .keys
            .hint(action)
            .map_or_else(|| action.label().to_owned(), |key| key.to_string())
    }

    pub fn open(&mut self, target: impl AsRef<Path>) {
        let target = target.as_ref();
        let opened = Command::new("xdg-open")
            .arg(target)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if opened.is_err() {
            self.error(format!("Couldn't open {}", target.display()));
        }
    }
}

pub trait Page {
    /// What keys can do right now, most specific first. Key lookup and the
    /// help popup both read this.
    fn actions(&self) -> &'static [Action];

    /// The footer's short list, with labels that fit the moment.
    fn hints(&self) -> &'static [(Action, &'static str)];

    /// True while a popup or a move owns the keys, so tabs can't switch.
    fn modal(&self) -> bool;

    /// The text field that has focus. The footer draws it.
    fn field(&self) -> Option<Field<'_>> {
        None
    }

    /// A key for the focused field.
    fn on_field_key(&mut self, _key: KeyEvent, _cx: &mut Ctx) {}

    fn on_action(&mut self, action: Action, cx: &mut Ctx);

    /// The tab was switched to.
    fn on_show(&mut self, _cx: &mut Ctx) {}

    fn draw(&mut self, frame: &mut Frame, area: Rect, cx: &mut Ctx);
}

pub struct Field<'a> {
    pub label: &'static str,
    pub input: &'a TextInput,
    /// What enter and esc do, like "enter search  esc cancel".
    pub hint: &'static str,
    /// The page draws the field itself (in a `ui::SearchBar`), so the
    /// footer only shows the hint.
    pub inline: bool,
}

/// Moves a selection by `delta` within `len` rows, clamped.
pub fn step(current: usize, len: usize, delta: isize) -> usize {
    current
        .saturating_add_signed(delta)
        .min(len.saturating_sub(1))
}

/// Where list navigation actions take a selection, or `None` for other actions.
pub fn navigate(action: Action, current: usize, len: usize, page: usize) -> Option<usize> {
    let page = isize::try_from(page.max(1)).unwrap_or(isize::MAX);
    Some(match action {
        Action::Up => step(current, len, -1),
        Action::Down => step(current, len, 1),
        Action::PageUp => step(current, len, -page),
        Action::PageDown => step(current, len, page),
        Action::Top => 0,
        Action::Bottom => len.saturating_sub(1),
        _ => return None,
    })
}

pub const NAVIGATION: [Action; 8] = [
    Action::Up,
    Action::Down,
    Action::PageUp,
    Action::PageDown,
    Action::Top,
    Action::Bottom,
    Action::Left,
    Action::Right,
];
