use crate::app::App;
use crate::editor::flush_pending_edits;
use crate::thread_shell::{ThreadShell, dispatch_global_mouse};
use crate::types::CustomizeState;
use crate::view_state::fetch_view_state;
use crossterm::event::{self, Event, KeyCode, KeyModifiers, MouseEventKind};
use horns_ratatui::Theme;
use ox_kernel::PathComponent;
use ox_path::oxpath;
use ox_types::{
    GlobalCommand, HistoryCommand, InboxCommand, Mode, PendingAction, Screen, ScreenSnapshot,
    ThreadCommand, UiCommand, UiSnapshot,
};
use ox_ui::text_input_store::EditSource;
use std::time::Duration;

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
    pub pending_customize: Option<CustomizeState>,
    pub show_shortcuts: bool,
    pub show_usage: bool,
    pub show_thread_info: bool,
    /// Cached thread info, keyed by `(thread_id, log_count_at_cache)`.
    /// `None` while the modal is closed or a fetch hasn't yet
    /// populated it on this turn — the renderer treats that as a
    /// loading state.
    pub thread_info: Option<ThreadInfoEntry>,
    pub history_search: Option<HistorySearchState>,
}

/// One snapshot of [`crate::types::ThreadInfo`] keyed by the log
/// length at the time it was captured. The pair `(info.meta.id,
/// log_count_at_cache)` is the cache identity — a change in either
/// invalidates and triggers a refetch.
pub(crate) struct ThreadInfoEntry {
    pub info: crate::types::ThreadInfo,
    pub log_count_at_cache: i64,
}

/// State for Ctrl+R reverse-incremental search.
pub(crate) struct HistorySearchState {
    pub query: String,
    pub results: Vec<String>,
    pub selected: usize,
}

/// Outcome of the legacy event loop, telling the top-level state
/// machine which way to pivot.
pub enum LegacyExit {
    /// User navigated to a horns-owned screen (today: settings).
    /// The caller transitions into `horns_loop::run_horns_settings_loop`.
    ToHorns,
    /// User asked to quit the program.
    Quit,
}

/// Async event loop that dispatches through the BrokerStore.
///
/// ALL state mutations go through UiStore via the broker. Text editing
/// commands (insert_char, delete_char) are dispatched directly to UiStore
/// when no InputStore binding matches. Application-level commands
/// (send, open, archive, quit) are signaled via UiStore's pending_action
/// field and handled by App methods.
///
/// Returns when the user transitions to a horns-owned screen (the
/// top-level state machine pivots to `horns_loop::run_horns_settings_loop`)
/// or asks to quit.
pub async fn run_async(
    app: &mut App,
    client: &ox_broker::ClientHandle,
    theme: &Theme,
    terminal: &mut ratatui::DefaultTerminal,
    needs_setup: bool,
) -> std::io::Result<LegacyExit> {
    use crate::key_encode::encode_key;

    let mut dialog = DialogState {
        pending_customize: None,
        show_shortcuts: false,
        show_usage: false,
        show_thread_info: false,
        thread_info: None,
        history_search: None,
    };
    let mut thread = ThreadShell::new();
    let mut history_explorer = crate::history_state::HistoryExplorer::new();
    let mut scroll_momentum = ScrollMomentum::new();

    // First-run still routes to the settings screen; the new pipeline
    // takes over from there. The wizard's first-run cursor placement
    // (land on `_new` overlay) is Phase Q2's responsibility — for P2 we
    // just navigate to the settings screen and let the cursor default
    // (set just below) place us on `settings/index`.
    if needs_setup {
        client
            .write_typed(
                &oxpath!("ui"),
                &UiCommand::Global(GlobalCommand::GoToSettings),
            )
            .await
            .ok();
    }

    // No local settings registries: bindings + commands + renderers
    // all live inside horns' side-tables, registered once by
    // `settings::install`. The event loop reads the View from the
    // broker each frame; help hints are projected from the broker's
    // `<bindings_prefix>` + `<commands_prefix>` subtrees.

    loop {
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
            // Keep the thread-info cache fresh before building the
            // view state — fetch_view_state remains a pure reader.
            refresh_thread_info_cache(client, &mut dialog).await;

            let vs = fetch_view_state(client, app, &dialog, thread.input_session.editor_mode).await;

            // State-machine pivot: when the user transitions to the
            // settings screen, the legacy loop yields. The top-level
            // state machine in `main.rs` re-enters
            // `horns_loop::run_horns_settings_loop` which owns the
            // terminal for the duration of the session.
            if matches!(&vs.ui.screen, ScreenSnapshot::Settings(_)) {
                return Ok(LegacyExit::ToHorns);
            }

            // Editor sync (detects editor appeared/disappeared, flushes edits)
            let had_editor = thread.had_editor;
            let has_editor = vs.ui.editor().is_some();
            if had_editor && !has_editor {
                thread.flush(client).await;
            }
            thread.sync_editor(&vs.ui);

            // Command-line submit drain. A submit write stages the
            // buffer text on `ui/command_line/pending_submit`; we
            // dispatch it through `command/exec` here (on a tick
            // separate from the submit write itself) and ack by
            // clearing the field. This breaks re-entrancy with UiStore
            // for commands whose target routes back into `ui/*`.
            if let Some(text) = vs.ui.command_line.pending_submit.clone() {
                let _ = client
                    .write(
                        &oxpath!("command", "exec"),
                        structfs_core_store::Record::parsed(structfs_core_store::Value::String(
                            text,
                        )),
                    )
                    .await;
                let _ = client
                    .write(
                        &oxpath!("ui", "command_line", "clear_pending_submit"),
                        structfs_core_store::Record::parsed(structfs_core_store::Value::Null),
                    )
                    .await;
            }

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

            // Settings: when the UiSnapshot reports Screen::Settings,
            // the legacy event loop returns and the top-level state
            // machine pivots to `run_horns_settings_loop`. Until then
            // — or transiently between transition writes — the legacy
            // frame logic below tolerates a Settings screen by simply
            // skipping the `terminal.draw` call. No inline settings
            // rendering happens here; the horns loop owns it.
            let on_settings_screen = matches!(&vs.ui.screen, ScreenSnapshot::Settings(_));

            // Draw. Settings is special: horns_ratatui's
            // `ViewRenderSubscription` holds the terminal mutex and
            // redraws on every render-tick from the broker, so the
            // event loop must NOT call `terminal.draw` here — they'd
            // race on the lock. For every other screen the event loop
            // owns the draw call as before.
            let mut pending_hyperlink: Option<crate::tui::PendingHyperlink> = None;
            if !on_settings_screen {
                terminal.draw(|frame| {
                    let (ch, vh, hm, hl) = crate::tui::draw(
                        frame,
                        &vs,
                        None,
                        theme,
                        &mut thread.text_input_view,
                        &mut history_explorer,
                    );
                    content_height = ch;
                    viewport_height = vh;
                    history_hit_map = hm;
                    pending_hyperlink = hl;
                })?;
            }

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
            let mut editor_content = None;
            let effects = crate::action_executor::execute(
                action,
                &mut dialog,
                &mut thread.input_session.editor_mode,
                &ui,
                selected_thread_id.as_deref(),
                &mut editor_content,
            );

            if effects.quit {
                return Ok(LegacyExit::Quit);
            }
            if effects.send_input {
                thread.handle_send_input(&ui, app, client).await;
            }
            if let Some(text) = editor_content {
                thread.input_session.set_content(&text);
            }
            if let Some(id) = &effects.open_thread {
                let _ = client
                    .write_typed(
                        &oxpath!("ui"),
                        &UiCommand::Global(GlobalCommand::Open {
                            thread_id: id.clone(),
                        }),
                    )
                    .await;
                // Ensure the agent worker is spawned for this thread.
                // First-access lazy mount runs the resume classifier and
                // sets `shell/resume_needed` for `AwaitingApproval` /
                // `AwaitingToolResult` tails (Task 3d), but the flag only
                // gets consumed when an `agent_worker` is alive to read
                // it. In production we previously spawned workers only
                // on `send_prompt`, which meant a user opening a thread
                // that was blocked on an approval at exit would NOT see
                // the modal reappear until they typed something —
                // breaking the resume promise. Calling `ensure_worker`
                // here closes that gap. The worker reads the flag, the
                // kernel prologue re-requests approval, the modal
                // appears.
                app.pool.ensure_worker(id);
            }
            if let Some(id) = &effects.archive_thread {
                if let Ok(id_comp) = PathComponent::try_new(id.as_str()) {
                    let update_path = ox_path::oxpath!("inbox", "threads", id_comp);
                    let archive = ox_types::UpdateThread {
                        id: None,
                        thread_state: None,
                        inbox_state: Some("done".to_string()),
                        updated_at: None,
                    };
                    let val = structfs_serde_store::to_value(&archive).unwrap();
                    let _ = client
                        .write(&update_path, structfs_core_store::Record::parsed(val))
                        .await;
                }
            }
            if let Some(decision) = effects.approval_response {
                if let ScreenSnapshot::Thread(snap) = &ui.screen {
                    let tid = Some(snap.thread_id.clone());
                    crate::key_handlers::send_approval_response(client, &tid, decision).await;
                }
            }
            for cmd in &effects.broker_commands {
                let _ = client.write_typed(&oxpath!("ui"), cmd).await;
            }
            // Load history search results if just entered
            if matches!(action, PendingAction::EnterHistorySearch) {
                if let Some(ref mut hs) = dialog.history_search {
                    hs.results = load_recent_inputs(client).await;
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
                    // Customize dialog is the only remaining bypass — it has
                    // its own text editing and complex widget state.
                    if dialog.pending_customize.is_some() {
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
                    } else if let Some(key_str) = encode_key(key.modifiers, key.code) {
                        dispatch_key(
                            &ui,
                            &key_str,
                            key.modifiers,
                            key.code,
                            has_approval_pending,
                            &mut dialog,
                            &mut thread,
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
                        // Settings: mouse handling is not yet wired into the
                        // new pipeline. The bespoke click-to-focus path
                        // operated on the legacy `SettingsState` which P2
                        // retired. Future phases reintroduce focus-driven
                        // mouse handling against the snapshot/registry.
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
                Event::Paste(ref text) => {
                    // Normalize line endings: \r\n → \n, bare \r → \n.
                    // Some clipboard sources (macOS) may use \r or \r\n.
                    let text = text.replace("\r\n", "\n").replace('\r', "\n");
                    // Settings paste is not yet wired into the new
                    // registry pipeline (would need a per-cursor
                    // text-paste command). The bespoke path that lived
                    // here is gone with the rest of the legacy shell.
                    if !matches!(&ui.screen, ScreenSnapshot::Settings(_)) && ui.editor().is_some() {
                        thread.input_session.insert(&text, EditSource::Paste);
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
// Input-store dispatch (non-settings screens)
// ---------------------------------------------------------------------------

/// Outcome of a key-dispatch attempt. The `Unbound` arm carries the
/// mode the broker's input-store resolver concluded — non-settings
/// clients use it to route the key through the appropriate text-input
/// fallback (`Insert`, `Command`, `Search`) without recomputing mode
/// locally.
///
/// Settings dispatches always return `Handled`: the settings install's
/// `KeyDispatchSubscription` runs in-broker and emits writes
/// asynchronously; the event loop does not observe a "miss" the way
/// the input-store path does.
enum KeyDispatchOutcome {
    Handled,
    Unbound { mode: Mode },
}

/// Non-settings dispatch path: encode the key as an `InputKeyEvent`,
/// write to `input/key`, and translate the substrate's reply path into
/// a `KeyDispatchOutcome`. The broker's `InputStore` mount runs the
/// mode resolver against live state and either invokes a matched
/// command or reports `unbound/<mode>` for the text-input fallback.
async fn send_via_input_store(
    client: &ox_broker::ClientHandle,
    key: &str,
    screen: Screen,
    flags: ox_types::ClientModalFlags,
) -> KeyDispatchOutcome {
    let event = ox_types::InputKeyEvent {
        mode: None,
        key: key.to_string(),
        screen,
        flags,
    };
    let result = client.write_typed(&oxpath!("input", "key"), &event).await;
    match result {
        Ok(p) if p.components.first().map(|c| c.as_str()) == Some("unbound") => {
            let mode = p
                .components
                .get(1)
                .and_then(|c| Mode::parse(c.as_str()))
                .unwrap_or(Mode::Normal);
            KeyDispatchOutcome::Unbound { mode }
        }
        Ok(_) => KeyDispatchOutcome::Handled,
        Err(e) => {
            tracing::warn!(error = %e, key = %key, "input key dispatch failed");
            // Treat genuine errors as handled to avoid the fallback
            // re-routing the key into a text-input handler in an
            // unexpected state.
            KeyDispatchOutcome::Handled
        }
    }
}

// ---------------------------------------------------------------------------
// Key dispatch — factored out to avoid duplication
// ---------------------------------------------------------------------------

/// Dispatch a key event through screen-specific handlers, then InputStore.
/// Populate / invalidate the thread-info cache before rendering.
///
/// Runs once per tick while the info modal is open. Resolves the
/// selected thread by screen (Inbox: row → id; Thread: snapshot id),
/// reads `threads/{id}/log/count` for a live freshness signal, and
/// compares `(id, log_count)` to the cache. Hit → return; miss →
/// `fetch_thread_info` and store the new entry.
///
/// ## Concurrency contract
///
/// The freshness signal is `log/count`, which reads
/// [`ox_kernel::log::SharedLog`]'s `Vec<LogEntry>` length under its
/// `Mutex`. Appends to the log and reads of its count are serialized
/// — we can never read a partial count relative to an in-flight
/// append. The log entries themselves, read later by
/// [`crate::view_state::fetch_thread_info`], see at least the entries
/// counted; they may see strictly more if an append lands between the
/// two reads. That's "cache may serve fresher than its key claims,"
/// which is benign — the next frame sees an even higher `log_count`
/// and refetches.
///
/// Expected RUST_LOG=thread_info=debug narrative for a typical modal
/// session against a thread with one message, then an append, then a
/// repeat refresh:
///
/// ```text
///   thread_info: cache miss — fetched thread_id=t_x log_count=2 duration_us=…
///   thread_info: cache hit thread_id=t_x log_count=2
///   thread_info: cache miss — fetched thread_id=t_x log_count=3 duration_us=…
/// ```
pub(crate) async fn refresh_thread_info_cache(
    client: &ox_broker::ClientHandle,
    dialog: &mut DialogState,
) {
    use structfs_core_store::Value;

    if !dialog.show_thread_info {
        return;
    }

    // 1. Read UiSnapshot once.
    let ui: UiSnapshot = client
        .read_typed::<UiSnapshot>(&oxpath!("ui"))
        .await
        .ok()
        .flatten()
        .unwrap_or_default();

    // 2. Resolve the selected thread row, screen-aware.
    let row: Option<crate::parse::InboxThread> = match &ui.screen {
        ScreenSnapshot::Inbox(s) => {
            let rows: Vec<crate::parse::InboxThread> = if s.search.active {
                crate::view_state::fetch_search_results(client, &s.search).await
            } else {
                match client.read(&oxpath!("inbox", "threads")).await {
                    Ok(Some(rec)) => rec
                        .as_value()
                        .map(crate::parse::parse_inbox_threads)
                        .unwrap_or_default(),
                    _ => Vec::new(),
                }
            };
            rows.into_iter().nth(s.selected_row)
        }
        ScreenSnapshot::Thread(s) => {
            let id_comp = match ox_kernel::PathComponent::try_new(s.thread_id.as_str()) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(
                        target: "thread_info",
                        thread_id = %s.thread_id, error = %e,
                        "invalid thread id; modal will show partial or loading state",
                    );
                    return;
                }
            };
            match client
                .read(&ox_path::oxpath!("inbox", "threads", id_comp))
                .await
            {
                Ok(Some(rec)) => rec
                    .as_value()
                    .map(|v| crate::parse::parse_inbox_threads(&Value::Array(vec![v.clone()])))
                    .and_then(|mut v| v.pop()),
                Ok(None) => {
                    tracing::warn!(
                        target: "thread_info",
                        thread_id = %s.thread_id,
                        "thread missing from inbox; modal will show loading state",
                    );
                    None
                }
                Err(e) => {
                    tracing::warn!(
                        target: "thread_info",
                        thread_id = %s.thread_id, error = %e,
                        "inbox row read failed; modal will show partial or loading state",
                    );
                    return;
                }
            }
        }
        _ => None,
    };

    let Some(row) = row else {
        // No selection (or no row found) — clear cache so the modal
        // renders the loading placeholder rather than a stale value.
        dialog.thread_info = None;
        tracing::debug!(
            target: "thread_info",
            "no selected thread row; cache cleared",
        );
        return;
    };

    // 3. Read live freshness signal (`log/count`).
    let id_comp = match ox_kernel::PathComponent::try_new(row.id.as_str()) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                target: "thread_info",
                thread_id = %row.id, error = %e,
                "invalid thread id for log/count read",
            );
            return;
        }
    };
    let count_path = ox_path::oxpath!("threads", id_comp, "log", "count");
    let log_count: i64 = match client.read(&count_path).await {
        Ok(Some(rec)) => match rec.as_value() {
            Some(Value::Integer(n)) => *n,
            _ => 0,
        },
        Ok(None) => 0,
        Err(e) => {
            tracing::warn!(
                target: "thread_info",
                thread_id = %row.id, error = %e,
                "log/count read failed; modal will show partial or loading state",
            );
            return;
        }
    };

    // 4. Compare `(id, log_count)` to cache.
    let is_hit = dialog
        .thread_info
        .as_ref()
        .map(|e| e.info.id() == row.id && e.log_count_at_cache == log_count)
        .unwrap_or(false);
    if is_hit {
        tracing::debug!(
            target: "thread_info",
            thread_id = %row.id, log_count,
            "cache hit",
        );
        return;
    }

    // 5. Miss → fetch and store.
    let start = std::time::Instant::now();
    let info = crate::view_state::fetch_thread_info(client, &row).await;
    tracing::debug!(
        target: "thread_info",
        thread_id = %row.id, log_count,
        duration_us = start.elapsed().as_micros() as u64,
        "cache miss — fetched",
    );
    dialog.thread_info = Some(ThreadInfoEntry {
        info,
        log_count_at_cache: log_count,
    });
}

///
/// Called for all key events that aren't consumed by modal overlays (shortcuts,
/// customize dialog, approval dialog).
///
/// On the settings screen, `dispatch_key` encodes the chord and writes
/// it to the horns settings install's input path. The
/// `KeyDispatchSubscription` registered by `settings::install` handles
/// the rest in-broker: matches the binding (or `TextInputHandler`),
/// runs the command, emits writes, bumps the render tick. After the
/// async write returns, we also check for the `_request_exit` flag
/// (`nav.ascend` writes it at the top-level cursor) and route to the
/// inbox if set.
///
/// Non-settings screens (inbox, thread, history) still go through the
/// legacy input-store dispatch path (`send_via_input_store`) which
/// returns either `Handled` or `Unbound { mode }` — the latter routes
/// the keystroke through a text-input fallback below.
#[allow(clippy::too_many_arguments)]
async fn dispatch_key(
    ui: &UiSnapshot,
    key_str: &str,
    modifiers: KeyModifiers,
    code: KeyCode,
    has_approval_pending: bool,
    dialog: &mut DialogState,
    thread: &mut ThreadShell,
    app: &mut App,
    client: &ox_broker::ClientHandle,
    terminal: &mut ratatui::DefaultTerminal,
) {
    // Settings dispatch crosses the broker — we write a `KeyChord`
    // record to the install's `input_path/key` and the
    // `KeyDispatchSubscription` registered by `settings::install`
    // reads the live cursor and runs the matched command from there.
    // No `&UiSnapshot` or `&ViewState` reaches the binding lookup;
    // the subscription's `snapshot` is a fresh broker reader.
    //
    // `has_approval_pending` and `ui` are still inspected *here* for
    // legitimate non-binding reasons: the screen tag (informational,
    // not a focus decision) and the Insert-fallback's editor-context
    // lookup (text routing, not binding lookup).
    let _ = has_approval_pending;

    // Esc dismisses the shortcuts modal first, before any
    // screen-specific dispatch sees the key. The modal is a global
    // overlay over whatever page the user is on; consuming Esc here
    // means hitting Esc to close `?` doesn't also collapse a
    // settings tree row, cancel an inline edit, or back out of the
    // thread view.
    if dialog.show_shortcuts && matches!(code, KeyCode::Esc) {
        dialog.show_shortcuts = false;
        return;
    }

    let input_screen = match &ui.screen {
        ScreenSnapshot::Inbox(_) => Screen::Inbox,
        ScreenSnapshot::Thread(_) => Screen::Thread,
        ScreenSnapshot::Settings(_) => Screen::Settings,
        ScreenSnapshot::History(_) => Screen::History,
    };

    // Settings: dispatch through the horns broker pipeline. We encode
    // the crossterm key as a `KeyChord` via the existing
    // `encode_key` → `parse_key_str` round-trip and write it to the
    // settings install's `input_path/key`. The horns
    // `KeyDispatchSubscription` registered by `settings::install`
    // watches that path, runs the matched command (or
    // `TextInputHandler`), applies the emitted writes through the
    // broker dispatcher, and bumps the render tick so the
    // `RenderSubscription` produces a fresh View.
    //
    // Non-settings screens continue through the legacy input-store
    // dispatch path: encode the key, write to `input/key`, the
    // InputStore-mounted resolver decides handled-or-unbound. The
    // `Unbound` outcome routes the key into a text-input fallback
    // (Insert / Command / Search) below.
    let outcome = if input_screen == Screen::Settings {
        if let Some(chord) = crate::key_chord_canonical::parse_key_str(key_str) {
            let key_path = crate::settings::input_key_path();
            if let Err(e) = client.write_typed(&key_path, &chord).await {
                tracing::warn!(error = %e, key = %key_str, "settings key dispatch broker write failed");
            }
            KeyDispatchOutcome::Handled
        } else {
            // Function keys etc. don't round-trip through the
            // encoder; nothing in the settings binding table claims
            // them today, so silently swallow them rather than
            // routing through the legacy input store (which would
            // misclassify the key for a screen that's no longer
            // wired through it).
            KeyDispatchOutcome::Handled
        }
    } else {
        send_via_input_store(
            client,
            key_str,
            input_screen,
            ox_types::ClientModalFlags {
                history_search_active: dialog.history_search.is_some(),
                show_shortcuts: dialog.show_shortcuts,
                show_usage: dialog.show_usage,
                show_thread_info: dialog.show_thread_info,
            },
        )
        .await
    };

    // After settings dispatch, react to a `_request_exit` write from
    // `nav.ascend` at the top-level cursor: clear the flag and route to
    // the inbox via `GoToInbox`. Settings keeps its own `ui/settings/*`
    // sub-state (cursor, etc.) so reopening the screen returns to where
    // the user left off.
    if input_screen == Screen::Settings {
        let exit_path = oxpath!("ui", "settings", "_request_exit");
        let want_exit = client
            .read_typed::<bool>(&exit_path)
            .await
            .ok()
            .flatten()
            .unwrap_or(false);
        if want_exit {
            // Clear the flag first so we don't loop on the next frame.
            let _ = client.write_typed(&exit_path, &false).await;
            let _ = client
                .write_typed(&oxpath!("ui"), &UiCommand::Global(GlobalCommand::GoToInbox))
                .await;
        }
    }

    if let KeyDispatchOutcome::Unbound { mode } = outcome {
        match mode {
            Mode::Command => handle_unbound_command_line_key(client, ui, code).await,
            Mode::Search => handle_unbound_search_key(client, code).await,
            Mode::Insert => {
                let insert_ctx = ui.editor().map(|e| e.context);
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
            _ => {}
        }
    }
}

/// Translate an unbound Search-mode key into a `SearchInsertChar` write.
/// Control keys (Esc, Enter, Backspace, Ctrl+U, Ctrl+C) are bound in the
/// binding table and never reach this fallback.
pub(crate) async fn handle_unbound_search_key(client: &ox_broker::ClientHandle, code: KeyCode) {
    if let KeyCode::Char(c) = code {
        let _ = client
            .write_typed(
                &oxpath!("ui"),
                &UiCommand::Inbox(InboxCommand::SearchInsertChar { char: c }),
            )
            .await;
    }
}

/// Translate an unbound Command-mode key into an edit on the command-line
/// buffer. Ordinary chars insert; Backspace deletes one char before the
/// cursor. Everything else is a no-op — control keys that matter (Esc,
/// Enter, Ctrl+C) are handled by Command-mode bindings above.
async fn handle_unbound_command_line_key(
    client: &ox_broker::ClientHandle,
    ui: &UiSnapshot,
    code: KeyCode,
) {
    use ox_ui::text_input_store::{Edit, EditOp, EditSequence, EditSource};

    let cursor = ui.command_line.cursor;
    let edit = match code {
        KeyCode::Char(c) => Edit {
            op: EditOp::Insert {
                text: c.to_string(),
            },
            at: cursor,
            source: EditSource::Key,
            ts_ms: 0,
        },
        KeyCode::Backspace => {
            if cursor == 0 {
                return;
            }
            // Delete the char ending at `cursor`. Find the previous char
            // boundary in the content so we handle multi-byte codepoints.
            let content = &ui.command_line.content;
            let prev_boundary = content[..cursor]
                .char_indices()
                .next_back()
                .map(|(i, _)| i)
                .unwrap_or(0);
            let len = cursor - prev_boundary;
            Edit {
                op: EditOp::Delete { len },
                at: prev_boundary,
                source: EditSource::Key,
                ts_ms: 0,
            }
        }
        _ => return,
    };
    let seq = EditSequence {
        edits: vec![edit],
        generation: 0,
    };
    let _ = client
        .write_typed(&oxpath!("ui", "command_line", "edit"), &seq)
        .await;
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
// Ctrl+R history search — load recent inputs
// ---------------------------------------------------------------------------

/// Load N most recent inputs from ox.db via broker.
///
/// Uses the unified search path with an empty terms list and "inputs" scope.
async fn load_recent_inputs(client: &ox_broker::ClientHandle) -> Vec<String> {
    use structfs_core_store::Value;

    let query_val = structfs_serde_store::json_to_value(serde_json::json!({
        "terms": [],
        "scope": "inputs",
    }));

    // Static path — safe to unwrap
    let search_path = structfs_core_store::Path::parse("inbox/search").unwrap();
    let handle = match client
        .write(&search_path, structfs_core_store::Record::parsed(query_val))
        .await
    {
        Ok(h) => h,
        Err(_) => return Vec::new(),
    };
    // handle is a Path from InboxStore; join with inbox prefix for broker routing
    let page_path = match structfs_core_store::Path::parse(&format!("inbox/{handle}/limit/50")) {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };

    match client.read(&page_path).await {
        Ok(Some(record)) => {
            let items = match record.as_value() {
                Some(Value::Map(map)) => match map.get("items") {
                    Some(Value::Array(arr)) => arr.clone(),
                    _ => return Vec::new(),
                },
                Some(Value::Array(arr)) => arr.clone(),
                _ => return Vec::new(),
            };
            items
                .iter()
                .filter_map(|v| match v {
                    Value::Map(m) => match m.get("text") {
                        Some(Value::String(s)) => Some(s.clone()),
                        _ => None,
                    },
                    _ => None,
                })
                .collect()
        }
        _ => Vec::new(),
    }
}
