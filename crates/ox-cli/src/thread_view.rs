use crate::app::{ChatMessage, ThreadView};
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Paragraph, Wrap};

/// Render a single thread's messages into `area`.
pub fn draw_thread(frame: &mut Frame, view: &ThreadView, scroll: u16, theme: &Theme, area: Rect) {
    let mut lines: Vec<Line> = Vec::new();

    for msg in &view.messages {
        match msg {
            ChatMessage::User(text) => {
                lines.push(Line::from(""));
                // Handle multiline user messages
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

    let text = Text::from(lines);
    let msg_height = area.height as usize;
    let total_lines = text.lines.len();
    let computed_scroll = if scroll == 0 {
        total_lines.saturating_sub(msg_height) as u16
    } else {
        let max_scroll = total_lines.saturating_sub(msg_height) as u16;
        max_scroll.saturating_sub(scroll)
    };
    let widget = Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .scroll((computed_scroll, 0));
    frame.render_widget(widget, area);
}
