use crate::theme::Theme;
use crate::view_state::ViewState;
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
pub(crate) fn draw(frame: &mut Frame, vs: &ViewState, theme: &Theme) -> (Option<usize>, usize) {
    let in_insert = vs.mode == "insert";
    let show_filter = vs.active_thread.is_none() && vs.search_active;

    // Build layout constraints
    let mut constraints = vec![Constraint::Length(1)]; // tab bar
    if show_filter {
        constraints.push(Constraint::Length(1)); // filter bar
    }
    constraints.push(Constraint::Min(1)); // content
    if in_insert {
        constraints.push(Constraint::Length(3)); // input box
    }
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

    if vs.active_thread.is_some() {
        // Build a ThreadView from broker-sourced data
        let view = crate::app::ThreadView {
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
        let input_block = Block::default()
            .borders(Borders::TOP)
            .border_style(theme.input_border)
            .title(title);
        let input = Paragraph::new(format!("> {}", vs.input)).block(input_block);
        frame.render_widget(input, input_area);

        // Cursor
        if vs.approval_pending.is_none() && vs.pending_customize.is_none() {
            if vs.insert_context.as_deref() != Some("search") {
                frame.set_cursor_position((input_area.x + vs.cursor as u16 + 2, input_area.y + 1));
            }
        }
    }

    // Status bar
    let status_area = chunks[idx];
    draw_status_bar(frame, vs, theme, status_area);

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

fn draw_status_bar(frame: &mut Frame, vs: &ViewState, theme: &Theme, area: Rect) {
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

    let hints = match (
        vs.mode.as_str(),
        vs.insert_context.as_deref(),
        vs.active_thread.is_some(),
    ) {
        ("normal", _, false) => " | i compose | / search | Enter open | d archive | q quit",
        ("normal", _, true) => " | i reply | j/k scroll | q/Esc inbox",
        ("insert", Some("search"), _) => " | Enter chip | Esc cancel",
        ("insert", _, _) => " | ^Enter send | Esc cancel",
        _ => "",
    };

    let status_line = Line::from(vec![
        mode_badge,
        Span::styled(context_info, theme.status),
        Span::styled(hints, theme.status),
    ]);
    frame.render_widget(Paragraph::new(status_line), area);
}
