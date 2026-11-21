//! The Browse tab: GameBanana's Deadlock mods. The list loads more as you
//! scroll, a category or search narrows it, and a mod's page (description
//! and files) opens in a popup to install from.

mod view;

use std::collections::HashSet;

use grimoire_core::backend::Request;
use grimoire_core::library::Incoming;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::widgets::ListState;
use ratatui::Frame;
use tempfile::TempDir;

use super::{navigate, Ctx, Field, Page};
use crate::config::keys::Action;
use crate::config::BrowseView;
use crate::core::Job;
use crate::gamebanana::{
    html, Details, Job as NetJob, JobKind, ModFile, Query, Reply, Sort, Submission,
};
use crate::ui::{Edit, TextInput};

const ACTIONS: &[Action] = &[
    Action::Details,
    Action::Install,
    Action::Search,
    Action::Category,
    Action::Sort,
    Action::Clear,
    Action::Refresh,
    Action::OpenPage,
    Action::Up,
    Action::Down,
    Action::PageUp,
    Action::PageDown,
    Action::Top,
    Action::Bottom,
    Action::Left,
    Action::Right,
    Action::View,
];
const HINTS: &[(Action, &str)] = &[
    (Action::Details, "details"),
    (Action::Install, "install"),
    (Action::Category, "category"),
    (Action::Search, "search"),
    (Action::Sort, "sort"),
    (Action::Clear, "clear"),
    (Action::View, "view"),
    (Action::OpenPage, "open page"),
];

const PAGE_ACTIONS: &[Action] = &[
    Action::Install,
    Action::Confirm,
    Action::Cancel,
    Action::OpenPage,
    Action::Up,
    Action::Down,
    Action::PageUp,
    Action::PageDown,
    Action::Top,
    Action::Bottom,
];
const PAGE_HINTS: &[(Action, &str)] = &[
    (Action::Install, "install this file"),
    (Action::Cancel, "close"),
    (Action::Down, "next file"),
    (Action::PageDown, "scroll"),
    (Action::OpenPage, "open page"),
];

/// Load the next page when the selection gets this close to the end.
const LOAD_AHEAD: usize = 5;

pub struct Browse {
    opened: bool,
    search: TextInput,
    query: Query,
    category_name: Option<String>,
    results: Vec<Submission>,
    next_page: u32,
    last_page: bool,
    loading: bool,
    selected: usize,
    list: ListState,
    page_rows: usize,
    card_columns: usize,
    card_rows: usize,
    /// The first row of cards on screen, so the grid scrolls like a list.
    card_top: usize,
    categories: Categories,
    suggestion: usize,
    /// Waiting on a mod's page: its id, and whether to install straight away.
    pending: Option<(u64, bool)>,
    page: Option<ModPage>,
    download: Option<Download>,
}

enum Categories {
    NotLoaded,
    Loading,
    Loaded(Vec<CategoryRow>),
}

pub(super) struct CategoryRow {
    pub id: Option<u64>,
    pub name: String,
    pub parent: Option<String>,
    pub items: u64,
    pub child: bool,
}

impl CategoryRow {
    /// `Skins \u{203a} Haze` for a hero, `HUD` for a root.
    pub fn path(&self) -> String {
        match &self.parent {
            Some(parent) => format!("{parent} \u{203a} {}", self.name),
            None => self.name.clone(),
        }
    }
}

#[derive(Clone, Copy)]
pub(super) enum Suggestion {
    All,
    Search,
    Category(usize),
}

/// A mod's page: description and files.
struct ModPage {
    submission: Submission,
    details: Details,
    text: String,
    files: Vec<ModFile>,
    file: ListState,
    scroll: u16,
}

struct Download {
    name: String,
    done: u64,
    total: u64,
}

impl Default for Browse {
    fn default() -> Self {
        Self {
            opened: false,
            search: TextInput::default(),
            query: Query {
                text: String::new(),
                sort: Sort::Liked,
                category: None,
            },
            category_name: None,
            results: Vec::new(),
            next_page: 1,
            last_page: false,
            loading: false,
            selected: 0,
            list: ListState::default(),
            page_rows: 10,
            card_columns: 1,
            card_rows: 1,
            card_top: 0,
            categories: Categories::NotLoaded,
            suggestion: 0,
            pending: None,
            page: None,
            download: None,
        }
    }
}

impl Page for Browse {
    fn actions(&self) -> &'static [Action] {
        if self.page.is_some() {
            PAGE_ACTIONS
        } else {
            ACTIONS
        }
    }

    fn hints(&self) -> &'static [(Action, &'static str)] {
        if self.page.is_some() {
            PAGE_HINTS
        } else {
            HINTS
        }
    }

    fn modal(&self) -> bool {
        self.page.is_some()
    }

    fn field(&self) -> Option<Field<'_>> {
        self.search.active().then_some(Field {
            label: "Search mods",
            input: &self.search,
            hint: "enter pick  esc cancel  up/down move",
            inline: true,
        })
    }

    fn on_field_key(&mut self, key: KeyEvent, cx: &mut Ctx) {
        self.field_key(key, cx);
    }

    /// The first visit loads the feed. Nothing goes to GameBanana before that.
    fn on_show(&mut self, cx: &mut Ctx) {
        if !self.opened {
            self.opened = true;
            self.reload(cx);
        }
    }

    fn on_action(&mut self, action: Action, cx: &mut Ctx) {
        if self.page.is_some() {
            self.on_page_action(action, cx);
            return;
        }

        let visible = self.visible(cx).len();
        if Self::view(cx) == BrowseView::Cards {
            let columns = isize::try_from(self.card_columns.max(1)).unwrap_or(isize::MAX);
            let screen = isize::try_from(self.card_rows).unwrap_or(isize::MAX);
            let selected = isize::try_from(self.selected).unwrap_or(isize::MAX);
            let delta = match action {
                Action::Left => Some(-1),
                Action::Right => Some(1),
                Action::Up => Some(-columns),
                Action::Down => Some(columns),
                Action::PageUp => Some(-columns.saturating_mul(screen)),
                Action::PageDown => Some(columns.saturating_mul(screen)),
                Action::Top => Some(-selected),
                Action::Bottom => Some(isize::MAX),
                _ => None,
            };
            if let Some(delta) = delta {
                self.selected = super::step(self.selected, visible, delta);
                self.load_more_if_near_end(cx, visible);
                return;
            }
        }
        let page = self.page_rows;
        if let Some(to) = navigate(action, self.selected, visible, page) {
            self.selected = to;
            self.load_more_if_near_end(cx, visible);
            return;
        }
        let selected = self.visible(cx).get(self.selected).map(|s| (*s).clone());
        match action {
            Action::Details | Action::Install => {
                if let Some(submission) = selected {
                    self.pending = Some((submission.id, action == Action::Install));
                    cx.net.send(NetJob::Details(submission));
                }
            }
            Action::Sort if !self.query.text.is_empty() => {
                cx.info("Search results come by best match. Clear the search to sort.");
            }
            Action::Sort => {
                self.query.sort = self.query.sort.next();
                self.reload(cx);
            }
            Action::Category => {
                self.open_search(true, cx);
            }
            Action::Search => self.open_search(false, cx),
            Action::View => Self::toggle_view(cx),
            Action::Clear => {
                self.query.text.clear();
                self.query.category = None;
                self.category_name = None;
                self.reload(cx);
            }
            Action::Refresh => self.reload(cx),
            Action::OpenPage => {
                if let Some(submission) = selected {
                    cx.open(&submission.url);
                }
            }
            _ => {}
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, cx: &mut Ctx) {
        self.render(frame, area, cx);
    }
}

impl Browse {
    /// What the header shows while a download runs.
    pub fn activity(&self) -> Option<(String, Option<u16>)> {
        let download = self.download.as_ref()?;
        let percent = (download.total > 0)
            .then(|| u16::try_from(download.done * 100 / download.total).unwrap_or(100));
        Some((download.name.clone(), percent))
    }

    pub fn busy(&self) -> bool {
        self.loading || self.pending.is_some() || self.download.is_some()
    }

    pub fn on_reply(&mut self, reply: Reply, cx: &mut Ctx) {
        match reply {
            Reply::List {
                query,
                page,
                records,
                last,
            } => {
                if query != self.query || page != self.next_page {
                    return;
                }
                self.loading = false;
                self.last_page = last;
                self.next_page = page + 1;
                let seen: HashSet<u64> = self.results.iter().map(|s| s.id).collect();
                self.results
                    .extend(records.into_iter().filter(|s| !seen.contains(&s.id)));
                let visible = self.visible(cx).len();
                self.load_more_if_near_end(cx, visible);
            }
            Reply::Categories(tree) => {
                let mut rows = Vec::new();
                for root in tree {
                    rows.push(CategoryRow {
                        id: Some(root.id),
                        name: root.name,
                        items: root.items,
                        child: false,
                        parent: None,
                    });
                    let parent = rows.last().map(|row| row.name.clone()).unwrap_or_default();
                    rows.extend(root.children.into_iter().map(|child| CategoryRow {
                        id: Some(child.id),
                        name: child.name,
                        items: child.items,
                        child: true,
                        parent: Some(parent.clone()),
                    }));
                }
                self.categories = Categories::Loaded(rows);
                if self.search.active() {
                    self.suggestion = self.default_suggestion();
                }
            }
            Reply::Details {
                submission,
                details,
            } => {
                let Some((id, install)) = self.pending.take() else {
                    return;
                };
                if id != submission.id {
                    return;
                }
                let files = details.offered();
                if install && files.len() == 1 {
                    self.download(cx, submission, details, files[0].clone());
                    return;
                }
                if install && files.is_empty() {
                    cx.info(format!("{} has no files to download.", submission.name));
                    return;
                }
                self.page = Some(ModPage {
                    text: html::to_text(&details.text),
                    submission,
                    details,
                    file: ListState::default().with_selected((!files.is_empty()).then_some(0)),
                    files,
                    scroll: 0,
                });
            }
            Reply::Progress { done, total } => {
                if let Some(download) = &mut self.download {
                    download.done = done;
                    download.total = total;
                }
            }
            Reply::Downloaded { name, mods, dir } => self.on_downloaded(&name, mods, dir, cx),
            Reply::Failed { kind, message } => {
                match kind {
                    JobKind::List => self.loading = false,
                    JobKind::Categories => self.categories = Categories::NotLoaded,
                    JobKind::Details => self.pending = None,
                    JobKind::Download => self.download = None,
                }
                cx.error(message);
            }
        }
    }

    /// Hands a finished download to the core to install, turned on when
    /// the settings say so and no other variant of it is on.
    fn on_downloaded(&mut self, name: &str, mods: Vec<Incoming>, dir: TempDir, cx: &mut Ctx) {
        self.download = None;
        let id = mods.first().and_then(|m| m.metadata.game_banana_id);
        let variant_on = id.is_some()
            && cx.core.mods().iter().any(|m| {
                m.placement.is_loaded()
                    && m.metadata.as_ref().and_then(|meta| meta.game_banana_id) == id
            });
        let count = mods.len();
        let enable = cx.core.snapshot.settings.enable_downloads && count == 1 && !variant_on;
        let variants = cx.key(Action::Variants);
        let message = match count {
            1 if enable => format!("Installed and turned on {name}."),
            1 if variant_on => format!(
                "Installed {name} as another variant. {variants} on it in Installed switches."
            ),
            1 => format!("Installed {name}. Turn it on in Installed."),
            n => format!("Installed {n} variants of {name}. {variants} on one in Installed picks."),
        };
        // Not `mutate`: the download is done and must not be dropped
        // because another write is still running. Jobs queue.
        cx.core.send(
            Job::new(Request::Install { mods, enable })
                .done(message)
                .hold(dir),
        );
    }

    fn on_page_action(&mut self, action: Action, cx: &mut Ctx) {
        let Some(page) = &mut self.page else { return };
        match action {
            Action::Up | Action::Down | Action::Top | Action::Bottom => {
                let at = page.file.selected().unwrap_or(0);
                if let Some(to) = navigate(action, at, page.files.len(), 1) {
                    page.file.select(Some(to));
                }
            }
            Action::PageUp => page.scroll = page.scroll.saturating_sub(5),
            Action::PageDown => page.scroll = page.scroll.saturating_add(5),
            Action::Install | Action::Confirm => {
                let Some(page) = self.page.take() else { return };
                let file = page
                    .file
                    .selected()
                    .and_then(|at| page.files.get(at))
                    .cloned();
                if let Some(file) = file {
                    self.download(cx, page.submission, page.details, file);
                }
            }
            Action::OpenPage => {
                let url = page.submission.url.clone();
                cx.open(url);
            }
            Action::Cancel => self.page = None,
            _ => {}
        }
    }

    /// Starts the listing over for the current query.
    fn reload(&mut self, cx: &mut Ctx) {
        self.results.clear();
        self.selected = 0;
        self.next_page = 1;
        self.last_page = false;
        self.fetch(cx);
    }

    fn fetch(&mut self, cx: &mut Ctx) {
        self.loading = true;
        cx.net.send(NetJob::List {
            query: self.query.clone(),
            page: self.next_page,
        });
    }

    fn load_more_if_near_end(&mut self, cx: &mut Ctx, visible: usize) {
        // Also when filtering left the screen part empty.
        let near_end = if cx.config.ui.browse_view == BrowseView::Cards {
            self.selected + self.page_rows + self.card_columns.saturating_mul(2) >= visible
        } else {
            self.selected + LOAD_AHEAD >= visible || visible < self.page_rows
        };
        if near_end && !self.loading && !self.last_page && self.opened {
            self.fetch(cx);
        }
    }

    fn download(&mut self, cx: &mut Ctx, submission: Submission, details: Details, file: ModFile) {
        if self.download.is_some() {
            cx.info("One download at a time. Wait for this one to finish.");
            return;
        }
        self.download = Some(Download {
            name: submission.name.clone(),
            done: 0,
            total: file.size,
        });
        cx.net.send(NetJob::Download {
            submission,
            details,
            file,
        });
    }

    fn visible<'a>(&'a self, cx: &Ctx) -> Vec<&'a Submission> {
        visible(&self.results, cx.core.snapshot.settings.show_nsfw)
    }

    fn view(cx: &Ctx) -> BrowseView {
        cx.config.ui.browse_view
    }

    fn toggle_view(cx: &mut Ctx) {
        let next = cx.config.ui.browse_view.next();
        match cx.paths.save(Some("ui"), "browse_view", next.name()) {
            Ok(()) => *cx.effect = Some(super::Effect::ReloadConfig),
            Err(error) => cx.error(error),
        }
    }

    fn open_search(&mut self, category: bool, cx: &mut Ctx) {
        self.search
            .start(if category { "" } else { &self.query.text });
        self.suggestion = self.default_suggestion();
        if matches!(self.categories, Categories::NotLoaded) {
            self.categories = Categories::Loading;
            cx.net.send(NetJob::Categories);
        }
    }

    pub(super) fn suggestions(&self) -> Vec<Suggestion> {
        let mut out = Vec::new();
        let text = self.search.text().trim();
        if !text.is_empty() {
            out.push(Suggestion::Search);
        }
        let Categories::Loaded(rows) = &self.categories else {
            return out;
        };
        if text.is_empty() {
            out.push(Suggestion::All);
            out.extend((0..rows.len()).map(Suggestion::Category));
            return out;
        }
        let mut matches: Vec<(usize, u8)> = rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| {
                let rank = category_match_rank(&row.name, row.parent.as_deref(), text)?;
                Some((index, rank))
            })
            .collect();
        matches.sort_by_key(|(index, rank)| (*rank, *index));
        out.extend(
            matches
                .into_iter()
                .map(|(index, _)| Suggestion::Category(index)),
        );
        out
    }

    fn field_key(&mut self, key: KeyEvent, cx: &mut Ctx) {
        match key.code {
            KeyCode::Esc => self.search.stop(),
            KeyCode::Up | KeyCode::Down | KeyCode::Tab | KeyCode::BackTab => {
                let len = self.suggestions().len();
                if len > 0 {
                    let forward = matches!(key.code, KeyCode::Down | KeyCode::Tab);
                    self.suggestion = if forward {
                        (self.suggestion + 1) % len
                    } else {
                        self.suggestion.saturating_add(len - 1) % len
                    };
                }
            }
            KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let len = self.suggestions().len();
                if len > 0 {
                    self.suggestion = (self.suggestion + 1) % len;
                }
            }
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                let len = self.suggestions().len();
                if len > 0 {
                    self.suggestion = self.suggestion.saturating_add(len - 1) % len;
                }
            }
            KeyCode::Enter => self.pick_suggestion(cx),
            KeyCode::Backspace
                if self.search.text().is_empty() && self.query.category.is_some() =>
            {
                self.query.category = None;
                self.category_name = None;
                self.reload(cx);
            }
            _ => {
                if matches!(self.search.on_key(key), Edit::Changed) {
                    self.suggestion = self.default_suggestion();
                }
            }
        }
    }

    fn pick_suggestion(&mut self, cx: &mut Ctx) {
        let selected = self.suggestions().get(self.suggestion).copied();
        match selected {
            Some(Suggestion::Search) | None if !self.search.text().trim().is_empty() => {
                self.search.text().trim().clone_into(&mut self.query.text);
                self.search.stop();
                self.reload(cx);
            }
            Some(Suggestion::Search) | None => {
                self.query.text.clear();
                self.search.stop();
                self.reload(cx);
            }
            Some(Suggestion::All) => {
                self.query.category = None;
                self.category_name = None;
                self.query.text.clear();
                self.search.stop();
                self.reload(cx);
            }
            Some(Suggestion::Category(index)) => {
                let Categories::Loaded(rows) = &self.categories else {
                    return;
                };
                let Some(row) = rows.get(index) else { return };
                self.query.category = row.id;
                self.category_name = Some(row.path());
                self.query.text.clear();
                self.search.stop();
                self.reload(cx);
            }
        }
    }

    /// The row enter picks before any moving: with nothing typed, the
    /// category already shown, so enter alone only clears the text.
    fn default_suggestion(&self) -> usize {
        let current = self.query.category;
        self.suggestions()
            .iter()
            .position(|suggestion| match (suggestion, &self.categories) {
                (Suggestion::All, _) => current.is_none(),
                (Suggestion::Category(index), Categories::Loaded(rows)) => {
                    current.is_some() && rows.get(*index).is_some_and(|row| row.id == current)
                }
                _ => false,
            })
            .filter(|_| self.search.text().trim().is_empty())
            .unwrap_or(0)
    }
}

fn category_match_rank(name: &str, parent: Option<&str>, query: &str) -> Option<u8> {
    match_rank(name, query)
        .into_iter()
        .chain(parent.and_then(|parent| match_rank(parent, query)))
        .min()
}

fn match_rank(candidate: &str, query: &str) -> Option<u8> {
    let candidate = candidate.to_lowercase();
    let query = query.to_lowercase();
    if candidate.starts_with(&query) {
        Some(0)
    } else if candidate.contains(&query) {
        Some(1)
    } else {
        None
    }
}

fn visible(results: &[Submission], show_nsfw: bool) -> Vec<&Submission> {
    results.iter().filter(|s| show_nsfw || !s.nsfw()).collect()
}

#[cfg(test)]
mod tests {
    use super::category_match_rank;

    #[test]
    fn category_match_ranks_prefix_before_substring_without_case() {
        assert_eq!(category_match_rank("Haze", Some("Skins"), "ha"), Some(0));
        assert_eq!(category_match_rank("The Haze", None, "ha"), Some(1));
        assert_eq!(category_match_rank("Haze", Some("Skins"), "SKI"), Some(0));
        assert_eq!(
            category_match_rank("The Haze", Some("Haze Skins"), "ha"),
            Some(0)
        );
        assert_eq!(category_match_rank("Vindicta", None, "HA"), None);
    }
}
