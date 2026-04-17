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

// ---------------------------------------------------------------------------
// Hit map — mouse click target tracking
// ---------------------------------------------------------------------------

/// Maps screen rows to clickable history entry targets.
/// Built during rendering, consumed by the event loop for mouse dispatch.
pub struct HistoryHitMap {
    /// Top-left Y coordinate of the content area.
    pub area_y: u16,
    /// Top-left X coordinate of the content area.
    pub area_x: u16,
    /// Line-based content scroll applied during rendering.
    pub content_scroll: u16,
    /// Per-entry hit targets.
    pub entries: Vec<HitEntry>,
}

/// Click targets for a single history entry.
pub struct HitEntry {
    /// The logical entry index in the log cache.
    pub entry_index: usize,
    /// Row offset (from area_y) of the summary line.
    pub summary_row: u16,
    /// Toolbar hit targets (only present when expanded).
    pub toolbar: Option<ToolbarHit>,
}

/// Column ranges for clickable toggles on the toolbar line.
pub struct ToolbarHit {
    /// Row offset (from area_y) of the toolbar line.
    pub row: u16,
    /// (start_col, end_col) for the pretty/raw toggle, relative to area start.
    pub pretty_cols: (u16, u16),
    /// (start_col, end_col) for the full/truncated toggle, relative to area start.
    pub full_cols: (u16, u16),
}

impl HistoryHitMap {
    fn new(area: Rect, content_scroll: u16) -> Self {
        Self {
            area_y: area.y,
            area_x: area.x,
            content_scroll,
            entries: Vec::new(),
        }
    }
}

#[allow(clippy::too_many_arguments)]
/// Render the history explorer screen from pre-computed state.
///
/// `selected` and `expanded` come from the UiSnapshot (UiStore owns these).
/// Everything else comes from `explorer` (event loop owns this).
pub fn draw_history(
    frame: &mut Frame,
    explorer: &mut HistoryExplorer,
    thread_id: &str,
    selected: usize,
    expanded: &std::collections::HashSet<usize>,
    pretty: &std::collections::HashSet<usize>,
    full: &std::collections::HashSet<usize>,
    thinking: bool,
    theme: &Theme,
    area: Rect,
) -> HistoryHitMap {
    let total = explorer.entry_count();
    let clamped_selected = if total == 0 {
        0
    } else {
        selected.min(total - 1)
    };
    let visible = explorer.layout.visible_range(total);

    let mut lines: Vec<Line> = Vec::new();
    let content_scroll = explorer.content_scroll();
    let mut hit_map = HistoryHitMap::new(area, content_scroll);

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
        let is_expanded = expanded.contains(&entry.index);
        let has_blocks = !entry.blocks.is_empty();

        // Record summary line row for hit map
        let summary_row = lines.len() as u16;

        match entry.entry_type.as_str() {
            "turn_start" | "turn_end" => {
                render_turn_boundary(entry, cursor, theme, &mut lines);
            }
            "completion_end" => {
                render_completion_end(entry, cursor, theme, &mut lines);
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
                render_message_entry(
                    entry,
                    is_selected,
                    cursor,
                    has_blocks,
                    is_expanded,
                    theme,
                    &mut lines,
                );
            }
        }

        // Expanded toolbar + blocks
        let mut toolbar_hit = None;
        if is_expanded && has_blocks {
            let is_pretty = pretty.contains(&entry.index);
            let is_full = full.contains(&entry.index);

            // Toolbar line with toggle indicators
            let toolbar_row = lines.len() as u16;
            toolbar_hit = Some(render_toolbar(
                toolbar_row,
                is_pretty,
                is_full,
                entry,
                theme,
                &mut lines,
            ));

            for block in &entry.blocks {
                render_block(block, theme, area.width, is_pretty, is_full, &mut lines);
            }
        }

        hit_map.entries.push(HitEntry {
            entry_index: entry.index,
            summary_row,
            toolbar: toolbar_hit,
        });
    }

    // Streaming indicator
    if thinking {
        lines.push(Line::from(Span::styled(
            "  ... streaming",
            theme.history_streaming,
        )));
    }

    // Feed render metrics back to explorer so it can clamp content_scroll
    // and auto-adjust to keep the selected entry visible.
    let content_height = lines.len() as u16;
    let viewport_height = area.height;
    let selected_summary_row = hit_map
        .entries
        .iter()
        .find(|e| e.entry_index == clamped_selected)
        .map(|e| e.summary_row);
    explorer.set_render_metrics(content_height, viewport_height, selected_summary_row);
    let content_scroll = explorer.content_scroll();

    // Render with line-based scroll — allows viewing expanded content
    // that overflows the viewport.
    let text = Text::from(lines);
    let widget = Paragraph::new(text).scroll((content_scroll, 0));
    frame.render_widget(widget, area);

    // Scrollbar — reflects content scroll position when content overflows
    if content_height > viewport_height {
        let max_scroll = content_height.saturating_sub(viewport_height) as usize;
        let position = content_scroll as usize;
        let mut scrollbar_state = ScrollbarState::new(max_scroll).position(position);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
    } else if total > explorer.layout.visible_range(total).len() {
        // Entry-level scrollbar when content fits but entries are clipped
        let max_offset = total.saturating_sub(visible.len());
        let position = explorer.layout.scroll_offset();
        let mut scrollbar_state = ScrollbarState::new(max_offset).position(position);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight);
        frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
    }

    // Update hit map with final content_scroll (may have been adjusted)
    hit_map.content_scroll = content_scroll;

    hit_map
}

// ---------------------------------------------------------------------------
// Per-entry-type renderers
// ---------------------------------------------------------------------------

fn render_completion_end(
    entry: &LogDisplayEntry,
    cursor: &str,
    theme: &Theme,
    out: &mut Vec<Line>,
) {
    let model = entry.meta.model.as_deref().unwrap_or("?");
    let in_tok = entry.meta.input_tokens.unwrap_or(0);
    let out_tok = entry.meta.output_tokens.unwrap_or(0);
    let cc = entry.meta.cache_creation_input_tokens.unwrap_or(0);
    let cr = entry.meta.cache_read_input_tokens.unwrap_or(0);
    let cost_str = ox_gate::pricing::estimate_cost_full(model, in_tok, out_tok, cc, cr)
        .map(|c| format!(" ${:.6}", c))
        .unwrap_or_default();
    let label = format!(" ── completion ── {model} ({in_tok}in / {out_tok}out){cost_str}");
    out.push(Line::from(vec![
        Span::styled(format!("{cursor} "), theme.history_meta),
        Span::styled(format!("#{:<4} ", entry.index), theme.history_index),
        Span::styled(label, theme.history_turn_boundary),
    ]));
}

fn render_turn_boundary(entry: &LogDisplayEntry, cursor: &str, theme: &Theme, out: &mut Vec<Line>) {
    let in_tok = entry.meta.input_tokens.unwrap_or(0);
    let out_tok = entry.meta.output_tokens.unwrap_or(0);
    let token_info = if in_tok > 0 || out_tok > 0 {
        let cc = entry.meta.cache_creation_input_tokens.unwrap_or(0);
        let cr = entry.meta.cache_read_input_tokens.unwrap_or(0);
        let model = entry.meta.model.as_deref().unwrap_or("");
        let cost_str = ox_gate::pricing::estimate_cost_full(model, in_tok, out_tok, cc, cr)
            .map(|c| format!(" ${:.6}", c))
            .unwrap_or_default();
        format!(" ({in_tok}in / {out_tok}out){cost_str}")
    } else {
        String::new()
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
        Span::styled(format!("{cursor} "), theme.history_approval_ask),
        Span::styled(format!("#{:<4} ", entry.index), theme.history_approval_ask),
        Span::styled("[approval?] ", theme.history_approval_ask),
        Span::styled(detail, theme.history_approval_ask),
    ]));
}

fn render_approval_resolved(
    entry: &LogDisplayEntry,
    cursor: &str,
    theme: &Theme,
    out: &mut Vec<Line>,
) {
    let decision = entry.meta.decision;
    let badge_style = match decision {
        Some(d) if d.is_allow() => theme.history_approval_allow,
        Some(_) => theme.history_approval_deny,
        None => theme.history_meta,
    };
    let tool = entry.meta.tool_name.as_deref().unwrap_or("");
    let badge = format!("[{}] ", decision.map(|d| d.as_str()).unwrap_or("resolved"));
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
    has_blocks: bool,
    is_expanded: bool,
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

    // Chevron indicating expandable/expanded state
    if has_blocks {
        let chevron = if is_expanded {
            " \u{25BC}"
        } else {
            " \u{25B6}"
        };
        summary_line.push(Span::styled(chevron, theme.history_block_tag));
    }

    out.push(Line::from(summary_line));
}

/// Render the toolbar line with pretty/full toggle indicators.
/// Returns a `ToolbarHit` with column ranges for mouse click targets.
fn render_toolbar(
    toolbar_row: u16,
    is_pretty: bool,
    is_full: bool,
    entry: &LogDisplayEntry,
    theme: &Theme,
    out: &mut Vec<Line>,
) -> ToolbarHit {
    let indent = "    ";

    // Pretty/raw toggle
    let pretty_label = if is_pretty { "[pretty]" } else { "[raw]" };
    let pretty_style = if is_pretty {
        theme.history_block_tag
    } else {
        theme.history_meta
    };
    let pretty_start = indent.len() as u16;
    let pretty_end = pretty_start + pretty_label.len() as u16;

    // Full/truncated toggle
    let total_lines: usize = entry
        .blocks
        .iter()
        .map(|b| {
            b.text
                .as_ref()
                .or(b.input_json.as_ref())
                .map(|t| t.lines().count())
                .unwrap_or(0)
        })
        .sum();
    let is_truncatable = total_lines > 30;
    let full_label = if is_full {
        format!("[{} lines]", total_lines)
    } else if is_truncatable {
        format!("[{}/{}]", 30, total_lines)
    } else {
        format!("[{} lines]", total_lines)
    };
    let full_style = if is_full {
        theme.history_block_tag
    } else {
        theme.history_meta
    };
    // 2 spaces gap between toggles
    let full_start = pretty_end + 2;
    let full_end = full_start + full_label.len() as u16;

    out.push(Line::from(vec![
        Span::raw(indent),
        Span::styled(pretty_label, pretty_style),
        Span::raw("  "),
        Span::styled(full_label, full_style),
    ]));

    ToolbarHit {
        row: toolbar_row,
        pretty_cols: (pretty_start, pretty_end),
        full_cols: (full_start, full_end),
    }
}

fn render_block(
    block: &HistoryBlock,
    theme: &Theme,
    width: u16,
    is_pretty: bool,
    is_full: bool,
    out: &mut Vec<Line>,
) {
    let indent = "    ";
    let content_width = (width as usize).saturating_sub(indent.len());
    let max_lines = if is_full { usize::MAX } else { 30 };

    match block.block_type.as_str() {
        "text" => {
            out.push(Line::from(Span::styled(
                "  [text]",
                theme.history_block_tag,
            )));
            if let Some(text) = &block.text {
                let display = if is_pretty {
                    try_pretty_json(text)
                } else {
                    text.clone()
                };
                render_content(&display, indent, content_width, max_lines, theme, out);
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
                let display = if is_pretty {
                    try_pretty_json(json)
                } else {
                    json.clone()
                };
                render_content(&display, indent, content_width, max_lines, theme, out);
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
                let display = if is_pretty {
                    try_pretty_json(text)
                } else {
                    text.clone()
                };
                render_content(&display, indent, content_width, max_lines, theme, out);
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

/// Render content lines with wrapping and optional truncation.
fn render_content(
    text: &str,
    indent: &str,
    content_width: usize,
    max_lines: usize,
    theme: &Theme,
    out: &mut Vec<Line>,
) {
    let source_lines: Vec<&str> = text.lines().collect();
    let total = source_lines.len();
    let showing = total.min(max_lines);
    push_wrapped(
        &source_lines[..showing].join("\n"),
        indent,
        content_width,
        theme.history_block_content,
        out,
    );
    if total > max_lines {
        out.push(Line::from(Span::styled(
            format!(
                "{indent}... ({} more lines, press f for full)",
                total - max_lines
            ),
            theme.history_meta,
        )));
    }
}

/// Try to parse as JSON and pretty-print; return original text if not JSON.
fn try_pretty_json(text: &str) -> String {
    // Try parsing as JSON value for pretty-printing
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(text) {
        if let Ok(pretty) = serde_json::to_string_pretty(&val) {
            return pretty;
        }
    }
    text.to_string()
}

/// Push text lines with soft-wrapping at the given width.
fn push_wrapped(
    text: &str,
    indent: &str,
    max_width: usize,
    style: ratatui::style::Style,
    out: &mut Vec<Line>,
) {
    if max_width == 0 {
        return;
    }
    for line in text.lines() {
        if line.len() <= max_width {
            out.push(Line::from(Span::styled(format!("{indent}{line}"), style)));
        } else {
            // Soft-wrap at max_width
            let mut remaining = line;
            while !remaining.is_empty() {
                let split = if remaining.len() <= max_width {
                    remaining.len()
                } else {
                    // Find a char boundary at or before max_width
                    let mut end = max_width;
                    while end > 0 && !remaining.is_char_boundary(end) {
                        end -= 1;
                    }
                    if end == 0 {
                        // Pathological: single char wider than terminal
                        remaining.len()
                    } else {
                        end
                    }
                };
                let (chunk, rest) = remaining.split_at(split);
                out.push(Line::from(Span::styled(format!("{indent}{chunk}"), style)));
                remaining = rest;
            }
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
