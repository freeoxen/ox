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

// ---------------------------------------------------------------------------
// Scroll momentum — exponential boost for fast scrolling, decay for slow
// ---------------------------------------------------------------------------

/// Tracks scroll velocity for acceleration. Fast continuous scrolling
/// ramps up; slow or direction-changed scrolling stays precise.
struct ScrollMomentum {
    last_time: std::time::Instant,
    /// -1 = up, 1 = down, 0 = none
    last_direction: i8,
    /// Multiplier applied to base scroll amount. Starts at 1.0.
    velocity: f32,
}

impl ScrollMomentum {
    fn new() -> Self {
        Self {
            last_time: std::time::Instant::now(),
            last_direction: 0,
            velocity: 1.0,
        }
    }

    /// Record a scroll event and return the number of lines to scroll.
    /// `direction`: -1 for up, 1 for down.
    fn scroll(&mut self, direction: i8) -> u16 {
        let now = std::time::Instant::now();
        let elapsed_ms = now.duration_since(self.last_time).as_millis();

        if direction != self.last_direction {
            // Direction changed — reset to base speed
            self.velocity = 1.0;
        } else if elapsed_ms < 60 {
            // Rapid scrolling — boost
            self.velocity = (self.velocity * 1.08).min(4.0);
        } else if elapsed_ms < 150 {
            // Moderate scrolling — gentle boost
            self.velocity = (self.velocity * 1.03).min(4.0);
        } else {
            // Slow/paused — decay back toward 1.0
            self.velocity = 1.0 + (self.velocity - 1.0) * 0.3;
            if self.velocity < 1.05 {
                self.velocity = 1.0;
            }
        }

        self.last_time = now;
        self.last_direction = direction;

        // Base 3 lines, scaled by velocity
        (3.0 * self.velocity).round() as u16
    }
}

/// Dialog-local state, owned by the event loop (not App, not broker).
pub(crate) struct DialogState {
    pub approval_selected: usize,
    pub pending_customize: Option<CustomizeState>,
    pub show_shortcuts: bool,
    pub show_usage: bool,
    pub history_search: Option<HistorySearchState>,
}

/// State for Ctrl+R reverse-incremental search.
pub(crate) struct HistorySearchState {
    pub query: String,
    pub results: Vec<String>,
    pub selected: usize,
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
        show_usage: false,
        history_search: None,
    };
    let mut thread = ThreadShell::new();
    let mut history_explorer = crate::history_state::HistoryExplorer::new();
    let mut scroll_momentum = ScrollMomentum::new();
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
        let approval_preview_len: usize;
        let approval_tool_len: usize;
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
            let mut pending_hyperlink: Option<crate::tui::PendingHyperlink> = None;
            terminal.draw(|frame| {
                let (ch, vh, hm, hl) = crate::tui::draw(
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
                pending_hyperlink = hl;
            })?;

            // Post-draw: write OSC 8 hyperlink if the usage dialog is showing a URL.
            // Ratatui doesn't support hyperlinks natively, so we write raw escapes
            // after the frame is flushed.
            if let Some(hl) = &pending_hyperlink {
                use std::io::Write;
                let mut stdout = std::io::stdout();
                // Move cursor to the URL position and write OSC 8 hyperlink
                let _ = crossterm::queue!(
                    stdout,
                    crossterm::cursor::MoveTo(hl.col, hl.row),
                    crossterm::style::Print(format!(
                        "\x1b]8;;{}\x07{}\x1b]8;;\x07",
                        hl.url, hl.text
                    ))
                );
                let _ = stdout.flush();
            }

            // Extract the few values needed after dropping vs
            selected_thread_id = match &vs.ui.screen {
                ScreenSnapshot::Inbox(s) => {
                    vs.inbox_threads.get(s.selected_row).map(|t| t.id.clone())
                }
                _ => None,
            };
            has_approval_pending = vs.approval_pending.is_some();
            // Stash preview info for approval dialog mouse hit-testing
            approval_preview_len = vs
                .approval_pending
                .as_ref()
                .map(|ap| ap.input_preview.len())
                .unwrap_or(0);
            approval_tool_len = vs
                .approval_pending
                .as_ref()
                .map(|ap| ap.tool_name.len())
                .unwrap_or(0);
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
        // 4. Drain all pending terminal events
        // -----------------------------------------------------------------
        let events: Vec<Event> = tokio::task::block_in_place(|| {
            let mut buf = Vec::new();
            // Block up to 50ms for the first event
            if event::poll(Duration::from_millis(50)).unwrap_or(false) {
                if let Ok(evt) = event::read() {
                    buf.push(evt);
                }
            }
            // Drain remaining queued events without blocking
            while event::poll(Duration::ZERO).unwrap_or(false) {
                if let Ok(evt) = event::read() {
                    buf.push(evt);
                } else {
                    break;
                }
            }
            buf
        });

        // -----------------------------------------------------------------
        // 5. Dispatch terminal events
        // -----------------------------------------------------------------
        for evt in events {
            match evt {
                Event::Key(key) => {
                    // History search — handles all keys when active
                    if dialog.history_search.is_some() {
                        handle_history_search_key(
                            &mut dialog,
                            app,
                            client,
                            &ui,
                            &mut thread,
                            key.modifiers,
                            key.code,
                        )
                        .await;
                    }
                    // Usage dialog — dismiss on any key
                    else if dialog.show_usage {
                        dialog.show_usage = false;
                    }
                    // Shortcuts modal — dismiss on ? or Esc, swallow all other keys
                    else if dialog.show_shortcuts {
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
                            let (term_w, term_h) = crossterm::terminal::size().unwrap_or((80, 24));
                            // Compute dialog dimensions matching draw_approval_dialog
                            let prefix_len = approval_tool_len + 3; // "[tool] "
                            let dialog_width = ((prefix_len + approval_preview_len + 4) as u16)
                                .clamp(50, term_w.saturating_sub(4));
                            let inner_width = dialog_width.saturating_sub(2) as usize;
                            let preview_avail = inner_width.saturating_sub(prefix_len);
                            let wrapped_count =
                                if preview_avail > 0 && approval_preview_len > preview_avail {
                                    approval_preview_len.div_ceil(preview_avail)
                                } else {
                                    1
                                };
                            let dialog_h =
                                (2 + wrapped_count as u16 + 9).min(term_h.saturating_sub(4));
                            let dialog_top = term_h.saturating_sub(dialog_h) / 2;
                            // Options start after: border(1) + wrapped lines + blank(1)
                            let first_option_row = dialog_top + 1 + wrapped_count as u16 + 1;
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
                        ScreenSnapshot::History(_) => match mouse.kind {
                            MouseEventKind::Down(_) => {
                                if let Some(ref hm) = history_hit_map {
                                    handle_history_click(client, hm, mouse.column, mouse.row).await;
                                }
                            }
                            MouseEventKind::ScrollUp => {
                                let lines = scroll_momentum.scroll(-1);
                                if history_explorer.at_content_top() {
                                    let _ = client
                                        .write_typed(
                                            &oxpath!("ui"),
                                            &UiCommand::History(HistoryCommand::SelectPrev),
                                        )
                                        .await;
                                } else {
                                    history_explorer.scroll_content_up(lines);
                                }
                            }
                            MouseEventKind::ScrollDown => {
                                let lines = scroll_momentum.scroll(1);
                                if history_explorer.at_content_bottom() {
                                    let _ = client
                                        .write_typed(
                                            &oxpath!("ui"),
                                            &UiCommand::History(HistoryCommand::SelectNext),
                                        )
                                        .await;
                                } else {
                                    history_explorer.scroll_content_down(lines);
                                }
                            }
                            _ => {}
                        },
                        // Thread normal mode: click on status bar → toggle usage dialog
                        ScreenSnapshot::Thread(_)
                            if matches!(mouse.kind, MouseEventKind::Down(_)) =>
                        {
                            let term_h = crossterm::terminal::size().map(|(_, h)| h).unwrap_or(24);
                            if mouse.row == term_h.saturating_sub(1) {
                                dialog.show_usage = !dialog.show_usage;
                            }
                        }
                        // Global fallback (scroll)
                        _ => {
                            let has_active_thread = matches!(&ui.screen, ScreenSnapshot::Thread(_));
                            let scroll_lines = match mouse.kind {
                                MouseEventKind::ScrollUp => scroll_momentum.scroll(-1),
                                MouseEventKind::ScrollDown => scroll_momentum.scroll(1),
                                _ => 3,
                            };
                            dispatch_global_mouse(
                                client,
                                has_active_thread,
                                has_approval_pending,
                                dialog.pending_customize.is_some(),
                                mouse.kind,
                                scroll_lines,
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

    let mode = if ui.editor().is_some() {
        Mode::Insert
    } else {
        Mode::Normal
    };

    // Ctrl+R in insert mode (Compose or Reply) → enter history search
    if mode == Mode::Insert
        && modifiers.contains(KeyModifiers::CONTROL)
        && code == KeyCode::Char('r')
    {
        let ctx = ui.editor().map(|e| e.context);
        if matches!(
            ctx,
            Some(InsertContext::Compose) | Some(InsertContext::Reply)
        ) {
            // Load recent inputs from ox.db for initial display
            let recent = load_recent_inputs(app, 50);
            dialog.history_search = Some(HistorySearchState {
                query: String::new(),
                results: recent,
                selected: 0,
            });
            return;
        }
    }

    // ? in normal mode toggles shortcuts modal (inbox + thread only)
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

// ---------------------------------------------------------------------------
// Ctrl+R history search key handling
// ---------------------------------------------------------------------------

async fn handle_history_search_key(
    dialog: &mut DialogState,
    app: &mut App,
    _client: &ox_broker::ClientHandle,
    _ui: &UiSnapshot,
    thread: &mut ThreadShell,
    modifiers: KeyModifiers,
    code: KeyCode,
) {
    let state = match &mut dialog.history_search {
        Some(s) => s,
        None => return,
    };

    match (modifiers, code) {
        // Ctrl+R again → cycle to next match
        (KeyModifiers::CONTROL, KeyCode::Char('r')) => {
            if !state.results.is_empty() {
                state.selected = (state.selected + 1) % state.results.len();
            }
        }
        // Enter → accept selected result, populate input editor
        (_, KeyCode::Enter) => {
            if let Some(text) = state.results.get(state.selected).cloned() {
                thread.input_session.set_content(&text);
            }
            dialog.history_search = None;
        }
        // Esc / Ctrl+G → cancel
        (_, KeyCode::Esc) | (KeyModifiers::CONTROL, KeyCode::Char('g')) => {
            dialog.history_search = None;
        }
        // Backspace → delete last char from query
        (_, KeyCode::Backspace) => {
            state.query.pop();
            refresh_search_results(state, app);
        }
        // Printable char → append to query, re-search
        (_, KeyCode::Char(c)) if !modifiers.contains(KeyModifiers::CONTROL) => {
            state.query.push(c);
            refresh_search_results(state, app);
        }
        _ => {}
    }
}

/// Re-query the search database based on the current query string.
fn refresh_search_results(state: &mut HistorySearchState, app: &mut App) {
    state.results = if state.query.is_empty() {
        load_recent_inputs(app, 50)
    } else {
        search_inputs_fts(app, &state.query, 50)
    };
    if state.results.is_empty() {
        state.selected = 0;
    } else {
        state.selected = state.selected.min(state.results.len() - 1);
    }
}

/// Load N most recent inputs from ox.db.
fn load_recent_inputs(app: &mut App, limit: usize) -> Vec<String> {
    use structfs_core_store::{Reader, Value};
    let path = structfs_core_store::Path::parse(&format!("inputs/recent/{limit}")).unwrap();
    match app.pool.inbox().read(&path) {
        Ok(Some(record)) => match record.as_value() {
            Some(Value::Array(arr)) => arr
                .iter()
                .filter_map(|v| match v {
                    Value::Map(m) => match m.get("text") {
                        Some(Value::String(s)) => Some(s.clone()),
                        _ => None,
                    },
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

/// FTS5 search over inputs via the StructFS write-then-read-handle pattern.
fn search_inputs_fts(app: &mut App, query: &str, limit: usize) -> Vec<String> {
    use structfs_core_store::{Reader, Value, Writer};
    let query_val = structfs_serde_store::json_to_value(serde_json::json!({
        "query": query,
        "limit": limit,
    }));
    let handle = match app.pool.inbox().write(
        &structfs_core_store::Path::parse("search/inputs").unwrap(),
        structfs_core_store::Record::parsed(query_val),
    ) {
        Ok(h) => h,
        Err(_) => return Vec::new(),
    };
    match app.pool.inbox().read(&handle) {
        Ok(Some(record)) => match record.as_value() {
            Some(Value::Array(arr)) => arr
                .iter()
                .filter_map(|v| match v {
                    Value::Map(m) => match m.get("text") {
                        Some(Value::String(s)) => Some(s.clone()),
                        _ => None,
                    },
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}
