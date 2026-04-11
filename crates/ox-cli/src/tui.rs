use crate::theme::Theme;
use crate::view_state::ViewState;
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use crate::text_input_view::desired_input_height;

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
    let in_insert = vs.mode == "insert";
    let show_filter = vs.active_thread.is_none() && vs.search_active;

    // Build layout constraints
    let mut constraints = vec![Constraint::Length(1)]; // tab bar
    if show_filter {
        constraints.push(Constraint::Length(1)); // filter bar
    }
    constraints.push(Constraint::Min(1)); // content
    let _input_height = if in_insert {
        let h = desired_input_height(text_input_view.content(), frame.area().width);
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

    if vs.screen == "settings" {
        crate::settings_view::draw_settings(frame, settings, theme, content_area);
    } else if vs.active_thread.is_some() {
        // Build a ThreadView from broker-sourced data
        let view = crate::types::ThreadView {
            messages: vs.messages.clone(),
            thinking: vs.thinking,
        };
        content_height = Some(crate::thread_view::draw_thread(
            frame,
            &view,
            vs.scroll,
            theme,
            content_area,
        ));
    } else {
        crate::inbox_view::draw_inbox(frame, vs, theme, content_area);
    }

    // Input box (only in insert mode)
    if in_insert {
        let input_area = chunks[idx];
        idx += 1;

        let ctx_label = match vs.insert_context.as_deref() {
            Some("compose") => " compose ",
            Some("reply") => " reply ",
            Some("search") => " search ",
            _ => "",
        };
        let title = if vs.thinking {
            " streaming... "
        } else {
            ctx_label
        };

        // Hide cursor when a modal overlay is active or in search context
        let show_cursor = vs.approval_pending.is_none()
            && vs.pending_customize.is_none()
            && vs.insert_context.as_deref() != Some("search");

        if show_cursor {
            text_input_view.render(frame, input_area, theme.input_border, title);
        } else {
            // Render without cursor (use a plain Paragraph like before)
            let input_block = Block::default()
                .borders(Borders::TOP)
                .border_style(theme.input_border)
                .title(title);
            let input = Paragraph::new(vs.input.as_str()).block(input_block);
            frame.render_widget(input, input_area);
        }
    }

    // Status bar
    let status_area = chunks[idx];
    draw_status_bar(frame, vs, settings, theme, status_area);

    // Modal overlays
    if let Some(customize) = vs.pending_customize {
        crate::dialogs::draw_customize_dialog(frame, customize, theme);
    } else if let Some((ref tool, ref preview)) = vs.approval_pending {
        crate::dialogs::draw_approval_dialog(frame, tool, preview, vs.approval_selected, theme);
    }

    (content_height, content_area.height as usize)
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
    let mode_badge = if vs.mode == "insert" {
        Span::styled(" INSERT ", theme.insert_badge)
    } else {
        Span::styled(" NORMAL ", theme.title_badge)
    };

    let context_info = if vs.active_thread.is_some() {
        let (ti, to) = vs.turn_tokens;
        format!(" {}in/{}out", ti, to)
    } else {
        let count = vs.inbox_threads.len();
        format!(" {} thread{}", count, if count == 1 { "" } else { "s" })
    };

    let hints: String = if vs.screen == "settings" {
        settings_hints(settings)
    } else {
        match (
            vs.mode.as_str(),
            vs.insert_context.as_deref(),
            vs.active_thread.is_some(),
        ) {
            ("normal", _, false) => {
                " | i compose | / search | s settings | Enter open | d archive | q quit".into()
            }
            ("normal", _, true) => " | i reply | j/k scroll | q/Esc inbox".into(),
            ("insert", Some("search"), _) => " | Enter chip | Esc cancel".into(),
            ("insert", _, _) => " | ^Enter send | Esc cancel".into(),
            _ => String::new(),
        }
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
