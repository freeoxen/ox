use crate::app::App;
use crate::settings_state::SettingsState;
use crate::theme::Theme;
use crate::types::{APPROVAL_OPTIONS, CustomizeState};
use crate::view_state::fetch_view_state;
use crossterm::event::{self, Event, KeyCode, KeyModifiers, MouseEventKind};
use std::time::Duration;
use structfs_core_store::Writer as StructWriter;

/// Dialog-local state, owned by the event loop (not App, not broker).
pub(crate) struct DialogState {
    pub approval_selected: usize,
    pub pending_customize: Option<CustomizeState>,
}

/// Async event loop that dispatches through the BrokerStore.
///
/// ALL state mutations go through UiStore via the broker. Text editing
/// commands (insert_char, delete_char) are dispatched directly to UiStore
/// when no InputStore binding matches. Application-level commands
/// (send, open, archive, quit) are signaled via UiStore's pending_action
/// field and handled by App methods.
pub async fn run_async(
    app: &mut App,
    client: &ox_broker::ClientHandle,
    theme: &Theme,
    terminal: &mut ratatui::DefaultTerminal,
    needs_setup: bool,
) -> std::io::Result<()> {
    use crate::key_encode::encode_key;
    use structfs_core_store::path;

    let mut dialog = DialogState {
        approval_selected: 0,
        pending_customize: None,
    };
    let mut settings = if needs_setup {
        // Navigate to settings screen via broker
        client
            .write(
                &structfs_core_store::path!("ui/go_to_settings"),
                structfs_core_store::Record::parsed(structfs_core_store::Value::Null),
            )
            .await
            .ok();
        SettingsState::new_wizard()
    } else {
        SettingsState::new()
    };

    loop {
        // 1. Fetch ViewState, draw, extract owned data needed after drop.
        //
        // ViewState borrows from App so we scope it tightly: draw, then
        // extract the owned fields we need for pending-action handling and
        // event dispatch, then drop the borrow.
        let pending_action: Option<String>;
        let input_text: String;
        let screen_owned: String;
        let mode_owned: String;
        let insert_context_owned: Option<String>;
        let has_active_thread: bool;
        let active_thread_id: Option<String>;
        let selected_thread_id: Option<String>;
        let search_active: bool;
        let has_approval_pending: bool;
        // For text editing fallback
        let cursor_pos: usize;
        let input_len: usize;

        let mut content_height: Option<usize> = None;
        let mut viewport_height: usize = 0;
        {
            let vs = fetch_view_state(client, app, &dialog).await;

            // Set row_count in UiStore (for inbox navigation bounds)
            // Only write on inbox screen — thread screen has no row selection.
            if vs.screen == "inbox" {
                let row_count = vs.inbox_threads.len() as i64;
                let _ = client
                    .write(&path!("ui/set_row_count"), cmd!("count" => row_count))
                    .await;
            }

            // Draw
            terminal.draw(|frame| {
                let (ch, vh) = crate::tui::draw(frame, &vs, &settings, theme);
                content_height = ch;
                viewport_height = vh;
            })?;

            // Update scroll_max and viewport_height in broker (after draw)
            if vs.active_thread.is_some() && viewport_height > 0 {
                let scroll_max = content_height.unwrap_or(0).saturating_sub(viewport_height) as i64;
                let _ = client
                    .write(
                        &path!("ui/set_scroll_max"),
                        cmd!("max" => scroll_max.max(0)),
                    )
                    .await;

                let _ = client
                    .write(
                        &path!("ui/set_viewport_height"),
                        cmd!("height" => viewport_height as i64),
                    )
                    .await;
            }

            // Extract owned copies of data needed after vs is dropped
            pending_action = vs.pending_action.clone();
            input_text = vs.input.clone();
            screen_owned = vs.screen.clone();
            mode_owned = vs.mode.clone();
            insert_context_owned = vs.insert_context.clone();
            has_active_thread = vs.active_thread.is_some();
            active_thread_id = vs.active_thread.clone();
            selected_thread_id = vs.inbox_threads.get(vs.selected_row).map(|t| t.id.clone());
            search_active = vs.search_active;
            cursor_pos = vs.cursor;
            input_len = vs.input.len();
            has_approval_pending = vs.approval_pending.is_some();
        }
        // vs is now dropped — safe to mutate app

        // 2. Handle pending_action
        if let Some(action) = &pending_action {
            match action.as_str() {
                "send_input" => {
                    let new_tid = app.send_input_with_text(
                        input_text.clone(),
                        &mode_owned,
                        insert_context_owned.as_deref(),
                        active_thread_id.as_deref(),
                    );
                    // Clear input and exit insert mode through broker
                    let _ = client.write(&path!("ui/clear_input"), cmd!()).await;
                    let _ = client.write(&path!("ui/exit_insert"), cmd!()).await;
                    // If compose created a new thread, open it in UiStore
                    if let Some(tid) = new_tid {
                        let _ = client
                            .write(&path!("ui/open"), cmd!("thread_id" => tid))
                            .await;
                    }
                }
                "quit" => return Ok(()),
                "open_selected" => {
                    if let Some(id) = &selected_thread_id {
                        let _ = client
                            .write(&path!("ui/open"), cmd!("thread_id" => id))
                            .await;
                    }
                }
                "archive_selected" => {
                    if let Some(id) = &selected_thread_id {
                        let update_path = ox_kernel::Path::from_components(vec![
                            "threads".to_string(),
                            id.clone(),
                        ]);
                        app.pool
                            .inbox()
                            .write(&update_path, cmd!("inbox_state" => "done"))
                            .ok();
                    }
                }
                _ => {}
            }
            // Clear the pending action
            let _ = client
                .write(&path!("ui/clear_pending_action"), cmd!())
                .await;
        }

        // Populate settings accounts from config when on the settings screen.
        if screen_owned == "settings" && settings.accounts.is_empty() {
            let config = crate::config::resolve_config(
                app.pool.inbox_root(),
                &crate::config::CliOverrides::default(),
            );
            settings.refresh_accounts(&config, &app.pool.inbox_root().join("keys"));
        }

        // 5. Poll terminal event
        let terminal_event = tokio::task::block_in_place(|| {
            if event::poll(Duration::from_millis(50)).unwrap_or(false) {
                event::read().ok()
            } else {
                None
            }
        });

        // 4. Handle terminal events
        if let Some(evt) = terminal_event {
            match evt {
                Event::Key(key) => {
                    // Customize dialog — bypass broker entirely
                    if dialog.pending_customize.is_some() {
                        crate::key_handlers::handle_customize_key(
                            &mut dialog,
                            client,
                            &active_thread_id,
                            key.code,
                        )
                        .await;
                    }
                    // Approval dialog — direct handling (reads from broker)
                    else if has_approval_pending && mode_owned == "normal" {
                        crate::key_handlers::handle_approval_key(
                            &mut dialog,
                            client,
                            &active_thread_id,
                            key.code,
                            key.modifiers,
                        )
                        .await;
                    }
                    // Normal + Insert — dispatch through broker
                    else if let Some(key_str) = encode_key(key.modifiers, key.code) {
                        let mode = mode_owned.as_str();
                        let screen = screen_owned.as_str();

                        // Settings screen — edit dialog key handling
                        if screen == "settings" && mode == "normal" && settings.editing.is_some()
                        {
                            use crate::settings_state::{DIALECTS, TestStatus};
                            let inbox_root = app.pool.inbox_root().to_path_buf();
                            let keys_dir = inbox_root.join("keys");

                            // Use an enum to signal post-match actions that
                            // require dropping the &mut borrow first.
                            enum EditAction {
                                None,
                                Cancel,
                                Save,
                                Handled,
                            }

                            let action = if let Some(ref mut editing) = settings.editing {
                                match key_str.as_str() {
                                    "Tab" => {
                                        editing.focus = (editing.focus + 1) % 4;
                                        EditAction::Handled
                                    }
                                    "Esc" => EditAction::Cancel,
                                    "Enter" => {
                                        if !editing.name.is_empty() {
                                            let entry = crate::config::AccountEntry {
                                                provider: DIALECTS[editing.dialect].to_string(),
                                                endpoint: if editing.endpoint.is_empty() {
                                                    None
                                                } else {
                                                    Some(editing.endpoint.clone())
                                                },
                                            };
                                            crate::config::write_account(
                                                &inbox_root,
                                                &editing.name,
                                                &entry,
                                            )
                                            .ok();
                                            if !editing.key.is_empty() {
                                                crate::config::write_key_file(
                                                    &keys_dir,
                                                    &editing.name,
                                                    &editing.key,
                                                )
                                                .ok();
                                            }
                                            EditAction::Save
                                        } else {
                                            EditAction::Handled
                                        }
                                    }
                                    "Left" if editing.focus == 1 => {
                                        editing.dialect = if editing.dialect == 0 {
                                            DIALECTS.len() - 1
                                        } else {
                                            editing.dialect - 1
                                        };
                                        EditAction::Handled
                                    }
                                    "Right" if editing.focus == 1 => {
                                        editing.dialect = if editing.dialect >= DIALECTS.len() - 1 {
                                            0
                                        } else {
                                            editing.dialect + 1
                                        };
                                        EditAction::Handled
                                    }
                                    "Backspace" => {
                                        match editing.focus {
                                            0 => {
                                                editing.name.pop();
                                            }
                                            2 => {
                                                editing.endpoint.pop();
                                            }
                                            3 => {
                                                editing.key.pop();
                                            }
                                            _ => {}
                                        }
                                        EditAction::Handled
                                    }
                                    "t" => EditAction::None, // handled below (needs non-mut borrow)
                                    other => {
                                        if other.len() == 1
                                            && !other.chars().next().unwrap().is_control()
                                        {
                                            let ch = other.chars().next().unwrap();
                                            match editing.focus {
                                                0 => editing.name.push(ch),
                                                2 => editing.endpoint.push(ch),
                                                3 => editing.key.push(ch),
                                                _ => {}
                                            }
                                            EditAction::Handled
                                        } else {
                                            EditAction::None
                                        }
                                    }
                                }
                            } else {
                                EditAction::None
                            };

                            // Now the &mut borrow on editing is dropped —
                            // safe to mutate settings.editing itself.
                            match action {
                                EditAction::Cancel => {
                                    settings.editing = None;
                                    settings.test_status = TestStatus::Idle;
                                    continue;
                                }
                                EditAction::Save => {
                                    settings.editing = None;
                                    settings.test_status = TestStatus::Idle;
                                    let config = crate::config::resolve_config(
                                        &inbox_root,
                                        &crate::config::CliOverrides::default(),
                                    );
                                    settings.refresh_accounts(&config, &keys_dir);
                                    // Advance wizard after first account save
                                    if let Some(ref mut step) = settings.wizard {
                                        use crate::settings_state::WizardStep;
                                        match step {
                                            WizardStep::AddAccount => {
                                                *step = WizardStep::SetDefaults;
                                                settings.focus = crate::settings_state::SettingsFocus::Defaults;
                                            }
                                            _ => {}
                                        }
                                    }
                                    continue;
                                }
                                EditAction::Handled => {
                                    continue;
                                }
                                EditAction::None => {}
                            }

                            // Handle 't' for test connection in edit dialog
                            // (done after match so the &mut borrow on editing is dropped)
                            if key_str == "t" {
                                if let Some(ref editing) = settings.editing {
                                    if editing.key.is_empty() {
                                        settings.test_status =
                                            TestStatus::Failed("No API key entered".into());
                                    } else {
                                        let dialect = DIALECTS[editing.dialect];
                                        let mut provider_config = match dialect {
                                            "openai" => ox_gate::ProviderConfig::openai(),
                                            _ => ox_gate::ProviderConfig::anthropic(),
                                        };
                                        if !editing.endpoint.is_empty() {
                                            provider_config.endpoint = editing.endpoint.clone();
                                        }
                                        let model = match dialect {
                                            "openai" => "gpt-4o-mini",
                                            _ => "claude-haiku-4-5-20251001",
                                        };
                                        let request = ox_kernel::CompletionRequest {
                                            model: model.to_string(),
                                            max_tokens: 1,
                                            system: String::new(),
                                            messages: vec![serde_json::json!({"role": "user", "content": "hi"})],
                                            tools: vec![],
                                            stream: true,
                                        };
                                        settings.test_status = TestStatus::Testing;
                                        let start = std::time::Instant::now();
                                        let send = crate::transport::make_send_fn(
                                            provider_config,
                                            editing.key.clone(),
                                        );
                                        match send(&request) {
                                            Ok(_) => {
                                                let ms = start.elapsed().as_millis();
                                                settings.test_status = TestStatus::Success(
                                                    format!("Connected ({dialect}, {ms}ms)"),
                                                );
                                            }
                                            Err(e) => {
                                                settings.test_status = TestStatus::Failed(e);
                                            }
                                        }
                                    }
                                }
                                continue;
                            }
                        }

                        // Settings screen navigation (before broker dispatch)
                        if screen == "settings"
                            && mode == "normal"
                            && settings.editing.is_none()
                        {
                            use crate::settings_state::SettingsFocus;
                            let handled = match key_str.as_str() {
                                "j" | "Down" => {
                                    if settings.focus == SettingsFocus::Accounts
                                        && !settings.accounts.is_empty()
                                    {
                                        settings.selected_account = (settings.selected_account + 1)
                                            .min(settings.accounts.len() - 1);
                                    } else if settings.focus == SettingsFocus::Defaults {
                                        settings.defaults_focus =
                                            (settings.defaults_focus + 1).min(2);
                                    }
                                    true
                                }
                                "k" | "Up" => {
                                    if settings.focus == SettingsFocus::Accounts {
                                        settings.selected_account =
                                            settings.selected_account.saturating_sub(1);
                                    } else if settings.focus == SettingsFocus::Defaults {
                                        settings.defaults_focus =
                                            settings.defaults_focus.saturating_sub(1);
                                    }
                                    true
                                }
                                "Tab" => {
                                    settings.focus = match settings.focus {
                                        SettingsFocus::Accounts => SettingsFocus::Defaults,
                                        SettingsFocus::Defaults => SettingsFocus::Accounts,
                                    };
                                    true
                                }
                                "a" => {
                                    if settings.focus == SettingsFocus::Accounts {
                                        settings.editing = Some(
                                            crate::settings_state::AccountEditFields {
                                                name: String::new(),
                                                dialect: 0,
                                                endpoint: String::new(),
                                                key: String::new(),
                                                focus: 0,
                                                is_new: true,
                                            },
                                        );
                                        settings.test_status =
                                            crate::settings_state::TestStatus::Idle;
                                    }
                                    true
                                }
                                "e" => {
                                    if settings.focus == SettingsFocus::Accounts {
                                        if let Some(acct) =
                                            settings.accounts.get(settings.selected_account)
                                        {
                                            use crate::settings_state::DIALECTS;
                                            let dialect_idx = DIALECTS
                                                .iter()
                                                .position(|d| *d == acct.dialect)
                                                .unwrap_or(0);
                                            let inbox_root =
                                                app.pool.inbox_root().to_path_buf();
                                            let keys_dir = inbox_root.join("keys");
                                            let key_val = crate::config::read_key_file(
                                                &keys_dir, &acct.name,
                                            )
                                            .unwrap_or_default();
                                            let config = crate::config::resolve_config(
                                                &inbox_root,
                                                &crate::config::CliOverrides::default(),
                                            );
                                            let endpoint = config
                                                .gate
                                                .accounts
                                                .get(&acct.name)
                                                .and_then(|e| e.endpoint.clone())
                                                .unwrap_or_default();
                                            settings.editing = Some(
                                                crate::settings_state::AccountEditFields {
                                                    name: acct.name.clone(),
                                                    dialect: dialect_idx,
                                                    endpoint,
                                                    key: key_val,
                                                    focus: 0,
                                                    is_new: false,
                                                },
                                            );
                                            settings.test_status =
                                                crate::settings_state::TestStatus::Idle;
                                        }
                                    }
                                    true
                                }
                                "Enter" if settings.wizard == Some(crate::settings_state::WizardStep::Done) => {
                                    settings.wizard = None;
                                    // Navigate back to inbox
                                    client.write(
                                        &structfs_core_store::path!("ui/go_to_inbox"),
                                        structfs_core_store::Record::parsed(structfs_core_store::Value::Null),
                                    ).await.ok();
                                    true
                                }
                                "Left" if settings.focus == SettingsFocus::Defaults => {
                                    match settings.defaults_focus {
                                        0 => {
                                            if !settings.accounts.is_empty() {
                                                settings.default_account_idx =
                                                    if settings.default_account_idx == 0 {
                                                        settings.accounts.len() - 1
                                                    } else {
                                                        settings.default_account_idx - 1
                                                    };
                                                settings.default_model_idx = 0;
                                            }
                                        }
                                        1 => {
                                            let dialect = settings
                                                .accounts
                                                .get(settings.default_account_idx)
                                                .map(|a| a.dialect.as_str())
                                                .unwrap_or("anthropic");
                                            let models =
                                                crate::settings_state::models_for_dialect(dialect);
                                            if !models.is_empty() {
                                                settings.default_model_idx =
                                                    if settings.default_model_idx == 0 {
                                                        models.len() - 1
                                                    } else {
                                                        settings.default_model_idx - 1
                                                    };
                                            }
                                        }
                                        _ => {}
                                    }
                                    true
                                }
                                "Right" if settings.focus == SettingsFocus::Defaults => {
                                    match settings.defaults_focus {
                                        0 => {
                                            if !settings.accounts.is_empty() {
                                                settings.default_account_idx =
                                                    (settings.default_account_idx + 1)
                                                        % settings.accounts.len();
                                                settings.default_model_idx = 0;
                                            }
                                        }
                                        1 => {
                                            let dialect = settings
                                                .accounts
                                                .get(settings.default_account_idx)
                                                .map(|a| a.dialect.as_str())
                                                .unwrap_or("anthropic");
                                            let models =
                                                crate::settings_state::models_for_dialect(dialect);
                                            if !models.is_empty() {
                                                settings.default_model_idx =
                                                    (settings.default_model_idx + 1) % models.len();
                                            }
                                        }
                                        _ => {}
                                    }
                                    true
                                }
                                "Backspace"
                                    if settings.focus == SettingsFocus::Defaults
                                        && settings.defaults_focus == 2 =>
                                {
                                    settings.default_max_tokens.pop();
                                    true
                                }
                                "Enter" if settings.focus == SettingsFocus::Defaults => {
                                    // Determine current selections
                                    let acct_name = settings
                                        .accounts
                                        .get(settings.default_account_idx)
                                        .map(|a| a.name.clone())
                                        .unwrap_or_default();
                                    let dialect = settings
                                        .accounts
                                        .get(settings.default_account_idx)
                                        .map(|a| a.dialect.as_str())
                                        .unwrap_or("anthropic");
                                    let models =
                                        crate::settings_state::models_for_dialect(dialect);
                                    let model = models
                                        .get(settings.default_model_idx)
                                        .copied()
                                        .unwrap_or("claude-sonnet-4-20250514");
                                    let max_tokens: i64 =
                                        settings.default_max_tokens.parse().unwrap_or(4096);

                                    // Write to ConfigStore via broker
                                    use structfs_core_store::{Record, Value};
                                    client
                                        .write(
                                            &path!("config/gate/defaults/account"),
                                            Record::parsed(Value::String(acct_name)),
                                        )
                                        .await
                                        .ok();
                                    client
                                        .write(
                                            &path!("config/gate/defaults/model"),
                                            Record::parsed(Value::String(model.to_string())),
                                        )
                                        .await
                                        .ok();
                                    client
                                        .write(
                                            &path!("config/gate/defaults/max_tokens"),
                                            Record::parsed(Value::Integer(max_tokens)),
                                        )
                                        .await
                                        .ok();
                                    // Persist to disk
                                    client
                                        .write(
                                            &path!("config/save"),
                                            Record::parsed(Value::Null),
                                        )
                                        .await
                                        .ok();

                                    // Advance wizard if active
                                    if let Some(ref mut step) = settings.wizard {
                                        if *step
                                            == crate::settings_state::WizardStep::SetDefaults
                                        {
                                            *step = crate::settings_state::WizardStep::Done;
                                        }
                                    }
                                    true
                                }
                                "Esc" | "q" if settings.wizard.is_some() => {
                                    // Allow skipping wizard — go to inbox
                                    settings.wizard = None;
                                    client.write(
                                        &structfs_core_store::path!("ui/go_to_inbox"),
                                        structfs_core_store::Record::parsed(structfs_core_store::Value::Null),
                                    ).await.ok();
                                    true
                                }
                                "d" => {
                                    if settings.focus == SettingsFocus::Accounts {
                                        if let Some(acct) =
                                            settings.accounts.get(settings.selected_account)
                                        {
                                            let name = acct.name.clone();
                                            let inbox_root =
                                                app.pool.inbox_root().to_path_buf();
                                            let keys_dir = inbox_root.join("keys");
                                            crate::config::delete_account(&inbox_root, &name)
                                                .ok();
                                            crate::config::delete_key_file(&keys_dir, &name)
                                                .ok();
                                            let config = crate::config::resolve_config(
                                                &inbox_root,
                                                &crate::config::CliOverrides::default(),
                                            );
                                            settings.refresh_accounts(&config, &keys_dir);
                                        }
                                    }
                                    true
                                }
                                "t" => {
                                    if settings.focus == SettingsFocus::Accounts {
                                        if let Some(acct) =
                                            settings.accounts.get(settings.selected_account)
                                        {
                                            let inbox_root =
                                                app.pool.inbox_root().to_path_buf();
                                            let keys_dir = inbox_root.join("keys");
                                            let key = crate::config::read_key_file(
                                                &keys_dir, &acct.name,
                                            )
                                            .unwrap_or_default();
                                            if key.is_empty() {
                                                settings.test_status =
                                                    crate::settings_state::TestStatus::Failed(
                                                        "No key file found".into(),
                                                    );
                                            } else {
                                                let config = crate::config::resolve_config(
                                                    &inbox_root,
                                                    &crate::config::CliOverrides::default(),
                                                );
                                                let entry =
                                                    config.gate.accounts.get(&acct.name);
                                                let dialect = entry
                                                    .map(|e| e.provider.as_str())
                                                    .unwrap_or("anthropic");
                                                let mut provider_config = match dialect {
                                                    "openai" => {
                                                        ox_gate::ProviderConfig::openai()
                                                    }
                                                    _ => {
                                                        ox_gate::ProviderConfig::anthropic()
                                                    }
                                                };
                                                if let Some(ep) =
                                                    entry.and_then(|e| e.endpoint.as_ref())
                                                {
                                                    provider_config.endpoint = ep.clone();
                                                }
                                                let model = match dialect {
                                                    "openai" => "gpt-4o-mini",
                                                    _ => "claude-haiku-4-5-20251001",
                                                };
                                                let request = ox_kernel::CompletionRequest {
                                                    model: model.to_string(),
                                                    max_tokens: 1,
                                                    system: String::new(),
                                                    messages: vec![serde_json::json!({"role": "user", "content": "hi"})],
                                                    tools: vec![],
                                                    stream: true,
                                                };
                                                let start = std::time::Instant::now();
                                                let send =
                                                    crate::transport::make_send_fn(
                                                        provider_config,
                                                        key,
                                                    );
                                                match send(&request) {
                                                    Ok(_) => {
                                                        let ms =
                                                            start.elapsed().as_millis();
                                                        settings.test_status =
                                                            crate::settings_state::TestStatus::Success(
                                                                format!(
                                                                    "Connected ({dialect}, {ms}ms)"
                                                                ),
                                                            );
                                                    }
                                                    Err(e) => {
                                                        settings.test_status =
                                                            crate::settings_state::TestStatus::Failed(e);
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    true
                                }
                                other
                                    if settings.focus == SettingsFocus::Defaults
                                        && settings.defaults_focus == 2
                                        && other.len() == 1
                                        && other.chars().next().unwrap().is_ascii_digit() =>
                                {
                                    settings
                                        .default_max_tokens
                                        .push(other.chars().next().unwrap());
                                    true
                                }
                                _ => false,
                            };
                            if handled {
                                continue;
                            }
                        }

                        // Search chip dismissal (1-9 in normal mode, inbox, search active)
                        if mode == "normal" && screen == "inbox" && search_active {
                            if let KeyCode::Char(c @ '1'..='9') = key.code {
                                let idx = (c as u8 - b'1') as usize;
                                let _ = client
                                    .write(
                                        &path!("ui/search_dismiss_chip"),
                                        cmd!("index" => idx as i64),
                                    )
                                    .await;
                                continue;
                            }
                        }

                        // Try InputStore dispatch
                        let result = client
                            .write(
                                &path!("input/key"),
                                cmd!("mode" => mode, "key" => key_str.clone(), "screen" => screen),
                            )
                            .await;

                        if result.is_err() && mode_owned == "insert" {
                            if insert_context_owned.as_deref() == Some("search") {
                                dispatch_search_edit(client, key.modifiers, key.code).await;
                            } else {
                                match key.code {
                                    KeyCode::Up => {
                                        if let Some((text, cursor)) = app.history_up(&input_text) {
                                            let _ = client
                                                .write(
                                                    &path!("ui/set_input"),
                                                    cmd!("text" => text, "cursor" => cursor as i64),
                                                )
                                                .await;
                                        }
                                    }
                                    KeyCode::Down => {
                                        if let Some((text, cursor)) = app.history_down() {
                                            let _ = client
                                                .write(
                                                    &path!("ui/set_input"),
                                                    cmd!("text" => text, "cursor" => cursor as i64),
                                                )
                                                .await;
                                        }
                                    }
                                    _ => {
                                        dispatch_text_edit_owned(
                                            client,
                                            cursor_pos,
                                            input_len,
                                            key.modifiers,
                                            key.code,
                                        )
                                        .await;
                                    }
                                }
                            }
                        }
                    }
                }
                Event::Mouse(mouse) => {
                    // Click on approval dialog
                    if let MouseEventKind::Down(_) = mouse.kind {
                        if has_approval_pending {
                            let term_h = crossterm::terminal::size().map(|(_, h)| h).unwrap_or(24);
                            let dialog_h = 13u16;
                            let dialog_top = term_h.saturating_sub(dialog_h) / 2;
                            let first_option_row = dialog_top + 3;
                            if mouse.row >= first_option_row
                                && mouse.row < first_option_row + APPROVAL_OPTIONS.len() as u16
                            {
                                let idx = (mouse.row - first_option_row) as usize;
                                dialog.approval_selected = idx;
                                crate::key_handlers::send_approval_response(
                                    client,
                                    &active_thread_id,
                                    APPROVAL_OPTIONS[idx].1,
                                )
                                .await;
                            }
                        }
                    } else {
                        dispatch_mouse_owned(
                            client,
                            has_active_thread,
                            has_approval_pending,
                            dialog.pending_customize.is_some(),
                            mouse.kind,
                        )
                        .await;
                    }
                }
                _ => {}
            }
        }
    }
}

/// Dispatch text editing commands through UiStore via the broker.
/// Called when no InputStore binding matches in insert mode.
/// Takes owned cursor/input data extracted from ViewState.
async fn dispatch_text_edit_owned(
    client: &ox_broker::ClientHandle,
    cursor: usize,
    input_len: usize,
    modifiers: KeyModifiers,
    code: KeyCode,
) {
    use structfs_core_store::path;

    match (modifiers, code) {
        (KeyModifiers::CONTROL, KeyCode::Char('a')) => {
            let _ = client
                .write(&path!("ui/set_input"), cmd!("cursor" => 0_i64))
                .await;
        }
        (KeyModifiers::CONTROL, KeyCode::Char('e')) => {
            let _ = client
                .write(&path!("ui/set_input"), cmd!("cursor" => input_len as i64))
                .await;
        }
        (_, KeyCode::Char(c)) => {
            let _ = client
                .write(
                    &path!("ui/insert_char"),
                    cmd!("char" => c.to_string(), "at" => cursor as i64),
                )
                .await;
        }
        (_, KeyCode::Enter) => {
            let _ = client
                .write(
                    &path!("ui/insert_char"),
                    cmd!("char" => "\n", "at" => cursor as i64),
                )
                .await;
        }
        (_, KeyCode::Backspace) => {
            let _ = client.write(&path!("ui/delete_char"), cmd!()).await;
        }
        (_, KeyCode::Left) => {
            let pos = cursor.saturating_sub(1);
            let _ = client
                .write(&path!("ui/set_input"), cmd!("cursor" => pos as i64))
                .await;
        }
        (_, KeyCode::Right) => {
            let pos = (cursor + 1).min(input_len);
            let _ = client
                .write(&path!("ui/set_input"), cmd!("cursor" => pos as i64))
                .await;
        }
        _ => {}
    }
}

/// Dispatch mouse events through UiStore via the broker.
/// Takes owned state extracted from ViewState.
async fn dispatch_mouse_owned(
    client: &ox_broker::ClientHandle,
    has_active_thread: bool,
    has_pending_approval: bool,
    has_pending_customize: bool,
    kind: MouseEventKind,
) {
    use structfs_core_store::path;

    if has_pending_approval || has_pending_customize {
        return;
    }

    match kind {
        MouseEventKind::ScrollUp => {
            if has_active_thread {
                let _ = client.write(&path!("ui/scroll_up"), cmd!()).await;
            } else {
                let _ = client.write(&path!("ui/select_prev"), cmd!()).await;
            }
        }
        MouseEventKind::ScrollDown => {
            if has_active_thread {
                let _ = client.write(&path!("ui/scroll_down"), cmd!()).await;
            } else {
                let _ = client.write(&path!("ui/select_next"), cmd!()).await;
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Search text editing — dispatched through UiStore via broker
// ---------------------------------------------------------------------------

/// Dispatch search text editing through UiStore via the broker.
async fn dispatch_search_edit(
    client: &ox_broker::ClientHandle,
    modifiers: KeyModifiers,
    code: KeyCode,
) {
    use structfs_core_store::path;

    match (modifiers, code) {
        (_, KeyCode::Enter) => {
            let _ = client.write(&path!("ui/search_save_chip"), cmd!()).await;
        }
        (KeyModifiers::CONTROL, KeyCode::Char('u')) => {
            let _ = client.write(&path!("ui/search_clear"), cmd!()).await;
        }
        (_, KeyCode::Backspace) => {
            let _ = client.write(&path!("ui/search_delete_char"), cmd!()).await;
        }
        (_, KeyCode::Char(c)) => {
            let _ = client
                .write(
                    &path!("ui/search_insert_char"),
                    cmd!("char" => c.to_string()),
                )
                .await;
        }
        _ => {}
    }
}
