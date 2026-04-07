use crate::app::{APPROVAL_OPTIONS, App, InputMode, InsertContext};
use crate::theme::Theme;
use crate::view_state::{ViewState, fetch_view_state};
use crossterm::event::{self, Event, KeyCode, KeyModifiers, MouseEventKind};
use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use std::time::Duration;
use structfs_core_store::Writer as StructWriter;

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
    use std::collections::BTreeMap;
    use structfs_core_store::{Record, Value, path};

    loop {
        // 1. Fetch ViewState, draw, extract owned data needed after drop.
        //
        // ViewState borrows from App so we scope it tightly: draw, then
        // extract the owned fields we need for pending-action handling and
        // event dispatch, then drop the borrow.
        let pending_action: Option<String>;
        let input_text: String;
        let screen_owned: String;
        let has_active_thread: bool;
        let active_thread_id: Option<String>;
        let selected_thread_id: Option<String>;
        let search_active: bool;
        let has_approval_pending: bool;
        // For text editing fallback
        let cursor_pos: usize;
        let input_len: usize;

        let mut content_height: Option<usize> = None;
        let mut viewport_height: usize = 0;
        {
            let vs = fetch_view_state(client, app).await;

            // Set row_count in UiStore (for inbox navigation bounds)
            // Only write on inbox screen — thread screen has no row selection.
            if vs.screen == "inbox" {
                let row_count = vs.inbox_threads.len() as i64;
                let mut rc = BTreeMap::new();
                rc.insert("count".to_string(), Value::Integer(row_count));
                let _ = client
                    .write(&path!("ui/set_row_count"), Record::parsed(Value::Map(rc)))
                    .await;
            }

            // Draw
            terminal.draw(|frame| {
                let (ch, vh) = draw(frame, &vs, theme);
                content_height = ch;
                viewport_height = vh;
            })?;

            // Update scroll_max and viewport_height in broker (after draw)
            if vs.active_thread.is_some() && viewport_height > 0 {
                let scroll_max = content_height.unwrap_or(0).saturating_sub(viewport_height) as i64;
                let mut sm = BTreeMap::new();
                sm.insert("max".to_string(), Value::Integer(scroll_max.max(0)));
                let _ = client
                    .write(&path!("ui/set_scroll_max"), Record::parsed(Value::Map(sm)))
                    .await;

                let mut vh = BTreeMap::new();
                vh.insert("height".to_string(), Value::Integer(viewport_height as i64));
                let _ = client
                    .write(
                        &path!("ui/set_viewport_height"),
                        Record::parsed(Value::Map(vh)),
                    )
                    .await;
            }

            // Extract owned copies of data needed after vs is dropped
            pending_action = vs.pending_action.clone();
            input_text = vs.input.clone();
            screen_owned = vs.screen.clone();
            has_active_thread = vs.active_thread.is_some();
            active_thread_id = vs.active_thread.clone();
            selected_thread_id = vs.inbox_threads.get(vs.selected_row).map(|t| t.id.clone());
            search_active = vs.search.is_active();
            cursor_pos = vs.cursor;
            input_len = vs.input.len();
            has_approval_pending = vs.approval_pending.is_some();
        }
        // vs is now dropped — safe to mutate app

        // 2. Handle pending_action
        if let Some(action) = &pending_action {
            match action.as_str() {
                "send_input" => {
                    app.send_input_with_text(input_text.clone());
                    sync_mode_to_broker(client, app).await;
                }
                "quit" => return Ok(()),
                "open_selected" => {
                    if let Some(id) = &selected_thread_id {
                        app.open_thread(id.clone());
                        let mut cmd = BTreeMap::new();
                        cmd.insert("thread_id".to_string(), Value::String(id.clone()));
                        let _ = client
                            .write(&path!("ui/open"), Record::parsed(Value::Map(cmd)))
                            .await;
                    }
                }
                "archive_selected" => {
                    if let Some(id) = &selected_thread_id {
                        let update_path = ox_kernel::Path::from_components(vec![
                            "threads".to_string(),
                            id.clone(),
                        ]);
                        let mut map = BTreeMap::new();
                        map.insert("inbox_state".to_string(), Value::String("done".to_string()));
                        app.pool
                            .inbox()
                            .write(&update_path, Record::parsed(Value::Map(map)))
                            .ok();
                    }
                }
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

        // 5. Poll terminal event
        let terminal_event = tokio::task::block_in_place(|| {
            if event::poll(Duration::from_millis(50)).unwrap_or(false) {
                event::read().ok()
            } else {
                None
            }
        });

        // 4. Handle terminal events
        if let Some(evt) = terminal_event {
            match evt {
                Event::Key(key) => {
                    // Customize dialog — bypass broker entirely
                    if app.pending_customize.is_some() {
                        handle_customize_key(app, client, &active_thread_id, key.code).await;
                    }
                    // Approval dialog — direct handling (reads from broker)
                    else if has_approval_pending && matches!(app.mode, InputMode::Normal) {
                        handle_approval_key(
                            app,
                            client,
                            &active_thread_id,
                            key.code,
                            key.modifiers,
                        )
                        .await;
                    }
                    // Normal + Insert — dispatch through broker
                    else if let Some(key_str) = encode_key(key.modifiers, key.code) {
                        let mode = match &app.mode {
                            InputMode::Normal => "normal",
                            InputMode::Insert(_) => "insert",
                        };
                        let screen = screen_owned.as_str();

                        // Search chip dismissal (1-9 in normal mode, inbox, search active)
                        if mode == "normal" && screen == "inbox" && search_active {
                            if let KeyCode::Char(c @ '1'..='9') = key.code {
                                let idx = (c as u8 - b'1') as usize;
                                app.search.dismiss_chip(idx);
                                continue;
                            }
                        }

                        let mut event_map = BTreeMap::new();
                        event_map.insert("mode".to_string(), Value::String(mode.to_string()));
                        event_map.insert("key".to_string(), Value::String(key_str.clone()));
                        event_map.insert("screen".to_string(), Value::String(screen.to_string()));

                        // Try InputStore dispatch
                        let result = client
                            .write(&path!("input/key"), Record::parsed(Value::Map(event_map)))
                            .await;

                        if result.is_err() {
                            // No binding — route through broker or search fallback
                            if let InputMode::Insert(ref ctx) = app.mode {
                                if *ctx == InsertContext::Search {
                                    handle_search_key(app, key.modifiers, key.code);
                                } else {
                                    match key.code {
                                        KeyCode::Up => app.history_up(),
                                        KeyCode::Down => app.history_down(),
                                        _ => {
                                            dispatch_text_edit_owned(
                                                client,
                                                cursor_pos,
                                                input_len,
                                                key.modifiers,
                                                key.code,
                                            )
                                            .await;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                Event::Mouse(mouse) => {
                    // Click on approval dialog
                    if let MouseEventKind::Down(_) = mouse.kind {
                        if has_approval_pending {
                            let term_h = crossterm::terminal::size().map(|(_, h)| h).unwrap_or(24);
                            let dialog_h = 13u16;
                            let dialog_top = term_h.saturating_sub(dialog_h) / 2;
                            let first_option_row = dialog_top + 3;
                            if mouse.row >= first_option_row
                                && mouse.row < first_option_row + APPROVAL_OPTIONS.len() as u16
                            {
                                let idx = (mouse.row - first_option_row) as usize;
                                app.approval_selected = idx;
                                send_approval_response(
                                    client,
                                    &active_thread_id,
                                    APPROVAL_OPTIONS[idx].1,
                                )
                                .await;
                            }
                        }
                    } else {
                        dispatch_mouse_owned(
                            client,
                            has_active_thread,
                            has_approval_pending,
                            app.pending_customize.is_some(),
                            mouse.kind,
                        )
                        .await;
                    }
                }
                _ => {}
            }
        }
    }
}

/// Write an approval response through the broker for the given thread.
async fn send_approval_response(
    client: &ox_broker::ClientHandle,
    active_thread_id: &Option<String>,
    response: &str,
) {
    use structfs_core_store::{Record, Value};

    if let Some(tid) = active_thread_id {
        let path =
            structfs_core_store::Path::parse(&format!("threads/{tid}/approval/response")).unwrap();
        let _ = client
            .write(&path, Record::parsed(Value::String(response.to_string())))
            .await;
    }
}

/// Sync App's mode back to broker after send_input changes it.
async fn sync_mode_to_broker(client: &ox_broker::ClientHandle, app: &App) {
    use std::collections::BTreeMap;
    use structfs_core_store::{Record, Value, path};

    match &app.mode {
        InputMode::Normal => {
            let _ = client
                .write(
                    &path!("ui/exit_insert"),
                    Record::parsed(Value::Map(BTreeMap::new())),
                )
                .await;
        }
        InputMode::Insert(ctx) => {
            let ctx_str = match ctx {
                InsertContext::Compose => "compose",
                InsertContext::Reply => "reply",
                InsertContext::Search => "search",
            };
            let mut cmd = BTreeMap::new();
            cmd.insert("context".to_string(), Value::String(ctx_str.to_string()));
            let _ = client
                .write(&path!("ui/enter_insert"), Record::parsed(Value::Map(cmd)))
                .await;
        }
    }

    // Sync input + cursor (send_input clears them)
    let mut input_cmd = BTreeMap::new();
    input_cmd.insert("text".to_string(), Value::String(app.input.clone()));
    input_cmd.insert("cursor".to_string(), Value::Integer(app.cursor as i64));
    let _ = client
        .write(
            &path!("ui/set_input"),
            Record::parsed(Value::Map(input_cmd)),
        )
        .await;

    // Sync active_thread if send_input opened a new thread
    if let Some(tid) = &app.active_thread {
        let mut cmd = BTreeMap::new();
        cmd.insert("thread_id".to_string(), Value::String(tid.clone()));
        let _ = client
            .write(&path!("ui/open"), Record::parsed(Value::Map(cmd)))
            .await;
    }
}

/// Dispatch text editing commands through UiStore via the broker.
/// Called when no InputStore binding matches in insert mode.
/// Takes owned cursor/input data extracted from ViewState.
async fn dispatch_text_edit_owned(
    client: &ox_broker::ClientHandle,
    cursor: usize,
    input_len: usize,
    modifiers: KeyModifiers,
    code: KeyCode,
) {
    use std::collections::BTreeMap;
    use structfs_core_store::{Record, Value, path};

    match (modifiers, code) {
        (KeyModifiers::CONTROL, KeyCode::Char('a')) => {
            let mut cmd = BTreeMap::new();
            cmd.insert("cursor".to_string(), Value::Integer(0));
            let _ = client
                .write(&path!("ui/set_input"), Record::parsed(Value::Map(cmd)))
                .await;
        }
        (KeyModifiers::CONTROL, KeyCode::Char('e')) => {
            let mut cmd = BTreeMap::new();
            cmd.insert("cursor".to_string(), Value::Integer(input_len as i64));
            let _ = client
                .write(&path!("ui/set_input"), Record::parsed(Value::Map(cmd)))
                .await;
        }
        (_, KeyCode::Char(c)) => {
            let mut cmd = BTreeMap::new();
            cmd.insert("char".to_string(), Value::String(c.to_string()));
            cmd.insert("at".to_string(), Value::Integer(cursor as i64));
            let _ = client
                .write(&path!("ui/insert_char"), Record::parsed(Value::Map(cmd)))
                .await;
        }
        (_, KeyCode::Enter) => {
            let mut cmd = BTreeMap::new();
            cmd.insert("char".to_string(), Value::String("\n".to_string()));
            cmd.insert("at".to_string(), Value::Integer(cursor as i64));
            let _ = client
                .write(&path!("ui/insert_char"), Record::parsed(Value::Map(cmd)))
                .await;
        }
        (_, KeyCode::Backspace) => {
            let _ = client
                .write(
                    &path!("ui/delete_char"),
                    Record::parsed(Value::Map(BTreeMap::new())),
                )
                .await;
        }
        (_, KeyCode::Left) => {
            let pos = cursor.saturating_sub(1);
            let mut cmd = BTreeMap::new();
            cmd.insert("cursor".to_string(), Value::Integer(pos as i64));
            let _ = client
                .write(&path!("ui/set_input"), Record::parsed(Value::Map(cmd)))
                .await;
        }
        (_, KeyCode::Right) => {
            let pos = (cursor + 1).min(input_len);
            let mut cmd = BTreeMap::new();
            cmd.insert("cursor".to_string(), Value::Integer(pos as i64));
            let _ = client
                .write(&path!("ui/set_input"), Record::parsed(Value::Map(cmd)))
                .await;
        }
        _ => {}
    }
}

/// Dispatch mouse events through UiStore via the broker.
/// Takes owned state extracted from ViewState.
async fn dispatch_mouse_owned(
    client: &ox_broker::ClientHandle,
    has_active_thread: bool,
    has_pending_approval: bool,
    has_pending_customize: bool,
    kind: MouseEventKind,
) {
    use std::collections::BTreeMap;
    use structfs_core_store::{Record, Value, path};

    if has_pending_approval || has_pending_customize {
        return;
    }

    match kind {
        MouseEventKind::ScrollUp => {
            if has_active_thread {
                let _ = client
                    .write(
                        &path!("ui/scroll_up"),
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
            if has_active_thread {
                let _ = client
                    .write(
                        &path!("ui/scroll_down"),
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
// Search text editing (only path that still bypasses broker)
// ---------------------------------------------------------------------------

fn handle_search_key(app: &mut App, modifiers: KeyModifiers, code: KeyCode) {
    match (modifiers, code) {
        (_, KeyCode::Enter) => app.search.save_chip(),
        (KeyModifiers::CONTROL, KeyCode::Char('u')) => app.search.live_query.clear(),
        (_, KeyCode::Backspace) => {
            app.search.live_query.pop();
        }
        (_, KeyCode::Char(c)) => app.search.live_query.push(c),
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Approval key handler (unchanged logic, adapted signature)
// ---------------------------------------------------------------------------

async fn handle_approval_key(
    app: &mut App,
    client: &ox_broker::ClientHandle,
    active_thread_id: &Option<String>,
    key: KeyCode,
    _modifiers: KeyModifiers,
) {
    match key {
        // vim navigation
        KeyCode::Up | KeyCode::Char('k') | KeyCode::Char('K') => {
            app.approval_selected = app.approval_selected.saturating_sub(1);
        }
        KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('J') => {
            if app.approval_selected < APPROVAL_OPTIONS.len() - 1 {
                app.approval_selected += 1;
            }
        }
        // number keys for direct selection
        KeyCode::Char(c @ '1'..='6') => {
            let idx = (c as u8 - b'1') as usize;
            if idx < APPROVAL_OPTIONS.len() {
                send_approval_response(client, active_thread_id, APPROVAL_OPTIONS[idx].1).await;
                app.approval_selected = 0;
            }
        }
        KeyCode::Enter => {
            send_approval_response(
                client,
                active_thread_id,
                APPROVAL_OPTIONS[app.approval_selected].1,
            )
            .await;
            app.approval_selected = 0;
        }
        // customize — enter customize dialog
        KeyCode::Char('c') | KeyCode::Char('C') => {
            // Read tool and input_preview from the pending approval in broker
            if let Some(tid) = active_thread_id {
                let pending_path =
                    structfs_core_store::Path::parse(&format!("threads/{tid}/approval/pending"))
                        .unwrap();
                if let Ok(Some(record)) = client.read(&pending_path).await {
                    if let Some(structfs_core_store::Value::Map(m)) = record.as_value() {
                        let tool = m
                            .get("tool_name")
                            .and_then(|v| match v {
                                structfs_core_store::Value::String(s) => Some(s.clone()),
                                _ => None,
                            })
                            .unwrap_or_default();
                        let input_preview = m
                            .get("input_preview")
                            .and_then(|v| match v {
                                structfs_core_store::Value::String(s) => Some(s.clone()),
                                _ => None,
                            })
                            .unwrap_or_default();
                        let args = infer_args(&tool, &input_preview);
                        app.pending_customize = Some(crate::app::CustomizeState {
                            tool,
                            args,
                            arg_cursor: 0,
                            effect_idx: 0,
                            scope_idx: 0,
                            focus: 0,
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
                }
            }
        }
        // quick keys
        KeyCode::Char('y') | KeyCode::Char('Y') => {
            send_approval_response(client, active_thread_id, "allow_once").await;
            app.approval_selected = 0;
        }
        KeyCode::Char('s') | KeyCode::Char('S') => {
            send_approval_response(client, active_thread_id, "allow_session").await;
            app.approval_selected = 0;
        }
        KeyCode::Char('a') | KeyCode::Char('A') => {
            send_approval_response(client, active_thread_id, "allow_always").await;
            app.approval_selected = 0;
        }
        KeyCode::Char('n') | KeyCode::Char('N') => {
            send_approval_response(client, active_thread_id, "deny_once").await;
            app.approval_selected = 0;
        }
        KeyCode::Char('d') | KeyCode::Char('D') => {
            send_approval_response(client, active_thread_id, "deny_always").await;
            app.approval_selected = 0;
        }
        KeyCode::Esc => {
            send_approval_response(client, active_thread_id, "deny_once").await;
            app.approval_selected = 0;
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Draw — composed view
// ---------------------------------------------------------------------------

/// Main draw function. Takes a ViewState snapshot instead of &mut App.
///
/// Returns `(content_height, viewport_height)` for scroll_max calculation.
fn draw(frame: &mut Frame, vs: &ViewState, theme: &Theme) -> (Option<usize>, usize) {
    let in_insert = matches!(vs.input_mode, InputMode::Insert(_));
    let show_filter = vs.active_thread.is_none() && vs.search.is_active();

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

        let ctx_label = match vs.input_mode {
            InputMode::Insert(InsertContext::Compose) => " compose ",
            InputMode::Insert(InsertContext::Reply) => " reply ",
            InputMode::Insert(InsertContext::Search) => " search ",
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
            match vs.input_mode {
                InputMode::Insert(InsertContext::Search) => {
                    // Search cursor would be in the filter bar, but we keep it simple
                }
                _ => {
                    frame.set_cursor_position((
                        input_area.x + vs.cursor as u16 + 2,
                        input_area.y + 1,
                    ));
                }
            }
        }
    }

    // Status bar
    let status_area = chunks[idx];
    draw_status_bar(frame, vs, theme, status_area);

    // Modal overlays
    if let Some(customize) = vs.pending_customize {
        draw_customize_dialog(frame, customize, theme);
    } else if let Some((ref tool, ref preview)) = vs.approval_pending {
        draw_approval_dialog(frame, tool, preview, vs.approval_selected, theme);
    }

    (content_height, content_area.height as usize)
}

// ---------------------------------------------------------------------------
// Status bar
// ---------------------------------------------------------------------------

fn draw_status_bar(frame: &mut Frame, vs: &ViewState, theme: &Theme, area: Rect) {
    let mode_badge = match vs.input_mode {
        InputMode::Normal => Span::styled(" NORMAL ", theme.title_badge),
        InputMode::Insert(_) => Span::styled(" INSERT ", theme.insert_badge),
    };

    let context_info = if vs.active_thread.is_some() {
        let (ti, to) = vs.turn_tokens;
        format!(" {}in/{}out", ti, to)
    } else {
        let count = vs.inbox_threads.len();
        format!(" {} thread{}", count, if count == 1 { "" } else { "s" })
    };

    let hints = match (vs.input_mode, vs.active_thread.is_some()) {
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

fn draw_approval_dialog(
    frame: &mut Frame,
    tool: &str,
    input_preview: &str,
    selected: usize,
    theme: &Theme,
) {
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
            Span::styled(format!("[{tool}] "), theme.approval_tool),
            Span::styled(input_preview, theme.approval_preview),
        ]),
        Line::from(""),
    ];

    for (i, (label, resp_str)) in APPROVAL_OPTIONS.iter().enumerate() {
        let is_allow = resp_str.starts_with("allow");
        let base_style = if is_allow {
            theme.approval_allow
        } else {
            theme.approval_deny
        };
        let style = if i == selected {
            theme.approval_selected
        } else {
            base_style
        };
        let marker = if i == selected { "> " } else { "  " };
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
#[allow(dead_code)]
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
#[allow(dead_code)]
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

async fn handle_customize_key(
    app: &mut App,
    client: &ox_broker::ClientHandle,
    active_thread_id: &Option<String>,
    key: KeyCode,
) {
    let cust = app.pending_customize.as_mut().unwrap();
    let total = cust.total_fields();
    match key {
        KeyCode::Esc => {
            app.pending_customize.take();
            send_approval_response(client, active_thread_id, "deny_once").await;
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
            // Determine effect and scope, write as string response
            let effect = EFFECTS[cust.effect_idx];
            let scope = SCOPES[cust.scope_idx];
            let response = format!("{effect}_{scope}");
            send_approval_response(client, active_thread_id, &response).await;
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
