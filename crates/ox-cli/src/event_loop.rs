use crate::app::App;
use crate::editor::{
    EditorMode, InputSession, execute_command_input, flush_pending_edits, submit_editor_content,
};
use crate::settings_state::SettingsState;
use crate::shell::Outcome;
use crate::theme::Theme;
use crate::types::{APPROVAL_OPTIONS, CustomizeState};
use crate::view_state::fetch_view_state;
use crossterm::event::{self, Event, KeyCode, KeyModifiers, MouseEventKind};
use ox_path::oxpath;
use ox_types::{InsertContext, Mode, PendingAction, Screen, UiCommand};
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
    let mut input_session = InputSession::new();
    let mut text_input_view = crate::text_input_view::TextInputView::new();
    let mut prev_mode = Mode::Normal;
    let mut settings = if needs_setup {
        // Navigate to settings screen via broker
        client
            .write_typed(&oxpath!("ui"), &UiCommand::GoToSettings)
            .await
            .ok();
        SettingsState::new_wizard()
    } else {
        SettingsState::new()
    };

    loop {
        // Poll pending async test connection
        if let Some(ref mut rx) = settings.pending_test {
            match rx.try_recv() {
                Ok(result) => {
                    match result.test {
                        Ok((dialect, ms)) => {
                            settings.test_status = crate::settings_state::TestStatus::Success(
                                format!("Connected ({dialect}, {ms}ms)"),
                            );
                        }
                        Err(e) => {
                            settings.test_status = crate::settings_state::TestStatus::Failed(e);
                        }
                    }
                    match result.models {
                        Ok(models) => {
                            settings.discovered_models = models;
                            settings.model_picker_idx = None;
                        }
                        Err(_) => {
                            settings.discovered_models.clear();
                        }
                    }
                    settings.pending_test = None;
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    // Still in progress — will check next frame
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    settings.test_status =
                        crate::settings_state::TestStatus::Failed("Test cancelled".into());
                    settings.pending_test = None;
                }
            }
        }

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
                input_session.editor_mode,
                &input_session.command_buffer,
            )
            .await;

            // Detect mode transitions for InputSession sync
            if vs.ui.mode != prev_mode {
                if vs.ui.mode == Mode::Insert {
                    // Entering insert mode — initialize InputSession from broker
                    input_session.init_from(vs.ui.input.content.clone(), vs.ui.input.cursor);
                    input_session.editor_mode = EditorMode::Insert;
                } else if prev_mode == Mode::Insert {
                    // Exiting insert mode — flush any pending edits
                    flush_pending_edits(&mut input_session, client).await;
                }
                prev_mode = vs.ui.mode;
            }

            // Set row_count in UiStore (for inbox navigation bounds)
            // Only write on inbox screen — thread screen has no row selection.
            if vs.ui.screen == Screen::Inbox {
                let row_count = vs.inbox_threads.len();
                let _ = client
                    .write_typed(&oxpath!("ui"), &UiCommand::SetRowCount { count: row_count })
                    .await;
            }

            // Prepare TextInputView from InputSession (optimistic local state)
            text_input_view.set_state(&input_session.content, input_session.cursor);

            // Draw
            terminal.draw(|frame| {
                let (ch, vh) = crate::tui::draw(frame, &vs, &settings, theme, &mut text_input_view);
                content_height = ch;
                viewport_height = vh;
            })?;

            // Update scroll_max and viewport_height in broker (after draw)
            if vs.ui.active_thread.is_some() && viewport_height > 0 {
                let scroll_max = content_height.unwrap_or(0).saturating_sub(viewport_height);
                let _ = client
                    .write_typed(&oxpath!("ui"), &UiCommand::SetScrollMax { max: scroll_max })
                    .await;

                let _ = client
                    .write_typed(
                        &oxpath!("ui"),
                        &UiCommand::SetViewportHeight {
                            height: viewport_height,
                        },
                    )
                    .await;
            }

            // Extract owned copies of data needed after vs is dropped
            pending_action = vs.ui.pending_action;
            screen_owned = vs.ui.screen;
            mode_owned = vs.ui.mode;
            insert_context_owned = vs.ui.insert_context;
            has_active_thread = vs.ui.active_thread.is_some();
            active_thread_id = vs.ui.active_thread.clone();
            selected_thread_id = vs
                .inbox_threads
                .get(vs.ui.selected_row)
                .map(|t| t.id.clone());
            search_active = vs.ui.search.active;
            has_approval_pending = vs.approval_pending.is_some();
        }
        // vs is now dropped — safe to mutate app

        // 2. Handle pending_action
        if let Some(action) = &pending_action {
            match action {
                PendingAction::SendInput => {
                    if insert_context_owned == Some(InsertContext::Command) {
                        flush_pending_edits(&mut input_session, client).await;
                        execute_command_input(&input_session.content, client).await;
                        let _ = client
                            .write_typed(&oxpath!("ui"), &UiCommand::ClearInput)
                            .await;
                        let _ = client
                            .write_typed(&oxpath!("ui"), &UiCommand::ExitInsert)
                            .await;
                        input_session.reset_after_submit();
                    } else {
                        let new_tid = submit_editor_content(&mut input_session, app, client).await;
                        if let Some(tid) = new_tid {
                            let _ = client
                                .write_typed(&oxpath!("ui"), &UiCommand::Open { thread_id: tid })
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
                                &UiCommand::Open {
                                    thread_id: id.clone(),
                                },
                            )
                            .await;
                    }
                }
                PendingAction::ArchiveSelected => {
                    if let Some(id) = &selected_thread_id {
                        let update_path = ox_path::oxpath!("threads", id);
                        app.pool
                            .inbox()
                            .write(&update_path, cmd!("inbox_state" => "done"))
                            .ok();
                    }
                }
            }
            // Clear the pending action
            let _ = client
                .write_typed(&oxpath!("ui"), &UiCommand::ClearPendingAction)
                .await;
        }

        // Populate settings accounts from config when on the settings screen.
        if screen_owned == Screen::Settings && settings.accounts.is_empty() {
            let config = crate::config::resolve_config(
                app.pool.inbox_root(),
                &crate::config::CliOverrides::default(),
            );
            settings.refresh_accounts(&config, &app.pool.inbox_root().join("keys"));
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
                        let mode_str = match mode_owned {
                            Mode::Normal => "normal",
                            Mode::Insert => "insert",
                        };
                        let screen_str = match screen_owned {
                            Screen::Inbox => "inbox",
                            Screen::Thread => "thread",
                            Screen::Settings => "settings",
                        };

                        // Settings screen — all key handling
                        if screen_owned == Screen::Settings && mode_owned == Mode::Normal {
                            let inbox_root = app.pool.inbox_root().to_path_buf();
                            if let Outcome::Handled = crate::settings_shell::handle_key(
                                &mut settings,
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
                                &mut input_session,
                            ) {
                                continue;
                            }
                        }

                        // Try InputStore dispatch
                        let result = client
                            .write(
                                &oxpath!("input", "key"),
                                cmd!("mode" => mode_str, "key" => key_str.clone(), "screen" => screen_str),
                            )
                            .await;

                        if result.is_err() && mode_owned == Mode::Insert {
                            if insert_context_owned == Some(InsertContext::Search) {
                                dispatch_search_edit(client, key.modifiers, key.code).await;
                            } else {
                                let tw = terminal.get_frame().area().width;
                                crate::thread_shell::handle_unbound_insert_key(
                                    &mut input_session,
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
                            MouseEventKind::Down(_) if text_input_view.is_on_border(mouse.row) => {
                                text_input_view.start_border_drag(mouse.row);
                            }
                            MouseEventKind::Drag(_) if text_input_view.is_dragging() => {
                                text_input_view.update_border_drag(mouse.row);
                            }
                            MouseEventKind::Up(_) if text_input_view.is_dragging() => {
                                text_input_view.end_border_drag();
                            }
                            // Click in input area — move cursor
                            MouseEventKind::Down(_) => {
                                if let Some(byte_pos) =
                                    text_input_view.click_to_byte_offset(mouse.column, mouse.row)
                                {
                                    input_session.cursor = byte_pos;
                                }
                            }
                            // Scroll in input area
                            MouseEventKind::ScrollUp
                                if text_input_view.contains(mouse.column, mouse.row) =>
                            {
                                text_input_view.scroll_by(-3);
                            }
                            MouseEventKind::ScrollDown
                                if text_input_view.contains(mouse.column, mouse.row) =>
                            {
                                text_input_view.scroll_by(3);
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
                        if screen_owned == Screen::Settings && settings.editing.is_some() {
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
                                if let Some(ref mut editing) = settings.editing {
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
                        input_session.insert(&text, EditSource::Paste);
                    }
                }
                _ => {}
            }

            // Batch flush pending edits after processing this event
            flush_pending_edits(&mut input_session, client).await;
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
                    .write_typed(&oxpath!("ui"), &UiCommand::ScrollUp)
                    .await;
            } else {
                let _ = client
                    .write_typed(&oxpath!("ui"), &UiCommand::SelectPrev)
                    .await;
            }
        }
        MouseEventKind::ScrollDown => {
            if has_active_thread {
                let _ = client
                    .write_typed(&oxpath!("ui"), &UiCommand::ScrollDown)
                    .await;
            } else {
                let _ = client
                    .write_typed(&oxpath!("ui"), &UiCommand::SelectNext)
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
                .write_typed(&oxpath!("ui"), &UiCommand::SearchSaveChip)
                .await;
        }
        (KeyModifiers::CONTROL, KeyCode::Char('u')) => {
            let _ = client
                .write_typed(&oxpath!("ui"), &UiCommand::SearchClear)
                .await;
        }
        (_, KeyCode::Backspace) => {
            let _ = client
                .write_typed(&oxpath!("ui"), &UiCommand::SearchDeleteChar)
                .await;
        }
        (_, KeyCode::Char(c)) => {
            let _ = client
                .write_typed(&oxpath!("ui"), &UiCommand::SearchInsertChar { char: c })
                .await;
        }
        _ => {}
    }
}
