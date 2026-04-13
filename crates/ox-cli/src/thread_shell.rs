//! Thread screen shell — owns editor state and input view.
//!
//! Extracted from event_loop.rs. ThreadShell holds the InputSession,
//! TextInputView, and prev_mode that were previously loose locals.

use crate::editor::{
    EditorMode, InputSession, flush_pending_edits, handle_editor_command_key,
    handle_editor_insert_key, handle_editor_normal_key,
};
use crate::shell::Outcome;
use crossterm::event::{KeyCode, MouseEvent, MouseEventKind};
use ox_types::{InsertContext, Mode, UiSnapshot};

/// Thread screen local state, owned by the event loop.
pub(crate) struct ThreadShell {
    pub input_session: InputSession,
    pub text_input_view: crate::text_input_view::TextInputView,
    pub prev_mode: Mode,
}

impl ThreadShell {
    pub fn new() -> Self {
        Self {
            input_session: InputSession::new(),
            text_input_view: crate::text_input_view::TextInputView::new(),
            prev_mode: Mode::Normal,
        }
    }

    /// Detect mode transitions and sync InputSession accordingly.
    ///
    /// Call each frame after fetching the UI snapshot.
    pub fn sync_mode(&mut self, ui: &UiSnapshot) {
        let (cur_mode, input_content, input_cursor) = match ui {
            UiSnapshot::Inbox(snap) => (snap.mode, &snap.input.content, snap.input.cursor),
            UiSnapshot::Thread(snap) => (snap.mode, &snap.input.content, snap.input.cursor),
            UiSnapshot::Settings(_) => (Mode::Normal, &String::new(), 0),
        };

        if cur_mode != self.prev_mode {
            if cur_mode == Mode::Insert {
                // Entering insert mode — initialize InputSession from broker
                self.input_session
                    .init_from(input_content.clone(), input_cursor);
                self.input_session.editor_mode = EditorMode::Insert;
            }
            // Note: exiting insert (prev_mode == Insert) is handled by flush()
            // which the caller invokes after sync_mode when prev was Insert.
            self.prev_mode = cur_mode;
        }
    }

    /// Flush pending edits to the broker.
    pub async fn flush(&mut self, client: &ox_broker::ClientHandle) {
        flush_pending_edits(&mut self.input_session, client).await;
    }

    /// Prepare the TextInputView from InputSession (optimistic local state).
    pub fn prepare_view(&mut self) {
        self.text_input_view
            .set_state(&self.input_session.content, self.input_session.cursor);
    }

    /// Handle mouse events on the thread screen (insert mode).
    ///
    /// Handles border drag, click-to-cursor, input area scroll, and falls
    /// through to global mouse dispatch for everything else.
    pub async fn handle_mouse(
        &mut self,
        mouse: MouseEvent,
        has_approval_pending: bool,
        has_pending_customize: bool,
        client: &ox_broker::ClientHandle,
    ) {
        match mouse.kind {
            MouseEventKind::Down(_) if self.text_input_view.is_on_border(mouse.row) => {
                self.text_input_view.start_border_drag(mouse.row);
            }
            MouseEventKind::Drag(_) if self.text_input_view.is_dragging() => {
                self.text_input_view.update_border_drag(mouse.row);
            }
            MouseEventKind::Up(_) if self.text_input_view.is_dragging() => {
                self.text_input_view.end_border_drag();
            }
            MouseEventKind::Down(_) => {
                if let Some(byte_pos) = self
                    .text_input_view
                    .click_to_byte_offset(mouse.column, mouse.row)
                {
                    self.input_session.cursor = byte_pos;
                }
            }
            MouseEventKind::ScrollUp if self.text_input_view.contains(mouse.column, mouse.row) => {
                self.text_input_view.scroll_by(-3);
            }
            MouseEventKind::ScrollDown
                if self.text_input_view.contains(mouse.column, mouse.row) =>
            {
                self.text_input_view.scroll_by(3);
            }
            _ => {
                dispatch_global_mouse(
                    client,
                    true,
                    has_approval_pending,
                    has_pending_customize,
                    mouse.kind,
                )
                .await;
            }
        }
    }

    /// Handle the SendInput pending action using the thread snapshot.
    pub async fn handle_send_input(
        &mut self,
        snap: &ox_types::ThreadSnapshot,
        app: &mut crate::app::App,
        client: &ox_broker::ClientHandle,
    ) {
        use crate::editor::{execute_command_input, submit_editor_content};
        use ox_path::oxpath;
        use ox_types::{GlobalCommand, ThreadCommand, UiCommand};

        if snap.insert_context == Some(InsertContext::Command) {
            flush_pending_edits(&mut self.input_session, client).await;
            execute_command_input(&self.input_session.content, client).await;
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
            self.input_session.reset_after_submit();
        } else {
            let new_tid = submit_editor_content(&mut self.input_session, app, client).await;
            if let Some(tid) = new_tid {
                let _ = client
                    .write_typed(
                        &oxpath!("ui"),
                        &UiCommand::Global(GlobalCommand::Open { thread_id: tid }),
                    )
                    .await;
            }
        }
    }
}

/// Handle thread-specific insert-mode keys.
///
/// Intercepts ESC to toggle between editor sub-modes before the InputStore
/// can fire `ui/exit_insert`. Returns `Outcome::Handled` when consumed.
pub(crate) fn handle_esc_intercept(
    key_str: &str,
    insert_context: Option<InsertContext>,
    input_session: &mut InputSession,
) -> Outcome {
    if key_str == "Esc"
        && insert_context != Some(InsertContext::Search)
        && insert_context != Some(InsertContext::Command)
    {
        match input_session.editor_mode {
            EditorMode::Insert => {
                input_session.editor_mode = EditorMode::Normal;
                return Outcome::Handled;
            }
            EditorMode::Command => {
                input_session.command_buffer.clear();
                input_session.editor_mode = EditorMode::Normal;
                return Outcome::Handled;
            }
            EditorMode::Normal => {
                // Let ESC fall through to InputStore → ui/exit_insert
            }
        }
    }

    Outcome::Ignored
}

/// Handle unbound insert-mode keys (after InputStore dispatch fails).
///
/// Routes to search editing, command editing, or vim-style editor sub-modes.
pub(crate) async fn handle_unbound_insert_key(
    input_session: &mut InputSession,
    insert_context: Option<InsertContext>,
    app: &mut crate::app::App,
    client: &ox_broker::ClientHandle,
    terminal_width: u16,
    modifiers: crossterm::event::KeyModifiers,
    code: KeyCode,
) -> Outcome {
    if insert_context == Some(InsertContext::Command) {
        // Command mode uses the same text editing as editor-insert
        handle_editor_insert_key(input_session, modifiers, code);
    } else {
        // Compose/reply: vim-style editor with sub-modes
        match input_session.editor_mode {
            EditorMode::Insert => {
                handle_editor_insert_key(input_session, modifiers, code);
            }
            EditorMode::Normal => {
                handle_editor_normal_key(input_session, app, client, terminal_width, code).await;
            }
            EditorMode::Command => {
                handle_editor_command_key(input_session, app, client, code).await;
            }
        }
    }

    Outcome::Handled
}

// ---------------------------------------------------------------------------
// Global mouse dispatch
// ---------------------------------------------------------------------------

/// Dispatch mouse scroll events: inbox = select prev/next, thread = scroll.
///
/// Shared by all screens as a fallback when no screen-specific handler matched.
pub(crate) async fn dispatch_global_mouse(
    client: &ox_broker::ClientHandle,
    has_active_thread: bool,
    has_pending_approval: bool,
    has_pending_customize: bool,
    kind: MouseEventKind,
) {
    use ox_path::oxpath;
    use ox_types::{InboxCommand, ThreadCommand, UiCommand};

    if has_pending_approval || has_pending_customize {
        return;
    }

    match kind {
        MouseEventKind::ScrollUp => {
            if has_active_thread {
                let _ = client
                    .write_typed(&oxpath!("ui"), &UiCommand::Thread(ThreadCommand::ScrollUp))
                    .await;
            } else {
                let _ = client
                    .write_typed(&oxpath!("ui"), &UiCommand::Inbox(InboxCommand::SelectPrev))
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
                    .write_typed(&oxpath!("ui"), &UiCommand::Inbox(InboxCommand::SelectNext))
                    .await;
            }
        }
        _ => {}
    }
}
