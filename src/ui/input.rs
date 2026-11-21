//! A one-line text field. While it's focused it gets raw keys, so typing
//! never trips a binding.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Default)]
pub struct TextInput {
    text: String,
    /// Byte offset, always on a char boundary.
    cursor: usize,
    active: bool,
}

pub enum Edit {
    Submit(String),
    Cancel,
    Changed,
    Unchanged,
}

impl TextInput {
    pub fn start(&mut self, text: &str) {
        text.clone_into(&mut self.text);
        self.cursor = self.text.len();
        self.active = true;
    }

    /// Gives up focus without submitting, for a page that took the text
    /// itself.
    pub fn stop(&mut self) {
        self.active = false;
    }

    pub fn active(&self) -> bool {
        self.active
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Text before and after the cursor, for drawing it.
    pub fn split(&self) -> (&str, &str) {
        self.text.split_at(self.cursor)
    }

    pub fn on_key(&mut self, key: KeyEvent) -> Edit {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Enter => {
                self.active = false;
                self.cursor = 0;
                return Edit::Submit(std::mem::take(&mut self.text));
            }
            KeyCode::Esc => {
                self.active = false;
                return Edit::Cancel;
            }
            KeyCode::Char('u') if ctrl => {
                self.text.drain(..self.cursor);
                self.cursor = 0;
            }
            KeyCode::Char('w') if ctrl => {
                let before = self.text[..self.cursor].trim_end();
                let start = before.rfind(' ').map_or(0, |i| i + 1);
                self.text.drain(start..self.cursor);
                self.cursor = start;
            }
            KeyCode::Char('a') if ctrl => self.cursor = 0,
            KeyCode::Char('e') if ctrl => self.cursor = self.text.len(),
            KeyCode::Char(c) if !ctrl => {
                self.text.insert(self.cursor, c);
                self.cursor += c.len_utf8();
            }
            KeyCode::Backspace => match self.text[..self.cursor].chars().next_back() {
                Some(c) => {
                    self.cursor -= c.len_utf8();
                    self.text.remove(self.cursor);
                }
                None => return Edit::Unchanged,
            },
            KeyCode::Delete if self.cursor < self.text.len() => {
                self.text.remove(self.cursor);
            }
            KeyCode::Left => {
                if let Some(c) = self.text[..self.cursor].chars().next_back() {
                    self.cursor -= c.len_utf8();
                }
                return Edit::Unchanged;
            }
            KeyCode::Right => {
                if let Some(c) = self.text[self.cursor..].chars().next() {
                    self.cursor += c.len_utf8();
                }
                return Edit::Unchanged;
            }
            KeyCode::Home => {
                self.cursor = 0;
                return Edit::Unchanged;
            }
            KeyCode::End => {
                self.cursor = self.text.len();
                return Edit::Unchanged;
            }
            _ => return Edit::Unchanged,
        }
        Edit::Changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(input: &mut TextInput, code: KeyCode, modifiers: KeyModifiers) -> Edit {
        input.on_key(KeyEvent::new(code, modifiers))
    }

    #[test]
    fn edits_around_the_cursor() {
        let mut input = TextInput::default();
        input.start("haze skin");
        press(&mut input, KeyCode::Char('w'), KeyModifiers::CONTROL);
        assert_eq!(input.text(), "haze ");
        press(&mut input, KeyCode::Left, KeyModifiers::NONE);
        press(&mut input, KeyCode::Char('é'), KeyModifiers::NONE);
        assert_eq!(input.split(), ("hazeé", " "));
        press(&mut input, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(input.text(), "haze ");
        match press(&mut input, KeyCode::Enter, KeyModifiers::NONE) {
            Edit::Submit(text) => assert_eq!(text, "haze "),
            _ => panic!("enter submits"),
        }
        assert!(!input.active());
    }
}
