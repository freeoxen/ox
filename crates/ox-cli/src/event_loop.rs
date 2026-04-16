use crate::app::App;
use crate::editor::flush_pending_edits;
use crate::settings_shell::SettingsShell;
use crate::settings_state::SettingsFocus;
use crate::shell::Outcome;
use crate::theme::Theme;
use crate::thread_shell::{ThreadShell, dispatch_global_mouse};
use crate::types::{APPROVAL_OPTIONS, CustomizeState};
use crate::view_state::fetch_view_state;
use crossterm::event::{self, Event, KeyCode, KeyModifiers, MouseEventKind};
use ox_kernel::PathComponent;
use ox_path::oxpath;
use ox_types::{
    GlobalCommand, HistoryCommand, InboxCommand, InputKeyEvent, InsertContext, Mode, PendingAction,
    Screen, ScreenSnapshot, ThreadCommand, UiCommand, UiSnapshot,
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
    let mut history_explorer = crate::history_state::HistoryExplorer::new();
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
        let mut history_hit_map: Option<crate::history_view::HistoryHitMap> = None;
        {
            let vs = fetch_view_state(
                client,
                app,
                &dialog,
                thread.input_session.editor_mode,
                &thread.input_session.command_buffer,
            )
            .await;

            // Editor sync (detects editor appeared/disappeared, flushes edits)
            let had_editor = thread.had_editor;
            let has_editor = vs.ui.editor().is_some();
            if had_editor && !has_editor {
                thread.flush(client).await;
            }
            thread.sync_editor(&vs.ui);

            // Set row_count in UiStore (navigation bounds)
            if matches!(&vs.ui.screen, ScreenSnapshot::Inbox(_)) {
                let row_count = vs.inbox_threads.len();
                let _ = client
                    .write_typed(
                        &oxpath!("ui"),
                        &UiCommand::Inbox(InboxCommand::SetRowCount { count: row_count }),
                    )
                    .await;
            }
            // Sync history explorer (incremental parse + layout)
            if let ScreenSnapshot::History(snap) = &vs.ui.screen {
                // Approximate viewport: terminal height minus tab bar (1) and status bar (1)
                let approx_viewport = terminal.get_frame().area().height.saturating_sub(2) as usize;
                history_explorer.sync(
                    &snap.thread_id,
                    &vs.raw_messages,
                    snap.selected_row,
                    &snap.expanded,
                    approx_viewport,
                );
                // UiStore still needs row_count for SelectNext/SelectLast bounds
                let row_count = history_explorer.entry_count();
                let _ = client
                    .write_typed(
                        &oxpath!("ui"),
                        &UiCommand::History(HistoryCommand::SetRowCount { count: row_count }),
                    )
                    .await;
            }

            // Prepare TextInputView from InputSession
            thread.prepare_view();

            // Populate settings accounts when on settings screen
            if matches!(&vs.ui.screen, ScreenSnapshot::Settings(_)) {
                settings_shell.ensure_accounts(app.pool.inbox_root());
            }

            // Draw
            terminal.draw(|frame| {
                let (ch, vh, hm) = crate::tui::draw(
                    frame,
                    &vs,
                    &settings_shell.state,
                    theme,
                    &mut thread.text_input_view,
                    &mut history_explorer,
                );
                content_height = ch;
                viewport_height = vh;
                history_hit_map = hm;
            })?;

            // Extract the few values needed after dropping vs
            selected_thread_id = match &vs.ui.screen {
                ScreenSnapshot::Inbox(s) => {
                    vs.inbox_threads.get(s.selected_row).map(|t| t.id.clone())
                }
                _ => None,
            };
            has_approval_pending = vs.approval_pending.is_some();
            ui = vs.ui.clone();
        }
        // vs is now dropped — safe to mutate app

        // -----------------------------------------------------------------
        // 2. Post-draw: scroll feedback (thread only)
        // -----------------------------------------------------------------
        if matches!(&ui.screen, ScreenSnapshot::Thread(_)) && viewport_height > 0 {
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

        // History scroll is handled by HistoryExplorer's LayoutManager — no feedback needed.

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
                    thread.handle_send_input(&ui, app, client).await;
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
                        let id_comp = match PathComponent::try_new(id.as_str()) {
                            Ok(c) => c,
                            Err(e) => {
                                tracing::warn!(error = %e, "invalid thread id for path");
                                continue;
                            }
                        };
                        let update_path = ox_path::oxpath!("threads", id_comp);
                        let archive = ox_types::UpdateThread {
                            id: None,
                            thread_state: None,
                            inbox_state: Some("done".to_string()),
                            updated_at: None,
                        };
                        let val = structfs_serde_store::to_value(&archive).unwrap();
                        app.pool
                            .inbox()
                            .write(&update_path, structfs_core_store::Record::parsed(val))
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
                        let active_thread_id = match &ui.screen {
                            ScreenSnapshot::Thread(s) => Some(s.thread_id.clone()),
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
                    else if let ScreenSnapshot::Thread(snap) = &ui.screen {
                        if has_approval_pending && ui.editor().is_none() {
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
                    match &ui.screen {
                        // Thread + insert mode: text input mouse handling
                        ScreenSnapshot::Thread(_) if ui.editor().is_some() => {
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
                        ScreenSnapshot::Settings(_)
                            if matches!(mouse.kind, MouseEventKind::Down(_))
                                && settings_shell.state.editing.is_some() =>
                        {
                            settings_shell.handle_mouse(mouse);
                        }
                        // Approval dialog click
                        ScreenSnapshot::Thread(snap)
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
                        // History screen: click-to-expand + content scroll
                        ScreenSnapshot::History(_) => {
                            match mouse.kind {
                                MouseEventKind::Down(_) => {
                                    if let Some(ref hm) = history_hit_map {
                                        handle_history_click(client, hm, mouse.column, mouse.row)
                                            .await;
                                    }
                                }
                                MouseEventKind::ScrollUp => {
                                    if history_explorer.at_content_top() {
                                        // At top of content — move selection up
                                        let _ = client
                                            .write_typed(
                                                &oxpath!("ui"),
                                                &UiCommand::History(HistoryCommand::SelectPrev),
                                            )
                                            .await;
                                    } else {
                                        history_explorer.scroll_content_up(3);
                                    }
                                }
                                MouseEventKind::ScrollDown => {
                                    if history_explorer.at_content_bottom() {
                                        // At bottom of content — move selection down
                                        let _ = client
                                            .write_typed(
                                                &oxpath!("ui"),
                                                &UiCommand::History(HistoryCommand::SelectNext),
                                            )
                                            .await;
                                    } else {
                                        history_explorer.scroll_content_down(3);
                                    }
                                }
                                _ => {}
                            }
                        }
                        // Global fallback (scroll)
                        _ => {
                            let has_active_thread = matches!(&ui.screen, ScreenSnapshot::Thread(_));
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
                    if matches!(&ui.screen, ScreenSnapshot::Settings(_)) {
                        // Paste into the focused settings field
                        if let Some(ref mut editing) = settings_shell.state.editing {
                            if let Some(input) = editing.focused_input() {
                                input.insert_str(&text);
                            }
                        } else if settings_shell.state.focus == SettingsFocus::Defaults {
                            match settings_shell.state.defaults_focus {
                                1 => {
                                    settings_shell.state.default_model.insert_str(&text);
                                    settings_shell.state.model_picker_idx = None;
                                }
                                2 => {
                                    // Only paste digits for max_tokens
                                    let digits: String =
                                        text.chars().filter(|c| c.is_ascii_digit()).collect();
                                    if !digits.is_empty() {
                                        settings_shell.state.default_max_tokens.insert_str(&digits);
                                    }
                                }
                                _ => {}
                            }
                        }
                    } else {
                        let is_insert = ui.editor().is_some();
                        let ctx = ui.editor().map(|e| e.context);
                        if is_insert && ctx != Some(InsertContext::Search) {
                            thread.input_session.insert(&text, EditSource::Paste);
                        }
                    }
                }
                // Focus events — swallow silently. Without EnableFocusChange,
                // these arrive as raw escape sequences that get misparsed as keys.
                Event::FocusGained | Event::FocusLost => {}
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
#[allow(clippy::too_many_arguments)]
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
    match &ui.screen {
        ScreenSnapshot::Settings(_) => {
            let inbox_root = app.pool.inbox_root().to_path_buf();
            if let Outcome::Handled = crate::settings_shell::handle_key(
                &mut settings_shell.state,
                key_str,
                modifiers,
                code,
                client,
                &inbox_root,
            )
            .await
            {
                return;
            }
        }
        ScreenSnapshot::Inbox(_) => {
            // ESC intercept in insert mode (compose/search on inbox)
            if ui.editor().is_some() {
                let insert_ctx = ui.editor().map(|e| e.context);
                if let Outcome::Handled = crate::thread_shell::handle_esc_intercept(
                    key_str,
                    insert_ctx,
                    &mut thread.input_session,
                ) {
                    return;
                }
            }
            if let ScreenSnapshot::Inbox(snap) = &ui.screen {
                if let Outcome::Handled =
                    crate::inbox_shell::handle_key(code, snap.search.active, client).await
                {
                    return;
                }
            }
        }
        ScreenSnapshot::Thread(_) => {
            // ESC intercept in insert mode
            if ui.editor().is_some() {
                let insert_ctx = ui.editor().map(|e| e.context);
                if let Outcome::Handled = crate::thread_shell::handle_esc_intercept(
                    key_str,
                    insert_ctx,
                    &mut thread.input_session,
                ) {
                    return;
                }
            }
        }
        ScreenSnapshot::History(_) => {
            // No screen-specific key handling — all goes through InputStore bindings
        }
    }

    // ? in normal mode toggles shortcuts modal (inbox + thread only)
    let mode = if ui.editor().is_some() {
        Mode::Insert
    } else {
        Mode::Normal
    };
    if mode == Mode::Normal && key_str == "?" && !matches!(&ui.screen, ScreenSnapshot::Settings(_))
    {
        dialog.show_shortcuts = !dialog.show_shortcuts;
        return;
    }

    // InputStore dispatch
    let (input_mode, input_screen) = match &ui.screen {
        ScreenSnapshot::Inbox(_) => (mode, Screen::Inbox),
        ScreenSnapshot::Thread(_) => (mode, Screen::Thread),
        ScreenSnapshot::Settings(_) => (Mode::Normal, Screen::Settings),
        ScreenSnapshot::History(_) => (mode, Screen::History),
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
        let is_insert = ui.editor().is_some();
        let insert_ctx = ui.editor().map(|e| e.context);
        if is_insert {
            if insert_ctx == Some(InsertContext::Search) {
                dispatch_search_edit(client, modifiers, code).await;
            } else {
                let tw = terminal.get_frame().area().width;
                crate::thread_shell::handle_unbound_insert_key(
                    &mut thread.input_session,
                    insert_ctx,
                    app,
                    client,
                    ui,
                    tw,
                    modifiers,
                    code,
                )
                .await;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// History click handling
// ---------------------------------------------------------------------------

/// Handle a mouse click on the history screen using the hit map from rendering.
async fn handle_history_click(
    client: &ox_broker::ClientHandle,
    hit_map: &crate::history_view::HistoryHitMap,
    col: u16,
    row: u16,
) {
    // Convert screen row to content row: account for area offset and content scroll
    let row_offset = row.saturating_sub(hit_map.area_y) + hit_map.content_scroll;
    let col_offset = col.saturating_sub(hit_map.area_x);

    for entry in &hit_map.entries {
        // Check toolbar line first (pretty/full toggles)
        if let Some(ref toolbar) = entry.toolbar {
            if row_offset == toolbar.row {
                // Select the entry first
                let _ = client
                    .write_typed(
                        &oxpath!("ui"),
                        &UiCommand::History(HistoryCommand::SelectRow {
                            row: entry.entry_index,
                        }),
                    )
                    .await;

                if col_offset >= toolbar.pretty_cols.0 && col_offset < toolbar.pretty_cols.1 {
                    let _ = client
                        .write_typed(
                            &oxpath!("ui"),
                            &UiCommand::History(HistoryCommand::TogglePretty),
                        )
                        .await;
                } else if col_offset >= toolbar.full_cols.0 && col_offset < toolbar.full_cols.1 {
                    let _ = client
                        .write_typed(
                            &oxpath!("ui"),
                            &UiCommand::History(HistoryCommand::ToggleFull),
                        )
                        .await;
                }
                return;
            }
        }

        // Check summary line (click to select + toggle expand)
        if row_offset == entry.summary_row {
            let _ = client
                .write_typed(
                    &oxpath!("ui"),
                    &UiCommand::History(HistoryCommand::SelectRow {
                        row: entry.entry_index,
                    }),
                )
                .await;
            let _ = client
                .write_typed(
                    &oxpath!("ui"),
                    &UiCommand::History(HistoryCommand::ToggleExpand),
                )
                .await;
            return;
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
