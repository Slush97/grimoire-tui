//! Drawing pieces every page shares. Everything here takes its colors from
//! the theme, so pages stay free of color decisions.

mod card;
mod input;
mod search;

use std::time::{Duration, Instant};

use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::symbols::line;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Clear, Padding, Paragraph, Wrap};
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::config::{Borders, Config, Icons};

pub use card::{card_height, draw_card, Card};
pub use input::{Edit, TextInput};
pub use search::{search_bar, SearchBar};

/// Marks used in lists, in the unicode or ascii set.
pub struct Glyphs {
    pub on: &'static str,
    pub off: &'static str,
    pub global: &'static str,
    pub grabbed: &'static str,
    pub installed: &'static str,
    pub warn: &'static str,
    pub ok: &'static str,
    pub error: &'static str,
    pub sep: &'static str,
    pub cursor: &'static str,
    pub prompt: &'static str,
}

const UNICODE: Glyphs = Glyphs {
    on: "\u{25cf}",
    off: "\u{25cb}",
    global: "\u{25c6}",
    grabbed: "\u{2195}",
    installed: "\u{2713}",
    warn: "\u{25b2}",
    ok: "\u{2713}",
    error: "\u{2717}",
    sep: " \u{b7} ",
    cursor: "\u{258f}",
    prompt: "\u{203a}",
};

const ASCII: Glyphs = Glyphs {
    on: "*",
    off: "-",
    global: "G",
    grabbed: "~",
    installed: "+",
    warn: "!",
    ok: "+",
    error: "x",
    sep: " | ",
    cursor: "_",
    prompt: ">",
};

pub fn glyphs(config: &Config) -> &'static Glyphs {
    match config.ui.icons {
        Icons::Unicode => &UNICODE,
        Icons::Ascii => &ASCII,
    }
}

const SPINNER: [&str; 10] = [
    "\u{280b}", "\u{2819}", "\u{2839}", "\u{2838}", "\u{283c}", "\u{2834}", "\u{2826}", "\u{2827}",
    "\u{2807}", "\u{280f}",
];

pub fn spinner(started: Instant) -> &'static str {
    let frame = started.elapsed().as_millis() / 80;
    SPINNER[usize::try_from(frame).unwrap_or(0) % SPINNER.len()]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Ok,
    Error,
}

/// A message in the footer that goes away on its own.
#[derive(Debug, Clone)]
pub struct Toast {
    pub text: String,
    pub kind: ToastKind,
    pub until: Instant,
}

impl Toast {
    pub fn new(kind: ToastKind, text: impl Into<String>) -> Self {
        let text = text.into();
        // Long enough to read: a base, plus time per word, longer for errors.
        let words = u64::try_from(text.split_whitespace().count()).unwrap_or(0);
        let base = if kind == ToastKind::Error { 4000 } else { 2500 };
        let until = Instant::now() + Duration::from_millis(base + words * 250);
        Self { text, kind, until }
    }
}

/// A bordered panel.
pub fn panel<'a>(config: &Config, title: impl Into<Line<'a>>) -> Block<'a> {
    let theme = &config.theme;
    Block::bordered()
        .border_type(config.ui.borders.into())
        .border_style(theme.border())
        .title(title.into().style(theme.title()))
        .padding(Padding::horizontal(1))
}

/// The frame of a list page: the list alone, or beside a details pane when
/// the page has one to show, the config wants it and there's room.
pub fn list_frame(
    frame: &mut Frame,
    config: &Config,
    area: Rect,
    title: Line<'static>,
    details: bool,
) -> (Rect, Option<Rect>) {
    if !(details && config.ui.details && area.width >= 120) {
        let block = panel(config, title);
        let inner = block.inner(area);
        frame.render_widget(block, area);
        return (inner, None);
    }
    let width = (area.width * 2 / 5).clamp(36, 56);
    let (list, details) = paired_panels(frame, config, area, width, title, " Details ".into());
    (list, Some(details))
}

/// A list and its details pane as one frame split by a divider, so their
/// borders don't double up. `right_width` counts the divider and the outer
/// border. Returns the insides of both, padded like `panel`.
pub fn paired_panels(
    frame: &mut Frame,
    config: &Config,
    area: Rect,
    right_width: u16,
    left_title: Line<'static>,
    right_title: Line<'static>,
) -> (Rect, Rect) {
    let theme = &config.theme;
    let block = Block::bordered()
        .border_type(config.ui.borders.into())
        .border_style(theme.border());
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let [left, divider, right] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(1),
        Constraint::Length(right_width.saturating_sub(2)),
    ])
    .areas(inner);

    let set = match config.ui.borders {
        Borders::Plain => line::NORMAL,
        Borders::Rounded => line::ROUNDED,
        Borders::Double => line::DOUBLE,
        Borders::Thick => line::THICK,
    };
    let joints = [
        (area.top(), set.horizontal_down),
        (area.bottom().saturating_sub(1), set.horizontal_up),
    ];
    let buf = frame.buffer_mut();
    for (y, symbol) in (divider.top()..divider.bottom())
        .map(|y| (y, set.vertical))
        .chain(joints)
    {
        if let Some(cell) = buf.cell_mut((divider.x, y)) {
            cell.set_symbol(symbol).set_style(theme.border());
        }
    }

    let title = |x: u16, width: u16| Rect::new(x, area.y, width, 1);
    frame.render_widget(left_title.style(theme.title()), title(left.x, left.width));
    frame.render_widget(
        right_title.style(theme.title()),
        title(right.x, right.width),
    );
    let pad = Block::new().padding(Padding::horizontal(1));
    (pad.inner(left), pad.inner(right))
}

/// A popup over `area`, cleared and framed. Returns the inside.
pub fn popup(
    frame: &mut Frame,
    config: &Config,
    area: Rect,
    size: (u16, u16),
    title: &str,
) -> Rect {
    let area = centered(area, size.0, size.1);
    let theme = &config.theme;
    let block = Block::bordered()
        .border_type(config.ui.borders.into())
        .border_style(theme.accent())
        .style(theme.popup())
        .title(Line::from(format!(" {title} ")).style(theme.title()))
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(Clear, area);
    frame.render_widget(block, area);
    inner
}

pub fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let [area] = Layout::horizontal([Constraint::Length(width.min(area.width))])
        .flex(Flex::Center)
        .areas(area);
    let [area] = Layout::vertical([Constraint::Length(height.min(area.height))])
        .flex(Flex::Center)
        .areas(area);
    area
}

/// A short message in the middle of a panel, for empty and error states.
pub fn notice(frame: &mut Frame, config: &Config, area: Rect, block: Block, lines: Vec<Line>) {
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let height = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    let [middle] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(inner);
    let text = Paragraph::new(lines)
        .style(config.theme.muted())
        .centered()
        .wrap(Wrap { trim: true });
    frame.render_widget(text, middle);
}

/// `text` cut to `width` columns, with an ellipsis when cut.
pub fn fit(text: &str, width: usize) -> String {
    if text.width() <= width {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let w = c.width().unwrap_or(0);
        if used + w + 1 > width {
            break;
        }
        used += w;
        out.push(c);
    }
    if width > 0 {
        out.push('\u{2026}');
    }
    out
}

/// `text` cut or padded to exactly `width` columns.
pub fn cell(text: &str, width: usize) -> String {
    let mut out = fit(text, width);
    let pad = width.saturating_sub(out.width());
    out.extend(std::iter::repeat_n(' ', pad));
    out
}

/// Styled pieces, cut in order to fit `width` columns, then padded to fill it.
pub fn fit_spans(parts: Vec<(String, Style)>, width: usize) -> Vec<Span<'static>> {
    let mut left = width;
    let mut spans = Vec::with_capacity(parts.len() + 1);
    for (text, style) in parts {
        if left == 0 {
            break;
        }
        let text = fit(&text, left);
        left -= text.width();
        spans.push(Span::styled(text, style));
    }
    if left > 0 {
        spans.push(Span::raw(" ".repeat(left)));
    }
    spans
}

/// A section header that runs the width of the list: `── Load order ──────`.
pub fn rule(config: &Config, label: &str, count: Option<usize>, width: u16) -> Line<'static> {
    let theme = &config.theme;
    let head = format!("\u{2500}\u{2500} {label} ");
    let tail = count.map(|count| format!(" {count}")).unwrap_or_default();
    let fill = usize::from(width).saturating_sub(head.width() + tail.width());
    Line::from(vec![
        Span::styled(head, theme.muted().add_modifier(Modifier::BOLD)),
        Span::styled("\u{2500}".repeat(fill), theme.faint()),
        Span::styled(tail, theme.faint()),
    ])
}

/// A `label  value` row for detail panes.
pub fn field<'a>(config: &Config, label: &str, value: impl Into<Span<'a>>) -> Line<'a> {
    Line::from(vec![
        Span::styled(format!("{label:<10}"), config.theme.muted()),
        value.into(),
    ])
}

pub fn bold(style: Style) -> Style {
    style.add_modifier(Modifier::BOLD)
}

pub fn size(bytes: u64) -> String {
    const KB: u64 = 1 << 10;
    const MB: u64 = 1 << 20;
    const GB: u64 = 1 << 30;
    match bytes {
        b if b >= GB => format!("{}.{} GB", b / GB, b % GB * 10 / GB),
        b if b >= MB => format!("{}.{} MB", b / MB, b % MB * 10 / MB),
        b if b >= KB => format!("{} KB", b / KB),
        b => format!("{b} B"),
    }
}

/// `1234` as `1.2k`, for like and view counts.
pub fn count(n: u64) -> String {
    match n {
        n if n >= 1_000_000 => format!("{}.{}M", n / 1_000_000, n % 1_000_000 / 100_000),
        n if n >= 1_000 => format!("{}.{}k", n / 1_000, n % 1_000 / 100),
        n => n.to_string(),
    }
}

/// A unix timestamp as `2026-10-05`.
pub fn date(timestamp: i64) -> String {
    // Howard Hinnant's days-to-civil.
    let days = timestamp.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_cuts_by_display_width() {
        assert_eq!(fit("Haze", 10), "Haze");
        assert_eq!(fit("Lady Geist", 6), "Lady \u{2026}");
        assert_eq!(cell("ab", 4), "ab  ");
        assert_eq!(fit("\u{5168}\u{5168}\u{5168}", 4), "\u{5168}\u{2026}");
    }

    #[test]
    fn numbers_read_short() {
        assert_eq!(count(999), "999");
        assert_eq!(count(1_250), "1.2k");
        assert_eq!(size(3 * (1 << 20) + (1 << 19)), "3.5 MB");
        assert_eq!(date(0), "1970-01-01");
        assert_eq!(date(1_791_158_400), "2026-10-05");
    }
}
