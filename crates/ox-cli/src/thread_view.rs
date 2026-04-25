use crate::theme::Theme;
use crate::types::{ChatMessage, ThreadView};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap};

/// Render a single thread's messages into `area`.
///
/// Returns the total content height in rendered lines (after wrapping)
/// so the caller can set scroll_max on UiStore.
///
/// `ledger_banner` is `Some(text)` when the thread mounted with a
/// non-`Ok` ledger health (Missing / RepairFailed / Degraded). Rendered
/// once at the very top of the transcript with the muted-accent
/// `theme.ledger_banner` style.
///
/// `approval` is `Some(req)` when a tool-call permission decision is
/// pending. The card is appended at the tail of the transcript using
/// [`crate::dialogs::build_approval_card_lines`] — non-blocking, so the
/// user can scroll to read prior context before deciding.
#[allow(clippy::too_many_arguments)]
pub fn draw_thread(
    frame: &mut Frame,
    view: &ThreadView,
    scroll: u16,
    theme: &Theme,
    ledger_banner: Option<&str>,
    approval: Option<&ox_types::ApprovalRequest>,
    approval_selected: usize,
    area: Rect,
) -> usize {
    let mut lines: Vec<Line> = Vec::new();

    if let Some(text) = ledger_banner {
        lines.push(Line::from(Span::styled(
            format!(" ! {text}"),
            theme.ledger_banner,
        )));
        lines.push(Line::from(""));
    }

    for msg in &view.messages {
        match msg {
            ChatMessage::User(text) => {
                lines.push(Line::from(""));
                for (i, line) in text.lines().enumerate() {
                    let prefix = if i == 0 { "> " } else { "  " };
                    lines.push(Line::from(vec![
                        Span::styled(prefix, theme.user_prompt),
                        Span::styled(line, theme.user_text),
                    ]));
                }
                lines.push(Line::from(""));
            }
            ChatMessage::AssistantChunk(text) => {
                for line in text.lines() {
                    lines.push(Line::from(Span::styled(line, theme.assistant_text)));
                }
            }
            ChatMessage::CompletionMeta {
                model,
                input_tokens,
                output_tokens,
                cache_creation_input_tokens,
                cache_read_input_tokens,
            } => {
                let cost_str = ox_gate::pricing::estimate_cost_full(
                    model,
                    *input_tokens,
                    *output_tokens,
                    *cache_creation_input_tokens,
                    *cache_read_input_tokens,
                )
                .map(|c| format!(" ${:.4}", c))
                .unwrap_or_default();
                lines.push(Line::from(Span::styled(
                    format!("  [{model}] {input_tokens}in/{output_tokens}out{cost_str}",),
                    theme.tool_meta,
                )));
            }
            ChatMessage::ToolCall { name } => {
                lines.push(Line::from(vec![
                    Span::styled(format!("  [{name}] "), theme.tool_name),
                    Span::styled("running...", theme.tool_running),
                ]));
            }
            ChatMessage::ToolResult { name, output } => {
                let line_count = output.lines().count();
                let preview_lines: Vec<&str> = output.lines().take(5).collect();

                lines.push(Line::from(vec![
                    Span::styled(format!("  [{name}] "), theme.tool_name),
                    Span::styled(
                        if line_count > 5 {
                            format!("({line_count} lines)")
                        } else {
                            format!(
                                "({line_count} line{})",
                                if line_count == 1 { "" } else { "s" }
                            )
                        },
                        theme.tool_meta,
                    ),
                ]));
                for pl in &preview_lines {
                    lines.push(Line::from(Span::styled(
                        format!("  | {pl}"),
                        theme.tool_output,
                    )));
                }
                if line_count > 5 {
                    lines.push(Line::from(Span::styled(
                        format!("  | ... ({} more)", line_count - 5),
                        theme.tool_overflow,
                    )));
                }
            }
            ChatMessage::Error(e) => {
                lines.push(Line::from(Span::styled(
                    format!("  error: {e}"),
                    theme.error,
                )));
            }
        }
    }

    // Thinking indicator
    if view.thinking && !matches!(view.messages.last(), Some(ChatMessage::AssistantChunk(_))) {
        lines.push(Line::from(Span::styled("  ...", theme.thinking)));
    }

    // Pending-approval card. Appended last so it sits at the bottom of
    // the transcript; conversation scrolling continues to work above it.
    if let Some(req) = approval {
        lines.extend(crate::dialogs::build_approval_card_lines(
            &req.tool_name,
            &req.tool_input,
            approval_selected,
            theme,
        ));
    }

    // Count rendered lines after wrapping
    let viewport_width = area.width as usize;
    let content_height = count_wrapped_lines(&lines, viewport_width);
    let viewport_height = area.height as usize;
    let max_scroll = content_height.saturating_sub(viewport_height);

    // Compute ratatui scroll offset (scroll=0 means showing bottom/newest)
    let computed_scroll = if scroll == 0 {
        max_scroll as u16
    } else {
        (max_scroll as u16).saturating_sub(scroll)
    };

    let text = Text::from(lines);
    let widget = Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .scroll((computed_scroll, 0));
    frame.render_widget(widget, area);

    // Scroll bar
    if content_height > viewport_height {
        let scroll_position = max_scroll.saturating_sub(scroll as usize);
        let mut scrollbar_state = ScrollbarState::new(max_scroll).position(scroll_position);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
    }

    content_height
}

/// Count the total rendered lines after word wrapping.
///
/// For each Line, estimates the wrapped line count based on the sum
/// of span character widths divided by viewport width.
fn count_wrapped_lines(lines: &[Line], width: usize) -> usize {
    if width == 0 {
        return lines.len();
    }
    lines
        .iter()
        .map(|line| {
            let w: usize = line.spans.iter().map(|s| s.content.len()).sum();
            if w == 0 { 1 } else { w.div_ceil(width) }
        })
        .sum()
}
