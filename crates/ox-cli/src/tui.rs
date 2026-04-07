use crate::app::{App, AppControl, ApprovalResponse, ApprovalState, InputMode, InsertContext};
use crate::theme::Theme;
use crossterm::event::{self, Event, KeyCode, KeyModifiers, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use std::time::Duration;

/// Run the TUI event loop. Blocks until the user quits.
pub fn run(
    app: &mut App,
    theme: &Theme,
    terminal: &mut ratatui::DefaultTerminal,
) -> std::io::Result<()> {
    loop {
        terminal.draw(|frame| draw(frame, app, theme))?;

        if event::poll(Duration::from_millis(50))? {
            match event::read()? {
                Event::Key(key) => {
                    if app.pending_customize.is_some() {
                        handle_customize_key(app, key.code);
                    } else if app.pending_approval.is_some() {
                        handle_approval_key(app, key.code, key.modifiers);
                    } else {
                        match &app.mode {
                            InputMode::Normal => {
                                handle_normal_key(app, key.modifiers, key.code);
                            }
                            InputMode::Insert(ctx) => {
                                handle_insert_key(app, ctx.clone(), key.modifiers, key.code);
                            }
                        }
                    }
                }
                Event::Mouse(mouse) => {
                    handle_mouse(app, mouse.kind, mouse.row);
                }
                _ => {}
            }
        }

        // Drain agent events
        while let Ok(event) = app.event_rx.try_recv() {
            app.handle_event(event);
        }

        // Check for permission requests
        if app.pending_approval.is_none() && app.pending_customize.is_none() {
            if let Ok(AppControl::PermissionRequest {
                thread_id,
                tool,
                input_preview,
                respond,
            }) = app.control_rx.try_recv()
            {
                // Update inbox state to blocked
                app.update_thread_state(&thread_id, "blocked_on_approval");
                // Auto-switch to the requesting thread's tab
                app.open_thread(thread_id.clone());
                app.pending_approval = Some(ApprovalState {
                    thread_id,
                    tool,
                    input_preview,
                    selected: 0,
                    respond,
                });
            }
        }

        if app.should_quit {
            return Ok(());
        }
    }
}

/// Async event loop that dispatches through the BrokerStore.
///
/// ALL state mutations go through UiStore via the broker. Text editing
/// commands (insert_char, delete_char) are dispatched directly to UiStore
/// when no InputStore binding matches. Application-level commands
/// (send, open, archive, quit) are signaled via UiStore's pending_action
/// field and handled by App methods.
pub async fn run_async(
    app: &mut App,
    client: &ox_broker::ClientHandle,
    theme: &Theme,
    terminal: &mut ratatui::DefaultTerminal,
) -> std::io::Result<()> {
    use crate::key_encode::encode_key;
    use crate::state_sync::sync_ui_to_app;
    use std::collections::BTreeMap;
    use structfs_core_store::{path, Record, Value};

    loop {
        // 1. Sync broker state → App fields, check for pending actions
        if let Some(action) = sync_ui_to_app(client, app).await {
            match action.as_str() {
                "send_input" => app.send_input(),
                "quit" => app.should_quit = true,
                "open_selected" => app.open_selected_thread(),
                "archive_selected" => app.archive_selected_thread(),
                _ => {}
            }
            // Clear the pending action
            let _ = client
                .write(
                    &path!("ui/clear_pending_action"),
                    Record::parsed(Value::Map(BTreeMap::new())),
                )
                .await;
        }

        // 2. Sync inbox row count to UiStore
        let row_count = app.cached_threads.len() as i64;
        let mut rc = BTreeMap::new();
        rc.insert("count".to_string(), Value::Integer(row_count));
        let _ = client
            .write(&path!("ui/set_row_count"), Record::parsed(Value::Map(rc)))
            .await;

        // 3. Draw
        terminal.draw(|frame| draw(frame, app, theme))?;

        // 4. Poll terminal event (blocking — bridge via block_in_place)
        let terminal_event = tokio::task::block_in_place(|| {
            if event::poll(Duration::from_millis(50)).unwrap_or(false) {
                event::read().ok()
            } else {
                None
            }
        });

        // 5. Handle event
        if let Some(evt) = terminal_event {
            match evt {
                Event::Key(key) => {
                    // Customize dialog — bypass broker entirely
                    if app.pending_customize.is_some() {
                        handle_customize_key(app, key.code);
                    }
                    // Approval dialog — direct handling
                    else if app.pending_approval.is_some()
                        && matches!(app.mode, InputMode::Normal)
                    {
                        handle_approval_key(app, key.code, key.modifiers);
                    }
                    // Normal + Insert — dispatch through broker
                    else if let Some(key_str) = encode_key(key.modifiers, key.code) {
                        let mode = match &app.mode {
                            InputMode::Normal => "normal",
                            InputMode::Insert(_) => "insert",
                        };
                        let screen = if app.active_thread.is_some() {
                            "thread"
                        } else {
                            "inbox"
                        };

                        let mut event_map = BTreeMap::new();
                        event_map
                            .insert("mode".to_string(), Value::String(mode.to_string()));
                        event_map
                            .insert("key".to_string(), Value::String(key_str.clone()));
                        event_map
                            .insert("screen".to_string(), Value::String(screen.to_string()));

                        // Try InputStore dispatch
                        let result = client
                            .write(
                                &path!("input/key"),
                                Record::parsed(Value::Map(event_map)),
                            )
                            .await;

                        if result.is_err() {
                            // No binding — route text editing through UiStore
                            if let InputMode::Insert(ref ctx) = app.mode {
                                if *ctx == InsertContext::Search {
                                    // Search editing stays direct — search state
                                    // is not in UiStore yet
                                    handle_insert_key(
                                        app,
                                        ctx.clone(),
                                        key.modifiers,
                                        key.code,
                                    );
                                } else {
                                    dispatch_text_edit(
                                        client,
                                        app,
                                        key.modifiers,
                                        key.code,
                                    )
                                    .await;
                                }
                            }
                        }
                    }
                }
                Event::Mouse(mouse) => {
                    dispatch_mouse(client, app, mouse.kind).await;
                }
                _ => {}
            }
        }

        // 6. Drain agent events (unchanged)
        while let Ok(event) = app.event_rx.try_recv() {
            app.handle_event(event);
        }

        // 7. Permission requests (unchanged)
        if app.pending_approval.is_none() && app.pending_customize.is_none() {
            if let Ok(AppControl::PermissionRequest {
                thread_id,
                tool,
                input_preview,
                respond,
            }) = app.control_rx.try_recv()
            {
                app.update_thread_state(&thread_id, "blocked_on_approval");
                app.open_thread(thread_id.clone());
                app.pending_approval = Some(ApprovalState {
                    thread_id,
                    tool,
                    input_preview,
                    selected: 0,
                    respond,
                });
            }
        }

        // 8. Quit
        if app.should_quit {
            return Ok(());
        }
    }
}

/// Dispatch text editing commands through UiStore via the broker.
/// Called when no InputStore binding matches in insert mode.
async fn dispatch_text_edit(
    client: &ox_broker::ClientHandle,
    app: &App,
    modifiers: KeyModifiers,
    code: KeyCode,
) {
    use std::collections::BTreeMap;
    use structfs_core_store::{path, Record, Value};

    let is_search = matches!(app.mode, InputMode::Insert(InsertContext::Search));

    match (modifiers, code) {
        (_, KeyCode::Char(c)) if !is_search => {
            let mut cmd = BTreeMap::new();
            cmd.insert("char".to_string(), Value::String(c.to_string()));
            cmd.insert("at".to_string(), Value::Integer(app.cursor as i64));
            let _ = client
                .write(&path!("ui/insert_char"), Record::parsed(Value::Map(cmd)))
                .await;
        }
        (_, KeyCode::Enter) if !is_search => {
            let mut cmd = BTreeMap::new();
            cmd.insert("char".to_string(), Value::String("\n".to_string()));
            cmd.insert("at".to_string(), Value::Integer(app.cursor as i64));
            let _ = client
                .write(&path!("ui/insert_char"), Record::parsed(Value::Map(cmd)))
                .await;
        }
        (_, KeyCode::Backspace) if !is_search => {
            let _ = client
                .write(
                    &path!("ui/delete_char"),
                    Record::parsed(Value::Map(BTreeMap::new())),
                )
                .await;
        }
        (_, KeyCode::Left) if !is_search => {
            let pos = app.cursor.saturating_sub(1);
            let mut cmd = BTreeMap::new();
            cmd.insert("text".to_string(), Value::String(app.input.clone()));
            cmd.insert("cursor".to_string(), Value::Integer(pos as i64));
            let _ = client
                .write(&path!("ui/set_input"), Record::parsed(Value::Map(cmd)))
                .await;
        }
        (_, KeyCode::Right) if !is_search => {
            let pos = (app.cursor + 1).min(app.input.len());
            let mut cmd = BTreeMap::new();
            cmd.insert("text".to_string(), Value::String(app.input.clone()));
            cmd.insert("cursor".to_string(), Value::Integer(pos as i64));
            let _ = client
                .write(&path!("ui/set_input"), Record::parsed(Value::Map(cmd)))
                .await;
        }
        (KeyModifiers::CONTROL, KeyCode::Char('a')) if !is_search => {
            let mut cmd = BTreeMap::new();
            cmd.insert("text".to_string(), Value::String(app.input.clone()));
            cmd.insert("cursor".to_string(), Value::Integer(0));
            let _ = client
                .write(&path!("ui/set_input"), Record::parsed(Value::Map(cmd)))
                .await;
        }
        (KeyModifiers::CONTROL, KeyCode::Char('e')) if !is_search => {
            let pos = app.input.len() as i64;
            let mut cmd = BTreeMap::new();
            cmd.insert("text".to_string(), Value::String(app.input.clone()));
            cmd.insert("cursor".to_string(), Value::Integer(pos));
            let _ = client
                .write(&path!("ui/set_input"), Record::parsed(Value::Map(cmd)))
                .await;
        }
        // Search text editing and unhandled keys: no-op here.
        // Search uses app.search.live_query directly — handled in the
        // caller via handle_insert_key fallback for search context.
        _ => {}
    }
}

/// Dispatch mouse events through UiStore via the broker.
async fn dispatch_mouse(
    client: &ox_broker::ClientHandle,
    app: &App,
    kind: MouseEventKind,
) {
    use std::collections::BTreeMap;
    use structfs_core_store::{path, Record, Value};

    if app.pending_approval.is_some() || app.pending_customize.is_some() {
        return;
    }

    match kind {
        MouseEventKind::ScrollUp => {
            if app.active_thread.is_some() {
                // Thread: scroll down (increase scroll = see older messages)
                let _ = client
                    .write(
                        &path!("ui/scroll_down"),
                        Record::parsed(Value::Map(BTreeMap::new())),
                    )
                    .await;
            } else {
                let _ = client
                    .write(
                        &path!("ui/select_prev"),
                        Record::parsed(Value::Map(BTreeMap::new())),
                    )
                    .await;
            }
        }
        MouseEventKind::ScrollDown => {
            if app.active_thread.is_some() {
                let _ = client
                    .write(
                        &path!("ui/scroll_up"),
                        Record::parsed(Value::Map(BTreeMap::new())),
                    )
                    .await;
            } else {
                let _ = client
                    .write(
                        &path!("ui/select_next"),
                        Record::parsed(Value::Map(BTreeMap::new())),
                    )
                    .await;
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Normal mode key handler
// ---------------------------------------------------------------------------

fn handle_normal_key(app: &mut App, modifiers: KeyModifiers, code: KeyCode) {
    let in_thread = app.active_thread.is_some();
    let in_inbox = !in_thread;

    match (modifiers, code) {
        // Quit / back
        (KeyModifiers::CONTROL, KeyCode::Char('c')) => {
            if in_thread {
                app.go_to_inbox();
            } else {
                app.should_quit = true;
            }
        }
        (_, KeyCode::Esc) | (_, KeyCode::Char('q')) => {
            if in_thread {
                app.go_to_inbox();
            } else if code == KeyCode::Char('q') {
                app.should_quit = true;
            }
        }

        // Enter insert mode
        (_, KeyCode::Char('i')) => {
            if in_thread {
                app.enter_reply();
            } else {
                app.enter_compose();
            }
        }
        (_, KeyCode::Char('/')) if in_inbox => {
            app.enter_search();
        }

        // Navigation
        (_, KeyCode::Char('j') | KeyCode::Down) if in_inbox => {
            let count = app.cached_threads.len();
            if count > 0 && app.selected_row < count - 1 {
                app.selected_row += 1;
            }
        }
        (_, KeyCode::Char('k') | KeyCode::Up) if in_inbox => {
            app.selected_row = app.selected_row.saturating_sub(1);
        }
        (_, KeyCode::Char('j') | KeyCode::Down) if in_thread => {
            app.scroll = app.scroll.saturating_sub(1);
        }
        (_, KeyCode::Char('k') | KeyCode::Up) if in_thread => {
            app.scroll = app.scroll.saturating_add(1);
        }

        // Enter — open thread or send if draft exists
        (_, KeyCode::Enter) => {
            if !app.input.is_empty() {
                app.send_input();
            } else if in_inbox {
                app.open_selected_thread();
            }
        }

        // Archive (inbox only)
        (_, KeyCode::Char('d')) if in_inbox => {
            app.archive_selected_thread();
        }

        // Dismiss search chips by number (inbox only)
        (_, KeyCode::Char(c @ '1'..='9')) if in_inbox && app.search.is_active() => {
            let idx = (c as u8 - b'1') as usize;
            app.search.dismiss_chip(idx);
        }

        // Back to inbox
        (KeyModifiers::CONTROL, KeyCode::Char('t')) => app.go_to_inbox(),

        // Quick approve shortcuts (thread only, when pending)
        (_, KeyCode::Char('y')) if in_thread && app.pending_approval.is_some() => {
            let approval = app.pending_approval.take().unwrap();
            approval.respond.send(ApprovalResponse::AllowOnce).ok();
        }
        (_, KeyCode::Char('n')) if in_thread && app.pending_approval.is_some() => {
            let approval = app.pending_approval.take().unwrap();
            approval.respond.send(ApprovalResponse::DenyOnce).ok();
        }
        (_, KeyCode::Char('s')) if in_thread && app.pending_approval.is_some() => {
            let approval = app.pending_approval.take().unwrap();
            approval.respond.send(ApprovalResponse::AllowSession).ok();
        }
        (_, KeyCode::Char('a')) if in_thread && app.pending_approval.is_some() => {
            let approval = app.pending_approval.take().unwrap();
            approval.respond.send(ApprovalResponse::AllowAlways).ok();
        }

        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Insert mode key handler
// ---------------------------------------------------------------------------

fn handle_insert_key(app: &mut App, ctx: InsertContext, modifiers: KeyModifiers, code: KeyCode) {
    match (modifiers, code) {
        // Send — Ctrl+S always works. Ctrl+Enter may arrive as different
        // key codes depending on the terminal emulator.
        (KeyModifiers::CONTROL, KeyCode::Char('s'))
        | (KeyModifiers::CONTROL, KeyCode::Enter)
        | (KeyModifiers::CONTROL, KeyCode::Char('\n'))
        | (KeyModifiers::CONTROL, KeyCode::Char('\r')) => {
            app.send_input();
        }
        // Exit insert
        (_, KeyCode::Esc) => {
            app.exit_insert();
        }
        // Enter — newline for compose/reply, save chip for search
        (_, KeyCode::Enter) => match ctx {
            InsertContext::Search => {
                app.search.save_chip();
            }
            InsertContext::Compose | InsertContext::Reply => {
                app.input.insert(app.cursor, '\n');
                app.cursor += 1;
            }
        },
        // Clear line
        (KeyModifiers::CONTROL, KeyCode::Char('u')) => {
            if ctx == InsertContext::Search {
                app.search.live_query.clear();
            } else {
                app.input.clear();
                app.cursor = 0;
            }
        }
        // Text editing
        (_, KeyCode::Backspace) => {
            if ctx == InsertContext::Search {
                app.search.live_query.pop();
            } else if app.cursor > 0 {
                app.cursor -= 1;
                app.input.remove(app.cursor);
            }
        }
        (_, KeyCode::Left) => {
            if ctx != InsertContext::Search {
                app.cursor = app.cursor.saturating_sub(1);
            }
        }
        (_, KeyCode::Right) => {
            if ctx != InsertContext::Search && app.cursor < app.input.len() {
                app.cursor += 1;
            }
        }
        (_, KeyCode::Up) => {
            if ctx != InsertContext::Search {
                app.history_up();
            }
        }
        (_, KeyCode::Down) => {
            if ctx != InsertContext::Search {
                app.history_down();
            }
        }
        (KeyModifiers::CONTROL, KeyCode::Char('a')) => {
            if ctx != InsertContext::Search {
                app.cursor = 0;
            }
        }
        (KeyModifiers::CONTROL, KeyCode::Char('e')) => {
            if ctx != InsertContext::Search {
                app.cursor = app.input.len();
            }
        }
        (_, KeyCode::Char(c)) => {
            if ctx == InsertContext::Search {
                app.search.live_query.push(c);
            } else {
                app.input.insert(app.cursor, c);
                app.cursor += 1;
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Approval key handler (unchanged logic, adapted signature)
// ---------------------------------------------------------------------------

fn handle_approval_key(app: &mut App, key: KeyCode, _modifiers: KeyModifiers) {
    let approval = app.pending_approval.as_mut().unwrap();
    match key {
        // vim navigation
        KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('K') => {
            approval.selected = approval.selected.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('J') => {
            if approval.selected < ApprovalState::OPTIONS.len() - 1 {
                approval.selected += 1;
            }
        }
        // number keys for direct selection
        KeyCode::Char(c @ '1'..='6') => {
            let idx = (c as u8 - b'1') as usize;
            if idx < ApprovalState::OPTIONS.len() {
                let response = ApprovalState::OPTIONS[idx].1.clone();
                let approval = app.pending_approval.take().unwrap();
                approval.respond.send(response).ok();
            }
        }
        KeyCode::Enter => {
            let response = ApprovalState::OPTIONS[approval.selected].1.clone();
            let approval = app.pending_approval.take().unwrap();
            approval.respond.send(response).ok();
        }
        // customize
        KeyCode::Char('c') | KeyCode::Char('C') => {
            let approval = app.pending_approval.take().unwrap();
            let args = infer_args(&approval.tool, &approval.input_preview);
            app.pending_customize = Some(crate::app::CustomizeState {
                tool: approval.tool,
                args,
                arg_cursor: 0,
                effect_idx: 0,
                scope_idx: 0,
                focus: 0,
                respond: approval.respond,
                network_idx: 1, // default: allow
                fs_rules: vec![crate::app::FsRuleState {
                    path: "$PWD".into(),
                    read: true,
                    write: true,
                    create: true,
                    delete: true,
                    execute: true,
                }],
                fs_sub_focus: 0,
                fs_path_cursor: 0,
            });
        }
        // quick keys
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            let approval = app.pending_approval.take().unwrap();
            approval.respond.send(ApprovalResponse::AllowOnce).ok();
        }
        KeyCode::Char('s') | KeyCode::Char('S') => {
            let approval = app.pending_approval.take().unwrap();
            approval.respond.send(ApprovalResponse::AllowSession).ok();
        }
        KeyCode::Char('a') | KeyCode::Char('A') => {
            let approval = app.pending_approval.take().unwrap();
            approval.respond.send(ApprovalResponse::AllowAlways).ok();
        }
        KeyCode::Char('n') | KeyCode::Char('N') => {
            let approval = app.pending_approval.take().unwrap();
            approval.respond.send(ApprovalResponse::DenyOnce).ok();
        }
        KeyCode::Char('d') | KeyCode::Char('D') => {
            let approval = app.pending_approval.take().unwrap();
            approval.respond.send(ApprovalResponse::DenyAlways).ok();
        }
        KeyCode::Esc => {
            let approval = app.pending_approval.take().unwrap();
            approval.respond.send(ApprovalResponse::DenyOnce).ok();
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Mouse handler
// ---------------------------------------------------------------------------

fn handle_mouse(app: &mut App, kind: MouseEventKind, row: u16) {
    match kind {
        MouseEventKind::ScrollUp => {
            if app.pending_approval.is_none() && app.pending_customize.is_none() {
                if app.active_thread.is_some() {
                    app.scroll = app.scroll.saturating_add(3);
                } else {
                    app.selected_row = app.selected_row.saturating_sub(1);
                }
            }
        }
        MouseEventKind::ScrollDown => {
            if app.pending_approval.is_none() && app.pending_customize.is_none() {
                if app.active_thread.is_some() {
                    app.scroll = app.scroll.saturating_sub(3);
                } else {
                    let count = app.cached_threads.len();
                    if count > 0 && app.selected_row < count - 1 {
                        app.selected_row += 1;
                    }
                }
            }
        }
        MouseEventKind::Down(_) => {
            // Click on approval dialog options
            if let Some(ref mut approval) = app.pending_approval {
                let term_h = crossterm::terminal::size().map(|(_, h)| h).unwrap_or(24);
                let dialog_h = 13u16;
                let dialog_top = term_h.saturating_sub(dialog_h) / 2;
                let first_option_row = dialog_top + 3;
                if row >= first_option_row
                    && row < first_option_row + ApprovalState::OPTIONS.len() as u16
                {
                    let idx = (row - first_option_row) as usize;
                    approval.selected = idx;
                    let response = ApprovalState::OPTIONS[idx].1.clone();
                    let approval = app.pending_approval.take().unwrap();
                    approval.respond.send(response).ok();
                }
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Draw — composed view
// ---------------------------------------------------------------------------

fn draw(frame: &mut Frame, app: &mut App, theme: &Theme) {
    let in_insert = matches!(app.mode, InputMode::Insert(_));
    let show_filter = app.active_thread.is_none() && app.search.is_active();

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
    crate::tab_bar::draw_tabs(frame, app, theme, chunks[idx]);
    idx += 1;

    // Filter bar (if active)
    if show_filter {
        crate::inbox_view::draw_filter_bar(frame, app, theme, chunks[idx]);
        idx += 1;
    }

    // Content area
    let content_area = chunks[idx];
    idx += 1;

    // We need to clone the active view data before calling draw_inbox (which borrows app mutably).
    if let Some(tid) = app.active_thread.clone() {
        let view = app.thread_views.entry(tid).or_default().clone();
        crate::thread_view::draw_thread(frame, &view, app.scroll, theme, content_area);
    } else {
        // Refresh cached threads once per frame, adjust scroll
        app.refresh_visible_threads();
        app.ensure_selected_visible(content_area.height as usize);
        crate::inbox_view::draw_inbox(frame, app, theme, content_area);
    }

    // Input box (only in insert mode)
    if in_insert {
        let input_area = chunks[idx];
        idx += 1;

        let ctx_label = match &app.mode {
            InputMode::Insert(InsertContext::Compose) => " compose ",
            InputMode::Insert(InsertContext::Reply) => " reply ",
            InputMode::Insert(InsertContext::Search) => " search ",
            _ => "",
        };
        let thinking = app.active_thinking();
        let title = if thinking {
            " streaming... "
        } else {
            ctx_label
        };
        let input_block = Block::default()
            .borders(Borders::TOP)
            .border_style(theme.input_border)
            .title(title);
        let input = Paragraph::new(format!("> {}", app.input)).block(input_block);
        frame.render_widget(input, input_area);

        // Cursor
        if app.pending_approval.is_none() && app.pending_customize.is_none() {
            match &app.mode {
                InputMode::Insert(InsertContext::Search) => {
                    // Search cursor would be in the filter bar, but we keep it simple
                }
                _ => {
                    frame.set_cursor_position((
                        input_area.x + app.cursor as u16 + 2,
                        input_area.y + 1,
                    ));
                }
            }
        }
    }

    // Status bar
    let status_area = chunks[idx];
    draw_status_bar(frame, app, theme, status_area);

    // Modal overlays
    if let Some(ref customize) = app.pending_customize {
        draw_customize_dialog(frame, customize, theme);
    } else if let Some(ref approval) = app.pending_approval {
        draw_approval_dialog(frame, approval, theme);
    }
}

// ---------------------------------------------------------------------------
// Status bar
// ---------------------------------------------------------------------------

fn draw_status_bar(frame: &mut Frame, app: &App, theme: &Theme, area: Rect) {
    let mode_badge = match &app.mode {
        InputMode::Normal => Span::styled(" NORMAL ", theme.title_badge),
        InputMode::Insert(_) => Span::styled(" INSERT ", theme.insert_badge),
    };

    let context_info = if let Some(ref tid) = app.active_thread {
        let view = app.thread_views.get(tid);
        let (ti, to) = view.map(|v| (v.tokens_in, v.tokens_out)).unwrap_or((0, 0));
        let ps = view.map(|v| &v.policy_stats);
        let mut s = format!(" {}in/{}out", ti, to);
        if let Some(ps) = ps {
            if ps.allowed > 0 || ps.denied > 0 || ps.asked > 0 {
                s.push_str(&format!(
                    " | ok:{} no:{} ask:{}",
                    ps.allowed, ps.denied, ps.asked
                ));
            }
        }
        s
    } else {
        let count = app.cached_threads.len();
        format!(" {} thread{}", count, if count == 1 { "" } else { "s" })
    };

    let hints = match (&app.mode, app.active_thread.is_some()) {
        (InputMode::Normal, false) => " | i compose | / search | Enter open | d archive | q quit",
        (InputMode::Normal, true) => " | i reply | j/k scroll | q/Esc inbox",
        (InputMode::Insert(InsertContext::Search), _) => " | Enter chip | Esc cancel",
        (InputMode::Insert(_), _) => " | ^Enter send | Esc cancel",
    };

    let status_line = Line::from(vec![
        mode_badge,
        Span::styled(context_info, theme.status),
        Span::styled(hints, theme.status),
    ]);
    frame.render_widget(Paragraph::new(status_line), area);
}

// ---------------------------------------------------------------------------
// Approval dialog (unchanged from original)
// ---------------------------------------------------------------------------

fn draw_approval_dialog(frame: &mut Frame, approval: &ApprovalState, theme: &Theme) {
    let area = frame.area();
    let dialog_width = 50.min(area.width.saturating_sub(4));
    let dialog_height = 13;
    let x = (area.width.saturating_sub(dialog_width)) / 2;
    let y = (area.height.saturating_sub(dialog_height)) / 2;
    let dialog_area = Rect::new(x, y, dialog_width, dialog_height);

    frame.render_widget(Clear, dialog_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.approval_border)
        .title(Span::styled(" Permission Required ", theme.approval_title));

    let inner = block.inner(dialog_area);
    frame.render_widget(block, dialog_area);

    let mut lines = vec![
        Line::from(vec![
            Span::styled(format!("[{}] ", approval.tool), theme.approval_tool),
            Span::styled(&approval.input_preview, theme.approval_preview),
        ]),
        Line::from(""),
    ];

    for (i, (label, resp)) in ApprovalState::OPTIONS.iter().enumerate() {
        let is_allow = matches!(
            resp,
            ApprovalResponse::AllowOnce
                | ApprovalResponse::AllowSession
                | ApprovalResponse::AllowAlways
        );
        let base_style = if is_allow {
            theme.approval_allow
        } else {
            theme.approval_deny
        };
        let style = if i == approval.selected {
            theme.approval_selected
        } else {
            base_style
        };
        let marker = if i == approval.selected { "> " } else { "  " };
        let num = i + 1;
        lines.push(Line::from(Span::styled(
            format!("{marker}{num}. {label}"),
            style,
        )));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  (c)ustomize rule | Esc deny once",
        theme.approval_option,
    )));

    let content = Paragraph::new(Text::from(lines));
    frame.render_widget(content, inner);
}

// ---------------------------------------------------------------------------
// Customize dialog (unchanged from original)
// ---------------------------------------------------------------------------

const EFFECTS: [&str; 2] = ["allow", "deny"];
const SCOPES: [&str; 3] = ["once", "session", "always"];
const NETWORKS: [&str; 3] = ["deny", "allow", "localhost"];

/// Decompose a tool call into editable arg strings.
fn infer_args(tool: &str, preview: &str) -> Vec<String> {
    match tool {
        "shell" => preview.split_whitespace().map(|s| s.to_string()).collect(),
        "read_file" | "write_file" | "edit_file" => vec![preview.to_string()],
        _ => vec![],
    }
}

/// Build a clash Node from the customize state.
fn build_node_from_customize(cust: &crate::app::CustomizeState) -> clash::policy::match_tree::Node {
    use clash::policy::match_tree::*;

    let sandbox_ref = if EFFECTS[cust.effect_idx] == "allow"
        && (cust.network_idx != 1 || !cust.fs_rules.is_empty())
    {
        Some(SandboxRef(format!("ox-{}", cust.tool)))
    } else {
        None
    };
    let decision = if EFFECTS[cust.effect_idx] == "allow" {
        Decision::Allow(sandbox_ref)
    } else {
        Decision::Deny
    };
    let leaf = Node::Decision(decision);

    if cust.tool == "shell" {
        // Build ToolName -> arg0 -> arg1 -> ... -> Decision
        let mut current = leaf;
        for (i, arg) in cust.args.iter().enumerate().rev() {
            let pattern = if arg == "*" {
                Pattern::Wildcard
            } else {
                Pattern::Literal(Value::Literal(arg.clone()))
            };
            current = Node::Condition {
                observe: Observable::PositionalArg(i as i32),
                pattern,
                children: vec![current],
                doc: None,
                source: None,
                terminal: false,
            };
        }
        Node::Condition {
            observe: Observable::ToolName,
            pattern: Pattern::Literal(Value::Literal(cust.tool.clone())),
            children: vec![current],
            doc: None,
            source: Some("ox-cli".into()),
            terminal: false,
        }
    } else if let Some(path) = cust.args.first() {
        // File tool: ToolName -> NamedArg("path") -> Decision
        Node::Condition {
            observe: Observable::ToolName,
            pattern: Pattern::Literal(Value::Literal(cust.tool.clone())),
            children: vec![Node::Condition {
                observe: Observable::NamedArg("path".into()),
                pattern: Pattern::Literal(Value::Literal(path.clone())),
                children: vec![leaf],
                doc: None,
                source: None,
                terminal: false,
            }],
            doc: None,
            source: Some("ox-cli".into()),
            terminal: false,
        }
    } else {
        Node::Condition {
            observe: Observable::ToolName,
            pattern: Pattern::Literal(Value::Literal(cust.tool.clone())),
            children: vec![leaf],
            doc: None,
            source: Some("ox-cli".into()),
            terminal: false,
        }
    }
}

/// Build a sandbox from the customize state. Returns None if no restrictions.
fn build_sandbox_from_customize(
    cust: &crate::app::CustomizeState,
) -> Option<(String, clash::policy::sandbox_types::SandboxPolicy)> {
    use clash::policy::sandbox_types::*;

    let network = match cust.network_idx {
        0 => NetworkPolicy::Deny,
        2 => NetworkPolicy::Localhost,
        _ => NetworkPolicy::Allow,
    };

    let rules: Vec<SandboxRule> = cust
        .fs_rules
        .iter()
        .map(|r| {
            let mut caps = Cap::empty();
            if r.read {
                caps |= Cap::READ;
            }
            if r.write {
                caps |= Cap::WRITE;
            }
            if r.create {
                caps |= Cap::CREATE;
            }
            if r.delete {
                caps |= Cap::DELETE;
            }
            if r.execute {
                caps |= Cap::EXECUTE;
            }
            SandboxRule {
                effect: RuleEffect::Allow,
                caps,
                path: r.path.clone(),
                path_match: PathMatch::Subpath,
                follow_worktrees: false,
                doc: None,
            }
        })
        .collect();

    // Skip sandbox if it's fully permissive (all allow, no fs restrictions)
    if matches!(network, NetworkPolicy::Allow) && rules.is_empty() {
        return None;
    }

    let name = format!("ox-{}", cust.tool);
    Some((
        name,
        SandboxPolicy {
            default: Cap::READ | Cap::EXECUTE,
            rules,
            network,
            doc: Some(format!("sandbox for {}", cust.tool)),
        },
    ))
}

fn handle_customize_key(app: &mut App, key: KeyCode) {
    let cust = app.pending_customize.as_mut().unwrap();
    let total = cust.total_fields();
    match key {
        KeyCode::Esc => {
            let cust = app.pending_customize.take().unwrap();
            cust.respond.send(ApprovalResponse::DenyOnce).ok();
        }
        KeyCode::Tab | KeyCode::Down => {
            cust.focus = if cust.focus >= total - 1 {
                0
            } else {
                cust.focus + 1
            };
            cust.arg_cursor = 0;
        }
        KeyCode::BackTab | KeyCode::Up => {
            cust.focus = if cust.focus == 0 {
                total - 1
            } else {
                cust.focus - 1
            };
            cust.arg_cursor = 0;
        }
        KeyCode::Enter => {
            let cust = app.pending_customize.take().unwrap();
            let node = build_node_from_customize(&cust);
            let sandbox = build_sandbox_from_customize(&cust);
            let response = ApprovalResponse::CustomNode {
                node: Box::new(node),
                sandbox,
                scope: SCOPES[cust.scope_idx].to_string(),
            };
            cust.respond.send(response).ok();
        }
        _ => {
            let num_args = cust.args.len();
            let add_f = cust.add_arg_field();
            let effect_f = cust.effect_field();
            let scope_f = cust.scope_field();

            if cust.focus < num_args {
                // Editing an arg pattern
                let pat = &mut cust.args[cust.focus];
                match key {
                    KeyCode::Char(c) => {
                        pat.insert(cust.arg_cursor, c);
                        cust.arg_cursor += 1;
                    }
                    KeyCode::Backspace if cust.arg_cursor > 0 => {
                        cust.arg_cursor -= 1;
                        pat.remove(cust.arg_cursor);
                    }
                    KeyCode::Left => cust.arg_cursor = cust.arg_cursor.saturating_sub(1),
                    KeyCode::Right if cust.arg_cursor < pat.len() => cust.arg_cursor += 1,
                    _ => {}
                }
            } else if cust.focus == add_f && cust.tool == "shell" {
                if matches!(key, KeyCode::Char(' ')) {
                    cust.args.push("*".into());
                    cust.focus = cust.args.len() - 1;
                    cust.arg_cursor = 1;
                }
            } else if cust.focus == effect_f {
                if matches!(
                    key,
                    KeyCode::Left
                        | KeyCode::Right
                        | KeyCode::Char('h')
                        | KeyCode::Char('l')
                        | KeyCode::Char(' ')
                ) {
                    cust.effect_idx = 1 - cust.effect_idx;
                }
            } else if cust.focus == scope_f {
                match key {
                    KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') => {
                        cust.scope_idx = (cust.scope_idx + 1) % SCOPES.len();
                    }
                    KeyCode::Left | KeyCode::Char('h') => {
                        cust.scope_idx = if cust.scope_idx == 0 {
                            SCOPES.len() - 1
                        } else {
                            cust.scope_idx - 1
                        };
                    }
                    _ => {}
                }
            } else if cust.focus == cust.network_field() {
                if matches!(
                    key,
                    KeyCode::Left
                        | KeyCode::Right
                        | KeyCode::Char('h')
                        | KeyCode::Char('l')
                        | KeyCode::Char(' ')
                ) {
                    cust.network_idx = (cust.network_idx + 1) % NETWORKS.len();
                }
            } else if cust.focus >= cust.fs_start()
                && cust.focus < cust.fs_start() + cust.fs_rules.len()
            {
                let idx = cust.focus - cust.fs_start();
                match cust.fs_sub_focus {
                    0 => match key {
                        KeyCode::Char(' ') => cust.fs_sub_focus = 1,
                        KeyCode::Char(c) => {
                            cust.fs_rules[idx].path.insert(cust.fs_path_cursor, c);
                            cust.fs_path_cursor += 1;
                        }
                        KeyCode::Backspace if cust.fs_path_cursor > 0 => {
                            cust.fs_path_cursor -= 1;
                            cust.fs_rules[idx].path.remove(cust.fs_path_cursor);
                        }
                        KeyCode::Left => {
                            cust.fs_path_cursor = cust.fs_path_cursor.saturating_sub(1)
                        }
                        KeyCode::Right if cust.fs_path_cursor < cust.fs_rules[idx].path.len() => {
                            cust.fs_path_cursor += 1;
                        }
                        _ => {}
                    },
                    1..=5 => match key {
                        KeyCode::Char(' ') => match cust.fs_sub_focus {
                            1 => cust.fs_rules[idx].read = !cust.fs_rules[idx].read,
                            2 => cust.fs_rules[idx].write = !cust.fs_rules[idx].write,
                            3 => cust.fs_rules[idx].create = !cust.fs_rules[idx].create,
                            4 => cust.fs_rules[idx].delete = !cust.fs_rules[idx].delete,
                            5 => cust.fs_rules[idx].execute = !cust.fs_rules[idx].execute,
                            _ => {}
                        },
                        KeyCode::Left | KeyCode::Char('h') => {
                            cust.fs_sub_focus = cust.fs_sub_focus.saturating_sub(1);
                        }
                        KeyCode::Right | KeyCode::Char('l') => {
                            cust.fs_sub_focus = (cust.fs_sub_focus + 1).min(5);
                        }
                        KeyCode::Char('x') => {
                            cust.fs_rules.remove(idx);
                            cust.fs_sub_focus = 0;
                        }
                        _ => {}
                    },
                    _ => {}
                }
            } else if cust.focus == cust.add_fs_field() && matches!(key, KeyCode::Char(' ')) {
                cust.fs_rules.push(crate::app::FsRuleState {
                    path: String::new(),
                    read: true,
                    write: false,
                    create: false,
                    delete: false,
                    execute: false,
                });
                cust.focus = cust.fs_start() + cust.fs_rules.len() - 1;
                cust.fs_sub_focus = 0;
                cust.fs_path_cursor = 0;
            }
        }
    }
}

fn draw_customize_dialog(frame: &mut Frame, cust: &crate::app::CustomizeState, theme: &Theme) {
    let area = frame.area();
    let dialog_width = 58.min(area.width.saturating_sub(4));
    let dialog_height = (10 + cust.args.len() as u16 + cust.fs_rules.len() as u16)
        .min(area.height.saturating_sub(4));
    let x = (area.width.saturating_sub(dialog_width)) / 2;
    let y = (area.height.saturating_sub(dialog_height)) / 2;
    let dialog_area = Rect::new(x, y, dialog_width, dialog_height);

    frame.render_widget(Clear, dialog_area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(theme.approval_border)
        .title(Span::styled(" Customize Rule ", theme.approval_title));
    let inner = block.inner(dialog_area);
    frame.render_widget(block, dialog_area);

    let sel = theme.approval_selected;
    let dim = theme.approval_option;
    let effect_color = if EFFECTS[cust.effect_idx] == "allow" {
        theme.approval_allow
    } else {
        theme.approval_deny
    };
    let net_color = if cust.network_idx == 1 {
        theme.approval_allow
    } else {
        theme.approval_deny
    };

    let mut lines = vec![Line::from(vec![
        Span::styled("  Tool:  ", dim),
        Span::styled(&cust.tool, theme.approval_tool),
    ])];

    // Arg fields
    let arg_label = if cust.tool == "shell" { "arg" } else { "path" };
    for (i, arg) in cust.args.iter().enumerate() {
        let focused = cust.focus == i;
        let label = if cust.tool == "shell" {
            format!("  {arg_label} {i}: ")
        } else {
            format!("  {arg_label}:   ")
        };
        lines.push(Line::from(vec![
            Span::styled(label, if focused { sel } else { dim }),
            Span::styled(format!("[{arg}]"), if focused { sel } else { dim }),
        ]));
    }
    if cust.tool == "shell" {
        let add_focused = cust.focus == cust.add_arg_field();
        lines.push(Line::from(Span::styled(
            "  + add argument (Space)",
            if add_focused { sel } else { dim },
        )));
    }

    let ef = cust.effect_field();
    let sf = cust.scope_field();
    lines.push(Line::from(vec![
        Span::styled("  Effect:  ", if cust.focus == ef { sel } else { dim }),
        Span::styled(
            format!("< {} >", EFFECTS[cust.effect_idx]),
            if cust.focus == ef { sel } else { effect_color },
        ),
    ]));
    lines.push(Line::from(vec![
        Span::styled("  Scope:   ", if cust.focus == sf { sel } else { dim }),
        Span::styled(
            format!("< {} >", SCOPES[cust.scope_idx]),
            if cust.focus == sf { sel } else { dim },
        ),
    ]));

    // Sandbox section
    let nf = cust.network_field();
    lines.push(Line::from(Span::styled("  -- Sandbox --", dim)));
    lines.push(Line::from(vec![
        Span::styled("  Network: ", if cust.focus == nf { sel } else { dim }),
        Span::styled(
            format!("< {} >", NETWORKS[cust.network_idx]),
            if cust.focus == nf { sel } else { net_color },
        ),
    ]));

    let fs_start = cust.fs_start();
    for (i, rule) in cust.fs_rules.iter().enumerate() {
        let is_focused = cust.focus == fs_start + i;
        let path_style = if is_focused && cust.fs_sub_focus == 0 {
            sel
        } else {
            dim
        };
        let mut spans = vec![
            Span::styled("    ", dim),
            Span::styled(format!("{:<14}", rule.path), path_style),
            Span::styled(" ", dim),
        ];
        for (label, enabled, sub_idx) in [
            ("r", rule.read, 1),
            ("w", rule.write, 2),
            ("c", rule.create, 3),
            ("d", rule.delete, 4),
            ("x", rule.execute, 5),
        ] {
            let pf = is_focused && cust.fs_sub_focus == sub_idx;
            let st = if pf {
                sel
            } else if enabled {
                theme.approval_allow
            } else {
                theme.approval_deny
            };
            spans.push(Span::styled(
                if enabled {
                    label.to_uppercase()
                } else {
                    "-".into()
                },
                st,
            ));
        }
        if is_focused && cust.fs_sub_focus > 0 {
            spans.push(Span::styled(" (x)rm", dim));
        }
        lines.push(Line::from(spans));
    }
    let add_fs_focused = cust.focus == cust.add_fs_field();
    lines.push(Line::from(Span::styled(
        "    + add path (Space)",
        if add_fs_focused { sel } else { dim },
    )));

    lines.push(Line::from(Span::styled(
        "  Up/Down | Space toggle | Enter save | Esc cancel",
        dim,
    )));

    let content = Paragraph::new(Text::from(lines));
    frame.render_widget(content, inner);
}
