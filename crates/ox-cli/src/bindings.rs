//! Default key binding table for the ox TUI.
//!
//! Encodes the current handle_normal_key / handle_insert_key /
//! handle_approval_key logic as declarative Binding structs.

use ox_ui::{Action, ActionField, Binding, BindingContext};
use structfs_core_store::{Path, Value};

/// Build the default binding table.
pub fn default_bindings() -> Vec<Binding> {
    let mut b = Vec::new();
    normal_mode(&mut b);
    insert_mode(&mut b);
    approval_mode(&mut b);
    b
}

fn p(s: &str) -> Path {
    Path::parse(s).expect("binding target must be valid path")
}

fn cmd(target: &str) -> Action {
    Action::Command {
        target: p(target),
        fields: vec![],
    }
}

fn cmd_with(target: &str, fields: Vec<ActionField>) -> Action {
    Action::Command {
        target: p(target),
        fields,
    }
}

fn static_field(key: &str, val: &str) -> ActionField {
    ActionField::Static {
        key: key.to_string(),
        value: Value::String(val.to_string()),
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
        cmd("ui/select_next"),
        "Move selection down",
    ));
    out.push(bind_screen(
        "normal",
        "Down",
        "inbox",
        cmd("ui/select_next"),
        "Move selection down",
    ));
    out.push(bind_screen(
        "normal",
        "k",
        "inbox",
        cmd("ui/select_prev"),
        "Move selection up",
    ));
    out.push(bind_screen(
        "normal",
        "Up",
        "inbox",
        cmd("ui/select_prev"),
        "Move selection up",
    ));

    out.push(bind_screen(
        "normal",
        "j",
        "thread",
        cmd("ui/scroll_down"),
        "Scroll down",
    ));
    out.push(bind_screen(
        "normal",
        "Down",
        "thread",
        cmd("ui/scroll_down"),
        "Scroll down",
    ));
    out.push(bind_screen(
        "normal",
        "k",
        "thread",
        cmd("ui/scroll_up"),
        "Scroll up",
    ));
    out.push(bind_screen(
        "normal",
        "Up",
        "thread",
        cmd("ui/scroll_up"),
        "Scroll up",
    ));

    // Screen transitions
    out.push(bind_screen(
        "normal",
        "Ctrl+c",
        "thread",
        cmd("ui/close"),
        "Back to inbox",
    ));
    out.push(bind_screen(
        "normal",
        "Ctrl+c",
        "inbox",
        cmd("ui/quit"),
        "Quit",
    ));
    out.push(bind_screen(
        "normal",
        "Esc",
        "thread",
        cmd("ui/close"),
        "Back to inbox",
    ));
    out.push(bind_screen(
        "normal",
        "q",
        "thread",
        cmd("ui/close"),
        "Back to inbox",
    ));
    out.push(bind_screen("normal", "q", "inbox", cmd("ui/quit"), "Quit"));
    out.push(bind("normal", "Ctrl+t", cmd("ui/close"), "Back to inbox"));

    // Enter insert mode — screen determines context
    out.push(bind_screen(
        "normal",
        "c",
        "inbox",
        cmd_with("ui/enter_insert", vec![static_field("context", "compose")]),
        "Compose new thread",
    ));
    out.push(bind_screen(
        "normal",
        "c",
        "thread",
        cmd_with("ui/enter_insert", vec![static_field("context", "reply")]),
        "Reply in thread",
    ));
    out.push(bind_screen(
        "normal",
        "/",
        "inbox",
        cmd_with("ui/enter_insert", vec![static_field("context", "search")]),
        "Search",
    ));

    // Command mode
    out.push(bind(
        "normal",
        ":",
        cmd_with("ui/enter_insert", vec![static_field("context", "command")]),
        "Command",
    ));
    out.push(bind(
        "normal",
        ";",
        cmd_with("ui/enter_insert", vec![static_field("context", "command")]),
        "Command",
    ));

    // -- Vim fast navigation --
    // g/G: go to top/bottom
    out.push(bind_screen(
        "normal",
        "g",
        "inbox",
        cmd("ui/select_first"),
        "Go to first",
    ));
    out.push(bind_screen(
        "normal",
        "G",
        "inbox",
        cmd("ui/select_last"),
        "Go to last",
    ));
    out.push(bind_screen(
        "normal",
        "g",
        "thread",
        cmd("ui/scroll_to_top"),
        "Go to top",
    ));
    out.push(bind_screen(
        "normal",
        "G",
        "thread",
        cmd("ui/scroll_to_bottom"),
        "Go to bottom",
    ));

    // Ctrl+d/u: half-page scroll
    out.push(bind_screen(
        "normal",
        "Ctrl+d",
        "thread",
        cmd("ui/scroll_half_page_down"),
        "Half page down",
    ));
    out.push(bind_screen(
        "normal",
        "Ctrl+u",
        "thread",
        cmd("ui/scroll_half_page_up"),
        "Half page up",
    ));

    // Ctrl+f/b: full page scroll
    out.push(bind_screen(
        "normal",
        "Ctrl+f",
        "thread",
        cmd("ui/scroll_page_down"),
        "Page down",
    ));
    out.push(bind_screen(
        "normal",
        "Ctrl+b",
        "thread",
        cmd("ui/scroll_page_up"),
        "Page up",
    ));

    // Settings
    out.push(bind_screen(
        "normal",
        "s",
        "inbox",
        cmd("ui/go_to_settings"),
        "Open settings",
    ));
    out.push(bind_screen(
        "normal",
        "Esc",
        "settings",
        cmd("ui/go_to_inbox"),
        "Back to inbox",
    ));
    out.push(bind_screen(
        "normal",
        "q",
        "settings",
        cmd("ui/go_to_inbox"),
        "Back to inbox",
    ));
    out.push(bind_screen(
        "normal",
        "Ctrl+c",
        "settings",
        cmd("ui/quit"),
        "Quit",
    ));

    // Thread actions
    out.push(bind_screen(
        "normal",
        "Enter",
        "inbox",
        cmd("ui/open_selected"),
        "Open thread",
    ));
    out.push(bind_screen(
        "normal",
        "d",
        "inbox",
        cmd("ui/archive_selected"),
        "Archive thread",
    ));

    // Approval quick keys (thread only)
    out.push(bind_screen(
        "normal",
        "y",
        "thread",
        cmd_with(
            "approval/response",
            vec![static_field("decision", "allow_once")],
        ),
        "Allow once",
    ));
    out.push(bind_screen(
        "normal",
        "n",
        "thread",
        cmd_with(
            "approval/response",
            vec![static_field("decision", "deny_once")],
        ),
        "Deny once",
    ));
    out.push(bind_screen(
        "normal",
        "s",
        "thread",
        cmd_with(
            "approval/response",
            vec![static_field("decision", "allow_session")],
        ),
        "Allow for session",
    ));
    out.push(bind_screen(
        "normal",
        "a",
        "thread",
        cmd_with(
            "approval/response",
            vec![static_field("decision", "allow_always")],
        ),
        "Allow always",
    ));
}

// ---------------------------------------------------------------------------
// Insert mode
// ---------------------------------------------------------------------------

fn insert_mode(out: &mut Vec<Binding>) {
    out.push(bind("insert", "Ctrl+s", cmd("ui/send_input"), "Send"));
    out.push(bind("insert", "Ctrl+Enter", cmd("ui/send_input"), "Send"));
    out.push(bind(
        "insert",
        "Ctrl+q",
        cmd("ui/exit_insert"),
        "Exit insert mode",
    ));
    // Ctrl+u: screen-specific because search mode handles its own clear
    out.push(bind_screen(
        "insert",
        "Ctrl+u",
        "inbox",
        cmd("ui/clear_input"),
        "Clear line",
    ));
    out.push(bind_screen(
        "insert",
        "Ctrl+u",
        "thread",
        cmd("ui/clear_input"),
        "Clear line",
    ));
}

// ---------------------------------------------------------------------------
// Approval mode
// ---------------------------------------------------------------------------

fn approval_mode(out: &mut Vec<Binding>) {
    out.push(bind("approval", "j", cmd("ui/select_next"), "Next option"));
    out.push(bind(
        "approval",
        "Down",
        cmd("ui/select_next"),
        "Next option",
    ));
    out.push(bind(
        "approval",
        "k",
        cmd("ui/select_prev"),
        "Previous option",
    ));
    out.push(bind(
        "approval",
        "Up",
        cmd("ui/select_prev"),
        "Previous option",
    ));

    out.push(bind(
        "approval",
        "y",
        cmd_with(
            "approval/response",
            vec![static_field("decision", "allow_once")],
        ),
        "Allow once",
    ));
    out.push(bind(
        "approval",
        "n",
        cmd_with(
            "approval/response",
            vec![static_field("decision", "deny_once")],
        ),
        "Deny once",
    ));
    out.push(bind(
        "approval",
        "s",
        cmd_with(
            "approval/response",
            vec![static_field("decision", "allow_session")],
        ),
        "Allow for session",
    ));
    out.push(bind(
        "approval",
        "a",
        cmd_with(
            "approval/response",
            vec![static_field("decision", "allow_always")],
        ),
        "Allow always",
    ));
    out.push(bind(
        "approval",
        "d",
        cmd_with(
            "approval/response",
            vec![static_field("decision", "deny_always")],
        ),
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
