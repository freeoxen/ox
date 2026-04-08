use crate::app::{APPROVAL_OPTIONS, App};
use crate::theme::Theme;
use crate::view_state::fetch_view_state;
use crossterm::event::{self, Event, KeyCode, KeyModifiers, MouseEventKind};
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
        let mode_owned: String;
        let insert_context_owned: Option<String>;
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
                let (ch, vh) = crate::tui::draw(frame, &vs, theme);
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
            mode_owned = vs.mode.clone();
            insert_context_owned = vs.insert_context.clone();
            has_active_thread = vs.active_thread.is_some();
            active_thread_id = vs.active_thread.clone();
            selected_thread_id = vs.inbox_threads.get(vs.selected_row).map(|t| t.id.clone());
            search_active = vs.search_active;
            cursor_pos = vs.cursor;
            input_len = vs.input.len();
            has_approval_pending = vs.approval_pending.is_some();
        }
        // vs is now dropped — safe to mutate app

        // 2. Handle pending_action
        if let Some(action) = &pending_action {
            match action.as_str() {
                "send_input" => {
                    let new_tid = app.send_input_with_text(
                        input_text.clone(),
                        &mode_owned,
                        insert_context_owned.as_deref(),
                        active_thread_id.as_deref(),
                    );
                    // Clear input and exit insert mode through broker
                    let _ = client
                        .write(
                            &path!("ui/clear_input"),
                            Record::parsed(Value::Map(BTreeMap::new())),
                        )
                        .await;
                    let _ = client
                        .write(
                            &path!("ui/exit_insert"),
                            Record::parsed(Value::Map(BTreeMap::new())),
                        )
                        .await;
                    // If compose created a new thread, open it in UiStore
                    if let Some(tid) = new_tid {
                        let mut cmd = BTreeMap::new();
                        cmd.insert("thread_id".to_string(), Value::String(tid));
                        let _ = client
                            .write(&path!("ui/open"), Record::parsed(Value::Map(cmd)))
                            .await;
                    }
                }
                "quit" => return Ok(()),
                "open_selected" => {
                    if let Some(id) = &selected_thread_id {
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
                        crate::key_handlers::handle_customize_key(
                            app,
                            client,
                            &active_thread_id,
                            key.code,
                        )
                        .await;
                    }
                    // Approval dialog — direct handling (reads from broker)
                    else if has_approval_pending && mode_owned == "normal" {
                        crate::key_handlers::handle_approval_key(
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
                        let mode = mode_owned.as_str();
                        let screen = screen_owned.as_str();

                        // Search chip dismissal (1-9 in normal mode, inbox, search active)
                        if mode == "normal" && screen == "inbox" && search_active {
                            if let KeyCode::Char(c @ '1'..='9') = key.code {
                                let idx = (c as u8 - b'1') as usize;
                                let mut cmd = BTreeMap::new();
                                cmd.insert("index".to_string(), Value::Integer(idx as i64));
                                let _ = client
                                    .write(
                                        &path!("ui/search_dismiss_chip"),
                                        Record::parsed(Value::Map(cmd)),
                                    )
                                    .await;
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
                            if mode_owned == "insert" {
                                if insert_context_owned.as_deref() == Some("search") {
                                    dispatch_search_edit(client, key.modifiers, key.code).await;
                                } else {
                                    match key.code {
                                        KeyCode::Up => {
                                            if let Some((text, cursor)) =
                                                app.history_up(&input_text)
                                            {
                                                let mut cmd = BTreeMap::new();
                                                cmd.insert("text".to_string(), Value::String(text));
                                                cmd.insert(
                                                    "cursor".to_string(),
                                                    Value::Integer(cursor as i64),
                                                );
                                                let _ = client
                                                    .write(
                                                        &path!("ui/set_input"),
                                                        Record::parsed(Value::Map(cmd)),
                                                    )
                                                    .await;
                                            }
                                        }
                                        KeyCode::Down => {
                                            if let Some((text, cursor)) = app.history_down() {
                                                let mut cmd = BTreeMap::new();
                                                cmd.insert("text".to_string(), Value::String(text));
                                                cmd.insert(
                                                    "cursor".to_string(),
                                                    Value::Integer(cursor as i64),
                                                );
                                                let _ = client
                                                    .write(
                                                        &path!("ui/set_input"),
                                                        Record::parsed(Value::Map(cmd)),
                                                    )
                                                    .await;
                                            }
                                        }
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
                                crate::key_handlers::send_approval_response(
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
// Search text editing — dispatched through UiStore via broker
// ---------------------------------------------------------------------------

/// Dispatch search text editing through UiStore via the broker.
async fn dispatch_search_edit(
    client: &ox_broker::ClientHandle,
    modifiers: KeyModifiers,
    code: KeyCode,
) {
    use std::collections::BTreeMap;
    use structfs_core_store::{Record, Value, path};

    match (modifiers, code) {
        (_, KeyCode::Enter) => {
            let _ = client
                .write(
                    &path!("ui/search_save_chip"),
                    Record::parsed(Value::Map(BTreeMap::new())),
                )
                .await;
        }
        (KeyModifiers::CONTROL, KeyCode::Char('u')) => {
            let _ = client
                .write(
                    &path!("ui/search_clear"),
                    Record::parsed(Value::Map(BTreeMap::new())),
                )
                .await;
        }
        (_, KeyCode::Backspace) => {
            let _ = client
                .write(
                    &path!("ui/search_delete_char"),
                    Record::parsed(Value::Map(BTreeMap::new())),
                )
                .await;
        }
        (_, KeyCode::Char(c)) => {
            let mut cmd = BTreeMap::new();
            cmd.insert("char".to_string(), Value::String(c.to_string()));
            let _ = client
                .write(
                    &path!("ui/search_insert_char"),
                    Record::parsed(Value::Map(cmd)),
                )
                .await;
        }
        _ => {}
    }
}
