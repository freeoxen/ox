//! SimpleInput — single-line text field with cursor.
//!
//! Used for dialog fields that don't need broker sync: settings account
//! fields, profile names, defaults. Provides the same editing model as
//! InputSession (content + cursor, insert-at-cursor, delete-at-cursor,
//! cursor movement) without the pending_edits / generation machinery.

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthChar;

/// A single-line text field with cursor tracking.
#[derive(Debug, Clone)]
pub struct SimpleInput {
    content: String,
    cursor: usize, // byte offset
}

#[allow(dead_code)]
impl SimpleInput {
    pub fn new() -> Self {
        Self {
            content: String::new(),
            cursor: 0,
        }
    }

    pub fn from(s: &str) -> Self {
        Self {
            cursor: s.len(),
            content: s.to_owned(),
        }
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn is_empty(&self) -> bool {
        self.content.is_empty()
    }

    pub fn len(&self) -> usize {
        self.content.len()
    }

    /// Replace all content, cursor to end.
    pub fn set(&mut self, s: &str) {
        self.content = s.to_owned();
        self.cursor = self.content.len();
    }

    /// Clear content and cursor.
    pub fn clear(&mut self) {
        self.content.clear();
        self.cursor = 0;
    }

    // -- Editing operations ---------------------------------------------------

    /// Insert a single character at cursor.
    pub fn insert_char(&mut self, ch: char) {
        let at = self.cursor.min(self.content.len());
        self.content.insert(at, ch);
        self.cursor = at + ch.len_utf8();
    }

    /// Insert a string at cursor (paste).
    pub fn insert_str(&mut self, s: &str) {
        let at = self.cursor.min(self.content.len());
        self.content.insert_str(at, s);
        self.cursor = at + s.len();
    }

    /// Delete one character before cursor (backspace).
    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            let before = &self.content[..self.cursor];
            let prev = before
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
            self.content.drain(prev..self.cursor);
            self.cursor = prev;
        }
    }

    /// Delete one character after cursor.
    pub fn delete_forward(&mut self) {
        if self.cursor < self.content.len() {
            let after = &self.content[self.cursor..];
            let len = after.chars().next().map(|c| c.len_utf8()).unwrap_or(0);
            self.content.drain(self.cursor..self.cursor + len);
        }
    }

    // -- Cursor movement ------------------------------------------------------

    pub fn move_left(&mut self) {
        let before = &self.content[..self.cursor];
        self.cursor = before
            .char_indices()
            .next_back()
            .map(|(i, _)| i)
            .unwrap_or(0);
    }

    pub fn move_right(&mut self) {
        let after = &self.content[self.cursor..];
        self.cursor += after.chars().next().map(|c| c.len_utf8()).unwrap_or(0);
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.content.len();
    }

    // -- Key dispatch ---------------------------------------------------------

    /// Handle a key event. Returns true if the key was consumed.
    pub fn handle_key(&mut self, modifiers: KeyModifiers, code: KeyCode) -> bool {
        match (modifiers, code) {
            (KeyModifiers::CONTROL, KeyCode::Char('a')) | (_, KeyCode::Home) => {
                self.home();
                true
            }
            (KeyModifiers::CONTROL, KeyCode::Char('e')) | (_, KeyCode::End) => {
                self.end();
                true
            }
            (_, KeyCode::Left) => {
                self.move_left();
                true
            }
            (_, KeyCode::Right) => {
                self.move_right();
                true
            }
            (_, KeyCode::Backspace) => {
                self.backspace();
                true
            }
            (_, KeyCode::Delete) => {
                self.delete_forward();
                true
            }
            (_, KeyCode::Char(c)) if !c.is_control() => {
                self.insert_char(c);
                true
            }
            _ => false,
        }
    }

    // -- Rendering ------------------------------------------------------------

    /// Render as a single-line field. If `focused`, sets the terminal cursor.
    ///
    /// `mask` replaces each character with a bullet (for password fields).
    pub fn render_inline(
        &self,
        frame: &mut Frame,
        area: Rect,
        style: Style,
        focused: bool,
        mask: bool,
    ) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let display: String = if mask {
            "\u{25cf}".repeat(self.content.chars().count())
        } else {
            self.content.clone()
        };

        let paragraph = Paragraph::new(Line::from(Span::styled(&display, style)));
        frame.render_widget(paragraph, area);

        if focused {
            // Compute display column for cursor
            let col = if mask {
                // Each masked char is one bullet (U+25CF, display width 1)
                self.content[..self.cursor].chars().count() as u16
            } else {
                self.content[..self.cursor]
                    .chars()
                    .map(|c| c.width().unwrap_or(0) as u16)
                    .sum()
            };
            let col = col.min(area.width.saturating_sub(1));
            frame.set_cursor_position((area.x + col, area.y));
        }
    }
}

impl Default for SimpleInput {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_is_empty() {
        let input = SimpleInput::new();
        assert!(input.is_empty());
        assert_eq!(input.cursor(), 0);
    }

    #[test]
    fn from_sets_cursor_to_end() {
        let input = SimpleInput::from("hello");
        assert_eq!(input.content(), "hello");
        assert_eq!(input.cursor(), 5);
    }

    #[test]
    fn insert_char_at_end() {
        let mut input = SimpleInput::new();
        input.insert_char('h');
        input.insert_char('i');
        assert_eq!(input.content(), "hi");
        assert_eq!(input.cursor(), 2);
    }

    #[test]
    fn insert_char_at_middle() {
        let mut input = SimpleInput::from("hllo");
        input.cursor = 1;
        input.insert_char('e');
        assert_eq!(input.content(), "hello");
        assert_eq!(input.cursor(), 2);
    }

    #[test]
    fn insert_str_paste() {
        let mut input = SimpleInput::from("hd");
        input.cursor = 1;
        input.insert_str("ello worl");
        assert_eq!(input.content(), "hello world");
        assert_eq!(input.cursor(), 10);
    }

    #[test]
    fn backspace_at_end() {
        let mut input = SimpleInput::from("hello");
        input.backspace();
        assert_eq!(input.content(), "hell");
        assert_eq!(input.cursor(), 4);
    }

    #[test]
    fn backspace_at_middle() {
        let mut input = SimpleInput::from("hello");
        input.cursor = 3;
        input.backspace();
        assert_eq!(input.content(), "helo");
        assert_eq!(input.cursor(), 2);
    }

    #[test]
    fn backspace_at_start_is_noop() {
        let mut input = SimpleInput::from("hello");
        input.cursor = 0;
        input.backspace();
        assert_eq!(input.content(), "hello");
        assert_eq!(input.cursor(), 0);
    }

    #[test]
    fn delete_forward_at_middle() {
        let mut input = SimpleInput::from("hello");
        input.cursor = 2;
        input.delete_forward();
        assert_eq!(input.content(), "helo");
        assert_eq!(input.cursor(), 2);
    }

    #[test]
    fn delete_forward_at_end_is_noop() {
        let mut input = SimpleInput::from("hi");
        input.delete_forward();
        assert_eq!(input.content(), "hi");
    }

    #[test]
    fn move_left_right() {
        let mut input = SimpleInput::from("abc");
        assert_eq!(input.cursor(), 3);
        input.move_left();
        assert_eq!(input.cursor(), 2);
        input.move_left();
        assert_eq!(input.cursor(), 1);
        input.move_right();
        assert_eq!(input.cursor(), 2);
    }

    #[test]
    fn move_left_at_start_stays() {
        let mut input = SimpleInput::from("x");
        input.cursor = 0;
        input.move_left();
        assert_eq!(input.cursor(), 0);
    }

    #[test]
    fn move_right_at_end_stays() {
        let mut input = SimpleInput::from("x");
        input.move_right();
        assert_eq!(input.cursor(), 1);
    }

    #[test]
    fn home_end() {
        let mut input = SimpleInput::from("hello");
        input.cursor = 3;
        input.home();
        assert_eq!(input.cursor(), 0);
        input.end();
        assert_eq!(input.cursor(), 5);
    }

    #[test]
    fn set_replaces_content() {
        let mut input = SimpleInput::from("old");
        input.set("new content");
        assert_eq!(input.content(), "new content");
        assert_eq!(input.cursor(), 11);
    }

    #[test]
    fn clear_resets() {
        let mut input = SimpleInput::from("stuff");
        input.clear();
        assert!(input.is_empty());
        assert_eq!(input.cursor(), 0);
    }

    #[test]
    fn handle_key_char() {
        let mut input = SimpleInput::new();
        assert!(input.handle_key(KeyModifiers::NONE, KeyCode::Char('a')));
        assert_eq!(input.content(), "a");
    }

    #[test]
    fn handle_key_backspace() {
        let mut input = SimpleInput::from("ab");
        assert!(input.handle_key(KeyModifiers::NONE, KeyCode::Backspace));
        assert_eq!(input.content(), "a");
    }

    #[test]
    fn handle_key_left_right() {
        let mut input = SimpleInput::from("ab");
        assert!(input.handle_key(KeyModifiers::NONE, KeyCode::Left));
        assert_eq!(input.cursor(), 1);
        assert!(input.handle_key(KeyModifiers::NONE, KeyCode::Right));
        assert_eq!(input.cursor(), 2);
    }

    #[test]
    fn handle_key_ctrl_a_e() {
        let mut input = SimpleInput::from("hello");
        input.cursor = 3;
        assert!(input.handle_key(KeyModifiers::CONTROL, KeyCode::Char('a')));
        assert_eq!(input.cursor(), 0);
        assert!(input.handle_key(KeyModifiers::CONTROL, KeyCode::Char('e')));
        assert_eq!(input.cursor(), 5);
    }

    #[test]
    fn handle_key_home_end() {
        let mut input = SimpleInput::from("hello");
        input.cursor = 3;
        assert!(input.handle_key(KeyModifiers::NONE, KeyCode::Home));
        assert_eq!(input.cursor(), 0);
        assert!(input.handle_key(KeyModifiers::NONE, KeyCode::End));
        assert_eq!(input.cursor(), 5);
    }

    #[test]
    fn handle_key_delete() {
        let mut input = SimpleInput::from("abc");
        input.cursor = 1;
        assert!(input.handle_key(KeyModifiers::NONE, KeyCode::Delete));
        assert_eq!(input.content(), "ac");
    }

    #[test]
    fn handle_key_unknown_returns_false() {
        let mut input = SimpleInput::new();
        assert!(!input.handle_key(KeyModifiers::NONE, KeyCode::F(1)));
    }

    #[test]
    fn multibyte_char_handling() {
        let mut input = SimpleInput::new();
        input.insert_char('é');
        input.insert_char('!');
        assert_eq!(input.content(), "é!");
        assert_eq!(input.cursor(), 3); // é is 2 bytes + ! is 1
        input.move_left();
        assert_eq!(input.cursor(), 2);
        input.move_left();
        assert_eq!(input.cursor(), 0);
        input.move_right();
        assert_eq!(input.cursor(), 2); // skip the full é
    }

    #[test]
    fn paste_into_empty() {
        let mut input = SimpleInput::new();
        input.insert_str("pasted text");
        assert_eq!(input.content(), "pasted text");
        assert_eq!(input.cursor(), 11);
    }
}
