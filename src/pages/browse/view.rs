//! Drawing the Browse tab: the search line and its suggestions, the results
//! as a list or as cards, the details pane and a mod's page.

use std::collections::HashSet;

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Clear, HighlightSpacing, List, ListItem, ListState, Padding, Paragraph, Wrap,
};
use ratatui::Frame;

use super::{visible, Browse, Categories, ModPage, Suggestion};
use crate::config::{BrowseView, Config};
use crate::gamebanana::Submission;
use crate::pages::Ctx;
use crate::ui;

/// Cards are about this wide; the grid fits as many as it can.
const CARD_WIDTH: u16 = 34;
const CARD_GAP: u16 = 1;

impl Browse {
    pub(super) fn render(&mut self, frame: &mut Frame, area: Rect, cx: &mut Ctx) {
        let config = cx.config;
        let list_view = config.ui.browse_view == BrowseView::List;
        let (left, right) = ui::list_frame(frame, config, area, " Browse ".into(), list_view);

        let installed: HashSet<u64> = cx
            .core
            .mods()
            .iter()
            .filter_map(|m| m.metadata.as_ref()?.game_banana_id)
            .collect();
        // Owned, so drawing the cards can borrow `self` mutably.
        let visible: Vec<Submission> = visible(&self.results, cx.core.snapshot.settings.show_nsfw)
            .into_iter()
            .cloned()
            .collect();
        let shown = visible.len();
        self.selected = self.selected.min(shown.saturating_sub(1));

        let [bar, body] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(left);
        self.draw_search(frame, bar, shown, cx);
        if visible.is_empty() {
            let message = if self.loading {
                format!("{} Loading", ui::spinner(cx.started))
            } else if self.opened {
                "Nothing here.".to_owned()
            } else {
                String::new()
            };
            ui::notice(frame, config, body, Block::new(), vec![Line::from(message)]);
        } else if list_view {
            self.draw_list(frame, body, &visible, &installed, cx);
        } else {
            self.draw_cards(frame, body, &visible, &installed, cx);
        }

        if let Some(area) = right {
            self.draw_details(frame, area, visible.get(self.selected), &installed, cx);
        }
        if let Some(page) = &mut self.page {
            draw_mod_page(frame, area, page, cx);
        }
        if self.search.active() {
            // In the grid the dropdown stays within the first card, so no
            // image is left partly covered: kitty can't draw the rest of a
            // thumbnail row once its first cell is hidden.
            let width = if list_view {
                64
            } else {
                let (_, card, x) = grid(body);
                x - body.x + card
            };
            self.draw_suggestions(frame, bar, width, area.bottom(), cx);
        }
        // Only drawing knows how many rows fit, so this is where a screen
        // that NSFW filtering left part empty asks for the next page.
        self.load_more_if_near_end(cx, shown);
    }

    fn draw_search(&self, frame: &mut Frame, area: Rect, shown: usize, cx: &Ctx) {
        let mut chips = Vec::new();
        if let Some(category) = &self.category_name {
            chips.push(category.clone());
        }
        // GameBanana's search ranks by best match and ignores the sort.
        if self.query.text.is_empty() {
            chips.push(self.query.sort.label().to_owned());
        }
        let hidden = self.results.len() - shown;
        let mut note = format!("{shown} shown");
        if hidden > 0 {
            let sep = ui::glyphs(cx.config).sep;
            note = format!("{note}{sep}{hidden} NSFW hidden");
        }
        if self.loading && shown > 0 {
            note = format!("{} {note}", ui::spinner(cx.started));
        }
        let bar = ui::SearchBar {
            input: self.search.active().then_some(&self.search),
            query: &self.query.text,
            placeholder: "Search mods",
            chips,
            note: Some(note),
        };
        ui::search_bar(frame, cx.config, area, &bar);
    }

    fn draw_list(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        visible: &[Submission],
        installed: &HashSet<u64>,
        cx: &Ctx,
    ) {
        let config = cx.config;
        let [head, body] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
        let columns = Columns::for_width(usize::from(area.width.saturating_sub(1)));
        frame.render_widget(header(config, columns), head);
        self.page_rows = usize::from(body.height);
        let mut items: Vec<ListItem> = visible
            .iter()
            .map(|s| row(config, s, installed.contains(&s.id), columns))
            .collect();
        if self.loading {
            let more = format!("  {} loading more", ui::spinner(cx.started));
            items.push(ListItem::new(Line::styled(more, config.theme.faint())));
        }
        self.list.select(Some(self.selected));
        let list = List::new(items)
            .highlight_style(config.theme.selected())
            .highlight_symbol(Line::from("\u{258c}").style(config.theme.accent()))
            .highlight_spacing(HighlightSpacing::Always);
        frame.render_stateful_widget(list, body, &mut self.list);
    }

    /// The grid. It scrolls by whole rows, only as far as it takes to keep
    /// the selected card on screen.
    fn draw_cards(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        visible: &[Submission],
        installed: &HashSet<u64>,
        cx: &mut Ctx,
    ) {
        let config = cx.config;
        let (columns, width, left) = grid(area);
        let image_rows = if cx.thumbs.enabled() {
            cx.thumbs.rows_for(width.saturating_sub(2))
        } else {
            0
        };
        // The image gives way first, so a short terminal still fits a row.
        let image_rows = image_rows.min(area.height.saturating_sub(ui::card_height(0)));
        let height = ui::card_height(image_rows);
        let rows = (area.height / height).max(1);

        let (columns, rows) = (usize::from(columns), usize::from(rows));
        self.card_columns = columns;
        self.card_rows = rows;
        self.page_rows = columns * rows;
        let row = self.selected / columns;
        if row < self.card_top {
            self.card_top = row;
        } else if row >= self.card_top + rows {
            self.card_top = row + 1 - rows;
        }

        let first = self.card_top * columns;
        let sep = ui::glyphs(config).sep;
        for (index, s) in visible.iter().enumerate().skip(first).take(columns * rows) {
            let at = index - first;
            let column = u16::try_from(at % columns).unwrap_or(0);
            let line = u16::try_from(at / columns).unwrap_or(0);
            let x = left + column * (width + CARD_GAP);
            let y = area.y + line * height;
            let card_area = Rect::new(x, y, width, height).intersection(area);
            let likes = format!("{} likes", ui::count(s.likes));
            let about = if s.author().is_empty() {
                likes
            } else {
                format!("{}{sep}{likes}", s.author())
            };
            let card = ui::Card {
                name: &s.name,
                about,
                thumbnail: s.thumbnail(),
                installed: installed.contains(&s.id),
                nsfw: s.nsfw(),
                selected: index == self.selected,
            };
            ui::draw_card(frame, config, cx.thumbs, card_area, &card);
        }
    }

    /// The dropdown under the search line while it has focus.
    fn draw_suggestions(&self, frame: &mut Frame, bar: Rect, width: u16, bottom: u16, cx: &Ctx) {
        let config = cx.config;
        let theme = &config.theme;
        let suggestions = self.suggestions();
        let loading = matches!(self.categories, Categories::Loading);
        let wanted = u16::try_from(suggestions.len() + usize::from(loading)).unwrap_or(u16::MAX);
        let top = bar.y + 1;
        let height = (wanted + 2).min(16).min(bottom.saturating_sub(top));
        let width = bar.width.min(width);
        if width < 16 || height < 3 {
            return;
        }
        let popup = Rect::new(bar.x, top, width, height);
        let block = Block::bordered()
            .border_type(config.ui.borders.into())
            .border_style(theme.accent())
            .style(theme.popup())
            .padding(Padding::horizontal(1));
        let inner = block.inner(popup);
        frame.render_widget(Clear, popup);
        frame.render_widget(block, popup);

        // Nothing typed lists the whole tree, so children only need their
        // own name under their root.
        let tree = self.search.text().trim().is_empty();
        let width = usize::from(inner.width.saturating_sub(1));
        let mut items: Vec<ListItem> = suggestions
            .iter()
            .filter_map(|suggestion| {
                let (label, count, style) = match *suggestion {
                    Suggestion::Search => (
                        format!("Search \"{}\"", self.search.text().trim()),
                        String::new(),
                        theme.key(),
                    ),
                    Suggestion::All => (
                        "All mods".to_owned(),
                        String::new(),
                        if self.query.category.is_none() {
                            theme.accent()
                        } else {
                            Style::new().fg(theme.fg)
                        },
                    ),
                    Suggestion::Category(index) => {
                        let Categories::Loaded(rows) = &self.categories else {
                            return None;
                        };
                        let row = rows.get(index)?;
                        let label = match (tree, row.child) {
                            (true, true) => format!("  {}", row.name),
                            (true, false) => row.name.clone(),
                            (false, _) => row.path(),
                        };
                        let style = if row.id == self.query.category {
                            theme.accent()
                        } else if row.child {
                            theme.muted()
                        } else {
                            Style::new().fg(theme.fg)
                        };
                        let count = if row.items > 0 {
                            ui::count(row.items)
                        } else {
                            String::new()
                        };
                        (label, count, style)
                    }
                };
                let room = width.saturating_sub(count.len() + 1);
                Some(ListItem::new(Line::from(vec![
                    Span::styled(ui::cell(&label, room), style),
                    Span::styled(format!(" {count}"), theme.faint()),
                ])))
            })
            .collect();
        if loading {
            items.push(ListItem::new(Line::styled(
                format!("{} Loading categories", ui::spinner(cx.started)),
                theme.faint(),
            )));
        }
        let selected =
            (!suggestions.is_empty()).then(|| self.suggestion.min(suggestions.len() - 1));
        let mut state = ListState::default().with_selected(selected);
        let list = List::new(items)
            .highlight_style(theme.selected())
            .highlight_symbol(Line::from("\u{258c}").style(theme.accent()))
            .highlight_spacing(HighlightSpacing::Always);
        frame.render_stateful_widget(list, inner, &mut state);
    }

    fn draw_details(
        &self,
        frame: &mut Frame,
        inner: Rect,
        selected: Option<&Submission>,
        installed: &HashSet<u64>,
        cx: &mut Ctx,
    ) {
        let config = cx.config;
        let theme = &config.theme;
        let Some(s) = selected else { return };

        let image_rows = match s.thumbnail() {
            Some(_) if cx.thumbs.enabled() => cx.thumbs.rows_for(inner.width).min(inner.height / 2),
            _ => 0,
        };
        let [image, _, text] = Layout::vertical([
            Constraint::Length(image_rows),
            Constraint::Length(u16::from(image_rows > 0)),
            Constraint::Min(0),
        ])
        .areas(inner);
        if let Some(url) = s.thumbnail().filter(|_| image_rows > 0) {
            cx.thumbs.draw(frame, image, &url, theme);
        }

        let mut lines = vec![Line::styled(
            s.name.clone(),
            ui::bold(Style::new().fg(theme.fg)),
        )];
        if !s.author().is_empty() {
            lines.push(Line::styled(format!("by {}", s.author()), theme.muted()));
        }
        lines.push(Line::default());
        if installed.contains(&s.id) {
            let mark = ui::glyphs(config).installed;
            lines.push(ui::field(
                config,
                "Status",
                Span::styled(format!("{mark} Installed"), Style::new().fg(theme.ok)),
            ));
        }
        if !s.category().is_empty() {
            lines.push(ui::field(config, "Category", s.category().to_owned()));
        }
        lines.push(ui::field(config, "Likes", ui::count(s.likes)));
        lines.push(ui::field(config, "Views", ui::count(s.views)));
        if s.downloads > 0 {
            lines.push(ui::field(config, "Downloads", ui::count(s.downloads)));
        }
        if s.added > 0 {
            lines.push(ui::field(config, "Added", ui::date(s.added)));
        }
        if s.modified > s.added {
            lines.push(ui::field(config, "Updated", ui::date(s.modified)));
        }
        if s.nsfw() {
            lines.push(ui::field(
                config,
                "Rating",
                Span::styled("NSFW", Style::new().fg(theme.warn)),
            ));
        }
        if let Some(download) = self.download.as_ref().filter(|d| d.name == s.name) {
            lines.push(Line::default());
            lines.push(Line::styled(
                format!(
                    "Downloading {} of {}",
                    ui::size(download.done),
                    ui::size(download.total)
                ),
                theme.accent(),
            ));
        } else if self.pending.is_some_and(|(id, _)| id == s.id) {
            lines.push(Line::default());
            lines.push(Line::styled(
                format!("{} Opening", ui::spinner(cx.started)),
                theme.accent(),
            ));
        }
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), text);
    }
}

/// Columns of cards that fit in `area`, how wide each is, and where the
/// first starts so the grid sits centered.
fn grid(area: Rect) -> (u16, u16, u16) {
    let columns = ((area.width + CARD_GAP) / (CARD_WIDTH + CARD_GAP)).max(1);
    let width = area.width.saturating_sub(CARD_GAP * (columns - 1)) / columns;
    let used = columns * width + CARD_GAP * (columns - 1);
    (columns, width, area.x + area.width.saturating_sub(used) / 2)
}

#[derive(Clone, Copy)]
struct Columns {
    name: usize,
    updated: usize,
    likes: usize,
    author: usize,
}

impl Columns {
    const MARK: usize = 2;

    fn for_width(width: usize) -> Self {
        let updated = if width >= 80 { 12 } else { 0 };
        let likes = if width >= 50 { 7 } else { 0 };
        let author = if width >= 70 { 18 } else { 0 };
        Self {
            // Capped so the other columns sit next to the name in a wide
            // terminal instead of across a gap.
            name: width
                .saturating_sub(Self::MARK + updated + likes + author)
                .min(64),
            updated,
            likes,
            author,
        }
    }
}

fn header(config: &Config, columns: Columns) -> Line<'static> {
    let style = config.theme.faint();
    let mut spans = vec![Span::styled(
        // One extra column for the selection bar the list draws.
        ui::cell("   Name", 1 + Columns::MARK + columns.name),
        style,
    )];
    if columns.updated > 0 {
        spans.push(Span::styled(ui::cell("  Updated", columns.updated), style));
    }
    if columns.likes > 0 {
        spans.push(Span::styled(
            format!("{:>width$}", "Likes", width = columns.likes),
            style,
        ));
    }
    if columns.author > 0 {
        spans.push(Span::styled(ui::cell("  By", columns.author), style));
    }
    Line::from(spans)
}

fn row(config: &Config, s: &Submission, installed: bool, columns: Columns) -> ListItem<'static> {
    let theme = &config.theme;
    let mark = if installed {
        Span::styled(
            format!("{} ", ui::glyphs(config).installed),
            Style::new().fg(theme.ok),
        )
    } else {
        Span::raw("  ")
    };
    let mut name = vec![(s.name.clone(), Style::new().fg(theme.fg))];
    if s.nsfw() {
        name.push((" 18+".to_owned(), Style::new().fg(theme.warn)));
    }
    let mut spans = vec![mark];
    spans.extend(ui::fit_spans(name, columns.name));
    if columns.updated > 0 {
        spans.push(Span::styled(
            ui::cell(
                &format!(
                    "  {}",
                    if s.modified > 0 {
                        ui::date(s.modified)
                    } else {
                        "unknown".to_owned()
                    }
                ),
                columns.updated,
            ),
            theme.faint(),
        ));
    }
    if columns.likes > 0 {
        spans.push(Span::styled(
            format!("{:>width$}", ui::count(s.likes), width = columns.likes),
            theme.muted(),
        ));
    }
    if columns.author > 0 {
        spans.push(Span::styled(
            ui::cell(&format!("  {}", s.author()), columns.author),
            theme.muted(),
        ));
    }
    ListItem::new(Line::from(spans))
}

fn draw_mod_page(frame: &mut Frame, area: Rect, page: &mut ModPage, cx: &Ctx) {
    let config = cx.config;
    let theme = &config.theme;
    // The whole body, so no thumbnail peeks out half covered.
    let (width, height) = (area.width, area.height);
    let inner = ui::popup(frame, config, area, (width, height), &page.submission.name);

    let files_height = u16::try_from(page.files.len().min(8) + 1).unwrap_or(9);
    let [meta, _, text, files] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(files_height),
    ])
    .areas(inner);

    let sep = ui::glyphs(config).sep;
    let s = &page.submission;
    let mut about = Vec::new();
    if !s.author().is_empty() {
        about.push(format!("by {}", s.author()));
    }
    if let Some(category) = page.details.category() {
        about.push(category.to_owned());
    }
    about.push(format!("{} likes", ui::count(s.likes)));
    frame.render_widget(Line::styled(about.join(sep), theme.muted()), meta);

    let description = if page.text.is_empty() {
        "No description.".to_owned()
    } else {
        page.text.clone()
    };
    let paragraph = Paragraph::new(description)
        .wrap(Wrap { trim: false })
        .scroll((page.scroll, 0));
    frame.render_widget(paragraph, text);

    let installed: HashSet<u64> = cx
        .core
        .mods()
        .iter()
        .filter_map(|m| m.metadata.as_ref()?.game_banana_file_id)
        .collect();
    let [rule, list] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(files);
    frame.render_widget(
        ui::rule(config, "Files", Some(page.files.len()), rule.width),
        rule,
    );
    if page.files.is_empty() {
        frame.render_widget(Line::styled("No files to download.", theme.faint()), list);
        return;
    }
    let name_width = page
        .files
        .iter()
        .map(|f| f.name.chars().count())
        .max()
        .unwrap_or(0)
        .min(32);
    let items: Vec<ListItem> = page
        .files
        .iter()
        .map(|file| {
            let mut spans = vec![
                Span::raw(ui::cell(&file.name, name_width + 2)),
                Span::styled(format!("{:>9}  ", ui::size(file.size)), theme.muted()),
                Span::styled(format!("{}  ", ui::date(file.added)), theme.faint()),
                Span::styled(
                    format!("{:>6} \u{2193}  ", ui::count(file.downloads)),
                    theme.faint(),
                ),
            ];
            if installed.contains(&file.id) {
                spans.push(Span::styled("installed  ", Style::new().fg(theme.ok)));
            }
            if file.archived {
                spans.push(Span::styled("old version  ", Style::new().fg(theme.warn)));
            }
            spans.push(Span::styled(
                file.description.trim().to_owned(),
                theme.muted(),
            ));
            ListItem::new(Line::from(spans))
        })
        .collect();
    let list_widget = List::new(items)
        .highlight_style(theme.selected())
        .highlight_symbol(Line::from("\u{258c}").style(theme.accent()))
        .highlight_spacing(HighlightSpacing::Always);
    frame.render_stateful_widget(list_widget, list, &mut page.file);
}
