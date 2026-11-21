//! A mod as a card: its thumbnail on top, the name and one line about it
//! below. Every card in a grid gets the same height from `card_height`, so
//! rows line up whether or not a mod has a preview.

use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Block;
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use super::{bold, fit, glyphs};
use crate::config::Config;
use crate::thumbs::Thumbs;

/// Two rows for the name, one for the line about it.
const TEXT_ROWS: u16 = 3;

pub struct Card<'a> {
    pub name: &'a str,
    /// The faint line under the name, like the author and likes.
    pub about: String,
    pub thumbnail: Option<String>,
    pub installed: bool,
    pub nsfw: bool,
    pub selected: bool,
}

/// The height of a card whose image takes `image_rows`, borders included.
pub fn card_height(image_rows: u16) -> u16 {
    image_rows + TEXT_ROWS + 2
}

pub fn draw_card(
    frame: &mut Frame,
    config: &Config,
    thumbs: &mut Thumbs,
    area: Rect,
    card: &Card<'_>,
) {
    let theme = &config.theme;
    let border = if card.selected {
        theme.accent()
    } else {
        theme.border()
    };
    let block = Block::bordered()
        .border_type(config.ui.borders.into())
        .border_style(border);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [image, text] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(TEXT_ROWS)]).areas(inner);
    if let (false, Some(url)) = (image.is_empty(), &card.thumbnail) {
        thumbs.draw(frame, image, url, theme);
    } else if !image.is_empty() {
        let [middle] = Layout::vertical([Constraint::Length(1)])
            .flex(Flex::Center)
            .areas(image);
        let note = Line::styled("No preview", theme.faint()).centered();
        frame.render_widget(note, middle);
    }

    let text_style = if card.selected {
        theme.selected()
    } else {
        Style::new()
    };
    frame.render_widget(Block::new().style(text_style), text);
    let text = Rect {
        x: text.x + 1,
        width: text.width.saturating_sub(2),
        ..text
    };
    let [first, second, about] = Layout::vertical([Constraint::Length(1); 3]).areas(text);
    let (line1, line2) = two_lines(card.name, usize::from(text.width));
    let name_style = bold(Style::new().fg(theme.fg));
    frame.render_widget(Line::styled(line1, name_style), first);
    frame.render_widget(Line::styled(line2, name_style), second);

    let glyphs = glyphs(config);
    let mut marks = Vec::new();
    if card.installed {
        marks.push(Span::styled(
            format!(" {}", glyphs.installed),
            Style::new().fg(theme.ok),
        ));
    }
    if card.nsfw {
        marks.push(Span::styled(" 18+", Style::new().fg(theme.warn)));
    }
    let marks_width: usize = marks.iter().map(|span| span.content.width()).sum();
    let room = usize::from(about.width).saturating_sub(marks_width);
    let mut spans = vec![Span::styled(fit(&card.about, room), theme.faint())];
    let used = spans[0].content.width();
    spans.push(Span::raw(" ".repeat(room.saturating_sub(used))));
    spans.extend(marks);
    frame.render_widget(Line::from(spans), about);
}

/// `text` over two lines of `width`, broken at a space when one fits and
/// cut with an ellipsis when it runs past the second.
fn two_lines(text: &str, width: usize) -> (String, String) {
    if text.width() <= width {
        return (text.to_owned(), String::new());
    }
    let mut used = 0;
    let mut cut = 0;
    let mut space = None;
    for (at, c) in text.char_indices() {
        let w = c.width().unwrap_or(0);
        if used + w > width {
            // A space right after a full line is the best break of all.
            if c == ' ' {
                space = Some(at);
            }
            break;
        }
        if c == ' ' {
            space = Some(at);
        }
        used += w;
        cut = at + c.len_utf8();
    }
    let (first, rest) = match space {
        Some(at) if at > 0 => (&text[..at], &text[at + 1..]),
        _ => text.split_at(cut),
    };
    (first.trim_end().to_owned(), fit(rest.trim_start(), width))
}

#[cfg(test)]
mod tests {
    use super::two_lines;

    #[test]
    fn names_wrap_at_a_space_and_cut_on_the_second_line() {
        assert_eq!(two_lines("Haze", 10), ("Haze".to_owned(), String::new()));
        assert_eq!(
            two_lines("Bunnysuit Mina (Old Gods Update)", 14),
            (
                "Bunnysuit Mina".to_owned(),
                "(Old Gods Upd\u{2026}".to_owned()
            )
        );
        assert_eq!(
            two_lines("Supercalifragilistic", 8),
            ("Supercal".to_owned(), "ifragil\u{2026}".to_owned())
        );
    }
}
