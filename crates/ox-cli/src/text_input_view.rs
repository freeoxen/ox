use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

pub struct TextInputView {
    content: String,
    cursor: usize,
    scroll_top: usize,
}

/// Returns (total_wrapped_lines, cursor_line, cursor_col).
///
/// Walks content char-by-char, tracking soft wraps at `width` and hard
/// wraps at `\n`. The cursor position is recorded when the accumulated
/// byte offset reaches `cursor_byte`.
fn compute_cursor_position(content: &str, cursor_byte: usize, width: u16) -> (usize, u16, u16) {
    if width == 0 {
        return (1, 0, 0);
    }
    let w = width as usize;
    let mut line: usize = 0;
    let mut col: usize = 0;
    let mut cursor_line: u16 = 0;
    let mut cursor_col: u16 = 0;
    let mut found = false;

    for (byte_pos, ch) in content.char_indices() {
        if byte_pos == cursor_byte && !found {
            cursor_line = line as u16;
            cursor_col = col as u16;
            found = true;
        }
        if ch == '\n' {
            line += 1;
            col = 0;
        } else {
            if col == w {
                line += 1;
                col = 0;
            }
            col += 1;
        }
    }
    // Cursor at end of content.
    if !found {
        cursor_line = line as u16;
        cursor_col = col as u16;
    }
    (line + 1, cursor_line, cursor_col)
}

impl TextInputView {
    pub fn new() -> Self {
        Self {
            content: String::new(),
            cursor: 0,
            scroll_top: 0,
        }
    }

    pub fn set_state(&mut self, content: &str, cursor: usize) {
        self.content = content.to_owned();
        self.cursor = cursor;
    }

    pub fn render(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        border_style: Style,
        title: &str,
    ) {
        let block = Block::default()
            .borders(Borders::TOP)
            .border_style(border_style)
            .title(title);
        let inner = block.inner(area);

        let (_, cursor_line, cursor_col) =
            compute_cursor_position(&self.content, self.cursor, inner.width);

        self.ensure_cursor_visible(inner.height, cursor_line);

        let paragraph = Paragraph::new(self.content.as_str())
            .block(block)
            .wrap(Wrap { trim: false })
            .scroll((self.scroll_top as u16, 0));

        frame.render_widget(paragraph, area);

        let visible_cursor_y = cursor_line.saturating_sub(self.scroll_top as u16);
        frame.set_cursor_position((
            inner.x + cursor_col,
            inner.y + visible_cursor_y,
        ));
    }

    fn ensure_cursor_visible(&mut self, viewport_height: u16, cursor_line: u16) {
        let cl = cursor_line as usize;
        let vh = viewport_height as usize;
        if cl < self.scroll_top {
            self.scroll_top = cl;
        } else if vh > 0 && cl >= self.scroll_top + vh {
            self.scroll_top = cl - vh + 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_string() {
        let (total, line, col) = compute_cursor_position("", 0, 80);
        assert_eq!((total, line, col), (1, 0, 0));
    }

    #[test]
    fn simple_cursor_mid() {
        let (_, line, col) = compute_cursor_position("hello", 3, 80);
        assert_eq!((line, col), (0, 3));
    }

    #[test]
    fn after_newline() {
        // cursor=6 is the 'w' in "world"
        let (_, line, col) = compute_cursor_position("hello\nworld", 6, 80);
        assert_eq!((line, col), (1, 0));
    }

    #[test]
    fn mid_second_line() {
        let (_, line, col) = compute_cursor_position("hello\nworld", 8, 80);
        assert_eq!((line, col), (1, 2));
    }

    #[test]
    fn soft_wrap() {
        // width=5, "abcdefghij" wraps to ["abcde","fghij"]
        let (total, line, col) = compute_cursor_position("abcdefghij", 7, 5);
        assert_eq!(total, 2);
        assert_eq!((line, col), (1, 2));
    }

    #[test]
    fn cursor_at_end() {
        let (_, line, col) = compute_cursor_position("hello", 5, 80);
        assert_eq!((line, col), (0, 5));
    }

    #[test]
    fn cursor_at_newline_char() {
        // cursor=5 is the '\n' itself
        let (_, line, col) = compute_cursor_position("hello\nworld", 5, 80);
        assert_eq!((line, col), (0, 5));
    }

    #[test]
    fn scroll_content_fits() {
        let mut view = TextInputView::new();
        view.set_state("short", 0);
        view.ensure_cursor_visible(10, 0);
        assert_eq!(view.scroll_top, 0);
    }

    #[test]
    fn scroll_cursor_below_viewport() {
        let mut view = TextInputView::new();
        view.scroll_top = 0;
        // cursor on line 5, viewport height 3 → scroll_top = 3
        view.ensure_cursor_visible(3, 5);
        assert_eq!(view.scroll_top, 3);
    }

    #[test]
    fn scroll_cursor_above_viewport() {
        let mut view = TextInputView::new();
        view.scroll_top = 5;
        // cursor on line 2 → scroll_top = 2
        view.ensure_cursor_visible(3, 2);
        assert_eq!(view.scroll_top, 2);
    }

    #[test]
    fn scroll_cursor_in_middle_unchanged() {
        let mut view = TextInputView::new();
        view.scroll_top = 2;
        // cursor on line 3, viewport height 5 → visible range 2..7, cursor in range
        view.ensure_cursor_visible(5, 3);
        assert_eq!(view.scroll_top, 2);
    }
}
