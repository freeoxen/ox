//! Focus resolution — single source of truth for "which modal is active."
//!
//! Both the renderer and the input dispatcher need to agree on which UI
//! surface is foregrounded. Computing it in two places invites drift;
//! this module owns the priority chain so every consumer picks the same
//! answer from the same inputs.
//!
//! The priority order, highest to lowest:
//!
//! 1. [`Mode::HistorySearch`] — reverse-i-search overlay
//! 2. [`Mode::Shortcuts`] — `?` help modal
//! 3. [`Mode::Usage`] — `$` usage modal
//! 4. [`Mode::Command`] — global vim-style `:` command line
//! 5. [`Mode::Search`] — inbox `/` search input
//! 6. [`Mode::Approval`] — pending tool approval (when editor not open)
//! 7. [`Mode::Insert`] — a screen editor is open
//! 8. [`Mode::Normal`] — default

use ox_types::{Mode, ScreenSnapshot, UiSnapshot};

/// Flags drawn from out-of-snapshot TUI dialog state that the input loop
/// tracks locally (overlays for modals not yet backed by broker state).
pub struct DialogFlags {
    pub history_search_active: bool,
    pub show_shortcuts: bool,
    pub show_usage: bool,
    pub show_thread_info: bool,
    pub has_approval_pending: bool,
}

/// Resolve the current focus mode from UI state + dialog flags. Pure
/// function — takes everything it needs; never reads a side channel.
pub fn focus_mode(ui: &UiSnapshot, dialog: &DialogFlags) -> Mode {
    let inbox_search_open = matches!(
        &ui.screen,
        ScreenSnapshot::Inbox(s) if s.search.mode_open
    );
    if dialog.history_search_active {
        Mode::HistorySearch
    } else if dialog.show_shortcuts {
        Mode::Shortcuts
    } else if dialog.show_usage {
        Mode::Usage
    } else if dialog.show_thread_info {
        Mode::ThreadInfo
    } else if ui.command_line.open {
        Mode::Command
    } else if inbox_search_open {
        Mode::Search
    } else if dialog.has_approval_pending && ui.editor().is_none() {
        Mode::Approval
    } else if ui.editor().is_some() {
        Mode::Insert
    } else {
        Mode::Normal
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ox_types::{CommandLineSnapshot, InboxSnapshot, SearchSnapshot};

    fn ui(command_line_open: bool, search_mode_open: bool, editor: bool) -> UiSnapshot {
        UiSnapshot {
            screen: ScreenSnapshot::Inbox(InboxSnapshot {
                editor: if editor {
                    Some(ox_types::EditorSnapshot {
                        context: ox_types::InsertContext::Compose,
                        content: String::new(),
                        cursor: 0,
                    })
                } else {
                    None
                },
                search: SearchSnapshot {
                    mode_open: search_mode_open,
                    active: search_mode_open,
                    ..Default::default()
                },
                ..Default::default()
            }),
            command_line: CommandLineSnapshot {
                open: command_line_open,
                ..Default::default()
            },
            pending_action: None,
        }
    }

    fn flags() -> DialogFlags {
        DialogFlags {
            history_search_active: false,
            show_shortcuts: false,
            show_usage: false,
            show_thread_info: false,
            has_approval_pending: false,
        }
    }

    #[test]
    fn default_is_normal() {
        assert_eq!(focus_mode(&ui(false, false, false), &flags()), Mode::Normal);
    }

    #[test]
    fn editor_open_gives_insert() {
        assert_eq!(focus_mode(&ui(false, false, true), &flags()), Mode::Insert);
    }

    #[test]
    fn command_line_beats_editor() {
        assert_eq!(focus_mode(&ui(true, false, true), &flags()), Mode::Command);
    }

    #[test]
    fn command_line_beats_search() {
        assert_eq!(focus_mode(&ui(true, true, false), &flags()), Mode::Command);
    }

    #[test]
    fn search_beats_editor() {
        assert_eq!(focus_mode(&ui(false, true, true), &flags()), Mode::Search);
    }

    #[test]
    fn history_search_beats_everything() {
        let mut f = flags();
        f.history_search_active = true;
        assert_eq!(focus_mode(&ui(true, true, true), &f), Mode::HistorySearch);
    }

    #[test]
    fn shortcuts_beats_command_line() {
        let mut f = flags();
        f.show_shortcuts = true;
        assert_eq!(focus_mode(&ui(true, false, false), &f), Mode::Shortcuts);
    }

    #[test]
    fn approval_only_when_no_editor() {
        let mut f = flags();
        f.has_approval_pending = true;
        assert_eq!(focus_mode(&ui(false, false, false), &f), Mode::Approval);
        // With editor open, approval defers
        assert_eq!(focus_mode(&ui(false, false, true), &f), Mode::Insert);
    }
}
