//! Settings screen rendering.

use crate::settings_state::{SettingsFocus, SettingsState, TestStatus};
use crate::theme::Theme;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

/// Draw the settings screen content area.
pub(crate) fn draw_settings(frame: &mut Frame, state: &SettingsState, theme: &Theme, area: Rect) {
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
        draw_account_edit_dialog(frame, editing, &state.test_status, state.status_scroll, theme);
    }
}

fn draw_accounts_section(frame: &mut Frame, state: &SettingsState, theme: &Theme, area: Rect) {
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
        let empty = Paragraph::new("  No accounts configured. Press 'a' to add one.").block(block);
        frame.render_widget(empty, area);
        return;
    }

    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Column header
    let header = format!(
        "     {:<16} {:<12} {:<24} {}",
        "Name", "API", "Endpoint", "Key"
    );
    if inner.height > 0 {
        let header_area = Rect {
            x: inner.x,
            y: inner.y,
            width: inner.width,
            height: 1,
        };
        frame.render_widget(
            Paragraph::new(Span::styled(
                header,
                Style::default().add_modifier(Modifier::DIM),
            )),
            header_area,
        );
    }

    for (i, acct) in state.accounts.iter().enumerate() {
        if i + 1 >= inner.height as usize {
            break;
        }
        let select_marker = if i == state.selected_account {
            "\u{25b8}"
        } else {
            " "
        };
        let default_marker = if acct.is_default { "\u{25cf}" } else { " " };
        let key_status = if acct.has_key { "\u{2713}" } else { "\u{2717}" };

        let line_str = format!(
            "{}{} {:<16} {:<12} {:<24} {}",
            select_marker,
            default_marker,
            acct.name,
            acct.dialect,
            acct.endpoint_display,
            key_status
        );

        let style = if i == state.selected_account && focused {
            theme.selected_bg
        } else {
            Style::default()
        };

        let line_area = Rect {
            x: inner.x,
            y: inner.y + 1 + i as u16,
            width: inner.width,
            height: 1,
        };
        frame.render_widget(Paragraph::new(Span::styled(line_str, style)), line_area);
    }

    // Show test status below the account rows (only when no edit dialog is
    // open). Routed through `TextPane` so a long failure message — typically
    // a multi-line 401/connection error from the transport — wraps instead
    // of getting clipped at the right edge.
    if state.editing.is_none() {
        let test_line = match &state.test_status {
            TestStatus::Idle => None,
            TestStatus::Testing => Some("  \u{23f3} Testing...".to_string()),
            TestStatus::Success(msg) => Some(format!("  \u{2713} {msg}")),
            TestStatus::Failed(msg) => Some(format!("  \u{2717} {msg}")),
        };
        if let Some(text) = test_line {
            let y = inner.y
                + 1
                + state
                    .accounts
                    .len()
                    .min(inner.height.saturating_sub(2) as usize) as u16;
            if y < inner.y + inner.height {
                let remaining = (inner.y + inner.height).saturating_sub(y);
                // Reserve the status block bigger than before — long
                // network errors are 5–6 wrapped rows. Scroll handles any
                // overflow without growing further.
                let status_area = Rect {
                    x: inner.x,
                    y,
                    width: inner.width,
                    height: remaining.min(6),
                };
                let style = match &state.test_status {
                    TestStatus::Failed(_) => Style::default().fg(ratatui::style::Color::Red),
                    TestStatus::Success(_) => Style::default().fg(ratatui::style::Color::Green),
                    _ => Style::default(),
                };
                crate::text_pane::TextPane::new(&text)
                    .style(style)
                    .h_overflow(crate::text_pane::HorizontalOverflow::Wrap)
                    .v_overflow(crate::text_pane::VerticalOverflow::Scroll {
                        offset: state.status_scroll,
                    })
                    .render(frame, status_area);
            }
        }
    }
}

fn draw_defaults_section(frame: &mut Frame, state: &SettingsState, theme: &Theme, area: Rect) {
    let focused = state.focus == SettingsFocus::Defaults && state.editing.is_none();
    let saved = state
        .save_flash_until
        .is_some_and(|t| t > std::time::Instant::now());
    let title = if saved {
        " Defaults \u{2713} Saved "
    } else {
        " Defaults "
    };
    let block = Block::default()
        .title(title)
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

    let model_suffix = if !state.discovered_models.is_empty() {
        format!(
            " (\u{2190}/\u{2192} {} models)",
            state.discovered_models.len()
        )
    } else {
        String::new()
    };

    let cursor = "\u{25b8} ";
    let no_cursor = "  ";

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width < 20 || inner.height < 3 {
        return;
    }

    let label_w = 16u16; // "  Max tokens: ▸ " = 16 chars

    // Row 0: Account (selector, not a text field)
    let row0 = Rect::new(inner.x, inner.y, inner.width, 1);
    frame.render_widget(
        Paragraph::new(Line::from(format!(
            "  Account:    {}{} (\u{2190}/\u{2192})",
            if state.defaults_focus == 0 {
                cursor
            } else {
                no_cursor
            },
            acct_name
        ))),
        row0,
    );

    // Row 1: Model (text field)
    let row1 = Rect::new(inner.x, inner.y + 1, inner.width, 1);
    let label1 = format!(
        "  Model:      {}",
        if state.defaults_focus == 1 {
            cursor
        } else {
            no_cursor
        }
    );
    frame.render_widget(Paragraph::new(Span::raw(&label1)), row1);
    let field1 = Rect::new(
        inner.x + label_w,
        inner.y + 1,
        inner.width.saturating_sub(label_w),
        1,
    );
    state.default_model.render_inline(
        frame,
        field1,
        Style::default(),
        focused && state.defaults_focus == 1,
        false,
    );
    // Render model suffix after the field content
    let model_content_w = state
        .default_model
        .content()
        .chars()
        .map(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(0) as u16)
        .sum::<u16>();
    if !model_suffix.is_empty() && model_content_w + (model_suffix.len() as u16) < field1.width {
        let suffix_area = Rect::new(
            field1.x + model_content_w,
            field1.y,
            field1.width.saturating_sub(model_content_w),
            1,
        );
        frame.render_widget(
            Paragraph::new(Span::styled(
                &model_suffix,
                Style::default().add_modifier(Modifier::DIM),
            )),
            suffix_area,
        );
    }

    // Row 2: Max tokens (text field)
    if inner.height > 2 {
        let row2 = Rect::new(inner.x, inner.y + 2, inner.width, 1);
        let label2 = format!(
            "  Max tokens: {}",
            if state.defaults_focus == 2 {
                cursor
            } else {
                no_cursor
            }
        );
        frame.render_widget(Paragraph::new(Span::raw(&label2)), row2);
        let field2 = Rect::new(
            inner.x + label_w,
            inner.y + 2,
            inner.width.saturating_sub(label_w),
            1,
        );
        state.default_max_tokens.render_inline(
            frame,
            field2,
            Style::default(),
            focused && state.defaults_focus == 2,
            false,
        );
    }
}

/// Draw the account add/edit dialog as a centered overlay.
///
/// Text fields use SimpleInput::render_inline() for proper cursor display.
pub(crate) fn draw_account_edit_dialog(
    frame: &mut Frame,
    editing: &crate::settings_state::AccountEditFields,
    test_status: &crate::settings_state::TestStatus,
    status_scroll: u16,
    theme: &Theme,
) {
    use crate::settings_state::{DIALECTS, TestStatus};
    use ratatui::widgets::Clear;

    // 14 rows: 4 fields, 1 spacer, ~6 rows for a wrapped multi-line
    // transport error (header / account+provider / cause / hint), 1 spacer,
    // 1 row help, 2 rows border. Long errors past the visible window are
    // reachable via PageUp/PageDown — TextPane handles scroll + indicators.
    let area = centered_rect(60, 14, frame.area());
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

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width < 20 || inner.height < 6 {
        return;
    }

    let dialect_str = DIALECTS.get(editing.dialect).unwrap_or(&"anthropic");

    let test_display = match test_status {
        TestStatus::Idle => String::new(),
        TestStatus::Testing => "  \u{23f3} Testing...".to_string(),
        TestStatus::Success(msg) => format!("  \u{2713} {msg}"),
        TestStatus::Failed(msg) => format!("  \u{2717} {msg}"),
    };

    let focus = editing.focus;
    let cursor = "\u{25b8} ";
    let no_cursor = "  ";

    // Label column width
    let label_w = 14u16; // "  API Key:  ▸ " = 14 chars

    // Row 0: Name
    let row0 = Rect::new(inner.x, inner.y, inner.width, 1);
    let label0 = format!(
        "  Name:     {}",
        if focus == 0 { cursor } else { no_cursor }
    );
    frame.render_widget(Paragraph::new(Span::raw(&label0)), row0);
    let field0 = Rect::new(
        inner.x + label_w,
        inner.y,
        inner.width.saturating_sub(label_w),
        1,
    );
    editing
        .name
        .render_inline(frame, field0, Style::default(), focus == 0, false);

    // Row 1: API dialect (not a text field)
    let row1 = Rect::new(inner.x, inner.y + 1, inner.width, 1);
    frame.render_widget(
        Paragraph::new(Line::from(format!(
            "  API:      {}{} (\u{2190}/\u{2192} to change)",
            if focus == 1 { cursor } else { no_cursor },
            dialect_str
        ))),
        row1,
    );

    // Row 2: Endpoint
    let row2 = Rect::new(inner.x, inner.y + 2, inner.width, 1);
    let label2 = format!(
        "  Endpoint: {}",
        if focus == 2 { cursor } else { no_cursor }
    );
    frame.render_widget(Paragraph::new(Span::raw(&label2)), row2);
    let field2 = Rect::new(
        inner.x + label_w,
        inner.y + 2,
        inner.width.saturating_sub(label_w),
        1,
    );
    if editing.endpoint.is_empty() && focus != 2 {
        let placeholder = match *dialect_str {
            "anthropic" => "(api.anthropic.com)",
            "openai" => "(api.openai.com)",
            _ => "(default)",
        };
        frame.render_widget(
            Paragraph::new(Span::styled(
                placeholder,
                Style::default().add_modifier(Modifier::DIM),
            )),
            field2,
        );
    } else {
        editing
            .endpoint
            .render_inline(frame, field2, Style::default(), focus == 2, false);
    }

    // Row 3: API Key (masked)
    let row3 = Rect::new(inner.x, inner.y + 3, inner.width, 1);
    let label3 = format!(
        "  API Key:  {}",
        if focus == 3 { cursor } else { no_cursor }
    );
    frame.render_widget(Paragraph::new(Span::raw(&label3)), row3);
    let field3 = Rect::new(
        inner.x + label_w,
        inner.y + 3,
        inner.width.saturating_sub(label_w),
        1,
    );
    if editing.key.is_empty() && focus != 3 {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "(empty)",
                Style::default().add_modifier(Modifier::DIM),
            )),
            field3,
        );
    } else {
        editing
            .key
            .render_inline(frame, field3, Style::default(), focus == 3, true);
    }

    // Status block: starts at row 5 (one spacer below the key field) and
    // grows to fill every remaining row above the bottom-anchored help line.
    // Multi-line error messages from the transport (header / context / cause
    // / hint) want at least 5–6 rows; an arbitrary cap clips them.
    if inner.height > 6 && !test_display.is_empty() {
        // Reserve one row at the bottom for the help line.
        let status_height = inner.height.saturating_sub(5).saturating_sub(1);
        let status_area = Rect::new(inner.x, inner.y + 5, inner.width, status_height);
        let style = match test_status {
            TestStatus::Failed(_) => Style::default().fg(ratatui::style::Color::Red),
            TestStatus::Success(_) => Style::default().fg(ratatui::style::Color::Green),
            _ => Style::default(),
        };
        crate::text_pane::TextPane::new(&test_display)
            .style(style)
            .h_overflow(crate::text_pane::HorizontalOverflow::Wrap)
            .v_overflow(crate::text_pane::VerticalOverflow::Scroll {
                offset: status_scroll,
            })
            .render(frame, status_area);
    }

    // Help line: anchored to the bottom of the dialog so the variable-height
    // status block above never overlaps it.
    if inner.height > 0 {
        let help_row = Rect::new(inner.x, inner.y + inner.height - 1, inner.width, 1);
        frame.render_widget(
            Paragraph::new(Line::from(
                "  Tab next | Shift+Tab prev | Ctrl+t test | PgUp/PgDn scroll msg | Enter save | Esc cancel",
            )),
            help_row,
        );
    }
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
