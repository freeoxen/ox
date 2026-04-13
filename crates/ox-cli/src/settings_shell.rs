//! Settings screen shell — owns ephemeral TUI state for the Settings screen.
//!
//! `SettingsShell` wraps `SettingsState` and adds poll / ensure helpers that
//! were previously inlined in `event_loop.rs`.

use crate::settings_state::{DIALECTS, SettingsFocus, SettingsState, TestStatus};
use crate::shell::Outcome;
use ox_path::oxpath;
use ox_types::{GlobalCommand, UiCommand};
use structfs_core_store::{Record, Value};

// -----------------------------------------------------------------------
// SettingsShell — event-loop-owned wrapper
// -----------------------------------------------------------------------

/// Settings screen local state, owned by the event loop.
pub(crate) struct SettingsShell {
    pub state: SettingsState,
}

impl SettingsShell {
    pub fn new() -> Self {
        Self {
            state: SettingsState::new(),
        }
    }

    pub fn new_wizard() -> Self {
        Self {
            state: SettingsState::new_wizard(),
        }
    }

    /// Poll the pending async test connection, updating status on completion.
    pub fn poll(&mut self) {
        if let Some(ref mut rx) = self.state.pending_test {
            match rx.try_recv() {
                Ok(result) => {
                    match result.test {
                        Ok((dialect, ms)) => {
                            self.state.test_status =
                                TestStatus::Success(format!("Connected ({dialect}, {ms}ms)"));
                        }
                        Err(e) => {
                            self.state.test_status = TestStatus::Failed(e);
                        }
                    }
                    match result.models {
                        Ok(models) => {
                            self.state.discovered_models = models;
                            self.state.model_picker_idx = None;
                        }
                        Err(_) => {
                            self.state.discovered_models.clear();
                        }
                    }
                    self.state.pending_test = None;
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Empty) => {
                    // Still in progress — will check next frame
                }
                Err(tokio::sync::oneshot::error::TryRecvError::Closed) => {
                    self.state.test_status = TestStatus::Failed("Test cancelled".into());
                    self.state.pending_test = None;
                }
            }
        }
    }

    /// Populate accounts from config if the list is empty and we are on the
    /// Settings screen.
    pub fn ensure_accounts(&mut self, inbox_root: &std::path::Path) {
        if self.state.accounts.is_empty() {
            let config =
                crate::config::resolve_config(inbox_root, &crate::config::CliOverrides::default());
            self.state
                .refresh_accounts(&config, &inbox_root.join("keys"));
        }
    }
}

// -----------------------------------------------------------------------
// Key handling
// -----------------------------------------------------------------------

/// Handle a key event on the Settings screen (normal mode).
///
/// Returns `Outcome::Handled` when the key was consumed, `Outcome::Ignored`
/// when the event loop should fall through to global dispatch.
pub(crate) async fn handle_key(
    settings: &mut SettingsState,
    key_str: &str,
    client: &ox_broker::ClientHandle,
    inbox_root: &std::path::Path,
) -> Outcome {
    // ---------- edit dialog ----------
    if settings.editing.is_some() {
        return handle_edit_dialog_key(settings, key_str, client, inbox_root).await;
    }

    // ---------- delete confirmation ----------
    if settings.delete_confirming {
        return handle_delete_confirm_key(settings, key_str, client, inbox_root).await;
    }

    // ---------- navigation ----------
    handle_navigation_key(settings, key_str, client, inbox_root).await
}

// -----------------------------------------------------------------------
// Edit dialog
// -----------------------------------------------------------------------

/// Post-match signal — avoids holding &mut borrow across actions.
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

async fn handle_edit_dialog_key(
    settings: &mut SettingsState,
    key_str: &str,
    client: &ox_broker::ClientHandle,
    inbox_root: &std::path::Path,
) -> Outcome {
    let keys_dir = inbox_root.join("keys");

    let action = if let Some(ref mut editing) = settings.editing {
        match key_str {
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
                if other.len() == 1 && !other.chars().next().unwrap().is_control() {
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

    // &mut borrow on editing is now dropped — safe to mutate settings.editing.
    match action {
        EditAction::Cancel => {
            settings.editing = None;
            settings.test_status = TestStatus::Idle;
            return Outcome::Handled;
        }
        EditAction::Save {
            name,
            provider,
            endpoint,
            key,
        } => {
            tracing::info!(
                name = %name,
                provider = %provider,
                has_endpoint = endpoint.is_some(),
                has_key = !key.is_empty(),
                "saving account via ConfigStore"
            );

            // Write account through ConfigStore (not direct file)
            let provider_path = ox_path::oxpath!("config", "gate", "accounts", name, "provider");
            client.write_typed(&provider_path, &provider).await.ok();
            if let Some(ep) = endpoint {
                let ep_path = ox_path::oxpath!("config", "gate", "accounts", name, "endpoint");
                client.write_typed(&ep_path, &ep).await.ok();
            }

            // Write key to ConfigStore + key file
            if !key.is_empty() {
                let key_path = ox_path::oxpath!("config", "gate", "accounts", name, "key");
                client.write_typed(&key_path, &key).await.ok();
                crate::config::write_key_file(&keys_dir, &name, &key).ok();
            }

            // If default account doesn't exist, set it to this one
            let current_default = client
                .read(&oxpath!("config", "gate", "defaults", "account"))
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
                    .write_typed(&oxpath!("config", "gate", "defaults", "account"), &name)
                    .await
                    .ok();
            }

            // Persist config to disk
            client
                .write(&oxpath!("config", "save"), Record::parsed(Value::Null))
                .await
                .ok();

            settings.editing = None;
            settings.test_status = TestStatus::Idle;
            let config =
                crate::config::resolve_config(inbox_root, &crate::config::CliOverrides::default());
            settings.refresh_accounts(&config, &keys_dir);
            // Advance wizard after first account save
            if let Some(ref mut step) = settings.wizard {
                use crate::settings_state::WizardStep;
                if *step == WizardStep::AddAccount {
                    *step = WizardStep::SetDefaults;
                    settings.focus = SettingsFocus::Defaults;
                }
            }
            return Outcome::Handled;
        }
        EditAction::Handled => {
            return Outcome::Handled;
        }
        EditAction::None => {}
    }

    // Handle Ctrl+t for test connection in edit dialog
    if key_str == "Ctrl+t" {
        if let Some(ref editing) = settings.editing {
            if editing.key.is_empty() {
                settings.test_status = TestStatus::Failed("No API key entered".into());
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
                    let test = crate::transport::test_connection_async(&pc, &key).await;
                    let models = if test.is_ok() {
                        crate::transport::fetch_model_catalog_async(&pc, &key).await
                    } else {
                        Err("skipped".into())
                    };
                    let _ = tx.send(crate::settings_state::TestResult { test, models });
                });
            }
        }
        return Outcome::Handled;
    }

    Outcome::Ignored
}

// -----------------------------------------------------------------------
// Delete confirmation
// -----------------------------------------------------------------------

async fn handle_delete_confirm_key(
    settings: &mut SettingsState,
    key_str: &str,
    client: &ox_broker::ClientHandle,
    inbox_root: &std::path::Path,
) -> Outcome {
    if key_str == "y" {
        if let Some(acct) = settings.accounts.get(settings.selected_account) {
            let name = acct.name.clone();
            let keys_dir = inbox_root.join("keys");

            // Delete account through ConfigStore (Null = delete)
            let provider_path = ox_path::oxpath!("config", "gate", "accounts", name, "provider");
            client
                .write(&provider_path, Record::parsed(Value::Null))
                .await
                .ok();
            let ep_path = ox_path::oxpath!("config", "gate", "accounts", name, "endpoint");
            client
                .write(&ep_path, Record::parsed(Value::Null))
                .await
                .ok();
            let key_path = ox_path::oxpath!("config", "gate", "accounts", name, "key");
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
                    .write_typed(&oxpath!("config", "gate", "defaults", "account"), &alt)
                    .await
                    .ok();
            }

            // Persist and delete key file
            client
                .write(&oxpath!("config", "save"), Record::parsed(Value::Null))
                .await
                .ok();
            crate::config::delete_key_file(&keys_dir, &name).ok();

            let config =
                crate::config::resolve_config(inbox_root, &crate::config::CliOverrides::default());
            settings.refresh_accounts(&config, &keys_dir);
        }
    }
    settings.delete_confirming = false;
    Outcome::Handled
}

// -----------------------------------------------------------------------
// Navigation (accounts / defaults / wizard)
// -----------------------------------------------------------------------

async fn handle_navigation_key(
    settings: &mut SettingsState,
    key_str: &str,
    client: &ox_broker::ClientHandle,
    inbox_root: &std::path::Path,
) -> Outcome {
    let handled = match key_str {
        "j" | "Down" => {
            if settings.focus == SettingsFocus::Accounts && !settings.accounts.is_empty() {
                settings.selected_account =
                    (settings.selected_account + 1).min(settings.accounts.len() - 1);
            } else if settings.focus == SettingsFocus::Defaults {
                settings.defaults_focus = (settings.defaults_focus + 1).min(2);
            }
            true
        }
        "k" | "Up" => {
            if settings.focus == SettingsFocus::Accounts {
                settings.selected_account = settings.selected_account.saturating_sub(1);
            } else if settings.focus == SettingsFocus::Defaults {
                settings.defaults_focus = settings.defaults_focus.saturating_sub(1);
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
                settings.editing = Some(crate::settings_state::AccountEditFields {
                    name: String::new(),
                    dialect: 0,
                    endpoint: String::new(),
                    key: String::new(),
                    focus: 0,
                    is_new: true,
                });
                settings.test_status = TestStatus::Idle;
            }
            true
        }
        "e" => {
            if settings.focus == SettingsFocus::Accounts {
                if let Some(acct) = settings.accounts.get(settings.selected_account) {
                    let dialect_idx = DIALECTS
                        .iter()
                        .position(|d| *d == acct.dialect)
                        .unwrap_or(0);
                    let keys_dir = inbox_root.join("keys");
                    let key_val =
                        crate::config::read_key_file(&keys_dir, &acct.name).unwrap_or_default();
                    let config = crate::config::resolve_config(
                        inbox_root,
                        &crate::config::CliOverrides::default(),
                    );
                    let endpoint = config
                        .gate
                        .accounts
                        .get(&acct.name)
                        .and_then(|e| e.endpoint.clone())
                        .unwrap_or_default();
                    settings.editing = Some(crate::settings_state::AccountEditFields {
                        name: acct.name.clone(),
                        dialect: dialect_idx,
                        endpoint,
                        key: key_val,
                        focus: 0,
                        is_new: false,
                    });
                    settings.test_status = TestStatus::Idle;
                }
            }
            true
        }
        "Enter" if settings.wizard == Some(crate::settings_state::WizardStep::Done) => {
            settings.wizard = None;
            client
                .write_typed(&oxpath!("ui"), &UiCommand::Global(GlobalCommand::GoToInbox))
                .await
                .ok();
            true
        }
        "Left" if settings.focus == SettingsFocus::Defaults => {
            match settings.defaults_focus {
                0 => {
                    if !settings.accounts.is_empty() {
                        settings.default_account_idx = if settings.default_account_idx == 0 {
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
                        settings.default_model = settings.discovered_models[new_idx].id.clone();
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
                            (settings.default_account_idx + 1) % settings.accounts.len();
                    }
                }
                1 => {
                    if !settings.discovered_models.is_empty() {
                        let idx = settings.model_picker_idx.unwrap_or(0);
                        let new_idx = (idx + 1) % settings.discovered_models.len();
                        settings.model_picker_idx = Some(new_idx);
                        settings.default_model = settings.discovered_models[new_idx].id.clone();
                    }
                }
                _ => {}
            }
            true
        }
        "Backspace"
            if settings.focus == SettingsFocus::Defaults && settings.defaults_focus == 1 =>
        {
            settings.default_model.pop();
            settings.model_picker_idx = None;
            true
        }
        "Backspace"
            if settings.focus == SettingsFocus::Defaults && settings.defaults_focus == 2 =>
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
            let max_tokens: i64 = settings.default_max_tokens.parse().unwrap_or(4096);

            // Write to ConfigStore via broker
            client
                .write_typed(
                    &oxpath!("config", "gate", "defaults", "account"),
                    &acct_name,
                )
                .await
                .ok();
            client
                .write_typed(&oxpath!("config", "gate", "defaults", "model"), &model)
                .await
                .ok();
            client
                .write_typed(
                    &oxpath!("config", "gate", "defaults", "max_tokens"),
                    &max_tokens,
                )
                .await
                .ok();
            // Persist to disk
            client
                .write(&oxpath!("config", "save"), Record::parsed(Value::Null))
                .await
                .ok();

            // Flash "Saved" confirmation
            settings.save_flash_until =
                Some(std::time::Instant::now() + std::time::Duration::from_secs(2));

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
                .write_typed(&oxpath!("ui"), &UiCommand::Global(GlobalCommand::GoToInbox))
                .await
                .ok();
            true
        }
        "d" => {
            if settings.focus == SettingsFocus::Accounts && !settings.accounts.is_empty() {
                settings.delete_confirming = true;
            }
            true
        }
        "*" => {
            if settings.focus == SettingsFocus::Accounts {
                if let Some(acct) = settings.accounts.get(settings.selected_account) {
                    let name = acct.name.clone();
                    client
                        .write_typed(&oxpath!("config", "gate", "defaults", "account"), &name)
                        .await
                        .ok();
                    client
                        .write(&oxpath!("config", "save"), Record::parsed(Value::Null))
                        .await
                        .ok();
                    let config = crate::config::resolve_config(
                        inbox_root,
                        &crate::config::CliOverrides::default(),
                    );
                    settings.refresh_accounts(&config, &inbox_root.join("keys"));
                }
            }
            true
        }
        "t" | "Ctrl+t" => {
            if settings.focus == SettingsFocus::Accounts {
                if let Some(acct) = settings.accounts.get(settings.selected_account) {
                    let keys_dir = inbox_root.join("keys");
                    let key =
                        crate::config::read_key_file(&keys_dir, &acct.name).unwrap_or_default();
                    if key.is_empty() {
                        settings.test_status = TestStatus::Failed("No key file found".into());
                    } else {
                        let config = crate::config::resolve_config(
                            inbox_root,
                            &crate::config::CliOverrides::default(),
                        );
                        let entry = config.gate.accounts.get(&acct.name);
                        let dialect = entry.map(|e| e.provider.as_str()).unwrap_or("anthropic");
                        let mut provider_config = match dialect {
                            "openai" => ox_gate::ProviderConfig::openai(),
                            _ => ox_gate::ProviderConfig::anthropic(),
                        };
                        if let Some(ep) = entry.and_then(|e| e.endpoint.as_ref()) {
                            provider_config.endpoint = ep.clone();
                        }

                        settings.test_status = TestStatus::Testing;
                        let (tx, rx) = tokio::sync::oneshot::channel();
                        settings.pending_test = Some(rx);

                        let pc = provider_config;
                        let k = key;
                        tokio::spawn(async move {
                            let test = crate::transport::test_connection_async(&pc, &k).await;
                            let models = if test.is_ok() {
                                crate::transport::fetch_model_catalog_async(&pc, &k).await
                            } else {
                                Err("skipped".into())
                            };
                            let _ = tx.send(crate::settings_state::TestResult { test, models });
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
        Outcome::Handled
    } else {
        Outcome::Ignored
    }
}
