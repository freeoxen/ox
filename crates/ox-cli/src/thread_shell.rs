//! Thread screen shell — owns editor state and input view.
//!
//! Extracted from event_loop.rs. ThreadShell holds the InputSession,
//! TextInputView, and prev_mode that were previously loose locals.

use crate::editor::{
    EditorMode, InputSession, flush_pending_edits, handle_editor_command_key,
    handle_editor_insert_key, handle_editor_normal_key,
};
use crate::shell::Outcome;
use crossterm::event::KeyCode;
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
        let cur_mode = match ui {
            UiSnapshot::Thread(snap) => snap.mode,
            _ => Mode::Normal,
        };

        if cur_mode != self.prev_mode {
            if cur_mode == Mode::Insert {
                // Entering insert mode — initialize InputSession from broker
                if let UiSnapshot::Thread(snap) = ui {
                    self.input_session
                        .init_from(snap.input.content.clone(), snap.input.cursor);
                }
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
