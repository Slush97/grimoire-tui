//! The frame around the pages (header, footer, help) and the loop that
//! feeds them keys, finished jobs and network replies.
//!
//! The loop only draws when something changed: a key, a reply, a toast
//! timing out, a spinner frame while work is running. Idle, it wakes four
//! times a second to check on the background threads and the config file.

use std::io;
use std::process::Command;
use std::time::{Duration, Instant, SystemTime};

use grimoire_core::backend::Request;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use unicode_width::UnicodeWidthStr;

use crate::config::keys::{Action, Key};
use crate::config::{self, Config};
use crate::core::{Core, Outcome};
use crate::gamebanana::Client;
use crate::pages::browse::Browse;
use crate::pages::installed::Installed;
use crate::pages::settings::Settings;
use crate::pages::{Ctx, Effect, Page, NAVIGATION};
use crate::thumbs::Thumbs;
use crate::ui::{self, Toast, ToastKind};

const GLOBAL: &[Action] = &[
    Action::Quit,
    Action::Help,
    Action::NextTab,
    Action::PrevTab,
    Action::InstalledTab,
    Action::BrowseTab,
    Action::SettingsTab,
];

const IDLE_TICK: Duration = Duration::from_millis(250);
const BUSY_TICK: Duration = Duration::from_millis(80);
const CONFIG_CHECK: Duration = Duration::from_secs(1);
const GAME_CHECK: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Installed,
    Browse,
    Settings,
}

impl Tab {
    const ALL: [Self; 3] = [Self::Installed, Self::Browse, Self::Settings];

    fn title(self) -> &'static str {
        match self {
            Self::Installed => "Installed",
            Self::Browse => "Browse",
            Self::Settings => "Settings",
        }
    }

    fn action(self) -> Action {
        match self {
            Self::Installed => Action::InstalledTab,
            Self::Browse => Action::BrowseTab,
            Self::Settings => Action::SettingsTab,
        }
    }

    fn step(self, forward: bool) -> Self {
        let at = Self::ALL.iter().position(|tab| *tab == self).unwrap_or(0);
        let len = Self::ALL.len();
        let next = if forward { at + 1 } else { at + len - 1 };
        Self::ALL[next % len]
    }
}

#[derive(Default)]
struct Pages {
    installed: Installed,
    browse: Browse,
    settings: Settings,
}

impl Pages {
    fn get(&mut self, tab: Tab) -> &mut dyn Page {
        match tab {
            Tab::Installed => &mut self.installed,
            Tab::Browse => &mut self.browse,
            Tab::Settings => &mut self.settings,
        }
    }
}

pub struct App {
    core: Core,
    net: Client,
    thumbs: Thumbs,
    config: Config,
    paths: config::Paths,
    config_stamp: [Option<SystemTime>; 2],
    pages: Pages,
    tab: Tab,
    help: bool,
    toast: Option<Toast>,
    effect: Option<Effect>,
    started: Instant,
    game_running: bool,
    checked_config: Instant,
    checked_game: Instant,
    redraw: Redraw,
}

/// What the next pass of the loop has to draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Redraw {
    Nothing,
    Changes,
    /// Every cell, not just the ones that changed.
    Everything,
}

impl App {
    pub fn new(
        core: Core,
        thumbs: Thumbs,
        config: Config,
        paths: config::Paths,
        problem: Option<String>,
    ) -> Self {
        let now = Instant::now();
        Self {
            config_stamp: paths.stamp(&config.theme_name),
            core,
            net: Client::spawn(),
            thumbs,
            config,
            paths,
            pages: Pages::default(),
            tab: Tab::Installed,
            help: false,
            toast: problem.map(|problem| Toast::new(ToastKind::Error, problem)),
            effect: None,
            started: now,
            game_running: grimoire_core::game::is_running(),
            checked_config: now,
            checked_game: now,
            redraw: Redraw::Everything,
        }
    }

    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        loop {
            let redraw = std::mem::replace(&mut self.redraw, Redraw::Nothing);
            if redraw != Redraw::Nothing {
                // Clearing inside the synchronized update means the terminal
                // never shows the blank screen in between.
                execute!(terminal.backend_mut(), BeginSynchronizedUpdate)?;
                if redraw == Redraw::Everything {
                    terminal.clear()?;
                }
                terminal.draw(|frame| self.draw(frame))?;
                if self.thumbs.changed_on_screen() {
                    terminal.clear()?;
                    terminal.draw(|frame| self.draw(frame))?;
                    self.thumbs.changed_on_screen();
                }
                execute!(terminal.backend_mut(), EndSynchronizedUpdate)?;
            }
            let tick = if self.working() { BUSY_TICK } else { IDLE_TICK };
            if event::poll(tick)? {
                loop {
                    match event::read()? {
                        Event::Key(key)
                            if key.kind != KeyEventKind::Release && !self.on_key(key) =>
                        {
                            return Ok(());
                        }
                        Event::FocusGained if !self.core.busy() => {
                            // Another app may have changed the game folder.
                            self.core.request(Request::Rescan);
                        }
                        _ => {}
                    }
                    self.changed();
                    if !event::poll(Duration::ZERO)? {
                        break;
                    }
                }
            }
            if let Some(effect) = self.effect.take() {
                self.apply(effect, terminal);
            }
            self.background();
        }
    }

    fn changed(&mut self) {
        if self.redraw == Redraw::Nothing {
            self.redraw = Redraw::Changes;
        }
    }

    /// Something is running that a spinner should show.
    fn working(&self) -> bool {
        self.core.busy() || self.pages.browse.busy() || self.thumbs.loading()
    }

    /// The pages, the open tab and a context for them, borrowed apart so a
    /// page can be handed the rest of the app.
    fn split(&mut self) -> (&mut Pages, Tab, Ctx<'_>) {
        let Self {
            core,
            net,
            thumbs,
            config,
            paths,
            pages,
            tab,
            toast,
            effect,
            started,
            ..
        } = self;
        let cx = Ctx {
            core,
            net,
            thumbs,
            config,
            paths,
            toast,
            effect,
            started: *started,
        };
        (pages, *tab, cx)
    }

    fn parts(&mut self) -> (&mut dyn Page, Ctx<'_>) {
        let (pages, tab, cx) = self.split();
        (pages.get(tab), cx)
    }

    /// Returns false to quit.
    fn on_key(&mut self, event: KeyEvent) -> bool {
        if event.code == KeyCode::Char('c') && event.modifiers.contains(KeyModifiers::CONTROL) {
            return false;
        }
        if self.help {
            self.help = false;
            return true;
        }
        let key = Key::from(event);
        let global = {
            let (page, mut cx) = self.parts();
            if page.field().is_some() {
                page.on_field_key(event, &mut cx);
                return true;
            }
            if let Some(action) = cx.config.keys.resolve(key, page.actions()) {
                page.on_action(action, &mut cx);
                return true;
            }
            if page.modal() {
                return true;
            }
            cx.config.keys.resolve(key, GLOBAL)
        };
        match global {
            Some(Action::Quit) => return false,
            Some(Action::Help) => self.help = true,
            Some(Action::NextTab) => self.show(self.tab.step(true)),
            Some(Action::PrevTab) => self.show(self.tab.step(false)),
            Some(Action::InstalledTab) => self.show(Tab::Installed),
            Some(Action::BrowseTab) => self.show(Tab::Browse),
            Some(Action::SettingsTab) => self.show(Tab::Settings),
            _ => {}
        }
        true
    }

    fn show(&mut self, tab: Tab) {
        self.tab = tab;
        let (page, mut cx) = self.parts();
        page.on_show(&mut cx);
    }

    /// Picks up whatever the background threads finished, and the checks
    /// that run on a timer.
    fn background(&mut self) {
        for outcome in self.core.poll() {
            match outcome {
                Outcome::Done(Some(message)) => {
                    self.toast = Some(Toast::new(ToastKind::Ok, message));
                }
                Outcome::Done(None) => {}
                Outcome::Failed(message) => {
                    self.toast = Some(Toast::new(ToastKind::Error, message));
                }
            }
            self.changed();
        }

        let replies = self.net.poll();
        if !replies.is_empty() {
            let (pages, _, mut cx) = self.split();
            for reply in replies {
                pages.browse.on_reply(reply, &mut cx);
            }
            self.changed();
        }

        if self.thumbs.poll() || self.working() {
            self.changed();
        }
        if self
            .toast
            .as_ref()
            .is_some_and(|toast| toast.until <= Instant::now())
        {
            self.toast = None;
            self.changed();
        }
        if self.checked_config.elapsed() >= CONFIG_CHECK {
            self.checked_config = Instant::now();
            if self.paths.stamp(&self.config.theme_name) != self.config_stamp {
                self.reload_config();
            }
        }
        if self.checked_game.elapsed() >= GAME_CHECK {
            self.checked_game = Instant::now();
            let running = grimoire_core::game::is_running();
            if running != self.game_running {
                self.game_running = running;
                self.changed();
            }
        }
    }

    /// Takes in the config file. A broken edit keeps the last good config
    /// and says what's wrong, so a typo never flips the theme mid-edit.
    fn reload_config(&mut self) {
        let (config, problem) = Config::load(&self.paths);
        self.config_stamp = self.paths.stamp(&config.theme_name);
        self.changed();
        if let Some(problem) = problem {
            self.toast = Some(Toast::new(ToastKind::Error, problem));
            return;
        }
        self.thumbs.set_protocol(config.images);
        self.thumbs.set_background(config.theme.bg);
        self.config = config;
    }

    fn apply(&mut self, effect: Effect, terminal: &mut DefaultTerminal) {
        match effect {
            Effect::ReloadConfig => self.reload_config(),
            Effect::Edit(path) => {
                let editor = std::env::var("VISUAL")
                    .or_else(|_| std::env::var("EDITOR"))
                    .unwrap_or_else(|_| "vi".to_owned());
                let mut words = editor.split_whitespace();
                let program = words.next().unwrap_or("vi");
                crate::restore_terminal();
                let status = Command::new(program).args(words).arg(&path).status();
                *terminal = crate::init_terminal();
                self.thumbs.forget();
                if let Err(err) = status {
                    self.toast = Some(Toast::new(
                        ToastKind::Error,
                        format!("Couldn't start {program}: {err}"),
                    ));
                }
                self.reload_config();
            }
        }
        self.changed();
    }

    fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        frame.render_widget(Block::new().style(self.config.theme.base()), area);
        let [header, body, footer] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .areas(area);
        self.draw_header(frame, header);

        let help = self.help;
        let (page, mut cx) = self.parts();
        page.draw(frame, body, &mut cx);
        draw_footer(frame, footer, page, &cx);
        if help {
            draw_help(frame, body, page.actions(), page.modal(), cx.config);
        }
    }

    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        let theme = &self.config.theme;
        frame.render_widget(Block::new().style(theme.popup()), area);

        let mut left = vec![Span::styled(
            format!(" {} GRIMOIRE ", ui::glyphs(&self.config).global),
            ui::bold(theme.accent()),
        )];
        for tab in Tab::ALL {
            let key = self
                .config
                .keys
                .hint(tab.action())
                .map(|key| format!("{key} "))
                .unwrap_or_default();
            if tab == self.tab {
                left.push(Span::styled(
                    format!(" {key}{} ", tab.title()),
                    ui::bold(Style::new().fg(theme.accent_fg).bg(theme.accent)),
                ));
            } else {
                left.push(Span::styled(format!(" {key}"), theme.faint()));
                left.push(Span::styled(format!("{} ", tab.title()), theme.muted()));
            }
            left.push(Span::raw(" "));
        }

        let mut right = Vec::new();
        if self.core.busy() {
            right.push(Span::styled(
                format!(" {} working ", ui::spinner(self.started)),
                theme.accent(),
            ));
        }
        if let Some((name, percent)) = self.pages.browse.activity() {
            let percent = percent.map(|p| format!(" {p}%")).unwrap_or_default();
            right.push(Span::styled(
                format!(" \u{2193} {}{percent} ", ui::fit(&name, 28)),
                theme.accent(),
            ));
        }
        if self.game_running {
            right.push(Span::styled(
                format!(" {} Deadlock is running ", ui::glyphs(&self.config).warn),
                Style::new().fg(theme.warn),
            ));
        }

        let right_width: usize = right.iter().map(|s| s.content.width()).sum();
        let [left_area, right_area] = Layout::horizontal([
            Constraint::Min(0),
            Constraint::Length(u16::try_from(right_width).unwrap_or(0)),
        ])
        .areas(area);
        frame.render_widget(Line::from(left), left_area);
        frame.render_widget(Line::from(right), right_area);
    }
}

fn draw_footer(frame: &mut Frame, area: Rect, page: &dyn Page, cx: &Ctx) {
    let config = cx.config;
    let theme = &config.theme;
    if let Some(field) = page.field().filter(|field| field.inline) {
        // "enter keep  esc clear": each pair is a key and what it does.
        let mut hints = vec![Span::raw(" ")];
        for pair in field.hint.split("  ") {
            let (key, label) = pair.split_once(' ').unwrap_or((pair, ""));
            hints.push(Span::styled(key, theme.key()));
            hints.push(Span::styled(format!(" {label}    "), theme.muted()));
        }
        frame.render_widget(Line::from(hints), area);
        return;
    }
    if let Some(field) = page.field() {
        let (before, after) = field.input.split();
        let line = Line::from(vec![
            Span::styled(format!(" {} \u{203a} ", field.label), theme.key()),
            Span::styled(before.to_owned(), Style::new().fg(theme.fg)),
            Span::styled(ui::glyphs(config).cursor, theme.accent()),
            Span::styled(after.to_owned(), Style::new().fg(theme.fg)),
        ]);
        let [left, right] = Layout::horizontal([
            Constraint::Min(0),
            Constraint::Length(u16::try_from(field.hint.width() + 1).unwrap_or(0)),
        ])
        .areas(area);
        frame.render_widget(line, left);
        frame.render_widget(Line::styled(field.hint, theme.faint()), right);
        return;
    }

    let toast = cx.toast.as_ref().map(|toast| {
        let glyphs = ui::glyphs(config);
        let (glyph, color) = match toast.kind {
            ToastKind::Info => ("\u{2022}", theme.accent),
            ToastKind::Ok => (glyphs.ok, theme.ok),
            ToastKind::Error => (glyphs.error, theme.error),
        };
        let max = usize::from(area.width) * 2 / 3;
        Line::styled(
            format!("{glyph} {} ", ui::fit(&toast.text, max.saturating_sub(3))),
            Style::new().fg(color),
        )
    });
    let toast_width = toast
        .as_ref()
        .map_or(0, |line| u16::try_from(line.width()).unwrap_or(0));
    let [left, right] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(toast_width)]).areas(area);

    let mut hints = vec![Span::raw(" ")];
    let mut used = 1;
    let actions = page
        .hints()
        .iter()
        .copied()
        .chain((!page.modal()).then_some((Action::Help, "all keys")));
    for (action, label) in actions {
        let Some(key) = config.keys.hint(action) else {
            continue;
        };
        let key = key.to_string();
        let cost = key.width() + label.width() + 5;
        if used + cost > usize::from(left.width) {
            break;
        }
        used += cost;
        hints.push(Span::styled(key, theme.key()));
        hints.push(Span::styled(format!(" {label}    "), theme.muted()));
    }
    frame.render_widget(Line::from(hints), left);
    if let Some(toast) = toast {
        frame.render_widget(toast, right);
    }
}

fn draw_help(frame: &mut Frame, area: Rect, actions: &[Action], modal: bool, config: &Config) {
    let theme = &config.theme;
    let section = |title: &str, list: &mut dyn Iterator<Item = Action>| -> Vec<Line<'static>> {
        let rows: Vec<Line> = list
            .filter(|action| !config.keys.keys(*action).is_empty())
            .map(|action| {
                let keys: Vec<String> = config
                    .keys
                    .keys(action)
                    .iter()
                    .map(ToString::to_string)
                    .collect();
                Line::from(vec![
                    Span::styled(ui::cell(&keys.join(", "), 16), theme.key()),
                    Span::styled(action.label(), Style::new().fg(theme.fg)),
                ])
            })
            .collect();
        if rows.is_empty() {
            return rows;
        }
        let mut lines = vec![Line::styled(title.to_owned(), ui::bold(theme.muted()))];
        lines.extend(rows);
        lines.push(Line::default());
        lines
    };
    let here = section(
        "Here",
        &mut actions.iter().copied().filter(|a| !NAVIGATION.contains(a)),
    );
    let mut rest = section(
        "Lists",
        &mut NAVIGATION.iter().copied().filter(|a| actions.contains(a)),
    );
    if !modal {
        rest.extend(section("Everywhere", &mut GLOBAL.iter().copied()));
    }
    let note = Line::styled(
        "Set keys in the config file. Any key closes this.",
        theme.faint(),
    );

    // One column when it fits, else "Here" beside the rest.
    let tall = here.len() + rest.len() + 3;
    if tall <= usize::from(area.height) {
        let mut lines = here;
        lines.extend(rest);
        lines.push(note);
        let height = u16::try_from(lines.len() + 2).unwrap_or(u16::MAX);
        let inner = ui::popup(frame, config, area, (56, height), "Keys");
        frame.render_widget(Paragraph::new(lines), inner);
        return;
    }
    let height = u16::try_from(here.len().max(rest.len()) + 3).unwrap_or(u16::MAX);
    let inner = ui::popup(frame, config, area, (100, height), "Keys");
    let [columns, foot] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(inner);
    let [left, right] =
        Layout::horizontal([Constraint::Fill(1), Constraint::Fill(1)]).areas(columns);
    frame.render_widget(Paragraph::new(here), left);
    frame.render_widget(Paragraph::new(rest), right);
    frame.render_widget(note, foot);
}
