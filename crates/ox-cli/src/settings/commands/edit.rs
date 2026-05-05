//! Inline-edit state machine for the accordion field rows.
//!
//! When the user presses Enter on a focused editable field row, the
//! tree dispatch calls one of the `begin.*` commands here. Begin:
//!
//!   - reads the field's current value off the data path,
//!   - converts it to a `String` (for numeric fields, decimal
//!     digits; empty for `None`),
//!   - writes that string into `ui/settings/edit_buffer`,
//!   - records the field row's path in `ui/settings/edit_field_path`,
//!   - flips `ui/settings/edit_mode = true`.
//!
//! While `edit_mode` is true the dispatcher's edit-mode pass routes
//! every printable char to `edit.insert_char` (append to buffer),
//! Backspace to `edit.delete_back` (pop from buffer), Enter to
//! `edit.commit` (parse + write to data path + clear state), and
//! Esc to `edit.cancel` (clear state without writing).
//!
//! The renderer picks up `edit_mode` + `edit_field_path` and
//! substitutes the data value with the live buffer plus a visible
//! cursor block, so the user sees what they're typing.

use ox_path::oxpath;
use ox_types::Screen;
use ox_types::settings::{AccountField, ModelField, ModelKey};
use ox_types::subscription::Write;
use structfs_core_store::{Path, Reader, Record, Value};
use structfs_serde_store::to_value;

use super::super::command_registry::CommandRegistry;
use super::super::visible_rows::{self, RowKind};
use super::navigation::{path_from_value, path_to_value};

#[allow(unused_imports)]
use super::command;

// ---------------------------------------------------------------------------
// Begin commands — one per field type, called by `tree.activate`.
// ---------------------------------------------------------------------------

command! {
    struct_name: BeginEditAccountEndpoint,
    id: "edit.begin.account_endpoint",
    title: "Edit Endpoint",
    description: "Enter inline edit mode for the focused account's endpoint.",
    screen: Screen::Settings,
    cursor: None,
    run: |snap, _ctx| begin_edit_account_text(snap, AccountField::Endpoint),
}

command! {
    struct_name: BeginEditAccountKey,
    id: "edit.begin.account_key",
    title: "Edit API Key",
    description: "Enter inline edit mode for the focused account's API key.",
    screen: Screen::Settings,
    cursor: None,
    run: |snap, _ctx| begin_edit_account_text(snap, AccountField::Key),
}

command! {
    struct_name: BeginEditModelField,
    id: "edit.begin.model_field",
    title: "Edit Model Override",
    description: "Enter inline edit mode for the focused model's numeric override.",
    screen: Screen::Settings,
    cursor: None,
    run: |snap, _ctx| begin_edit_model_field_inner(snap),
}

// ---------------------------------------------------------------------------
// Buffer mutations — bound at `Exact(settings/_edit_mode)`.
// ---------------------------------------------------------------------------

command! {
    struct_name: InsertChar,
    id: "edit.insert_char",
    title: "Insert character",
    description: "Append the just-pressed printable char to the edit buffer.",
    screen: Screen::Settings,
    cursor: None,
    run: |snap, ctx| insert_char(snap, ctx),
}

command! {
    struct_name: DeleteBack,
    id: "edit.delete_back",
    title: "Backspace",
    description: "Remove the last character from the edit buffer.",
    screen: Screen::Settings,
    cursor: None,
    run: |snap, _ctx| delete_back(snap),
}

command! {
    struct_name: Commit,
    id: "edit.commit",
    title: "Commit",
    description: "Persist the edit buffer to the field's data path and exit edit mode.",
    screen: Screen::Settings,
    cursor: None,
    run: |snap, _ctx| commit(snap),
}

command! {
    struct_name: Cancel,
    id: "edit.cancel",
    title: "Cancel",
    description: "Discard the edit buffer and exit edit mode.",
    screen: Screen::Settings,
    cursor: None,
    run: |snap, _ctx| cancel(snap),
}

pub fn register(reg: &mut CommandRegistry) {
    reg.register(Box::new(BeginEditAccountEndpoint::new()));
    reg.register(Box::new(BeginEditAccountKey::new()));
    reg.register(Box::new(BeginEditModelField::new()));
    reg.register(Box::new(InsertChar::new()));
    reg.register(Box::new(DeleteBack::new()));
    reg.register(Box::new(Commit::new()));
    reg.register(Box::new(Cancel::new()));
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

/// Resolve the focused row to its path + RowKind. Returns `None` when
/// `focused_row` is unset or names a row that vanished from the
/// visible tree (e.g. an account got deleted under the user's cursor).
fn focused_row(data: &mut dyn Reader) -> Option<visible_rows::VisibleRow> {
    let path = read_focused_path(data)?;
    visible_rows::enumerate(data)
        .into_iter()
        .find(|r| r.path == path)
}

fn read_focused_path(data: &mut dyn Reader) -> Option<Path> {
    let r = data
        .read(&oxpath!("ui", "settings", "focused_row"))
        .ok()
        .flatten()?;
    path_from_value(r.as_value()?)
}

fn read_string(data: &mut dyn Reader, path: &Path) -> Option<String> {
    let r = data.read(path).ok().flatten()?;
    match r.as_value()? {
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

fn read_path(data: &mut dyn Reader, path: &Path) -> Option<Path> {
    let r = data.read(path).ok().flatten()?;
    path_from_value(r.as_value()?)
}

/// Begin edit for an account text field (Endpoint or Key). Reads the
/// current value off the data path; if no value exists yet (empty
/// endpoint, unset key), seeds the buffer with an empty string so the
/// user can type to fill it in.
pub(super) fn begin_edit_account_endpoint(data: &mut dyn Reader) -> Vec<Write> {
    begin_edit_account_text(data, AccountField::Endpoint)
}

pub(super) fn begin_edit_account_key(data: &mut dyn Reader) -> Vec<Write> {
    begin_edit_account_text(data, AccountField::Key)
}

pub(super) fn begin_edit_model_field(data: &mut dyn Reader) -> Vec<Write> {
    begin_edit_model_field_inner(data)
}

fn begin_edit_account_text(data: &mut dyn Reader, field: AccountField) -> Vec<Write> {
    let row = match focused_row(data) {
        Some(r) => r,
        None => return Vec::new(),
    };
    let account = match row.kind {
        RowKind::AccountField {
            account,
            field: row_field,
        } if row_field == field => account,
        _ => return Vec::new(),
    };
    let initial = match field {
        AccountField::Endpoint => current_endpoint(data, &account).unwrap_or_default(),
        AccountField::Key => current_api_key(data, &account).unwrap_or_default(),
        // Caller guarantees a text field; the registry-level command
        // ids dispatch the right one. Still defensive.
        _ => return Vec::new(),
    };
    enter_edit_mode(row.path, initial)
}

/// Begin edit for a model numeric override (max_context_size or
/// max_output_tokens). Seeds the buffer with the current decimal
/// representation, or empty when the override is `None`.
fn begin_edit_model_field_inner(data: &mut dyn Reader) -> Vec<Write> {
    let row = match focused_row(data) {
        Some(r) => r,
        None => return Vec::new(),
    };
    let (account, model_id, field) = match row.kind {
        RowKind::ModelField {
            account,
            model_id,
            field,
        } => (account, model_id, field),
        _ => return Vec::new(),
    };
    let initial = current_model_override(data, &account, &model_id, field).unwrap_or_default();
    enter_edit_mode(row.path, initial)
}

/// Seed the inline three-stage manual-model form. The form lives at
/// `ui/settings/manual_model/*`:
///
/// - `account: String` — which connection we're adding to
/// - `stage: "id" | "ctx" | "out"` — current field being edited
/// - `buffer: String` — the live buffer
/// - `staged_id: String` — committed id from previous stage
/// - `staged_ctx: String` — committed ctx (raw text) from previous stage
///
/// Activation flips edit_mode on so the dispatcher's edit-mode pass
/// routes printable chars into the buffer; the Commit command's
/// manual-model branch advances stages and ultimately writes the new
/// ModelInfo.
pub(crate) fn begin_manual_model(_data: &mut dyn Reader, account: &str) -> Vec<Write> {
    let account_value = match to_value(&account.to_string()) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let stage_value = match to_value(&"id".to_string()) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    vec![
        Write {
            path: oxpath!("ui", "settings", "manual_model", "account"),
            record: Record::parsed(account_value),
        },
        Write {
            path: oxpath!("ui", "settings", "manual_model", "stage"),
            record: Record::parsed(stage_value),
        },
        Write {
            path: oxpath!("ui", "settings", "manual_model", "buffer"),
            record: Record::parsed(Value::String(String::new())),
        },
        Write {
            path: oxpath!("ui", "settings", "edit_mode"),
            record: Record::parsed(Value::Bool(true)),
        },
    ]
}

fn enter_edit_mode(field_path: Path, buffer: String) -> Vec<Write> {
    vec![
        Write {
            path: oxpath!("ui", "settings", "edit_field_path"),
            record: Record::parsed(path_to_value(&field_path)),
        },
        Write {
            path: oxpath!("ui", "settings", "edit_buffer"),
            record: Record::parsed(Value::String(buffer)),
        },
        Write {
            path: oxpath!("ui", "settings", "edit_mode"),
            record: Record::parsed(Value::Bool(true)),
        },
    ]
}

fn clear_edit_state() -> Vec<Write> {
    vec![
        Write {
            path: oxpath!("ui", "settings", "edit_mode"),
            record: Record::parsed(Value::Bool(false)),
        },
        Write {
            path: oxpath!("ui", "settings", "edit_buffer"),
            record: Record::parsed(Value::Null),
        },
        Write {
            path: oxpath!("ui", "settings", "edit_field_path"),
            record: Record::parsed(Value::Null),
        },
    ]
}

fn insert_char(
    data: &mut dyn Reader,
    ctx: &super::super::command_registry::CommandCtx<'_>,
) -> Vec<Write> {
    use ox_types::key_chord::KeyCodeRepr;

    let chord = match ctx.last_keystroke.as_ref() {
        Some(c) => c,
        None => return Vec::new(),
    };
    let ch = match chord.code {
        KeyCodeRepr::Char(c) => c,
        _ => return Vec::new(),
    };
    let current = read_string(data, &oxpath!("ui", "settings", "edit_buffer")).unwrap_or_default();
    let field_path = match read_path(data, &oxpath!("ui", "settings", "edit_field_path")) {
        Some(p) => p,
        None => return Vec::new(),
    };
    // Numeric fields only accept ASCII digits. Account text fields
    // accept anything printable. The row's RowKind tells us which.
    let row = visible_rows::enumerate(data)
        .into_iter()
        .find(|r| r.path == field_path);
    let accept = match row.as_ref().map(|r| &r.kind) {
        Some(RowKind::ModelField { .. }) => ch.is_ascii_digit(),
        Some(RowKind::AccountField { .. }) => true,
        _ => false,
    };
    if !accept {
        return Vec::new();
    }
    let mut next = current;
    next.push(ch);
    vec![Write {
        path: oxpath!("ui", "settings", "edit_buffer"),
        record: Record::parsed(Value::String(next)),
    }]
}

fn delete_back(data: &mut dyn Reader) -> Vec<Write> {
    let mut current =
        read_string(data, &oxpath!("ui", "settings", "edit_buffer")).unwrap_or_default();
    if current.pop().is_none() {
        return Vec::new();
    }
    vec![Write {
        path: oxpath!("ui", "settings", "edit_buffer"),
        record: Record::parsed(Value::String(current)),
    }]
}

fn cancel(data: &mut dyn Reader) -> Vec<Write> {
    let mut writes = Vec::new();
    // Manual-model form clears its own state additionally so the user
    // can abandon a partially-filled form without leaving stale paths
    // behind. Only fires when a stage value is actually set; otherwise
    // the regular edit-mode cancel suffices.
    if super::super::renderers::util::read_typed::<String>(
        data,
        &oxpath!("ui", "settings", "manual_model", "stage"),
    )
    .is_some()
    {
        for sub in ["account", "stage", "buffer", "staged_id", "staged_ctx"] {
            let comp = ox_kernel::PathComponent::try_new(sub).expect("identifier");
            writes.push(Write {
                path: oxpath!("ui", "settings", "manual_model", comp),
                record: Record::parsed(Value::Null),
            });
        }
    }
    writes.extend(clear_edit_state());
    writes
}

fn commit(data: &mut dyn Reader) -> Vec<Write> {
    // Manual-model form takes precedence: when a manual_model/stage
    // value is set, route Enter through the staged form's state machine
    // rather than the regular field-commit path. Stage advances ("id" →
    // "ctx" → "out") write only the form's own paths; the final stage
    // additionally writes the assembled ModelInfo to the catalog and
    // tears the form down.
    if let Some(stage) = super::super::renderers::util::read_typed::<String>(
        data,
        &oxpath!("ui", "settings", "manual_model", "stage"),
    ) {
        return commit_manual_model(data, &stage);
    }
    let field_path = match read_path(data, &oxpath!("ui", "settings", "edit_field_path")) {
        Some(p) => p,
        None => return clear_edit_state(),
    };
    let buffer = read_string(data, &oxpath!("ui", "settings", "edit_buffer")).unwrap_or_default();
    let row = visible_rows::enumerate(data)
        .into_iter()
        .find(|r| r.path == field_path);
    let mut writes: Vec<Write> = match row.map(|r| r.kind) {
        Some(RowKind::AccountField { account, field }) => {
            commit_account_field(data, &account, field, &buffer)
        }
        Some(RowKind::ModelField {
            account,
            model_id,
            field,
        }) => commit_model_field(data, &account, &model_id, field, &buffer),
        _ => Vec::new(),
    };
    writes.extend(clear_edit_state());
    writes
}

fn commit_manual_model(data: &mut dyn Reader, stage: &str) -> Vec<Write> {
    let buffer: String = super::super::renderers::util::read_typed(
        data,
        &oxpath!("ui", "settings", "manual_model", "buffer"),
    )
    .unwrap_or_default();
    let trimmed = buffer.trim();

    match stage {
        "id" => {
            if trimmed.is_empty() {
                return Vec::new();
            }
            vec![
                Write {
                    path: oxpath!("ui", "settings", "manual_model", "stage"),
                    record: Record::parsed(Value::String("ctx".into())),
                },
                Write {
                    path: oxpath!("ui", "settings", "manual_model", "staged_id"),
                    record: Record::parsed(Value::String(trimmed.to_string())),
                },
                Write {
                    path: oxpath!("ui", "settings", "manual_model", "buffer"),
                    record: Record::parsed(Value::String(String::new())),
                },
            ]
        }
        "ctx" => {
            let n: u32 = match trimmed.parse() {
                Ok(n) if n > 0 => n,
                _ => return Vec::new(),
            };
            vec![
                Write {
                    path: oxpath!("ui", "settings", "manual_model", "stage"),
                    record: Record::parsed(Value::String("out".into())),
                },
                Write {
                    path: oxpath!("ui", "settings", "manual_model", "staged_ctx"),
                    record: Record::parsed(Value::String(n.to_string())),
                },
                Write {
                    path: oxpath!("ui", "settings", "manual_model", "buffer"),
                    record: Record::parsed(Value::String(String::new())),
                },
            ]
        }
        "out" => {
            let out: u32 = match trimmed.parse() {
                Ok(n) if n > 0 => n,
                _ => return Vec::new(),
            };
            let id: String = super::super::renderers::util::read_typed(
                data,
                &oxpath!("ui", "settings", "manual_model", "staged_id"),
            )
            .unwrap_or_default();
            let ctx: u32 = super::super::renderers::util::read_typed::<String>(
                data,
                &oxpath!("ui", "settings", "manual_model", "staged_ctx"),
            )
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
            let account: String = super::super::renderers::util::read_typed(
                data,
                &oxpath!("ui", "settings", "manual_model", "account"),
            )
            .unwrap_or_default();

            let comp = match ox_kernel::PathComponent::try_new(&account) {
                Ok(c) => c,
                Err(_) => return Vec::new(),
            };

            // Read the existing catalog (absent → empty) and append the
            // new entry. Display name defaults to the id; the user can
            // refine it later via the model-field rows.
            let catalog_path = oxpath!("config", "gate", "accounts", comp, "models");
            let mut catalog: Vec<ox_gate::ModelInfo> =
                super::super::renderers::util::read_typed(data, &catalog_path).unwrap_or_default();
            catalog.push(ox_gate::ModelInfo {
                id: id.clone(),
                display_name: id,
                max_context_size: Some(ctx),
                max_output_tokens: Some(out),
                source: ox_gate::ModelInfoSource::UserEntered,
            });
            let catalog_value = match to_value(&catalog) {
                Ok(v) => v,
                Err(_) => return Vec::new(),
            };

            // Write the catalog and clear the form state. Each form-state
            // path becomes a Null write to retire it; edit_mode flips off
            // so the dispatcher returns to normal navigation.
            vec![
                Write {
                    path: catalog_path,
                    record: Record::parsed(catalog_value),
                },
                Write {
                    path: oxpath!("ui", "settings", "manual_model", "account"),
                    record: Record::parsed(Value::Null),
                },
                Write {
                    path: oxpath!("ui", "settings", "manual_model", "stage"),
                    record: Record::parsed(Value::Null),
                },
                Write {
                    path: oxpath!("ui", "settings", "manual_model", "buffer"),
                    record: Record::parsed(Value::Null),
                },
                Write {
                    path: oxpath!("ui", "settings", "manual_model", "staged_id"),
                    record: Record::parsed(Value::Null),
                },
                Write {
                    path: oxpath!("ui", "settings", "manual_model", "staged_ctx"),
                    record: Record::parsed(Value::Null),
                },
                Write {
                    path: oxpath!("ui", "settings", "edit_mode"),
                    record: Record::parsed(Value::Bool(false)),
                },
            ]
        }
        _ => Vec::new(),
    }
}

// ---------------------------------------------------------------------------
// Per-field reads + writes
// ---------------------------------------------------------------------------

fn current_endpoint(data: &mut dyn Reader, account: &str) -> Option<String> {
    let acct_comp = ox_kernel::PathComponent::try_new(account).ok()?;
    let acct: ox_gate::AccountConfig = super::super::renderers::util::read_typed(
        data,
        &oxpath!("config", "gate", "accounts", acct_comp),
    )
    .or_else(|| {
        // TOML-loaded accounts have no parent leaf; synthesize a
        // default. The provider name lives at the child path; pull it
        // there if present.
        let provider_str: Option<String> = read_child_string(data, account, "provider").or(None);
        provider_str.map(|p| ox_gate::AccountConfig { provider: p })
    })?;
    let provider_comp = ox_kernel::PathComponent::try_new(&acct.provider).ok()?;
    let provider: ox_gate::ProviderConfig = super::super::renderers::util::read_typed(
        data,
        &oxpath!("config", "gate", "providers", provider_comp),
    )?;
    Some(provider.endpoint)
}

fn current_api_key(data: &mut dyn Reader, account: &str) -> Option<String> {
    let acct_comp = ox_kernel::PathComponent::try_new(account).ok()?;
    let key: ox_gate::ApiKey =
        super::super::renderers::util::read_typed(data, &oxpath!("secret", "keys", acct_comp))?;
    Some(key.expose().to_string())
}

fn current_model_override(
    data: &mut dyn Reader,
    account: &str,
    model_id: &str,
    field: ModelField,
) -> Option<String> {
    let acct_comp = ox_kernel::PathComponent::try_new(account).ok()?;
    let models: Vec<ox_gate::ModelInfo> = super::super::renderers::util::read_typed(
        data,
        &oxpath!("config", "gate", "accounts", acct_comp, "models"),
    )?;
    let model = models.into_iter().find(|m| m.id == model_id)?;
    let value = match field {
        ModelField::ContextSizeOverride => model.max_context_size,
        ModelField::OutputTokensOverride => model.max_output_tokens,
    };
    Some(value.map(|n| n.to_string()).unwrap_or_default())
}

fn read_child_string(data: &mut dyn Reader, account: &str, child: &str) -> Option<String> {
    let acct_comp = ox_kernel::PathComponent::try_new(account).ok()?;
    let child_comp = ox_kernel::PathComponent::try_new(child).ok()?;
    let r = data
        .read(&oxpath!(
            "config", "gate", "accounts", acct_comp, child_comp
        ))
        .ok()
        .flatten()?;
    match r.as_value()? {
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

fn commit_account_field(
    data: &mut dyn Reader,
    account: &str,
    field: AccountField,
    buffer: &str,
) -> Vec<Write> {
    let acct_comp = match ox_kernel::PathComponent::try_new(account) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    match field {
        AccountField::Endpoint => {
            // Resolve provider name; synthesize default AccountConfig if
            // no parent leaf exists yet.
            let acct: ox_gate::AccountConfig = super::super::renderers::util::read_typed(
                data,
                &oxpath!("config", "gate", "accounts", acct_comp.clone()),
            )
            .or_else(|| {
                read_child_string(data, account, "provider")
                    .map(|p| ox_gate::AccountConfig { provider: p })
            })
            .unwrap_or(ox_gate::AccountConfig {
                provider: "anthropic".to_string(),
            });
            let provider_comp = match ox_kernel::PathComponent::try_new(&acct.provider) {
                Ok(c) => c,
                Err(_) => return Vec::new(),
            };
            let provider_path = oxpath!("config", "gate", "providers", provider_comp);
            let mut provider: ox_gate::ProviderConfig =
                super::super::renderers::util::read_typed(data, &provider_path).unwrap_or_else(
                    || ox_gate::ProviderConfig {
                        dialect: acct.provider.clone(),
                        endpoint: String::new(),
                        version: String::new(),
                        auth: None,
                    },
                );
            provider.endpoint = buffer.to_string();
            let value = match to_value(&provider) {
                Ok(v) => v,
                Err(_) => return Vec::new(),
            };
            vec![Write {
                path: provider_path,
                record: Record::parsed(value),
            }]
        }
        AccountField::Key => {
            let key_path = oxpath!("secret", "keys", acct_comp);
            let api_key = ox_gate::ApiKey::new(buffer.to_string());
            let value = match to_value(&api_key) {
                Ok(v) => v,
                Err(_) => return Vec::new(),
            };
            vec![Write {
                path: key_path,
                record: Record::parsed(value),
            }]
        }
        // Read-only / selectors don't enter edit mode; defensive.
        AccountField::Name | AccountField::Protocol | AccountField::Auth => Vec::new(),
    }
}

fn commit_model_field(
    data: &mut dyn Reader,
    account: &str,
    model_id: &str,
    field: ModelField,
    buffer: &str,
) -> Vec<Write> {
    let acct_comp = match ox_kernel::PathComponent::try_new(account) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let models_path = oxpath!("config", "gate", "accounts", acct_comp, "models");
    let mut models: Vec<ox_gate::ModelInfo> =
        match super::super::renderers::util::read_typed(data, &models_path) {
            Some(m) => m,
            None => return Vec::new(),
        };
    let model = match models.iter_mut().find(|m| m.id == model_id) {
        Some(m) => m,
        None => return Vec::new(),
    };
    let parsed: Option<u32> = if buffer.is_empty() {
        None
    } else {
        match buffer.parse::<u32>() {
            Ok(n) => Some(n),
            Err(_) => return Vec::new(),
        }
    };
    match field {
        ModelField::ContextSizeOverride => model.max_context_size = parsed,
        ModelField::OutputTokensOverride => model.max_output_tokens = parsed,
    }
    model.source = ox_gate::ModelInfoSource::UserOverride;
    let value = match to_value(&models) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    vec![Write {
        path: models_path,
        record: Record::parsed(value),
    }]
}

/// What the renderer needs to know mid-edit — the path of the row
/// currently being edited and the live buffer contents. `None` when
/// edit mode is not active. Public so the index renderer can
/// substitute the row's stored label with the live buffer.
#[derive(Clone, Debug)]
pub struct EditState {
    pub field_path: Path,
    pub buffer: String,
}

pub fn read_edit_state(data: &mut dyn Reader) -> Option<EditState> {
    let active = data
        .read(&oxpath!("ui", "settings", "edit_mode"))
        .ok()
        .flatten()
        .and_then(|r| match r.as_value() {
            Some(Value::Bool(b)) => Some(*b),
            _ => None,
        })
        .unwrap_or(false);
    if !active {
        return None;
    }
    let field_path = read_path(data, &oxpath!("ui", "settings", "edit_field_path"))?;
    let buffer = read_string(data, &oxpath!("ui", "settings", "edit_buffer")).unwrap_or_default();
    Some(EditState { field_path, buffer })
}

#[allow(dead_code)] // re-exported via `read_edit_state`'s field_path; placeholder
fn _force_modelkey_use(_k: ModelKey) {}

#[cfg(test)]
mod tests {
    use super::*;

    use ox_gate::{AccountConfig, ModelInfo, ModelInfoSource};
    use ox_types::{BadgeSource, SettingsIndexEntry};

    use crate::settings::command_registry::{Command, CommandCtx};
    use crate::settings::commands::navigation::path_to_value;
    use crate::settings::registry::RendererRegistry;
    use crate::settings::snapshot::SettingsSnapshot;
    use crate::settings::visible_rows::expanded_set_to_value;

    fn run<C: Command>(cmd: &C, snap: &mut SettingsSnapshot) -> Vec<Write> {
        let registry = RendererRegistry::new();
        let ctx = CommandCtx {
            registry: &registry,
            last_keystroke: None,
        };
        cmd.run(snap, &ctx)
    }

    fn run_with_key<C: Command>(cmd: &C, snap: &mut SettingsSnapshot, ch: char) -> Vec<Write> {
        use ox_types::KeyChord;
        use ox_types::key_chord::{KeyCodeRepr, KeyModifierSet};
        let registry = RendererRegistry::new();
        let ctx = CommandCtx {
            registry: &registry,
            last_keystroke: Some(KeyChord {
                modifiers: KeyModifierSet::default(),
                code: KeyCodeRepr::Char(ch),
            }),
        };
        cmd.run(snap, &ctx)
    }

    fn write_index_with_account(snap: &mut SettingsSnapshot, name: &str, provider: &str) {
        snap.insert(
            &oxpath!("settings", "index", "entries", "accounts"),
            to_value(&SettingsIndexEntry {
                id: "accounts".to_string(),
                label: "Accounts".to_string(),
                description: String::new(),
                target_cursor: Path::parse("settings/accounts").unwrap(),
                badge: BadgeSource::None,
            })
            .unwrap(),
        );
        let comp = ox_kernel::PathComponent::try_new(name).unwrap();
        snap.insert(
            &oxpath!("config", "gate", "accounts", comp),
            to_value(&AccountConfig {
                provider: provider.to_string(),
            })
            .unwrap(),
        );
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&[
                "settings/accounts".to_string(),
                format!("settings/accounts/{name}"),
            ]),
        );
    }

    fn apply_writes(snap: &mut SettingsSnapshot, writes: &[Write]) {
        for w in writes {
            snap.insert(&w.path, w.record.as_value().unwrap().clone());
        }
    }

    #[test]
    fn begin_edit_endpoint_seeds_buffer_with_current_value() {
        let mut snap = SettingsSnapshot::empty();
        write_index_with_account(&mut snap, "alpha", "anthropic");
        let provider_comp = ox_kernel::PathComponent::try_new("anthropic").unwrap();
        snap.insert(
            &oxpath!("config", "gate", "providers", provider_comp),
            to_value(&ox_gate::ProviderConfig {
                dialect: "anthropic".into(),
                endpoint: "https://api.anthropic.com".into(),
                version: String::new(),
                auth: None,
            })
            .unwrap(),
        );
        snap.insert(
            &oxpath!("ui", "settings", "focused_row"),
            path_to_value(&oxpath!("settings", "accounts", "alpha", "endpoint")),
        );

        let writes = run(&BeginEditAccountEndpoint::new(), &mut snap);
        // edit_field_path + edit_buffer + edit_mode
        assert_eq!(writes.len(), 3);
        assert_eq!(writes[0].path, oxpath!("ui", "settings", "edit_field_path"));
        assert_eq!(writes[1].path, oxpath!("ui", "settings", "edit_buffer"));
        match &writes[1].record {
            Record::Parsed(Value::String(s)) => {
                assert_eq!(s, "https://api.anthropic.com");
            }
            other => panic!("buffer is not a String: {other:?}"),
        }
        assert_eq!(writes[2].path, oxpath!("ui", "settings", "edit_mode"));
    }

    #[test]
    fn insert_char_appends_to_buffer() {
        let mut snap = SettingsSnapshot::empty();
        write_index_with_account(&mut snap, "alpha", "anthropic");
        snap.insert(
            &oxpath!("ui", "settings", "edit_buffer"),
            Value::String("hello".into()),
        );
        snap.insert(
            &oxpath!("ui", "settings", "edit_field_path"),
            path_to_value(&oxpath!("settings", "accounts", "alpha", "endpoint")),
        );

        let writes = run_with_key(&InsertChar::new(), &mut snap, '!');
        assert_eq!(writes.len(), 1);
        match &writes[0].record {
            Record::Parsed(Value::String(s)) => assert_eq!(s, "hello!"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn insert_char_rejects_non_digit_for_model_field() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("settings", "index", "entries", "models"),
            to_value(&SettingsIndexEntry {
                id: "models".to_string(),
                label: "Models".to_string(),
                description: String::new(),
                target_cursor: Path::parse("settings/models").unwrap(),
                badge: BadgeSource::None,
            })
            .unwrap(),
        );
        let comp = ox_kernel::PathComponent::try_new("alpha").unwrap();
        snap.insert(
            &oxpath!("config", "gate", "accounts", comp.clone()),
            to_value(&AccountConfig {
                provider: "alpha".into(),
            })
            .unwrap(),
        );
        snap.insert(
            &oxpath!("config", "gate", "accounts", comp, "models"),
            to_value(&vec![ModelInfo {
                id: "m1".into(),
                display_name: "m1".into(),
                max_context_size: Some(100),
                max_output_tokens: None,
                source: ModelInfoSource::Server,
            }])
            .unwrap(),
        );
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&[
                "settings/models".to_string(),
                "settings/models/alpha/m1".to_string(),
            ]),
        );
        let field_path = oxpath!("settings", "models", "alpha", "m1", "max_context_size");
        snap.insert(
            &oxpath!("ui", "settings", "edit_buffer"),
            Value::String("100".into()),
        );
        snap.insert(
            &oxpath!("ui", "settings", "edit_field_path"),
            path_to_value(&field_path),
        );

        let writes = run_with_key(&InsertChar::new(), &mut snap, 'x');
        assert!(
            writes.is_empty(),
            "non-digit must be rejected for model fields"
        );

        let writes = run_with_key(&InsertChar::new(), &mut snap, '5');
        assert_eq!(writes.len(), 1);
        match &writes[0].record {
            Record::Parsed(Value::String(s)) => assert_eq!(s, "1005"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn delete_back_pops_from_buffer() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "edit_buffer"),
            Value::String("hello".into()),
        );
        let writes = run(&DeleteBack::new(), &mut snap);
        assert_eq!(writes.len(), 1);
        match &writes[0].record {
            Record::Parsed(Value::String(s)) => assert_eq!(s, "hell"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn delete_back_on_empty_buffer_is_inert() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "edit_buffer"),
            Value::String(String::new()),
        );
        let writes = run(&DeleteBack::new(), &mut snap);
        assert!(writes.is_empty());
    }

    #[test]
    fn cancel_clears_edit_state_without_writing_data() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(&oxpath!("ui", "settings", "edit_mode"), Value::Bool(true));
        let writes = run(&Cancel::new(), &mut snap);
        // edit_mode=false + edit_buffer=Null + edit_field_path=Null
        assert_eq!(writes.len(), 3);
        assert_eq!(writes[0].path, oxpath!("ui", "settings", "edit_mode"));
        match &writes[0].record {
            Record::Parsed(Value::Bool(false)) => {}
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn commit_endpoint_writes_provider_config_and_clears_state() {
        let mut snap = SettingsSnapshot::empty();
        write_index_with_account(&mut snap, "alpha", "anthropic");
        let provider_comp = ox_kernel::PathComponent::try_new("anthropic").unwrap();
        snap.insert(
            &oxpath!("config", "gate", "providers", provider_comp.clone()),
            to_value(&ox_gate::ProviderConfig {
                dialect: "anthropic".into(),
                endpoint: "old".into(),
                version: String::new(),
                auth: None,
            })
            .unwrap(),
        );
        snap.insert(&oxpath!("ui", "settings", "edit_mode"), Value::Bool(true));
        snap.insert(
            &oxpath!("ui", "settings", "edit_buffer"),
            Value::String("https://new.example".into()),
        );
        snap.insert(
            &oxpath!("ui", "settings", "edit_field_path"),
            path_to_value(&oxpath!("settings", "accounts", "alpha", "endpoint")),
        );

        let writes = run(&Commit::new(), &mut snap);
        // ProviderConfig write + 3 clear-state writes
        assert_eq!(writes.len(), 4);
        assert_eq!(
            writes[0].path,
            oxpath!("config", "gate", "providers", provider_comp)
        );
        // Apply and verify the endpoint changed.
        apply_writes(&mut snap, &writes);
        let provider: ox_gate::ProviderConfig = super::super::super::renderers::util::read_typed(
            &mut snap,
            &oxpath!(
                "config",
                "gate",
                "providers",
                ox_kernel::PathComponent::try_new("anthropic").unwrap()
            ),
        )
        .unwrap();
        assert_eq!(provider.endpoint, "https://new.example");
    }

    #[test]
    fn commit_with_no_field_path_just_clears_state() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(&oxpath!("ui", "settings", "edit_mode"), Value::Bool(true));
        let writes = run(&Commit::new(), &mut snap);
        // No data write; just the 3 clear-state entries.
        assert_eq!(writes.len(), 3);
    }

    #[test]
    fn read_edit_state_reflects_active_edit() {
        let mut snap = SettingsSnapshot::empty();
        let field_path = oxpath!("settings", "accounts", "alpha", "endpoint");
        snap.insert(&oxpath!("ui", "settings", "edit_mode"), Value::Bool(true));
        snap.insert(
            &oxpath!("ui", "settings", "edit_field_path"),
            path_to_value(&field_path),
        );
        snap.insert(
            &oxpath!("ui", "settings", "edit_buffer"),
            Value::String("typing".into()),
        );
        let state = read_edit_state(&mut snap).unwrap();
        assert_eq!(state.field_path, field_path);
        assert_eq!(state.buffer, "typing");
    }

    #[test]
    fn read_edit_state_returns_none_when_inactive() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(&oxpath!("ui", "settings", "edit_mode"), Value::Bool(false));
        assert!(read_edit_state(&mut snap).is_none());
    }

    #[test]
    fn manual_model_commit_id_advances_to_ctx_stage() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "account"),
            Value::String("alpha".into()),
        );
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "stage"),
            Value::String("id".into()),
        );
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "buffer"),
            Value::String("custom-model".into()),
        );
        let writes = run(&Commit::new(), &mut snap);
        // Expect stage="ctx", staged_id="custom-model", buffer="" → 3 writes.
        let by_path: std::collections::BTreeMap<_, _> = writes
            .iter()
            .map(|w| (w.path.to_string(), w.record.as_value().unwrap().clone()))
            .collect();
        assert_eq!(
            by_path.get("ui/settings/manual_model/stage").unwrap(),
            &Value::String("ctx".into())
        );
        assert_eq!(
            by_path.get("ui/settings/manual_model/staged_id").unwrap(),
            &Value::String("custom-model".into())
        );
        assert_eq!(
            by_path.get("ui/settings/manual_model/buffer").unwrap(),
            &Value::String(String::new())
        );
    }

    #[test]
    fn manual_model_commit_id_rejects_empty() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "stage"),
            Value::String("id".into()),
        );
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "buffer"),
            Value::String("   ".into()),
        );
        let writes = run(&Commit::new(), &mut snap);
        assert!(writes.is_empty(), "empty/whitespace id must not advance");
    }

    #[test]
    fn manual_model_commit_ctx_rejects_non_numeric() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "stage"),
            Value::String("ctx".into()),
        );
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "buffer"),
            Value::String("not-a-number".into()),
        );
        let writes = run(&Commit::new(), &mut snap);
        assert!(writes.is_empty());
    }

    #[test]
    fn manual_model_cancel_clears_form_without_writing_catalog() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "stage"),
            Value::String("ctx".into()),
        );
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "staged_id"),
            Value::String("custom".into()),
        );
        snap.insert(&oxpath!("ui", "settings", "edit_mode"), Value::Bool(true));
        let writes = run(&Cancel::new(), &mut snap);
        // No catalog write; all manual_model paths nulled; edit_mode off.
        assert!(
            !writes
                .iter()
                .any(|w| w.path.to_string().starts_with("config/gate/accounts"))
        );
        assert!(
            writes
                .iter()
                .any(|w| w.path.to_string() == "ui/settings/manual_model/stage")
        );
        assert!(
            writes
                .iter()
                .any(|w| w.path == oxpath!("ui", "settings", "edit_mode"))
        );
    }

    #[test]
    fn manual_model_commit_out_writes_full_modelinfo_and_clears_form() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "account"),
            Value::String("alpha".into()),
        );
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "stage"),
            Value::String("out".into()),
        );
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "staged_id"),
            Value::String("custom-model".into()),
        );
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "staged_ctx"),
            Value::String("100000".into()),
        );
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "buffer"),
            Value::String("8000".into()),
        );
        let writes = run(&Commit::new(), &mut snap);
        // The catalog write goes to config/gate/accounts/alpha/models;
        // form clears via several deletes; edit_mode flips off.
        let catalog_write = writes
            .iter()
            .find(|w| w.path.to_string() == "config/gate/accounts/alpha/models")
            .expect("catalog write");
        let models: Vec<ox_gate::ModelInfo> =
            structfs_serde_store::from_value(catalog_write.record.as_value().unwrap().clone())
                .unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "custom-model");
        assert_eq!(models[0].max_context_size, Some(100_000));
        assert_eq!(models[0].max_output_tokens, Some(8_000));
        assert!(matches!(
            models[0].source,
            ox_gate::ModelInfoSource::UserEntered
        ));
    }
}
