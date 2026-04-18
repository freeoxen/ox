use ox_types::*;

// --- ui.rs enums (existing behavior, preserved) ---

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
fn settings_focus_roundtrip() {
    for variant in [SettingsFocus::Accounts, SettingsFocus::Defaults] {
        let json = serde_json::to_string(&variant).unwrap();
        let back: SettingsFocus = serde_json::from_str(&json).unwrap();
        assert_eq!(variant, back);
    }
    assert_eq!(SettingsFocus::default(), SettingsFocus::Accounts);
    let json = serde_json::to_string(&SettingsFocus::Accounts).unwrap();
    assert_eq!(json, r#""accounts""#);
}

#[test]
fn wizard_step_roundtrip() {
    for variant in [
        WizardStep::AddAccount,
        WizardStep::SetDefaults,
        WizardStep::Done,
    ] {
        let json = serde_json::to_string(&variant).unwrap();
        let back: WizardStep = serde_json::from_str(&json).unwrap();
        assert_eq!(variant, back);
    }
    let json = serde_json::to_string(&WizardStep::AddAccount).unwrap();
    assert_eq!(json, r#""add_account""#);
}

#[test]
fn account_edit_fields_roundtrip() {
    let fields = AccountEditFields {
        name: "my-account".to_string(),
        dialect: 1,
        endpoint: "https://api.example.com".to_string(),
        key: "sk-secret".to_string(),
        focus: 2,
        is_new: true,
    };
    let json = serde_json::to_string(&fields).unwrap();
    let back: AccountEditFields = serde_json::from_str(&json).unwrap();
    assert_eq!(fields, back);
}

// --- Hierarchical UiCommand ---

#[test]
fn ui_command_global_quit_has_scope_tag() {
    let cmd = UiCommand::Global(GlobalCommand::Quit);
    let json = serde_json::to_string(&cmd).unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["scope"], "global");
    assert_eq!(v["command"]["command"], "quit");
}

#[test]
fn ui_command_inbox_select_next_has_scope_tag() {
    let cmd = UiCommand::Inbox(InboxCommand::SelectNext);
    let json = serde_json::to_string(&cmd).unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["scope"], "inbox");
    assert_eq!(v["command"]["command"], "select_next");
}

#[test]
fn ui_command_thread_set_scroll_max_roundtrip() {
    let cmd = UiCommand::Thread(ThreadCommand::SetScrollMax { max: 42 });
    let json = serde_json::to_string(&cmd).unwrap();
    let back: UiCommand = serde_json::from_str(&json).unwrap();
    let json2 = serde_json::to_string(&back).unwrap();
    assert_eq!(json, json2);

    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["scope"], "thread");
    assert_eq!(v["command"]["max"], 42);
}

#[test]
fn ui_command_settings_toggle_focus_roundtrip() {
    let cmd = UiCommand::Settings(SettingsCommand::ToggleFocus);
    let json = serde_json::to_string(&cmd).unwrap();
    let back: UiCommand = serde_json::from_str(&json).unwrap();
    let json2 = serde_json::to_string(&back).unwrap();
    assert_eq!(json, json2);

    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["scope"], "settings");
}

#[test]
fn global_command_open_roundtrip() {
    let cmd = UiCommand::Global(GlobalCommand::Open {
        thread_id: "thread-42".to_string(),
    });
    let json = serde_json::to_string(&cmd).unwrap();
    let back: UiCommand = serde_json::from_str(&json).unwrap();
    let json2 = serde_json::to_string(&back).unwrap();
    assert_eq!(json, json2);
}

#[test]
fn thread_command_reply_roundtrip() {
    let cmd = UiCommand::Thread(ThreadCommand::Reply);
    let json = serde_json::to_string(&cmd).unwrap();
    let back: UiCommand = serde_json::from_str(&json).unwrap();
    let json2 = serde_json::to_string(&back).unwrap();
    assert_eq!(json, json2);
}

#[test]
fn inbox_command_search_dismiss_chip_roundtrip() {
    let cmd = UiCommand::Inbox(InboxCommand::SearchDismissChip { index: 3 });
    let json = serde_json::to_string(&cmd).unwrap();
    let back: UiCommand = serde_json::from_str(&json).unwrap();
    let json2 = serde_json::to_string(&back).unwrap();
    assert_eq!(json, json2);
}

#[test]
fn settings_command_edit_save_roundtrip() {
    let cmd = UiCommand::Settings(SettingsCommand::EditSave {
        name: "acme".to_string(),
        provider: "anthropic".to_string(),
        endpoint: Some("https://api.example.com".to_string()),
        key: "sk-secret".to_string(),
    });
    let json = serde_json::to_string(&cmd).unwrap();
    let back: UiCommand = serde_json::from_str(&json).unwrap();
    let json2 = serde_json::to_string(&back).unwrap();
    assert_eq!(json, json2);
}

// --- Screen-discriminated UiSnapshot ---

#[test]
fn ui_snapshot_inbox_default_has_screen_tag() {
    let snapshot = UiSnapshot::default();
    let json = serde_json::to_string(&snapshot).unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    // screen is nested inside the screen field
    assert_eq!(v["screen"]["screen"], "inbox");
}

#[test]
fn ui_snapshot_thread_roundtrip() {
    let snapshot = UiSnapshot {
        screen: ScreenSnapshot::Thread(ThreadSnapshot {
            thread_id: "t-123".to_string(),
            scroll: 10,
            scroll_max: 100,
            viewport_height: 40,
            editor: Some(EditorSnapshot {
                context: InsertContext::Compose,
                content: "hello".to_string(),
                cursor: 5,
            }),
        }),
        pending_action: None,
    };
    let json = serde_json::to_string(&snapshot).unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["screen"]["screen"], "thread");
    assert_eq!(v["screen"]["thread_id"], "t-123");
    assert_eq!(v["screen"]["scroll"], 10);

    let back: UiSnapshot = serde_json::from_str(&json).unwrap();
    let json2 = serde_json::to_string(&back).unwrap();
    assert_eq!(json, json2);
}

#[test]
fn ui_snapshot_settings_default_roundtrip() {
    let snapshot = UiSnapshot {
        screen: ScreenSnapshot::Settings(SettingsSnapshot::default()),
        pending_action: None,
    };
    let json = serde_json::to_string(&snapshot).unwrap();
    let back: UiSnapshot = serde_json::from_str(&json).unwrap();
    let json2 = serde_json::to_string(&back).unwrap();
    assert_eq!(json, json2);

    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(v["screen"]["screen"], "settings");
}

#[test]
fn ui_snapshot_inbox_with_search_roundtrip() {
    let snapshot = UiSnapshot {
        screen: ScreenSnapshot::Inbox(InboxSnapshot {
            selected_row: 3,
            row_count: 10,
            editor: None,
            search: SearchSnapshot {
                chips: vec!["tag:urgent".to_string()],
                live_query: "foo".to_string(),
                active: true,
                result_handle: None,
            },
        }),
        pending_action: Some(PendingAction::OpenSelected),
    };
    let json = serde_json::to_string(&snapshot).unwrap();
    let back: UiSnapshot = serde_json::from_str(&json).unwrap();
    let json2 = serde_json::to_string(&back).unwrap();
    assert_eq!(json, json2);
}

// --- InputKeyEvent ---

#[test]
fn input_key_event_roundtrip() {
    let event = InputKeyEvent {
        mode: Mode::Normal,
        key: "j".to_string(),
        screen: Screen::Inbox,
    };
    let json = serde_json::to_string(&event).unwrap();
    let back: InputKeyEvent = serde_json::from_str(&json).unwrap();
    assert_eq!(back.key, "j");
    assert_eq!(back.screen, Screen::Inbox);
    assert_eq!(back.mode, Mode::Normal);
}

// --- ApprovalResponse ---

#[test]
fn approval_response_roundtrip() {
    let resp = ApprovalResponse {
        decision: Decision::AllowOnce,
    };
    let json = serde_json::to_string(&resp).unwrap();
    let back: ApprovalResponse = serde_json::from_str(&json).unwrap();
    assert_eq!(back.decision, Decision::AllowOnce);
}

// --- Unchanged types ---

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
        ..Default::default()
    };
    let json = serde_json::to_string(&usage).unwrap();
    let back: TokenUsage = serde_json::from_str(&json).unwrap();
    assert_eq!(usage, back);

    // Old serialized data without cache fields should deserialize with 0s
    let old_json = r#"{"input_tokens":100,"output_tokens":50}"#;
    let old: TokenUsage = serde_json::from_str(old_json).unwrap();
    assert_eq!(old.cache_creation_input_tokens, 0);
    assert_eq!(old.cache_read_input_tokens, 0);
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
