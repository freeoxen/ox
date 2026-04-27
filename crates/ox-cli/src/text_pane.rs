//! `TextPane` — a single reusable widget for rendering bounded prose into a
//! ratatui [`Rect`], with explicit horizontal and vertical overflow policies.
//!
//! Status lines, validation errors, test results, and similar prose all need
//! the same shape: text that may be longer than the slot it gets, in either
//! dimension. Each call site rolling its own truncation produces inconsistent
//! UX (one place wraps, another clips, a third silently swallows the tail).
//! `TextPane` consolidates the policy choice into a single configurable
//! component so every caller picks from the same menu.
//!
//! The widget is stateless; if scrolling is wanted in the future, callers
//! pass an offset alongside [`VerticalOverflow::Clip`] and we add a scroll
//! variant that takes a position from the caller's state.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::widgets::{Paragraph, Wrap};

/// What to do when text is wider than its area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // `Clip` is a configuration knob; not every site exercises it.
pub enum HorizontalOverflow {
    /// Truncate the line and end with `…`.
    Clip,
    /// Word-wrap onto the next line; ratatui falls back to char-wrap when a
    /// single word doesn't fit.
    Wrap,
}

/// What to do when text is taller than its area.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(dead_code)] // `Allow` is a configuration knob; not every site exercises it.
pub enum VerticalOverflow {
    /// Truncate vertically. The last visible line ends with `…` so users
    /// know more text was cut.
    Clip,
    /// Render whatever the area gives us; let ratatui drop overflow lines
    /// silently. Use when the caller has already sized the area to the text.
    Allow,
    /// Caller-controlled scroll offset. The pane renders content shifted up
    /// by `offset` rows and draws `↑`/`↓` indicators on the top/bottom-right
    /// edges when more content exists in that direction. The caller owns
    /// the offset value (typically in its UI state) and updates it in
    /// response to keys like PageUp/PageDown.
    Scroll { offset: u16 },
}

/// A bounded text region. Build with [`TextPane::new`], configure with the
/// builder methods, render with [`TextPane::render`].
///
/// ```text
/// let pane = TextPane::new(message)
///     .style(Style::default().fg(Color::Red))
///     .h_overflow(HorizontalOverflow::Wrap)
///     .v_overflow(VerticalOverflow::Clip);
/// pane.render(frame, status_rect);
/// ```
pub struct TextPane<'a> {
    text: &'a str,
    style: Style,
    h_overflow: HorizontalOverflow,
    v_overflow: VerticalOverflow,
}

impl<'a> TextPane<'a> {
    /// Defaults to `Wrap` horizontally and `Clip` vertically — the right
    /// posture for status messages, where wrapping is forgiving and
    /// truncation needs to be visible.
    pub fn new(text: &'a str) -> Self {
        Self {
            text,
            style: Style::default(),
            h_overflow: HorizontalOverflow::Wrap,
            v_overflow: VerticalOverflow::Clip,
        }
    }

    pub fn style(mut self, s: Style) -> Self {
        self.style = s;
        self
    }

    pub fn h_overflow(mut self, o: HorizontalOverflow) -> Self {
        self.h_overflow = o;
        self
    }

    pub fn v_overflow(mut self, o: VerticalOverflow) -> Self {
        self.v_overflow = o;
        self
    }

    /// Number of rows the text would occupy at the given width, given the
    /// configured horizontal overflow mode. Useful for laying out around the
    /// pane (e.g. anchoring a help line at `area + measured_height`).
    ///
    /// `Clip` returns the explicit-newline count. `Wrap` approximates the
    /// wrapped row count by char-width division — accurate enough for
    /// layout decisions; the actual wrap is performed by ratatui at render.
    pub fn measure(&self, width: u16) -> u16 {
        if width == 0 {
            return 0;
        }
        match self.h_overflow {
            HorizontalOverflow::Clip => self.text.lines().count().max(1).min(u16::MAX as usize) as u16,
            HorizontalOverflow::Wrap => {
                let w = width as usize;
                let rows: usize = self
                    .text
                    .lines()
                    .map(|line| line.chars().count().max(1).div_ceil(w))
                    .sum();
                rows.max(1).min(u16::MAX as usize) as u16
            }
        }
    }

    /// Maximum scroll offset for `text` rendered into `area_width × area_height`.
    /// Useful when the caller owns the scroll position and wants to clamp it
    /// without rendering. Returns `0` when content fits.
    #[allow(dead_code)] // public helper; not used in-tree yet but part of the API.
    pub fn max_scroll(&self, area_width: u16, area_height: u16) -> u16 {
        let needed = self.measure(area_width);
        needed.saturating_sub(area_height)
    }

    /// Render into `area`. Returns the height actually used (≤ `area.height`).
    pub fn render(self, frame: &mut Frame, area: Rect) -> u16 {
        if area.width == 0 || area.height == 0 {
            return 0;
        }

        let needed = self.measure(area.width);
        let must_truncate =
            matches!(self.v_overflow, VerticalOverflow::Clip) && needed > area.height;

        let display = if must_truncate {
            truncate_to_fit(self.text, area.width, area.height, self.h_overflow)
        } else {
            self.text.to_string()
        };

        // Pass the string directly so ratatui treats `\n` as forced line
        // breaks (a `Span` would collapse them into whitespace), then apply
        // the foreground style at the paragraph level so it covers all
        // generated lines.
        let mut paragraph = Paragraph::new(display).style(self.style);
        if let HorizontalOverflow::Wrap = self.h_overflow {
            paragraph = paragraph.wrap(Wrap { trim: false });
        }
        // Scroll mode: shift content up by the caller-supplied offset
        // (clamped to the actual scroll range), and draw edge indicators
        // after rendering so they sit on top of the body.
        let scroll_state = if let VerticalOverflow::Scroll { offset } = self.v_overflow {
            let max = needed.saturating_sub(area.height);
            let clamped = offset.min(max);
            paragraph = paragraph.scroll((clamped, 0));
            Some((clamped, max))
        } else {
            None
        };
        frame.render_widget(paragraph, area);

        if let Some((offset, max)) = scroll_state {
            draw_scroll_indicators(frame, area, offset, max);
        }

        needed.min(area.height)
    }
}

/// Draw `↑` / `↓` glyphs on the top-right and bottom-right edges of `area`
/// when the caller's scroll position has content above or below the visible
/// window. Glyphs are dim so they're noticeable but not loud.
fn draw_scroll_indicators(frame: &mut Frame, area: Rect, offset: u16, max_offset: u16) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let dim = Style::default().add_modifier(ratatui::style::Modifier::DIM);
    if offset > 0 && area.height > 0 {
        let r = Rect::new(area.x + area.width - 1, area.y, 1, 1);
        frame.render_widget(Paragraph::new("\u{2191}").style(dim), r);
    }
    if offset < max_offset && area.height > 1 {
        let r = Rect::new(area.x + area.width - 1, area.y + area.height - 1, 1, 1);
        frame.render_widget(Paragraph::new("\u{2193}").style(dim), r);
    }
}

/// Truncate `text` so the result fits within `width` × `height` rows when
/// rendered with the given horizontal overflow mode, ending with `…` to
/// signal the cut. Operates on character counts, which is fine for the
/// status-message domain (ASCII / mostly-narrow text). For East Asian
/// width-2 glyphs the `…` may land one column early; acceptable.
fn truncate_to_fit(
    text: &str,
    width: u16,
    height: u16,
    h_overflow: HorizontalOverflow,
) -> String {
    let w = width as usize;
    let h = height as usize;
    if w == 0 || h == 0 {
        return String::new();
    }

    match h_overflow {
        HorizontalOverflow::Clip => {
            // Take the first (height - 1) full lines, then a clipped final line
            // that ends with `…`.
            let keep = h.saturating_sub(1);
            let mut lines: Vec<String> = text
                .lines()
                .take(keep)
                .map(|s| clip_line(s, w))
                .collect();
            if let Some(rest) = text.lines().nth(keep) {
                lines.push(clip_line_with_ellipsis(rest, w));
            }
            lines.join("\n")
        }
        HorizontalOverflow::Wrap => {
            // Budget chars-per-row × (height - 1) for the visible body, then
            // one row's worth ending in `…`.
            let budget = w * h.saturating_sub(1);
            let mut taken = String::with_capacity(budget);
            let mut count = 0usize;
            for ch in text.chars() {
                if count >= budget {
                    break;
                }
                taken.push(ch);
                count += 1;
            }
            // Replace the last char with `…` so the truncation is visible.
            // If we didn't take anything, just emit `…`.
            if taken.is_empty() {
                "…".to_string()
            } else {
                taken.pop();
                taken.push('…');
                taken
            }
        }
    }
}

fn clip_line(line: &str, width: usize) -> String {
    if line.chars().count() <= width {
        line.to_string()
    } else {
        clip_line_with_ellipsis(line, width)
    }
}

fn clip_line_with_ellipsis(line: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let take = width.saturating_sub(1);
    let mut out: String = line.chars().take(take).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measure_wrap_short_message_one_row() {
        let pane = TextPane::new("hi");
        assert_eq!(pane.measure(40), 1);
    }

    #[test]
    fn measure_wrap_long_message_multiple_rows() {
        let long = "a".repeat(100);
        let pane = TextPane::new(&long);
        // 100 chars / 40 width = 3 rows (ceil).
        assert_eq!(pane.measure(40), 3);
    }

    #[test]
    fn measure_clip_counts_explicit_lines() {
        let pane = TextPane::new("one\ntwo\nthree").h_overflow(HorizontalOverflow::Clip);
        assert_eq!(pane.measure(40), 3);
    }

    #[test]
    fn measure_zero_width_is_zero() {
        let pane = TextPane::new("anything");
        assert_eq!(pane.measure(0), 0);
    }

    #[test]
    fn truncate_wrap_appends_ellipsis_when_overflowing() {
        let out = truncate_to_fit("abcdefghij", 4, 1, HorizontalOverflow::Wrap);
        // budget = 4 * 0 = 0 → "…"; height of 1 is the all-or-nothing case.
        assert_eq!(out, "…");

        // Two-row budget = 4 chars visible, 1 char becomes `…`.
        let out = truncate_to_fit("abcdefghij", 4, 2, HorizontalOverflow::Wrap);
        assert!(out.ends_with('…'), "got {out:?}");
        // `abc…` is the first 3 chars + ellipsis after popping the 4th.
        assert_eq!(out.chars().count(), 4);
    }

    #[test]
    fn truncate_clip_shows_first_lines_and_final_ellipsis() {
        let out = truncate_to_fit("one\ntwo\nthree\nfour", 10, 2, HorizontalOverflow::Clip);
        // 1 full line + 1 clipped line ending with `…`.
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "one");
        assert!(lines[1].ends_with('…'));
    }
}
