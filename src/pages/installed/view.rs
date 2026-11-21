//! Drawing the Installed tab: the grouped list, the details pane and the
//! popups. Nothing here changes what's on disk.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use grimoire_core::backend::GameState;
use grimoire_core::gameinfo::GameinfoStatus;
use grimoire_core::library::{InstalledMod, Placement};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, HighlightSpacing, List, ListItem, Paragraph, Wrap};
use ratatui::Frame;
use unicode_width::UnicodeWidthChar;

use super::{Installed, Section, Show, VariantPicker};
use crate::config::keys::Action;
use crate::config::theme::Theme;
use crate::config::Config;
use crate::pages::Ctx;
use crate::ui;

impl Installed {
    pub(super) fn render(&mut self, frame: &mut Frame, area: Rect, cx: &mut Ctx) {
        let config = cx.config;
        self.catch_up(cx.core);
        // Cheap (it's Arcs), and frees `cx` for the thumbnail loader below.
        let snapshot = cx.core.snapshot.clone();
        let problem = match &snapshot.game {
            GameState::NotSet => vec![
                Line::from("No Deadlock folder yet."),
                Line::from(format!(
                    "Set it in Settings, or press {} there to find it.",
                    cx.key(Action::Detect)
                )),
            ],
            GameState::Invalid { path, reason } => vec![
                Line::from(format!("{} isn't usable: {reason}.", path.display())),
                Line::from("Fix it in Settings."),
            ],
            GameState::Ready(game) => match &*game.mods {
                Err(err) => vec![Line::from(err.clone())],
                Ok(_) => Vec::new(),
            },
        };
        let Some(list) = snapshot
            .game()
            .and_then(|game| game.mods.as_ref().as_ref().ok())
        else {
            ui::notice(
                frame,
                config,
                area,
                ui::panel(config, " Installed "),
                problem,
            );
            return;
        };

        let mods = &list.mods;
        let counts = Counts::of(mods);
        let mut title_spans = self.title(config);
        title_spans.extend(counts.line(config));
        let (left, right) = ui::list_frame(frame, config, area, Line::from(title_spans), true);

        let banners = Self::banners(cx);
        let [banner_area, list_area] = Layout::vertical([
            Constraint::Length(u16::try_from(banners.len()).unwrap_or(0)),
            Constraint::Min(0),
        ])
        .areas(left);
        if !banners.is_empty() {
            frame.render_widget(Paragraph::new(banners), banner_area);
        }

        if mods.is_empty() {
            let lines = vec![
                Line::from("No mods installed yet."),
                Line::from(format!(
                    "Find some in Browse ({}).",
                    cx.key(Action::BrowseTab)
                )),
            ];
            ui::notice(frame, config, list_area, Block::new(), lines);
            self.draw_details(frame, right, None, cx);
            return;
        }

        let [search_area, rows_area] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(list_area);
        self.page_rows = usize::from(rows_area.height.saturating_sub(2));
        let visible = self.visible(mods);
        self.draw_search(frame, search_area, visible.len(), mods.len(), config);
        let cursor = self.cursor(&visible);
        if visible.is_empty() {
            let lines = vec![
                Line::from("Nothing matches."),
                Line::from(format!("{} clears the filter.", cx.key(Action::Clear))),
            ];
            ui::notice(frame, config, rows_area, Block::new(), lines);
            self.draw_details(frame, right, None, cx);
            return;
        }

        self.draw_list(frame, rows_area, &visible, cursor, mods, config);

        let selected = cursor.and_then(|at| visible.get(at)).copied();
        let variants = selected.map_or(0, |m| m.variants(mods).count());
        self.draw_details(frame, right, selected.map(|m| (m, variants)), cx);

        if let Some(picker) = &mut self.variants {
            draw_picker(frame, area, picker, mods, config);
        }
        if let Some((_, name)) = &self.confirm_delete {
            draw_confirm_delete(frame, area, name, cx);
        }
    }

    fn draw_search(
        &self,
        frame: &mut Frame,
        area: Rect,
        shown: usize,
        total: usize,
        config: &Config,
    ) {
        let chips = match self.show {
            Show::All => Vec::new(),
            Show::On => vec!["On only".to_owned()],
            Show::Off => vec!["Off only".to_owned()],
        };
        let filtered = !self.query.trim().is_empty() || self.show != Show::All;
        let bar = ui::SearchBar {
            input: self.filter.active().then_some(&self.filter),
            query: &self.query,
            placeholder: "Filter installed mods",
            chips,
            note: filtered.then(|| format!("{shown} of {total}")),
        };
        ui::search_bar(frame, config, area, &bar);
    }

    /// The rows, with a rule above each section.
    fn draw_list(
        &mut self,
        frame: &mut Frame,
        inner: Rect,
        visible: &[&InstalledMod],
        cursor: Option<usize>,
        mods: &[InstalledMod],
        config: &Config,
    ) {
        let width = usize::from(inner.width.saturating_sub(1));
        let columns = Columns::for_width(width);
        let mut items = Vec::with_capacity(visible.len() + 3);
        let mut cursor_row = 0;
        let mut section = None;
        for (i, m) in visible.iter().enumerate() {
            let this = Section::of(m.placement);
            if section != Some(this) {
                section = Some(this);
                let count = visible
                    .iter()
                    .filter(|v| Section::of(v.placement) == this)
                    .count();
                items.push(ListItem::new(ui::rule(
                    config,
                    this.label(),
                    Some(count),
                    inner.width.saturating_sub(1),
                )));
            }
            if Some(i) == cursor {
                cursor_row = items.len();
            }
            let position = self.grab.as_ref().and_then(|grab| {
                let at = grab.order.iter().position(|key| *key == m.key)?;
                Some((at, grab.key == m.key))
            });
            items.push(row(config, m, mods, position, columns, &self.query));
        }
        self.list.select(Some(cursor_row));
        let list = List::new(items)
            .highlight_style(config.theme.selected())
            .highlight_symbol(Line::from("\u{258c}").style(config.theme.accent()))
            .highlight_spacing(HighlightSpacing::Always);
        frame.render_stateful_widget(list, inner, &mut self.list);
    }

    fn title(&self, config: &Config) -> Vec<Span<'static>> {
        let theme = &config.theme;
        let mut spans = vec![Span::styled(" Installed ", theme.title())];
        if self.grab.is_some() {
            spans.push(Span::styled(" Moving ", theme.accent()));
        }
        spans
    }

    /// Problems that stop mods from loading, above the list.
    fn banners(cx: &Ctx) -> Vec<Line<'static>> {
        let Some(game) = cx.core.snapshot.game() else {
            return Vec::new();
        };
        let theme = &cx.config.theme;
        let warn = ui::glyphs(cx.config).warn;
        let mut lines = Vec::new();
        let gameinfo = match &*game.gameinfo {
            Ok(GameinfoStatus::Configured) => None,
            Ok(GameinfoStatus::NeedsFix(_)) => Some(format!(
                "gameinfo.gi isn't set up for mods, so they won't load. {} fixes it.",
                cx.key(Action::FixGameinfo)
            )),
            Ok(GameinfoStatus::Missing) => {
                Some("gameinfo.gi is missing. Verify the game files in Steam.".to_owned())
            }
            Ok(GameinfoStatus::Unrepairable) => Some(
                "gameinfo.gi has no FileSystem section. Verify the game files in Steam.".to_owned(),
            ),
            Err(err) => Some(err.clone()),
        };
        if let Some(text) = gameinfo {
            lines.push(Line::styled(
                format!(" {warn} {text}"),
                Style::new().fg(theme.warn),
            ));
        }
        if matches!(&*game.mods, Ok(list) if !list.leftovers.is_empty()) {
            lines.push(Line::styled(
                format!(
                    " {warn} A load order change was interrupted. {} finishes it.",
                    cx.key(Action::Repair)
                ),
                Style::new().fg(theme.warn),
            ));
        }
        lines
    }

    fn draw_details(
        &mut self,
        frame: &mut Frame,
        area: Option<Rect>,
        selected: Option<(&InstalledMod, usize)>,
        cx: &mut Ctx,
    ) {
        let Some(area) = area else { return };
        let config = cx.config;
        let theme = &config.theme;
        let inner = area;
        let Some((m, variants)) = selected else {
            return;
        };
        let meta = m.metadata.as_ref();

        let thumbnail = meta.and_then(|meta| meta.thumbnail_url.as_deref());
        let hide_nsfw = meta.is_some_and(|meta| meta.nsfw) && !cx.core.snapshot.settings.show_nsfw;
        let image_rows = if thumbnail.is_some() && cx.thumbs.enabled() {
            cx.thumbs.rows_for(inner.width).min(inner.height / 2)
        } else {
            0
        };
        let [image, _, text] = Layout::vertical([
            Constraint::Length(image_rows),
            Constraint::Length(u16::from(image_rows > 0)),
            Constraint::Min(0),
        ])
        .areas(inner);
        if let Some(url) = thumbnail.filter(|_| image_rows > 0) {
            if hide_nsfw {
                let middle = Rect::new(image.x, image.y + image.height / 2, image.width, 1);
                let note = Line::from("NSFW preview hidden")
                    .style(theme.faint())
                    .centered();
                frame.render_widget(note, middle);
            } else {
                cx.thumbs.draw(frame, image, url, theme);
            }
        }

        let lines = self.detail_lines(m, variants, cx);
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), text);
    }

    /// Everything known about one mod, under its thumbnail.
    fn detail_lines(&mut self, m: &InstalledMod, variants: usize, cx: &Ctx) -> Vec<Line<'static>> {
        let config = cx.config;
        let theme = &config.theme;
        let meta = m.metadata.as_ref();
        let sep = ui::glyphs(config).sep;
        let mut lines = vec![Line::styled(
            m.display_name(),
            ui::bold(Style::new().fg(theme.fg)),
        )];
        if let Some(author) = meta.and_then(|meta| meta.author.as_deref()) {
            lines.push(Line::styled(format!("by {author}"), theme.muted()));
        }
        lines.push(Line::default());
        let status = match m.placement {
            Placement::Priority { .. } => {
                Span::styled("Global, loads first", ui::bold(theme.accent()))
            }
            Placement::Enabled { root: 0, slot } => Span::styled(
                format!("On, slot {slot:02}"),
                ui::bold(Style::new().fg(theme.ok)),
            ),
            Placement::Enabled { root, slot } => Span::styled(
                format!("On, addons{root} slot {slot:02}"),
                ui::bold(Style::new().fg(theme.ok)),
            ),
            Placement::Disabled => Span::styled("Off", ui::bold(theme.faint())),
        };
        lines.push(ui::field(config, "Status", status));
        let category = meta.and_then(|meta| {
            let hero = meta.locker_hero.as_deref();
            match (meta.category_name.as_deref(), hero) {
                (Some(category), Some(hero)) => Some(format!("{category}{sep}{hero}")),
                (category, hero) => category.or(hero).map(str::to_owned),
            }
        });
        if let Some(category) = category {
            lines.push(ui::field(config, "Category", category));
        }
        if variants > 0 {
            let name = m.variant_name().unwrap_or("this one");
            lines.push(ui::field(config, "Variant", name.to_owned()));
        }
        if let Some(info) = self.file_info(&m.path) {
            lines.push(ui::field(config, "Size", ui::size(info.size)));
            if let Some(modified) = info.modified {
                let secs = modified
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
                lines.push(ui::field(config, "Added", ui::date(secs)));
            }
        }
        lines.push(ui::field(config, "File", m.key.to_string()));
        if let Some(id) = meta.and_then(|meta| meta.game_banana_id) {
            lines.push(ui::field(
                config,
                "Page",
                format!("gamebanana.com/mods/{id}"),
            ));
        }
        if meta.is_some_and(|meta| meta.nsfw) {
            lines.push(ui::field(
                config,
                "Rating",
                Span::styled("NSFW", Style::new().fg(theme.warn)),
            ));
        }
        if variants > 0 {
            lines.push(Line::default());
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{} variants installed. ", variants + 1),
                    theme.muted(),
                ),
                Span::styled(cx.key(Action::Variants), theme.key()),
                Span::styled(" switches.", theme.muted()),
            ]));
        }
        if let Some(description) = meta.and_then(|meta| meta.file_description.as_deref()) {
            lines.push(Line::default());
            lines.push(Line::styled(description.trim().to_owned(), theme.muted()));
        }
        lines
    }

    /// Size on disk with chunk files, and when it was added. Read once per
    /// selected file.
    fn file_info(&mut self, path: &Path) -> Option<&FileInfo> {
        if self.file.as_ref().is_none_or(|info| info.path != path) {
            self.file = read_file_info(path);
        }
        self.file.as_ref()
    }
}

pub(super) struct FileInfo {
    path: PathBuf,
    size: u64,
    modified: Option<SystemTime>,
}

fn read_file_info(path: &Path) -> Option<FileInfo> {
    let meta = fs::metadata(path).ok()?;
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    let stem = name.strip_suffix("_dir.vpk")?;
    let chunk_prefix = format!("{stem}_");
    let chunks: u64 = path
        .parent()
        .and_then(|dir| fs::read_dir(dir).ok())
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            name.strip_prefix(&chunk_prefix)
                .and_then(|rest| rest.strip_suffix(".vpk"))
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        })
        .filter_map(|entry| entry.metadata().ok())
        .map(|meta| meta.len())
        .sum();
    Some(FileInfo {
        path: path.to_path_buf(),
        size: meta.len() + chunks,
        modified: meta.modified().ok(),
    })
}

struct Counts {
    global: usize,
    on: usize,
    off: usize,
}

impl Counts {
    fn of(mods: &[InstalledMod]) -> Self {
        let mut counts = Self {
            global: 0,
            on: 0,
            off: 0,
        };
        for m in mods {
            match Section::of(m.placement) {
                Section::Global => counts.global += 1,
                Section::LoadOrder => counts.on += 1,
                Section::Off => counts.off += 1,
            }
        }
        counts
    }

    fn line(&self, config: &Config) -> Vec<Span<'static>> {
        let theme = &config.theme;
        let glyphs = ui::glyphs(config);
        let mut spans = vec![Span::raw(" ")];
        if self.global > 0 {
            spans.push(Span::styled(
                format!("{} {} ", glyphs.global, self.global),
                theme.accent(),
            ));
        }
        spans.push(Span::styled(
            format!("{} {} ", glyphs.on, self.on),
            Style::new().fg(theme.ok),
        ));
        spans.push(Span::styled(
            format!("{} {} ", glyphs.off, self.off),
            theme.faint(),
        ));
        spans
    }
}

/// Column widths for a row, dropping category and author when narrow.
#[derive(Clone, Copy)]
struct Columns {
    name: usize,
    category: usize,
    author: usize,
}

impl Columns {
    const MARK: usize = 2;
    const SLOT: usize = 6;

    fn for_width(width: usize) -> Self {
        let available = width.saturating_sub(Self::MARK + Self::SLOT);
        let (category, author) = if available >= 42 {
            ((available / 5).clamp(13, 20), (available / 4).clamp(15, 24))
        } else if available >= 20 {
            (0, 14)
        } else {
            (0, 0)
        };
        let name = available.saturating_sub(category + author).min(48);
        Self {
            name,
            category,
            author,
        }
    }
}

/// One mod's row. `position` is its place in a grab and whether it's the
/// grabbed one.
fn row(
    config: &Config,
    m: &InstalledMod,
    mods: &[InstalledMod],
    position: Option<(usize, bool)>,
    columns: Columns,
    query: &str,
) -> ListItem<'static> {
    let theme = &config.theme;
    let glyphs = ui::glyphs(config);
    let grabbed = position.is_some_and(|(_, grabbed)| grabbed);
    let (mark, mark_style) = match m.placement {
        _ if grabbed => (glyphs.grabbed, theme.key()),
        Placement::Priority { .. } => (glyphs.global, theme.accent()),
        Placement::Enabled { .. } => (glyphs.on, Style::new().fg(theme.ok)),
        Placement::Disabled => (glyphs.off, theme.faint()),
    };
    let slot = match (position, m.placement) {
        (Some((at, _)), _) => format!("{:02}", at + 1),
        (None, Placement::Priority { .. } | Placement::Disabled) => String::new(),
        (None, Placement::Enabled { root: 0, slot }) => format!("{slot:02}"),
        (None, Placement::Enabled { root, slot }) => format!("{root}:{slot:02}"),
    };
    let name_style = if grabbed {
        theme.key()
    } else if m.placement.is_loaded() {
        Style::new().fg(theme.fg)
    } else {
        theme.muted()
    };

    let meta = m.metadata.as_ref();
    let display_name = m.display_name();
    let mut name = highlighted(&display_name, query, usize::MAX, name_style, theme);
    if m.variants(mods).next().is_some() {
        if let Some(variant) = m.variant_name() {
            name.extend(highlighted(
                &format!("{}{variant}", glyphs.sep),
                query,
                usize::MAX,
                theme.muted(),
                theme,
            ));
        }
    }
    if meta.is_some_and(|meta| meta.nsfw) {
        name.push((" 18+".to_owned(), Style::new().fg(theme.warn)));
    }

    let mut spans = vec![
        Span::styled(format!("{mark} "), mark_style),
        Span::styled(ui::cell(&slot, Columns::SLOT), theme.faint()),
    ];
    spans.extend(ui::fit_spans(name, columns.name));
    if columns.category > 0 {
        let category = meta
            .and_then(|meta| meta.category_name.as_deref())
            .unwrap_or("");
        spans.extend(
            highlighted(
                &format!("  {category}"),
                query,
                columns.category,
                theme.faint(),
                theme,
            )
            .into_iter()
            .map(|(text, style)| Span::styled(text, style)),
        );
    }
    if columns.author > 0 {
        let author = meta.and_then(|meta| meta.author.as_deref()).unwrap_or("");
        spans.extend(
            highlighted(
                &format!("  {author}"),
                query,
                columns.author,
                theme.muted(),
                theme,
            )
            .into_iter()
            .map(|(text, style)| Span::styled(text, style)),
        );
    }
    ListItem::new(Line::from(spans))
}

fn highlighted(
    text: &str,
    query: &str,
    width: usize,
    base: Style,
    theme: &Theme,
) -> Vec<(String, Style)> {
    let ranges = match_ranges(text, query);
    let fitted = ui::fit(text, width);
    let clipped = fitted != text;
    let content_width = if clipped {
        width.saturating_sub(1)
    } else {
        width
    };
    let mut pieces: Vec<(String, Style)> = Vec::new();
    let mut used = 0;
    for (offset, ch) in text.char_indices() {
        let char_width = ch.width().unwrap_or(0);
        if used + char_width > content_width {
            break;
        }
        let matched = ranges
            .iter()
            .any(|(start, end)| *start <= offset && offset < *end);
        let style = if matched {
            base.fg(theme.accent).add_modifier(Modifier::BOLD)
        } else {
            base
        };
        if let Some((last, _)) = pieces
            .last_mut()
            .filter(|(_, last_style)| *last_style == style)
        {
            last.push(ch);
        } else {
            pieces.push((ch.to_string(), style));
        }
        used += char_width;
    }
    if clipped && width > 0 {
        pieces.push(("\u{2026}".to_owned(), base));
        used += 1;
    }
    if width != usize::MAX && used < width {
        pieces.push((" ".repeat(width - used), base));
    }
    pieces
}

fn match_ranges(text: &str, query: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut lowered = String::new();
    let mut map = Vec::new();
    for (offset, ch) in text.char_indices() {
        let end = offset + ch.len_utf8();
        for lower in ch.to_lowercase() {
            let start = lowered.len();
            lowered.push(lower);
            map.push((start, lowered.len(), offset, end));
        }
    }
    for word in query.split_whitespace().map(str::to_lowercase) {
        for (start, _) in lowered.match_indices(&word) {
            let end = start + word.len();
            let Some(first) = map.iter().find(|(_, low_end, _, _)| *low_end > start) else {
                continue;
            };
            let Some(last) = map
                .iter()
                .rev()
                .find(|(low_start, _, _, _)| *low_start < end)
            else {
                continue;
            };
            ranges.push((first.2, last.3));
        }
    }
    ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in ranges {
        if let Some(last) = merged
            .last_mut()
            .filter(|(_, previous_end)| *previous_end >= start)
        {
            last.1 = last.1.max(end);
        } else {
            merged.push((start, end));
        }
    }
    merged
}

fn draw_picker(
    frame: &mut Frame,
    area: Rect,
    picker: &mut VariantPicker,
    mods: &[InstalledMod],
    config: &Config,
) {
    let theme = &config.theme;
    let glyphs = ui::glyphs(config);
    let items: Vec<ListItem> = picker
        .keys
        .iter()
        .filter_map(|key| mods.iter().find(|m| m.key == *key))
        .map(|m| {
            let loaded = m.placement.is_loaded();
            let (mark, style) = if loaded {
                (glyphs.on, Style::new().fg(theme.ok))
            } else {
                (glyphs.off, theme.faint())
            };
            let name = m
                .variant_name()
                .map_or_else(|| m.display_name(), str::to_owned);
            ListItem::new(Line::from(vec![
                Span::styled(format!("{mark} "), style),
                Span::raw(name),
            ]))
        })
        .collect();
    let height = u16::try_from(items.len() + 2).unwrap_or(u16::MAX);
    let width = area.width.saturating_sub(8).min(70);
    let inner = ui::popup(
        frame,
        config,
        area,
        (width, height),
        &format!("Variants of {}", picker.name),
    );
    let list = List::new(items)
        .highlight_style(theme.selected())
        .highlight_symbol(Line::from("\u{258c}").style(theme.accent()))
        .highlight_spacing(HighlightSpacing::Always);
    frame.render_stateful_widget(list, inner, &mut picker.list);
}

fn draw_confirm_delete(frame: &mut Frame, area: Rect, name: &str, cx: &Ctx) {
    let config = cx.config;
    let inner = ui::popup(frame, config, area, (60, 7), "Delete");
    let lines = vec![
        Line::from(format!("Delete {name}?")).style(ui::bold(Style::new())),
        Line::from("The file is removed from disk. This can't be undone.")
            .style(config.theme.muted()),
        Line::default(),
        Line::from(vec![
            Span::styled(cx.key(Action::Confirm), config.theme.key()),
            Span::styled(" delete   ", config.theme.muted()),
            Span::styled(cx.key(Action::Cancel), config.theme.key()),
            Span::styled(" keep", config.theme.muted()),
        ]),
    ];
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;
    use unicode_width::UnicodeWidthStr;

    fn test_theme() -> Theme {
        Theme {
            accent: Color::Yellow,
            ..Theme::default()
        }
    }

    #[test]
    fn highlights_each_matching_word_and_merges_overlaps() {
        let ranges = match_ranges("Haze Black Haze", "black haze aze");
        assert_eq!(ranges, [(0, 4), (5, 10), (11, 15)]);
    }

    #[test]
    fn highlighting_respects_truncation_and_wide_cells() {
        let theme = test_theme();
        let base = Style::new().fg(Color::Gray);
        let spans = highlighted("全景 Haze", "haze", 6, base, &theme);
        let text = spans
            .iter()
            .map(|(text, _)| text.as_str())
            .collect::<String>();
        assert_eq!(text, "全景 …");
        assert_eq!(text.width(), 6);
        assert!(spans.iter().all(|(text, _)| text.width() <= 6));

        let spans = highlighted("Haze Black", "black", 6, base, &theme);
        let text = spans
            .iter()
            .map(|(text, _)| text.as_str())
            .collect::<String>();
        assert_eq!(text, "Haze …");
        assert_eq!(text.width(), 6);
        let spans = highlighted("Haze Black", "black", 10, base, &theme);
        assert!(spans
            .iter()
            .any(|(text, style)| text == "Black" && style.fg == Some(Color::Yellow)));
    }
}
