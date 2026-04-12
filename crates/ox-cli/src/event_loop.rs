use crate::app::App;
use crate::editor::{
    execute_command_input, flush_pending_edits, submit_editor_content,
};
use crate::settings_shell::SettingsShell;
use crate::shell::Outcome;
use crate::theme::Theme;
use crate::thread_shell::ThreadShell;
use crate::types::{APPROVAL_OPTIONS, CustomizeState};
use crate::view_state::fetch_view_state;
use crossterm::event::{self, Event, KeyCode, KeyModifiers, MouseEventKind};
use ox_path::oxpath;
use ox_types::{
    GlobalCommand, InboxCommand, InsertContext, Mode, PendingAction, Screen, ThreadCommand,
    UiCommand, UiSnapshot,
};
use ox_ui::text_input_store::EditSource;
use std::time::Duration;
use structfs_core_store::Writer as StructWriter;

/// Dialog-local state, owned by the event loop (not App, not broker).
pub(crate) struct DialogState {
    pub approval_selected: usize,
    pub pending_customize: Option<CustomizeState>,
    pub show_shortcuts: bool,
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
    needs_setup: bool,
) -> std::io::Result<()> {
    use crate::key_encode::encode_key;

    let mut dialog = DialogState {
        approval_selected: 0,
        pending_customize: None,
        show_shortcuts: false,
    };
    let mut thread = ThreadShell::new();
    let mut settings_shell = if needs_setup {
        // Navigate to settings screen via broker
        client
            .write_typed(&oxpath!("ui"), &UiCommand::Global(GlobalCommand::GoToSettings))
            .await
            .ok();
        SettingsShell::new_wizard()
    } else {
        SettingsShell::new()
    };

    loop {
        // Poll pending async test connection
        settings_shell.poll();

        // 1. Fetch ViewState, draw, extract owned data needed after drop.
        //
        // ViewState borrows from App so we scope it tightly: draw, then
        // extract the owned fields we need for pending-action handling and
        // event dispatch, then drop the borrow.
        let pending_action: Option<PendingAction>;
        let screen_owned: Screen;
        let mode_owned: Mode;
        let insert_context_owned: Option<InsertContext>;
        let has_active_thread: bool;
        let active_thread_id: Option<String>;
        let selected_thread_id: Option<String>;
        let search_active: bool;
        let has_approval_pending: bool;

        let mut content_height: Option<usize> = None;
        let mut viewport_height: usize = 0;
        {
            let vs = fetch_view_state(
                client,
                app,
                &dialog,
                thread.input_session.editor_mode,
                &thread.input_session.command_buffer,
            )
            .await;

            // Extract state from the snapshot variant
            let cur_mode = match &vs.ui {
                UiSnapshot::Thread(snap) => snap.mode,
                _ => Mode::Normal,
            };

            // Detect mode transitions for InputSession sync
            let was_insert = thread.prev_mode == Mode::Insert && cur_mode != Mode::Insert;
            if was_insert {
                // Exiting insert mode — flush any pending edits
                thread.flush(client).await;
            }
            thread.sync_mode(&vs.ui);

            // Set row_count in UiStore (for inbox navigation bounds)
            // Only write on inbox screen — thread screen has no row selection.
            if matches!(&vs.ui, UiSnapshot::Inbox(_)) {
                let row_count = vs.inbox_threads.len();
                let _ = client
                    .write_typed(
                        &oxpath!("ui"),
                        &UiCommand::Inbox(InboxCommand::SetRowCount { count: row_count }),
                    )
                    .await;
            }

            // Prepare TextInputView from InputSession (optimistic local state)
            thread.prepare_view();

            // Draw
            terminal.draw(|frame| {
                let (ch, vh) = crate::tui::draw(frame, &vs, &settings_shell.state, theme, &mut thread.text_input_view);
                content_height = ch;
                viewport_height = vh;
            })?;

            // Update scroll_max and viewport_height in broker (after draw)
            if matches!(&vs.ui, UiSnapshot::Thread(_)) && viewport_height > 0 {
                let scroll_max = content_height.unwrap_or(0).saturating_sub(viewport_height);
                let _ = client
                    .write_typed(
                        &oxpath!("ui"),
                        &UiCommand::Thread(ThreadCommand::SetScrollMax { max: scroll_max }),
                    )
                    .await;

                let _ = client
                    .write_typed(
                        &oxpath!("ui"),
                        &UiCommand::Thread(ThreadCommand::SetViewportHeight {
                            height: viewport_height,
                        }),
                    )
                    .await;
            }

            // Extract owned copies of data needed after vs is dropped
            pending_action = match &vs.ui {
                UiSnapshot::Inbox(s) => s.pending_action,
                UiSnapshot::Thread(s) => s.pending_action,
                UiSnapshot::Settings(s) => s.pending_action,
            };
            screen_owned = match &vs.ui {
                UiSnapshot::Inbox(_) => Screen::Inbox,
                UiSnapshot::Thread(_) => Screen::Thread,
                UiSnapshot::Settings(_) => Screen::Settings,
            };
            mode_owned = cur_mode;
            insert_context_owned = match &vs.ui {
                UiSnapshot::Thread(s) => s.insert_context,
                _ => None,
            };
            has_active_thread = matches!(&vs.ui, UiSnapshot::Thread(_));
            active_thread_id = match &vs.ui {
                UiSnapshot::Thread(s) => Some(s.thread_id.clone()),
                _ => None,
            };
            selected_thread_id = match &vs.ui {
                UiSnapshot::Inbox(s) => vs
                    .inbox_threads
                    .get(s.selected_row)
                    .map(|t| t.id.clone()),
                _ => None,
            };
            search_active = match &vs.ui {
                UiSnapshot::Inbox(s) => s.search.active,
                _ => false,
            };
            has_approval_pending = vs.approval_pending.is_some();
        }
        // vs is now dropped — safe to mutate app

        // 2. Handle pending_action
        if let Some(action) = &pending_action {
            match action {
                PendingAction::SendInput => {
                    if insert_context_owned == Some(InsertContext::Command) {
                        flush_pending_edits(&mut thread.input_session, client).await;
                        execute_command_input(&thread.input_session.content, client).await;
                        let _ = client
                            .write_typed(
                                &oxpath!("ui"),
                                &UiCommand::Thread(ThreadCommand::ClearInput),
                            )
                            .await;
                        let _ = client
                            .write_typed(
                                &oxpath!("ui"),
                                &UiCommand::Thread(ThreadCommand::ExitInsert),
                            )
                            .await;
                        thread.input_session.reset_after_submit();
                    } else {
                        let new_tid = submit_editor_content(&mut thread.input_session, app, client).await;
                        if let Some(tid) = new_tid {
                            let _ = client
                                .write_typed(
                                    &oxpath!("ui"),
                                    &UiCommand::Global(GlobalCommand::Open {
                                        thread_id: tid,
                                    }),
                                )
                                .await;
                        }
                    }
                }
                PendingAction::Quit => return Ok(()),
                PendingAction::OpenSelected => {
                    if let Some(id) = &selected_thread_id {
                        let _ = client
                            .write_typed(
                                &oxpath!("ui"),
                                &UiCommand::Global(GlobalCommand::Open {
                                    thread_id: id.clone(),
                                }),
                            )
                            .await;
                    }
                }
                PendingAction::ArchiveSelected => {
                    if let Some(id) = &selected_thread_id {
                        let update_path = ox_path::oxpath!("threads", id);
                        let mut map = std::collections::BTreeMap::new();
                        map.insert(
                            "inbox_state".to_string(),
                            structfs_core_store::Value::String("done".to_string()),
                        );
                        app.pool
                            .inbox()
                            .write(
                                &update_path,
                                structfs_core_store::Record::parsed(
                                    structfs_core_store::Value::Map(map),
                                ),
                            )
                            .ok();
                    }
                }
            }
            // Clear the pending action
            let _ = client
                .write_typed(
                    &oxpath!("ui"),
                    &UiCommand::Global(GlobalCommand::ClearPendingAction),
                )
                .await;
        }

        // Populate settings accounts from config when on the settings screen.
        if screen_owned == Screen::Settings {
            settings_shell.ensure_accounts(app.pool.inbox_root());
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
                    // Shortcuts modal — dismiss on ? or Esc, swallow all other keys
                    if dialog.show_shortcuts {
                        if let Some(key_str) = encode_key(key.modifiers, key.code) {
                            if key_str == "?" || key_str == "Esc" || key_str == "Ctrl+q" {
                                dialog.show_shortcuts = false;
                            }
                        }
                    }
                    // Customize dialog — bypass broker entirely
                    else if dialog.pending_customize.is_some() {
                        crate::key_handlers::handle_customize_key(
                            &mut dialog,
                            client,
                            &active_thread_id,
                            key.code,
                        )
                        .await;
                    }
                    // Approval dialog — direct handling (reads from broker)
                    else if has_approval_pending && mode_owned == Mode::Normal {
                        crate::key_handlers::handle_approval_key(
                            &mut dialog,
                            client,
                            &active_thread_id,
                            key.code,
                            key.modifiers,
                        )
                        .await;
                    }
                    // Normal + Insert — dispatch through broker
                    else if let Some(key_str) = encode_key(key.modifiers, key.code) {
                        // Settings screen — all key handling
                        if screen_owned == Screen::Settings && mode_owned == Mode::Normal {
                            let inbox_root = app.pool.inbox_root().to_path_buf();
                            if let Outcome::Handled = crate::settings_shell::handle_key(
                                &mut settings_shell.state,
                                &key_str,
                                client,
                                &inbox_root,
                            )
                            .await
                            {
                                continue;
                            }
                        }

                        // Inbox screen — search chip dismissal
                        if mode_owned == Mode::Normal && screen_owned == Screen::Inbox {
                            if let Outcome::Handled =
                                crate::inbox_shell::handle_key(key.code, search_active, client)
                                    .await
                            {
                                continue;
                            }
                        }

                        // ? in normal mode toggles shortcuts modal
                        if mode_owned == Mode::Normal && key_str == "?" {
                            dialog.show_shortcuts = !dialog.show_shortcuts;
                            continue;
                        }

                        // In editor sub-modes (compose/reply), intercept ESC
                        // before the InputStore can fire ui/exit_insert
                        if mode_owned == Mode::Insert {
                            if let Outcome::Handled = crate::thread_shell::handle_esc_intercept(
                                &key_str,
                                insert_context_owned,
                                &mut thread.input_session,
                            ) {
                                continue;
                            }
                        }

                        // Try InputStore dispatch
                        let result = client
                            .write_typed(
                                &oxpath!("input", "key"),
                                &ox_types::InputKeyEvent {
                                    mode: mode_owned,
                                    key: key_str.clone(),
                                    screen: screen_owned,
                                },
                            )
                            .await;

                        if result.is_err() && mode_owned == Mode::Insert {
                            if insert_context_owned == Some(InsertContext::Search) {
                                dispatch_search_edit(client, key.modifiers, key.code).await;
                            } else {
                                let tw = terminal.get_frame().area().width;
                                crate::thread_shell::handle_unbound_insert_key(
                                    &mut thread.input_session,
                                    insert_context_owned,
                                    app,
                                    client,
                                    tw,
                                    key.modifiers,
                                    key.code,
                                )
                                .await;
                            }
                        }
                    }
                }
                Event::Mouse(mouse) => {
                    // Border drag handling
                    if mode_owned == Mode::Insert {
                        match mouse.kind {
                            MouseEventKind::Down(_) if thread.text_input_view.is_on_border(mouse.row) => {
                                thread.text_input_view.start_border_drag(mouse.row);
                            }
                            MouseEventKind::Drag(_) if thread.text_input_view.is_dragging() => {
                                thread.text_input_view.update_border_drag(mouse.row);
                            }
                            MouseEventKind::Up(_) if thread.text_input_view.is_dragging() => {
                                thread.text_input_view.end_border_drag();
                            }
                            // Click in input area — move cursor
                            MouseEventKind::Down(_) => {
                                if let Some(byte_pos) =
                                    thread.text_input_view.click_to_byte_offset(mouse.column, mouse.row)
                                {
                                    thread.input_session.cursor = byte_pos;
                                }
                            }
                            // Scroll in input area
                            MouseEventKind::ScrollUp
                                if thread.text_input_view.contains(mouse.column, mouse.row) =>
                            {
                                thread.text_input_view.scroll_by(-3);
                            }
                            MouseEventKind::ScrollDown
                                if thread.text_input_view.contains(mouse.column, mouse.row) =>
                            {
                                thread.text_input_view.scroll_by(3);
                            }
                            _ => {
                                // Fall through to normal mouse dispatch
                                dispatch_mouse_owned(
                                    client,
                                    has_active_thread,
                                    has_approval_pending,
                                    dialog.pending_customize.is_some(),
                                    mouse.kind,
                                )
                                .await;
                            }
                        }
                    } else
                    // Click on settings edit dialog
                    if let MouseEventKind::Down(_) = mouse.kind {
                        if screen_owned == Screen::Settings && settings_shell.state.editing.is_some() {
                            let term_size = crossterm::terminal::size().unwrap_or((80, 24));
                            let dialog_h = 10u16;
                            let dialog_w = term_size.0 * 60 / 100;
                            let dialog_top = term_size.1.saturating_sub(dialog_h) / 2;
                            let dialog_left = (term_size.0.saturating_sub(dialog_w)) / 2;
                            // Fields start at row offset 1 inside the bordered dialog
                            // Row 0: Name, Row 1: Dialect, Row 2: Endpoint, Row 3: Key
                            let field_first_row = dialog_top + 1;
                            if mouse.row >= field_first_row
                                && mouse.row < field_first_row + 4
                                && mouse.column >= dialog_left
                                && mouse.column < dialog_left + dialog_w
                            {
                                let field = (mouse.row - field_first_row) as usize;
                                if let Some(ref mut editing) = settings_shell.state.editing {
                                    editing.focus = field;
                                }
                            }
                        }
                    }
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
                                dialog.approval_selected = idx;
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
                            dialog.pending_customize.is_some(),
                            mouse.kind,
                        )
                        .await;
                    }
                }
                Event::Paste(text) => {
                    if mode_owned == Mode::Insert
                        && insert_context_owned != Some(InsertContext::Search)
                    {
                        thread.input_session.insert(&text, EditSource::Paste);
                    }
                }
                _ => {}
            }

            // Batch flush pending edits after processing this event
            flush_pending_edits(&mut thread.input_session, client).await;
        }
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
    if has_pending_approval || has_pending_customize {
        return;
    }

    match kind {
        MouseEventKind::ScrollUp => {
            if has_active_thread {
                let _ = client
                    .write_typed(
                        &oxpath!("ui"),
                        &UiCommand::Thread(ThreadCommand::ScrollUp),
                    )
                    .await;
            } else {
                let _ = client
                    .write_typed(
                        &oxpath!("ui"),
                        &UiCommand::Inbox(InboxCommand::SelectPrev),
                    )
                    .await;
            }
        }
        MouseEventKind::ScrollDown => {
            if has_active_thread {
                let _ = client
                    .write_typed(
                        &oxpath!("ui"),
                        &UiCommand::Thread(ThreadCommand::ScrollDown),
                    )
                    .await;
            } else {
                let _ = client
                    .write_typed(
                        &oxpath!("ui"),
                        &UiCommand::Inbox(InboxCommand::SelectNext),
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
    match (modifiers, code) {
        (_, KeyCode::Enter) => {
            let _ = client
                .write_typed(
                    &oxpath!("ui"),
                    &UiCommand::Inbox(InboxCommand::SearchSaveChip),
                )
                .await;
        }
        (KeyModifiers::CONTROL, KeyCode::Char('u')) => {
            let _ = client
                .write_typed(
                    &oxpath!("ui"),
                    &UiCommand::Inbox(InboxCommand::SearchClear),
                )
                .await;
        }
        (_, KeyCode::Backspace) => {
            let _ = client
                .write_typed(
                    &oxpath!("ui"),
                    &UiCommand::Inbox(InboxCommand::SearchDeleteChar),
                )
                .await;
        }
        (_, KeyCode::Char(c)) => {
            let _ = client
                .write_typed(
                    &oxpath!("ui"),
                    &UiCommand::Inbox(InboxCommand::SearchInsertChar { char: c }),
                )
                .await;
        }
        _ => {}
    }
}
