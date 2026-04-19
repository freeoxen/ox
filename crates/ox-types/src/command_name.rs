//! Typed command names — compile-time checked identifiers for all built-in commands.

use serde::{Deserialize, Serialize};

/// Every built-in command in the system.
///
/// Using this enum instead of raw strings ensures that binding registrations
/// and command invocations are checked at compile time. A typo in a command
/// name is a compiler error, not a silent runtime no-match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandName {
    // -- Navigation --
    SelectNext,
    SelectPrev,
    SelectFirst,
    SelectLast,
    ScrollUp,
    ScrollDown,
    ScrollToTop,
    ScrollToBottom,
    ScrollPageUp,
    ScrollPageDown,
    ScrollHalfPageUp,
    ScrollHalfPageDown,

    // -- Screen transitions --
    Open,
    Close,
    Settings,
    Inbox,
    OpenSelected,
    Quit,

    // -- Mode transitions --
    Compose,
    Reply,
    Search,
    ExitInsert,

    // -- Command line (global vim-style `:`) --
    OpenCommandLine,
    CloseCommandLine,
    SubmitCommandLine,

    // -- Text input --
    SendInput,
    ClearInput,

    // -- Thread actions --
    ArchiveSelected,

    // -- Search --
    SearchClose,
    SearchInsertChar,
    SearchDeleteChar,
    SearchClear,
    SearchSaveChip,
    SearchDismissChip,

    // -- Modals --
    ShowModal,
    DismissModal,

    // -- Approval --
    Approve,
    ApprovalConfirm,
    ApprovalSelectNext,
    ApprovalSelectPrev,
    ApprovalScrollDown,
    ApprovalScrollUp,

    // -- Internal --
    SetRowCount,
    SetScrollMax,
    SetViewportHeight,
    SetInput,
    SetStatus,
    ClearPendingAction,

    // -- Modal dialogs --
    ToggleShortcuts,
    DismissShortcuts,
    DismissUsage,
    ToggleUsage,

    // -- History search --
    EnterHistorySearch,
    HistorySearchCycle,
    AcceptHistorySearch,
    DismissHistorySearch,

    // -- Editor sub-modes --
    ToggleEditorMode,

    // -- History explorer --
    OpenHistory,
    BackToThread,
    ToggleExpand,
    ExpandAll,
    CollapseAll,
    TogglePretty,
    ToggleFull,
    SelectPageUp,
    SelectPageDown,
    SelectHalfPageUp,
    SelectHalfPageDown,
}

impl CommandName {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SelectNext => "select_next",
            Self::SelectPrev => "select_prev",
            Self::SelectFirst => "select_first",
            Self::SelectLast => "select_last",
            Self::ScrollUp => "scroll_up",
            Self::ScrollDown => "scroll_down",
            Self::ScrollToTop => "scroll_to_top",
            Self::ScrollToBottom => "scroll_to_bottom",
            Self::ScrollPageUp => "scroll_page_up",
            Self::ScrollPageDown => "scroll_page_down",
            Self::ScrollHalfPageUp => "scroll_half_page_up",
            Self::ScrollHalfPageDown => "scroll_half_page_down",
            Self::Open => "open",
            Self::Close => "close",
            Self::Settings => "settings",
            Self::Inbox => "inbox",
            Self::OpenSelected => "open_selected",
            Self::Quit => "quit",
            Self::Compose => "compose",
            Self::Reply => "reply",
            Self::Search => "search",
            Self::ExitInsert => "exit_insert",
            Self::OpenCommandLine => "open_command_line",
            Self::CloseCommandLine => "close_command_line",
            Self::SubmitCommandLine => "submit_command_line",
            Self::SendInput => "send_input",
            Self::ClearInput => "clear_input",
            Self::ArchiveSelected => "archive_selected",
            Self::SearchClose => "search_close",
            Self::SearchInsertChar => "search_insert_char",
            Self::SearchDeleteChar => "search_delete_char",
            Self::SearchClear => "search_clear",
            Self::SearchSaveChip => "search_save_chip",
            Self::SearchDismissChip => "search_dismiss_chip",
            Self::ShowModal => "show_modal",
            Self::DismissModal => "dismiss_modal",
            Self::Approve => "approve",
            Self::ApprovalConfirm => "approval_confirm",
            Self::ApprovalSelectNext => "approval_select_next",
            Self::ApprovalSelectPrev => "approval_select_prev",
            Self::ApprovalScrollDown => "approval_scroll_down",
            Self::ApprovalScrollUp => "approval_scroll_up",
            Self::SetRowCount => "set_row_count",
            Self::SetScrollMax => "set_scroll_max",
            Self::SetViewportHeight => "set_viewport_height",
            Self::SetInput => "set_input",
            Self::SetStatus => "set_status",
            Self::ClearPendingAction => "clear_pending_action",
            Self::ToggleShortcuts => "toggle_shortcuts",
            Self::DismissShortcuts => "dismiss_shortcuts",
            Self::DismissUsage => "dismiss_usage",
            Self::ToggleUsage => "toggle_usage",
            Self::EnterHistorySearch => "enter_history_search",
            Self::HistorySearchCycle => "history_search_cycle",
            Self::AcceptHistorySearch => "accept_history_search",
            Self::DismissHistorySearch => "dismiss_history_search",
            Self::ToggleEditorMode => "toggle_editor_mode",
            Self::OpenHistory => "open_history",
            Self::BackToThread => "back_to_thread",
            Self::ToggleExpand => "toggle_expand",
            Self::ExpandAll => "expand_all",
            Self::CollapseAll => "collapse_all",
            Self::TogglePretty => "toggle_pretty",
            Self::ToggleFull => "toggle_full",
            Self::SelectPageUp => "select_page_up",
            Self::SelectPageDown => "select_page_down",
            Self::SelectHalfPageUp => "select_half_page_up",
            Self::SelectHalfPageDown => "select_half_page_down",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "select_next" => Some(Self::SelectNext),
            "select_prev" => Some(Self::SelectPrev),
            "select_first" => Some(Self::SelectFirst),
            "select_last" => Some(Self::SelectLast),
            "scroll_up" => Some(Self::ScrollUp),
            "scroll_down" => Some(Self::ScrollDown),
            "scroll_to_top" => Some(Self::ScrollToTop),
            "scroll_to_bottom" => Some(Self::ScrollToBottom),
            "scroll_page_up" => Some(Self::ScrollPageUp),
            "scroll_page_down" => Some(Self::ScrollPageDown),
            "scroll_half_page_up" => Some(Self::ScrollHalfPageUp),
            "scroll_half_page_down" => Some(Self::ScrollHalfPageDown),
            "open" => Some(Self::Open),
            "close" => Some(Self::Close),
            "settings" => Some(Self::Settings),
            "inbox" => Some(Self::Inbox),
            "open_selected" => Some(Self::OpenSelected),
            "quit" => Some(Self::Quit),
            "compose" => Some(Self::Compose),
            "reply" => Some(Self::Reply),
            "search" => Some(Self::Search),
            "exit_insert" => Some(Self::ExitInsert),
            "open_command_line" => Some(Self::OpenCommandLine),
            "close_command_line" => Some(Self::CloseCommandLine),
            "submit_command_line" => Some(Self::SubmitCommandLine),
            "send_input" => Some(Self::SendInput),
            "clear_input" => Some(Self::ClearInput),
            "archive_selected" => Some(Self::ArchiveSelected),
            "search_close" => Some(Self::SearchClose),
            "search_insert_char" => Some(Self::SearchInsertChar),
            "search_delete_char" => Some(Self::SearchDeleteChar),
            "search_clear" => Some(Self::SearchClear),
            "search_save_chip" => Some(Self::SearchSaveChip),
            "search_dismiss_chip" => Some(Self::SearchDismissChip),
            "show_modal" => Some(Self::ShowModal),
            "dismiss_modal" => Some(Self::DismissModal),
            "approve" => Some(Self::Approve),
            "approval_confirm" => Some(Self::ApprovalConfirm),
            "approval_select_next" => Some(Self::ApprovalSelectNext),
            "approval_select_prev" => Some(Self::ApprovalSelectPrev),
            "approval_scroll_down" => Some(Self::ApprovalScrollDown),
            "approval_scroll_up" => Some(Self::ApprovalScrollUp),
            "set_row_count" => Some(Self::SetRowCount),
            "set_scroll_max" => Some(Self::SetScrollMax),
            "set_viewport_height" => Some(Self::SetViewportHeight),
            "set_input" => Some(Self::SetInput),
            "set_status" => Some(Self::SetStatus),
            "clear_pending_action" => Some(Self::ClearPendingAction),
            "toggle_shortcuts" => Some(Self::ToggleShortcuts),
            "dismiss_shortcuts" => Some(Self::DismissShortcuts),
            "dismiss_usage" => Some(Self::DismissUsage),
            "toggle_usage" => Some(Self::ToggleUsage),
            "enter_history_search" => Some(Self::EnterHistorySearch),
            "history_search_cycle" => Some(Self::HistorySearchCycle),
            "accept_history_search" => Some(Self::AcceptHistorySearch),
            "dismiss_history_search" => Some(Self::DismissHistorySearch),
            "toggle_editor_mode" => Some(Self::ToggleEditorMode),
            "open_history" => Some(Self::OpenHistory),
            "back_to_thread" => Some(Self::BackToThread),
            "toggle_expand" => Some(Self::ToggleExpand),
            "expand_all" => Some(Self::ExpandAll),
            "collapse_all" => Some(Self::CollapseAll),
            "toggle_pretty" => Some(Self::TogglePretty),
            "toggle_full" => Some(Self::ToggleFull),
            "select_page_up" => Some(Self::SelectPageUp),
            "select_page_down" => Some(Self::SelectPageDown),
            "select_half_page_up" => Some(Self::SelectHalfPageUp),
            "select_half_page_down" => Some(Self::SelectHalfPageDown),
            _ => None,
        }
    }
}

impl std::fmt::Display for CommandName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_all_variants() {
        let variants = [
            CommandName::SelectNext,
            CommandName::SelectPrev,
            CommandName::Close,
            CommandName::Quit,
            CommandName::Approve,
            CommandName::ApprovalConfirm,
            CommandName::ToggleExpand,
            CommandName::SelectHalfPageDown,
        ];
        for v in variants {
            let s = v.as_str();
            let parsed = CommandName::parse(s).unwrap_or_else(|| panic!("parse failed for {s}"));
            assert_eq!(parsed, v);
        }
    }

    #[test]
    fn parse_unknown_returns_none() {
        assert_eq!(CommandName::parse("nonexistent"), None);
    }

    #[test]
    fn serde_round_trip() {
        let cmd = CommandName::Approve;
        let json = serde_json::to_string(&cmd).unwrap();
        assert_eq!(json, "\"approve\"");
        let back: CommandName = serde_json::from_str(&json).unwrap();
        assert_eq!(back, CommandName::Approve);
    }
}
