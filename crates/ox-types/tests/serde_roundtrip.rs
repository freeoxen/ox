use ox_types::*;

#[test]
fn screen_roundtrip_and_snake_case() {
    let screen = Screen::Inbox;
    let json = serde_json::to_string(&screen).unwrap();
    assert_eq!(json, r#""inbox""#);

    let screen = Screen::Settings;
    let json = serde_json::to_string(&screen).unwrap();
    assert_eq!(json, r#""settings""#);

    for variant in [Screen::Inbox, Screen::Thread, Screen::Settings] {
        let json = serde_json::to_string(&variant).unwrap();
        let back: Screen = serde_json::from_str(&json).unwrap();
        assert_eq!(variant, back);
    }
}

#[test]
fn mode_roundtrip() {
    for variant in [Mode::Normal, Mode::Insert] {
        let json = serde_json::to_string(&variant).unwrap();
        let back: Mode = serde_json::from_str(&json).unwrap();
        assert_eq!(variant, back);
    }
}

#[test]
fn insert_context_roundtrip() {
    for variant in [
        InsertContext::Compose,
        InsertContext::Reply,
        InsertContext::Search,
        InsertContext::Command,
    ] {
        let json = serde_json::to_string(&variant).unwrap();
        let back: InsertContext = serde_json::from_str(&json).unwrap();
        assert_eq!(variant, back);
    }
}

#[test]
fn pending_action_roundtrip_and_snake_case() {
    let action = PendingAction::SendInput;
    let json = serde_json::to_string(&action).unwrap();
    assert_eq!(json, r#""send_input""#);

    let action = PendingAction::OpenSelected;
    let json = serde_json::to_string(&action).unwrap();
    assert_eq!(json, r#""open_selected""#);

    let action = PendingAction::ArchiveSelected;
    let json = serde_json::to_string(&action).unwrap();
    assert_eq!(json, r#""archive_selected""#);

    for variant in [
        PendingAction::SendInput,
        PendingAction::Quit,
        PendingAction::OpenSelected,
        PendingAction::ArchiveSelected,
    ] {
        let json = serde_json::to_string(&variant).unwrap();
        let back: PendingAction = serde_json::from_str(&json).unwrap();
        assert_eq!(variant, back);
    }
}

#[test]
fn ui_snapshot_default_and_roundtrip() {
    let snapshot = UiSnapshot::default();
    assert_eq!(snapshot.screen, Screen::Inbox);
    assert_eq!(snapshot.mode, Mode::Normal);
    assert!(snapshot.active_thread.is_none());
    assert!(snapshot.insert_context.is_none());
    assert_eq!(snapshot.selected_row, 0);
    assert_eq!(snapshot.scroll, 0);
    assert!(snapshot.pending_action.is_none());
    assert_eq!(snapshot.input.content, "");
    assert_eq!(snapshot.input.cursor, 0);
    assert!(snapshot.search.chips.is_empty());
    assert_eq!(snapshot.search.live_query, "");
    assert!(!snapshot.search.active);

    let json = serde_json::to_string(&snapshot).unwrap();
    let back: UiSnapshot = serde_json::from_str(&json).unwrap();
    assert_eq!(back.screen, snapshot.screen);
    assert_eq!(back.mode, snapshot.mode);
    assert_eq!(back.selected_row, snapshot.selected_row);
}

#[test]
fn ui_command_tagged_serialization() {
    let cmd = UiCommand::SelectNext;
    let json = serde_json::to_string(&cmd).unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["command"], "select_next");
}

#[test]
fn ui_command_unit_variants_roundtrip() {
    let commands = vec![
        UiCommand::SelectNext,
        UiCommand::SelectPrev,
        UiCommand::SelectFirst,
        UiCommand::SelectLast,
        UiCommand::Close,
        UiCommand::GoToSettings,
        UiCommand::GoToInbox,
        UiCommand::ExitInsert,
        UiCommand::ClearInput,
        UiCommand::ScrollUp,
        UiCommand::ScrollDown,
        UiCommand::ScrollToTop,
        UiCommand::ScrollToBottom,
        UiCommand::ScrollPageUp,
        UiCommand::ScrollPageDown,
        UiCommand::ScrollHalfPageUp,
        UiCommand::ScrollHalfPageDown,
        UiCommand::SendInput,
        UiCommand::Quit,
        UiCommand::OpenSelected,
        UiCommand::ArchiveSelected,
        UiCommand::ClearPendingAction,
        UiCommand::SearchDeleteChar,
        UiCommand::SearchClear,
        UiCommand::SearchSaveChip,
    ];

    for cmd in commands {
        let json = serde_json::to_string(&cmd).unwrap();
        let back: UiCommand = serde_json::from_str(&json).unwrap();
        let json2 = serde_json::to_string(&back).unwrap();
        assert_eq!(json, json2);
    }
}

#[test]
fn ui_command_open_roundtrip() {
    let cmd = UiCommand::Open {
        thread_id: "thread-42".to_string(),
    };
    let json = serde_json::to_string(&cmd).unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["command"], "open");
    assert_eq!(v["thread_id"], "thread-42");

    let back: UiCommand = serde_json::from_str(&json).unwrap();
    let json2 = serde_json::to_string(&back).unwrap();
    assert_eq!(json, json2);
}

#[test]
fn ui_command_enter_insert_roundtrip() {
    let cmd = UiCommand::EnterInsert {
        context: InsertContext::Reply,
    };
    let json = serde_json::to_string(&cmd).unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["command"], "enter_insert");
    assert_eq!(v["context"], "reply");

    let back: UiCommand = serde_json::from_str(&json).unwrap();
    let json2 = serde_json::to_string(&back).unwrap();
    assert_eq!(json, json2);
}

#[test]
fn ui_command_search_dismiss_chip_roundtrip() {
    let cmd = UiCommand::SearchDismissChip { index: 3 };
    let json = serde_json::to_string(&cmd).unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["command"], "search_dismiss_chip");
    assert_eq!(v["index"], 3);

    let back: UiCommand = serde_json::from_str(&json).unwrap();
    let json2 = serde_json::to_string(&back).unwrap();
    assert_eq!(json, json2);
}

#[test]
fn tool_status_roundtrip() {
    let status = ToolStatus {
        name: "bash".to_string(),
        status: "running".to_string(),
    };
    let json = serde_json::to_string(&status).unwrap();
    let back: ToolStatus = serde_json::from_str(&json).unwrap();
    assert_eq!(status, back);
}

#[test]
fn token_usage_default_and_roundtrip() {
    let usage = TokenUsage::default();
    assert_eq!(usage.input_tokens, 0);
    assert_eq!(usage.output_tokens, 0);

    let usage = TokenUsage {
        input_tokens: 1500,
        output_tokens: 300,
    };
    let json = serde_json::to_string(&usage).unwrap();
    let back: TokenUsage = serde_json::from_str(&json).unwrap();
    assert_eq!(usage, back);
}

#[test]
fn approval_request_roundtrip() {
    let req = ApprovalRequest {
        tool_name: "file_write".to_string(),
        input_preview: "Writing to /tmp/test.txt".to_string(),
    };
    let json = serde_json::to_string(&req).unwrap();
    let back: ApprovalRequest = serde_json::from_str(&json).unwrap();
    assert_eq!(back.tool_name, "file_write");
    assert_eq!(back.input_preview, "Writing to /tmp/test.txt");
}
