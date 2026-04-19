//! Action executor — handles PendingAction side effects.
//!
//! Separates pure state transitions (testable) from async broker writes
//! (event loop context). The event loop calls `execute` with the action
//! and context; this module returns the state mutations and broker commands
//! to execute.

use ox_types::{
    Decision, InboxCommand, PendingAction, ScreenSnapshot, ThreadCommand, UiCommand, UiSnapshot,
};

use crate::editor::EditorMode;
use crate::event_loop::{DialogState, HistorySearchState};
use crate::types::APPROVAL_OPTIONS;

/// Side effects to execute after processing a PendingAction.
pub(crate) struct ActionEffects {
    /// Commands to send to UiStore via the broker.
    pub broker_commands: Vec<UiCommand>,
    /// Whether to quit the application.
    pub quit: bool,
    /// Whether to trigger send_input handling.
    pub send_input: bool,
    /// Approval response to send (thread-scoped).
    pub approval_response: Option<Decision>,
    /// Thread to open by ID.
    pub open_thread: Option<String>,
    /// Thread to archive by ID.
    pub archive_thread: Option<String>,
}

impl ActionEffects {
    fn empty() -> Self {
        Self {
            broker_commands: Vec::new(),
            quit: false,
            send_input: false,
            approval_response: None,
            open_thread: None,
            archive_thread: None,
        }
    }
}

/// Execute a PendingAction, producing state mutations on DialogState and
/// a set of effects for the event loop to apply.
///
/// Pure state transitions happen here (testable). Async broker writes
/// are returned as effects for the caller to execute.
pub(crate) fn execute(
    action: PendingAction,
    dialog: &mut DialogState,
    editor_mode: &mut EditorMode,
    ui: &UiSnapshot,
    selected_thread_id: Option<&str>,
    editor_content_setter: &mut Option<String>,
) -> ActionEffects {
    let mut effects = ActionEffects::empty();

    match action {
        PendingAction::Quit => {
            effects.quit = true;
        }
        PendingAction::SendInput => {
            effects.send_input = true;
        }
        PendingAction::OpenSelected => {
            if let Some(id) = selected_thread_id {
                effects.open_thread = Some(id.to_string());
            }
        }
        PendingAction::ArchiveSelected => {
            if let Some(id) = selected_thread_id {
                effects.archive_thread = Some(id.to_string());
            }
        }
        PendingAction::ApprovalConfirm => {
            if let ScreenSnapshot::Thread(snap) = &ui.screen {
                let idx = snap.approval_selected;
                if idx < APPROVAL_OPTIONS.len() {
                    effects.approval_response = Some(APPROVAL_OPTIONS[idx].1);
                }
            }
        }
        PendingAction::Approve(decision) => {
            effects.approval_response = Some(decision);
        }
        PendingAction::ToggleShortcuts => {
            dialog.show_shortcuts = !dialog.show_shortcuts;
        }
        PendingAction::DismissShortcuts => {
            dialog.show_shortcuts = false;
        }
        PendingAction::DismissUsage => {
            dialog.show_usage = false;
        }
        PendingAction::ToggleUsage => {
            dialog.show_usage = !dialog.show_usage;
        }
        PendingAction::EnterHistorySearch => {
            // Results are loaded asynchronously by the caller
            dialog.history_search = Some(HistorySearchState {
                query: String::new(),
                results: Vec::new(),
                selected: 0,
            });
        }
        PendingAction::HistorySearchCycle => {
            if let Some(ref mut state) = dialog.history_search {
                if !state.results.is_empty() {
                    state.selected = (state.selected + 1) % state.results.len();
                }
            }
        }
        PendingAction::AcceptHistorySearch => {
            if let Some(ref state) = dialog.history_search {
                if let Some(text) = state.results.get(state.selected).cloned() {
                    *editor_content_setter = Some(text);
                }
            }
            dialog.history_search = None;
        }
        PendingAction::DismissHistorySearch => {
            dialog.history_search = None;
        }
        PendingAction::ToggleEditorMode => {
            match *editor_mode {
                EditorMode::Insert => {
                    *editor_mode = EditorMode::Normal;
                }
                EditorMode::Normal => {
                    // Exit insert entirely — dismiss editor on current screen
                    match &ui.screen {
                        ScreenSnapshot::Thread(_) => {
                            effects
                                .broker_commands
                                .push(UiCommand::Thread(ThreadCommand::DismissEditor));
                        }
                        ScreenSnapshot::Inbox(_) => {
                            effects
                                .broker_commands
                                .push(UiCommand::Inbox(InboxCommand::DismissEditor));
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    effects
}

#[cfg(test)]
mod tests {
    use super::*;
    use ox_types::{InboxSnapshot, ThreadSnapshot};

    fn empty_dialog() -> DialogState {
        DialogState {
            pending_customize: None,
            show_shortcuts: false,
            show_usage: false,
            history_search: None,
        }
    }

    fn inbox_ui() -> UiSnapshot {
        UiSnapshot {
            screen: ScreenSnapshot::Inbox(InboxSnapshot::default()),
            pending_action: None,
            command_line: Default::default(),
        }
    }

    fn thread_ui(thread_id: &str) -> UiSnapshot {
        UiSnapshot {
            screen: ScreenSnapshot::Thread(ThreadSnapshot {
                thread_id: thread_id.to_string(),
                scroll: 0,
                scroll_max: 0,
                viewport_height: 0,
                editor: None,
                approval_selected: 0,
                approval_preview_scroll: 0,
            }),
            pending_action: None,
            command_line: Default::default(),
        }
    }

    #[test]
    fn quit_sets_flag() {
        let mut dialog = empty_dialog();
        let mut mode = EditorMode::Insert;
        let mut setter = None;
        let effects = execute(
            PendingAction::Quit,
            &mut dialog,
            &mut mode,
            &inbox_ui(),
            None,
            &mut setter,
        );
        assert!(effects.quit);
    }

    #[test]
    fn toggle_shortcuts() {
        let mut dialog = empty_dialog();
        let mut mode = EditorMode::Insert;
        let mut setter = None;

        execute(
            PendingAction::ToggleShortcuts,
            &mut dialog,
            &mut mode,
            &inbox_ui(),
            None,
            &mut setter,
        );
        assert!(dialog.show_shortcuts);

        execute(
            PendingAction::ToggleShortcuts,
            &mut dialog,
            &mut mode,
            &inbox_ui(),
            None,
            &mut setter,
        );
        assert!(!dialog.show_shortcuts);
    }

    #[test]
    fn dismiss_usage() {
        let mut dialog = empty_dialog();
        dialog.show_usage = true;
        let mut mode = EditorMode::Insert;
        let mut setter = None;
        execute(
            PendingAction::DismissUsage,
            &mut dialog,
            &mut mode,
            &inbox_ui(),
            None,
            &mut setter,
        );
        assert!(!dialog.show_usage);
    }

    #[test]
    fn enter_history_search_creates_state() {
        let mut dialog = empty_dialog();
        let mut mode = EditorMode::Insert;
        let mut setter = None;
        execute(
            PendingAction::EnterHistorySearch,
            &mut dialog,
            &mut mode,
            &inbox_ui(),
            None,
            &mut setter,
        );
        assert!(dialog.history_search.is_some());
        let hs = dialog.history_search.as_ref().unwrap();
        assert!(hs.query.is_empty());
        assert_eq!(hs.selected, 0);
    }

    #[test]
    fn history_search_cycle() {
        let mut dialog = empty_dialog();
        dialog.history_search = Some(HistorySearchState {
            query: String::new(),
            results: vec!["a".into(), "b".into(), "c".into()],
            selected: 0,
        });
        let mut mode = EditorMode::Insert;
        let mut setter = None;

        execute(
            PendingAction::HistorySearchCycle,
            &mut dialog,
            &mut mode,
            &inbox_ui(),
            None,
            &mut setter,
        );
        assert_eq!(dialog.history_search.as_ref().unwrap().selected, 1);

        execute(
            PendingAction::HistorySearchCycle,
            &mut dialog,
            &mut mode,
            &inbox_ui(),
            None,
            &mut setter,
        );
        assert_eq!(dialog.history_search.as_ref().unwrap().selected, 2);

        // Wraps around
        execute(
            PendingAction::HistorySearchCycle,
            &mut dialog,
            &mut mode,
            &inbox_ui(),
            None,
            &mut setter,
        );
        assert_eq!(dialog.history_search.as_ref().unwrap().selected, 0);
    }

    #[test]
    fn accept_history_search_sets_content() {
        let mut dialog = empty_dialog();
        dialog.history_search = Some(HistorySearchState {
            query: String::new(),
            results: vec!["selected text".into()],
            selected: 0,
        });
        let mut mode = EditorMode::Insert;
        let mut setter = None;

        execute(
            PendingAction::AcceptHistorySearch,
            &mut dialog,
            &mut mode,
            &inbox_ui(),
            None,
            &mut setter,
        );
        assert!(dialog.history_search.is_none());
        assert_eq!(setter, Some("selected text".to_string()));
    }

    #[test]
    fn dismiss_history_search_clears() {
        let mut dialog = empty_dialog();
        dialog.history_search = Some(HistorySearchState {
            query: "q".into(),
            results: vec!["x".into()],
            selected: 0,
        });
        let mut mode = EditorMode::Insert;
        let mut setter = None;

        execute(
            PendingAction::DismissHistorySearch,
            &mut dialog,
            &mut mode,
            &inbox_ui(),
            None,
            &mut setter,
        );
        assert!(dialog.history_search.is_none());
        assert!(setter.is_none());
    }

    #[test]
    fn toggle_editor_mode_insert_to_normal() {
        let mut dialog = empty_dialog();
        let mut mode = EditorMode::Insert;
        let mut setter = None;
        let effects = execute(
            PendingAction::ToggleEditorMode,
            &mut dialog,
            &mut mode,
            &inbox_ui(),
            None,
            &mut setter,
        );
        assert_eq!(mode, EditorMode::Normal);
        assert!(effects.broker_commands.is_empty());
    }

    #[test]
    fn toggle_editor_mode_normal_exits_insert_thread() {
        let mut dialog = empty_dialog();
        let mut mode = EditorMode::Normal;
        let mut setter = None;
        let effects = execute(
            PendingAction::ToggleEditorMode,
            &mut dialog,
            &mut mode,
            &thread_ui("t_1"),
            None,
            &mut setter,
        );
        assert_eq!(effects.broker_commands.len(), 1);
        assert!(matches!(
            effects.broker_commands[0],
            UiCommand::Thread(ThreadCommand::DismissEditor)
        ));
    }

    #[test]
    fn toggle_editor_mode_normal_exits_insert_inbox() {
        let mut dialog = empty_dialog();
        let mut mode = EditorMode::Normal;
        let mut setter = None;
        let effects = execute(
            PendingAction::ToggleEditorMode,
            &mut dialog,
            &mut mode,
            &inbox_ui(),
            None,
            &mut setter,
        );
        assert_eq!(effects.broker_commands.len(), 1);
        assert!(matches!(
            effects.broker_commands[0],
            UiCommand::Inbox(InboxCommand::DismissEditor)
        ));
    }

    #[test]
    fn approval_confirm_reads_selected() {
        let mut dialog = empty_dialog();
        let mut mode = EditorMode::Insert;
        let mut setter = None;
        let mut ui = thread_ui("t_1");
        if let ScreenSnapshot::Thread(ref mut snap) = ui.screen {
            snap.approval_selected = 2; // AllowAlways
        }
        let effects = execute(
            PendingAction::ApprovalConfirm,
            &mut dialog,
            &mut mode,
            &ui,
            None,
            &mut setter,
        );
        assert_eq!(effects.approval_response, Some(APPROVAL_OPTIONS[2].1));
    }

    #[test]
    fn open_selected_passes_thread_id() {
        let mut dialog = empty_dialog();
        let mut mode = EditorMode::Insert;
        let mut setter = None;
        let effects = execute(
            PendingAction::OpenSelected,
            &mut dialog,
            &mut mode,
            &inbox_ui(),
            Some("t_42"),
            &mut setter,
        );
        assert_eq!(effects.open_thread.as_deref(), Some("t_42"));
    }

    #[test]
    fn archive_selected_passes_thread_id() {
        let mut dialog = empty_dialog();
        let mut mode = EditorMode::Insert;
        let mut setter = None;
        let effects = execute(
            PendingAction::ArchiveSelected,
            &mut dialog,
            &mut mode,
            &inbox_ui(),
            Some("t_99"),
            &mut setter,
        );
        assert_eq!(effects.archive_thread.as_deref(), Some("t_99"));
    }
}
