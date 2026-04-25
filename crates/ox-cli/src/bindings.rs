//! Default key binding table for the ox TUI.
//!
//! Encodes the current handle_normal_key / handle_insert_key /
//! handle_approval_key logic as declarative Binding structs.

use std::collections::BTreeMap;

use crossterm::event::{KeyCode, KeyModifiers};
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
    shortcuts_mode(&mut b);
    usage_mode(&mut b);
    thread_info_mode(&mut b);
    history_search_mode(&mut b);
    command_line_mode(&mut b);
    search_mode(&mut b);
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

/// Like [`invoke_with`] but with a single typed integer argument. Used
/// for commands whose registry declaration is `ParamKind::Integer` —
/// the registry rejects String values even if they parse as numbers.
fn invoke_with_int(command: CommandName, key: &str, value: i64) -> Action {
    let mut map = BTreeMap::new();
    map.insert(key.to_string(), serde_json::Value::from(value));
    Action::Invoke { command, args: map }
}

/// Encode key for binding registration. Panics if the key is not encodable
/// (e.g. F-keys) — this is a compile-time/startup check, not a runtime path.
fn key(code: KeyCode) -> String {
    crate::key_encode::encode_key(KeyModifiers::NONE, code)
        .unwrap_or_else(|| panic!("key {code:?} is not encodable"))
}

fn ctrl(code: KeyCode) -> String {
    crate::key_encode::encode_key(KeyModifiers::CONTROL, code)
        .unwrap_or_else(|| panic!("Ctrl+{code:?} is not encodable"))
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
use KeyCode::Char;
use Mode::{Approval, Insert, Normal};
use Screen::{History, Inbox, Settings, Thread};

// Pre-computed key strings for common keys
fn esc() -> String {
    key(KeyCode::Esc)
}
fn enter() -> String {
    key(KeyCode::Enter)
}
fn up() -> String {
    key(KeyCode::Up)
}
fn down() -> String {
    key(KeyCode::Down)
}

// ---------------------------------------------------------------------------
// Normal mode
// ---------------------------------------------------------------------------

fn normal_mode(out: &mut Vec<Binding>) {
    // Navigation — screen-specific
    out.push(bind_screen(
        Normal,
        &key(Char('j')),
        Inbox,
        invoke(Cmd::SelectNext),
        "Move selection down",
    ));
    out.push(bind_screen(
        Normal,
        &down(),
        Inbox,
        invoke(Cmd::SelectNext),
        "Move selection down",
    ));
    out.push(bind_screen(
        Normal,
        &key(Char('k')),
        Inbox,
        invoke(Cmd::SelectPrev),
        "Move selection up",
    ));
    out.push(bind_screen(
        Normal,
        &up(),
        Inbox,
        invoke(Cmd::SelectPrev),
        "Move selection up",
    ));

    out.push(bind_screen(
        Normal,
        &key(Char('j')),
        Thread,
        invoke(Cmd::ScrollDown),
        "Scroll down",
    ));
    out.push(bind_screen(
        Normal,
        &down(),
        Thread,
        invoke(Cmd::ScrollDown),
        "Scroll down",
    ));
    out.push(bind_screen(
        Normal,
        &key(Char('k')),
        Thread,
        invoke(Cmd::ScrollUp),
        "Scroll up",
    ));
    out.push(bind_screen(
        Normal,
        &up(),
        Thread,
        invoke(Cmd::ScrollUp),
        "Scroll up",
    ));

    // Screen transitions
    out.push(bind_screen(
        Normal,
        &ctrl(Char('c')),
        Thread,
        invoke(Cmd::Close),
        "Back to inbox",
    ));
    out.push(bind_screen(
        Normal,
        &ctrl(Char('c')),
        Inbox,
        invoke(Cmd::Quit),
        "Quit",
    ));
    out.push(hint(Normal, &esc(), Thread, invoke(Cmd::Close), "Back"));
    out.push(bind_screen(
        Normal,
        &key(Char('q')),
        Thread,
        invoke(Cmd::Close),
        "Back to inbox",
    ));
    out.push(bind_screen(
        Normal,
        &key(Char('q')),
        Inbox,
        invoke(Cmd::Quit),
        "Quit",
    ));
    out.push(bind(
        Normal,
        &ctrl(Char('t')),
        invoke(Cmd::Close),
        "Back to inbox",
    ));

    // Enter insert mode — screen determines context
    out.push(hint(
        Normal,
        &key(Char('c')),
        Inbox,
        invoke(Cmd::Compose),
        "Compose",
    ));
    out.push(hint(
        Normal,
        &key(Char('c')),
        Thread,
        invoke(Cmd::Reply),
        "Reply",
    ));
    // Enter on a thread row also opens the reply composer. Without this
    // binding `Normal+Thread+Enter` is a dead zone: any window where the
    // focus mode hasn't yet flipped to `Approval` (e.g. immediately after
    // re-entering a thread blocked on a tool-call decision) would
    // silently drop the keypress. The new `Approval` binding still wins
    // when an approval is actually pending — it sits higher in the focus
    // priority chain and therefore matches first.
    out.push(bind_screen(
        Normal,
        &enter(),
        Thread,
        invoke(Cmd::Reply),
        "Reply",
    ));
    out.push(hint(
        Normal,
        &key(Char('/')),
        Inbox,
        invoke(Cmd::Search),
        "Search",
    ));

    // Command line (global vim-style `:`). Works on every screen.
    out.push(bind(
        Normal,
        &key(Char(':')),
        invoke(Cmd::OpenCommandLine),
        "Command",
    ));
    out.push(bind(
        Normal,
        &key(Char(';')),
        invoke(Cmd::OpenCommandLine),
        "Command",
    ));

    // -- Vim fast navigation --
    // g/G: go to top/bottom
    out.push(bind_screen(
        Normal,
        &key(Char('g')),
        Inbox,
        invoke(Cmd::SelectFirst),
        "Go to first",
    ));
    out.push(bind_screen(
        Normal,
        &key(Char('G')),
        Inbox,
        invoke(Cmd::SelectLast),
        "Go to last",
    ));
    out.push(bind_screen(
        Normal,
        &key(Char('g')),
        Thread,
        invoke(Cmd::ScrollToTop),
        "Go to top",
    ));
    out.push(bind_screen(
        Normal,
        &key(Char('G')),
        Thread,
        invoke(Cmd::ScrollToBottom),
        "Go to bottom",
    ));

    // d/u and Ctrl+d/u: half-page scroll
    out.push(bind_screen(
        Normal,
        &key(Char('d')),
        Thread,
        invoke(Cmd::ScrollHalfPageDown),
        "Half page down",
    ));
    out.push(bind_screen(
        Normal,
        &ctrl(Char('d')),
        Thread,
        invoke(Cmd::ScrollHalfPageDown),
        "Half page down",
    ));
    out.push(bind_screen(
        Normal,
        &key(Char('u')),
        Thread,
        invoke(Cmd::ScrollHalfPageUp),
        "Half page up",
    ));
    out.push(bind_screen(
        Normal,
        &ctrl(Char('u')),
        Thread,
        invoke(Cmd::ScrollHalfPageUp),
        "Half page up",
    ));

    // Ctrl+f/b: full page scroll
    out.push(bind_screen(
        Normal,
        &ctrl(Char('f')),
        Thread,
        invoke(Cmd::ScrollPageDown),
        "Page down",
    ));
    out.push(bind_screen(
        Normal,
        &ctrl(Char('b')),
        Thread,
        invoke(Cmd::ScrollPageUp),
        "Page up",
    ));

    // History explorer
    out.push(hint(
        Normal,
        &key(Char('h')),
        Thread,
        invoke(Cmd::OpenHistory),
        "History",
    ));

    // Settings
    out.push(bind_screen(
        Normal,
        &key(Char('s')),
        Inbox,
        invoke(Cmd::Settings),
        "Open settings",
    ));
    out.push(bind_screen(
        Normal,
        &esc(),
        Settings,
        invoke(Cmd::Inbox),
        "Back to inbox",
    ));
    out.push(bind_screen(
        Normal,
        &key(Char('q')),
        Settings,
        invoke(Cmd::Inbox),
        "Back to inbox",
    ));
    out.push(bind_screen(
        Normal,
        &ctrl(Char('c')),
        Settings,
        invoke(Cmd::Quit),
        "Quit",
    ));

    // Thread actions
    out.push(hint(
        Normal,
        &enter(),
        Inbox,
        invoke(Cmd::OpenSelected),
        "Open",
    ));
    out.push(bind_screen(
        Normal,
        &key(Char('d')),
        Inbox,
        invoke(Cmd::ArchiveSelected),
        "Archive thread",
    ));

    // Modal toggles
    out.push(bind_screen(
        Normal,
        &key(Char('?')),
        Inbox,
        invoke(Cmd::ToggleShortcuts),
        "Show shortcuts",
    ));
    out.push(bind_screen(
        Normal,
        &key(Char('?')),
        Thread,
        invoke(Cmd::ToggleShortcuts),
        "Show shortcuts",
    ));
    out.push(bind_screen(
        Normal,
        &key(Char('$')),
        Inbox,
        invoke(Cmd::ToggleUsage),
        "Usage info",
    ));
    out.push(bind_screen(
        Normal,
        &key(Char('i')),
        Inbox,
        invoke(Cmd::ToggleThreadInfo),
        "Thread info",
    ));
    out.push(bind_screen(
        Normal,
        &key(Char('i')),
        Thread,
        invoke(Cmd::ToggleThreadInfo),
        "Thread info",
    ));
    out.push(bind_screen(
        Normal,
        &key(Char('$')),
        Thread,
        invoke(Cmd::ToggleUsage),
        "Usage info",
    ));

    // Approval quick keys (thread only)
    out.push(bind_screen(
        Normal,
        &key(Char('y')),
        Thread,
        invoke_with(Cmd::Approve, &[("decision", "allow_once")]),
        "Allow once",
    ));
    out.push(bind_screen(
        Normal,
        &key(Char('n')),
        Thread,
        invoke_with(Cmd::Approve, &[("decision", "deny_once")]),
        "Deny once",
    ));
    out.push(bind_screen(
        Normal,
        &key(Char('s')),
        Thread,
        invoke_with(Cmd::Approve, &[("decision", "allow_session")]),
        "Allow for session",
    ));
    out.push(bind_screen(
        Normal,
        &key(Char('a')),
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
        &key(Char('j')),
        History,
        invoke(Cmd::SelectNext),
        "Move selection down",
    ));
    out.push(bind_screen(
        Normal,
        &down(),
        History,
        invoke(Cmd::SelectNext),
        "Move selection down",
    ));
    out.push(bind_screen(
        Normal,
        &key(Char('k')),
        History,
        invoke(Cmd::SelectPrev),
        "Move selection up",
    ));
    out.push(bind_screen(
        Normal,
        &up(),
        History,
        invoke(Cmd::SelectPrev),
        "Move selection up",
    ));
    out.push(bind_screen(
        Normal,
        &key(Char('g')),
        History,
        invoke(Cmd::SelectFirst),
        "Go to first",
    ));
    out.push(bind_screen(
        Normal,
        &key(Char('G')),
        History,
        invoke(Cmd::SelectLast),
        "Go to last",
    ));

    // Expand
    out.push(hint(
        Normal,
        &enter(),
        History,
        invoke(Cmd::ToggleExpand),
        "Expand",
    ));
    out.push(bind_screen(
        Normal,
        &key(Char(' ')),
        History,
        invoke(Cmd::ToggleExpand),
        "Toggle expand",
    ));
    out.push(bind_screen(
        Normal,
        &key(Char('e')),
        History,
        invoke(Cmd::ExpandAll),
        "Expand all",
    ));
    out.push(bind_screen(
        Normal,
        &key(Char('E')),
        History,
        invoke(Cmd::CollapseAll),
        "Collapse all",
    ));
    out.push(hint(
        Normal,
        &key(Char('p')),
        History,
        invoke(Cmd::TogglePretty),
        "Pretty",
    ));
    out.push(hint(
        Normal,
        &key(Char('f')),
        History,
        invoke(Cmd::ToggleFull),
        "Full",
    ));

    // Page movement (moves selection, not just viewport)
    out.push(bind_screen(
        Normal,
        &key(Char('d')),
        History,
        invoke(Cmd::SelectHalfPageDown),
        "Half page down",
    ));
    out.push(bind_screen(
        Normal,
        &ctrl(Char('d')),
        History,
        invoke(Cmd::SelectHalfPageDown),
        "Half page down",
    ));
    out.push(bind_screen(
        Normal,
        &key(Char('u')),
        History,
        invoke(Cmd::SelectHalfPageUp),
        "Half page up",
    ));
    out.push(bind_screen(
        Normal,
        &ctrl(Char('u')),
        History,
        invoke(Cmd::SelectHalfPageUp),
        "Half page up",
    ));
    out.push(bind_screen(
        Normal,
        &ctrl(Char('f')),
        History,
        invoke(Cmd::SelectPageDown),
        "Page down",
    ));
    out.push(bind_screen(
        Normal,
        &ctrl(Char('b')),
        History,
        invoke(Cmd::SelectPageUp),
        "Page up",
    ));

    // Exit
    out.push(hint(
        Normal,
        &esc(),
        History,
        invoke(Cmd::BackToThread),
        "Back",
    ));
    out.push(bind_screen(
        Normal,
        &key(Char('q')),
        History,
        invoke(Cmd::BackToThread),
        "Back to thread",
    ));
    out.push(bind_screen(
        Normal,
        &key(Char('h')),
        History,
        invoke(Cmd::BackToThread),
        "Back to thread",
    ));
    out.push(bind_screen(
        Normal,
        &ctrl(Char('c')),
        History,
        invoke(Cmd::BackToThread),
        "Back to thread",
    ));
}

// ---------------------------------------------------------------------------
// Insert mode
// ---------------------------------------------------------------------------

fn insert_mode(out: &mut Vec<Binding>) {
    out.push(bind(
        Insert,
        &ctrl(Char('s')),
        invoke(Cmd::SendInput),
        "Send",
    ));
    out.push(bind(
        Insert,
        &ctrl(KeyCode::Enter),
        invoke(Cmd::SendInput),
        "Send",
    ));
    // ESC toggles editor sub-mode (Insert→Normal→exit)
    out.push(bind(
        Insert,
        &esc(),
        invoke(Cmd::ToggleEditorMode),
        "Toggle mode",
    ));
    out.push(bind(
        Insert,
        &ctrl(Char('q')),
        invoke(Cmd::ExitInsert),
        "Exit insert",
    ));
    // Ctrl+u: screen-specific because search mode handles its own clear
    out.push(bind_screen(
        Insert,
        &ctrl(Char('u')),
        Inbox,
        invoke(Cmd::ClearInput),
        "Clear line",
    ));
    out.push(bind_screen(
        Insert,
        &ctrl(Char('u')),
        Thread,
        invoke(Cmd::ClearInput),
        "Clear line",
    ));
    // Ctrl+R: enter history search (compose/reply only)
    out.push(bind_screen(
        Insert,
        &ctrl(Char('r')),
        Thread,
        invoke(Cmd::EnterHistorySearch),
        "History search",
    ));
    out.push(bind_screen(
        Insert,
        &ctrl(Char('r')),
        Inbox,
        invoke(Cmd::EnterHistorySearch),
        "History search",
    ));
    // Chip dismissal keys (1-9) in Normal+Inbox — chips are a persistent
    // view filter managed outside of Search mode.
    for i in 1..=9u8 {
        out.push(bind_screen(
            Normal,
            &i.to_string(),
            Inbox,
            invoke_with_int(Cmd::SearchDismissChip, "index", (i - 1) as i64),
            &format!("Dismiss chip {i}"),
        ));
    }
}

// ---------------------------------------------------------------------------
// Approval mode
// ---------------------------------------------------------------------------

fn approval_mode(out: &mut Vec<Binding>) {
    // Option navigation
    out.push(bind(
        Approval,
        &key(Char('j')),
        invoke(Cmd::ApprovalSelectNext),
        "Next option",
    ));
    out.push(bind(
        Approval,
        &down(),
        invoke(Cmd::ApprovalSelectNext),
        "Next option",
    ));
    out.push(bind(
        Approval,
        &key(Char('k')),
        invoke(Cmd::ApprovalSelectPrev),
        "Previous option",
    ));
    out.push(bind(
        Approval,
        &up(),
        invoke(Cmd::ApprovalSelectPrev),
        "Previous option",
    ));
    // Conversation scroll — the approval card is now inline at the
    // bottom of the transcript, so the user can scroll the whole
    // conversation (including the card body) before deciding. We
    // mirror the Normal/Thread scroll bindings here so they keep
    // working while focus is in Approval mode.
    out.push(bind(
        Approval,
        &ctrl(Char('j')),
        invoke(Cmd::ScrollDown),
        "Scroll down",
    ));
    out.push(bind(
        Approval,
        &ctrl(Char('k')),
        invoke(Cmd::ScrollUp),
        "Scroll up",
    ));
    out.push(bind(
        Approval,
        &ctrl(Char('d')),
        invoke(Cmd::ScrollHalfPageDown),
        "Half page down",
    ));
    out.push(bind(
        Approval,
        &ctrl(Char('u')),
        invoke(Cmd::ScrollHalfPageUp),
        "Half page up",
    ));
    out.push(bind(
        Approval,
        &ctrl(Char('f')),
        invoke(Cmd::ScrollPageDown),
        "Page down",
    ));
    out.push(bind(
        Approval,
        &ctrl(Char('b')),
        invoke(Cmd::ScrollPageUp),
        "Page up",
    ));
    out.push(bind(
        Approval,
        &key(Char('g')),
        invoke(Cmd::ScrollToTop),
        "Scroll to top",
    ));
    out.push(bind(
        Approval,
        &key(Char('G')),
        invoke(Cmd::ScrollToBottom),
        "Scroll to bottom",
    ));

    out.push(bind(
        Approval,
        &key(Char('y')),
        invoke_with(Cmd::Approve, &[("decision", "allow_once")]),
        "Allow once",
    ));
    out.push(bind(
        Approval,
        &key(Char('n')),
        invoke_with(Cmd::Approve, &[("decision", "deny_once")]),
        "Deny once",
    ));
    out.push(bind(
        Approval,
        &key(Char('s')),
        invoke_with(Cmd::Approve, &[("decision", "allow_session")]),
        "Allow for session",
    ));
    out.push(bind(
        Approval,
        &key(Char('a')),
        invoke_with(Cmd::Approve, &[("decision", "allow_always")]),
        "Allow always",
    ));
    out.push(bind(
        Approval,
        &key(Char('d')),
        invoke_with(Cmd::Approve, &[("decision", "deny_always")]),
        "Deny always",
    ));
    // Enter confirms currently selected option
    out.push(bind(
        Approval,
        &enter(),
        invoke(Cmd::ApprovalConfirm),
        "Confirm selected",
    ));
    // Esc closes thread (same as q)
    out.push(bind(Approval, &esc(), invoke(Cmd::Close), "Close thread"));
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
    out.push(bind(
        Approval,
        &key(Char('q')),
        invoke(Cmd::Close),
        "Close thread",
    ));
}

// ---------------------------------------------------------------------------
// Shortcuts modal mode
// ---------------------------------------------------------------------------

fn shortcuts_mode(out: &mut Vec<Binding>) {
    out.push(bind(
        Mode::Shortcuts,
        &key(Char('?')),
        invoke(Cmd::DismissShortcuts),
        "Close",
    ));
    out.push(bind(
        Mode::Shortcuts,
        &esc(),
        invoke(Cmd::DismissShortcuts),
        "Close",
    ));
    out.push(bind(
        Mode::Shortcuts,
        &ctrl(Char('q')),
        invoke(Cmd::DismissShortcuts),
        "Close",
    ));
}

// ---------------------------------------------------------------------------
// Usage dialog mode
// ---------------------------------------------------------------------------

fn usage_mode(out: &mut Vec<Binding>) {
    // Any key dismisses — Esc is the canonical one
    out.push(bind(
        Mode::Usage,
        &esc(),
        invoke(Cmd::DismissUsage),
        "Close",
    ));
}

// ---------------------------------------------------------------------------
// Thread info dialog mode
// ---------------------------------------------------------------------------

fn thread_info_mode(out: &mut Vec<Binding>) {
    out.push(bind(
        Mode::ThreadInfo,
        &esc(),
        invoke(Cmd::DismissThreadInfo),
        "Close",
    ));
    out.push(bind(
        Mode::ThreadInfo,
        &key(Char('i')),
        invoke(Cmd::DismissThreadInfo),
        "Close",
    ));
    out.push(bind(
        Mode::ThreadInfo,
        &key(Char('q')),
        invoke(Cmd::DismissThreadInfo),
        "Close",
    ));
    out.push(bind(
        Mode::ThreadInfo,
        &ctrl(Char('c')),
        invoke(Cmd::DismissThreadInfo),
        "Close",
    ));
}

// ---------------------------------------------------------------------------
// History search mode
// ---------------------------------------------------------------------------

fn history_search_mode(out: &mut Vec<Binding>) {
    out.push(bind(
        Mode::HistorySearch,
        &esc(),
        invoke(Cmd::DismissHistorySearch),
        "Cancel",
    ));
    out.push(bind(
        Mode::HistorySearch,
        &ctrl(Char('g')),
        invoke(Cmd::DismissHistorySearch),
        "Cancel",
    ));
    out.push(bind(
        Mode::HistorySearch,
        &enter(),
        invoke(Cmd::AcceptHistorySearch),
        "Accept",
    ));
    out.push(bind(
        Mode::HistorySearch,
        &ctrl(Char('r')),
        invoke(Cmd::HistorySearchCycle),
        "Next match",
    ));
}

// ---------------------------------------------------------------------------
// Search mode (the inbox `/` prompt). Unbound keys fall through to the
// Search-mode handler which writes SearchInsertChar. Enter commits the
// live query as a chip; Esc closes the mode and drops the query.
// ---------------------------------------------------------------------------

fn search_mode(out: &mut Vec<Binding>) {
    out.push(bind(
        Mode::Search,
        &esc(),
        invoke(Cmd::SearchClose),
        "Close",
    ));
    out.push(bind(
        Mode::Search,
        &ctrl(Char('c')),
        invoke(Cmd::SearchClose),
        "Close",
    ));
    out.push(bind(
        Mode::Search,
        &enter(),
        invoke(Cmd::SearchSaveChip),
        "Save filter",
    ));
    out.push(bind(
        Mode::Search,
        &key(KeyCode::Backspace),
        invoke(Cmd::SearchDeleteChar),
        "Delete",
    ));
    out.push(bind(
        Mode::Search,
        &ctrl(Char('u')),
        invoke(Cmd::SearchClear),
        "Clear query",
    ));
}

// ---------------------------------------------------------------------------
// Command-line mode (the global `:` prompt). Text editing keys fall
// through to the unbound handler, which writes edits to
// `ui/command_line/edit`. Only the control keys are bound.
// ---------------------------------------------------------------------------

fn command_line_mode(out: &mut Vec<Binding>) {
    out.push(bind(
        Mode::Command,
        &esc(),
        invoke(Cmd::CloseCommandLine),
        "Cancel",
    ));
    out.push(bind(
        Mode::Command,
        &ctrl(Char('c')),
        invoke(Cmd::CloseCommandLine),
        "Cancel",
    ));
    out.push(bind(
        Mode::Command,
        &enter(),
        invoke(Cmd::SubmitCommandLine),
        "Run",
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
