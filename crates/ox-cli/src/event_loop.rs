use crate::app::App;
use crate::types::APPROVAL_OPTIONS;
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
    use structfs_core_store::path;

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
                let _ = client
                    .write(&path!("ui/set_row_count"), cmd!("count" => row_count))
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
                let _ = client
                    .write(
                        &path!("ui/set_scroll_max"),
                        cmd!("max" => scroll_max.max(0)),
                    )
                    .await;

                let _ = client
                    .write(
                        &path!("ui/set_viewport_height"),
                        cmd!("height" => viewport_height as i64),
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
                        .write(&path!("ui/clear_input"), cmd!())
                        .await;
                    let _ = client
                        .write(&path!("ui/exit_insert"), cmd!())
                        .await;
                    // If compose created a new thread, open it in UiStore
                    if let Some(tid) = new_tid {
                        let _ = client
                            .write(&path!("ui/open"), cmd!("thread_id" => tid))
                            .await;
                    }
                }
                "quit" => return Ok(()),
                "open_selected" => {
                    if let Some(id) = &selected_thread_id {
                        let _ = client
                            .write(&path!("ui/open"), cmd!("thread_id" => id))
                            .await;
                    }
                }
                "archive_selected" => {
                    if let Some(id) = &selected_thread_id {
                        let update_path = ox_kernel::Path::from_components(vec![
                            "threads".to_string(),
                            id.clone(),
                        ]);
                        app.pool
                            .inbox()
                            .write(&update_path, cmd!("inbox_state" => "done"))
                            .ok();
                    }
                }
                _ => {}
            }
            // Clear the pending action
            let _ = client
                .write(&path!("ui/clear_pending_action"), cmd!())
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
                                let _ = client
                                    .write(
                                        &path!("ui/search_dismiss_chip"),
                                        cmd!("index" => idx as i64),
                                    )
                                    .await;
                                continue;
                            }
                        }

                        // Try InputStore dispatch
                        let result = client
                            .write(
                                &path!("input/key"),
                                cmd!("mode" => mode, "key" => key_str.clone(), "screen" => screen),
                            )
                            .await;

                        if result.is_err() && mode_owned == "insert" {
                            if insert_context_owned.as_deref() == Some("search") {
                                dispatch_search_edit(client, key.modifiers, key.code).await;
                            } else {
                                match key.code {
                                    KeyCode::Up => {
                                        if let Some((text, cursor)) = app.history_up(&input_text) {
                                            let _ = client
                                                .write(
                                                    &path!("ui/set_input"),
                                                    cmd!("text" => text, "cursor" => cursor as i64),
                                                )
                                                .await;
                                        }
                                    }
                                    KeyCode::Down => {
                                        if let Some((text, cursor)) = app.history_down() {
                                            let _ = client
                                                .write(
                                                    &path!("ui/set_input"),
                                                    cmd!("text" => text, "cursor" => cursor as i64),
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
    use structfs_core_store::path;

    match (modifiers, code) {
        (KeyModifiers::CONTROL, KeyCode::Char('a')) => {
            let _ = client
                .write(&path!("ui/set_input"), cmd!("cursor" => 0_i64))
                .await;
        }
        (KeyModifiers::CONTROL, KeyCode::Char('e')) => {
            let _ = client
                .write(
                    &path!("ui/set_input"),
                    cmd!("cursor" => input_len as i64),
                )
                .await;
        }
        (_, KeyCode::Char(c)) => {
            let _ = client
                .write(
                    &path!("ui/insert_char"),
                    cmd!("char" => c.to_string(), "at" => cursor as i64),
                )
                .await;
        }
        (_, KeyCode::Enter) => {
            let _ = client
                .write(
                    &path!("ui/insert_char"),
                    cmd!("char" => "\n", "at" => cursor as i64),
                )
                .await;
        }
        (_, KeyCode::Backspace) => {
            let _ = client
                .write(&path!("ui/delete_char"), cmd!())
                .await;
        }
        (_, KeyCode::Left) => {
            let pos = cursor.saturating_sub(1);
            let _ = client
                .write(&path!("ui/set_input"), cmd!("cursor" => pos as i64))
                .await;
        }
        (_, KeyCode::Right) => {
            let pos = (cursor + 1).min(input_len);
            let _ = client
                .write(&path!("ui/set_input"), cmd!("cursor" => pos as i64))
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
    use structfs_core_store::path;

    if has_pending_approval || has_pending_customize {
        return;
    }

    match kind {
        MouseEventKind::ScrollUp => {
            if has_active_thread {
                let _ = client
                    .write(&path!("ui/scroll_up"), cmd!())
                    .await;
            } else {
                let _ = client
                    .write(&path!("ui/select_prev"), cmd!())
                    .await;
            }
        }
        MouseEventKind::ScrollDown => {
            if has_active_thread {
                let _ = client
                    .write(&path!("ui/scroll_down"), cmd!())
                    .await;
            } else {
                let _ = client
                    .write(&path!("ui/select_next"), cmd!())
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
    use structfs_core_store::path;

    match (modifiers, code) {
        (_, KeyCode::Enter) => {
            let _ = client
                .write(&path!("ui/search_save_chip"), cmd!())
                .await;
        }
        (KeyModifiers::CONTROL, KeyCode::Char('u')) => {
            let _ = client
                .write(&path!("ui/search_clear"), cmd!())
                .await;
        }
        (_, KeyCode::Backspace) => {
            let _ = client
                .write(&path!("ui/search_delete_char"), cmd!())
                .await;
        }
        (_, KeyCode::Char(c)) => {
            let _ = client
                .write(
                    &path!("ui/search_insert_char"),
                    cmd!("char" => c.to_string()),
                )
                .await;
        }
        _ => {}
    }
}
