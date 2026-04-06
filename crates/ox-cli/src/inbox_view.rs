use crate::app::{App, ChatMessage};
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

/// Render the inbox thread list into `area`.
/// Uses `app.cached_threads` (refreshed once per frame by draw()).
pub fn draw_inbox(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let threads = &app.cached_threads;

    if threads.is_empty() {
        let empty = Paragraph::new(Line::from(Span::styled(
            if app.search.is_active() {
                "  No threads match the current filter"
            } else {
                "  No threads — press i to compose"
            },
            theme.assistant_text,
        )));
        frame.render_widget(empty, area);
        return;
    }

    let row_height = 2usize; // 2 lines per thread row
    let visible_rows = (area.height as usize) / row_height;

    let mut lines: Vec<Line> = Vec::new();
    let end = (app.inbox_scroll + visible_rows).min(threads.len());
    for (i, (_, title, state, labels, token_count, last_seq)) in
        threads.iter().enumerate().take(end).skip(app.inbox_scroll)
    {
        let is_selected = i == app.selected_row;
        let base_style = if is_selected {
            theme.selected_bg
        } else {
            ratatui::style::Style::default()
        };

        // Line 1: state label + title
        let (dot, state_label) = state_indicator(state, theme);
        let max_title = (area.width as usize).saturating_sub(state_label.len() + 6);
        let title_text = if title.len() > max_title {
            format!(
                "{}...",
                &title[..max_title.saturating_sub(3).min(title.len())]
            )
        } else {
            title.clone()
        };
        let title_style = if state == "completed" {
            theme.tool_meta
        } else {
            theme.user_text
        };

        lines.push(Line::from(vec![
            Span::styled(if is_selected { "▸ " } else { "  " }, base_style),
            dot,
            Span::raw(" "),
            Span::styled(title_text, title_style.patch(base_style)),
        ]));

        // Line 2: labels + activity + tokens (from in-memory ThreadView for live data)
        let mut meta_spans: Vec<Span> = vec![Span::styled("    ", base_style)];
        for label in labels {
            meta_spans.push(Span::styled(
                format!("[{label}] "),
                theme.tool_meta.patch(base_style),
            ));
        }

        let thread_id = &threads[i].0;
        if let Some(view) = app.thread_views.get(thread_id) {
            // Activity: show what the agent is currently doing
            if view.thinking {
                // Find the last tool call or show "streaming..."
                let activity = view
                    .messages
                    .iter()
                    .rev()
                    .find_map(|m| match m {
                        ChatMessage::ToolCall { name } => Some(format!("[{name}] ")),
                        _ => None,
                    })
                    .unwrap_or_else(|| "streaming... ".to_string());
                meta_spans.push(Span::styled(activity, theme.thinking.patch(base_style)));
            }

            // Token count from live view
            let total_tokens = view.tokens_in + view.tokens_out;
            if total_tokens > 0 {
                let tok_str = if total_tokens >= 1000 {
                    format!("{:.1}k tok ", total_tokens as f64 / 1000.0)
                } else {
                    format!("{total_tokens} tok ")
                };
                meta_spans.push(Span::styled(tok_str, theme.tool_meta.patch(base_style)));
            }

            // Message count
            let msg_count = view.messages.len();
            if msg_count > 0 {
                meta_spans.push(Span::styled(
                    format!("{msg_count} msgs"),
                    theme.tool_meta.patch(base_style),
                ));
            }
        } else {
            // Fallback to SQLite data for threads without a live view
            if *token_count > 0 {
                let tok_str = if *token_count >= 1000 {
                    format!("{:.1}k tok", *token_count as f64 / 1000.0)
                } else {
                    format!("{token_count} tok")
                };
                meta_spans.push(Span::styled(tok_str, theme.tool_meta.patch(base_style)));
            }
            if *last_seq >= 0 {
                let msg_count = *last_seq + 1;
                meta_spans.push(Span::styled(
                    format!(" {msg_count} msgs"),
                    theme.tool_meta.patch(base_style),
                ));
            }
        }
        lines.push(Line::from(meta_spans));
    }

    let list = Paragraph::new(lines);
    frame.render_widget(list, area);
}

/// Render the search/filter bar.
pub fn draw_filter_bar(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let mut spans = vec![Span::styled("/ ", theme.tool_name)];
    for (i, chip) in app.search.chips.iter().enumerate() {
        spans.push(Span::styled(
            format!("[{}: {}] ", i + 1, chip),
            theme.tool_meta,
        ));
    }
    spans.push(Span::styled(&app.search.live_query, theme.user_text));
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Colored state indicator: returns (dot_span, label_string) for layout width calculation.
fn state_indicator<'a>(state: &str, theme: &Theme) -> (Span<'a>, String) {
    match state {
        "running" => (
            Span::styled("● RUNNING", theme.state_running),
            "● RUNNING".to_string(),
        ),
        "blocked_on_approval" => (
            Span::styled("● BLOCKED", theme.state_blocked),
            "● BLOCKED".to_string(),
        ),
        "waiting_for_input" => (
            Span::styled("● WAITING", theme.state_waiting),
            "● WAITING".to_string(),
        ),
        "errored" => (
            Span::styled("● ERRORED", theme.state_errored),
            "● ERRORED".to_string(),
        ),
        "completed" => (
            Span::styled("● DONE", theme.state_completed),
            "● DONE".to_string(),
        ),
        _ => (Span::styled("● ???", theme.status), "● ???".to_string()),
    }
}
