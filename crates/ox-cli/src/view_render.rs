//! View → ratatui translator.
//!
//! The translator is the single point in `ox-cli` that touches `ratatui`.
//! Renderers produce a [`View`] (curated, ratatui-agnostic widget set in
//! `ox-view`); this module turns that tree into ratatui draw calls against a
//! [`Frame`].
//!
//! Hygiene rules (enforced by review, not code):
//! - The translator is **dumb**. It does not inspect data values to decide
//!   *which* widget to render — that is a renderer concern. It only knows how
//!   to draw a `View`, variant by variant.
//! - `render_to_frame` is **total** over the `View` enum: every variant has
//!   an explicit arm, with no `_ => ...` catch-all.
//! - Mapping helpers (`map_color`, `map_style`, `map_modifiers`,
//!   `map_direction`, `map_align`) are 1:1 with ratatui's vocabulary, so
//!   adding a `Color` (or modifier, etc.) to `ox-view` forces a corresponding
//!   match-arm here at compile time.

use ox_view::{
    Align, BannerKind, Color, Direction, FormRow, FormValue, ListItem, ModifierSet, Padding,
    Sizing, Span, Style, StyledLine, View,
};
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment as RAlignment, Constraint, Direction as RDirection, Layout, Rect};
use ratatui::style::{Color as RColor, Modifier as RModifier, Style as RStyle};
use ratatui::text::{Line, Span as RSpan};
use ratatui::widgets::{
    Block, Borders, List as RList, ListItem as RListItem, ListState, Paragraph,
};

use crate::theme::Theme;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Render `view` into `area` of `frame`.
///
/// Total over the `View` enum.
pub(crate) fn render_to_frame(view: &View, frame: &mut Frame, area: Rect, theme: &Theme) {
    match view {
        View::Empty => {}
        View::Text { spans, align } => render_text(spans, *align, frame, area),
        View::Stack { dir, children } => render_stack(dir, children, frame, area, theme),
        View::Frame {
            title,
            title_right,
            content,
        } => render_frame(
            title.as_deref(),
            title_right.as_deref(),
            content,
            frame,
            area,
            theme,
        ),
        View::List { items, selected } => render_list(items, *selected, frame, area, theme),
        View::Form { rows, focused } => render_form(rows, *focused, frame, area, theme),
        View::Modal {
            background,
            foreground,
            dim,
        } => render_modal(background, foreground, *dim, frame, area, theme),
        View::Banner { kind, content } => render_banner(kind, content, frame, area),
        View::StatusBlock {
            title,
            lines,
            scroll_offset,
        } => render_status_block(title, lines, *scroll_offset, frame, area),
        View::Pad { padding, child } => render_pad(*padding, child, frame, area, theme),
    }
}

// ---------------------------------------------------------------------------
// Per-variant renderers
// ---------------------------------------------------------------------------

fn render_text(spans: &[Span], align: Align, frame: &mut Frame, area: Rect) {
    let line = Line::from(spans.iter().map(map_span).collect::<Vec<_>>());
    let para = Paragraph::new(line).alignment(map_align(align));
    frame.render_widget(para, area);
}

fn render_stack(
    dir: &Direction,
    children: &[(View, Sizing)],
    frame: &mut Frame,
    area: Rect,
    theme: &Theme,
) {
    let constraints: Vec<Constraint> = children.iter().map(|(_, s)| map_sizing(s)).collect();
    let chunks = Layout::default()
        .direction(map_direction(dir))
        .constraints(constraints)
        .split(area);
    for ((child, _), sub_area) in children.iter().zip(chunks.iter()) {
        render_to_frame(child, frame, *sub_area, theme);
    }
}

fn render_list(
    items: &[ListItem],
    selected: Option<usize>,
    frame: &mut Frame,
    area: Rect,
    _theme: &Theme,
) {
    let ritems: Vec<RListItem> = items
        .iter()
        .map(|it| {
            let mut primary_spans: Vec<RSpan> = Vec::new();
            if let Some(spans) = &it.primary_spans {
                for s in spans {
                    primary_spans.push(RSpan::styled(s.text.clone(), map_style(s.style)));
                }
            } else {
                primary_spans.push(RSpan::raw(it.primary.clone()));
            }
            if let Some(badge) = &it.badge {
                primary_spans.push(RSpan::raw(" "));
                primary_spans.push(RSpan::styled(
                    format!("[{}]", badge),
                    RStyle::default().add_modifier(RModifier::DIM),
                ));
            }
            let mut lines = vec![Line::from(primary_spans)];
            if let Some(secondary) = &it.secondary {
                lines.push(Line::from(RSpan::styled(
                    format!("  {}", secondary),
                    RStyle::default().add_modifier(RModifier::DIM),
                )));
            }
            RListItem::new(lines)
        })
        .collect();

    let list =
        RList::new(ritems).highlight_style(RStyle::default().add_modifier(RModifier::REVERSED));

    let mut state = ListState::default();
    state.select(selected);
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_form(
    rows: &[FormRow],
    focused: Option<usize>,
    frame: &mut Frame,
    area: Rect,
    _theme: &Theme,
) {
    let mut lines: Vec<Line> = Vec::with_capacity(rows.len());
    for (i, row) in rows.iter().enumerate() {
        let is_focused = focused == Some(i);
        let label = format!("{}: ", row.label);
        let value_text = format_form_value(&row.value);

        let mut spans: Vec<RSpan> = Vec::new();
        let label_style = if is_focused {
            RStyle::default().add_modifier(RModifier::BOLD | RModifier::UNDERLINED)
        } else {
            RStyle::default().add_modifier(RModifier::BOLD)
        };
        spans.push(RSpan::styled(label, label_style));

        let value_style = if is_focused {
            RStyle::default().add_modifier(RModifier::UNDERLINED)
        } else {
            RStyle::default()
        };
        spans.push(RSpan::styled(value_text, value_style));

        if let Some(hint) = &row.hint {
            spans.push(RSpan::raw("  "));
            spans.push(RSpan::styled(
                hint.clone(),
                RStyle::default().add_modifier(RModifier::DIM),
            ));
        }
        if let Some(err) = &row.error {
            spans.push(RSpan::raw("  "));
            spans.push(RSpan::styled(
                err.clone(),
                RStyle::default().fg(RColor::Red),
            ));
        }

        lines.push(Line::from(spans));
    }

    let para = Paragraph::new(lines);
    frame.render_widget(para, area);
}

/// Draws a bordered Block (with optional left- and right-aligned titles)
/// around `content`. The inner area shrinks to account for the border;
/// content recursively renders into that.
fn render_frame(
    title: Option<&str>,
    title_right: Option<&str>,
    content: &View,
    frame: &mut Frame,
    area: Rect,
    theme: &Theme,
) {
    let mut block = Block::default().borders(Borders::ALL);
    if let Some(t) = title {
        block = block.title_top(Line::from(t.to_string()));
    }
    if let Some(tr) = title_right {
        block = block.title_top(Line::from(tr.to_string()).right_aligned());
    }
    let inner = block.inner(area);
    frame.render_widget(block, area);
    render_to_frame(content, frame, inner, theme);
}

fn format_form_value(v: &FormValue) -> String {
    match v {
        FormValue::Text {
            value,
            cursor: _,
            masked,
        } => {
            if *masked {
                "\u{2022}".repeat(value.chars().count())
            } else {
                value.clone()
            }
        }
        FormValue::Selector { options, current } => {
            let opt = options.get(*current).map(String::as_str).unwrap_or("");
            format!("< {} >", opt)
        }
        FormValue::ReadOnly(s) => s.clone(),
    }
}

fn render_modal(
    background: &View,
    foreground: &View,
    dim: bool,
    frame: &mut Frame,
    area: Rect,
    theme: &Theme,
) {
    render_to_frame(background, frame, area, theme);
    if dim {
        dim_buffer(frame.buffer_mut(), area);
    }
    let centered = centered_rect(60, 50, area);
    render_to_frame(foreground, frame, centered, theme);
}

fn dim_buffer(buf: &mut Buffer, area: Rect) {
    for y in area.y..area.y.saturating_add(area.height) {
        for x in area.x..area.x.saturating_add(area.width) {
            if x < buf.area.x + buf.area.width && y < buf.area.y + buf.area.height {
                let cell = &mut buf[(x, y)];
                cell.set_style(RStyle::default().add_modifier(RModifier::DIM));
            }
        }
    }
}

fn render_banner(kind: &BannerKind, content: &str, frame: &mut Frame, area: Rect) {
    let style = match kind {
        BannerKind::Info => RStyle::default().bg(RColor::Blue).fg(RColor::White),
        BannerKind::Error => RStyle::default().bg(RColor::Red).fg(RColor::White),
    };
    let block = Block::default().borders(Borders::ALL).style(style);
    let para = Paragraph::new(content.to_string())
        .style(style)
        .block(block);
    frame.render_widget(para, area);
}

fn render_status_block(
    title: &str,
    lines: &[StyledLine],
    scroll_offset: u16,
    frame: &mut Frame,
    area: Rect,
) {
    let rlines: Vec<Line> = lines
        .iter()
        .map(|StyledLine(spans)| Line::from(spans.iter().map(map_span).collect::<Vec<_>>()))
        .collect();
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title.to_string());
    let para = Paragraph::new(rlines)
        .block(block)
        .scroll((scroll_offset, 0));
    frame.render_widget(para, area);
}

fn render_pad(padding: Padding, child: &View, frame: &mut Frame, area: Rect, theme: &Theme) {
    let h_inset = padding.left.saturating_add(padding.right);
    let v_inset = padding.top.saturating_add(padding.bottom);
    let inner = Rect {
        x: area.x.saturating_add(padding.left),
        y: area.y.saturating_add(padding.top),
        width: area.width.saturating_sub(h_inset),
        height: area.height.saturating_sub(v_inset),
    };
    render_to_frame(child, frame, inner, theme);
}

// ---------------------------------------------------------------------------
// Mapping helpers (1:1 with ratatui's vocabulary)
// ---------------------------------------------------------------------------

fn map_span(s: &Span) -> RSpan<'static> {
    RSpan::styled(s.text.clone(), map_style(s.style))
}

fn map_style(s: Style) -> RStyle {
    let mut out = RStyle::default();
    if let Some(fg) = s.fg {
        out = out.fg(map_color(fg));
    }
    if let Some(bg) = s.bg {
        out = out.bg(map_color(bg));
    }
    out.add_modifier(map_modifiers(s.modifiers))
}

fn map_modifiers(m: ModifierSet) -> RModifier {
    let mut out = RModifier::empty();
    if m.bold {
        out |= RModifier::BOLD;
    }
    if m.italic {
        out |= RModifier::ITALIC;
    }
    if m.underline {
        out |= RModifier::UNDERLINED;
    }
    if m.dim {
        out |= RModifier::DIM;
    }
    if m.reversed {
        out |= RModifier::REVERSED;
    }
    out
}

fn map_color(c: Color) -> RColor {
    match c {
        Color::Reset => RColor::Reset,
        Color::Black => RColor::Black,
        Color::Red => RColor::Red,
        Color::Green => RColor::Green,
        Color::Yellow => RColor::Yellow,
        Color::Blue => RColor::Blue,
        Color::Magenta => RColor::Magenta,
        Color::Cyan => RColor::Cyan,
        Color::White => RColor::White,
        Color::Gray => RColor::Gray,
        Color::DarkGray => RColor::DarkGray,
        Color::LightRed => RColor::LightRed,
        Color::LightGreen => RColor::LightGreen,
        Color::LightYellow => RColor::LightYellow,
        Color::LightBlue => RColor::LightBlue,
        Color::LightMagenta => RColor::LightMagenta,
        Color::LightCyan => RColor::LightCyan,
        Color::Indexed(i) => RColor::Indexed(i),
        Color::Rgb(r, g, b) => RColor::Rgb(r, g, b),
    }
}

fn map_direction(d: &Direction) -> RDirection {
    match d {
        Direction::Horizontal => RDirection::Horizontal,
        Direction::Vertical => RDirection::Vertical,
    }
}

fn map_align(a: Align) -> RAlignment {
    match a {
        Align::Left => RAlignment::Left,
        Align::Center => RAlignment::Center,
        Align::Right => RAlignment::Right,
    }
}

fn map_sizing(s: &Sizing) -> Constraint {
    match s {
        Sizing::Fill => Constraint::Min(0),
        Sizing::Fixed(n) => Constraint::Length(*n),
        Sizing::Min(n) => Constraint::Min(*n),
    }
}

// ---------------------------------------------------------------------------
// Geometry helper
// ---------------------------------------------------------------------------

/// Compute a centered sub-rect occupying `percent_x` / `percent_y` of `r`.
///
/// The translator wants percentage-based centering for `Modal` overlays.
fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_w = r.width.saturating_mul(percent_x) / 100;
    let popup_h = r.height.saturating_mul(percent_y) / 100;
    let x = r.x + (r.width.saturating_sub(popup_w)) / 2;
    let y = r.y + (r.height.saturating_sub(popup_h)) / 2;
    Rect::new(x, y, popup_w.min(r.width), popup_h.min(r.height))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ox_view::{
        Align, BannerKind, FormRow, FormValue, ListItem, Padding, Sizing, Span, StyledLine, View,
    };
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;

    fn render_view(view: View, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        let theme = Theme::default();
        terminal
            .draw(|f| render_to_frame(&view, f, f.area(), &theme))
            .unwrap();
        format_buffer(terminal.backend().buffer())
    }

    fn format_buffer(buf: &Buffer) -> String {
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let cell = &buf[(x, y)];
                out.push_str(cell.symbol());
            }
            // Trim trailing spaces so snapshots stay readable when the
            // last column is blank padding; preserve interior runs.
            while out.ends_with(' ') {
                out.pop();
            }
            out.push('\n');
        }
        out
    }

    // --- Empty -----------------------------------------------------------

    #[test]
    fn renders_empty_as_blank() {
        let out = render_view(View::Empty, 10, 3);
        insta::assert_snapshot!(out);
    }

    // --- Text ------------------------------------------------------------

    #[test]
    fn renders_text_left_aligned() {
        let view = View::Text {
            spans: vec![Span::plain("hello world")],
            align: Align::Left,
        };
        let out = render_view(view, 20, 1);
        insta::assert_snapshot!(out);
    }

    // --- Stack -----------------------------------------------------------

    #[test]
    fn renders_stack_vertical_with_two_children() {
        let view = View::stack_v(vec![
            (View::text("top"), Sizing::Fixed(1)),
            (View::text("bottom"), Sizing::Fill),
        ]);
        let out = render_view(view, 20, 3);
        insta::assert_snapshot!(out);
    }

    // --- List ------------------------------------------------------------

    #[test]
    fn renders_list_with_title_and_selection() {
        let view = View::Frame {
            title: Some("accounts".into()),
            title_right: None,
            content: Box::new(View::List {
                items: vec![
                    ListItem {
                        primary: "personal".into(),
                        primary_spans: None,
                        secondary: Some("anthropic".into()),
                        badge: Some("default".into()),
                        focus: None,
                    },
                    ListItem {
                        primary: "work".into(),
                        primary_spans: None,
                        secondary: Some("openai".into()),
                        badge: None,
                        focus: None,
                    },
                ],
                selected: Some(1),
            }),
        };
        let out = render_view(view, 30, 6);
        insta::assert_snapshot!(out);
    }

    // --- Form ------------------------------------------------------------

    #[test]
    fn renders_form_with_focused_row() {
        let view = View::Frame {
            title: Some("provider".into()),
            title_right: None,
            content: Box::new(View::Form {
                rows: vec![
                    FormRow {
                        label: "endpoint".into(),
                        value: FormValue::Text {
                            value: "https://api.example.com".into(),
                            cursor: 0,
                            masked: false,
                        },
                        error: None,
                        hint: Some("HTTPS URL".into()),
                    },
                    FormRow {
                        label: "api_key".into(),
                        value: FormValue::Text {
                            value: "secret".into(),
                            cursor: 0,
                            masked: true,
                        },
                        error: Some("required".into()),
                        hint: None,
                    },
                ],
                focused: Some(0),
            }),
        };
        let out = render_view(view, 60, 5);
        insta::assert_snapshot!(out);
    }

    // --- Modal -----------------------------------------------------------

    #[test]
    fn renders_modal_centered_over_background() {
        let background = View::stack_v(vec![
            (View::text("status"), Sizing::Fixed(1)),
            (
                View::Frame {
                    title: Some("items".into()),
                    title_right: None,
                    content: Box::new(View::List {
                        items: vec![ListItem {
                            primary: "alpha".into(),
                            primary_spans: None,
                            secondary: None,
                            badge: None,
                            focus: None,
                        }],
                        selected: Some(0),
                    }),
                },
                Sizing::Fill,
            ),
        ]);
        let foreground = View::Frame {
            title: Some("edit".into()),
            title_right: None,
            content: Box::new(View::Form {
                rows: vec![FormRow {
                    label: "name".into(),
                    value: FormValue::ReadOnly("personal".into()),
                    error: None,
                    hint: None,
                }],
                focused: Some(0),
            }),
        };
        let view = View::Modal {
            background: Box::new(background),
            foreground: Box::new(foreground),
            dim: true,
        };
        let out = render_view(view, 40, 12);
        insta::assert_snapshot!(out);
    }

    // --- Banner ----------------------------------------------------------

    #[test]
    fn renders_banner_error() {
        let view = View::Banner {
            kind: BannerKind::Error,
            content: "something failed".into(),
        };
        let out = render_view(view, 30, 3);
        insta::assert_snapshot!(out);
    }

    #[test]
    fn renders_banner_info() {
        let view = View::Banner {
            kind: BannerKind::Info,
            content: "saved".into(),
        };
        let out = render_view(view, 30, 3);
        insta::assert_snapshot!(out);
    }

    // --- StatusBlock -----------------------------------------------------

    #[test]
    fn renders_status_block_with_scroll_offset() {
        let view = View::StatusBlock {
            title: "log".into(),
            lines: vec![
                StyledLine(vec![Span::plain("line 1")]),
                StyledLine(vec![Span::plain("line 2")]),
                StyledLine(vec![Span::plain("line 3")]),
                StyledLine(vec![Span::plain("line 4")]),
                StyledLine(vec![Span::plain("line 5")]),
            ],
            scroll_offset: 2,
        };
        let out = render_view(view, 20, 5);
        insta::assert_snapshot!(out);
    }

    // --- Pad -------------------------------------------------------------

    #[test]
    fn renders_pad_insets_inner_view() {
        let view = View::pad(
            View::text("inside"),
            Padding {
                top: 1,
                right: 2,
                bottom: 1,
                left: 2,
            },
        );
        let out = render_view(view, 14, 4);
        insta::assert_snapshot!(out);
    }
}
