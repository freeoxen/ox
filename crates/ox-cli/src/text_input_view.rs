use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};
use unicode_width::UnicodeWidthChar;

/// Maximum number of lines the input area can expand to.
const MAX_INPUT_LINES: u16 = 12;

pub struct TextInputView {
    content: String,
    cursor: usize,
    scroll_top: usize,
}

/// A wrapped display line: the byte range it covers and its display width.
struct WrapLine {
    start: usize,
    end: usize,
}

/// Wrap content into display lines at the given width using unicode display widths.
/// This is the single source of truth for where line breaks occur.
fn wrap_lines(content: &str, width: u16) -> Vec<WrapLine> {
    if width == 0 {
        return vec![WrapLine { start: 0, end: content.len() }];
    }
    let w = width as usize;
    let mut lines = Vec::new();
    let mut line_start = 0;
    let mut col = 0;

    for (byte_pos, ch) in content.char_indices() {
        if ch == '\n' {
            lines.push(WrapLine { start: line_start, end: byte_pos });
            line_start = byte_pos + ch.len_utf8();
            col = 0;
            continue;
        }
        let char_w = ch.width().unwrap_or(0);
        if col + char_w > w && col > 0 {
            // Soft wrap before this character
            lines.push(WrapLine { start: line_start, end: byte_pos });
            line_start = byte_pos;
            col = 0;
        }
        col += char_w;
    }
    // Final line (even if empty — cursor can be here)
    lines.push(WrapLine { start: line_start, end: content.len() });
    lines
}

/// Find the (line_index, column) for a byte offset within wrapped lines.
fn cursor_in_lines(content: &str, cursor_byte: usize, lines: &[WrapLine]) -> (usize, u16) {
    for (i, wl) in lines.iter().enumerate() {
        if cursor_byte >= wl.start && cursor_byte <= wl.end {
            // Cursor is on this line. Compute display column.
            let before_cursor = &content[wl.start..cursor_byte];
            let col: usize = before_cursor.chars().map(|c| c.width().unwrap_or(0)).sum();
            // If cursor_byte == wl.end and this isn't the last line,
            // it could be at the wrap point. Prefer start of next line
            // only if cursor_byte equals end AND there's a next line
            // AND the char at cursor_byte is not a newline (newlines
            // always end a line, cursor after newline = start of next).
            if cursor_byte == wl.end && i + 1 < lines.len() && cursor_byte < content.len() {
                let ch = content[cursor_byte..].chars().next();
                if ch != Some('\n') {
                    // Cursor is at the wrap point — show at start of next line
                    return (i + 1, 0);
                }
            }
            return (i, col as u16);
        }
    }
    // Fallback: end of last line
    let last = lines.len().saturating_sub(1);
    let col: usize = if let Some(wl) = lines.last() {
        content[wl.start..wl.end].chars().map(|c| c.width().unwrap_or(0)).sum()
    } else {
        0
    };
    (last, col as u16)
}

/// Desired input area height based on content, clamped to MAX_INPUT_LINES.
/// Includes 1 line for the top border.
pub fn desired_input_height(content: &str, width: u16) -> u16 {
    let lines = wrap_lines(content, width.saturating_sub(0));
    let text_lines = (lines.len() as u16).max(1);
    // +1 for the top border
    (text_lines + 1).min(MAX_INPUT_LINES + 1)
}

impl TextInputView {
    pub fn new() -> Self {
        Self {
            content: String::new(),
            cursor: 0,
            scroll_top: 0,
        }
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn set_state(&mut self, content: &str, cursor: usize) {
        self.content = content.to_owned();
        self.cursor = cursor.min(content.len());
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
        if inner.width == 0 || inner.height == 0 {
            frame.render_widget(block, area);
            return;
        }

        let lines = wrap_lines(&self.content, inner.width);
        let (cursor_line, cursor_col) = cursor_in_lines(&self.content, self.cursor, &lines);
        let total_lines = lines.len();

        // Scroll management
        self.ensure_cursor_visible(inner.height as usize, cursor_line);
        let visible_end = (self.scroll_top + inner.height as usize).min(total_lines);

        // Build visible lines as ratatui Spans
        let display_lines: Vec<Line> = lines[self.scroll_top..visible_end]
            .iter()
            .map(|wl| Line::from(Span::raw(&self.content[wl.start..wl.end])))
            .collect();

        let paragraph = Paragraph::new(display_lines).block(block);
        frame.render_widget(paragraph, area);

        // Scrollbar (only if content overflows)
        if total_lines > inner.height as usize {
            let mut scrollbar_state = ScrollbarState::new(total_lines.saturating_sub(inner.height as usize))
                .position(self.scroll_top);
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
            frame.render_stateful_widget(scrollbar, inner, &mut scrollbar_state);
        }

        // Cursor
        let visible_cursor_y = cursor_line.saturating_sub(self.scroll_top);
        if (visible_cursor_y as u16) < inner.height {
            frame.set_cursor_position((
                inner.x + cursor_col,
                inner.y + visible_cursor_y as u16,
            ));
        }
    }

    fn ensure_cursor_visible(&mut self, viewport_height: usize, cursor_line: usize) {
        if cursor_line < self.scroll_top {
            self.scroll_top = cursor_line;
        } else if viewport_height > 0 && cursor_line >= self.scroll_top + viewport_height {
            self.scroll_top = cursor_line - viewport_height + 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_empty() {
        let lines = wrap_lines("", 80);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].start, 0);
        assert_eq!(lines[0].end, 0);
    }

    #[test]
    fn wrap_no_wrap_needed() {
        let lines = wrap_lines("hello", 80);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn wrap_hard_newline() {
        let lines = wrap_lines("hello\nworld", 80);
        assert_eq!(lines.len(), 2);
        assert_eq!(&"hello\nworld"[lines[0].start..lines[0].end], "hello");
        assert_eq!(&"hello\nworld"[lines[1].start..lines[1].end], "world");
    }

    #[test]
    fn wrap_soft_wrap() {
        let lines = wrap_lines("abcdefghij", 5);
        assert_eq!(lines.len(), 2);
        assert_eq!(&"abcdefghij"[lines[0].start..lines[0].end], "abcde");
        assert_eq!(&"abcdefghij"[lines[1].start..lines[1].end], "fghij");
    }

    #[test]
    fn wrap_soft_wrap_exact_boundary() {
        let lines = wrap_lines("abcde", 5);
        assert_eq!(lines.len(), 1); // exactly fits, no wrap
    }

    #[test]
    fn wrap_mixed_hard_and_soft() {
        let lines = wrap_lines("abc\ndefghij", 5);
        assert_eq!(lines.len(), 3);
        assert_eq!(&"abc\ndefghij"[lines[0].start..lines[0].end], "abc");
        assert_eq!(&"abc\ndefghij"[lines[1].start..lines[1].end], "defgh");
        assert_eq!(&"abc\ndefghij"[lines[2].start..lines[2].end], "ij");
    }

    #[test]
    fn cursor_at_start() {
        let lines = wrap_lines("hello", 80);
        let (line, col) = cursor_in_lines("hello", 0, &lines);
        assert_eq!((line, col), (0, 0));
    }

    #[test]
    fn cursor_mid_line() {
        let lines = wrap_lines("hello", 80);
        let (line, col) = cursor_in_lines("hello", 3, &lines);
        assert_eq!((line, col), (0, 3));
    }

    #[test]
    fn cursor_end_of_content() {
        let lines = wrap_lines("hello", 80);
        let (line, col) = cursor_in_lines("hello", 5, &lines);
        assert_eq!((line, col), (0, 5));
    }

    #[test]
    fn cursor_after_newline() {
        let content = "hello\nworld";
        let lines = wrap_lines(content, 80);
        // byte 6 = 'w' in "world"
        let (line, col) = cursor_in_lines(content, 6, &lines);
        assert_eq!((line, col), (1, 0));
    }

    #[test]
    fn cursor_on_wrapped_line() {
        let content = "abcdefghij";
        let lines = wrap_lines(content, 5);
        // byte 7 = 'h', which is on the wrapped second line at col 2
        let (line, col) = cursor_in_lines(content, 7, &lines);
        assert_eq!((line, col), (1, 2));
    }

    #[test]
    fn cursor_at_wrap_boundary() {
        let content = "abcdefghij";
        let lines = wrap_lines(content, 5);
        // byte 5 = 'f', which is at the start of the wrapped line
        let (line, col) = cursor_in_lines(content, 5, &lines);
        assert_eq!((line, col), (1, 0));
    }

    #[test]
    fn cursor_at_end_of_first_wrap_line() {
        let content = "abcdefghij";
        let lines = wrap_lines(content, 5);
        // byte 4 = 'e', last char of first line
        let (line, col) = cursor_in_lines(content, 4, &lines);
        assert_eq!((line, col), (0, 4));
    }

    #[test]
    fn desired_height_single_line() {
        assert_eq!(desired_input_height("hello", 80), 2); // 1 text + 1 border
    }

    #[test]
    fn desired_height_multiline() {
        assert_eq!(desired_input_height("a\nb\nc", 80), 4); // 3 text + 1 border
    }

    #[test]
    fn desired_height_capped() {
        let long = "a\n".repeat(50);
        let h = desired_input_height(&long, 80);
        assert_eq!(h, MAX_INPUT_LINES + 1);
    }

    #[test]
    fn scroll_fits() {
        let mut view = TextInputView::new();
        view.ensure_cursor_visible(10, 3);
        assert_eq!(view.scroll_top, 0);
    }

    #[test]
    fn scroll_cursor_below() {
        let mut view = TextInputView::new();
        view.ensure_cursor_visible(3, 5);
        assert_eq!(view.scroll_top, 3);
    }

    #[test]
    fn scroll_cursor_above() {
        let mut view = TextInputView::new();
        view.scroll_top = 5;
        view.ensure_cursor_visible(3, 2);
        assert_eq!(view.scroll_top, 2);
    }
}
