//! Default key binding table for the ox TUI.
//!
//! Encodes the current handle_normal_key / handle_insert_key /
//! handle_approval_key logic as declarative Binding structs.

use std::collections::BTreeMap;

use ox_types::CommandName;
use ox_types::ui::{Mode, Screen};
use ox_ui::{Action, Binding, BindingContext};

/// Build the default binding table.
pub fn default_bindings() -> Vec<Binding> {
    let mut b = Vec::new();
    normal_mode(&mut b);
    history_mode(&mut b);
    insert_mode(&mut b);
    approval_mode(&mut b);
    b
}

fn invoke(command: CommandName) -> Action {
    Action::Invoke {
        command,
        args: BTreeMap::new(),
    }
}

fn invoke_with(command: CommandName, args: &[(&str, &str)]) -> Action {
    let mut map = BTreeMap::new();
    for (k, v) in args {
        map.insert(k.to_string(), serde_json::Value::String(v.to_string()));
    }
    Action::Invoke { command, args: map }
}

fn bind(mode: Mode, key: &str, action: Action, desc: &str) -> Binding {
    Binding {
        context: BindingContext {
            mode,
            key: key.to_string(),
            screen: None,
        },
        action,
        description: desc.to_string(),
        status_hint: false,
    }
}

fn bind_screen(mode: Mode, key: &str, screen: Screen, action: Action, desc: &str) -> Binding {
    Binding {
        context: BindingContext {
            mode,
            key: key.to_string(),
            screen: Some(screen),
        },
        action,
        description: desc.to_string(),
        status_hint: false,
    }
}

/// Like bind_screen, but marks this binding for display in the status bar.
fn hint(mode: Mode, key: &str, screen: Screen, action: Action, desc: &str) -> Binding {
    Binding {
        context: BindingContext {
            mode,
            key: key.to_string(),
            screen: Some(screen),
        },
        action,
        description: desc.to_string(),
        status_hint: true,
    }
}

use CommandName as Cmd;
use Mode::{Approval, Insert, Normal};
use Screen::{History, Inbox, Settings, Thread};

// ---------------------------------------------------------------------------
// Normal mode
// ---------------------------------------------------------------------------

fn normal_mode(out: &mut Vec<Binding>) {
    // Navigation — screen-specific
    out.push(bind_screen(
        Normal,
        "j",
        Inbox,
        invoke(Cmd::SelectNext),
        "Move selection down",
    ));
    out.push(bind_screen(
        Normal,
        "Down",
        Inbox,
        invoke(Cmd::SelectNext),
        "Move selection down",
    ));
    out.push(bind_screen(
        Normal,
        "k",
        Inbox,
        invoke(Cmd::SelectPrev),
        "Move selection up",
    ));
    out.push(bind_screen(
        Normal,
        "Up",
        Inbox,
        invoke(Cmd::SelectPrev),
        "Move selection up",
    ));

    out.push(bind_screen(
        Normal,
        "j",
        Thread,
        invoke(Cmd::ScrollDown),
        "Scroll down",
    ));
    out.push(bind_screen(
        Normal,
        "Down",
        Thread,
        invoke(Cmd::ScrollDown),
        "Scroll down",
    ));
    out.push(bind_screen(
        Normal,
        "k",
        Thread,
        invoke(Cmd::ScrollUp),
        "Scroll up",
    ));
    out.push(bind_screen(
        Normal,
        "Up",
        Thread,
        invoke(Cmd::ScrollUp),
        "Scroll up",
    ));

    // Screen transitions
    out.push(bind_screen(
        Normal,
        "Ctrl+c",
        Thread,
        invoke(Cmd::Close),
        "Back to inbox",
    ));
    out.push(bind_screen(
        Normal,
        "Ctrl+c",
        Inbox,
        invoke(Cmd::Quit),
        "Quit",
    ));
    out.push(hint(Normal, "Esc", Thread, invoke(Cmd::Close), "Back"));
    out.push(bind_screen(
        Normal,
        "q",
        Thread,
        invoke(Cmd::Close),
        "Back to inbox",
    ));
    out.push(bind_screen(Normal, "q", Inbox, invoke(Cmd::Quit), "Quit"));
    out.push(bind(Normal, "Ctrl+t", invoke(Cmd::Close), "Back to inbox"));

    // Enter insert mode — screen determines context
    out.push(hint(Normal, "c", Inbox, invoke(Cmd::Compose), "Compose"));
    out.push(hint(Normal, "c", Thread, invoke(Cmd::Reply), "Reply"));
    out.push(hint(Normal, "/", Inbox, invoke(Cmd::Search), "Search"));

    // Command mode
    out.push(bind(Normal, ":", invoke(Cmd::EnterCommand), "Command"));
    out.push(bind(Normal, ";", invoke(Cmd::EnterCommand), "Command"));

    // -- Vim fast navigation --
    // g/G: go to top/bottom
    out.push(bind_screen(
        Normal,
        "g",
        Inbox,
        invoke(Cmd::SelectFirst),
        "Go to first",
    ));
    out.push(bind_screen(
        Normal,
        "G",
        Inbox,
        invoke(Cmd::SelectLast),
        "Go to last",
    ));
    out.push(bind_screen(
        Normal,
        "g",
        Thread,
        invoke(Cmd::ScrollToTop),
        "Go to top",
    ));
    out.push(bind_screen(
        Normal,
        "G",
        Thread,
        invoke(Cmd::ScrollToBottom),
        "Go to bottom",
    ));

    // d/u and Ctrl+d/u: half-page scroll
    out.push(bind_screen(
        Normal,
        "d",
        Thread,
        invoke(Cmd::ScrollHalfPageDown),
        "Half page down",
    ));
    out.push(bind_screen(
        Normal,
        "Ctrl+d",
        Thread,
        invoke(Cmd::ScrollHalfPageDown),
        "Half page down",
    ));
    out.push(bind_screen(
        Normal,
        "u",
        Thread,
        invoke(Cmd::ScrollHalfPageUp),
        "Half page up",
    ));
    out.push(bind_screen(
        Normal,
        "Ctrl+u",
        Thread,
        invoke(Cmd::ScrollHalfPageUp),
        "Half page up",
    ));

    // Ctrl+f/b: full page scroll
    out.push(bind_screen(
        Normal,
        "Ctrl+f",
        Thread,
        invoke(Cmd::ScrollPageDown),
        "Page down",
    ));
    out.push(bind_screen(
        Normal,
        "Ctrl+b",
        Thread,
        invoke(Cmd::ScrollPageUp),
        "Page up",
    ));

    // History explorer
    out.push(hint(
        Normal,
        "h",
        Thread,
        invoke(Cmd::OpenHistory),
        "History",
    ));

    // Settings
    out.push(bind_screen(
        Normal,
        "s",
        Inbox,
        invoke(Cmd::Settings),
        "Open settings",
    ));
    out.push(bind_screen(
        Normal,
        "Esc",
        Settings,
        invoke(Cmd::Inbox),
        "Back to inbox",
    ));
    out.push(bind_screen(
        Normal,
        "q",
        Settings,
        invoke(Cmd::Inbox),
        "Back to inbox",
    ));
    out.push(bind_screen(
        Normal,
        "Ctrl+c",
        Settings,
        invoke(Cmd::Quit),
        "Quit",
    ));

    // Thread actions
    out.push(hint(
        Normal,
        "Enter",
        Inbox,
        invoke(Cmd::OpenSelected),
        "Open",
    ));
    out.push(bind_screen(
        Normal,
        "d",
        Inbox,
        invoke(Cmd::ArchiveSelected),
        "Archive thread",
    ));

    // Approval quick keys (thread only)
    out.push(bind_screen(
        Normal,
        "y",
        Thread,
        invoke_with(Cmd::Approve, &[("decision", "allow_once")]),
        "Allow once",
    ));
    out.push(bind_screen(
        Normal,
        "n",
        Thread,
        invoke_with(Cmd::Approve, &[("decision", "deny_once")]),
        "Deny once",
    ));
    out.push(bind_screen(
        Normal,
        "s",
        Thread,
        invoke_with(Cmd::Approve, &[("decision", "allow_session")]),
        "Allow for session",
    ));
    out.push(bind_screen(
        Normal,
        "a",
        Thread,
        invoke_with(Cmd::Approve, &[("decision", "allow_always")]),
        "Allow always",
    ));
}

// ---------------------------------------------------------------------------
// History mode
// ---------------------------------------------------------------------------

fn history_mode(out: &mut Vec<Binding>) {
    // Navigation
    out.push(bind_screen(
        Normal,
        "j",
        History,
        invoke(Cmd::SelectNext),
        "Move selection down",
    ));
    out.push(bind_screen(
        Normal,
        "Down",
        History,
        invoke(Cmd::SelectNext),
        "Move selection down",
    ));
    out.push(bind_screen(
        Normal,
        "k",
        History,
        invoke(Cmd::SelectPrev),
        "Move selection up",
    ));
    out.push(bind_screen(
        Normal,
        "Up",
        History,
        invoke(Cmd::SelectPrev),
        "Move selection up",
    ));
    out.push(bind_screen(
        Normal,
        "g",
        History,
        invoke(Cmd::SelectFirst),
        "Go to first",
    ));
    out.push(bind_screen(
        Normal,
        "G",
        History,
        invoke(Cmd::SelectLast),
        "Go to last",
    ));

    // Expand
    out.push(hint(
        Normal,
        "Enter",
        History,
        invoke(Cmd::ToggleExpand),
        "Expand",
    ));
    out.push(bind_screen(
        Normal,
        " ",
        History,
        invoke(Cmd::ToggleExpand),
        "Toggle expand",
    ));
    out.push(bind_screen(
        Normal,
        "e",
        History,
        invoke(Cmd::ExpandAll),
        "Expand all",
    ));
    out.push(bind_screen(
        Normal,
        "E",
        History,
        invoke(Cmd::CollapseAll),
        "Collapse all",
    ));
    out.push(hint(
        Normal,
        "p",
        History,
        invoke(Cmd::TogglePretty),
        "Pretty",
    ));
    out.push(hint(Normal, "f", History, invoke(Cmd::ToggleFull), "Full"));

    // Page movement (moves selection, not just viewport)
    out.push(bind_screen(
        Normal,
        "d",
        History,
        invoke(Cmd::SelectHalfPageDown),
        "Half page down",
    ));
    out.push(bind_screen(
        Normal,
        "Ctrl+d",
        History,
        invoke(Cmd::SelectHalfPageDown),
        "Half page down",
    ));
    out.push(bind_screen(
        Normal,
        "u",
        History,
        invoke(Cmd::SelectHalfPageUp),
        "Half page up",
    ));
    out.push(bind_screen(
        Normal,
        "Ctrl+u",
        History,
        invoke(Cmd::SelectHalfPageUp),
        "Half page up",
    ));
    out.push(bind_screen(
        Normal,
        "Ctrl+f",
        History,
        invoke(Cmd::SelectPageDown),
        "Page down",
    ));
    out.push(bind_screen(
        Normal,
        "Ctrl+b",
        History,
        invoke(Cmd::SelectPageUp),
        "Page up",
    ));

    // Exit
    out.push(hint(
        Normal,
        "Esc",
        History,
        invoke(Cmd::BackToThread),
        "Back",
    ));
    out.push(bind_screen(
        Normal,
        "q",
        History,
        invoke(Cmd::BackToThread),
        "Back to thread",
    ));
    out.push(bind_screen(
        Normal,
        "h",
        History,
        invoke(Cmd::BackToThread),
        "Back to thread",
    ));
    out.push(bind_screen(
        Normal,
        "Ctrl+c",
        History,
        invoke(Cmd::BackToThread),
        "Back to thread",
    ));
}

// ---------------------------------------------------------------------------
// Insert mode
// ---------------------------------------------------------------------------

fn insert_mode(out: &mut Vec<Binding>) {
    out.push(bind(Insert, "Ctrl+s", invoke(Cmd::SendInput), "Send"));
    out.push(bind(Insert, "Ctrl+Enter", invoke(Cmd::SendInput), "Send"));
    out.push(bind(Insert, "Esc", invoke(Cmd::ExitInsert), "Normal mode"));
    out.push(bind(
        Insert,
        "Ctrl+q",
        invoke(Cmd::ExitInsert),
        "Normal mode",
    ));
    // Ctrl+u: screen-specific because search mode handles its own clear
    out.push(bind_screen(
        Insert,
        "Ctrl+u",
        Inbox,
        invoke(Cmd::ClearInput),
        "Clear line",
    ));
    out.push(bind_screen(
        Insert,
        "Ctrl+u",
        Thread,
        invoke(Cmd::ClearInput),
        "Clear line",
    ));
}

// ---------------------------------------------------------------------------
// Approval mode
// ---------------------------------------------------------------------------

fn approval_mode(out: &mut Vec<Binding>) {
    // Option navigation
    out.push(bind(
        Approval,
        "j",
        invoke(Cmd::ApprovalSelectNext),
        "Next option",
    ));
    out.push(bind(
        Approval,
        "Down",
        invoke(Cmd::ApprovalSelectNext),
        "Next option",
    ));
    out.push(bind(
        Approval,
        "k",
        invoke(Cmd::ApprovalSelectPrev),
        "Previous option",
    ));
    out.push(bind(
        Approval,
        "Up",
        invoke(Cmd::ApprovalSelectPrev),
        "Previous option",
    ));
    // Preview scroll
    out.push(bind(
        Approval,
        "Ctrl+j",
        invoke(Cmd::ApprovalScrollDown),
        "Scroll preview down",
    ));
    out.push(bind(
        Approval,
        "Ctrl+k",
        invoke(Cmd::ApprovalScrollUp),
        "Scroll preview up",
    ));

    out.push(bind(
        Approval,
        "y",
        invoke_with(Cmd::Approve, &[("decision", "allow_once")]),
        "Allow once",
    ));
    out.push(bind(
        Approval,
        "n",
        invoke_with(Cmd::Approve, &[("decision", "deny_once")]),
        "Deny once",
    ));
    out.push(bind(
        Approval,
        "s",
        invoke_with(Cmd::Approve, &[("decision", "allow_session")]),
        "Allow for session",
    ));
    out.push(bind(
        Approval,
        "a",
        invoke_with(Cmd::Approve, &[("decision", "allow_always")]),
        "Allow always",
    ));
    out.push(bind(
        Approval,
        "d",
        invoke_with(Cmd::Approve, &[("decision", "deny_always")]),
        "Deny always",
    ));
    // Enter confirms currently selected option
    out.push(bind(
        Approval,
        "Enter",
        invoke(Cmd::ApprovalConfirm),
        "Confirm selected",
    ));
    // Esc closes thread (same as q)
    out.push(bind(Approval, "Escape", invoke(Cmd::Close), "Close thread"));
    // Number keys for direct selection
    for (i, (_, decision)) in crate::types::APPROVAL_OPTIONS.iter().enumerate() {
        let key = format!("{}", i + 1);
        let decision_str = decision.as_str();
        out.push(bind(
            Approval,
            &key,
            invoke_with(Cmd::Approve, &[("decision", decision_str)]),
            &format!("Option {}", i + 1),
        ));
    }
    // q to close thread (falls through to normal dispatch)
    out.push(bind(Approval, "q", invoke(Cmd::Close), "Close thread"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_bindings_not_empty() {
        let bindings = default_bindings();
        assert!(!bindings.is_empty());
    }

    #[test]
    fn j_has_screen_specific_bindings() {
        let bindings = default_bindings();
        let j_inbox: Vec<_> = bindings
            .iter()
            .filter(|b| {
                b.context.mode == Normal && b.context.key == "j" && b.context.screen == Some(Inbox)
            })
            .collect();
        let j_thread: Vec<_> = bindings
            .iter()
            .filter(|b| {
                b.context.mode == Normal && b.context.key == "j" && b.context.screen == Some(Thread)
            })
            .collect();
        assert_eq!(j_inbox.len(), 1);
        assert_eq!(j_thread.len(), 1);
    }

    #[test]
    fn all_three_modes_have_bindings() {
        let bindings = default_bindings();
        assert!(bindings.iter().any(|b| b.context.mode == Normal));
        assert!(bindings.iter().any(|b| b.context.mode == Insert));
        assert!(bindings.iter().any(|b| b.context.mode == Approval));
    }

    #[test]
    fn h_on_thread_opens_history() {
        let bindings = default_bindings();
        let found: Vec<_> = bindings
            .iter()
            .filter(|b| {
                b.context.mode == Normal && b.context.key == "h" && b.context.screen == Some(Thread)
            })
            .collect();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].description, "History");
    }

    #[test]
    fn history_screen_has_bindings() {
        let bindings = default_bindings();
        let history_bindings: Vec<_> = bindings
            .iter()
            .filter(|b| b.context.screen == Some(History))
            .collect();
        assert!(
            history_bindings.len() >= 16,
            "expected at least 16 history bindings, got {}",
            history_bindings.len()
        );
    }

    #[test]
    fn bindings_have_descriptions() {
        let bindings = default_bindings();
        for b in &bindings {
            assert!(
                !b.description.is_empty(),
                "binding {:?} has empty description",
                b.context
            );
        }
    }
}
