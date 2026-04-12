use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use unicode_width::UnicodeWidthChar;

/// Maximum number of lines the input area can expand to.
const MAX_INPUT_LINES: u16 = 12;

pub struct TextInputView {
    content: String,
    cursor: usize,
    scroll_top: usize,
    /// The inner area (inside border) from the last render, for mouse hit testing.
    last_inner: Option<Rect>,
    /// User-overridden height via drag resize. None = auto-size.
    height_override: Option<u16>,
    /// Whether a border drag is in progress.
    dragging_border: bool,
    /// The terminal row where the drag started.
    drag_start_row: u16,
    /// The input height when the drag started.
    drag_start_height: u16,
}

/// A wrapped display line: the byte range it covers.
pub struct WrapLine {
    pub start: usize,
    pub end: usize,
}

/// Wrap content into display lines at the given width using unicode display widths.
/// This is the single source of truth for where line breaks occur.
pub fn wrap_lines(content: &str, width: u16) -> Vec<WrapLine> {
    if width == 0 {
        return vec![WrapLine {
            start: 0,
            end: content.len(),
        }];
    }
    let w = width as usize;
    let mut lines = Vec::new();
    let mut line_start = 0;
    let mut col = 0;

    for (byte_pos, ch) in content.char_indices() {
        if ch == '\n' {
            lines.push(WrapLine {
                start: line_start,
                end: byte_pos,
            });
            line_start = byte_pos + ch.len_utf8();
            col = 0;
            continue;
        }
        let char_w = ch.width().unwrap_or(0);
        if col + char_w > w && col > 0 {
            // Soft wrap before this character
            lines.push(WrapLine {
                start: line_start,
                end: byte_pos,
            });
            line_start = byte_pos;
            col = 0;
        }
        col += char_w;
    }
    // Final line (even if empty — cursor can be here)
    lines.push(WrapLine {
        start: line_start,
        end: content.len(),
    });
    lines
}

/// Find the (line_index, column) for a byte offset within wrapped lines.
pub fn cursor_in_lines(content: &str, cursor_byte: usize, lines: &[WrapLine]) -> (usize, u16) {
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
        content[wl.start..wl.end]
            .chars()
            .map(|c| c.width().unwrap_or(0))
            .sum()
    } else {
        0
    };
    (last, col as u16)
}

/// Convert a (line_index, display_column) to a byte offset in the content.
/// Clamps to valid positions. Used for Up/Down arrows and mouse click.
pub fn byte_offset_at(
    content: &str,
    lines: &[WrapLine],
    target_line: usize,
    target_col: u16,
) -> usize {
    let line_idx = target_line.min(lines.len().saturating_sub(1));
    let wl = &lines[line_idx];
    let line_str = &content[wl.start..wl.end];
    let mut col = 0u16;
    for (byte_offset, ch) in line_str.char_indices() {
        if col >= target_col {
            return wl.start + byte_offset;
        }
        col += ch.width().unwrap_or(0) as u16;
    }
    // Past end of line — clamp to end
    wl.end
}

/// Desired input area height based on content, clamped to MAX_INPUT_LINES.
/// Includes 1 line for the top border.
pub fn desired_input_height(content: &str, width: u16, height_override: Option<u16>) -> u16 {
    if let Some(h) = height_override {
        // +1 for border, clamp to [2, MAX+1]
        return (h + 1).clamp(2, MAX_INPUT_LINES + 1);
    }
    let lines = wrap_lines(content, width);
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
            last_inner: None,
            height_override: None,
            dragging_border: false,
            drag_start_row: 0,
            drag_start_height: 0,
        }
    }

    pub fn content(&self) -> &str {
        &self.content
    }

    pub fn set_state(&mut self, content: &str, cursor: usize) {
        self.content = content.to_owned();
        self.cursor = cursor.min(content.len());
    }

    pub fn render(&mut self, frame: &mut Frame, area: Rect, border_style: Style, title: &str) {
        // Compute inner area from border geometry (title doesn't affect it)
        let base_block = Block::default()
            .borders(Borders::TOP)
            .border_style(border_style);
        let inner = base_block.inner(area);
        if inner.width == 0 || inner.height == 0 {
            let block = base_block.title(title);
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

        self.last_inner = Some(inner);

        // Build block with line position indicator when content overflows
        let mut block = base_block.title(title);
        if total_lines > inner.height as usize {
            let indicator = format!(" {}/{} ", cursor_line + 1, total_lines);
            block = block.title_top(Line::from(indicator).right_aligned());
        }

        let paragraph = Paragraph::new(display_lines).block(block);
        frame.render_widget(paragraph, area);

        // Scrollbar (only if content overflows)
        if total_lines > inner.height as usize {
            let mut scrollbar_state =
                ScrollbarState::new(total_lines.saturating_sub(inner.height as usize))
                    .position(self.scroll_top);
            let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
            frame.render_stateful_widget(scrollbar, inner, &mut scrollbar_state);
        }

        // Cursor
        let visible_cursor_y = cursor_line.saturating_sub(self.scroll_top);
        if (visible_cursor_y as u16) < inner.height {
            frame.set_cursor_position((inner.x + cursor_col, inner.y + visible_cursor_y as u16));
        }
    }

    /// Is the given terminal position inside the input area (including border)?
    pub fn contains(&self, col: u16, row: u16) -> bool {
        if let Some(inner) = self.last_inner {
            // Include one row above inner for the top border
            let area_top = inner.y.saturating_sub(1);
            col >= inner.x
                && col < inner.x + inner.width
                && row >= area_top
                && row < inner.y + inner.height
        } else {
            false
        }
    }

    /// Is the given terminal row on the top border of the input area?
    pub fn is_on_border(&self, row: u16) -> bool {
        if let Some(inner) = self.last_inner {
            row == inner.y.saturating_sub(1)
        } else {
            false
        }
    }

    /// Current height override (for layout computation).
    pub fn height_override(&self) -> Option<u16> {
        self.height_override
    }

    /// Start a border drag at the given terminal row.
    pub fn start_border_drag(&mut self, row: u16) {
        self.dragging_border = true;
        self.drag_start_row = row;
        self.drag_start_height = self.last_inner.map(|r| r.height).unwrap_or(3);
    }

    /// Update during border drag. Row moves up = taller input.
    pub fn update_border_drag(&mut self, row: u16) {
        if !self.dragging_border {
            return;
        }
        let delta = self.drag_start_row as i32 - row as i32;
        let new_h = (self.drag_start_height as i32 + delta).clamp(1, MAX_INPUT_LINES as i32) as u16;
        self.height_override = Some(new_h);
    }

    /// End the border drag.
    pub fn end_border_drag(&mut self) {
        self.dragging_border = false;
    }

    /// Whether a border drag is in progress.
    pub fn is_dragging(&self) -> bool {
        self.dragging_border
    }

    /// Scroll the input view by a delta (positive = down, negative = up).
    pub fn scroll_by(&mut self, delta: i32) {
        if delta > 0 {
            let lines = wrap_lines(
                &self.content,
                self.last_inner.map(|r| r.width).unwrap_or(80),
            );
            let vh = self.last_inner.map(|r| r.height as usize).unwrap_or(1);
            let max_scroll = lines.len().saturating_sub(vh);
            self.scroll_top = (self.scroll_top + delta as usize).min(max_scroll);
        } else {
            self.scroll_top = self.scroll_top.saturating_sub((-delta) as usize);
        }
    }

    /// Handle a mouse click at terminal (column, row). Returns the byte offset
    /// to move the cursor to, or None if the click is outside the input area.
    pub fn click_to_byte_offset(&self, col: u16, row: u16) -> Option<usize> {
        let inner = self.last_inner?;
        if col < inner.x
            || col >= inner.x + inner.width
            || row < inner.y
            || row >= inner.y + inner.height
        {
            return None;
        }
        let click_col = col - inner.x;
        let click_row = (row - inner.y) as usize + self.scroll_top;
        let lines = wrap_lines(&self.content, inner.width);
        Some(byte_offset_at(&self.content, &lines, click_row, click_col))
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
        assert_eq!(desired_input_height("hello", 80, None), 2); // 1 text + 1 border
    }

    #[test]
    fn desired_height_multiline() {
        assert_eq!(desired_input_height("a\nb\nc", 80, None), 4); // 3 text + 1 border
    }

    #[test]
    fn desired_height_capped() {
        let long = "a\n".repeat(50);
        let h = desired_input_height(&long, 80, None);
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
