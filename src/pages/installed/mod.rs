//! The Installed tab: every mod in the game folder in load order, grouped
//! into Global, Load order and Off, with the selected mod's details beside
//! the list.
//!
//! The selection follows a mod, not a row, so turning a mod on (which moves
//! it into the load order) keeps it selected.

mod view;

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use grimoire_core::backend::Request;
use grimoire_core::library::{self, InstalledMod, MetaKey, ModList, Placement};
use ratatui::crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::widgets::ListState;
use ratatui::Frame;

use super::{navigate, Ctx, Field, Page};
use crate::config::keys::Action;
use crate::core::{Core, Job};
use crate::ui::{Edit, TextInput};
use view::FileInfo;

const ACTIONS: &[Action] = &[
    Action::Search,
    Action::Toggle,
    Action::Grab,
    Action::MoveUp,
    Action::MoveDown,
    Action::Variants,
    Action::Global,
    Action::Delete,
    Action::Show,
    Action::Clear,
    Action::OpenPage,
    Action::OpenFolder,
    Action::Refresh,
    Action::Repair,
    Action::FixGameinfo,
    Action::Up,
    Action::Down,
    Action::PageUp,
    Action::PageDown,
    Action::Top,
    Action::Bottom,
];
const HINTS: &[(Action, &str)] = &[
    (Action::Toggle, "on/off"),
    (Action::Grab, "move"),
    (Action::Variants, "variants"),
    (Action::Global, "global"),
    (Action::Search, "filter"),
    (Action::Show, "show"),
    (Action::Delete, "delete"),
];

const GRAB_ACTIONS: &[Action] = &[
    Action::Confirm,
    Action::Cancel,
    Action::Grab,
    Action::Toggle,
    Action::Up,
    Action::Down,
    Action::PageUp,
    Action::PageDown,
    Action::Top,
    Action::Bottom,
];
const GRAB_HINTS: &[(Action, &str)] = &[
    (Action::Up, "load earlier"),
    (Action::Down, "load later"),
    (Action::Confirm, "drop"),
    (Action::Cancel, "cancel"),
];

const PICKER_ACTIONS: &[Action] = &[
    Action::Confirm,
    Action::Cancel,
    Action::Variants,
    Action::Up,
    Action::Down,
    Action::PageUp,
    Action::PageDown,
    Action::Top,
    Action::Bottom,
];
const PICKER_HINTS: &[(Action, &str)] = &[
    (Action::Confirm, "load only this one"),
    (Action::Cancel, "close"),
];

const CONFIRM_ACTIONS: &[Action] = &[Action::Confirm, Action::Cancel];
const CONFIRM_HINTS: &[(Action, &str)] = &[(Action::Confirm, "delete"), (Action::Cancel, "keep")];

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum Show {
    #[default]
    All,
    On,
    Off,
}

impl Show {
    fn next(self) -> Self {
        match self {
            Self::All => Self::On,
            Self::On => Self::Off,
            Self::Off => Self::All,
        }
    }

    fn admits(self, placement: Placement) -> bool {
        match self {
            Self::All => true,
            Self::On => placement.is_loaded(),
            Self::Off => !placement.is_loaded(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Global,
    LoadOrder,
    Off,
}

impl Section {
    fn of(placement: Placement) -> Self {
        match placement {
            Placement::Priority { .. } => Self::Global,
            Placement::Enabled { .. } => Self::LoadOrder,
            Placement::Disabled => Self::Off,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Global => "Global",
            Self::LoadOrder => "Load order",
            Self::Off => "Off",
        }
    }
}

#[derive(Default)]
pub struct Installed {
    selected: Option<MetaKey>,
    /// Where the selection was, so it lands nearby when its mod goes away.
    last_index: usize,
    list: ListState,
    page_rows: usize,
    filter: TextInput,
    query: String,
    show: Show,
    grab: Option<Grab>,
    variants: Option<VariantPicker>,
    confirm_delete: Option<(MetaKey, String)>,
    follow: Option<Follow>,
    file: Option<FileInfo>,
}

/// Where the selected mod will be once a job that moves it lands. Mods are
/// keyed by file name and every move renames the file, so the old key
/// can't find it.
struct Follow {
    /// The list the job started from. The job landed once the snapshot has
    /// a different one.
    before: Arc<Result<ModList, String>>,
    to: Target,
}

enum Target {
    /// The one key that wasn't there before: where a toggled mod went.
    NewKey,
    /// This place in the load order: where a moved mod went.
    LoadOrder(usize),
    Key(MetaKey),
}

/// A mod picked up to move. The list shows the new order as it moves, and
/// dropping it writes that order in one reorder.
struct Grab {
    key: MetaKey,
    order: Vec<MetaKey>,
    start: Vec<MetaKey>,
}

/// Every installed file of one GameBanana submission, to pick which loads.
struct VariantPicker {
    name: String,
    keys: Vec<MetaKey>,
    list: ListState,
}

impl Page for Installed {
    fn actions(&self) -> &'static [Action] {
        if self.confirm_delete.is_some() {
            CONFIRM_ACTIONS
        } else if self.variants.is_some() {
            PICKER_ACTIONS
        } else if self.grab.is_some() {
            GRAB_ACTIONS
        } else {
            ACTIONS
        }
    }

    fn hints(&self) -> &'static [(Action, &'static str)] {
        if self.confirm_delete.is_some() {
            CONFIRM_HINTS
        } else if self.variants.is_some() {
            PICKER_HINTS
        } else if self.grab.is_some() {
            GRAB_HINTS
        } else {
            HINTS
        }
    }

    fn modal(&self) -> bool {
        self.confirm_delete.is_some() || self.variants.is_some() || self.grab.is_some()
    }

    fn field(&self) -> Option<Field<'_>> {
        self.filter.active().then_some(Field {
            label: "Filter",
            input: &self.filter,
            hint: "enter keep  esc clear",
            inline: true,
        })
    }

    fn on_field_key(&mut self, key: KeyEvent, _cx: &mut Ctx) {
        match self.filter.on_key(key) {
            Edit::Submit(text) => text.trim().clone_into(&mut self.query),
            Edit::Cancel => self.query.clear(),
            Edit::Changed => self.filter.text().clone_into(&mut self.query),
            Edit::Unchanged => {}
        }
    }

    fn on_action(&mut self, action: Action, cx: &mut Ctx) {
        self.catch_up(cx.core);
        if self.confirm_delete.is_some() {
            if let (Action::Confirm, Some((key, name))) = (action, self.confirm_delete.take()) {
                cx.mutate(Job::new(Request::Delete(vec![key])).done(format!("Deleted {name}.")));
            }
            self.confirm_delete = None;
            return;
        }
        if self.variants.is_some() {
            self.on_picker_action(action, cx);
            return;
        }
        if self.grab.is_some() {
            self.on_grab_action(action, cx);
            return;
        }

        let (selected, at, len) = {
            let visible = self.visible(cx.core.mods());
            let at = self.cursor(&visible);
            let selected = at.and_then(|at| visible.get(at)).map(|m| (*m).clone());
            (selected, at.unwrap_or(0), visible.len())
        };
        if let Some(to) = navigate(action, at, len, self.page_rows) {
            let visible = self.visible(cx.core.mods());
            if let Some(m) = visible.get(to) {
                self.selected = Some(m.key.clone());
                self.last_index = to;
            }
            return;
        }
        match action {
            Action::Search => self.filter.start(&self.query),
            Action::Show => self.show = self.show.next(),
            Action::Clear => {
                self.query.clear();
                self.show = Show::All;
            }
            Action::Refresh => cx.core.request(Request::Rescan),
            Action::Repair => {
                cx.mutate(Job::new(Request::Repair).done("Finished the interrupted move."));
            }
            Action::FixGameinfo => {
                cx.mutate(Job::new(Request::RepairGameinfo).done("Fixed gameinfo.gi."));
            }
            Action::OpenFolder => {
                let folder = selected
                    .as_ref()
                    .and_then(|m| m.path.parent().map(Path::to_path_buf))
                    .or_else(|| {
                        let game = cx.core.snapshot.game()?;
                        Some(game.install.citadel().join("addons"))
                    });
                if let Some(folder) = folder {
                    cx.open(folder);
                }
            }
            _ => {
                if let Some(m) = selected {
                    self.on_mod_action(action, &m, cx);
                }
            }
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, cx: &mut Ctx) {
        self.render(frame, area, cx);
    }
}

impl Installed {
    fn on_mod_action(&mut self, action: Action, m: &InstalledMod, cx: &mut Ctx) {
        match action {
            Action::Toggle => {
                let enabling = !m.placement.is_loaded();
                let request = if enabling {
                    Request::Enable(m.key.clone())
                } else {
                    Request::Disable(m.key.clone())
                };
                let clash = enabling && m.variants(cx.core.mods()).any(|v| v.placement.is_loaded());
                let mut job = Job::new(request);
                if clash {
                    job = job.done(format!(
                        "Another variant of this mod is on too. {} picks one.",
                        cx.key(Action::Variants)
                    ));
                }
                self.send(cx, job, Target::NewKey);
            }
            Action::Global => {
                let global = matches!(m.placement, Placement::Priority { .. });
                let job = Job::new(Request::SetGlobal(m.key.clone(), !global));
                self.send(cx, job, Target::NewKey);
            }
            Action::Grab => {
                let order = library::enabled_order(cx.core.mods());
                if !order.contains(&m.key) {
                    cx.info(
                        "Only mods in the load order can move. Global mods load before all of them.",
                    );
                    return;
                }
                self.query.clear();
                self.show = Show::All;
                self.grab = Some(Grab {
                    key: m.key.clone(),
                    start: order.clone(),
                    order,
                });
            }
            Action::MoveUp | Action::MoveDown => {
                let delta = if action == Action::MoveUp { -1 } else { 1 };
                let order = library::enabled_order(cx.core.mods());
                let Some(to) = order
                    .iter()
                    .position(|key| *key == m.key)
                    .and_then(|from| from.checked_add_signed(delta))
                    .filter(|to| *to < order.len())
                else {
                    return;
                };
                if let Some(reordered) = library::move_to(&order, &m.key, to) {
                    self.send(
                        cx,
                        Job::new(Request::Reorder(reordered)),
                        Target::LoadOrder(to),
                    );
                }
            }
            Action::Variants => {
                let mut variants: Vec<&InstalledMod> = m.variants(cx.core.mods()).collect();
                if variants.is_empty() {
                    cx.info("No other variants of this mod are installed. Browse has the rest.");
                    return;
                }
                variants.push(m);
                variants.sort_by_key(|v| v.variant_name().unwrap_or_default().to_lowercase());
                let at = variants.iter().position(|v| v.key == m.key);
                self.variants = Some(VariantPicker {
                    name: m.display_name(),
                    keys: variants.iter().map(|v| v.key.clone()).collect(),
                    list: ListState::default().with_selected(at),
                });
            }
            Action::Delete => self.confirm_delete = Some((m.key.clone(), m.display_name())),
            Action::OpenPage => match m.metadata.as_ref().and_then(|meta| meta.game_banana_id) {
                Some(id) => cx.open(format!("https://gamebanana.com/mods/{id}")),
                None => cx.info("This mod isn't from GameBanana."),
            },
            _ => {}
        }
    }

    fn on_grab_action(&mut self, action: Action, cx: &mut Ctx) {
        let Some(grab) = &mut self.grab else { return };
        let Some(at) = grab.order.iter().position(|key| *key == grab.key) else {
            self.grab = None;
            return;
        };
        if let Some(to) = navigate(action, at, grab.order.len(), self.page_rows) {
            let key = grab.order.remove(at);
            grab.order.insert(to, key);
            return;
        }
        match action {
            Action::Confirm | Action::Grab | Action::Toggle => {
                if let Some(grab) = self.grab.take() {
                    if grab.order != grab.start {
                        let to = Target::LoadOrder(at);
                        self.send(cx, Job::new(Request::Reorder(grab.order)), to);
                    }
                }
            }
            Action::Cancel => self.grab = None,
            _ => {}
        }
    }

    /// Makes the chosen variant the only one of its mod that loads. It takes
    /// over the slot of a variant that was on, so the load order holds.
    fn on_picker_action(&mut self, action: Action, cx: &mut Ctx) {
        let Some(picker) = &mut self.variants else {
            return;
        };
        let at = picker.list.selected().unwrap_or(0);
        if let Some(to) = navigate(action, at, picker.keys.len(), self.page_rows) {
            picker.list.select(Some(to));
            return;
        }
        match action {
            Action::Confirm => {
                let Some(picker) = self.variants.take() else {
                    return;
                };
                let Some(chosen) = picker.keys.get(at) else {
                    return;
                };
                let mods = cx.core.mods();
                let loaded = |key: &MetaKey| {
                    mods.iter()
                        .any(|m| m.key == *key && m.placement.is_loaded())
                };
                let others: Vec<&MetaKey> = picker
                    .keys
                    .iter()
                    .filter(|key| *key != chosen && loaded(key))
                    .collect();
                let chosen_on = loaded(chosen);
                let (swap_from, rest) = match others.split_first() {
                    Some((first, rest)) if !chosen_on => (Some(*first), rest),
                    _ => (None, &others[..]),
                };
                let mut requests: Vec<Request> = rest
                    .iter()
                    .map(|key| Request::Disable((*key).clone()))
                    .collect();
                // The chosen one takes over a swapped variant's slot, and
                // with it that file name.
                let to = match swap_from {
                    Some(from) => {
                        requests.push(Request::SwapVariant(from.clone(), chosen.clone()));
                        Target::Key(from.clone())
                    }
                    None if !chosen_on => {
                        requests.push(Request::Enable(chosen.clone()));
                        Target::NewKey
                    }
                    None => Target::Key(chosen.clone()),
                };
                let name = mods
                    .iter()
                    .find(|m| m.key == *chosen)
                    .and_then(InstalledMod::variant_name)
                    .unwrap_or(&picker.name)
                    .to_owned();
                self.selected = Some(chosen.clone());
                if !requests.is_empty() {
                    let job = Job::all(requests).done(format!("Now loading {name}."));
                    self.send(cx, job, to);
                }
            }
            Action::Cancel | Action::Variants => self.variants = None,
            _ => {}
        }
    }

    /// Sends a job that moves the selected mod, and follows the mod to
    /// wherever `to` says it lands.
    fn send(&mut self, cx: &mut Ctx, job: Job, to: Target) {
        let before = cx.core.snapshot.game().map(|game| Arc::clone(&game.mods));
        if cx.mutate(job) {
            self.follow = before.map(|before| Follow { before, to });
        }
    }

    /// Moves the selection to where a landed job put the mod.
    fn catch_up(&mut self, core: &Core) {
        if self.follow.is_none() || core.busy() {
            return;
        }
        let (Some(follow), Some(game)) = (self.follow.take(), core.snapshot.game()) else {
            return;
        };
        // The same list means nothing changed: the job failed.
        if Arc::ptr_eq(&follow.before, &game.mods) {
            return;
        }
        let (Ok(before), Ok(now)) = (&*follow.before, &*game.mods) else {
            return;
        };
        if let Some(key) = landed(&before.mods, &now.mods, follow.to) {
            self.selected = Some(key);
        }
    }

    /// The mods on screen, in screen order. While a mod is grabbed the load
    /// order follows the grab, not the disk.
    fn visible<'a>(&self, mods: &'a [InstalledMod]) -> Vec<&'a InstalledMod> {
        if let Some(grab) = &self.grab {
            let by_key: HashMap<&MetaKey, &InstalledMod> =
                mods.iter().map(|m| (&m.key, m)).collect();
            let in_section = |section| {
                mods.iter()
                    .filter(move |m| Section::of(m.placement) == section)
            };
            return in_section(Section::Global)
                .chain(grab.order.iter().filter_map(|key| by_key.get(key).copied()))
                .chain(in_section(Section::Off))
                .collect();
        }
        let query = self.query.trim().to_lowercase();
        mods.iter()
            .filter(|m| self.show.admits(m.placement) && matches(m, &query))
            .collect()
    }

    /// Index of the selected mod in `visible`. When it's gone, the mod that
    /// took its place.
    fn cursor(&mut self, visible: &[&InstalledMod]) -> Option<usize> {
        if visible.is_empty() {
            return None;
        }
        let at = self
            .selected
            .as_ref()
            .and_then(|key| visible.iter().position(|m| m.key == *key))
            .unwrap_or_else(|| self.last_index.min(visible.len() - 1));
        self.selected = Some(visible[at].key.clone());
        self.last_index = at;
        Some(at)
    }
}

/// The key a mod has after a job moved it to `to`.
fn landed(before: &[InstalledMod], now: &[InstalledMod], to: Target) -> Option<MetaKey> {
    match to {
        Target::Key(key) => Some(key),
        Target::LoadOrder(at) => library::enabled_order(now).get(at).cloned(),
        Target::NewKey => {
            let old: HashSet<&MetaKey> = before.iter().map(|m| &m.key).collect();
            let mut fresh = now.iter().filter(|m| !old.contains(&m.key));
            match (fresh.next(), fresh.next()) {
                (Some(m), None) => Some(m.key.clone()),
                _ => None,
            }
        }
    }
}

fn matches(m: &InstalledMod, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let fields = std::iter::once(m.display_name())
        .chain(std::iter::once(m.key.to_string()))
        .chain(m.metadata.iter().flat_map(|meta| {
            [
                meta.author.as_deref(),
                meta.category_name.as_deref(),
                meta.locker_hero.as_deref(),
                meta.variant_label.as_deref(),
                meta.file_description.as_deref(),
            ]
            .into_iter()
            .flatten()
            .map(str::to_owned)
        }))
        .map(|field| field.to_lowercase())
        .collect::<Vec<_>>();
    let query = query.to_lowercase();
    query
        .split_whitespace()
        .all(|word| fields.iter().any(|field| field.contains(word)))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use grimoire_core::metadata::ModMetadata;

    fn installed(key: &str, placement: Placement, name: Option<&str>) -> InstalledMod {
        InstalledMod {
            key: MetaKey::for_path(Path::new(key)),
            path: PathBuf::from(key),
            placement,
            metadata: name.map(|name| ModMetadata {
                mod_name: Some(name.to_owned()),
                author: Some("someone".to_owned()),
                ..ModMetadata::default()
            }),
        }
    }

    fn mods() -> Vec<InstalledMod> {
        vec![
            installed(
                "pak01_dir.vpk",
                Placement::Enabled { root: 0, slot: 1 },
                Some("Blue Haze"),
            ),
            installed(
                "pak02_dir.vpk",
                Placement::Enabled { root: 0, slot: 2 },
                None,
            ),
            installed("old_dir.vpk", Placement::Disabled, Some("Red Haze")),
        ]
    }

    #[test]
    fn selection_follows_the_mod_when_it_moves() {
        let mut page = Installed::default();
        let before = mods();
        page.selected = Some(before[2].key.clone());
        let visible = page.visible(&before);
        assert_eq!(page.cursor(&visible), Some(2));

        // Turned on, it moves to the top of the load order.
        let mut after = mods();
        after[2].placement = Placement::Enabled { root: 0, slot: 1 };
        after.rotate_right(1);
        let visible = page.visible(&after);
        assert_eq!(page.cursor(&visible), Some(0));
    }

    #[test]
    fn selection_lands_nearby_when_the_mod_goes() {
        let mut page = Installed::default();
        let list = mods();
        page.selected = Some(list[2].key.clone());
        let visible = page.visible(&list);
        page.cursor(&visible);
        let gone = &list[..2];
        let visible = page.visible(gone);
        assert_eq!(page.cursor(&visible), Some(1));
    }

    #[test]
    fn filter_and_show_narrow_the_list() {
        let mut page = Installed::default();
        let list = mods();
        page.query = "HAZE".to_owned();
        assert_eq!(page.visible(&list).len(), 2);
        page.show = Show::Off;
        assert_eq!(page.visible(&list).len(), 1);
        page.query = "someone".to_owned();
        page.show = Show::On;
        assert_eq!(page.visible(&list).len(), 1);
    }

    #[test]
    fn search_words_can_match_different_fields_in_any_order() {
        let mut list = mods();
        list[0].metadata.as_mut().unwrap().mod_name = Some("Haze Black Recolor".to_owned());
        assert!(matches(&list[0], "black haze"));
        assert!(matches(&list[0], "SOMEONE black"));
        assert!(!matches(&list[0], "black missing"));
    }

    #[test]
    fn landed_finds_a_moved_mod() {
        let before = mods();
        // Disabling pak02 renames it into .disabled/.
        let mut now = mods();
        now[1] = installed(".disabled/thing_dir.vpk", Placement::Disabled, None);
        assert_eq!(
            landed(&before, &now, Target::NewKey).map(|k| k.to_string()),
            Some("thing_dir.vpk".to_owned())
        );
        // Two new keys: too ambiguous to guess.
        now[0] = installed(
            "pak09_dir.vpk",
            Placement::Enabled { root: 0, slot: 9 },
            None,
        );
        assert_eq!(landed(&before, &now, Target::NewKey), None);
        assert_eq!(
            landed(&before, &before, Target::LoadOrder(1)).map(|k| k.to_string()),
            Some("pak02_dir.vpk".to_owned())
        );
    }

    #[test]
    fn grab_shows_the_new_order() {
        let mut page = Installed::default();
        let list = mods();
        let order = vec![list[1].key.clone(), list[0].key.clone()];
        page.grab = Some(Grab {
            key: list[0].key.clone(),
            start: order.clone(),
            order,
        });
        let visible: Vec<&str> = page.visible(&list).iter().map(|m| m.key.as_str()).collect();
        assert_eq!(visible, ["pak02_dir.vpk", "pak01_dir.vpk", "old_dir.vpk"]);
    }
}
