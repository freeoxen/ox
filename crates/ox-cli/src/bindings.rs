//! Default key binding table for the ox TUI.
//!
//! Encodes the current handle_normal_key / handle_insert_key /
//! handle_approval_key logic as declarative Binding structs.

use std::collections::BTreeMap;

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

fn invoke(command: &str) -> Action {
    Action::Invoke {
        command: command.to_string(),
        args: BTreeMap::new(),
    }
}

fn invoke_with(command: &str, args: &[(&str, &str)]) -> Action {
    let mut map = BTreeMap::new();
    for (k, v) in args {
        map.insert(k.to_string(), serde_json::Value::String(v.to_string()));
    }
    Action::Invoke {
        command: command.to_string(),
        args: map,
    }
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
        invoke("select_next"),
        "Move selection down",
    ));
    out.push(bind_screen(
        Normal,
        "Down",
        Inbox,
        invoke("select_next"),
        "Move selection down",
    ));
    out.push(bind_screen(
        Normal,
        "k",
        Inbox,
        invoke("select_prev"),
        "Move selection up",
    ));
    out.push(bind_screen(
        Normal,
        "Up",
        Inbox,
        invoke("select_prev"),
        "Move selection up",
    ));

    out.push(bind_screen(
        Normal,
        "j",
        Thread,
        invoke("scroll_down"),
        "Scroll down",
    ));
    out.push(bind_screen(
        Normal,
        "Down",
        Thread,
        invoke("scroll_down"),
        "Scroll down",
    ));
    out.push(bind_screen(
        Normal,
        "k",
        Thread,
        invoke("scroll_up"),
        "Scroll up",
    ));
    out.push(bind_screen(
        Normal,
        "Up",
        Thread,
        invoke("scroll_up"),
        "Scroll up",
    ));

    // Screen transitions
    out.push(bind_screen(
        Normal,
        "Ctrl+c",
        Thread,
        invoke("close"),
        "Back to inbox",
    ));
    out.push(bind_screen(Normal, "Ctrl+c", Inbox, invoke("quit"), "Quit"));
    out.push(hint(Normal, "Esc", Thread, invoke("close"), "Back"));
    out.push(bind_screen(
        Normal,
        "q",
        Thread,
        invoke("close"),
        "Back to inbox",
    ));
    out.push(bind_screen(Normal, "q", Inbox, invoke("quit"), "Quit"));
    out.push(bind(Normal, "Ctrl+t", invoke("close"), "Back to inbox"));

    // Enter insert mode — screen determines context
    out.push(hint(Normal, "c", Inbox, invoke("compose"), "Compose"));
    out.push(hint(Normal, "c", Thread, invoke("reply"), "Reply"));
    out.push(hint(Normal, "/", Inbox, invoke("search"), "Search"));

    // Command mode
    out.push(bind(Normal, ":", invoke("enter_command"), "Command"));
    out.push(bind(Normal, ";", invoke("enter_command"), "Command"));

    // -- Vim fast navigation --
    // g/G: go to top/bottom
    out.push(bind_screen(
        Normal,
        "g",
        Inbox,
        invoke("select_first"),
        "Go to first",
    ));
    out.push(bind_screen(
        Normal,
        "G",
        Inbox,
        invoke("select_last"),
        "Go to last",
    ));
    out.push(bind_screen(
        Normal,
        "g",
        Thread,
        invoke("scroll_to_top"),
        "Go to top",
    ));
    out.push(bind_screen(
        Normal,
        "G",
        Thread,
        invoke("scroll_to_bottom"),
        "Go to bottom",
    ));

    // d/u and Ctrl+d/u: half-page scroll
    out.push(bind_screen(
        Normal,
        "d",
        Thread,
        invoke("scroll_half_page_down"),
        "Half page down",
    ));
    out.push(bind_screen(
        Normal,
        "Ctrl+d",
        Thread,
        invoke("scroll_half_page_down"),
        "Half page down",
    ));
    out.push(bind_screen(
        Normal,
        "u",
        Thread,
        invoke("scroll_half_page_up"),
        "Half page up",
    ));
    out.push(bind_screen(
        Normal,
        "Ctrl+u",
        Thread,
        invoke("scroll_half_page_up"),
        "Half page up",
    ));

    // Ctrl+f/b: full page scroll
    out.push(bind_screen(
        Normal,
        "Ctrl+f",
        Thread,
        invoke("scroll_page_down"),
        "Page down",
    ));
    out.push(bind_screen(
        Normal,
        "Ctrl+b",
        Thread,
        invoke("scroll_page_up"),
        "Page up",
    ));

    // History explorer
    out.push(hint(Normal, "h", Thread, invoke("open_history"), "History"));

    // Settings
    out.push(bind_screen(
        Normal,
        "s",
        Inbox,
        invoke("settings"),
        "Open settings",
    ));
    out.push(bind_screen(
        Normal,
        "Esc",
        Settings,
        invoke("inbox"),
        "Back to inbox",
    ));
    out.push(bind_screen(
        Normal,
        "q",
        Settings,
        invoke("inbox"),
        "Back to inbox",
    ));
    out.push(bind_screen(
        Normal,
        "Ctrl+c",
        Settings,
        invoke("quit"),
        "Quit",
    ));

    // Thread actions
    out.push(hint(
        Normal,
        "Enter",
        Inbox,
        invoke("open_selected"),
        "Open",
    ));
    out.push(bind_screen(
        Normal,
        "d",
        Inbox,
        invoke("archive_selected"),
        "Archive thread",
    ));

    // Approval quick keys (thread only)
    out.push(bind_screen(
        Normal,
        "y",
        Thread,
        invoke_with("approve", &[("decision", "allow_once")]),
        "Allow once",
    ));
    out.push(bind_screen(
        Normal,
        "n",
        Thread,
        invoke_with("approve", &[("decision", "deny_once")]),
        "Deny once",
    ));
    out.push(bind_screen(
        Normal,
        "s",
        Thread,
        invoke_with("approve", &[("decision", "allow_session")]),
        "Allow for session",
    ));
    out.push(bind_screen(
        Normal,
        "a",
        Thread,
        invoke_with("approve", &[("decision", "allow_always")]),
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
        invoke("select_next"),
        "Move selection down",
    ));
    out.push(bind_screen(
        Normal,
        "Down",
        History,
        invoke("select_next"),
        "Move selection down",
    ));
    out.push(bind_screen(
        Normal,
        "k",
        History,
        invoke("select_prev"),
        "Move selection up",
    ));
    out.push(bind_screen(
        Normal,
        "Up",
        History,
        invoke("select_prev"),
        "Move selection up",
    ));
    out.push(bind_screen(
        Normal,
        "g",
        History,
        invoke("select_first"),
        "Go to first",
    ));
    out.push(bind_screen(
        Normal,
        "G",
        History,
        invoke("select_last"),
        "Go to last",
    ));

    // Expand
    out.push(hint(
        Normal,
        "Enter",
        History,
        invoke("toggle_expand"),
        "Expand",
    ));
    out.push(bind_screen(
        Normal,
        " ",
        History,
        invoke("toggle_expand"),
        "Toggle expand",
    ));
    out.push(bind_screen(
        Normal,
        "e",
        History,
        invoke("expand_all"),
        "Expand all",
    ));
    out.push(bind_screen(
        Normal,
        "E",
        History,
        invoke("collapse_all"),
        "Collapse all",
    ));
    out.push(hint(
        Normal,
        "p",
        History,
        invoke("toggle_pretty"),
        "Pretty",
    ));
    out.push(hint(Normal, "f", History, invoke("toggle_full"), "Full"));

    // Page movement (moves selection, not just viewport)
    out.push(bind_screen(
        Normal,
        "d",
        History,
        invoke("select_half_page_down"),
        "Half page down",
    ));
    out.push(bind_screen(
        Normal,
        "Ctrl+d",
        History,
        invoke("select_half_page_down"),
        "Half page down",
    ));
    out.push(bind_screen(
        Normal,
        "u",
        History,
        invoke("select_half_page_up"),
        "Half page up",
    ));
    out.push(bind_screen(
        Normal,
        "Ctrl+u",
        History,
        invoke("select_half_page_up"),
        "Half page up",
    ));
    out.push(bind_screen(
        Normal,
        "Ctrl+f",
        History,
        invoke("select_page_down"),
        "Page down",
    ));
    out.push(bind_screen(
        Normal,
        "Ctrl+b",
        History,
        invoke("select_page_up"),
        "Page up",
    ));

    // Exit
    out.push(hint(
        Normal,
        "Esc",
        History,
        invoke("back_to_thread"),
        "Back",
    ));
    out.push(bind_screen(
        Normal,
        "q",
        History,
        invoke("back_to_thread"),
        "Back to thread",
    ));
    out.push(bind_screen(
        Normal,
        "h",
        History,
        invoke("back_to_thread"),
        "Back to thread",
    ));
    out.push(bind_screen(
        Normal,
        "Ctrl+c",
        History,
        invoke("back_to_thread"),
        "Back to thread",
    ));
}

// ---------------------------------------------------------------------------
// Insert mode
// ---------------------------------------------------------------------------

fn insert_mode(out: &mut Vec<Binding>) {
    out.push(bind(Insert, "Ctrl+s", invoke("send_input"), "Send"));
    out.push(bind(Insert, "Ctrl+Enter", invoke("send_input"), "Send"));
    out.push(bind(Insert, "Esc", invoke("exit_insert"), "Normal mode"));
    out.push(bind(Insert, "Ctrl+q", invoke("exit_insert"), "Normal mode"));
    // Ctrl+u: screen-specific because search mode handles its own clear
    out.push(bind_screen(
        Insert,
        "Ctrl+u",
        Inbox,
        invoke("clear_input"),
        "Clear line",
    ));
    out.push(bind_screen(
        Insert,
        "Ctrl+u",
        Thread,
        invoke("clear_input"),
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
        invoke("approval_select_next"),
        "Next option",
    ));
    out.push(bind(
        Approval,
        "Down",
        invoke("approval_select_next"),
        "Next option",
    ));
    out.push(bind(
        Approval,
        "k",
        invoke("approval_select_prev"),
        "Previous option",
    ));
    out.push(bind(
        Approval,
        "Up",
        invoke("approval_select_prev"),
        "Previous option",
    ));
    // Preview scroll
    out.push(bind(
        Approval,
        "Ctrl+j",
        invoke("approval_scroll_down"),
        "Scroll preview down",
    ));
    out.push(bind(
        Approval,
        "Ctrl+k",
        invoke("approval_scroll_up"),
        "Scroll preview up",
    ));

    out.push(bind(
        Approval,
        "y",
        invoke_with("approve", &[("decision", "allow_once")]),
        "Allow once",
    ));
    out.push(bind(
        Approval,
        "n",
        invoke_with("approve", &[("decision", "deny_once")]),
        "Deny once",
    ));
    out.push(bind(
        Approval,
        "s",
        invoke_with("approve", &[("decision", "allow_session")]),
        "Allow for session",
    ));
    out.push(bind(
        Approval,
        "a",
        invoke_with("approve", &[("decision", "allow_always")]),
        "Allow always",
    ));
    out.push(bind(
        Approval,
        "d",
        invoke_with("approve", &[("decision", "deny_always")]),
        "Deny always",
    ));
    // Enter confirms currently selected option
    out.push(bind(
        Approval,
        "Enter",
        invoke("approval_confirm"),
        "Confirm selected",
    ));
    // Esc closes thread (same as q)
    out.push(bind(Approval, "Escape", invoke("close"), "Close thread"));
    // Number keys for direct selection
    for (i, (_, decision)) in crate::types::APPROVAL_OPTIONS.iter().enumerate() {
        let key = format!("{}", i + 1);
        let decision_str = decision.as_str();
        out.push(bind(
            Approval,
            &key,
            invoke_with("approve", &[("decision", decision_str)]),
            &format!("Option {}", i + 1),
        ));
    }
    // q to close thread (falls through to normal dispatch)
    out.push(bind(Approval, "q", invoke("close"), "Close thread"));
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
