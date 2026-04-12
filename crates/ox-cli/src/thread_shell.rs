//! Thread screen key handling — extracted from event_loop.rs.
//!
//! Handles editor sub-mode dispatch (compose/reply) and ESC interception.

use crate::editor::{
    EditorMode, InputSession, handle_editor_command_key, handle_editor_insert_key,
    handle_editor_normal_key,
};
use crate::shell::Outcome;
use crossterm::event::KeyCode;
use ox_types::InsertContext;

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
