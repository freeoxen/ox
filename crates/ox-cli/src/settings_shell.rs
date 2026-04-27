//! Settings screen shell — owns ephemeral TUI state for the Settings screen.
//!
//! `SettingsShell` wraps `SettingsState` and adds poll / ensure helpers that
//! were previously inlined in `event_loop.rs`.

use crate::settings_state::{SettingsFocus, SettingsState, TestStatus};
use crate::shell::Outcome;
use crate::simple_input::SimpleInput;
use crossterm::event::{KeyCode, KeyModifiers};
use ox_path::oxpath;
use ox_types::{GlobalCommand, UiCommand};
use structfs_core_store::{Record, Value};

/// Resolve the `ProviderConfig` an account points at.
///
/// Looks up `gate.providers.{provider_ref}` in the loaded config; falls back
/// to the matching built-in (`anthropic` or `openai`) when no entry exists,
/// so accounts that simply name a built-in dialect keep working without
/// requiring an explicit provider table entry.
fn resolve_provider_config(
    config: &crate::config::OxConfig,
    provider_ref: &str,
) -> ox_gate::ProviderConfig {
    if let Some(entry) = config.gate.providers.get(provider_ref) {
        return ox_gate::ProviderConfig {
            dialect: entry.dialect.clone(),
            endpoint: entry.endpoint.clone(),
            version: entry.version.clone(),
            auth: entry.auth.clone(),
        };
    }
    match provider_ref {
        "openai" => ox_gate::ProviderConfig::openai(),
        _ => ox_gate::ProviderConfig::anthropic(),
    }
}

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
                            self.state.set_status(TestStatus::Success(format!(
                                "Connected ({dialect}, {ms}ms)"
                            )));
                        }
                        Err(e) => {
                            self.state.set_status(TestStatus::Failed(e));
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
                    self.state.set_status(TestStatus::Failed("Test cancelled".into()));
                    self.state.pending_test = None;
                }
            }
        }
    }

    /// Handle mouse click on the settings edit dialog (focus field selection).
    pub fn handle_mouse(&mut self, mouse: crossterm::event::MouseEvent) {
        if let crossterm::event::MouseEventKind::Down(_) = mouse.kind {
            if self.state.editing.is_some() {
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
                    if let Some(ref mut editing) = self.state.editing {
                        editing.focus = field;
                    }
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
    modifiers: KeyModifiers,
    code: KeyCode,
    client: &ox_broker::ClientHandle,
    inbox_root: &std::path::Path,
) -> Outcome {
    // ---------- edit dialog ----------
    if settings.editing.is_some() {
        return handle_edit_dialog_key(settings, key_str, modifiers, code, client, inbox_root)
            .await;
    }

    // ---------- delete confirmation ----------
    if settings.delete_confirming {
        return handle_delete_confirm_key(settings, key_str, client, inbox_root).await;
    }

    // ---------- navigation ----------
    handle_navigation_key(settings, key_str, modifiers, code, client, inbox_root).await
}

// -----------------------------------------------------------------------
// Edit dialog
// -----------------------------------------------------------------------

/// Post-match signal — avoids holding &mut borrow across actions.
enum EditAction {
    None,
    Cancel,
    Save(SaveSpec),
    /// Stay in the dialog and surface a validation error on the status line.
    /// Used in place of silent `Handled` rejections so the user always sees
    /// why a key (typically Enter) didn't do what they expected.
    Reject(String),
    Handled,
}

/// Validated payload for a save. Built by `save_action_from_editing` so the
/// post-match dispatcher receives a fully-resolved spec — provider id, the
/// concrete `ProviderConfig` to write (only when `custom`), and a key whose
/// presence-or-absence already matches the auth scheme.
struct SaveSpec {
    name: String,
    /// What to write at `gate.accounts.{name}.provider` — either a built-in
    /// preset id ("anthropic", "openai", "lm-studio", …) or the account
    /// name when the user picked Custom (a per-account provider is
    /// synthesized).
    provider_ref: String,
    /// `Some` only for the Custom preset — the user-entered provider
    /// definition that needs to be written to `gate.providers.{name}`.
    custom_provider: Option<ox_gate::ProviderConfig>,
    /// Auth scheme of the chosen preset. Used by the post-match dispatcher
    /// to decide whether to write a key file at all (LM Studio: skip).
    auth: ox_gate::AuthScheme,
    key: String,
}

/// Validate the dialog state and produce a `SaveSpec`, or a `Reject` with
/// a specific reason. Single point of save-time validation so every
/// rejection path produces a status the user can read.
fn save_action_from_editing(editing: &crate::settings_state::AccountEditFields) -> EditAction {
    if editing.name.is_empty() {
        return EditAction::Reject("Name is required.".into());
    }

    let preset = editing.preset();
    let auth = preset.auth.clone();
    let key = editing.key.content().to_owned();

    if auth.requires_key() && key.is_empty() {
        return EditAction::Reject(format!(
            "An API key is required for {}.",
            preset.label
        ));
    }

    if preset.custom {
        let endpoint = editing.endpoint.content().to_owned();
        if let Err(msg) = ox_gate::validate_endpoint(&endpoint) {
            return EditAction::Reject(msg);
        }
        // Custom: account points at a synthesized provider named after
        // the account; write a fresh ProviderConfig with the user-supplied
        // endpoint and the preset's auth (Custom defaults to None — the
        // user can't change auth from the dialog yet; that's a follow-up).
        return EditAction::Save(SaveSpec {
            name: editing.name.content().to_owned(),
            provider_ref: editing.name.content().to_owned(),
            custom_provider: Some(ox_gate::ProviderConfig {
                dialect: preset.dialect.to_string(),
                endpoint,
                version: preset.version.to_string(),
                auth: Some(auth.clone()),
            }),
            auth,
            key,
        });
    }

    // Non-custom preset: account points at the preset's canonical id.
    // The provider entry is written too so the runtime resolves it
    // through the ConfigStore (without relying on built-in `anthropic` /
    // `openai` defaults, which lack auth metadata in older configs).
    EditAction::Save(SaveSpec {
        name: editing.name.content().to_owned(),
        provider_ref: preset.id.to_string(),
        custom_provider: Some(ox_gate::ProviderConfig {
            dialect: preset.dialect.to_string(),
            endpoint: preset.endpoint.to_string(),
            version: preset.version.to_string(),
            auth: Some(auth.clone()),
        }),
        auth,
        key,
    })
}

async fn handle_edit_dialog_key(
    settings: &mut SettingsState,
    key_str: &str,
    modifiers: KeyModifiers,
    code: KeyCode,
    client: &ox_broker::ClientHandle,
    inbox_root: &std::path::Path,
) -> Outcome {
    let keys_dir = inbox_root.join("keys");

    // Status scrolling — works in the edit dialog regardless of which field
    // is focused. PageDown/PageUp scroll one row at a time so a multi-line
    // transport error can be read without expanding the status block.
    if matches!(key_str, "PageDown" | "Ctrl+d") {
        settings.status_scroll = settings.status_scroll.saturating_add(1);
        return Outcome::Handled;
    }
    if matches!(key_str, "PageUp" | "Ctrl+u") {
        settings.status_scroll = settings.status_scroll.saturating_sub(1);
        return Outcome::Handled;
    }

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
            "Enter" => save_action_from_editing(editing),
            "Left" if editing.focus == 1 => {
                let n = ox_gate::presets().len();
                editing.preset_idx = if editing.preset_idx == 0 {
                    n - 1
                } else {
                    editing.preset_idx - 1
                };
                // When switching to a non-custom preset, refill the
                // endpoint field with the preset's URL so the user sees
                // what'll be used. Custom preset keeps whatever was typed.
                if !editing.preset().custom {
                    editing.endpoint = SimpleInput::from(editing.preset().endpoint);
                }
                EditAction::Handled
            }
            "Right" if editing.focus == 1 => {
                let n = ox_gate::presets().len();
                editing.preset_idx = (editing.preset_idx + 1) % n;
                if !editing.preset().custom {
                    editing.endpoint = SimpleInput::from(editing.preset().endpoint);
                }
                EditAction::Handled
            }
            "Ctrl+t" => {
                EditAction::None // handled below as test connection
            }
            _ => {
                // Route to the focused text field's SimpleInput
                if let Some(input) = editing.focused_input() {
                    if input.handle_key(modifiers, code) {
                        EditAction::Handled
                    } else {
                        EditAction::None
                    }
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
            settings.set_status(TestStatus::Idle);
            return Outcome::Handled;
        }
        EditAction::Reject(msg) => {
            settings.set_status(TestStatus::Failed(msg));
            return Outcome::Handled;
        }
        EditAction::Save(spec) => {
            let SaveSpec {
                name,
                provider_ref,
                custom_provider,
                auth,
                key,
            } = spec;

            tracing::info!(
                name = %name,
                provider_ref = %provider_ref,
                synthesizes_provider = custom_provider.is_some(),
                requires_key = auth.requires_key(),
                "saving account via ConfigStore"
            );

            let name_comp = match ox_kernel::PathComponent::try_new(name.as_str()) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(error = %e, "invalid account name for path");
                    return Outcome::Handled;
                }
            };
            // Provider id (used as path component): either the canonical
            // preset id ("anthropic", "lm-studio", …) or the account name
            // (custom). Both are validated as identifiers via PathComponent.
            let provider_comp = match ox_kernel::PathComponent::try_new(provider_ref.as_str()) {
                Ok(c) => c,
                Err(e) => {
                    settings.set_status(TestStatus::Failed(format!(
                        "Invalid provider name '{provider_ref}': {e}"
                    )));
                    return Outcome::Handled;
                }
            };

            if let Some(prov) = custom_provider {
                let dialect_path = ox_path::oxpath!(
                    "config", "gate", "providers", provider_comp.clone(), "dialect"
                );
                client.write_typed(&dialect_path, &prov.dialect).await.ok();
                let endpoint_path = ox_path::oxpath!(
                    "config", "gate", "providers", provider_comp.clone(), "endpoint"
                );
                client.write_typed(&endpoint_path, &prov.endpoint).await.ok();
                let version_path = ox_path::oxpath!(
                    "config", "gate", "providers", provider_comp.clone(), "version"
                );
                client.write_typed(&version_path, &prov.version).await.ok();
                // Auth scheme as a kebab-case string. GateStore's
                // resolve_provider parses these back. None means "use
                // the dialect default"; we always write an explicit value
                // here so the wire shape is unambiguous.
                let auth_str = match auth {
                    ox_gate::AuthScheme::XApiKey => "x-api-key",
                    ox_gate::AuthScheme::BearerToken => "bearer-token",
                    ox_gate::AuthScheme::None => "none",
                };
                let auth_path = ox_path::oxpath!(
                    "config", "gate", "providers", provider_comp.clone(), "auth"
                );
                client
                    .write_typed(&auth_path, &auth_str.to_string())
                    .await
                    .ok();
            }

            let provider_path =
                ox_path::oxpath!("config", "gate", "accounts", name_comp.clone(), "provider");
            client.write_typed(&provider_path, &provider_ref).await.ok();

            // Write the key only if the auth scheme actually uses one.
            // Skipping the file write for unauthenticated providers means
            // a stray empty key file isn't created for LM Studio / Ollama.
            if auth.requires_key() && !key.is_empty() {
                let key_path = ox_path::oxpath!("config", "gate", "accounts", name_comp, "key");
                client.write_typed(&key_path, &key).await.ok();
                crate::config::write_key_file(&keys_dir, &name, &key).ok();
            }

            // If default account doesn't exist, set it to this one
            let current_default = client
                .read_typed::<String>(&oxpath!("config", "gate", "defaults", "account"))
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
            let default_exists = if current_default.is_empty() {
                false
            } else if let Ok(cd_comp) = ox_kernel::PathComponent::try_new(current_default.as_str())
            {
                client
                    .read(&ox_path::oxpath!(
                        "config", "gate", "accounts", cd_comp, "provider"
                    ))
                    .await
                    .ok()
                    .flatten()
                    .is_some()
            } else {
                false
            };
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
            settings.set_status(TestStatus::Idle);
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

    // Handle Ctrl+t for test connection in edit dialog. Builds a transient
    // ProviderConfig from the chosen preset (with the endpoint substituted
    // for Custom) and an empty key when auth=None — same shape the save
    // flow will produce.
    if key_str == "Ctrl+t" {
        if let Some(ref editing) = settings.editing {
            let preset = editing.preset();

            // Endpoint check fires only for Custom — preset endpoints are
            // already valid by construction.
            let endpoint = if preset.custom {
                if let Err(msg) = ox_gate::validate_endpoint(editing.endpoint.content()) {
                    settings.set_status(TestStatus::Failed(msg));
                    return Outcome::Handled;
                }
                editing.endpoint.content().to_owned()
            } else {
                preset.endpoint.to_string()
            };

            if preset.auth.requires_key() && editing.key.is_empty() {
                settings.set_status(TestStatus::Failed(format!(
                    "An API key is required for {}.",
                    preset.label
                )));
                return Outcome::Handled;
            }

            let provider_config = ox_gate::ProviderConfig {
                dialect: preset.dialect.to_string(),
                endpoint,
                version: preset.version.to_string(),
                auth: Some(preset.auth.clone()),
            };
            let provider_label = if preset.custom {
                editing.name.content().to_owned()
            } else {
                preset.id.to_string()
            };
            let api_key_for_test = editing.key.content().to_owned();

            settings.set_status(TestStatus::Testing);
            let (tx, rx) = tokio::sync::oneshot::channel();
            settings.pending_test = Some(rx);

            let pc = provider_config;
            let key = api_key_for_test;
            let provider_name = provider_label;
            tokio::spawn(async move {
                let test =
                    crate::transport::test_connection_async(&pc, &key, &provider_name).await;
                let models = if test.is_ok() {
                    crate::transport::fetch_model_catalog_async(&pc, &key, &provider_name).await
                } else {
                    Err("skipped".into())
                };
                let _ = tx.send(crate::settings_state::TestResult { test, models });
            });
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
            let name_comp = match ox_kernel::PathComponent::try_new(name.as_str()) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(error = %e, "invalid account name for path");
                    settings.delete_confirming = false;
                    return Outcome::Handled;
                }
            };
            let provider_path =
                ox_path::oxpath!("config", "gate", "accounts", name_comp.clone(), "provider");
            client
                .write(&provider_path, Record::parsed(Value::Null))
                .await
                .ok();
            let key_path =
                ox_path::oxpath!("config", "gate", "accounts", name_comp.clone(), "key");
            client
                .write(&key_path, Record::parsed(Value::Null))
                .await
                .ok();

            // If the account had a synthesized per-account provider (UI flow
            // creates one at gate.providers.{name} when an endpoint was set),
            // tear it down too. Built-in providers ("anthropic", "openai")
            // share the namespace and must not be deleted; a name collision
            // with a built-in is benign because we only nuke a provider
            // entry that was authored under the same name as the account.
            let prov_dialect = ox_path::oxpath!(
                "config", "gate", "providers", name_comp.clone(), "dialect"
            );
            client.write(&prov_dialect, Record::parsed(Value::Null)).await.ok();
            let prov_endpoint = ox_path::oxpath!(
                "config", "gate", "providers", name_comp.clone(), "endpoint"
            );
            client.write(&prov_endpoint, Record::parsed(Value::Null)).await.ok();
            let prov_version = ox_path::oxpath!(
                "config", "gate", "providers", name_comp.clone(), "version"
            );
            client.write(&prov_version, Record::parsed(Value::Null)).await.ok();

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
    modifiers: KeyModifiers,
    code: KeyCode,
    client: &ox_broker::ClientHandle,
    inbox_root: &std::path::Path,
) -> Outcome {
    // Status scrolling — applies on the accounts list path too, so a long
    // test failure shown below the list is reachable. PageDown/PageUp move
    // one row at a time; the caller-managed offset is clamped at render time
    // by the `TextPane`.
    if matches!(key_str, "PageDown" | "Ctrl+d") {
        settings.status_scroll = settings.status_scroll.saturating_add(1);
        return Outcome::Handled;
    }
    if matches!(key_str, "PageUp" | "Ctrl+u") {
        settings.status_scroll = settings.status_scroll.saturating_sub(1);
        return Outcome::Handled;
    }

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
                let presets = ox_gate::presets();
                settings.editing = Some(crate::settings_state::AccountEditFields {
                    name: SimpleInput::new(),
                    preset_idx: 0,
                    endpoint: SimpleInput::from(presets[0].endpoint),
                    key: SimpleInput::new(),
                    focus: 0,
                    is_new: true,
                });
                settings.set_status(TestStatus::Idle);
            }
            true
        }
        "e" => {
            if settings.focus == SettingsFocus::Accounts {
                if let Some(acct) = settings.accounts.get(settings.selected_account) {
                    let keys_dir = inbox_root.join("keys");
                    let key_val =
                        crate::config::read_key_file(&keys_dir, &acct.name).unwrap_or_default();
                    let config = crate::config::resolve_config(
                        inbox_root,
                        &crate::config::CliOverrides::default(),
                    );
                    let provider_ref = config
                        .gate
                        .accounts
                        .get(&acct.name)
                        .map(|e| e.provider.clone())
                        .unwrap_or_default();
                    // Map the existing account onto a preset by id when
                    // possible. If the account points at a per-account or
                    // unknown provider, fall back to the Custom preset and
                    // prefill the endpoint from the provider entry.
                    let presets = ox_gate::presets();
                    let preset_idx = presets
                        .iter()
                        .position(|p| !p.custom && p.id == provider_ref)
                        .unwrap_or_else(|| {
                            presets.iter().position(|p| p.custom).unwrap_or(0)
                        });
                    let endpoint = if presets[preset_idx].custom {
                        config
                            .gate
                            .providers
                            .get(&provider_ref)
                            .map(|p| p.endpoint.clone())
                            .unwrap_or_default()
                    } else {
                        presets[preset_idx].endpoint.to_string()
                    };
                    settings.editing = Some(crate::settings_state::AccountEditFields {
                        name: SimpleInput::from(&acct.name),
                        preset_idx,
                        endpoint: SimpleInput::from(&endpoint),
                        key: SimpleInput::from(&key_val),
                        focus: 0,
                        is_new: false,
                    });
                    settings.set_status(TestStatus::Idle);
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
                0 if !settings.accounts.is_empty() => {
                    settings.default_account_idx = if settings.default_account_idx == 0 {
                        settings.accounts.len() - 1
                    } else {
                        settings.default_account_idx - 1
                    };
                }
                1 if !settings.discovered_models.is_empty() => {
                    let idx = settings.model_picker_idx.unwrap_or(0);
                    let new_idx = if idx == 0 {
                        settings.discovered_models.len() - 1
                    } else {
                        idx - 1
                    };
                    settings.model_picker_idx = Some(new_idx);
                    settings
                        .default_model
                        .set(&settings.discovered_models[new_idx].id);
                }
                _ => {}
            }
            true
        }
        "Right" if settings.focus == SettingsFocus::Defaults => {
            match settings.defaults_focus {
                0 if !settings.accounts.is_empty() => {
                    settings.default_account_idx =
                        (settings.default_account_idx + 1) % settings.accounts.len();
                }
                1 if !settings.discovered_models.is_empty() => {
                    let idx = settings.model_picker_idx.unwrap_or(0);
                    let new_idx = (idx + 1) % settings.discovered_models.len();
                    settings.model_picker_idx = Some(new_idx);
                    settings
                        .default_model
                        .set(&settings.discovered_models[new_idx].id);
                }
                _ => {}
            }
            true
        }
        // Backspace on model/max_tokens: fall through to the generic SimpleInput handler below
        "Enter" if settings.focus == SettingsFocus::Defaults => {
            // Determine current selections
            let acct_name = settings
                .accounts
                .get(settings.default_account_idx)
                .map(|a| a.name.clone())
                .unwrap_or_default();
            let model = settings.default_model.content().to_owned();
            let max_tokens: i64 = settings
                .default_max_tokens
                .content()
                .parse()
                .unwrap_or(4096);

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
                    // Empty key is fine: unauthenticated providers (LM Studio,
                    // Ollama) accept anything. The server's response is more
                    // informative than a local file check would be.
                    let key =
                        crate::config::read_key_file(&keys_dir, &acct.name).unwrap_or_default();
                    let config = crate::config::resolve_config(
                        inbox_root,
                        &crate::config::CliOverrides::default(),
                    );
                    let provider_ref = config
                        .gate
                        .accounts
                        .get(&acct.name)
                        .map(|e| e.provider.clone())
                        .unwrap_or_else(|| "anthropic".to_string());
                    let provider_config = resolve_provider_config(&config, &provider_ref);

                    settings.set_status(TestStatus::Testing);
                    let (tx, rx) = tokio::sync::oneshot::channel();
                    settings.pending_test = Some(rx);

                    let pc = provider_config;
                    let k = key;
                    let provider_name = provider_ref;
                    tokio::spawn(async move {
                        let test =
                            crate::transport::test_connection_async(&pc, &k, &provider_name).await;
                        let models = if test.is_ok() {
                            crate::transport::fetch_model_catalog_async(&pc, &k, &provider_name)
                                .await
                        } else {
                            Err("skipped".into())
                        };
                        let _ = tx.send(crate::settings_state::TestResult { test, models });
                    });
                }
            }
            true
        }
        _ if settings.focus == SettingsFocus::Defaults
            && settings.defaults_focus == 1
            && settings.default_model.handle_key(modifiers, code) =>
        {
            settings.model_picker_idx = None;
            true
        }
        _ if settings.focus == SettingsFocus::Defaults && settings.defaults_focus == 2 => {
            // Only allow digits for max_tokens
            if matches!(code, KeyCode::Char(c) if !c.is_ascii_digit()) {
                false
            } else {
                settings.default_max_tokens.handle_key(modifiers, code)
            }
        }
        _ => false,
    };

    if handled {
        Outcome::Handled
    } else {
        Outcome::Ignored
    }
}
