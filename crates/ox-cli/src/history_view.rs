use crate::parse::{parse_log_entries, LogDisplayEntry, HistoryBlock};
use crate::theme::Theme;
use crate::view_state::ViewState;
use ox_types::ScreenSnapshot;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap};

/// Render the history explorer screen.
/// Returns (content_height, viewport_height) for scroll_max feedback.
pub fn draw_history(
    frame: &mut Frame,
    vs: &ViewState,
    theme: &Theme,
    area: Rect,
) -> (usize, usize) {
    let snap = match &vs.ui.screen {
        ScreenSnapshot::History(s) => s,
        _ => return (0, area.height as usize),
    };

    let entries = parse_log_entries(&vs.raw_messages);
    let entry_count = entries.len();

    let selected_row = snap.selected_row.min(entry_count.saturating_sub(1));
    let scroll = snap.scroll;
    let expanded: std::collections::HashSet<usize> = snap.expanded.iter().copied().collect();

    let mut lines: Vec<Line> = Vec::new();

    // Header line
    lines.push(Line::from(vec![
        Span::styled(" HISTORY ", theme.history_header),
        Span::styled(format!(" {} messages", entry_count), theme.history_meta),
        Span::styled(format!("  {}", snap.thread_id), theme.history_meta),
    ]));
    lines.push(Line::from(""));

    // Entry rows
    for entry in &entries {
        let is_selected = entry.index == selected_row;
        let cursor = if is_selected { ">" } else { " " };

        match entry.entry_type.as_str() {
            "turn_start" | "turn_end" => {
                let token_info = match (entry.meta.input_tokens, entry.meta.output_tokens) {
                    (Some(i), Some(o)) if i > 0 || o > 0 => format!(" ({}in / {}out)", i, o),
                    (Some(i), None) if i > 0 => format!(" ({}in)", i),
                    (None, Some(o)) if o > 0 => format!(" ({}out)", o),
                    _ => String::new(),
                };
                let label = format!(
                    " ── {}{} ──",
                    entry.entry_type.replace('_', " "),
                    token_info
                );
                lines.push(Line::from(vec![
                    Span::styled(format!("{cursor} "), theme.history_meta),
                    Span::styled(format!("#{:<4} ", entry.index), theme.history_index),
                    Span::styled(label, theme.history_turn_boundary),
                ]));
            }
            "approval_requested" => {
                let tool = entry.meta.tool_name.as_deref().unwrap_or("");
                let preview = entry.meta.input_preview.as_deref().unwrap_or("");
                let detail = if preview.is_empty() {
                    tool.to_string()
                } else {
                    format!("{}: \"{}\"", tool, preview)
                };
                lines.push(Line::from(vec![
                    Span::styled(format!("{cursor} "), theme.history_meta),
                    Span::styled(format!("#{:<4} ", entry.index), theme.history_index),
                    Span::styled("[approval?] ", theme.history_approval_ask),
                    Span::styled(detail, theme.history_summary),
                ]));
            }
            "approval_resolved" => {
                let decision = entry.meta.decision.as_deref().unwrap_or("");
                let badge_style = if decision.starts_with("allow") {
                    theme.history_approval_allow
                } else {
                    theme.history_approval_deny
                };
                let tool = entry.meta.tool_name.as_deref().unwrap_or("");
                let badge = format!("[{}] ", if decision.is_empty() { "resolved" } else { decision });
                lines.push(Line::from(vec![
                    Span::styled(format!("{cursor} "), theme.history_meta),
                    Span::styled(format!("#{:<4} ", entry.index), theme.history_index),
                    Span::styled(badge, badge_style),
                    Span::styled(tool.to_string(), theme.history_summary),
                ]));
            }
            "error" => {
                lines.push(Line::from(vec![
                    Span::styled(format!("{cursor} "), theme.history_meta),
                    Span::styled(format!("#{:<4} ", entry.index), theme.history_index),
                    Span::styled("[error] ", theme.history_duplicate),
                    Span::styled(entry.summary.clone(), theme.history_summary),
                ]));
            }
            "meta" => {
                lines.push(Line::from(vec![
                    Span::styled(format!("{cursor} "), theme.history_meta),
                    Span::styled(format!("#{:<4} ", entry.index), theme.history_index),
                    Span::styled("[meta] ", theme.history_meta),
                    Span::styled(entry.summary.clone(), theme.history_summary),
                ]));
            }
            _ => {
                // "user", "assistant", "tool_call", "tool_result"
                render_message_entry(entry, is_selected, cursor, theme, &mut lines);
            }
        }

        // Expanded blocks
        if expanded.contains(&entry.index) && !entry.blocks.is_empty() {
            for block in &entry.blocks {
                render_block(block, theme, &mut lines);
            }
        }
    }

    // Streaming indicator
    if vs.turn.thinking {
        lines.push(Line::from(Span::styled(
            "  ... streaming",
            theme.history_streaming,
        )));
    }

    // Scroll model (same as thread_view.rs)
    let viewport_width = area.width as usize;
    let content_height = count_wrapped_lines(&lines, viewport_width);
    let viewport_height = area.height as usize;
    let max_scroll = content_height.saturating_sub(viewport_height);

    let computed_scroll = if scroll == 0 {
        0u16
    } else {
        scroll.min(max_scroll) as u16
    };

    let text = Text::from(lines);
    let widget = Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .scroll((computed_scroll, 0));
    frame.render_widget(widget, area);

    // Scrollbar
    if content_height > viewport_height {
        let scroll_position = computed_scroll as usize;
        let mut scrollbar_state = ScrollbarState::new(max_scroll).position(scroll_position);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
    }

    (content_height, viewport_height)
}

/// Render a message-type entry (user / assistant / tool_call / tool_result).
fn render_message_entry(
    entry: &LogDisplayEntry,
    is_selected: bool,
    cursor: &str,
    theme: &Theme,
    out: &mut Vec<Line>,
) {
    let role_style = match entry.entry_type.as_str() {
        "user" => theme.history_role_user,
        "assistant" => theme.history_role_assistant,
        _ => theme.history_role_tool,
    };
    let role_label = format!("{:<12}", entry.entry_type);

    let summary_style = if is_selected {
        theme.history_selected
    } else {
        theme.history_summary
    };

    let meta = format!(
        " ({} block{}, {} chars)",
        entry.meta.block_count,
        if entry.meta.block_count == 1 { "" } else { "s" },
        entry.meta.text_len,
    );

    let mut summary_line = vec![
        Span::styled(format!("{cursor} "), theme.history_meta),
        Span::styled(format!("#{:<4} ", entry.index), theme.history_index),
        Span::styled(role_label, role_style),
    ];

    if entry.flags.duplicate_content {
        let dup_label = match entry.flags.duplicate_of {
            Some(n) => format!(" [DUP of #{}]", n),
            None => " [DUP]".to_string(),
        };
        summary_line.push(Span::styled(dup_label, theme.history_duplicate));
    }

    summary_line.push(Span::styled(
        format!(" \"{}\"", entry.summary),
        summary_style,
    ));
    summary_line.push(Span::styled(meta, theme.history_meta));

    out.push(Line::from(summary_line));
}

/// Render an expanded content block, appending lines to `out`.
fn render_block(block: &HistoryBlock, theme: &Theme, out: &mut Vec<Line>) {
    match block.block_type.as_str() {
        "text" => {
            out.push(Line::from(Span::styled(
                "  [text]",
                theme.history_block_tag,
            )));
            if let Some(text) = &block.text {
                for line in text.lines() {
                    out.push(Line::from(Span::styled(
                        format!("    {line}"),
                        theme.history_block_content,
                    )));
                }
            }
        }
        "tool_use" => {
            let name = block.tool_name.as_deref().unwrap_or("unknown");
            let id = block
                .tool_use_id
                .as_deref()
                .map(|s| truncate_id(s, 16))
                .unwrap_or_default();
            out.push(Line::from(vec![
                Span::styled("  [tool_use] ", theme.history_block_tag),
                Span::styled(name.to_string(), theme.history_role_tool),
                Span::styled(format!(" {id}"), theme.history_meta),
            ]));
            if let Some(json) = &block.input_json {
                for line in json.lines() {
                    out.push(Line::from(Span::styled(
                        format!("    {line}"),
                        theme.history_block_content,
                    )));
                }
            }
        }
        "tool_result" => {
            let id = block
                .tool_use_id
                .as_deref()
                .map(|s| truncate_id(s, 16))
                .unwrap_or_default();
            out.push(Line::from(vec![
                Span::styled("  [tool_result] ", theme.history_block_tag),
                Span::styled(id, theme.history_meta),
            ]));
            if let Some(text) = &block.text {
                let preview: Vec<&str> = text.lines().take(10).collect();
                let total = text.lines().count();
                for line in &preview {
                    out.push(Line::from(Span::styled(
                        format!("    {line}"),
                        theme.history_block_content,
                    )));
                }
                if total > 10 {
                    out.push(Line::from(Span::styled(
                        format!("    ... ({} more lines)", total - 10),
                        theme.history_meta,
                    )));
                }
            }
        }
        other => {
            out.push(Line::from(Span::styled(
                format!("  [{other}]"),
                theme.history_block_tag,
            )));
        }
    }
}

/// Count total rendered lines after word-wrapping (same as thread_view.rs).
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

/// Truncate `s` to `max` chars with `...` suffix if needed.
fn truncate_id(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max.saturating_sub(3)])
    }
}
