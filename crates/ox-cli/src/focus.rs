//! Focus resolution — single source of truth for "which surface owns
//! the keys right now."
//!
//! # Architecture
//!
//! There is exactly **one** function that implements the priority
//! chain: [`focus_mode`]. It is pure: it takes a [`FocusInputs`]
//! value and returns a [`Mode`].
//!
//! Two adapters build [`FocusInputs`] from the two places focus state
//! lives:
//!
//! - [`FocusInputs::from_snapshot`] — sync, builds from a TUI-side
//!   `UiSnapshot` + locally-tracked [`DialogFlags`]. Used by the
//!   renderer and the unbound-key fallback (status bar, chrome).
//! - [`FocusInputs::from_broker`] — async, reads live broker state
//!   and combines it with client-local modal flags carried in the
//!   key-event request map. Used by the server-side
//!   [`crate::broker_setup`] mode resolver to authoritatively decide
//!   which binding fires for a key event.
//!
//! Both adapters terminate at the same `focus_mode(&inputs)` call,
//! so there is no possibility of drift between renderer focus and
//! dispatch focus.
//!
//! # Priority order, highest to lowest
//!
//! 1. [`Mode::HistorySearch`] — reverse-i-search overlay
//! 2. [`Mode::Shortcuts`] — `?` help modal
//! 3. [`Mode::Usage`] — `$` usage modal
//! 4. [`Mode::ThreadInfo`] — thread info modal
//! 5. [`Mode::Command`] — global vim-style `:` command line
//! 6. [`Mode::Search`] — inbox `/` search input
//! 7. [`Mode::Approval`] — pending tool approval (when editor not
//!    open). Note: this is a *key-routing* label — the approval
//!    renders inline as a card at the tail of the transcript, NOT as
//!    a blocking modal. Conversation scroll keys are bound in this
//!    mode too so the user can scroll up to read context before
//!    deciding.
//! 8. [`Mode::Insert`] — a screen editor is open
//! 9. [`Mode::Normal`] — default

use ox_types::{Mode, ScreenSnapshot, UiSnapshot};
use std::collections::BTreeMap;
use structfs_core_store::Value;

/// Flags drawn from the client-side TUI state that the broker has no
/// view of (overlays for modals not currently backed by broker
/// state). These are carried in the key-event request map so the
/// server-side resolver can combine them with its own live reads.
pub struct DialogFlags {
    pub history_search_active: bool,
    pub show_shortcuts: bool,
    pub show_usage: bool,
    pub show_thread_info: bool,
    pub has_approval_pending: bool,
}

/// All inputs to the focus priority chain. Constructed by one of the
/// two adapter constructors; consumed by [`focus_mode`].
///
/// The fields are deliberately flat booleans — every input to the
/// rule is one bit, the rule is one priority chain. Adding a new
/// surface to the focus order means adding one field here and one
/// branch in [`focus_mode`].
pub struct FocusInputs {
    // Broker-owned focus surfaces.
    pub command_line_open: bool,
    pub editor_open: bool,
    pub inbox_search_open: bool,
    pub has_approval_pending: bool,
    // Client-local modal surfaces.
    pub history_search_active: bool,
    pub show_shortcuts: bool,
    pub show_usage: bool,
    pub show_thread_info: bool,
}

/// THE focus rule. Pure. The single source of truth for which mode
/// owns the keys given a complete picture of focus state.
///
/// Both the renderer (via [`FocusInputs::from_snapshot`]) and the
/// server resolver (via [`FocusInputs::from_broker`]) call into this
/// function. There is no other place that may decide focus.
pub fn focus_mode(inputs: &FocusInputs) -> Mode {
    if inputs.history_search_active {
        Mode::HistorySearch
    } else if inputs.show_shortcuts {
        Mode::Shortcuts
    } else if inputs.show_usage {
        Mode::Usage
    } else if inputs.show_thread_info {
        Mode::ThreadInfo
    } else if inputs.command_line_open {
        Mode::Command
    } else if inputs.inbox_search_open {
        Mode::Search
    } else if inputs.has_approval_pending && !inputs.editor_open {
        Mode::Approval
    } else if inputs.editor_open {
        Mode::Insert
    } else {
        Mode::Normal
    }
}

impl FocusInputs {
    /// Build inputs from a client-side `UiSnapshot` + locally-tracked
    /// dialog flags. Used by the renderer and any client code that
    /// needs to know "what mode would the broker resolve right now?"
    /// for purely-presentational reasons (status bar, fallback
    /// routing for unbound keys).
    ///
    /// **Never use this for dispatch decisions.** Snapshots can be
    /// stale; dispatch decisions belong on the broker side. See
    /// [`FocusInputs::from_broker`].
    pub fn from_snapshot(ui: &UiSnapshot, dialog: &DialogFlags) -> Self {
        let inbox_search_open = matches!(
            &ui.screen,
            ScreenSnapshot::Inbox(s) if s.search.mode_open
        );
        FocusInputs {
            command_line_open: ui.command_line.open,
            editor_open: ui.editor().is_some(),
            inbox_search_open,
            has_approval_pending: dialog.has_approval_pending,
            history_search_active: dialog.history_search_active,
            show_shortcuts: dialog.show_shortcuts,
            show_usage: dialog.show_usage,
            show_thread_info: dialog.show_thread_info,
        }
    }

    /// Build inputs from live broker state plus the client-local
    /// modal flags carried in the key-event request map. Used by the
    /// server-side mode resolver wired in [`crate::broker_setup`].
    ///
    /// This is the only place that issues broker reads to compute
    /// focus. By doing it at dispatch time, the result reflects the
    /// most recent state — closing snapshot-vs-dispatch races for
    /// every broker-owned focus field at once.
    pub async fn from_broker(
        client: &ox_broker::ClientHandle,
        req_map: &BTreeMap<String, Value>,
    ) -> Self {
        let ui: UiSnapshot = client
            .read_typed::<UiSnapshot>(&structfs_core_store::path!("ui"))
            .await
            .ok()
            .flatten()
            .unwrap_or_default();

        let inbox_search_open = matches!(
            &ui.screen,
            ScreenSnapshot::Inbox(s) if s.search.mode_open
        );

        // Approval pending is only meaningful on the Thread screen;
        // tolerate `Value::Null` (no pending) by swallowing decode
        // errors with `.ok().flatten()`.
        let has_approval_pending = if let ScreenSnapshot::Thread(snap) = &ui.screen {
            match ox_kernel::PathComponent::try_new(snap.thread_id.as_str()) {
                Ok(tid) => {
                    let path = ox_path::oxpath!("threads", tid, "approval", "pending");
                    client
                        .read_typed::<ox_types::ApprovalRequest>(&path)
                        .await
                        .ok()
                        .flatten()
                        .is_some_and(|ap| !ap.tool_name.is_empty())
                }
                Err(_) => false,
            }
        } else {
            false
        };

        FocusInputs {
            command_line_open: ui.command_line.open,
            editor_open: ui.editor().is_some(),
            inbox_search_open,
            has_approval_pending,
            history_search_active: bool_flag(req_map, "history_search_active"),
            show_shortcuts: bool_flag(req_map, "show_shortcuts"),
            show_usage: bool_flag(req_map, "show_usage"),
            show_thread_info: bool_flag(req_map, "show_thread_info"),
        }
    }
}

fn bool_flag(map: &BTreeMap<String, Value>, key: &str) -> bool {
    matches!(map.get(key), Some(Value::Bool(true)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ox_types::{CommandLineSnapshot, InboxSnapshot, SearchSnapshot};

    fn inputs() -> FocusInputs {
        FocusInputs {
            command_line_open: false,
            editor_open: false,
            inbox_search_open: false,
            has_approval_pending: false,
            history_search_active: false,
            show_shortcuts: false,
            show_usage: false,
            show_thread_info: false,
        }
    }

    // -- focus_mode (the rule) ------------------------------------

    #[test]
    fn default_is_normal() {
        assert_eq!(focus_mode(&inputs()), Mode::Normal);
    }

    #[test]
    fn editor_open_gives_insert() {
        let i = FocusInputs {
            editor_open: true,
            ..inputs()
        };
        assert_eq!(focus_mode(&i), Mode::Insert);
    }

    #[test]
    fn command_line_beats_editor() {
        let i = FocusInputs {
            command_line_open: true,
            editor_open: true,
            ..inputs()
        };
        assert_eq!(focus_mode(&i), Mode::Command);
    }

    #[test]
    fn command_line_beats_search() {
        let i = FocusInputs {
            command_line_open: true,
            inbox_search_open: true,
            ..inputs()
        };
        assert_eq!(focus_mode(&i), Mode::Command);
    }

    #[test]
    fn search_beats_editor() {
        let i = FocusInputs {
            inbox_search_open: true,
            editor_open: true,
            ..inputs()
        };
        assert_eq!(focus_mode(&i), Mode::Search);
    }

    #[test]
    fn history_search_beats_everything() {
        let i = FocusInputs {
            history_search_active: true,
            command_line_open: true,
            inbox_search_open: true,
            editor_open: true,
            has_approval_pending: true,
            ..inputs()
        };
        assert_eq!(focus_mode(&i), Mode::HistorySearch);
    }

    #[test]
    fn shortcuts_beats_command_line() {
        let i = FocusInputs {
            show_shortcuts: true,
            command_line_open: true,
            ..inputs()
        };
        assert_eq!(focus_mode(&i), Mode::Shortcuts);
    }

    #[test]
    fn approval_only_when_no_editor() {
        let i = FocusInputs {
            has_approval_pending: true,
            ..inputs()
        };
        assert_eq!(focus_mode(&i), Mode::Approval);
        let i = FocusInputs {
            has_approval_pending: true,
            editor_open: true,
            ..inputs()
        };
        assert_eq!(focus_mode(&i), Mode::Insert);
    }

    // -- from_snapshot (the renderer-side adapter) ----------------

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

    fn dialog_flags() -> DialogFlags {
        DialogFlags {
            history_search_active: false,
            show_shortcuts: false,
            show_usage: false,
            show_thread_info: false,
            has_approval_pending: false,
        }
    }

    #[test]
    fn from_snapshot_extracts_command_line_open() {
        let i = FocusInputs::from_snapshot(&ui(true, false, false), &dialog_flags());
        assert!(i.command_line_open);
        assert_eq!(focus_mode(&i), Mode::Command);
    }

    #[test]
    fn from_snapshot_extracts_editor_open() {
        let i = FocusInputs::from_snapshot(&ui(false, false, true), &dialog_flags());
        assert!(i.editor_open);
        assert_eq!(focus_mode(&i), Mode::Insert);
    }

    #[test]
    fn from_snapshot_extracts_inbox_search_open() {
        let i = FocusInputs::from_snapshot(&ui(false, true, false), &dialog_flags());
        assert!(i.inbox_search_open);
        assert_eq!(focus_mode(&i), Mode::Search);
    }

    #[test]
    fn from_snapshot_carries_dialog_flags() {
        let mut f = dialog_flags();
        f.show_shortcuts = true;
        let i = FocusInputs::from_snapshot(&ui(false, false, false), &f);
        assert!(i.show_shortcuts);
        assert_eq!(focus_mode(&i), Mode::Shortcuts);
    }
}
