use crate::app::App;
use crate::editor::{
    EditorMode, InputSession, execute_command_input, flush_pending_edits,
    handle_editor_command_key, handle_editor_insert_key, handle_editor_normal_key,
};
use crate::settings_state::SettingsState;
use crate::theme::Theme;
use crate::types::{APPROVAL_OPTIONS, CustomizeState};
use crate::view_state::fetch_view_state;
use crossterm::event::{self, Event, KeyCode, KeyModifiers, MouseEventKind};
use ox_ui::text_input_store::EditSource;
use std::time::Duration;
use structfs_core_store::Writer as StructWriter;

/// Dialog-local state, owned by the event loop (not App, not broker).
pub(crate) struct DialogState {
    pub approval_selected: usize,
    pub pending_customize: Option<CustomizeState>,
    pub show_shortcuts: bool,
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
        show_shortcuts: false,
    };
    let mut input_session = InputSession::new();
    let mut text_input_view = crate::text_input_view::TextInputView::new();
    let mut prev_mode = String::new();
    let mut settings = if needs_setup {
        // Navigate to settings screen via broker
        client
            .write(&structfs_core_store::path!("ui/go_to_settings"), cmd!())
            .await
            .ok();
        SettingsState::new_wizard()
    } else {
        SettingsState::new()
    };

    loop {
        // Poll pending async test connection
        if let Some(ref mut rx) = settings.pending_test {
            match rx.try_recv() {
                Ok(result) => {
                    match result.test {
                        Ok((dialect, ms)) => {
                            settings.test_status = crate::settings_state::TestStatus::Success(
                                format!("Connected ({dialect}, {ms}ms)"),
                            );
                        }
                        Err(e) => {
                            settings.test_status = crate::settings_state::TestStatus::Failed(e);
                        }
                    }
                    match result.models {
                        Ok(models) => {
                            settings.discovered_models = models;
                            settings.model_picker_idx = None;
                        }
                        Err(_) => {
                            settings.discovered_models.clear();
                        }
                    }
                    settings.pending_test = None;
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    // Still in progress — will check next frame
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    settings.test_status =
                        crate::settings_state::TestStatus::Failed("Test cancelled".into());
                    settings.pending_test = None;
                }
            }
        }

        // 1. Fetch ViewState, draw, extract owned data needed after drop.
        //
        // ViewState borrows from App so we scope it tightly: draw, then
        // extract the owned fields we need for pending-action handling and
        // event dispatch, then drop the borrow.
        let pending_action: Option<String>;
        let screen_owned: String;
        let mode_owned: String;
        let insert_context_owned: Option<String>;
        let has_active_thread: bool;
        let active_thread_id: Option<String>;
        let selected_thread_id: Option<String>;
        let search_active: bool;
        let has_approval_pending: bool;

        let mut content_height: Option<usize> = None;
        let mut viewport_height: usize = 0;
        {
            let vs = fetch_view_state(
                client,
                app,
                &dialog,
                input_session.editor_mode,
                &input_session.command_buffer,
            )
            .await;

            // Detect mode transitions for InputSession sync
            if vs.mode != prev_mode {
                if vs.mode == "insert" {
                    // Entering insert mode — initialize InputSession from broker
                    input_session.init_from(vs.input.clone(), vs.cursor);
                    input_session.editor_mode = EditorMode::Insert;
                } else if prev_mode == "insert" {
                    // Exiting insert mode — flush any pending edits
                    flush_pending_edits(&mut input_session, client).await;
                }
                prev_mode = vs.mode.clone();
            }

            // Set row_count in UiStore (for inbox navigation bounds)
            // Only write on inbox screen — thread screen has no row selection.
            if vs.screen == "inbox" {
                let row_count = vs.inbox_threads.len() as i64;
                let _ = client
                    .write(&path!("ui/set_row_count"), cmd!("count" => row_count))
                    .await;
            }

            // Prepare TextInputView from InputSession (optimistic local state)
            text_input_view.set_state(&input_session.content, input_session.cursor);

            // Draw
            terminal.draw(|frame| {
                let (ch, vh) = crate::tui::draw(frame, &vs, &settings, theme, &mut text_input_view);
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
            screen_owned = vs.screen.clone();
            mode_owned = vs.mode.clone();
            insert_context_owned = vs.insert_context.clone();
            has_active_thread = vs.active_thread.is_some();
            active_thread_id = vs.active_thread.clone();
            selected_thread_id = vs.inbox_threads.get(vs.selected_row).map(|t| t.id.clone());
            search_active = vs.search_active;
            has_approval_pending = vs.approval_pending.is_some();
        }
        // vs is now dropped — safe to mutate app

        // 2. Handle pending_action
        if let Some(action) = &pending_action {
            match action.as_str() {
                "send_input" => {
                    // Flush pending edits so broker is in sync
                    flush_pending_edits(&mut input_session, client).await;
                    let submit_text = input_session.content.clone();

                    if insert_context_owned.as_deref() == Some("command") {
                        // Command mode: parse input as command invocation
                        execute_command_input(&submit_text, client).await;
                    } else {
                        let new_tid = app.send_input_with_text(
                            submit_text,
                            &mode_owned,
                            insert_context_owned.as_deref(),
                            active_thread_id.as_deref(),
                        );
                        // If compose created a new thread, open it in UiStore
                        if let Some(tid) = new_tid {
                            let _ = client
                                .write(&path!("ui/open"), cmd!("thread_id" => tid))
                                .await;
                        }
                    }
                    // Clear input and exit insert mode through broker
                    let _ = client.write(&path!("ui/clear_input"), cmd!()).await;
                    let _ = client.write(&path!("ui/exit_insert"), cmd!()).await;
                    // Reset local InputSession
                    input_session.reset_after_submit();
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
                        let update_path = ox_path::oxpath!("threads", id);
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
                    // Shortcuts modal — dismiss on ? or Esc, swallow all other keys
                    if dialog.show_shortcuts {
                        if let Some(key_str) = encode_key(key.modifiers, key.code) {
                            if key_str == "?" || key_str == "Esc" || key_str == "Ctrl+q" {
                                dialog.show_shortcuts = false;
                            }
                        }
                    }
                    // Customize dialog — bypass broker entirely
                    else if dialog.pending_customize.is_some() {
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
                        if screen == "settings" && mode == "normal" && settings.editing.is_some() {
                            use crate::settings_state::{DIALECTS, TestStatus};
                            let inbox_root = app.pool.inbox_root().to_path_buf();
                            let keys_dir = inbox_root.join("keys");

                            // Use an enum to signal post-match actions that
                            // require dropping the &mut borrow first.
                            enum EditAction {
                                None,
                                Cancel,
                                Save {
                                    name: String,
                                    provider: String,
                                    endpoint: Option<String>,
                                    key: String,
                                },
                                Handled,
                            }

                            let action = if let Some(ref mut editing) = settings.editing {
                                match key_str.as_str() {
                                    "Tab" | "Down" => {
                                        editing.focus = (editing.focus + 1) % 4;
                                        EditAction::Handled
                                    }
                                    "Shift+Tab" | "Up" => {
                                        editing.focus = if editing.focus == 0 {
                                            3
                                        } else {
                                            editing.focus - 1
                                        };
                                        EditAction::Handled
                                    }
                                    "Esc" => EditAction::Cancel,
                                    "Enter" => {
                                        if !editing.name.is_empty() {
                                            EditAction::Save {
                                                name: editing.name.clone(),
                                                provider: DIALECTS[editing.dialect].to_string(),
                                                endpoint: if editing.endpoint.is_empty() {
                                                    None
                                                } else {
                                                    Some(editing.endpoint.clone())
                                                },
                                                key: editing.key.clone(),
                                            }
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
                                    "Ctrl+t" => {
                                        EditAction::None // handled below as test connection
                                    }
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
                                EditAction::Save {
                                    name,
                                    provider,
                                    endpoint,
                                    key,
                                } => {
                                    use structfs_core_store::{Record, Value};
                                    tracing::info!(
                                        name = %name,
                                        provider = %provider,
                                        has_endpoint = endpoint.is_some(),
                                        has_key = !key.is_empty(),
                                        "saving account via ConfigStore"
                                    );

                                    // Write account through ConfigStore (not direct file)
                                    let provider_path = ox_path::oxpath!(
                                        "config", "gate", "accounts", name, "provider"
                                    );
                                    client
                                        .write(
                                            &provider_path,
                                            Record::parsed(Value::String(provider)),
                                        )
                                        .await
                                        .ok();
                                    if let Some(ep) = endpoint {
                                        let ep_path = ox_path::oxpath!(
                                            "config", "gate", "accounts", name, "endpoint"
                                        );
                                        client
                                            .write(&ep_path, Record::parsed(Value::String(ep)))
                                            .await
                                            .ok();
                                    }

                                    // Write key to ConfigStore (in-memory for session)
                                    // AND to key file (for persistence across sessions)
                                    if !key.is_empty() {
                                        let key_path = ox_path::oxpath!(
                                            "config", "gate", "accounts", name, "key"
                                        );
                                        client
                                            .write(
                                                &key_path,
                                                Record::parsed(Value::String(key.clone())),
                                            )
                                            .await
                                            .ok();
                                        crate::config::write_key_file(&keys_dir, &name, &key).ok();
                                    }

                                    // If default account doesn't exist, set it to this one
                                    let current_default = client
                                        .read(&path!("config/gate/defaults/account"))
                                        .await
                                        .ok()
                                        .flatten()
                                        .and_then(|r| match r.as_value() {
                                            Some(Value::String(s)) => Some(s.clone()),
                                            _ => None,
                                        })
                                        .unwrap_or_default();
                                    let default_exists = client
                                        .read(&ox_path::oxpath!(
                                            "config",
                                            "gate",
                                            "accounts",
                                            current_default,
                                            "provider"
                                        ))
                                        .await
                                        .ok()
                                        .flatten()
                                        .is_some();
                                    if !default_exists {
                                        tracing::info!(
                                            old_default = %current_default,
                                            new_default = %name,
                                            "auto-setting default account"
                                        );
                                        client
                                            .write(
                                                &path!("config/gate/defaults/account"),
                                                Record::parsed(Value::String(name.clone())),
                                            )
                                            .await
                                            .ok();
                                    }

                                    // Persist config to disk
                                    client
                                        .write(&path!("config/save"), Record::parsed(Value::Null))
                                        .await
                                        .ok();

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
                                        if *step == WizardStep::AddAccount {
                                            *step = WizardStep::SetDefaults;
                                            settings.focus =
                                                crate::settings_state::SettingsFocus::Defaults;
                                        }
                                    }
                                    continue;
                                }
                                EditAction::Handled => {
                                    continue;
                                }
                                EditAction::None => {}
                            }

                            // Handle Ctrl+t for test connection in edit dialog
                            // (done after match so the &mut borrow on editing is dropped)
                            if key_str == "Ctrl+t" {
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
                                        let api_key_for_test = editing.key.clone();

                                        settings.test_status = TestStatus::Testing;
                                        let (tx, rx) = tokio::sync::oneshot::channel();
                                        settings.pending_test = Some(rx);

                                        let pc = provider_config;
                                        let key = api_key_for_test;
                                        tokio::spawn(async move {
                                            let test =
                                                crate::transport::test_connection_async(&pc, &key)
                                                    .await;
                                            let models = if test.is_ok() {
                                                crate::transport::fetch_model_catalog_async(
                                                    &pc, &key,
                                                )
                                                .await
                                            } else {
                                                Err("skipped".into())
                                            };
                                            let _ = tx.send(crate::settings_state::TestResult {
                                                test,
                                                models,
                                            });
                                        });
                                    }
                                }
                                continue;
                            }
                        }

                        // Settings screen navigation (before broker dispatch)
                        if screen == "settings" && mode == "normal" && settings.editing.is_none() {
                            use crate::settings_state::SettingsFocus;

                            // Delete confirmation — absorb all keys until y/n
                            if settings.delete_confirming {
                                if key_str == "y" {
                                    if let Some(acct) =
                                        settings.accounts.get(settings.selected_account)
                                    {
                                        use structfs_core_store::{Record, Value};
                                        let name = acct.name.clone();
                                        let inbox_root = app.pool.inbox_root().to_path_buf();
                                        let keys_dir = inbox_root.join("keys");

                                        // Delete account through ConfigStore (Null = delete)
                                        let provider_path = ox_path::oxpath!(
                                            "config", "gate", "accounts", name, "provider"
                                        );
                                        client
                                            .write(&provider_path, Record::parsed(Value::Null))
                                            .await
                                            .ok();
                                        let ep_path = ox_path::oxpath!(
                                            "config", "gate", "accounts", name, "endpoint"
                                        );
                                        client
                                            .write(&ep_path, Record::parsed(Value::Null))
                                            .await
                                            .ok();
                                        let key_path = ox_path::oxpath!(
                                            "config", "gate", "accounts", name, "key"
                                        );
                                        client
                                            .write(&key_path, Record::parsed(Value::Null))
                                            .await
                                            .ok();

                                        // Update default if deleted account was default
                                        if acct.is_default {
                                            let alt = settings
                                                .accounts
                                                .iter()
                                                .find(|a| a.name != name)
                                                .map(|a| a.name.clone())
                                                .unwrap_or_default();
                                            client
                                                .write(
                                                    &path!("config/gate/defaults/account"),
                                                    Record::parsed(Value::String(alt)),
                                                )
                                                .await
                                                .ok();
                                        }

                                        // Persist and delete key file
                                        client
                                            .write(
                                                &path!("config/save"),
                                                Record::parsed(Value::Null),
                                            )
                                            .await
                                            .ok();
                                        crate::config::delete_key_file(&keys_dir, &name).ok();

                                        let config = crate::config::resolve_config(
                                            &inbox_root,
                                            &crate::config::CliOverrides::default(),
                                        );
                                        settings.refresh_accounts(&config, &keys_dir);
                                    }
                                }
                                settings.delete_confirming = false;
                                continue;
                            }

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
                                        settings.editing =
                                            Some(crate::settings_state::AccountEditFields {
                                                name: String::new(),
                                                dialect: 0,
                                                endpoint: String::new(),
                                                key: String::new(),
                                                focus: 0,
                                                is_new: true,
                                            });
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
                                            let inbox_root = app.pool.inbox_root().to_path_buf();
                                            let keys_dir = inbox_root.join("keys");
                                            let key_val =
                                                crate::config::read_key_file(&keys_dir, &acct.name)
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
                                            settings.editing =
                                                Some(crate::settings_state::AccountEditFields {
                                                    name: acct.name.clone(),
                                                    dialect: dialect_idx,
                                                    endpoint,
                                                    key: key_val,
                                                    focus: 0,
                                                    is_new: false,
                                                });
                                            settings.test_status =
                                                crate::settings_state::TestStatus::Idle;
                                        }
                                    }
                                    true
                                }
                                "Enter"
                                    if settings.wizard
                                        == Some(crate::settings_state::WizardStep::Done) =>
                                {
                                    settings.wizard = None;
                                    // Navigate back to inbox
                                    client
                                        .write(
                                            &structfs_core_store::path!("ui/go_to_inbox"),
                                            cmd!(),
                                        )
                                        .await
                                        .ok();
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
                                            }
                                        }
                                        1 => {
                                            if !settings.discovered_models.is_empty() {
                                                let idx = settings.model_picker_idx.unwrap_or(0);
                                                let new_idx = if idx == 0 {
                                                    settings.discovered_models.len() - 1
                                                } else {
                                                    idx - 1
                                                };
                                                settings.model_picker_idx = Some(new_idx);
                                                settings.default_model =
                                                    settings.discovered_models[new_idx].id.clone();
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
                                            }
                                        }
                                        1 => {
                                            if !settings.discovered_models.is_empty() {
                                                let idx = settings.model_picker_idx.unwrap_or(0);
                                                let new_idx =
                                                    (idx + 1) % settings.discovered_models.len();
                                                settings.model_picker_idx = Some(new_idx);
                                                settings.default_model =
                                                    settings.discovered_models[new_idx].id.clone();
                                            }
                                        }
                                        _ => {}
                                    }
                                    true
                                }
                                "Backspace"
                                    if settings.focus == SettingsFocus::Defaults
                                        && settings.defaults_focus == 1 =>
                                {
                                    settings.default_model.pop();
                                    settings.model_picker_idx = None;
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
                                    let model = settings.default_model.clone();
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
                                        .write(&path!("config/save"), Record::parsed(Value::Null))
                                        .await
                                        .ok();

                                    // Flash "Saved" confirmation
                                    settings.save_flash_until = Some(
                                        std::time::Instant::now()
                                            + std::time::Duration::from_secs(2),
                                    );

                                    // Advance wizard if active
                                    if let Some(ref mut step) = settings.wizard {
                                        if *step == crate::settings_state::WizardStep::SetDefaults {
                                            *step = crate::settings_state::WizardStep::Done;
                                        }
                                    }
                                    true
                                }
                                "Esc" | "q" if settings.wizard.is_some() => {
                                    // Allow skipping wizard — go to inbox
                                    settings.wizard = None;
                                    client
                                        .write(
                                            &structfs_core_store::path!("ui/go_to_inbox"),
                                            cmd!(),
                                        )
                                        .await
                                        .ok();
                                    true
                                }
                                "d" => {
                                    if settings.focus == SettingsFocus::Accounts
                                        && !settings.accounts.is_empty()
                                    {
                                        settings.delete_confirming = true;
                                    }
                                    true
                                }
                                "*" => {
                                    if settings.focus == SettingsFocus::Accounts {
                                        if let Some(acct) =
                                            settings.accounts.get(settings.selected_account)
                                        {
                                            let name = acct.name.clone();
                                            use structfs_core_store::{Record, Value};
                                            client
                                                .write(
                                                    &path!("config/gate/defaults/account"),
                                                    Record::parsed(Value::String(name)),
                                                )
                                                .await
                                                .ok();
                                            client
                                                .write(
                                                    &path!("config/save"),
                                                    Record::parsed(Value::Null),
                                                )
                                                .await
                                                .ok();
                                            let inbox_root = app.pool.inbox_root().to_path_buf();
                                            let config = crate::config::resolve_config(
                                                &inbox_root,
                                                &crate::config::CliOverrides::default(),
                                            );
                                            settings.refresh_accounts(
                                                &config,
                                                &inbox_root.join("keys"),
                                            );
                                        }
                                    }
                                    true
                                }
                                "t" | "Ctrl+t" => {
                                    if settings.focus == SettingsFocus::Accounts {
                                        if let Some(acct) =
                                            settings.accounts.get(settings.selected_account)
                                        {
                                            let inbox_root = app.pool.inbox_root().to_path_buf();
                                            let keys_dir = inbox_root.join("keys");
                                            let key =
                                                crate::config::read_key_file(&keys_dir, &acct.name)
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
                                                let entry = config.gate.accounts.get(&acct.name);
                                                let dialect = entry
                                                    .map(|e| e.provider.as_str())
                                                    .unwrap_or("anthropic");
                                                let mut provider_config = match dialect {
                                                    "openai" => ox_gate::ProviderConfig::openai(),
                                                    _ => ox_gate::ProviderConfig::anthropic(),
                                                };
                                                if let Some(ep) =
                                                    entry.and_then(|e| e.endpoint.as_ref())
                                                {
                                                    provider_config.endpoint = ep.clone();
                                                }

                                                settings.test_status =
                                                    crate::settings_state::TestStatus::Testing;
                                                let (tx, rx) = tokio::sync::oneshot::channel();
                                                settings.pending_test = Some(rx);

                                                let pc = provider_config;
                                                let k = key;
                                                tokio::spawn(async move {
                                                    let test =
                                                        crate::transport::test_connection_async(
                                                            &pc, &k,
                                                        )
                                                        .await;
                                                    let models = if test.is_ok() {
                                                        crate::transport::fetch_model_catalog_async(
                                                            &pc, &k,
                                                        )
                                                        .await
                                                    } else {
                                                        Err("skipped".into())
                                                    };
                                                    let _ = tx.send(
                                                        crate::settings_state::TestResult {
                                                            test,
                                                            models,
                                                        },
                                                    );
                                                });
                                            }
                                        }
                                    }
                                    true
                                }
                                other
                                    if settings.focus == SettingsFocus::Defaults
                                        && settings.defaults_focus == 1
                                        && other.len() == 1
                                        && !other.chars().next().unwrap().is_control() =>
                                {
                                    settings.default_model.push(other.chars().next().unwrap());
                                    settings.model_picker_idx = None;
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

                        // ? in normal mode toggles shortcuts modal
                        if mode == "normal" && key_str == "?" {
                            dialog.show_shortcuts = !dialog.show_shortcuts;
                            continue;
                        }

                        // In editor sub-modes (compose/reply), intercept ESC
                        // before the InputStore can fire ui/exit_insert
                        if mode == "insert"
                            && key_str == "Esc"
                            && insert_context_owned.as_deref() != Some("search")
                            && insert_context_owned.as_deref() != Some("command")
                        {
                            match input_session.editor_mode {
                                EditorMode::Insert => {
                                    input_session.editor_mode = EditorMode::Normal;
                                    continue;
                                }
                                EditorMode::Command => {
                                    input_session.command_buffer.clear();
                                    input_session.editor_mode = EditorMode::Normal;
                                    continue;
                                }
                                EditorMode::Normal => {
                                    // Let ESC fall through to InputStore → ui/exit_insert
                                }
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
                            } else if insert_context_owned.as_deref() == Some("command") {
                                // Command mode uses the same text editing as editor-insert
                                handle_editor_insert_key(
                                    &mut input_session,
                                    key.modifiers,
                                    key.code,
                                );
                            } else {
                                // Compose/reply: vim-style editor with sub-modes
                                // (ESC from editor-insert → editor-normal is intercepted
                                //  above the InputStore dispatch, so we only see non-ESC here)
                                match input_session.editor_mode {
                                    EditorMode::Insert => {
                                        handle_editor_insert_key(
                                            &mut input_session,
                                            key.modifiers,
                                            key.code,
                                        );
                                    }
                                    EditorMode::Normal => {
                                        let tw = terminal.get_frame().area().width;
                                        handle_editor_normal_key(
                                            &mut input_session,
                                            app,
                                            client,
                                            tw,
                                            key.code,
                                        )
                                        .await;
                                    }
                                    EditorMode::Command => {
                                        handle_editor_command_key(
                                            &mut input_session,
                                            app,
                                            client,
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
                    // Border drag handling
                    if mode_owned == "insert" {
                        match mouse.kind {
                            MouseEventKind::Down(_) if text_input_view.is_on_border(mouse.row) => {
                                text_input_view.start_border_drag(mouse.row);
                            }
                            MouseEventKind::Drag(_) if text_input_view.is_dragging() => {
                                text_input_view.update_border_drag(mouse.row);
                            }
                            MouseEventKind::Up(_) if text_input_view.is_dragging() => {
                                text_input_view.end_border_drag();
                            }
                            // Click in input area — move cursor
                            MouseEventKind::Down(_) => {
                                if let Some(byte_pos) =
                                    text_input_view.click_to_byte_offset(mouse.column, mouse.row)
                                {
                                    input_session.cursor = byte_pos;
                                }
                            }
                            // Scroll in input area
                            MouseEventKind::ScrollUp
                                if text_input_view.contains(mouse.column, mouse.row) =>
                            {
                                text_input_view.scroll_by(-3);
                            }
                            MouseEventKind::ScrollDown
                                if text_input_view.contains(mouse.column, mouse.row) =>
                            {
                                text_input_view.scroll_by(3);
                            }
                            _ => {
                                // Fall through to normal mouse dispatch
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
                    } else
                    // Click on settings edit dialog
                    if let MouseEventKind::Down(_) = mouse.kind {
                        if screen_owned == "settings" && settings.editing.is_some() {
                            let term_size = crossterm::terminal::size().unwrap_or((80, 24));
                            let dialog_h = 10u16;
                            let dialog_w = term_size.0 * 60 / 100;
                            let dialog_top = term_size.1.saturating_sub(dialog_h) / 2;
                            let dialog_left = (term_size.0.saturating_sub(dialog_w)) / 2;
                            // Fields start at row offset 1 inside the bordered dialog
                            // Row 0: Name, Row 1: Dialect, Row 2: Endpoint, Row 3: Key
                            let field_first_row = dialog_top + 1;
                            if mouse.row >= field_first_row
                                && mouse.row < field_first_row + 4
                                && mouse.column >= dialog_left
                                && mouse.column < dialog_left + dialog_w
                            {
                                let field = (mouse.row - field_first_row) as usize;
                                if let Some(ref mut editing) = settings.editing {
                                    editing.focus = field;
                                }
                            }
                        }
                    }
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
                Event::Paste(text) => {
                    if mode_owned == "insert" && insert_context_owned.as_deref() != Some("search") {
                        input_session.insert(&text, EditSource::Paste);
                    }
                }
                _ => {}
            }

            // Batch flush pending edits after processing this event
            flush_pending_edits(&mut input_session, client).await;
        }
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
