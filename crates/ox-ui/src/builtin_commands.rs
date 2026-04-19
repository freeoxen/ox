//! Built-in command catalog — every action the system can perform.

use crate::command_def::{StaticCommandDef, StaticParamDef, StaticParamKind};

/// Returns the complete built-in command catalog.
pub fn builtin_commands() -> &'static [StaticCommandDef] {
    BUILTIN_COMMANDS
}

static BUILTIN_COMMANDS: &[StaticCommandDef] = &[
    // -- Navigation --
    StaticCommandDef {
        name: "select_next",
        target: "ui/select_next",
        params: &[],
        description: "Move selection down",
        user_facing: true,
    },
    StaticCommandDef {
        name: "select_prev",
        target: "ui/select_prev",
        params: &[],
        description: "Move selection up",
        user_facing: true,
    },
    StaticCommandDef {
        name: "select_first",
        target: "ui/select_first",
        params: &[],
        description: "Jump to first item",
        user_facing: true,
    },
    StaticCommandDef {
        name: "select_last",
        target: "ui/select_last",
        params: &[],
        description: "Jump to last item",
        user_facing: true,
    },
    StaticCommandDef {
        name: "scroll_up",
        target: "ui/scroll_up",
        params: &[],
        description: "Scroll viewport up",
        user_facing: true,
    },
    StaticCommandDef {
        name: "scroll_down",
        target: "ui/scroll_down",
        params: &[],
        description: "Scroll viewport down",
        user_facing: true,
    },
    StaticCommandDef {
        name: "scroll_to_top",
        target: "ui/scroll_to_top",
        params: &[],
        description: "Scroll to top",
        user_facing: true,
    },
    StaticCommandDef {
        name: "scroll_to_bottom",
        target: "ui/scroll_to_bottom",
        params: &[],
        description: "Scroll to bottom",
        user_facing: true,
    },
    StaticCommandDef {
        name: "scroll_page_up",
        target: "ui/scroll_page_up",
        params: &[],
        description: "Scroll one page up",
        user_facing: true,
    },
    StaticCommandDef {
        name: "scroll_page_down",
        target: "ui/scroll_page_down",
        params: &[],
        description: "Scroll one page down",
        user_facing: true,
    },
    StaticCommandDef {
        name: "scroll_half_page_up",
        target: "ui/scroll_half_page_up",
        params: &[],
        description: "Scroll half page up",
        user_facing: true,
    },
    StaticCommandDef {
        name: "scroll_half_page_down",
        target: "ui/scroll_half_page_down",
        params: &[],
        description: "Scroll half page down",
        user_facing: true,
    },
    // -- Screen transitions --
    StaticCommandDef {
        name: "open",
        target: "ui/open",
        params: &[StaticParamDef {
            name: "thread_id",
            kind: StaticParamKind::String,
            required: true,
            default: None,
        }],
        description: "Open a thread",
        user_facing: true,
    },
    StaticCommandDef {
        name: "close",
        target: "ui/close",
        params: &[],
        description: "Back to inbox",
        user_facing: true,
    },
    StaticCommandDef {
        name: "settings",
        target: "ui/go_to_settings",
        params: &[],
        description: "Open settings screen",
        user_facing: true,
    },
    StaticCommandDef {
        name: "inbox",
        target: "ui/go_to_inbox",
        params: &[],
        description: "Return to inbox",
        user_facing: true,
    },
    StaticCommandDef {
        name: "open_selected",
        target: "ui/open_selected",
        params: &[],
        description: "Open currently selected thread",
        user_facing: true,
    },
    StaticCommandDef {
        name: "quit",
        target: "ui/quit",
        params: &[],
        description: "Quit the application",
        user_facing: true,
    },
    // -- Mode transitions --
    StaticCommandDef {
        name: "compose",
        target: "ui/enter_insert",
        params: &[StaticParamDef {
            name: "context",
            kind: StaticParamKind::Enum(&["compose", "reply", "search", "command"]),
            required: true,
            default: Some("compose"),
        }],
        description: "Open compose input",
        user_facing: true,
    },
    StaticCommandDef {
        name: "reply",
        target: "ui/enter_insert",
        params: &[StaticParamDef {
            name: "context",
            kind: StaticParamKind::Enum(&["compose", "reply", "search", "command"]),
            required: true,
            default: Some("reply"),
        }],
        description: "Open reply input",
        user_facing: true,
    },
    StaticCommandDef {
        name: "search",
        target: "ui/enter_insert",
        params: &[StaticParamDef {
            name: "context",
            kind: StaticParamKind::Enum(&["compose", "reply", "search", "command"]),
            required: true,
            default: Some("search"),
        }],
        description: "Open search input",
        user_facing: true,
    },
    StaticCommandDef {
        name: "enter_command",
        target: "ui/enter_insert",
        params: &[StaticParamDef {
            name: "context",
            kind: StaticParamKind::Enum(&["compose", "reply", "search", "command"]),
            required: true,
            default: Some("command"),
        }],
        description: "Open command line",
        user_facing: true,
    },
    StaticCommandDef {
        name: "exit_insert",
        target: "ui/exit_insert",
        params: &[],
        description: "Exit insert mode",
        user_facing: true,
    },
    // -- Command line (global vim-style `:`) --
    StaticCommandDef {
        name: "open_command_line",
        target: "ui/command_line/open",
        params: &[],
        description: "Open the global command line",
        user_facing: true,
    },
    StaticCommandDef {
        name: "close_command_line",
        target: "ui/command_line/close",
        params: &[],
        description: "Close the global command line",
        user_facing: true,
    },
    StaticCommandDef {
        name: "submit_command_line",
        target: "ui/command_line/submit",
        params: &[],
        description: "Submit the global command line",
        user_facing: true,
    },
    // -- Text input --
    StaticCommandDef {
        name: "send_input",
        target: "ui/send_input",
        params: &[],
        description: "Send current input",
        user_facing: true,
    },
    StaticCommandDef {
        name: "clear_input",
        target: "ui/clear_input",
        params: &[],
        description: "Clear input buffer",
        user_facing: true,
    },
    // -- Thread actions --
    StaticCommandDef {
        name: "archive_selected",
        target: "ui/archive_selected",
        params: &[],
        description: "Archive selected thread",
        user_facing: true,
    },
    // -- Search --
    StaticCommandDef {
        name: "search_insert_char",
        target: "ui/search_insert_char",
        params: &[StaticParamDef {
            name: "char",
            kind: StaticParamKind::String,
            required: true,
            default: None,
        }],
        description: "Append to search query",
        user_facing: false,
    },
    StaticCommandDef {
        name: "search_delete_char",
        target: "ui/search_delete_char",
        params: &[],
        description: "Delete last search char",
        user_facing: false,
    },
    StaticCommandDef {
        name: "search_clear",
        target: "ui/search_clear",
        params: &[],
        description: "Clear search query",
        user_facing: true,
    },
    StaticCommandDef {
        name: "search_save_chip",
        target: "ui/search_save_chip",
        params: &[],
        description: "Save query as search chip",
        user_facing: false,
    },
    StaticCommandDef {
        name: "search_dismiss_chip",
        target: "ui/search_dismiss_chip",
        params: &[StaticParamDef {
            name: "index",
            kind: StaticParamKind::Integer,
            required: true,
            default: None,
        }],
        description: "Remove a search chip",
        user_facing: false,
    },
    // -- Modals --
    StaticCommandDef {
        name: "show_modal",
        target: "ui/show_modal",
        params: &[],
        description: "Show a modal dialog",
        user_facing: false,
    },
    StaticCommandDef {
        name: "dismiss_modal",
        target: "ui/dismiss_modal",
        params: &[],
        description: "Dismiss current modal",
        user_facing: true,
    },
    // -- Approval --
    StaticCommandDef {
        name: "approve",
        target: "ui/approve",
        params: &[StaticParamDef {
            name: "decision",
            kind: StaticParamKind::Enum(&[
                "allow_once",
                "deny_once",
                "allow_session",
                "allow_always",
                "deny_always",
                "deny_session",
            ]),
            required: true,
            default: None,
        }],
        description: "Respond to approval request",
        user_facing: true,
    },
    StaticCommandDef {
        name: "approval_confirm",
        target: "ui/approval_confirm",
        params: &[],
        description: "Confirm selected approval option",
        user_facing: true,
    },
    StaticCommandDef {
        name: "approval_select_next",
        target: "ui/approval_select_next",
        params: &[],
        description: "Select next approval option",
        user_facing: true,
    },
    StaticCommandDef {
        name: "approval_select_prev",
        target: "ui/approval_select_prev",
        params: &[],
        description: "Select previous approval option",
        user_facing: true,
    },
    StaticCommandDef {
        name: "approval_scroll_down",
        target: "ui/approval_scroll_down",
        params: &[],
        description: "Scroll approval preview down",
        user_facing: true,
    },
    StaticCommandDef {
        name: "approval_scroll_up",
        target: "ui/approval_scroll_up",
        params: &[],
        description: "Scroll approval preview up",
        user_facing: true,
    },
    // -- Modal dialogs --
    StaticCommandDef {
        name: "toggle_shortcuts",
        target: "ui/toggle_shortcuts",
        params: &[],
        description: "Toggle shortcuts help",
        user_facing: true,
    },
    StaticCommandDef {
        name: "dismiss_shortcuts",
        target: "ui/dismiss_shortcuts",
        params: &[],
        description: "Dismiss shortcuts help",
        user_facing: true,
    },
    StaticCommandDef {
        name: "dismiss_usage",
        target: "ui/dismiss_usage",
        params: &[],
        description: "Dismiss usage dialog",
        user_facing: true,
    },
    StaticCommandDef {
        name: "toggle_usage",
        target: "ui/toggle_usage",
        params: &[],
        description: "Toggle usage info",
        user_facing: true,
    },
    // -- History search --
    StaticCommandDef {
        name: "enter_history_search",
        target: "ui/enter_history_search",
        params: &[],
        description: "Enter history search",
        user_facing: true,
    },
    StaticCommandDef {
        name: "history_search_cycle",
        target: "ui/history_search_cycle",
        params: &[],
        description: "Cycle to next history match",
        user_facing: true,
    },
    StaticCommandDef {
        name: "accept_history_search",
        target: "ui/accept_history_search",
        params: &[],
        description: "Accept history search result",
        user_facing: true,
    },
    StaticCommandDef {
        name: "dismiss_history_search",
        target: "ui/dismiss_history_search",
        params: &[],
        description: "Cancel history search",
        user_facing: true,
    },
    // -- Editor sub-modes --
    StaticCommandDef {
        name: "toggle_editor_mode",
        target: "ui/toggle_editor_mode",
        params: &[],
        description: "Toggle editor insert/normal mode",
        user_facing: true,
    },
    // -- Internal --
    StaticCommandDef {
        name: "set_row_count",
        target: "ui/set_row_count",
        params: &[StaticParamDef {
            name: "count",
            kind: StaticParamKind::Integer,
            required: true,
            default: None,
        }],
        description: "Set list row count",
        user_facing: false,
    },
    StaticCommandDef {
        name: "set_scroll_max",
        target: "ui/set_scroll_max",
        params: &[StaticParamDef {
            name: "max",
            kind: StaticParamKind::Integer,
            required: true,
            default: None,
        }],
        description: "Set max scroll position",
        user_facing: false,
    },
    StaticCommandDef {
        name: "set_viewport_height",
        target: "ui/set_viewport_height",
        params: &[StaticParamDef {
            name: "height",
            kind: StaticParamKind::Integer,
            required: true,
            default: None,
        }],
        description: "Set viewport height",
        user_facing: false,
    },
    StaticCommandDef {
        name: "set_input",
        target: "ui/set_input",
        params: &[
            StaticParamDef {
                name: "text",
                kind: StaticParamKind::String,
                required: false,
                default: None,
            },
            StaticParamDef {
                name: "cursor",
                kind: StaticParamKind::Integer,
                required: false,
                default: None,
            },
        ],
        description: "Set input content",
        user_facing: false,
    },
    StaticCommandDef {
        name: "set_status",
        target: "ui/set_status",
        params: &[StaticParamDef {
            name: "text",
            kind: StaticParamKind::String,
            required: false,
            default: None,
        }],
        description: "Set status bar message",
        user_facing: false,
    },
    StaticCommandDef {
        name: "clear_pending_action",
        target: "ui/clear_pending_action",
        params: &[],
        description: "Clear pending action flag",
        user_facing: false,
    },
    // -- History explorer --
    StaticCommandDef {
        name: "open_history",
        target: "ui/open_history",
        params: &[],
        description: "Open history explorer",
        user_facing: true,
    },
    StaticCommandDef {
        name: "back_to_thread",
        target: "ui/back_to_thread",
        params: &[],
        description: "Return to thread from history",
        user_facing: true,
    },
    StaticCommandDef {
        name: "toggle_expand",
        target: "ui/toggle_expand",
        params: &[],
        description: "Toggle expand/collapse message",
        user_facing: true,
    },
    StaticCommandDef {
        name: "expand_all",
        target: "ui/expand_all",
        params: &[],
        description: "Expand all messages",
        user_facing: true,
    },
    StaticCommandDef {
        name: "collapse_all",
        target: "ui/collapse_all",
        params: &[],
        description: "Collapse all messages",
        user_facing: true,
    },
    StaticCommandDef {
        name: "toggle_pretty",
        target: "ui/toggle_pretty",
        params: &[],
        description: "Toggle pretty-print for selected entry",
        user_facing: true,
    },
    StaticCommandDef {
        name: "toggle_full",
        target: "ui/toggle_full",
        params: &[],
        description: "Toggle full content for selected entry",
        user_facing: true,
    },
    StaticCommandDef {
        name: "select_page_up",
        target: "ui/select_page_up",
        params: &[],
        description: "Page up (move selection)",
        user_facing: true,
    },
    StaticCommandDef {
        name: "select_page_down",
        target: "ui/select_page_down",
        params: &[],
        description: "Page down (move selection)",
        user_facing: true,
    },
    StaticCommandDef {
        name: "select_half_page_up",
        target: "ui/select_half_page_up",
        params: &[],
        description: "Half page up (move selection)",
        user_facing: true,
    },
    StaticCommandDef {
        name: "select_half_page_down",
        target: "ui/select_half_page_down",
        params: &[],
        description: "Half page down (move selection)",
        user_facing: true,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_not_empty() {
        let cmds = builtin_commands();
        assert!(!cmds.is_empty());
    }

    #[test]
    fn all_names_are_unique() {
        let cmds = builtin_commands();
        let mut names = std::collections::HashSet::new();
        for cmd in cmds {
            assert!(
                names.insert(cmd.name),
                "duplicate command name: {}",
                cmd.name
            );
        }
    }

    #[test]
    fn compose_command_exists() {
        let cmds = builtin_commands();
        let compose = cmds
            .iter()
            .find(|c| c.name == "compose")
            .expect("compose command missing");
        assert_eq!(compose.target, "ui/enter_insert");
        assert!(compose.user_facing);
        assert_eq!(compose.params.len(), 1);
        assert_eq!(compose.params[0].name, "context");
        assert!(compose.params[0].required);
    }

    #[test]
    fn quit_command_exists() {
        let cmds = builtin_commands();
        let quit = cmds
            .iter()
            .find(|c| c.name == "quit")
            .expect("quit command missing");
        assert_eq!(quit.target, "ui/quit");
        assert!(quit.user_facing);
        assert!(quit.params.is_empty());
    }

    #[test]
    fn internal_commands_not_user_facing() {
        let cmds = builtin_commands();
        let set_row = cmds
            .iter()
            .find(|c| c.name == "set_row_count")
            .expect("set_row_count missing");
        assert!(!set_row.user_facing);
    }

    #[test]
    fn all_convert_to_command_def() {
        let cmds = builtin_commands();
        for cmd in cmds {
            let def = cmd.to_command_def();
            assert!(!def.name.is_empty());
            assert!(!def.target.is_empty());
        }
    }
}
