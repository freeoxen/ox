//! Default key binding table for the ox TUI.
//!
//! Encodes the current handle_normal_key / handle_insert_key /
//! handle_approval_key logic as declarative Binding structs.

use std::collections::BTreeMap;

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

fn bind(mode: &str, key: &str, action: Action, desc: &str) -> Binding {
    Binding {
        context: BindingContext {
            mode: mode.to_string(),
            key: key.to_string(),
            screen: None,
        },
        action,
        description: desc.to_string(),
    }
}

fn bind_screen(mode: &str, key: &str, screen: &str, action: Action, desc: &str) -> Binding {
    Binding {
        context: BindingContext {
            mode: mode.to_string(),
            key: key.to_string(),
            screen: Some(screen.to_string()),
        },
        action,
        description: desc.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Normal mode
// ---------------------------------------------------------------------------

fn normal_mode(out: &mut Vec<Binding>) {
    // Navigation — screen-specific
    out.push(bind_screen(
        "normal",
        "j",
        "inbox",
        invoke("select_next"),
        "Move selection down",
    ));
    out.push(bind_screen(
        "normal",
        "Down",
        "inbox",
        invoke("select_next"),
        "Move selection down",
    ));
    out.push(bind_screen(
        "normal",
        "k",
        "inbox",
        invoke("select_prev"),
        "Move selection up",
    ));
    out.push(bind_screen(
        "normal",
        "Up",
        "inbox",
        invoke("select_prev"),
        "Move selection up",
    ));

    out.push(bind_screen(
        "normal",
        "j",
        "thread",
        invoke("scroll_down"),
        "Scroll down",
    ));
    out.push(bind_screen(
        "normal",
        "Down",
        "thread",
        invoke("scroll_down"),
        "Scroll down",
    ));
    out.push(bind_screen(
        "normal",
        "k",
        "thread",
        invoke("scroll_up"),
        "Scroll up",
    ));
    out.push(bind_screen(
        "normal",
        "Up",
        "thread",
        invoke("scroll_up"),
        "Scroll up",
    ));

    // Screen transitions
    out.push(bind_screen(
        "normal",
        "Ctrl+c",
        "thread",
        invoke("close"),
        "Back to inbox",
    ));
    out.push(bind_screen(
        "normal",
        "Ctrl+c",
        "inbox",
        invoke("quit"),
        "Quit",
    ));
    out.push(bind_screen(
        "normal",
        "Esc",
        "thread",
        invoke("close"),
        "Back to inbox",
    ));
    out.push(bind_screen(
        "normal",
        "q",
        "thread",
        invoke("close"),
        "Back to inbox",
    ));
    out.push(bind_screen("normal", "q", "inbox", invoke("quit"), "Quit"));
    out.push(bind("normal", "Ctrl+t", invoke("close"), "Back to inbox"));

    // Enter insert mode — screen determines context
    out.push(bind_screen(
        "normal",
        "c",
        "inbox",
        invoke("compose"),
        "Compose new thread",
    ));
    out.push(bind_screen(
        "normal",
        "c",
        "thread",
        invoke("reply"),
        "Reply in thread",
    ));
    out.push(bind_screen(
        "normal",
        "/",
        "inbox",
        invoke("search"),
        "Search",
    ));

    // Command mode
    out.push(bind("normal", ":", invoke("enter_command"), "Command"));
    out.push(bind("normal", ";", invoke("enter_command"), "Command"));

    // -- Vim fast navigation --
    // g/G: go to top/bottom
    out.push(bind_screen(
        "normal",
        "g",
        "inbox",
        invoke("select_first"),
        "Go to first",
    ));
    out.push(bind_screen(
        "normal",
        "G",
        "inbox",
        invoke("select_last"),
        "Go to last",
    ));
    out.push(bind_screen(
        "normal",
        "g",
        "thread",
        invoke("scroll_to_top"),
        "Go to top",
    ));
    out.push(bind_screen(
        "normal",
        "G",
        "thread",
        invoke("scroll_to_bottom"),
        "Go to bottom",
    ));

    // Ctrl+d/u: half-page scroll
    out.push(bind_screen(
        "normal",
        "Ctrl+d",
        "thread",
        invoke("scroll_half_page_down"),
        "Half page down",
    ));
    out.push(bind_screen(
        "normal",
        "Ctrl+u",
        "thread",
        invoke("scroll_half_page_up"),
        "Half page up",
    ));

    // Ctrl+f/b: full page scroll
    out.push(bind_screen(
        "normal",
        "Ctrl+f",
        "thread",
        invoke("scroll_page_down"),
        "Page down",
    ));
    out.push(bind_screen(
        "normal",
        "Ctrl+b",
        "thread",
        invoke("scroll_page_up"),
        "Page up",
    ));

    // History explorer
    out.push(bind_screen(
        "normal",
        "h",
        "thread",
        invoke("open_history"),
        "History explorer",
    ));

    // Settings
    out.push(bind_screen(
        "normal",
        "s",
        "inbox",
        invoke("settings"),
        "Open settings",
    ));
    out.push(bind_screen(
        "normal",
        "Esc",
        "settings",
        invoke("inbox"),
        "Back to inbox",
    ));
    out.push(bind_screen(
        "normal",
        "q",
        "settings",
        invoke("inbox"),
        "Back to inbox",
    ));
    out.push(bind_screen(
        "normal",
        "Ctrl+c",
        "settings",
        invoke("quit"),
        "Quit",
    ));

    // Thread actions
    out.push(bind_screen(
        "normal",
        "Enter",
        "inbox",
        invoke("open_selected"),
        "Open thread",
    ));
    out.push(bind_screen(
        "normal",
        "d",
        "inbox",
        invoke("archive_selected"),
        "Archive thread",
    ));

    // Approval quick keys (thread only)
    out.push(bind_screen(
        "normal",
        "y",
        "thread",
        invoke_with("approve", &[("decision", "allow_once")]),
        "Allow once",
    ));
    out.push(bind_screen(
        "normal",
        "n",
        "thread",
        invoke_with("approve", &[("decision", "deny_once")]),
        "Deny once",
    ));
    out.push(bind_screen(
        "normal",
        "s",
        "thread",
        invoke_with("approve", &[("decision", "allow_session")]),
        "Allow for session",
    ));
    out.push(bind_screen(
        "normal",
        "a",
        "thread",
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
        "normal",
        "j",
        "history",
        invoke("select_next"),
        "Move selection down",
    ));
    out.push(bind_screen(
        "normal",
        "Down",
        "history",
        invoke("select_next"),
        "Move selection down",
    ));
    out.push(bind_screen(
        "normal",
        "k",
        "history",
        invoke("select_prev"),
        "Move selection up",
    ));
    out.push(bind_screen(
        "normal",
        "Up",
        "history",
        invoke("select_prev"),
        "Move selection up",
    ));
    out.push(bind_screen(
        "normal",
        "g",
        "history",
        invoke("select_first"),
        "Go to first",
    ));
    out.push(bind_screen(
        "normal",
        "G",
        "history",
        invoke("select_last"),
        "Go to last",
    ));

    // Expand
    out.push(bind_screen(
        "normal",
        "Enter",
        "history",
        invoke("toggle_expand"),
        "Toggle expand",
    ));
    out.push(bind_screen(
        "normal",
        " ",
        "history",
        invoke("toggle_expand"),
        "Toggle expand",
    ));
    out.push(bind_screen(
        "normal",
        "e",
        "history",
        invoke("expand_all"),
        "Expand all",
    ));
    out.push(bind_screen(
        "normal",
        "E",
        "history",
        invoke("collapse_all"),
        "Collapse all",
    ));

    // Page movement (moves selection, not just viewport)
    out.push(bind_screen(
        "normal",
        "d",
        "history",
        invoke("select_half_page_down"),
        "Half page down",
    ));
    out.push(bind_screen(
        "normal",
        "Ctrl+d",
        "history",
        invoke("select_half_page_down"),
        "Half page down",
    ));
    out.push(bind_screen(
        "normal",
        "u",
        "history",
        invoke("select_half_page_up"),
        "Half page up",
    ));
    out.push(bind_screen(
        "normal",
        "Ctrl+u",
        "history",
        invoke("select_half_page_up"),
        "Half page up",
    ));
    out.push(bind_screen(
        "normal",
        "Ctrl+f",
        "history",
        invoke("select_page_down"),
        "Page down",
    ));
    out.push(bind_screen(
        "normal",
        "Ctrl+b",
        "history",
        invoke("select_page_up"),
        "Page up",
    ));

    // Exit
    out.push(bind_screen(
        "normal",
        "Esc",
        "history",
        invoke("back_to_thread"),
        "Back to thread",
    ));
    out.push(bind_screen(
        "normal",
        "q",
        "history",
        invoke("back_to_thread"),
        "Back to thread",
    ));
    out.push(bind_screen(
        "normal",
        "h",
        "history",
        invoke("back_to_thread"),
        "Back to thread",
    ));
    out.push(bind_screen(
        "normal",
        "Ctrl+c",
        "history",
        invoke("back_to_thread"),
        "Back to thread",
    ));
}

// ---------------------------------------------------------------------------
// Insert mode
// ---------------------------------------------------------------------------

fn insert_mode(out: &mut Vec<Binding>) {
    out.push(bind("insert", "Ctrl+s", invoke("send_input"), "Send"));
    out.push(bind("insert", "Ctrl+Enter", invoke("send_input"), "Send"));
    out.push(bind("insert", "Esc", invoke("exit_insert"), "Normal mode"));
    out.push(bind(
        "insert",
        "Ctrl+q",
        invoke("exit_insert"),
        "Normal mode",
    ));
    // Ctrl+u: screen-specific because search mode handles its own clear
    out.push(bind_screen(
        "insert",
        "Ctrl+u",
        "inbox",
        invoke("clear_input"),
        "Clear line",
    ));
    out.push(bind_screen(
        "insert",
        "Ctrl+u",
        "thread",
        invoke("clear_input"),
        "Clear line",
    ));
}

// ---------------------------------------------------------------------------
// Approval mode
// ---------------------------------------------------------------------------

fn approval_mode(out: &mut Vec<Binding>) {
    out.push(bind("approval", "j", invoke("select_next"), "Next option"));
    out.push(bind(
        "approval",
        "Down",
        invoke("select_next"),
        "Next option",
    ));
    out.push(bind(
        "approval",
        "k",
        invoke("select_prev"),
        "Previous option",
    ));
    out.push(bind(
        "approval",
        "Up",
        invoke("select_prev"),
        "Previous option",
    ));

    out.push(bind(
        "approval",
        "y",
        invoke_with("approve", &[("decision", "allow_once")]),
        "Allow once",
    ));
    out.push(bind(
        "approval",
        "n",
        invoke_with("approve", &[("decision", "deny_once")]),
        "Deny once",
    ));
    out.push(bind(
        "approval",
        "s",
        invoke_with("approve", &[("decision", "allow_session")]),
        "Allow for session",
    ));
    out.push(bind(
        "approval",
        "a",
        invoke_with("approve", &[("decision", "allow_always")]),
        "Allow always",
    ));
    out.push(bind(
        "approval",
        "d",
        invoke_with("approve", &[("decision", "deny_always")]),
        "Deny always",
    ));
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
                b.context.mode == "normal"
                    && b.context.key == "j"
                    && b.context.screen.as_deref() == Some("inbox")
            })
            .collect();
        let j_thread: Vec<_> = bindings
            .iter()
            .filter(|b| {
                b.context.mode == "normal"
                    && b.context.key == "j"
                    && b.context.screen.as_deref() == Some("thread")
            })
            .collect();
        assert_eq!(j_inbox.len(), 1);
        assert_eq!(j_thread.len(), 1);
    }

    #[test]
    fn all_three_modes_have_bindings() {
        let bindings = default_bindings();
        assert!(bindings.iter().any(|b| b.context.mode == "normal"));
        assert!(bindings.iter().any(|b| b.context.mode == "insert"));
        assert!(bindings.iter().any(|b| b.context.mode == "approval"));
    }

    #[test]
    fn h_on_thread_opens_history() {
        let bindings = default_bindings();
        let found: Vec<_> = bindings
            .iter()
            .filter(|b| {
                b.context.mode == "normal"
                    && b.context.key == "h"
                    && b.context.screen.as_deref() == Some("thread")
            })
            .collect();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].description, "History explorer");
    }

    #[test]
    fn history_screen_has_bindings() {
        let bindings = default_bindings();
        let history_bindings: Vec<_> = bindings
            .iter()
            .filter(|b| b.context.screen.as_deref() == Some("history"))
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
