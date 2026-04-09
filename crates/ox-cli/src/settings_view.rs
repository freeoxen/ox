//! Settings screen rendering.

use crate::settings_state::{SettingsFocus, SettingsState};
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

/// Draw the settings screen content area.
pub(crate) fn draw_settings(
    frame: &mut Frame,
    state: &SettingsState,
    theme: &Theme,
    area: Rect,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(5),    // accounts
            Constraint::Length(5), // defaults
        ])
        .split(area);

    draw_accounts_section(frame, state, theme, chunks[0]);
    draw_defaults_section(frame, state, theme, chunks[1]);
}

fn draw_accounts_section(
    frame: &mut Frame,
    state: &SettingsState,
    theme: &Theme,
    area: Rect,
) {
    let focused = state.focus == SettingsFocus::Accounts && state.editing.is_none();
    let block = Block::default()
        .title(" Accounts ")
        .borders(Borders::ALL)
        .border_style(if focused {
            theme.title_badge
        } else {
            theme.input_border
        });

    if state.accounts.is_empty() {
        let empty = Paragraph::new("  No accounts configured. Press 'a' to add one.")
            .block(block);
        frame.render_widget(empty, area);
        return;
    }

    let inner = block.inner(area);
    frame.render_widget(block, area);

    for (i, acct) in state.accounts.iter().enumerate() {
        if i >= inner.height as usize {
            break;
        }
        let marker = if acct.is_default { "\u{25cf}" } else { " " };
        let key_status = if acct.has_key { "\u{2713}" } else { "\u{2717}" };
        let selected = i == state.selected_account && focused;

        let line_str = format!(
            " {} {:<16} {:<12} {:<24} {}",
            marker, acct.name, acct.dialect, acct.endpoint_display, key_status
        );

        let style = if selected {
            theme.selected_bg
        } else {
            Style::default()
        };

        let line_area = Rect {
            x: inner.x,
            y: inner.y + i as u16,
            width: inner.width,
            height: 1,
        };
        frame.render_widget(Paragraph::new(Span::styled(line_str, style)), line_area);
    }
}

fn draw_defaults_section(
    frame: &mut Frame,
    state: &SettingsState,
    theme: &Theme,
    area: Rect,
) {
    let focused = state.focus == SettingsFocus::Defaults && state.editing.is_none();
    let block = Block::default()
        .title(" Defaults ")
        .borders(Borders::ALL)
        .border_style(if focused {
            theme.title_badge
        } else {
            theme.input_border
        });

    let acct_name = state
        .accounts
        .get(state.default_account_idx)
        .map(|a| a.name.as_str())
        .unwrap_or("(none)");

    let lines = vec![
        Line::from(format!("  Account:    {acct_name}")),
        Line::from(format!("  Max tokens: {}", state.default_max_tokens)),
    ];
    frame.render_widget(Paragraph::new(lines).block(block), area);
}
