//! Settings screen rendering.

use crate::settings_state::SettingsState;
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

/// Draw the settings screen content area.
pub(crate) fn draw_settings(
    frame: &mut Frame,
    state: &SettingsState,
    theme: &Theme,
    area: Rect,
) {
    let title = if state.wizard.is_some() {
        " Setup Wizard "
    } else {
        " Settings "
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(theme.input_border);

    let line_count = state.accounts.len();
    let info = if line_count == 0 {
        "  No accounts configured. Press 'a' to add one.".to_string()
    } else {
        format!("  {} account(s) configured", line_count)
    };
    let lines = vec![
        Line::from(""),
        Line::from(Span::raw(info)),
        Line::from(""),
        Line::from(Span::styled(
            "  [a]dd  [e]dit  [d]elete  [t]est  [Esc] back",
            theme.status,
        )),
    ];
    frame.render_widget(Paragraph::new(lines).block(block), area);
}
