//! The search line at the top of a list: the query, or the field with its
//! cursor while it has focus, and the other active filters as chips.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Block;
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::{fit, glyphs, TextInput};
use crate::config::Config;

pub struct SearchBar<'a> {
    /// The field while it has focus.
    pub input: Option<&'a TextInput>,
    /// What's searched for when the field doesn't have focus.
    pub query: &'a str,
    /// Shown when there's no query, like "Search mods".
    pub placeholder: &'a str,
    /// Filters besides the text, like a category or a sort.
    pub chips: Vec<String>,
    /// Right-aligned and faint, like "12 of 67".
    pub note: Option<String>,
}

pub fn search_bar(frame: &mut Frame, config: &Config, area: Rect, bar: &SearchBar) {
    let theme = &config.theme;
    let glyphs = glyphs(config);
    frame.render_widget(Block::new().style(theme.popup()), area);

    let mut right = Vec::new();
    for chip in &bar.chips {
        right.push(Span::styled(
            format!(" {chip} "),
            Style::new().fg(theme.fg).bg(theme.selection),
        ));
        right.push(Span::raw(" "));
    }
    if let Some(note) = &bar.note {
        right.push(Span::styled(format!("{note} "), theme.faint()));
    }
    let right_width: usize = right.iter().map(|span| span.content.width()).sum();
    let right_width = right_width.min(usize::from(area.width) / 2);
    let [left_area, right_area] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(u16::try_from(right_width).unwrap_or(0)),
    ])
    .areas(area);

    let prompt = format!(" {} ", glyphs.prompt);
    // Room for the text after the prompt and the cursor.
    let room = usize::from(left_area.width).saturating_sub(prompt.width() + 1);
    let mut left = Vec::with_capacity(4);
    match bar.input {
        Some(input) => {
            left.push(Span::styled(prompt, theme.key()));
            let (before, after) = input.split();
            // Keeps the cursor on screen by dropping the start of a long query.
            let skip = before.width().saturating_sub(room);
            let before: String = before
                .chars()
                .scan(0, |seen, c| {
                    *seen += c.width().unwrap_or(0);
                    Some((*seen, c))
                })
                .filter(|(seen, _)| *seen > skip)
                .map(|(_, c)| c)
                .collect();
            let after = fit(after, room.saturating_sub(before.width()));
            left.push(Span::styled(before, Style::new().fg(theme.fg)));
            left.push(Span::styled(glyphs.cursor, theme.accent()));
            left.push(Span::styled(after, Style::new().fg(theme.fg)));
        }
        None if bar.query.is_empty() => {
            left.push(Span::styled(prompt, theme.faint()));
            left.push(Span::styled(fit(bar.placeholder, room), theme.faint()));
        }
        None => {
            left.push(Span::styled(prompt, theme.accent()));
            left.push(Span::styled(
                fit(bar.query, room),
                Style::new().fg(theme.fg),
            ));
        }
    }
    frame.render_widget(Line::from(left), left_area);
    frame.render_widget(Line::from(right), right_area);
}
