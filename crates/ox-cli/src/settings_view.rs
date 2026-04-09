//! Settings screen rendering.

use crate::settings_state::{SettingsFocus, SettingsState, TestStatus};
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
        .constraints(if state.wizard.is_some() {
            vec![
                Constraint::Length(1), // wizard step
                Constraint::Min(5),    // accounts
                Constraint::Length(5), // defaults
            ]
        } else {
            vec![
                Constraint::Min(5),    // accounts
                Constraint::Length(5), // defaults
            ]
        })
        .split(area);

    let mut idx = 0;
    if let Some(ref step) = state.wizard {
        use crate::settings_state::WizardStep;
        let step_text = match step {
            WizardStep::AddAccount => " Step 1/2: Add your first account ",
            WizardStep::SetDefaults => " Step 2/2: Set your defaults (Enter to confirm) ",
            WizardStep::Done => " Setup complete! Press Enter to continue. ",
        };
        frame.render_widget(
            Paragraph::new(Span::styled(step_text, theme.title_badge)),
            chunks[idx],
        );
        idx += 1;
    }

    draw_accounts_section(frame, state, theme, chunks[idx]);
    idx += 1;
    draw_defaults_section(frame, state, theme, chunks[idx]);

    if let Some(ref editing) = state.editing {
        draw_account_edit_dialog(frame, editing, &state.test_status, theme);
    }
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

    // Show test status below the account rows (only when no edit dialog is open)
    if state.editing.is_none() {
        let test_line = match &state.test_status {
            TestStatus::Idle => None,
            TestStatus::Testing => Some("  \u{23f3} Testing...".to_string()),
            TestStatus::Success(msg) => Some(format!("  \u{2713} {msg}")),
            TestStatus::Failed(msg) => Some(format!("  \u{2717} {msg}")),
        };
        if let Some(text) = test_line {
            let y = inner.y + state.accounts.len().min(inner.height as usize - 1) as u16;
            if y < inner.y + inner.height {
                let status_area = Rect {
                    x: inner.x,
                    y,
                    width: inner.width,
                    height: 1,
                };
                frame.render_widget(Paragraph::new(Span::raw(text)), status_area);
            }
        }
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

/// Draw the account add/edit dialog as a centered overlay.
pub(crate) fn draw_account_edit_dialog(
    frame: &mut Frame,
    editing: &crate::settings_state::AccountEditFields,
    test_status: &crate::settings_state::TestStatus,
    theme: &Theme,
) {
    use crate::settings_state::{DIALECTS, TestStatus};
    use ratatui::widgets::Clear;

    let area = centered_rect(60, 10, frame.area());
    frame.render_widget(Clear, area);

    let title = if editing.is_new {
        " Add Account "
    } else {
        " Edit Account "
    };
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(theme.title_badge);

    let dialect_str = DIALECTS.get(editing.dialect).unwrap_or(&"anthropic");
    let key_display = if editing.key.is_empty() {
        "(empty)".to_string()
    } else {
        let len = editing.key.len();
        if len > 4 {
            format!(
                "{}...{}",
                "\u{25cf}".repeat((len - 4).min(10)),
                &editing.key[len - 4..]
            )
        } else {
            "\u{25cf}".repeat(len)
        }
    };

    let test_display = match test_status {
        TestStatus::Idle => String::new(),
        TestStatus::Testing => "  \u{23f3} Testing...".to_string(),
        TestStatus::Success(msg) => format!("  \u{2713} {msg}"),
        TestStatus::Failed(msg) => format!("  \u{2717} {msg}"),
    };

    let focus = editing.focus;
    let cursor = "\u{25b8} ";
    let no_cursor = "  ";

    let lines = vec![
        Line::from(format!(
            "  Name:     {}{}",
            if focus == 0 { cursor } else { no_cursor },
            editing.name
        )),
        Line::from(format!(
            "  Dialect:  {}{} (\u{2190}/\u{2192} to change)",
            if focus == 1 { cursor } else { no_cursor },
            dialect_str
        )),
        Line::from(format!(
            "  Endpoint: {}{}",
            if focus == 2 { cursor } else { no_cursor },
            if editing.endpoint.is_empty() {
                "(default for dialect)".to_string()
            } else {
                editing.endpoint.clone()
            }
        )),
        Line::from(format!(
            "  API Key:  {}{}",
            if focus == 3 { cursor } else { no_cursor },
            key_display
        )),
        Line::from(""),
        Line::from(format!(
            "  Tab next | t test | Enter save | Esc cancel{test_display}"
        )),
    ];

    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn centered_rect(percent_x: u16, height: u16, r: Rect) -> Rect {
    let popup_width = r.width * percent_x / 100;
    let x = (r.width.saturating_sub(popup_width)) / 2;
    let y = (r.height.saturating_sub(height)) / 2;
    Rect::new(
        r.x + x,
        r.y + y,
        popup_width.min(r.width),
        height.min(r.height),
    )
}
