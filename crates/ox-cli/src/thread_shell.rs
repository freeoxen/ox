//! Thread screen shell — owns editor state and input view.
//!
//! Extracted from event_loop.rs. ThreadShell holds the InputSession,
//! TextInputView, and had_editor that were previously loose locals.

use crate::editor::{
    EditorMode, InputSession, flush_pending_edits, handle_editor_insert_key,
    handle_editor_normal_key,
};
use crate::shell::Outcome;
use crossterm::event::{KeyCode, MouseEvent, MouseEventKind};
use ox_types::{InsertContext, ScreenSnapshot, UiSnapshot};

/// Thread screen local state, owned by the event loop.
pub(crate) struct ThreadShell {
    pub input_session: InputSession,
    pub text_input_view: crate::text_input_view::TextInputView,
    pub had_editor: bool,
}

impl ThreadShell {
    pub fn new() -> Self {
        Self {
            input_session: InputSession::new(),
            text_input_view: crate::text_input_view::TextInputView::new(),
            had_editor: false,
        }
    }

    /// Detect editor transitions and sync InputSession accordingly.
    ///
    /// Call each frame after fetching the UI snapshot.
    pub fn sync_editor(&mut self, ui: &UiSnapshot) {
        let has_editor = ui.editor().is_some();

        if has_editor && !self.had_editor {
            // Editor appeared — initialize InputSession from snapshot
            if let Some(editor) = ui.editor() {
                self.input_session
                    .init_from(editor.content.clone(), editor.cursor);
                self.input_session.editor_mode = EditorMode::Insert;
            }
        }
        // Note: editor disappeared (had_editor && !has_editor) is handled by flush()
        // which the caller invokes before sync_editor.
        self.had_editor = has_editor;
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
                    3, // no momentum in insert mode
                )
                .await;
            }
        }
    }

    /// Handle the SendInput pending action using the UI snapshot.
    pub async fn handle_send_input(
        &mut self,
        ui: &UiSnapshot,
        app: &mut crate::app::App,
        client: &ox_broker::ClientHandle,
    ) {
        use crate::editor::submit_editor_content;
        use ox_path::oxpath;
        use ox_types::UiCommand;

        let new_tid = submit_editor_content(&mut self.input_session, app, client).await;
        // Only auto-navigate to the new thread if we're already on a thread
        // screen (reply). Compose from inbox stays on the inbox.
        if let Some(tid) = new_tid {
            if matches!(&ui.screen, ScreenSnapshot::Thread(_)) {
                let _ = client
                    .write_typed(
                        &oxpath!("ui"),
                        &UiCommand::Global(ox_types::GlobalCommand::Open { thread_id: tid }),
                    )
                    .await;
            }
        }
    }
}

/// Handle unbound insert-mode keys (after InputStore dispatch fails).
///
/// Routes to the editor's Insert or Normal sub-mode. The global `:`
/// command line is a separate modal surface handled upstream.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn handle_unbound_insert_key(
    input_session: &mut InputSession,
    _insert_context: Option<InsertContext>,
    app: &mut crate::app::App,
    client: &ox_broker::ClientHandle,
    _ui: &UiSnapshot,
    terminal_width: u16,
    modifiers: crossterm::event::KeyModifiers,
    code: KeyCode,
) -> Outcome {
    match input_session.editor_mode {
        EditorMode::Insert => {
            handle_editor_insert_key(input_session, modifiers, code);
        }
        EditorMode::Normal => {
            handle_editor_normal_key(input_session, app, client, terminal_width, code).await;
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
    scroll_lines: u16,
) {
    use ox_path::oxpath;
    use ox_types::{InboxCommand, ThreadCommand, UiCommand};

    if has_pending_approval || has_pending_customize {
        return;
    }

    match kind {
        MouseEventKind::ScrollUp => {
            if has_active_thread {
                for _ in 0..scroll_lines {
                    let _ = client
                        .write_typed(&oxpath!("ui"), &UiCommand::Thread(ThreadCommand::ScrollUp))
                        .await;
                }
            } else {
                let _ = client
                    .write_typed(&oxpath!("ui"), &UiCommand::Inbox(InboxCommand::SelectPrev))
                    .await;
            }
        }
        MouseEventKind::ScrollDown => {
            if has_active_thread {
                for _ in 0..scroll_lines {
                    let _ = client
                        .write_typed(
                            &oxpath!("ui"),
                            &UiCommand::Thread(ThreadCommand::ScrollDown),
                        )
                        .await;
                }
            } else {
                let _ = client
                    .write_typed(&oxpath!("ui"), &UiCommand::Inbox(InboxCommand::SelectNext))
                    .await;
            }
        }
        _ => {}
    }
}
