//! History explorer renderer — pure view over HistoryExplorer state.
//!
//! Receives pre-cached, pre-laid-out data from HistoryExplorer.
//! Only renders the visible entry range. No parsing, no I/O.

use crate::history_state::HistoryExplorer;
use crate::parse::{HistoryBlock, LogDisplayEntry};
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState};

#[allow(clippy::too_many_arguments)]
/// Render the history explorer screen from pre-computed state.
///
/// `selected` and `expanded` come from the UiSnapshot (UiStore owns these).
/// Everything else comes from `explorer` (event loop owns this).
pub fn draw_history(
    frame: &mut Frame,
    explorer: &HistoryExplorer,
    thread_id: &str,
    selected: usize,
    expanded: &std::collections::HashSet<usize>,
    thinking: bool,
    theme: &Theme,
    area: Rect,
) {
    let total = explorer.entry_count();
    let clamped_selected = if total == 0 {
        0
    } else {
        selected.min(total - 1)
    };
    let visible = explorer.layout.visible_range(total);

    let mut lines: Vec<Line> = Vec::new();

    // Header
    lines.push(Line::from(vec![
        Span::styled(" HISTORY ", theme.history_header),
        Span::styled(format!(" {} entries", total), theme.history_meta),
        Span::styled(
            format!("  {}", truncate_id(thread_id, 16)),
            theme.history_meta,
        ),
    ]));
    lines.push(Line::from(""));

    // Visible entries only
    let entries = explorer.cache.slice(visible.clone());
    for entry in entries {
        let is_selected = entry.index == clamped_selected;
        let cursor = if is_selected { ">" } else { " " };

        match entry.entry_type.as_str() {
            "turn_start" | "turn_end" => {
                render_turn_boundary(entry, cursor, theme, &mut lines);
            }
            "approval_requested" => {
                render_approval_requested(entry, cursor, theme, &mut lines);
            }
            "approval_resolved" => {
                render_approval_resolved(entry, cursor, theme, &mut lines);
            }
            "error" => {
                render_error(entry, cursor, theme, &mut lines);
            }
            "meta" => {
                render_meta(entry, cursor, theme, &mut lines);
            }
            _ => {
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
    if thinking {
        lines.push(Line::from(Span::styled(
            "  ... streaming",
            theme.history_streaming,
        )));
    }

    // Render as paragraph (no scroll offset — we already sliced to visible range)
    let text = Text::from(lines);
    let widget = Paragraph::new(text);
    frame.render_widget(widget, area);

    // Scrollbar
    if total > explorer.layout.visible_range(total).len() {
        let max_offset = total.saturating_sub(visible.len());
        let position = explorer.layout.scroll_offset();
        let mut scrollbar_state = ScrollbarState::new(max_offset).position(position);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
    }
}

// ---------------------------------------------------------------------------
// Per-entry-type renderers
// ---------------------------------------------------------------------------

fn render_turn_boundary(entry: &LogDisplayEntry, cursor: &str, theme: &Theme, out: &mut Vec<Line>) {
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
    out.push(Line::from(vec![
        Span::styled(format!("{cursor} "), theme.history_meta),
        Span::styled(format!("#{:<4} ", entry.index), theme.history_index),
        Span::styled(label, theme.history_turn_boundary),
    ]));
}

fn render_approval_requested(
    entry: &LogDisplayEntry,
    cursor: &str,
    theme: &Theme,
    out: &mut Vec<Line>,
) {
    let tool = entry.meta.tool_name.as_deref().unwrap_or("");
    let preview = entry.meta.input_preview.as_deref().unwrap_or("");
    let detail = if preview.is_empty() {
        tool.to_string()
    } else {
        format!("{tool}: \"{preview}\"")
    };
    out.push(Line::from(vec![
        Span::styled(format!("{cursor} "), theme.history_meta),
        Span::styled(format!("#{:<4} ", entry.index), theme.history_index),
        Span::styled("[approval?] ", theme.history_approval_ask),
        Span::styled(detail, theme.history_summary),
    ]));
}

fn render_approval_resolved(
    entry: &LogDisplayEntry,
    cursor: &str,
    theme: &Theme,
    out: &mut Vec<Line>,
) {
    let decision = entry.meta.decision.as_deref().unwrap_or("");
    let badge_style = if decision.starts_with("allow") {
        theme.history_approval_allow
    } else {
        theme.history_approval_deny
    };
    let tool = entry.meta.tool_name.as_deref().unwrap_or("");
    let badge = format!(
        "[{}] ",
        if decision.is_empty() {
            "resolved"
        } else {
            decision
        }
    );
    out.push(Line::from(vec![
        Span::styled(format!("{cursor} "), theme.history_meta),
        Span::styled(format!("#{:<4} ", entry.index), theme.history_index),
        Span::styled(badge, badge_style),
        Span::styled(tool.to_string(), theme.history_summary),
    ]));
}

fn render_error(entry: &LogDisplayEntry, cursor: &str, theme: &Theme, out: &mut Vec<Line>) {
    out.push(Line::from(vec![
        Span::styled(format!("{cursor} "), theme.history_meta),
        Span::styled(format!("#{:<4} ", entry.index), theme.history_index),
        Span::styled("[error] ", theme.history_duplicate),
        Span::styled(entry.summary.clone(), theme.history_summary),
    ]));
}

fn render_meta(entry: &LogDisplayEntry, cursor: &str, theme: &Theme, out: &mut Vec<Line>) {
    out.push(Line::from(vec![
        Span::styled(format!("{cursor} "), theme.history_meta),
        Span::styled(format!("#{:<4} ", entry.index), theme.history_index),
        Span::styled("[meta] ", theme.history_meta),
        Span::styled(entry.summary.clone(), theme.history_summary),
    ]));
}

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

fn truncate_id(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}...", &s[..max.saturating_sub(3)])
    }
}
