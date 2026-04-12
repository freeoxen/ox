use crate::theme::Theme;
use crate::view_state::ViewState;
use ox_types::UiSnapshot;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

/// Render the title/header bar into `area`.
pub fn draw_tabs(frame: &mut Frame, vs: &ViewState, theme: &Theme, area: Rect) {
    let spans = if matches!(&vs.ui, UiSnapshot::Settings(_)) {
        vec![
            Span::styled(" ox ", theme.title_badge),
            Span::styled(" settings ", theme.title_info),
        ]
    } else if let UiSnapshot::Thread(ref snap) = vs.ui {
        let tid = &snap.thread_id;
        // Thread view — show thread title from messages
        let title = vs
            .messages
            .first()
            .map(|m| match m {
                crate::types::ChatMessage::User(s) => {
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
        let count = vs.inbox_threads.len();
        vec![
            Span::styled(" ox ", theme.title_badge),
            Span::styled(format!(" inbox ({count}) "), theme.title_info),
            Span::styled(format!(" {} ({}) ", vs.model, vs.provider), theme.status),
        ]
    };

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}
