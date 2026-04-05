use crate::app::App;
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

/// Render the title/header bar into `area`.
pub fn draw_tabs(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let spans = if let Some(ref tid) = app.active_thread {
        // Thread view — show thread title
        let title = app
            .thread_views
            .get(tid)
            .and_then(|v| v.messages.first())
            .map(|m| match m {
                crate::app::ChatMessage::User(s) => {
                    let truncated: String = s.chars().take(50).collect();
                    truncated
                }
                _ => tid.clone(),
            })
            .unwrap_or_else(|| tid.clone());
        vec![
            Span::styled(" ox ", theme.title_badge),
            Span::styled(format!(" {title} "), theme.title_info),
        ]
    } else {
        // Inbox view
        let count = app.cached_threads.len();
        vec![
            Span::styled(" ox ", theme.title_badge),
            Span::styled(format!(" inbox ({count}) "), theme.title_info),
            Span::styled(format!(" {} ({}) ", app.model, app.provider), theme.status),
        ]
    };

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}
