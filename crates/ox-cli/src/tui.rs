use crate::text_input_view::desired_input_height;
use crate::theme::Theme;
use crate::view_state::ViewState;
use ox_types::{InsertContext, ScreenSnapshot};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};

// ---------------------------------------------------------------------------
// Draw — composed view
// ---------------------------------------------------------------------------

/// Main draw function. Takes a ViewState snapshot instead of &mut App.
///
/// Returns `(content_height, viewport_height)` for scroll_max calculation.
pub(crate) fn draw(
    frame: &mut Frame,
    vs: &ViewState,
    settings: &crate::settings_state::SettingsState,
    theme: &Theme,
    text_input_view: &mut crate::text_input_view::TextInputView,
) -> (Option<usize>, usize) {
    let editor = vs.ui.editor();
    let cur_insert_context = editor.map(|e| e.context);
    let is_command_mode = cur_insert_context == Some(InsertContext::Command);
    let in_insert = editor.is_some() && !is_command_mode;
    let show_filter = matches!(&vs.ui.screen, ScreenSnapshot::Inbox(s) if s.search.active);

    // Build layout constraints
    let mut constraints = vec![Constraint::Length(1)]; // tab bar
    if show_filter {
        constraints.push(Constraint::Length(1)); // filter bar
    }
    constraints.push(Constraint::Min(1)); // content
    let _input_height = if in_insert {
        let h = desired_input_height(
            text_input_view.content(),
            frame.area().width,
            text_input_view.height_override(),
        );
        constraints.push(Constraint::Length(h));
        h
    } else {
        0
    };
    constraints.push(Constraint::Length(1)); // status bar

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(frame.area());

    let mut idx = 0;

    // Tab bar
    crate::tab_bar::draw_tabs(frame, vs, theme, chunks[idx]);
    idx += 1;

    // Filter bar (if active)
    if show_filter {
        crate::inbox_view::draw_filter_bar(frame, vs, theme, chunks[idx]);
        idx += 1;
    }

    // Content area
    let content_area = chunks[idx];
    idx += 1;

    let mut content_height: Option<usize> = None;

    match &vs.ui.screen {
        ScreenSnapshot::Settings(_) => {
            crate::settings_view::draw_settings(frame, settings, theme, content_area);
        }
        ScreenSnapshot::Thread(snap) => {
            // Build a ThreadView from broker-sourced data
            let view = crate::types::ThreadView {
                messages: vs.messages.clone(),
                thinking: vs.turn.thinking,
            };
            content_height = Some(crate::thread_view::draw_thread(
                frame,
                &view,
                snap.scroll as u16,
                theme,
                content_area,
            ));
        }
        ScreenSnapshot::Inbox(_) => {
            crate::inbox_view::draw_inbox(frame, vs, theme, content_area);
        }
        ScreenSnapshot::History(_) => {
            let (ch, _vh) = crate::history_view::draw_history(frame, vs, theme, content_area);
            content_height = Some(ch);
        }
    }

    // Input box (only in insert mode)
    if in_insert {
        let input_area = chunks[idx];
        idx += 1;

        let title = if vs.turn.thinking {
            " streaming... "
        } else if vs.editor_mode == crate::editor::EditorMode::Normal {
            " NORMAL "
        } else {
            ""
        };

        // Hide cursor when a modal overlay is active or in search context
        let show_cursor = vs.approval_pending.is_none()
            && vs.pending_customize.is_none()
            && cur_insert_context != Some(InsertContext::Search);

        if show_cursor {
            text_input_view.render(frame, input_area, theme.input_border, title);
        } else {
            // Render without cursor (use a plain Paragraph like before)
            let input_block = Block::default()
                .borders(Borders::TOP)
                .border_style(theme.input_border)
                .title(title);
            let input_content = vs.ui.editor().map(|e| e.content.as_str()).unwrap_or("");
            let input = Paragraph::new(input_content).block(input_block);
            frame.render_widget(input, input_area);
        }
    }

    // Status bar / command line
    let status_area = chunks[idx];
    let show_command_line = is_command_mode || vs.editor_mode == crate::editor::EditorMode::Command;
    if show_command_line {
        draw_command_line(frame, vs, theme, status_area);
    } else {
        draw_status_bar(frame, vs, settings, theme, status_area);
    }

    // Modal overlays
    if vs.show_shortcuts {
        let mode_str = if vs.ui.editor().is_some() {
            "insert"
        } else {
            "normal"
        };
        let screen_str = match &vs.ui.screen {
            ScreenSnapshot::Inbox(_) => "inbox",
            ScreenSnapshot::Thread(_) => "thread",
            ScreenSnapshot::Settings(_) => "settings",
            ScreenSnapshot::History(_) => "history",
        };
        crate::dialogs::draw_shortcuts_modal(frame, &vs.key_hints, mode_str, screen_str, theme);
    } else if let Some(customize) = vs.pending_customize {
        crate::dialogs::draw_customize_dialog(frame, customize, theme);
    } else if let Some(ref ap) = vs.approval_pending {
        crate::dialogs::draw_approval_dialog(
            frame,
            &ap.tool_name,
            &ap.input_preview,
            vs.approval_selected,
            theme,
        );
    }

    (content_height, content_area.height as usize)
}

// ---------------------------------------------------------------------------
// Command line (vim-style : prompt)
// ---------------------------------------------------------------------------

fn draw_command_line(frame: &mut Frame, vs: &ViewState, _theme: &Theme, area: Rect) {
    let prompt = Span::styled(
        ":",
        ratatui::style::Style::default().add_modifier(ratatui::style::Modifier::BOLD),
    );
    // Editor-command mode uses the command buffer; app-level command mode uses the input
    let input_content_for_cmd = vs.ui.editor().map(|e| e.content.as_str()).unwrap_or("");
    let text = if vs.editor_mode == crate::editor::EditorMode::Command {
        &vs.editor_command_buffer
    } else {
        input_content_for_cmd
    };
    let input = Span::raw(text);
    let line = Line::from(vec![prompt, input]);
    frame.render_widget(Paragraph::new(line), area);

    let cursor_x = area.x + 1 + text.len() as u16;
    if cursor_x < area.x + area.width {
        frame.set_cursor_position((cursor_x, area.y));
    }
}

// ---------------------------------------------------------------------------
// Status bar
// ---------------------------------------------------------------------------

fn draw_status_bar(
    frame: &mut Frame,
    vs: &ViewState,
    settings: &crate::settings_state::SettingsState,
    theme: &Theme,
    area: Rect,
) {
    let editor = vs.ui.editor();
    let has_editor = editor.is_some();
    let mode_badge = if has_editor {
        let label = match editor.map(|e| e.context) {
            Some(InsertContext::Compose) => " COMPOSE ",
            Some(InsertContext::Reply) => " REPLY ",
            Some(InsertContext::Search) => " SEARCH ",
            Some(InsertContext::Command) => " COMMAND ",
            None => " INSERT ",
        };
        Span::styled(label, theme.insert_badge)
    } else {
        Span::styled(" NORMAL ", theme.title_badge)
    };

    let context_info = if matches!(&vs.ui.screen, ScreenSnapshot::Thread(_)) {
        format!(
            " {}in/{}out",
            vs.turn.tokens.input_tokens, vs.turn.tokens.output_tokens
        )
    } else {
        let count = vs.inbox_threads.len();
        format!(" {} thread{}", count, if count == 1 { "" } else { "s" })
    };

    let hints: String = if matches!(&vs.ui.screen, ScreenSnapshot::Settings(_)) {
        settings_hints(settings)
    } else if vs.key_hints.is_empty() {
        String::new()
    } else {
        let mut s = String::new();
        for (key, desc) in &vs.key_hints {
            s.push_str(" | ");
            s.push_str(key);
            s.push(' ');
            s.push_str(desc);
        }
        s
    };

    let status_line = Line::from(vec![
        mode_badge,
        Span::styled(context_info, theme.status),
        Span::styled(hints, theme.status),
    ]);
    frame.render_widget(Paragraph::new(status_line), area);
}

fn settings_hints(settings: &crate::settings_state::SettingsState) -> String {
    use crate::settings_state::SettingsFocus;
    if settings.delete_confirming {
        let name = settings
            .accounts
            .get(settings.selected_account)
            .map(|a| a.name.as_str())
            .unwrap_or("?");
        format!(" Delete \"{name}\"? y to confirm \u{00b7} any key to cancel")
    } else if settings.editing.is_some() {
        String::new()
    } else {
        match settings.focus {
            SettingsFocus::Accounts => {
                " | a add | e edit | d del | t test | * default | Tab \u{2193} | Esc back".into()
            }
            SettingsFocus::Defaults => {
                " | \u{2191}/\u{2193} field | \u{2190}/\u{2192} value | Enter save | Tab \u{2191} | Esc back".into()
            }
        }
    }
}
