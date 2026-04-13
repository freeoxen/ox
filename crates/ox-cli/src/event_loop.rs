use crate::app::App;
use crate::editor::flush_pending_edits;
use crate::settings_shell::SettingsShell;
use crate::shell::Outcome;
use crate::theme::Theme;
use crate::thread_shell::{ThreadShell, dispatch_global_mouse};
use crate::types::{APPROVAL_OPTIONS, CustomizeState};
use crate::view_state::fetch_view_state;
use crossterm::event::{self, Event, KeyCode, KeyModifiers, MouseEventKind};
use ox_path::oxpath;
use ox_types::{
    GlobalCommand, InboxCommand, InputKeyEvent, InsertContext, Mode, PendingAction, Screen,
    ThreadCommand, UiCommand, UiSnapshot,
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
            .write_typed(
                &oxpath!("ui"),
                &UiCommand::Global(GlobalCommand::GoToSettings),
            )
            .await
            .ok();
        SettingsShell::new_wizard()
    } else {
        SettingsShell::new()
    };

    loop {
        // Poll pending async test connection
        settings_shell.poll();

        // -----------------------------------------------------------------
        // 1. Fetch, draw, extract what we need — scope ViewState tightly
        // -----------------------------------------------------------------
        let ui: UiSnapshot;
        let selected_thread_id: Option<String>;
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

            // Mode sync (detects insert→normal transitions, flushes edits)
            let was_insert = thread.prev_mode == Mode::Insert
                && !matches!(&vs.ui, UiSnapshot::Thread(s) if s.mode == Mode::Insert);
            if was_insert {
                thread.flush(client).await;
            }
            thread.sync_mode(&vs.ui);

            // Set row_count in UiStore (inbox navigation bounds)
            if matches!(&vs.ui, UiSnapshot::Inbox(_)) {
                let row_count = vs.inbox_threads.len();
                let _ = client
                    .write_typed(
                        &oxpath!("ui"),
                        &UiCommand::Inbox(InboxCommand::SetRowCount { count: row_count }),
                    )
                    .await;
            }

            // Prepare TextInputView from InputSession
            thread.prepare_view();

            // Populate settings accounts when on settings screen
            if matches!(&vs.ui, UiSnapshot::Settings(_)) {
                settings_shell.ensure_accounts(app.pool.inbox_root());
            }

            // Draw
            terminal.draw(|frame| {
                let (ch, vh) = crate::tui::draw(
                    frame,
                    &vs,
                    &settings_shell.state,
                    theme,
                    &mut thread.text_input_view,
                );
                content_height = ch;
                viewport_height = vh;
            })?;

            // Extract the few values needed after dropping vs
            selected_thread_id = match &vs.ui {
                UiSnapshot::Inbox(s) => vs.inbox_threads.get(s.selected_row).map(|t| t.id.clone()),
                _ => None,
            };
            has_approval_pending = vs.approval_pending.is_some();
            ui = vs.ui.clone();
        }
        // vs is now dropped — safe to mutate app

        // -----------------------------------------------------------------
        // 2. Post-draw: scroll feedback (thread only)
        // -----------------------------------------------------------------
        if let UiSnapshot::Thread(_) = &ui {
            if viewport_height > 0 {
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
        }

        // -----------------------------------------------------------------
        // 3. Handle pending action
        // -----------------------------------------------------------------
        if let Some(action) = ui.pending_action() {
            let _ = client
                .write_typed(
                    &oxpath!("ui"),
                    &UiCommand::Global(GlobalCommand::ClearPendingAction),
                )
                .await;
            match action {
                PendingAction::Quit => return Ok(()),
                PendingAction::SendInput => {
                    if let UiSnapshot::Thread(snap) = &ui {
                        thread.handle_send_input(snap, app, client).await;
                    }
                }
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
        }

        // -----------------------------------------------------------------
        // 4. Poll terminal event
        // -----------------------------------------------------------------
        let terminal_event = tokio::task::block_in_place(|| {
            if event::poll(Duration::from_millis(50)).unwrap_or(false) {
                event::read().ok()
            } else {
                None
            }
        });

        // -----------------------------------------------------------------
        // 5. Dispatch terminal event
        // -----------------------------------------------------------------
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
                        let active_thread_id = match &ui {
                            UiSnapshot::Thread(s) => Some(s.thread_id.clone()),
                            _ => None,
                        };
                        crate::key_handlers::handle_customize_key(
                            &mut dialog,
                            client,
                            &active_thread_id,
                            key.code,
                        )
                        .await;
                    }
                    // Approval dialog (thread screen, normal mode)
                    else if let UiSnapshot::Thread(snap) = &ui {
                        if has_approval_pending && snap.mode == Mode::Normal {
                            let active_thread_id = Some(snap.thread_id.clone());
                            crate::key_handlers::handle_approval_key(
                                &mut dialog,
                                client,
                                &active_thread_id,
                                key.code,
                                key.modifiers,
                            )
                            .await;
                        } else if let Some(key_str) = encode_key(key.modifiers, key.code) {
                            dispatch_key(
                                &ui,
                                &key_str,
                                key.modifiers,
                                key.code,
                                &mut dialog,
                                &mut thread,
                                &mut settings_shell,
                                app,
                                client,
                                terminal,
                            )
                            .await;
                        }
                    } else if let Some(key_str) = encode_key(key.modifiers, key.code) {
                        dispatch_key(
                            &ui,
                            &key_str,
                            key.modifiers,
                            key.code,
                            &mut dialog,
                            &mut thread,
                            &mut settings_shell,
                            app,
                            client,
                            terminal,
                        )
                        .await;
                    }
                }
                Event::Mouse(mouse) => {
                    match &ui {
                        // Thread + insert mode: text input mouse handling
                        UiSnapshot::Thread(snap) if snap.mode == Mode::Insert => {
                            thread
                                .handle_mouse(
                                    mouse,
                                    has_approval_pending,
                                    dialog.pending_customize.is_some(),
                                    client,
                                )
                                .await;
                        }
                        // Settings: edit dialog click-to-focus
                        UiSnapshot::Settings(_)
                            if matches!(mouse.kind, MouseEventKind::Down(_))
                                && settings_shell.state.editing.is_some() =>
                        {
                            settings_shell.handle_mouse(mouse);
                        }
                        // Approval dialog click
                        UiSnapshot::Thread(snap)
                            if has_approval_pending
                                && matches!(mouse.kind, MouseEventKind::Down(_)) =>
                        {
                            let term_h = crossterm::terminal::size().map(|(_, h)| h).unwrap_or(24);
                            let dialog_h = 13u16;
                            let dialog_top = term_h.saturating_sub(dialog_h) / 2;
                            let first_option_row = dialog_top + 3;
                            if mouse.row >= first_option_row
                                && mouse.row < first_option_row + APPROVAL_OPTIONS.len() as u16
                            {
                                let idx = (mouse.row - first_option_row) as usize;
                                dialog.approval_selected = idx;
                                let active_thread_id = Some(snap.thread_id.clone());
                                crate::key_handlers::send_approval_response(
                                    client,
                                    &active_thread_id,
                                    APPROVAL_OPTIONS[idx].1,
                                )
                                .await;
                            }
                        }
                        // Global fallback (scroll)
                        _ => {
                            let has_active_thread = matches!(&ui, UiSnapshot::Thread(_));
                            dispatch_global_mouse(
                                client,
                                has_active_thread,
                                has_approval_pending,
                                dialog.pending_customize.is_some(),
                                mouse.kind,
                            )
                            .await;
                        }
                    }
                }
                Event::Paste(text) => {
                    if let UiSnapshot::Thread(snap) = &ui {
                        if snap.mode == Mode::Insert
                            && snap.insert_context != Some(InsertContext::Search)
                        {
                            thread.input_session.insert(&text, EditSource::Paste);
                        }
                    }
                }
                _ => {}
            }

            // Batch flush pending edits after processing this event
            flush_pending_edits(&mut thread.input_session, client).await;
        }
    }
}

// ---------------------------------------------------------------------------
// Key dispatch — factored out to avoid duplication
// ---------------------------------------------------------------------------

/// Dispatch a key event through screen-specific handlers, then InputStore.
///
/// Called for all key events that aren't consumed by modal overlays (shortcuts,
/// customize dialog, approval dialog).
async fn dispatch_key(
    ui: &UiSnapshot,
    key_str: &str,
    modifiers: KeyModifiers,
    code: KeyCode,
    dialog: &mut DialogState,
    thread: &mut ThreadShell,
    settings_shell: &mut SettingsShell,
    app: &mut App,
    client: &ox_broker::ClientHandle,
    terminal: &mut ratatui::DefaultTerminal,
) {
    // Screen-specific handlers first
    match ui {
        UiSnapshot::Settings(_) => {
            let inbox_root = app.pool.inbox_root().to_path_buf();
            if let Outcome::Handled = crate::settings_shell::handle_key(
                &mut settings_shell.state,
                key_str,
                client,
                &inbox_root,
            )
            .await
            {
                return;
            }
        }
        UiSnapshot::Inbox(snap) => {
            if let Outcome::Handled =
                crate::inbox_shell::handle_key(code, snap.search.active, client).await
            {
                return;
            }
        }
        UiSnapshot::Thread(snap) => {
            // ESC intercept in insert mode
            if snap.mode == Mode::Insert {
                if let Outcome::Handled = crate::thread_shell::handle_esc_intercept(
                    key_str,
                    snap.insert_context,
                    &mut thread.input_session,
                ) {
                    return;
                }
            }
        }
    }

    // ? in normal mode toggles shortcuts modal (inbox + thread only)
    let mode = match ui {
        UiSnapshot::Thread(s) => s.mode,
        _ => Mode::Normal,
    };
    if mode == Mode::Normal && key_str == "?" && !matches!(ui, UiSnapshot::Settings(_)) {
        dialog.show_shortcuts = !dialog.show_shortcuts;
        return;
    }

    // InputStore dispatch
    let (input_mode, input_screen) = match ui {
        UiSnapshot::Inbox(_) => (Mode::Normal, Screen::Inbox),
        UiSnapshot::Thread(s) => (s.mode, Screen::Thread),
        UiSnapshot::Settings(_) => (Mode::Normal, Screen::Settings),
    };
    let result = client
        .write_typed(
            &oxpath!("input", "key"),
            &InputKeyEvent {
                mode: input_mode,
                key: key_str.to_string(),
                screen: input_screen,
            },
        )
        .await;

    // Unbound insert key fallback
    if result.is_err() {
        if let UiSnapshot::Thread(snap) = ui {
            if snap.mode == Mode::Insert {
                if snap.insert_context == Some(InsertContext::Search) {
                    dispatch_search_edit(client, modifiers, code).await;
                } else {
                    let tw = terminal.get_frame().area().width;
                    crate::thread_shell::handle_unbound_insert_key(
                        &mut thread.input_session,
                        snap.insert_context,
                        app,
                        client,
                        tw,
                        modifiers,
                        code,
                    )
                    .await;
                }
            }
        }
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
                .write_typed(&oxpath!("ui"), &UiCommand::Inbox(InboxCommand::SearchClear))
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
