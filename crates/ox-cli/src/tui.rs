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

/// Hyperlink to render via OSC 8 after the frame is flushed.
pub(crate) struct PendingHyperlink {
    pub row: u16,
    pub col: u16,
    pub url: String,
    pub text: String,
}

/// Main draw function. Takes a ViewState snapshot instead of &mut App.
///
/// Returns `(content_height, viewport_height, history_hit_map, pending_hyperlink)`.
pub(crate) fn draw(
    frame: &mut Frame,
    vs: &ViewState,
    settings: &crate::settings_state::SettingsState,
    theme: &Theme,
    text_input_view: &mut crate::text_input_view::TextInputView,
    history_explorer: &mut crate::history_state::HistoryExplorer,
) -> (
    Option<usize>,
    usize,
    Option<crate::history_view::HistoryHitMap>,
    Option<PendingHyperlink>,
) {
    let editor = vs.ui.editor();
    let in_insert = editor.is_some();

    // Chip row shows above the content on the inbox whenever chips are
    // set. Chips are a persistent view filter — make them visible.
    let chips_visible =
        matches!(&vs.ui.screen, ScreenSnapshot::Inbox(s) if !s.search.chips.is_empty());

    // Build layout constraints. The status row at the bottom is the
    // single modal surface for history-search, command line, and
    // inbox search prompt — they all render at chunks[last].
    let mut constraints = vec![Constraint::Length(1)]; // tab bar
    if chips_visible {
        constraints.push(Constraint::Length(1)); // chip row
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

    // Chip row (inbox with filters set)
    if chips_visible {
        crate::inbox_view::draw_chip_bar(frame, vs, theme, chunks[idx]);
        idx += 1;
    }

    // Content area
    let content_area = chunks[idx];
    idx += 1;

    let mut content_height: Option<usize> = None;
    let mut history_hit_map: Option<crate::history_view::HistoryHitMap> = None;

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
                vs.ledger_banner,
                vs.approval_pending.as_ref(),
                vs.approval_selected,
                content_area,
            ));
        }
        ScreenSnapshot::Inbox(_) => {
            crate::inbox_view::draw_inbox(frame, vs, theme, content_area);
        }
        ScreenSnapshot::History(snap) => {
            let expanded: std::collections::HashSet<usize> =
                snap.expanded.iter().copied().collect();
            history_hit_map = Some(crate::history_view::draw_history(
                frame,
                history_explorer,
                &snap.thread_id,
                snap.selected_row,
                &expanded,
                &vs.history_pretty,
                &vs.history_full,
                vs.turn.thinking,
                theme,
                vs.ledger_banner,
                content_area,
            ));
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

        // Hide cursor only when a true blocking modal is active. The
        // pending-approval card is inline (not a modal), so the editor
        // cursor stays visible while a permission decision is pending.
        let show_cursor = vs.pending_customize.is_none();

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

    // Bottom row: modal status line. Content is picked from the shared
    // `focus_mode` so renderer and input router never disagree about
    // which surface is foregrounded. Chips are shown in a dedicated row
    // up top (see `draw_chip_bar`), so the status row here only carries
    // transient prompts and the default status bar.
    use ox_types::Mode;
    let status_area = chunks[idx];
    let focus = vs.focus();
    match focus {
        Mode::HistorySearch => {
            if let Some((query, results, selected)) = &vs.history_search {
                draw_history_search(frame, query, results, *selected, theme, status_area);
            }
        }
        Mode::Command => draw_command_line(frame, vs, theme, status_area),
        Mode::Search => crate::inbox_view::draw_search_prompt(frame, vs, theme, status_area),
        _ => draw_status_bar(frame, vs, settings, theme, status_area),
    }

    // Modal overlays
    let mut pending_hyperlink: Option<PendingHyperlink> = None;

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
    } else if vs.show_usage {
        pending_hyperlink = crate::dialogs::draw_usage_dialog(
            frame,
            &vs.model,
            &vs.turn.session_tokens,
            &vs.turn.last_run_tokens,
            &vs.turn.per_model_usage,
            &vs.pricing_overrides,
            theme,
        );
    } else if vs.show_thread_info {
        crate::dialogs::draw_thread_info_modal(
            frame,
            vs.thread_info.as_ref(),
            &vs.model,
            &vs.pricing_overrides,
            vs.approval_pending.as_ref(),
            theme,
        );
    }
    // Pending-approval is no longer a modal — it renders inline as the
    // tail of the conversation in `draw_thread`. See
    // `dialogs::build_approval_card_lines`.

    (
        content_height,
        content_area.height as usize,
        history_hit_map,
        pending_hyperlink,
    )
}

// ---------------------------------------------------------------------------
// Command line (vim-style : prompt)
// ---------------------------------------------------------------------------

fn draw_history_search(
    frame: &mut Frame,
    query: &str,
    results: &[String],
    selected: usize,
    theme: &Theme,
    area: Rect,
) {
    let matched = results.get(selected).map(|s| s.as_str()).unwrap_or("");
    let line = Line::from(vec![
        Span::styled(
            "(reverse-i-search)",
            ratatui::style::Style::default().add_modifier(ratatui::style::Modifier::BOLD),
        ),
        Span::styled(format!("'{query}': "), theme.status),
        Span::raw(matched),
    ]);
    frame.render_widget(Paragraph::new(line), area);

    // Position cursor after the query
    let cursor_x = area.x + 18 + 1 + query.len() as u16 + 3;
    if cursor_x < area.x + area.width {
        frame.set_cursor_position((cursor_x, area.y));
    }
}

fn draw_command_line(frame: &mut Frame, vs: &ViewState, _theme: &Theme, area: Rect) {
    let prompt = Span::styled(
        ":",
        ratatui::style::Style::default().add_modifier(ratatui::style::Modifier::BOLD),
    );
    let text: &str = vs.ui.command_line.content.as_str();
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
            None => " INSERT ",
        };
        Span::styled(label, theme.insert_badge)
    } else {
        Span::styled(" NORMAL ", theme.title_badge)
    };

    let context_info = if matches!(&vs.ui.screen, ScreenSnapshot::Thread(_)) {
        let st = &vs.turn.session_tokens;
        let lr = &vs.turn.last_run_tokens;

        if st.input_tokens == 0 && st.output_tokens == 0 {
            if !vs.messages.is_empty() {
                " (usage not tracked)".into()
            } else {
                String::new()
            }
        } else {
            let session_cost = ox_gate::pricing::estimate_cost_full_with_overrides(
                &vs.model,
                st.input_tokens,
                st.output_tokens,
                st.cache_creation_input_tokens,
                st.cache_read_input_tokens,
                &vs.pricing_overrides,
            );
            let run_cost = ox_gate::pricing::estimate_cost_full_with_overrides(
                &vs.model,
                lr.input_tokens,
                lr.output_tokens,
                lr.cache_creation_input_tokens,
                lr.cache_read_input_tokens,
                &vs.pricing_overrides,
            );

            let mut parts = Vec::new();

            // During streaming: show live "this query" cost delta
            // After turn: show last completed run cost
            if vs.turn.thinking {
                let rs = &vs.turn.run_start_session;
                let delta_in = st.input_tokens.saturating_sub(rs.input_tokens);
                let delta_out = st.output_tokens.saturating_sub(rs.output_tokens);
                let delta_cc = st
                    .cache_creation_input_tokens
                    .saturating_sub(rs.cache_creation_input_tokens);
                let delta_cr = st
                    .cache_read_input_tokens
                    .saturating_sub(rs.cache_read_input_tokens);
                if let Some(c) = ox_gate::pricing::estimate_cost_full_with_overrides(
                    &vs.model,
                    delta_in,
                    delta_out,
                    delta_cc,
                    delta_cr,
                    &vs.pricing_overrides,
                ) {
                    parts.push(format!("${:.4}...", c));
                }
            } else if lr.input_tokens > 0 || lr.output_tokens > 0 {
                if let Some(c) = run_cost {
                    parts.push(format!("${:.4}", c));
                } else {
                    parts.push(format!(
                        "{}in/{}out",
                        format_tokens(lr.input_tokens),
                        format_tokens(lr.output_tokens)
                    ));
                }
            }

            // Session total
            if let Some(c) = session_cost {
                parts.push(format!("${:.4} total", c));
            } else {
                parts.push(format!(
                    "{}in/{}out total",
                    format_tokens(st.input_tokens),
                    format_tokens(st.output_tokens)
                ));
            }

            format!(" {}", parts.join(" \u{00b7} "))
        }
    } else {
        let count = vs.inbox_threads.len();
        format!(" {} thread{}", count, if count == 1 { "" } else { "s" })
    };

    let hints: String = if matches!(&vs.ui.screen, ScreenSnapshot::Settings(_)) {
        settings_hints(settings)
    } else {
        let mut s = String::new();
        for h in &vs.key_hints {
            if h.status_hint {
                s.push_str(" | ");
                s.push_str(&h.key);
                s.push(' ');
                s.push_str(&h.description);
            }
        }
        s.push_str(" | ? help");
        s
    };

    let status_line = Line::from(vec![
        mode_badge,
        Span::styled(context_info, theme.status),
        Span::styled(hints, theme.status),
    ]);
    frame.render_widget(Paragraph::new(status_line), area);
}

/// Format a token count for compact display: 0, 1.2k, 45k, 1.2M.
fn format_tokens(n: u32) -> String {
    if n == 0 {
        "0".into()
    } else if n < 1_000 {
        format!("{n}")
    } else if n < 100_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else if n < 1_000_000 {
        format!("{}k", n / 1_000)
    } else {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    }
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
