//! Account/model/field commands — the bulk of day-one settings actions.
//!
//! These commands cover: opening compose/delete-confirm modes,
//! test/refresh triggers, primary-model binding, an app-save trigger,
//! field focus cycling, in-place text editing, and selector cycling
//! for Protocol / Auth.
//!
//! All commands are pure (`run` returns `Vec<Write>`). Async-only
//! actions (test, refresh, save) write `…/<verb>_now` triggers that
//! gate-side subscriptions consume. Synchronous actions (account
//! create / delete) are direct writes from the CLI; they don't need
//! a subscription middle-layer.

use ox_kernel::AccountName;
use ox_path::oxpath;
use ox_types::Screen;
use ox_types::settings::{AccountField, GlobalBanner, ModelField, ModelKey};
use ox_types::subscription::Write;
use structfs_core_store::{Path, Reader, Record, Value};
use structfs_serde_store::to_value;

use ox_gate::{AccountConfig, AuthScheme, CompletionRole, ProviderConfig};

use crate::settings::command_registry::CommandRegistry;
use crate::settings::renderers::util::read_typed;

#[allow(unused_imports)]
use super::command;
use super::navigation::path_to_value;

// ---------------------------------------------------------------------------
// Cursor-shuffle commands (overlays)
// ---------------------------------------------------------------------------

command! {
    struct_name: AccountsAdd,
    id: "accounts.add",
    title: "Add Connection",
    description: "Open the inline new-connection prompt at the top of the accounts section.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "accounts")),
    run: |snap, _ctx| accounts_add(snap),
}

// Compose-mode commands. The dispatcher routes printable / Backspace /
// Enter / Esc to these while `ui/settings/new_account/buffer` is `Some`,
// via the synthetic `settings/_compose_new_account` binding scope.
// The buffer is the single source of truth for the in-flight name.

command! {
    struct_name: AccountsComposeInsertChar,
    id: "accounts.compose.insert_char",
    title: "Insert character",
    description: "Append the just-pressed printable char to the new-account name buffer.",
    screen: Screen::Settings,
    cursor: None,
    run: |snap, ctx| accounts_compose_insert_char(snap, ctx),
}

command! {
    struct_name: AccountsComposeDeleteBack,
    id: "accounts.compose.delete_back",
    title: "Backspace",
    description: "Pop the last character from the new-account name buffer.",
    screen: Screen::Settings,
    cursor: None,
    run: |snap, _ctx| accounts_compose_delete_back(snap),
}

command! {
    struct_name: AccountsComposeCommit,
    id: "accounts.compose.commit",
    title: "Create connection",
    description: "Validate the buffered name and materialize the AccountConfig.",
    screen: Screen::Settings,
    cursor: None,
    run: |snap, _ctx| accounts_compose_commit(snap),
}

command! {
    struct_name: AccountsComposeCancel,
    id: "accounts.compose.cancel",
    title: "Cancel new connection",
    description: "Discard the new-account buffer; exit compose mode.",
    screen: Screen::Settings,
    cursor: None,
    run: |_snap, _ctx| vec![Write {
        path: oxpath!("ui", "settings", "new_account", "buffer"),
        record: Record::parsed(Value::Null),
    }],
}

// Pending-delete confirmation commands. The dispatcher routes y / n / Esc
// to these while `ui/settings/pending_delete` is `Some(_)`, via the
// synthetic `settings/_pending_delete` binding scope. The pending pointer
// is the single source of truth for which account is being confirmed.

command! {
    struct_name: AccountsConfirmDelete,
    id: "accounts.confirm.delete",
    title: "Confirm delete",
    description: "Delete the pending account record; clear the pending-delete pointer.",
    screen: Screen::Settings,
    cursor: None,
    run: |snap, _ctx| accounts_confirm_delete(snap),
}

command! {
    struct_name: AccountsConfirmCancel,
    id: "accounts.confirm.cancel",
    title: "Cancel delete",
    description: "Dismiss the delete-confirmation banner without deleting.",
    screen: Screen::Settings,
    cursor: None,
    run: |_snap, _ctx| vec![Write {
        path: oxpath!("ui", "settings", "pending_delete"),
        record: Record::parsed(Value::Null),
    }],
}

command! {
    struct_name: AccountsDeleteConfirm,
    id: "accounts.delete_confirm",
    title: "Delete Connection…",
    description: "Open the delete-confirmation banner for the selected Connection.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "accounts")),
    run: |snap, _ctx| accounts_delete_confirm(snap),
}

// ---------------------------------------------------------------------------
// Subscription-request commands
// ---------------------------------------------------------------------------

command! {
    struct_name: AccountTest,
    id: "account.test",
    title: "Test Connection",
    description: "Trigger a connection test for the selected Connection.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "accounts", "_detail")),
    run: |snap, _ctx| account_test(snap),
}

command! {
    struct_name: AccountRefresh,
    id: "account.refresh",
    title: "Refresh Catalog",
    description: "Re-fetch the model catalog for the selected model's Connection.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "models")),
    run: |snap, _ctx| account_refresh(snap),
}

command! {
    struct_name: ModelsSetBootstrap,
    id: "models.set_bootstrap",
    title: "Set as Bootstrap",
    description: "Bind config/gate/completions/bootstrap to the selected (account, model). Also writes the legacy config/gate/completions/primary path during the migration window.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "models")),
    run: |snap, _ctx| models_set_bootstrap(snap),
}

command! {
    struct_name: ModelsToggleDefault,
    id: "models.toggle_default",
    title: "Toggle Default-Available",
    description: "Add or remove the focused (account, model) from the default-available set.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "models")),
    run: |snap, _ctx| models_toggle_default(snap),
}

command! {
    struct_name: AppSave,
    id: "app.save",
    title: "Save",
    description: "Persist edits to disk via the save subscription.",
    screen: Screen::Settings,
    cursor: None,
    run: |_snap, _ctx| vec![Write {
        path: oxpath!("config", "save"),
        record: Record::parsed(Value::Null),
    }],
}

// ---------------------------------------------------------------------------
// Field focus cycling
// ---------------------------------------------------------------------------

command! {
    struct_name: FieldAccountNext,
    id: "field.account.next",
    title: "Next Field",
    description: "Cycle the focused Connection-detail field forward.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "accounts", "_detail")),
    run: |snap, _ctx| field_account_step(snap, 1),
}

command! {
    struct_name: FieldAccountPrev,
    id: "field.account.prev",
    title: "Previous Field",
    description: "Cycle the focused Connection-detail field backward.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "accounts", "_detail")),
    run: |snap, _ctx| field_account_step(snap, -1),
}

command! {
    struct_name: FieldModelNext,
    id: "field.model.next",
    title: "Next Field",
    description: "Cycle the focused model-detail field forward.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "models", "_detail")),
    run: |snap, _ctx| field_model_step(snap, 1),
}

command! {
    struct_name: FieldModelPrev,
    id: "field.model.prev",
    title: "Previous Field",
    description: "Cycle the focused model-detail field backward.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "models", "_detail")),
    run: |snap, _ctx| field_model_step(snap, -1),
}

// ---------------------------------------------------------------------------
// Selector cycling (Protocol, Auth)
// ---------------------------------------------------------------------------

command! {
    struct_name: SelectorCycleProtocol,
    id: "selector.cycle.protocol",
    title: "Cycle Protocol",
    description: "Advance the Connection's protocol/dialect selector.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "accounts", "_detail")),
    run: |snap, _ctx| selector_cycle_protocol(snap),
}

command! {
    struct_name: SelectorCycleAuth,
    id: "selector.cycle.auth",
    title: "Cycle Auth",
    description: "Advance the provider's auth-scheme selector.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "accounts", "_detail")),
    run: |snap, _ctx| selector_cycle_auth(snap),
}

command! {
    struct_name: AccountsForkProvider,
    id: "accounts.fork_provider",
    title: "Fork Provider",
    description: "Clone the bound provider so this Connection no longer shares it with others. Edits to endpoint/auth/version then affect only this Connection.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "accounts")),
    run: |snap, _ctx| accounts_fork_provider(snap),
}

// Manual-model commands. The dispatcher routes printable / Backspace /
// Enter / Esc to these while `ui/settings/manual_model/stage` holds a
// typed `ManualModelStage` value, via the synthetic
// `settings/_manual_model` binding scope. The stage value doubles as
// the mode discriminator: typed shape is the new flow; legacy
// stringly-typed shape ("id"/"ctx"/"out") is the dormant old flow.

command! {
    struct_name: ModelsAddManual,
    id: "models.add_manual",
    title: "Add Model Manually",
    description: "Open the inline three-stage manual-model entry form for the focused account.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "models")),
    run: |snap, _ctx| models_add_manual(snap),
}

command! {
    struct_name: ModelsManualInsertChar,
    id: "models.compose_manual.insert_char",
    title: "Insert character (manual model)",
    description: "Append the just-pressed char to the manual-model buffer (per-stage rules).",
    screen: Screen::Settings,
    cursor: None,
    run: |snap, ctx| models_manual_insert_char(snap, ctx),
}

command! {
    struct_name: ModelsManualDeleteBack,
    id: "models.compose_manual.delete_back",
    title: "Backspace (manual model)",
    description: "Pop the last character from the manual-model buffer.",
    screen: Screen::Settings,
    cursor: None,
    run: |snap, _ctx| models_manual_delete_back(snap),
}

command! {
    struct_name: ModelsManualCommit,
    id: "models.compose_manual.commit",
    title: "Commit stage (manual model)",
    description: "Advance the form's stage; the final stage finalizes the new ModelInfo into the catalog.",
    screen: Screen::Settings,
    cursor: None,
    run: |snap, _ctx| models_manual_commit(snap),
}

command! {
    struct_name: ModelsManualCancel,
    id: "models.compose_manual.cancel",
    title: "Cancel manual model",
    description: "Discard the manual-model buffer and exit compose mode.",
    screen: Screen::Settings,
    cursor: None,
    run: |snap, _ctx| models_manual_cancel(snap),
}

// ---------------------------------------------------------------------------
// Implementation helpers
// ---------------------------------------------------------------------------

/// Resolve the active account name. The accordion's per-row `Prefix`
/// bindings fire while the focused row sits anywhere under
/// `settings/accounts`, so we honor the focused row first; the
/// legacy `_detail` page (still used by editing flows) writes its
/// selection to `ui/settings/accounts/selected` and we fall back to
/// that if no focus is set.
///
/// Validates the read string at this boundary: a stored value that
/// fails `AccountName::try_new` is treated as "no account selected"
/// rather than propagated as an unvalidated `String`.
fn read_selected_account(data: &mut dyn Reader) -> Option<AccountName> {
    if let Some(name) = focused_account(data) {
        return Some(name);
    }
    let raw: Option<String> =
        read_typed::<Option<String>>(data, &oxpath!("ui", "settings", "accounts", "selected"))
            .flatten();
    raw.and_then(|s| AccountName::try_new(s).ok())
}

/// Same shape for models. Reads the focused row first, falls back to
/// the legacy `_detail` selection.
fn read_selected_model(data: &mut dyn Reader) -> Option<ModelKey> {
    if let Some(key) = focused_model(data) {
        return Some(key);
    }
    read_typed::<Option<ModelKey>>(data, &oxpath!("ui", "settings", "models", "selected")).flatten()
}

/// Resolve the focused row to its real (un-sanitized)
/// account name. Row paths in the visible tree pass user-supplied ids
/// through `safe_component`, which substitutes any non-identifier
/// char with `_`; the original id lives on `RowKind`. Walking the
/// enumeration to find the row whose path matches `focused`
/// recovers the original.
///
/// `RowKind` carries the un-sanitized name as a `String`. This
/// boundary re-validates via `AccountName::try_new`; a row whose
/// account name fails validation produces no match (the same behavior
/// as having no row focused).
fn focused_account(data: &mut dyn Reader) -> Option<AccountName> {
    use crate::settings::visible_rows::{self, RowKind};
    let path = focused_path(data)?;
    let rows = visible_rows::enumerate(data);
    rows.into_iter().find_map(|r| {
        if r.path != path {
            return None;
        }
        let name = match r.kind {
            RowKind::Account { name } => name,
            // Field rows under an expanded account also count — the
            // user has focused a field that belongs to that account.
            RowKind::AccountField { account, .. } => account,
            RowKind::Entry { .. } | RowKind::Model { .. } | RowKind::ModelField { .. } => {
                return None;
            }
        };
        AccountName::try_new(name).ok()
    })
}

/// Same shape for models. `RowKind::Model` and `RowKind::ModelField`
/// both carry the un-sanitized `(account, model_id)` regardless of
/// how the row's path component got encoded.
fn focused_model(data: &mut dyn Reader) -> Option<ModelKey> {
    use crate::settings::visible_rows::{self, RowKind};
    let path = focused_path(data)?;
    let rows = visible_rows::enumerate(data);
    rows.into_iter().find_map(|r| {
        if r.path != path {
            return None;
        }
        match r.kind {
            RowKind::Model { account, model_id } => Some(ModelKey { account, model_id }),
            RowKind::ModelField {
                account, model_id, ..
            } => Some(ModelKey { account, model_id }),
            RowKind::Entry { .. } | RowKind::Account { .. } | RowKind::AccountField { .. } => None,
        }
    })
}

fn focused_path(data: &mut dyn Reader) -> Option<Path> {
    let r = data
        .read(&oxpath!("ui", "settings", "focused"))
        .ok()
        .flatten()?;
    super::navigation::path_from_value(r.as_value()?)
}

fn account_request_path(name: &AccountName, suffix: &str) -> Option<Path> {
    let suf = ox_kernel::PathComponent::try_new(suffix).ok()?;
    Some(oxpath!(
        "config",
        "gate",
        "accounts",
        name.to_path_component(),
        suf
    ))
}

fn null_write(path: Path) -> Write {
    Write {
        path,
        record: Record::parsed(Value::Null),
    }
}

fn accounts_add(data: &mut dyn Reader) -> Vec<Write> {
    use crate::settings::visible_rows::{expanded_set_to_value, read_expanded_set};

    // Open compose mode by writing `Some("")` to the new-account buffer.
    // The dispatcher's compose-mode pass picks this up and routes
    // printable / Backspace / Enter / Esc into the
    // `accounts.compose.*` commands; the renderer reads the buffer to
    // emit the inline name prompt in place of the static
    // "+ New connection" affordance. No focus / edit_mode / edit_buffer
    // bookkeeping — this command's only job is to flip the section open
    // and arm the buffer.
    let mut expanded = read_expanded_set(data);
    let accounts_key = "settings/accounts".to_string();
    if !expanded.iter().any(|s| s == &accounts_key) {
        expanded.push(accounts_key);
    }

    vec![
        Write {
            path: oxpath!("ui", "settings", "expanded"),
            record: Record::parsed(expanded_set_to_value(&expanded)),
        },
        Write {
            path: oxpath!("ui", "settings", "new_account", "buffer"),
            record: Record::parsed(Value::String(String::new())),
        },
    ]
}

fn accounts_compose_insert_char(
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
    let current: String =
        read_typed(data, &oxpath!("ui", "settings", "new_account", "buffer")).unwrap_or_default();
    let mut next = current;
    next.push(ch);
    vec![Write {
        path: oxpath!("ui", "settings", "new_account", "buffer"),
        record: Record::parsed(Value::String(next)),
    }]
}

fn accounts_compose_delete_back(data: &mut dyn Reader) -> Vec<Write> {
    let mut current: String =
        read_typed(data, &oxpath!("ui", "settings", "new_account", "buffer")).unwrap_or_default();
    if current.pop().is_none() {
        return Vec::new();
    }
    vec![Write {
        path: oxpath!("ui", "settings", "new_account", "buffer"),
        record: Record::parsed(Value::String(current)),
    }]
}

fn accounts_compose_commit(data: &mut dyn Reader) -> Vec<Write> {
    let buffer: String =
        read_typed(data, &oxpath!("ui", "settings", "new_account", "buffer")).unwrap_or_default();
    let trimmed = buffer.trim();
    // Empty/whitespace: silent no-op so compose mode stays open.
    if trimmed.is_empty() {
        return Vec::new();
    }
    let name = match AccountName::try_new(trimmed.to_string()) {
        Ok(n) => n,
        Err(_) => {
            let banner = GlobalBanner::error(format!("Invalid account name: '{}'", trimmed));
            return vec![Write {
                path: oxpath!("ui", "global", "banner"),
                record: Record::parsed(to_value(&banner).unwrap()),
            }];
        }
    };
    let comp = name.to_path_component();

    let cfg = AccountConfig {
        provider: "anthropic".to_string(),
    };
    let new_account_row = oxpath!("settings", "accounts", comp.clone());
    let mut expanded: Vec<String> =
        read_typed(data, &oxpath!("ui", "settings", "expanded")).unwrap_or_default();
    let accounts_key = "settings/accounts".to_string();
    let new_row_key = format!("settings/accounts/{}", name);
    if !expanded.iter().any(|s| s == &accounts_key) {
        expanded.push(accounts_key);
    }
    if !expanded.iter().any(|s| s == &new_row_key) {
        expanded.push(new_row_key);
    }

    let acct_path = oxpath!("config", "gate", "accounts", comp);
    let cfg_value = match to_value(&cfg) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let selected_value = match to_value(&Some(name.as_str().to_string())) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let expanded_value = match to_value(&expanded) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    vec![
        Write {
            path: acct_path,
            record: Record::parsed(cfg_value),
        },
        Write {
            path: oxpath!("ui", "settings", "accounts", "selected"),
            record: Record::parsed(selected_value),
        },
        Write {
            path: oxpath!("ui", "settings", "cursor"),
            record: Record::parsed(path_to_value(&oxpath!("settings", "index"))),
        },
        Write {
            path: oxpath!("ui", "settings", "focused"),
            record: Record::parsed(path_to_value(&new_account_row)),
        },
        Write {
            path: oxpath!("ui", "settings", "expanded"),
            record: Record::parsed(expanded_value),
        },
        Write {
            path: oxpath!("ui", "settings", "new_account", "buffer"),
            record: Record::parsed(Value::Null),
        },
    ]
}

// ---------------------------------------------------------------------------
// Manual-model helpers
// ---------------------------------------------------------------------------

/// Open the manual-model entry form for the focused account. Resolves
/// the focused row to its un-sanitized account name (Model and
/// ModelField rows both carry it), then seeds
/// `manual_model/{account, stage, buffer}` with the typed Stage::Id
/// value. The PascalCase wire format distinguishes the new flow from
/// the legacy stringly-typed write site, which lets the dispatcher
/// gate cleanly on shape.
fn models_add_manual(data: &mut dyn Reader) -> Vec<Write> {
    use ox_types::settings::ManualModelStage;
    use crate::settings::visible_rows::{enumerate, RowKind};

    let focused = match focused_path(data) {
        Some(p) => p,
        None => return Vec::new(),
    };
    let rows = enumerate(data);
    let account = rows.iter().find(|r| r.path == focused).and_then(|r| match &r.kind {
        RowKind::Model { account, .. } => Some(account.clone()),
        RowKind::ModelField { account, .. } => Some(account.clone()),
        RowKind::Entry { .. } | RowKind::Account { .. } | RowKind::AccountField { .. } => None,
    });
    let Some(account) = account else {
        return Vec::new();
    };

    let stage_value = match to_value(&ManualModelStage::Id) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    vec![
        Write {
            path: oxpath!("ui", "settings", "manual_model", "account"),
            record: Record::parsed(Value::String(account)),
        },
        Write {
            path: oxpath!("ui", "settings", "manual_model", "stage"),
            record: Record::parsed(stage_value),
        },
        Write {
            path: oxpath!("ui", "settings", "manual_model", "buffer"),
            record: Record::parsed(Value::String(String::new())),
        },
    ]
}

/// Read the typed-shape stage. Returns `None` when absent or when the
/// stored value is the legacy stringly-typed shape — the same
/// discriminator the dispatcher uses, so once a command runs here it
/// can trust the typed shape is in place.
fn models_manual_read_stage(data: &mut dyn Reader) -> Option<ox_types::settings::ManualModelStage> {
    read_typed(data, &oxpath!("ui", "settings", "manual_model", "stage"))
}

fn models_manual_insert_char(
    data: &mut dyn Reader,
    ctx: &super::super::command_registry::CommandCtx<'_>,
) -> Vec<Write> {
    use ox_types::key_chord::KeyCodeRepr;
    use ox_types::settings::ManualModelStage;

    let chord = match ctx.last_keystroke.as_ref() {
        Some(c) => c,
        None => return Vec::new(),
    };
    let ch = match chord.code {
        KeyCodeRepr::Char(c) => c,
        _ => return Vec::new(),
    };
    let stage = match models_manual_read_stage(data) {
        Some(s) => s,
        None => return Vec::new(),
    };
    // Per-stage accept rules: Id accepts any printable char; Ctx and
    // Out accept ASCII digits only (the values are u32 sizes).
    let accept = match stage {
        ManualModelStage::Id => true,
        ManualModelStage::Ctx | ManualModelStage::Out => ch.is_ascii_digit(),
    };
    if !accept {
        return Vec::new();
    }
    let current: String =
        read_typed(data, &oxpath!("ui", "settings", "manual_model", "buffer")).unwrap_or_default();
    let mut next = current;
    next.push(ch);
    vec![Write {
        path: oxpath!("ui", "settings", "manual_model", "buffer"),
        record: Record::parsed(Value::String(next)),
    }]
}

fn models_manual_delete_back(data: &mut dyn Reader) -> Vec<Write> {
    let mut current: String =
        read_typed(data, &oxpath!("ui", "settings", "manual_model", "buffer")).unwrap_or_default();
    if current.pop().is_none() {
        return Vec::new();
    }
    vec![Write {
        path: oxpath!("ui", "settings", "manual_model", "buffer"),
        record: Record::parsed(Value::String(current)),
    }]
}

fn models_manual_commit(data: &mut dyn Reader) -> Vec<Write> {
    use ox_gate::{ModelInfo, ModelInfoSource};
    use ox_types::settings::ManualModelStage;

    let stage = match models_manual_read_stage(data) {
        Some(s) => s,
        None => return Vec::new(),
    };
    let buffer: String =
        read_typed(data, &oxpath!("ui", "settings", "manual_model", "buffer")).unwrap_or_default();
    let trimmed = buffer.trim();

    match stage {
        ManualModelStage::Id => {
            if trimmed.is_empty() {
                return Vec::new();
            }
            let next_stage = match to_value(&ManualModelStage::Ctx) {
                Ok(v) => v,
                Err(_) => return Vec::new(),
            };
            vec![
                Write {
                    path: oxpath!("ui", "settings", "manual_model", "stage"),
                    record: Record::parsed(next_stage),
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
        ManualModelStage::Ctx => {
            let n: u32 = match trimmed.parse() {
                Ok(n) if n > 0 => n,
                _ => return Vec::new(),
            };
            let next_stage = match to_value(&ManualModelStage::Out) {
                Ok(v) => v,
                Err(_) => return Vec::new(),
            };
            vec![
                Write {
                    path: oxpath!("ui", "settings", "manual_model", "stage"),
                    record: Record::parsed(next_stage),
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
        ManualModelStage::Out => {
            let out: u32 = match trimmed.parse() {
                Ok(n) if n > 0 => n,
                _ => return Vec::new(),
            };
            let id: String = read_typed(
                data,
                &oxpath!("ui", "settings", "manual_model", "staged_id"),
            )
            .unwrap_or_default();
            let ctx: u32 = read_typed::<String>(
                data,
                &oxpath!("ui", "settings", "manual_model", "staged_ctx"),
            )
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
            let account_raw: String = read_typed(
                data,
                &oxpath!("ui", "settings", "manual_model", "account"),
            )
            .unwrap_or_default();
            // Broker-read boundary: lift the raw String into AccountName.
            let account = match AccountName::try_new(account_raw) {
                Ok(n) => n,
                Err(_) => return Vec::new(),
            };
            let comp = account.to_path_component();

            let catalog_path = oxpath!("config", "gate", "accounts", comp, "models");
            let mut catalog: Vec<ModelInfo> = read_typed(data, &catalog_path).unwrap_or_default();
            catalog.push(ModelInfo {
                id: id.clone(),
                display_name: id,
                max_context_size: Some(ctx),
                max_output_tokens: Some(out),
                source: ModelInfoSource::UserEntered,
            });
            let catalog_value = match to_value(&catalog) {
                Ok(v) => v,
                Err(_) => return Vec::new(),
            };

            // Write the catalog and clear the form state.
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
            ]
        }
    }
}

fn models_manual_cancel(_data: &mut dyn Reader) -> Vec<Write> {
    // Clear all manual_model paths.
    vec![
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
    ]
}

fn accounts_delete_confirm(data: &mut dyn Reader) -> Vec<Write> {
    let name = match read_selected_account(data) {
        Some(n) => n,
        None => return Vec::new(),
    };
    vec![Write {
        path: oxpath!("ui", "settings", "pending_delete"),
        record: Record::parsed(Value::String(name.into_string())),
    }]
}

fn accounts_confirm_delete(data: &mut dyn Reader) -> Vec<Write> {
    use ox_kernel::PathComponent;

    // Read the pending account name. If unset, the dispatch shouldn't
    // have routed here — defensive no-op.
    let name: String = read_typed(data, &oxpath!("ui", "settings", "pending_delete"))
        .unwrap_or_default();
    if name.is_empty() {
        return Vec::new();
    }
    let comp = match PathComponent::try_new(&name) {
        Ok(c) => c,
        Err(_) => {
            // Pending pointer somehow got an invalid name. Clear it
            // defensively so we don't leave the user stuck in
            // confirmation mode.
            return vec![Write {
                path: oxpath!("ui", "settings", "pending_delete"),
                record: Record::parsed(Value::Null),
            }];
        }
    };

    vec![
        // The actual delete — Null write to the canonical account
        // path. The AccountDeleteCleanupSubscription watches Prefix
        // for null writes at account-record depth and does the
        // cross-cutting side-data cleanup.
        Write {
            path: oxpath!("config", "gate", "accounts", comp),
            record: Record::parsed(Value::Null),
        },
        // Clear the pending pointer.
        Write {
            path: oxpath!("ui", "settings", "pending_delete"),
            record: Record::parsed(Value::Null),
        },
    ]
}

fn account_test(data: &mut dyn Reader) -> Vec<Write> {
    let name = match read_selected_account(data) {
        Some(n) => n,
        None => return Vec::new(),
    };
    match account_request_path(&name, "test_now") {
        Some(p) => vec![null_write(p)],
        None => Vec::new(),
    }
}

fn account_refresh(data: &mut dyn Reader) -> Vec<Write> {
    // Resolve the target account from whatever row is focused. The
    // Models-section path (Model / ModelField rows) stays — `r` was
    // historically bound there. Accounts-section paths (Account /
    // AccountField rows) are the fallback so empty-catalog accounts —
    // which contribute zero rows in the Models section — remain
    // refreshable from their Connections-section row.
    let name = match read_selected_model(data) {
        // ModelKey.account is still a plain String; validate at this
        // boundary so the rest of the function operates on AccountName.
        Some(k) => match AccountName::try_new(k.account) {
            Ok(n) => n,
            Err(_) => return Vec::new(),
        },
        None => match read_selected_account(data) {
            Some(n) => n,
            None => return Vec::new(),
        },
    };
    match account_request_path(&name, "refresh_now") {
        Some(p) => vec![null_write(p)],
        None => Vec::new(),
    }
}

fn models_set_bootstrap(data: &mut dyn Reader) -> Vec<Write> {
    let key = match read_selected_model(data) {
        Some(k) => k,
        None => return Vec::new(),
    };
    let role = CompletionRole {
        account: key.account,
        model_id: key.model_id,
    };
    let value = match to_value(&role) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "models.set_bootstrap: failed to encode CompletionRole");
            return Vec::new();
        }
    };
    // Two writes during the migration window: the new path is the
    // source of truth, the legacy path stays in lockstep so a downgrade
    // (or any kernel call site that hasn't yet switched) sees the same
    // bootstrap choice. The legacy write can be removed in a follow-up
    // once every reader has migrated.
    vec![
        Write {
            path: oxpath!("config", "gate", "completions", "bootstrap"),
            record: Record::parsed(value.clone()),
        },
        Write {
            path: oxpath!("config", "gate", "completions", "primary"),
            record: Record::parsed(value),
        },
    ]
}

/// Read-modify-write toggle on
/// `config/gate/completions/default_available: Vec<ModelKey>`. The
/// record's empty/absent state is the canonical "no explicit subset"
/// signal — when the toggle empties the set, write `Value::Null` to
/// delete the record so the kernel falls back to the implicit
/// "all cataloged models default-available" behavior.
fn models_toggle_default(data: &mut dyn Reader) -> Vec<Write> {
    let key = match read_selected_model(data) {
        Some(k) => k,
        None => return Vec::new(),
    };
    let current: Vec<ModelKey> = read_typed(
        data,
        &oxpath!("config", "gate", "completions", "default_available"),
    )
    .unwrap_or_default();

    let mut next = current;
    if let Some(pos) = next
        .iter()
        .position(|k| k.account == key.account && k.model_id == key.model_id)
    {
        next.remove(pos);
    } else {
        next.push(key);
    }

    if next.is_empty() {
        return vec![Write {
            path: oxpath!("config", "gate", "completions", "default_available"),
            record: Record::parsed(Value::Null),
        }];
    }

    let value = match to_value(&next) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "models.toggle_default: failed to encode");
            return Vec::new();
        }
    };
    vec![Write {
        path: oxpath!("config", "gate", "completions", "default_available"),
        record: Record::parsed(value),
    }]
}

const ACCOUNT_FIELDS: [AccountField; 5] = [
    AccountField::Name,
    AccountField::Protocol,
    AccountField::Endpoint,
    AccountField::Auth,
    AccountField::Key,
];

const MODEL_FIELDS: [ModelField; 2] = [
    ModelField::ContextSizeOverride,
    ModelField::OutputTokensOverride,
];

fn cycle_index(current: usize, len: usize, delta: isize) -> usize {
    let len_i = len as isize;
    let next = (current as isize + delta).rem_euclid(len_i);
    next as usize
}

fn field_account_step(data: &mut dyn Reader, delta: isize) -> Vec<Write> {
    let current: AccountField =
        read_typed(data, &oxpath!("ui", "settings", "account_detail", "field"))
            .unwrap_or(AccountField::Name);
    let idx = ACCOUNT_FIELDS
        .iter()
        .position(|f| *f == current)
        .unwrap_or(0);
    let next = ACCOUNT_FIELDS[cycle_index(idx, ACCOUNT_FIELDS.len(), delta)];
    let value = match to_value(&next) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "field.account: failed to encode AccountField");
            return Vec::new();
        }
    };
    vec![Write {
        path: oxpath!("ui", "settings", "account_detail", "field"),
        record: Record::parsed(value),
    }]
}

fn field_model_step(data: &mut dyn Reader, delta: isize) -> Vec<Write> {
    let current: ModelField = read_typed(data, &oxpath!("ui", "settings", "model_detail", "field"))
        .unwrap_or(ModelField::ContextSizeOverride);
    let idx = MODEL_FIELDS.iter().position(|f| *f == current).unwrap_or(0);
    let next = MODEL_FIELDS[cycle_index(idx, MODEL_FIELDS.len(), delta)];
    let value = match to_value(&next) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "field.model: failed to encode ModelField");
            return Vec::new();
        }
    };
    vec![Write {
        path: oxpath!("ui", "settings", "model_detail", "field"),
        record: Record::parsed(value),
    }]
}

/// Ordered display labels for the Auth carousel. Auth's options are a
/// fixed wire-protocol enum (`AuthScheme`), so the labels are static —
/// unlike Protocol, whose options come from the broker via
/// `resolve_protocol_options`.
pub const AUTH_DISPLAY: &[&str] = &["x-api-key", "bearer-token", "none"];

/// Resolve the carousel options for the Protocol field.
///
/// Protocol characterizes *what wire format the endpoint speaks* — the
/// dialect — not which provider record an account binds to. Options come
/// from the built-in preset table's `dialect` field (today: `anthropic`,
/// `openai`); a custom value currently bound to a provider record is
/// appended so cycling never silently overwrites a dialect we don't
/// recognize.
///
/// Provider record *names* (e.g. `LMStudio`, `corp-gateway`) are NOT
/// carousel options — they're identifiers for endpoint+dialect+auth
/// bundles. Multiple records can speak the same dialect; that's why
/// `LMStudio (openai dialect)` and `lm_studio (openai dialect)` appear
/// once each as `openai`, not twice.
pub fn resolve_protocol_options(_data: &mut dyn Reader, current: &str) -> Vec<String> {
    let _ = _data; // kept for signature stability; future dialects could be data-driven
    let mut options: Vec<String> = ox_gate::presets()
        .iter()
        .filter(|p| !p.custom)
        .map(|p| p.dialect.to_string())
        .collect();
    // Dedupe in case two presets share a dialect (none today; defensive).
    options.dedup();

    if !current.is_empty() && !options.iter().any(|o| o == current) {
        options.push(current.to_string());
    }

    options
}

/// Direction for selector cycling. `Forward` is what the legacy
/// `selector_cycle_*` commands have always done; `Back` mirrors it.
#[derive(Clone, Copy)]
pub(crate) enum CycleDir {
    Forward,
    Back,
}

pub(crate) fn selector_cycle_protocol(data: &mut dyn Reader) -> Vec<Write> {
    selector_cycle_protocol_dir(data, CycleDir::Forward)
}

pub(crate) fn selector_cycle_protocol_back(data: &mut dyn Reader) -> Vec<Write> {
    selector_cycle_protocol_dir(data, CycleDir::Back)
}

fn selector_cycle_protocol_dir(data: &mut dyn Reader, dir: CycleDir) -> Vec<Write> {
    let selected = match read_selected_account(data) {
        Some(s) => s,
        None => {
            tracing::info!("selector.cycle.protocol: no selected account, no-op");
            return Vec::new();
        }
    };
    let acct_name_comp = selected.to_path_component();
    let acct_path = oxpath!("config", "gate", "accounts", acct_name_comp);
    // TOML-loaded accounts may not have a parent `AccountConfig` leaf
    // — only child fields. Synthesize one (using a child `provider`
    // string if present) so the bound-provider lookup below can find
    // a record to mutate.
    let acct: AccountConfig = read_typed(data, &acct_path).unwrap_or_else(|| AccountConfig {
        provider: read_account_child_string(data, &selected, "provider")
            .unwrap_or_else(|| "anthropic".to_string()),
    });

    // The carousel cycles *dialects*, not provider record names. The
    // mutation target is the bound provider record's `dialect` field —
    // the account's `provider` reference stays stable. If multiple
    // accounts share this provider record, the dialect change applies
    // to all of them; the share-set indicator on each account row
    // surfaces that coupling so the user isn't surprised.
    let provider_name_comp = match ox_kernel::PathComponent::try_new(&acct.provider) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let provider_path = oxpath!("config", "gate", "providers", provider_name_comp);
    // Read the provider via the assembling helper so a TOML-loaded
    // record (flat sub-keys, no parent Map) still returns the user's
    // actual endpoint/auth/version. Without this we'd synthesize an
    // empty default and the first cycle would silently wipe those
    // fields when writing the parent Map back. Only when even the
    // flat sub-keys are absent (true orphan binding — account points
    // at a provider record that doesn't exist) do we synthesize
    // defaults; that path creates the record with the new dialect.
    let mut provider: ProviderConfig =
        crate::settings::visible_rows::read_provider_assembling_flat(data, &acct.provider)
            .unwrap_or_else(|| ProviderConfig {
                dialect: acct.provider.clone(),
                endpoint: String::new(),
                version: String::new(),
                auth: None,
            });

    let options = resolve_protocol_options(data, &provider.dialect);
    if options.is_empty() {
        return Vec::new();
    }
    let idx = options
        .iter()
        .position(|o| o == &provider.dialect)
        .unwrap_or(0);
    let next = match dir {
        CycleDir::Forward => options[(idx + 1) % options.len()].clone(),
        CycleDir::Back => options[(idx + options.len() - 1) % options.len()].clone(),
    };
    provider.dialect = next;

    let value = match to_value(&provider) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "selector.cycle.protocol: failed to encode ProviderConfig");
            return Vec::new();
        }
    };
    tracing::info!(
        account = %selected,
        provider = %acct.provider,
        new_dialect = %provider.dialect,
        "selector.cycle.protocol: writing provider record"
    );
    vec![Write {
        path: provider_path,
        record: Record::parsed(value),
    }]
}

command! {
    struct_name: CycleFieldNext,
    id: "cycle.field.next",
    title: "Cycle field next",
    description: "Advance the focused selector field to its next option.",
    screen: Screen::Settings,
    cursor: None,
    run: |snap, _ctx| cycle_field(snap, CycleDir::Forward),
}

command! {
    struct_name: CycleFieldPrev,
    id: "cycle.field.prev",
    title: "Cycle field prev",
    description: "Advance the focused selector field to its previous option.",
    screen: Screen::Settings,
    cursor: None,
    run: |snap, _ctx| cycle_field(snap, CycleDir::Back),
}

fn cycle_field(data: &mut dyn Reader, dir: CycleDir) -> Vec<Write> {
    use crate::settings::visible_rows::{self, RowKind};
    let path = match focused_path(data) {
        Some(p) => p,
        None => return Vec::new(),
    };
    let rows = visible_rows::enumerate(data);
    let row = match rows.into_iter().find(|r| r.path == path) {
        Some(r) => r,
        None => return Vec::new(),
    };
    match row.kind {
        RowKind::AccountField {
            field: AccountField::Protocol,
            ..
        } => selector_cycle_protocol_dir(data, dir),
        RowKind::AccountField {
            field: AccountField::Auth,
            ..
        } => selector_cycle_auth_dir(data, dir),
        RowKind::AccountField {
            field: AccountField::Name | AccountField::Endpoint | AccountField::Key,
            ..
        }
        | RowKind::Entry { .. }
        | RowKind::Account { .. }
        | RowKind::Model { .. }
        | RowKind::ModelField { .. } => Vec::new(),
    }
}

/// Read a child string under `config/gate/accounts/{name}/{child}`
/// — the shape TOML-loaded accounts produce when there's no
/// AccountConfig leaf at the parent.
fn read_account_child_string(
    data: &mut dyn Reader,
    account: &AccountName,
    child: &str,
) -> Option<String> {
    let child_comp = ox_kernel::PathComponent::try_new(child).ok()?;
    let r = data
        .read(&oxpath!(
            "config",
            "gate",
            "accounts",
            account.to_path_component(),
            child_comp
        ))
        .ok()
        .flatten()?;
    match r.as_value()? {
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

const AUTH_OPTIONS: [AuthScheme; 3] = [
    AuthScheme::XApiKey,
    AuthScheme::BearerToken,
    AuthScheme::None,
];

pub(crate) fn selector_cycle_auth(data: &mut dyn Reader) -> Vec<Write> {
    selector_cycle_auth_dir(data, CycleDir::Forward)
}

pub(crate) fn selector_cycle_auth_back(data: &mut dyn Reader) -> Vec<Write> {
    selector_cycle_auth_dir(data, CycleDir::Back)
}

fn selector_cycle_auth_dir(data: &mut dyn Reader, dir: CycleDir) -> Vec<Write> {
    let selected = match read_selected_account(data) {
        Some(s) => s,
        None => return Vec::new(),
    };
    let name_comp = selected.to_path_component();
    // Synthesize a default `AccountConfig` when the parent leaf is
    // missing (TOML-loaded accounts), pulling the provider name
    // from the child path if present.
    let acct: AccountConfig = read_typed(
        data,
        &oxpath!("config", "gate", "accounts", name_comp.clone()),
    )
    .unwrap_or_else(|| AccountConfig {
        provider: read_account_child_string(data, &selected, "provider")
            .unwrap_or_else(|| "anthropic".to_string()),
    });
    let provider_comp = match ox_kernel::PathComponent::try_new(&acct.provider) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let provider_path = oxpath!("config", "gate", "providers", provider_comp);
    // Same shape for ProviderConfig: synthesize a default when no
    // leaf exists, so the first cycle creates the row.
    let mut provider: ProviderConfig =
        read_typed(data, &provider_path).unwrap_or_else(|| ProviderConfig {
            dialect: acct.provider.clone(),
            endpoint: String::new(),
            version: String::new(),
            auth: None,
        });
    let current = provider.resolved_auth();
    let idx = AUTH_OPTIONS.iter().position(|a| *a == current).unwrap_or(0);
    let next = match dir {
        CycleDir::Forward => AUTH_OPTIONS[(idx + 1) % AUTH_OPTIONS.len()].clone(),
        CycleDir::Back => AUTH_OPTIONS[(idx + AUTH_OPTIONS.len() - 1) % AUTH_OPTIONS.len()].clone(),
    };
    provider.auth = Some(next);
    let value = match to_value(&provider) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "selector.cycle.auth: failed to encode ProviderConfig");
            return Vec::new();
        }
    };
    vec![Write {
        path: provider_path,
        record: Record::parsed(value),
    }]
}

/// Clone the bound provider record under a fresh name and repoint the
/// selected account at the new entry. No-op when the provider isn't
/// shared with any other account — already exclusive, no untangling
/// needed. No-op when no account is selected or the provider name
/// can't form a valid path component.
fn accounts_fork_provider(data: &mut dyn Reader) -> Vec<Write> {
    let selected = match read_selected_account(data) {
        Some(s) => s,
        None => return Vec::new(),
    };
    let acct_comp = selected.to_path_component();
    let acct_path = oxpath!("config", "gate", "accounts", acct_comp.clone());
    let mut acct: AccountConfig = match read_typed(data, &acct_path) {
        Some(a) => a,
        None => return Vec::new(),
    };

    // Count other accounts that share this provider. If zero, the
    // fork is a no-op — the provider is already exclusive to this
    // account and untangling would only add a confusing rename.
    let names = crate::settings::renderers::util::child_names_under(data, "config/gate/accounts");
    let mut other_users = 0;
    for n in &names {
        if n == selected.as_str() {
            continue;
        }
        if let Ok(other_comp) = ox_kernel::PathComponent::try_new(n) {
            let other: Option<AccountConfig> =
                read_typed(data, &oxpath!("config", "gate", "accounts", other_comp));
            if let Some(o) = other {
                if o.provider == acct.provider {
                    other_users += 1;
                }
            }
        }
    }
    if other_users == 0 {
        return Vec::new();
    }

    // Read the currently-bound provider record. If it's missing, fork
    // a default-shaped one — better to surface the renamed provider
    // with sensible defaults than to silently no-op.
    let existing_provider_comp = match ox_kernel::PathComponent::try_new(&acct.provider) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let provider: ProviderConfig = read_typed(
        data,
        &oxpath!("config", "gate", "providers", existing_provider_comp),
    )
    .unwrap_or_else(|| ProviderConfig {
        dialect: acct.provider.clone(),
        endpoint: String::new(),
        version: String::new(),
        auth: None,
    });

    // Forked name: "{account}_fork". `safe_component` lands at a
    // valid PathComponent. Two-account fork sequences would collide,
    // but that's rare enough we accept the simple case.
    let base = format!("{}_fork", selected);
    let forked_name = crate::settings::visible_rows::safe_component(&base);
    let forked_comp = match ox_kernel::PathComponent::try_new(&forked_name) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let forked_path = oxpath!("config", "gate", "providers", forked_comp);

    let provider_value = match to_value(&provider) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    acct.provider = forked_name;
    let acct_value = match to_value(&acct) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    vec![
        Write {
            path: forked_path,
            record: Record::parsed(provider_value),
        },
        Write {
            path: acct_path,
            record: Record::parsed(acct_value),
        },
    ]
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

pub fn register(reg: &mut CommandRegistry) {
    reg.register(Box::new(AccountsAdd::new()));
    reg.register(Box::new(AccountsComposeInsertChar::new()));
    reg.register(Box::new(AccountsComposeDeleteBack::new()));
    reg.register(Box::new(AccountsComposeCommit::new()));
    reg.register(Box::new(AccountsComposeCancel::new()));
    reg.register(Box::new(AccountsDeleteConfirm::new()));
    reg.register(Box::new(AccountsConfirmDelete::new()));
    reg.register(Box::new(AccountsConfirmCancel::new()));
    reg.register(Box::new(AccountTest::new()));
    reg.register(Box::new(AccountRefresh::new()));
    reg.register(Box::new(ModelsSetBootstrap::new()));
    reg.register(Box::new(ModelsToggleDefault::new()));
    reg.register(Box::new(AppSave::new()));
    reg.register(Box::new(FieldAccountNext::new()));
    reg.register(Box::new(SelectorCycleProtocol::new()));
    reg.register(Box::new(SelectorCycleAuth::new()));
    reg.register(Box::new(AccountsForkProvider::new()));
    reg.register(Box::new(CycleFieldNext::new()));
    reg.register(Box::new(CycleFieldPrev::new()));
    reg.register(Box::new(ModelsAddManual::new()));
    reg.register(Box::new(ModelsManualInsertChar::new()));
    reg.register(Box::new(ModelsManualDeleteBack::new()));
    reg.register(Box::new(ModelsManualCommit::new()));
    reg.register(Box::new(ModelsManualCancel::new()));
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use ox_types::key_chord::{KeyChord, KeyCodeRepr, KeyModifierSet};

    use crate::settings::command_registry::{Command, CommandCtx};
    use crate::settings::registry::RendererRegistry;
    use crate::settings::snapshot::SettingsSnapshot;

    fn run_cmd<C: Command>(cmd: &C, snap: &mut SettingsSnapshot) -> Vec<Write> {
        let registry = RendererRegistry::new();
        let ctx = CommandCtx {
            registry: &registry,
            last_keystroke: None,
        };
        cmd.run(snap, &ctx)
    }

    fn write_account(snap: &mut SettingsSnapshot, name: &str, provider: &str) {
        let comp = ox_kernel::PathComponent::try_new(name).unwrap();
        snap.insert(
            &oxpath!("config", "gate", "accounts", comp),
            to_value(&AccountConfig {
                provider: provider.into(),
            })
            .unwrap(),
        );
    }

    fn write_provider(snap: &mut SettingsSnapshot, name: &str, endpoint: &str, auth: AuthScheme) {
        let comp = ox_kernel::PathComponent::try_new(name).unwrap();
        snap.insert(
            &oxpath!("config", "gate", "providers", comp),
            to_value(&ProviderConfig {
                dialect: name.into(),
                endpoint: endpoint.into(),
                version: String::new(),
                auth: Some(auth),
            })
            .unwrap(),
        );
    }

    fn select_account(snap: &mut SettingsSnapshot, name: &str) {
        snap.insert(
            &oxpath!("ui", "settings", "accounts", "selected"),
            to_value(&Some(name.to_string())).unwrap(),
        );
    }

    fn select_model(snap: &mut SettingsSnapshot, account: &str, model_id: &str) {
        snap.insert(
            &oxpath!("ui", "settings", "models", "selected"),
            to_value(&Some(ModelKey {
                account: account.into(),
                model_id: model_id.into(),
            }))
            .unwrap(),
        );
    }

    fn assert_cursor_write(writes: &[Write], expected_target: structfs_core_store::Path) {
        assert!(writes.iter().any(|w| {
            w.path == oxpath!("ui", "settings", "cursor")
                && match &w.record {
                    Record::Parsed(v) => {
                        super::super::navigation::path_from_value(v)
                            == Some(expected_target.clone())
                    }
                    _ => false,
                }
        }));
    }

    fn assert_null_write(writes: &[Write], expected_path: structfs_core_store::Path) {
        let hit = writes
            .iter()
            .any(|w| w.path == expected_path && matches!(&w.record, Record::Parsed(Value::Null)));
        assert!(
            hit,
            "expected Null write at {expected_path}, got {writes:?}"
        );
    }

    // -- Cursor-shuffle tests ---------------------------------------------------

    #[test]
    fn accounts_add_writes_buffer_and_expands_section() {
        let mut snap = SettingsSnapshot::empty();
        let writes = run_cmd(&AccountsAdd::new(), &mut snap);
        let by_path: std::collections::BTreeMap<_, _> = writes
            .iter()
            .map(|w| (w.path.to_string(), w.record.clone()))
            .collect();

        // expanded set must contain settings/accounts.
        let exp = by_path.get("ui/settings/expanded").expect("expanded write");
        let set: Vec<String> = match exp {
            Record::Parsed(v) => structfs_serde_store::from_value(v.clone()).unwrap(),
            other => panic!("unexpected: {other:?}"),
        };
        assert!(
            set.iter().any(|s| s == "settings/accounts"),
            "expanded set must include settings/accounts; got {set:?}"
        );

        // new_account/buffer must be Some("").
        let buf = by_path
            .get("ui/settings/new_account/buffer")
            .expect("buffer write");
        match buf {
            Record::Parsed(Value::String(s)) => assert!(s.is_empty()),
            other => panic!("expected buffer = Some(\"\"); got {other:?}"),
        }

        // The new substrate doesn't touch edit-mode state nor focus —
        // those belong to the field-edit flow now.
        assert!(!by_path.contains_key("ui/settings/edit_mode"));
        assert!(!by_path.contains_key("ui/settings/edit_field_path"));
        assert!(!by_path.contains_key("ui/settings/edit_buffer"));
        assert!(!by_path.contains_key("ui/settings/focused"));
    }

    #[test]
    fn accounts_add_preserves_existing_expanded_entries() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            crate::settings::visible_rows::expanded_set_to_value(&["settings/models".to_string()]),
        );
        let writes = run_cmd(&AccountsAdd::new(), &mut snap);
        let exp = writes
            .iter()
            .find(|w| w.path == oxpath!("ui", "settings", "expanded"))
            .expect("expanded write");
        let set: Vec<String> = match &exp.record {
            Record::Parsed(v) => structfs_serde_store::from_value(v.clone()).unwrap(),
            other => panic!("unexpected: {other:?}"),
        };
        assert!(
            set.iter().any(|s| s == "settings/models"),
            "must not drop pre-existing entries; got {set:?}"
        );
        assert!(
            set.iter().any(|s| s == "settings/accounts"),
            "must add settings/accounts; got {set:?}"
        );
    }

    // -- Compose-mode tests -----------------------------------------------------

    #[test]
    fn accounts_compose_commit_writes_account_record_and_cascade() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "new_account", "buffer"),
            Value::String("alpha".into()),
        );
        let writes = run_cmd(&AccountsComposeCommit::new(), &mut snap);
        let by_path: std::collections::BTreeMap<_, _> = writes
            .iter()
            .map(|w| (w.path.to_string(), w.record.clone()))
            .collect();

        // 1. Account record materialized at config/gate/accounts/alpha.
        let acct = by_path
            .get("config/gate/accounts/alpha")
            .expect("account record write");
        let cfg: AccountConfig = match acct {
            Record::Parsed(v) => structfs_serde_store::from_value(v.clone()).unwrap(),
            other => panic!("unexpected record: {other:?}"),
        };
        assert_eq!(cfg.provider, "anthropic");

        // 2. Buffer cleared (Null write) — exits compose mode.
        let buf = by_path
            .get("ui/settings/new_account/buffer")
            .expect("buffer cleared");
        assert!(matches!(buf, Record::Parsed(Value::Null)));

        // 3. Selection / cursor / focused / expanded all written.
        assert!(by_path.contains_key("ui/settings/accounts/selected"));
        assert!(by_path.contains_key("ui/settings/cursor"));
        assert!(by_path.contains_key("ui/settings/focused"));
        assert!(by_path.contains_key("ui/settings/expanded"));

        // Expanded set contains both the section and the new account row.
        let exp = by_path.get("ui/settings/expanded").unwrap();
        let set: Vec<String> = match exp {
            Record::Parsed(v) => structfs_serde_store::from_value(v.clone()).unwrap(),
            other => panic!("unexpected: {other:?}"),
        };
        assert!(set.iter().any(|s| s == "settings/accounts"));
        assert!(set.iter().any(|s| s == "settings/accounts/alpha"));
    }

    #[test]
    fn accounts_compose_commit_with_empty_buffer_silent_no_op() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "new_account", "buffer"),
            Value::String("   ".into()),
        );
        let writes = run_cmd(&AccountsComposeCommit::new(), &mut snap);
        assert!(
            writes.is_empty(),
            "expected no writes for whitespace buffer; got {writes:?}"
        );
    }

    #[test]
    fn accounts_compose_commit_with_leading_underscore_writes_account_record() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "new_account", "buffer"),
            Value::String("_personal".into()),
        );
        let writes = run_cmd(&AccountsComposeCommit::new(), &mut snap);
        let by_path: std::collections::BTreeMap<_, _> = writes
            .iter()
            .map(|w| (w.path.to_string(), w.record.clone()))
            .collect();

        let acct = by_path
            .get("config/gate/accounts/_personal")
            .expect("account record write at canonical _personal path");
        let cfg: AccountConfig = match acct {
            Record::Parsed(v) => structfs_serde_store::from_value(v.clone()).unwrap(),
            other => panic!("unexpected: {other:?}"),
        };
        assert_eq!(cfg.provider, "anthropic");

        assert!(
            !by_path.contains_key("ui/global/banner"),
            "_-prefixed names must no longer emit a reservation banner"
        );
    }

    #[test]
    fn accounts_compose_commit_with_invalid_name_emits_banner() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "new_account", "buffer"),
            Value::String("bad-name".into()),
        );
        let writes = run_cmd(&AccountsComposeCommit::new(), &mut snap);
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "global", "banner"));
        let banner: ox_types::settings::GlobalBanner = match &writes[0].record {
            Record::Parsed(v) => structfs_serde_store::from_value(v.clone()).unwrap(),
            other => panic!("unexpected: {other:?}"),
        };
        match banner {
            ox_types::settings::GlobalBanner::Error { message, .. } => {
                assert!(
                    message.contains("Invalid"),
                    "banner must mention invalidity; got {message:?}"
                );
                assert!(
                    message.contains("bad-name"),
                    "banner must mention the offending name; got {message:?}"
                );
            }
            other => panic!("expected Error banner; got {other:?}"),
        }
    }

    #[test]
    fn accounts_compose_commit_with_interior_underscore_writes_account_record() {
        // Interior underscores like `alpha_beta` are valid identifiers
        // and commit normally.
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "new_account", "buffer"),
            Value::String("alpha_beta".into()),
        );
        let writes = run_cmd(&AccountsComposeCommit::new(), &mut snap);
        let by_path: std::collections::BTreeMap<_, _> = writes
            .iter()
            .map(|w| (w.path.to_string(), w.record.clone()))
            .collect();
        let acct = by_path
            .get("config/gate/accounts/alpha_beta")
            .expect("account record write");
        let cfg: AccountConfig = match acct {
            Record::Parsed(v) => structfs_serde_store::from_value(v.clone()).unwrap(),
            other => panic!("unexpected: {other:?}"),
        };
        assert_eq!(cfg.provider, "anthropic");
    }

    #[test]
    fn accounts_compose_insert_char_appends_to_buffer() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "new_account", "buffer"),
            Value::String("alph".into()),
        );
        // CommandCtx with last_keystroke = 'a'.
        let registry = RendererRegistry::new();
        let ctx = CommandCtx {
            registry: &registry,
            last_keystroke: Some(KeyChord {
                modifiers: KeyModifierSet::default(),
                code: KeyCodeRepr::Char('a'),
            }),
        };
        let writes = AccountsComposeInsertChar::new().run(&mut snap, &ctx);
        assert_eq!(writes.len(), 1);
        assert_eq!(
            writes[0].path,
            oxpath!("ui", "settings", "new_account", "buffer")
        );
        match &writes[0].record {
            Record::Parsed(Value::String(s)) => assert_eq!(s, "alpha"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn accounts_compose_delete_back_pops_buffer() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "new_account", "buffer"),
            Value::String("alpha".into()),
        );
        let writes = run_cmd(&AccountsComposeDeleteBack::new(), &mut snap);
        assert_eq!(writes.len(), 1);
        assert_eq!(
            writes[0].path,
            oxpath!("ui", "settings", "new_account", "buffer")
        );
        match &writes[0].record {
            Record::Parsed(Value::String(s)) => assert_eq!(s, "alph"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn accounts_compose_delete_back_on_empty_is_no_op() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "new_account", "buffer"),
            Value::String(String::new()),
        );
        let writes = run_cmd(&AccountsComposeDeleteBack::new(), &mut snap);
        assert!(writes.is_empty());
    }

    #[test]
    fn accounts_compose_cancel_clears_buffer() {
        let mut snap = SettingsSnapshot::empty();
        let writes = run_cmd(&AccountsComposeCancel::new(), &mut snap);
        assert_eq!(writes.len(), 1);
        assert_eq!(
            writes[0].path,
            oxpath!("ui", "settings", "new_account", "buffer")
        );
        assert!(matches!(&writes[0].record, Record::Parsed(Value::Null)));
    }

    #[test]
    fn accounts_delete_confirm_writes_pending_delete_when_selected() {
        let mut snap = SettingsSnapshot::empty();
        select_account(&mut snap, "alpha");
        let writes = run_cmd(&AccountsDeleteConfirm::new(), &mut snap);
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "settings", "pending_delete"));
        match &writes[0].record {
            Record::Parsed(Value::String(s)) => assert_eq!(s, "alpha"),
            other => panic!("expected pending_delete = Some(\"alpha\"); got {other:?}"),
        }
    }

    #[test]
    fn accounts_delete_confirm_inert_without_selection() {
        let mut snap = SettingsSnapshot::empty();
        let writes = run_cmd(&AccountsDeleteConfirm::new(), &mut snap);
        assert!(writes.is_empty());
    }

    // -- Subscription requests --------------------------------------------------

    #[test]
    fn fork_provider_clones_record_and_repoints_account() {
        let mut snap = SettingsSnapshot::empty();
        // Two accounts share provider "anthropic".
        write_account(&mut snap, "personal", "anthropic");
        write_account(&mut snap, "work", "anthropic");
        write_provider(
            &mut snap,
            "anthropic",
            "https://api.anthropic.com",
            AuthScheme::XApiKey,
        );
        select_account(&mut snap, "personal");
        let writes = run_cmd(&AccountsForkProvider::new(), &mut snap);

        // Expect: write a new provider "personal_fork" + repoint personal's account.
        let provider_write = writes
            .iter()
            .find(|w| w.path.to_string() == "config/gate/providers/personal_fork")
            .expect("forked provider write");
        let pc: ProviderConfig =
            structfs_serde_store::from_value(provider_write.record.as_value().unwrap().clone())
                .unwrap();
        assert_eq!(pc.endpoint, "https://api.anthropic.com");

        let account_write = writes
            .iter()
            .find(|w| w.path.to_string() == "config/gate/accounts/personal")
            .expect("account repoint");
        let ac: AccountConfig =
            structfs_serde_store::from_value(account_write.record.as_value().unwrap().clone())
                .unwrap();
        assert_eq!(ac.provider, "personal_fork");
    }

    #[test]
    fn fork_provider_no_op_when_provider_not_shared() {
        let mut snap = SettingsSnapshot::empty();
        write_account(&mut snap, "lone", "openai");
        write_provider(
            &mut snap,
            "openai",
            "https://api.openai.com",
            AuthScheme::BearerToken,
        );
        select_account(&mut snap, "lone");
        let writes = run_cmd(&AccountsForkProvider::new(), &mut snap);
        // No need to fork — the provider is already exclusive.
        assert!(writes.is_empty());
    }

    #[test]
    fn account_test_writes_test_now_when_selected() {
        let mut snap = SettingsSnapshot::empty();
        select_account(&mut snap, "alpha");
        let writes = run_cmd(&AccountTest::new(), &mut snap);
        let comp = ox_kernel::PathComponent::try_new("alpha").unwrap();
        assert_null_write(
            &writes,
            oxpath!("config", "gate", "accounts", comp, "test_now"),
        );
    }

    #[test]
    fn account_refresh_writes_refresh_now_using_selected_model_account() {
        let mut snap = SettingsSnapshot::empty();
        select_model(&mut snap, "alpha", "m1");
        let writes = run_cmd(&AccountRefresh::new(), &mut snap);
        let comp = ox_kernel::PathComponent::try_new("alpha").unwrap();
        assert_null_write(
            &writes,
            oxpath!("config", "gate", "accounts", comp, "refresh_now"),
        );
    }

    #[test]
    fn account_refresh_falls_back_to_focused_account_row() {
        // Empty-catalog accounts have no focusable Model rows so
        // `read_selected_model` returns None. account.refresh must
        // still find the target via the focused Account row — that's
        // what makes `r` reachable from the Connections section.
        let mut snap = SettingsSnapshot::empty();
        write_index_entries_for_manual(&mut snap);
        write_account(&mut snap, "alpha", "openai");
        let expanded = crate::settings::visible_rows::expanded_set_to_value(&[
            "settings/accounts".to_string(),
        ]);
        snap.insert(&oxpath!("ui", "settings", "expanded"), expanded);
        let comp = ox_kernel::PathComponent::try_new("alpha").unwrap();
        snap.insert(
            &oxpath!("ui", "settings", "focused"),
            path_to_value(&oxpath!("settings", "accounts", comp.clone())),
        );
        let writes = run_cmd(&AccountRefresh::new(), &mut snap);
        assert_null_write(
            &writes,
            oxpath!("config", "gate", "accounts", comp, "refresh_now"),
        );
    }

    #[test]
    fn models_set_bootstrap_writes_both_paths_for_migration() {
        let mut snap = SettingsSnapshot::empty();
        select_model(&mut snap, "alpha", "claude-sonnet-4");
        let writes = run_cmd(&ModelsSetBootstrap::new(), &mut snap);
        // Two writes during the migration window: new path AND legacy path.
        // Order doesn't matter.
        assert_eq!(writes.len(), 2);
        let paths: Vec<String> = writes.iter().map(|w| w.path.to_string()).collect();
        assert!(
            paths
                .iter()
                .any(|p| p == "config/gate/completions/bootstrap")
        );
        assert!(paths.iter().any(|p| p == "config/gate/completions/primary"));
        // Both must encode the same CompletionRole.
        for w in &writes {
            let role: CompletionRole =
                structfs_serde_store::from_value(w.record.as_value().unwrap().clone()).unwrap();
            assert_eq!(role.account, "alpha");
            assert_eq!(role.model_id, "claude-sonnet-4");
        }
    }

    #[test]
    fn app_save_writes_null_to_config_save() {
        let mut snap = SettingsSnapshot::empty();
        let writes = run_cmd(&AppSave::new(), &mut snap);
        assert_null_write(&writes, oxpath!("config", "save"));
    }

    // -- Field cycling ----------------------------------------------------------

    #[test]
    fn field_account_next_cycles_through_variants() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "account_detail", "field"),
            to_value(&AccountField::Name).unwrap(),
        );
        let writes = run_cmd(&FieldAccountNext::new(), &mut snap);
        match &writes[0].record {
            Record::Parsed(v) => {
                let f: AccountField = structfs_serde_store::from_value(v.clone()).unwrap();
                assert_eq!(f, AccountField::Protocol);
            }
            other => panic!("unexpected record: {other:?}"),
        }
    }

    #[test]
    fn field_account_prev_wraps_at_start() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "account_detail", "field"),
            to_value(&AccountField::Name).unwrap(),
        );
        let writes = run_cmd(&FieldAccountPrev::new(), &mut snap);
        match &writes[0].record {
            Record::Parsed(v) => {
                let f: AccountField = structfs_serde_store::from_value(v.clone()).unwrap();
                assert_eq!(f, AccountField::Key);
            }
            other => panic!("unexpected record: {other:?}"),
        }
    }

    #[test]
    fn field_model_next_cycles() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "model_detail", "field"),
            to_value(&ModelField::ContextSizeOverride).unwrap(),
        );
        let writes = run_cmd(&FieldModelNext::new(), &mut snap);
        match &writes[0].record {
            Record::Parsed(v) => {
                let f: ModelField = structfs_serde_store::from_value(v.clone()).unwrap();
                assert_eq!(f, ModelField::OutputTokensOverride);
            }
            other => panic!("unexpected record: {other:?}"),
        }
    }

    #[test]
    fn field_model_prev_wraps() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "model_detail", "field"),
            to_value(&ModelField::ContextSizeOverride).unwrap(),
        );
        let writes = run_cmd(&FieldModelPrev::new(), &mut snap);
        match &writes[0].record {
            Record::Parsed(v) => {
                let f: ModelField = structfs_serde_store::from_value(v.clone()).unwrap();
                assert_eq!(f, ModelField::OutputTokensOverride);
            }
            other => panic!("unexpected record: {other:?}"),
        }
    }

    // -- Field text editing -----------------------------------------------------

    fn keystroke_char(c: char) -> KeyChord {
        KeyChord {
            modifiers: KeyModifierSet::default(),
            code: KeyCodeRepr::Char(c),
        }
    }

    fn setup_endpoint_edit(snap: &mut SettingsSnapshot, endpoint: &str, cursor: u32) {
        write_account(snap, "alpha", "anthropic");
        write_provider(snap, "anthropic", endpoint, AuthScheme::XApiKey);
        select_account(snap, "alpha");
        snap.insert(
            &oxpath!("ui", "settings", "account_detail", "field"),
            to_value(&AccountField::Endpoint).unwrap(),
        );
        snap.insert(
            &oxpath!("ui", "settings", "edit_cursor"),
            Value::Integer(cursor as i64),
        );
    }

    // (text-edit tests retired with FieldInsert/FieldDeleteBack —
    // the inline edit-buffer model is covered by tests in
    // `settings::commands::edit::tests`.)

    // -- Selectors --------------------------------------------------------------

    #[test]
    fn selector_cycle_protocol_synthesizes_provider_for_toml_loaded() {
        // TOML-loaded accounts may have no AccountConfig leaf (only child
        // `…/{name}/provider`), AND no provider record. The cycle must
        // still advance: synthesize a default ProviderConfig from
        // acct.provider, advance its dialect, write the synthesized
        // record at config/gate/providers/{provider_name}.
        let mut snap = SettingsSnapshot::empty();
        let comp = ox_kernel::PathComponent::try_new("alpha").unwrap();
        snap.insert(
            &oxpath!("config", "gate", "accounts", comp, "provider"),
            Value::String("anthropic".into()),
        );
        snap.insert(
            &oxpath!("ui", "settings", "accounts", "selected"),
            to_value(&Some("alpha".to_string())).unwrap(),
        );
        let writes = run_cmd(&SelectorCycleProtocol::new(), &mut snap);
        assert_eq!(writes.len(), 1);
        let prov_comp = ox_kernel::PathComponent::try_new("anthropic").unwrap();
        assert_eq!(
            writes[0].path,
            oxpath!("config", "gate", "providers", prov_comp)
        );
        // Synthesized default had dialect="anthropic" (from acct.provider);
        // forward cycle through ["anthropic", "openai"] lands on "openai".
        match &writes[0].record {
            Record::Parsed(v) => {
                let pc: ProviderConfig = structfs_serde_store::from_value(v.clone()).unwrap();
                assert_eq!(pc.dialect, "openai");
            }
            other => panic!("unexpected record: {other:?}"),
        }
    }

    #[test]
    fn selector_cycle_protocol_writes_provider_record_dialect_not_account() {
        // Cycle Protocol mutates the bound provider record's dialect.
        // The account's provider reference (the record name) stays stable.
        let mut snap = SettingsSnapshot::empty();
        write_account(&mut snap, "alpha", "anthropic");
        write_provider(
            &mut snap,
            "anthropic",
            "https://api.anthropic.com",
            AuthScheme::XApiKey,
        );
        select_account(&mut snap, "alpha");
        let writes = run_cmd(&SelectorCycleProtocol::new(), &mut snap);
        // Provider record gets written, account does not.
        let prov_comp = ox_kernel::PathComponent::try_new("anthropic").unwrap();
        let prov_write = writes
            .iter()
            .find(|w| w.path == oxpath!("config", "gate", "providers", prov_comp))
            .expect("provider write");
        match &prov_write.record {
            Record::Parsed(v) => {
                let pc: ProviderConfig = structfs_serde_store::from_value(v.clone()).unwrap();
                // write_provider seeded dialect="anthropic"; cycle forward
                // through ["anthropic", "openai"] lands on "openai".
                assert_eq!(pc.dialect, "openai");
                // Endpoint and auth on the provider stay untouched.
                assert_eq!(pc.endpoint, "https://api.anthropic.com");
                assert_eq!(pc.auth, Some(AuthScheme::XApiKey));
            }
            other => panic!("unexpected record: {other:?}"),
        }
        let acct_comp = ox_kernel::PathComponent::try_new("alpha").unwrap();
        assert!(
            !writes
                .iter()
                .any(|w| w.path == oxpath!("config", "gate", "accounts", acct_comp)),
            "cycle must not write the account record",
        );
    }

    #[test]
    fn selector_cycle_auth_advances_through_variants() {
        let mut snap = SettingsSnapshot::empty();
        write_account(&mut snap, "alpha", "anthropic");
        write_provider(
            &mut snap,
            "anthropic",
            "https://api.anthropic.com",
            AuthScheme::XApiKey,
        );
        select_account(&mut snap, "alpha");
        let writes = run_cmd(&SelectorCycleAuth::new(), &mut snap);
        let comp = ox_kernel::PathComponent::try_new("anthropic").unwrap();
        let prov_write = writes
            .iter()
            .find(|w| w.path == oxpath!("config", "gate", "providers", comp))
            .expect("provider write");
        match &prov_write.record {
            Record::Parsed(v) => {
                let pc: ProviderConfig = structfs_serde_store::from_value(v.clone()).unwrap();
                assert_eq!(pc.auth, Some(AuthScheme::BearerToken));
            }
            other => panic!("unexpected record: {other:?}"),
        }
    }

    // -- resolve_protocol_options ----------------------------------------

    #[test]
    fn resolve_protocol_options_lists_presets_first() {
        let mut snap = SettingsSnapshot::empty();
        let opts = resolve_protocol_options(&mut snap, "anthropic");
        assert_eq!(opts, vec!["anthropic".to_string(), "openai".to_string()]);
    }

    #[test]
    fn resolve_protocol_options_does_not_enumerate_user_provider_names() {
        // Provider record names (e.g. LMStudio, lm_studio, corp-gateway)
        // are NOT carousel options — they're identifiers for endpoint
        // bundles, not dialects. Multiple records can speak the same
        // dialect (LMStudio at openai, lm_studio at openai); the
        // carousel still shows just `openai` once.
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("config", "gate", "providers", "LMStudio", "dialect"),
            Value::String("openai".into()),
        );
        snap.insert(
            &oxpath!("config", "gate", "providers", "lm_studio", "dialect"),
            Value::String("openai".into()),
        );
        let opts = resolve_protocol_options(&mut snap, "openai");
        assert_eq!(opts, vec!["anthropic".to_string(), "openai".to_string()]);
    }

    #[test]
    fn resolve_protocol_options_appends_current_dialect_when_unknown() {
        // A connection currently bound to a dialect we don't ship a
        // preset for (e.g. an experimental dialect named "groq") must
        // still appear in the carousel — otherwise cycling silently
        // overwrites it via the idx-0 fallback. Provider record names
        // never get this treatment; only the actual current dialect
        // string the bound provider's `dialect` field holds.
        let mut snap = SettingsSnapshot::empty();
        let opts = resolve_protocol_options(&mut snap, "groq");
        assert_eq!(
            opts,
            vec![
                "anthropic".to_string(),
                "openai".to_string(),
                "groq".to_string()
            ]
        );
    }

    #[test]
    fn resolve_protocol_options_does_not_append_empty_current() {
        let mut snap = SettingsSnapshot::empty();
        let opts = resolve_protocol_options(&mut snap, "");
        assert_eq!(opts, vec!["anthropic".to_string(), "openai".to_string()]);
    }

    // -- selector_cycle_protocol_dir, post-dynamic-options ---------------

    #[test]
    fn cycle_protocol_forward_from_unknown_dialect_visits_current_then_wraps() {
        // A connection's bound provider has dialect="groq" (not in our
        // preset table). resolve_protocol_options appends it; cycling
        // forward through ["anthropic", "openai", "groq"] from idx 2
        // wraps to idx 0 = "anthropic". Critically, we mutate the
        // provider record's dialect — not the account's provider field.
        let mut snap = SettingsSnapshot::empty();
        write_account(&mut snap, "local", "experimental");
        // Provider record `experimental` with a non-preset dialect.
        let prov_comp = ox_kernel::PathComponent::try_new("experimental").unwrap();
        snap.insert(
            &oxpath!("config", "gate", "providers", prov_comp.clone()),
            to_value(&ProviderConfig {
                dialect: "groq".into(),
                endpoint: "https://api.groq.example".into(),
                version: String::new(),
                auth: None,
            })
            .unwrap(),
        );
        select_account(&mut snap, "local");

        let writes = selector_cycle_protocol_dir(&mut snap, CycleDir::Forward);
        assert_eq!(writes.len(), 1);
        assert_eq!(
            writes[0].path,
            oxpath!("config", "gate", "providers", prov_comp)
        );
        let written: ProviderConfig =
            structfs_serde_store::from_value(writes[0].record.as_value().unwrap().clone()).unwrap();
        assert_eq!(written.dialect, "anthropic");
        // Endpoint/auth survive — only dialect changes.
        assert_eq!(written.endpoint, "https://api.groq.example");
    }

    #[test]
    fn cycle_protocol_back_from_unknown_dialect_lands_on_previous_option() {
        // Same setup as the forward test; back from idx 2 = "openai".
        let mut snap = SettingsSnapshot::empty();
        write_account(&mut snap, "local", "experimental");
        let prov_comp = ox_kernel::PathComponent::try_new("experimental").unwrap();
        snap.insert(
            &oxpath!("config", "gate", "providers", prov_comp),
            to_value(&ProviderConfig {
                dialect: "groq".into(),
                endpoint: "https://api.groq.example".into(),
                version: String::new(),
                auth: None,
            })
            .unwrap(),
        );
        select_account(&mut snap, "local");

        let writes = selector_cycle_protocol_dir(&mut snap, CycleDir::Back);
        assert_eq!(writes.len(), 1);
        let written: ProviderConfig =
            structfs_serde_store::from_value(writes[0].record.as_value().unwrap().clone()).unwrap();
        assert_eq!(written.dialect, "openai");
    }

    #[test]
    fn cycle_protocol_repro_user_toml_flat_keys_two_cycles_advance() {
        // Realistic TOML shape: provider record loaded via TomlFileBacking
        // arrives as FLAT sub-keys (gate/providers/LMStudio/dialect = "openai",
        // /endpoint, /version, /auth) — there is no parent `Value::Map` at
        // gate/providers/LMStudio. The ConfigStore's runtime layer adds
        // the parent Map on first cycle write, and subsequent cycles must
        // read it and advance honestly.
        //
        // Both cycles must advance — the second exercises the
        // runtime-Map-overrides-base-flat-keys read path, which is
        // structurally distinct from the synthesizing first cycle.
        let mut snap = SettingsSnapshot::empty();
        // Account: flat-key (no parent Map) — matches TomlFileBacking output.
        let acct_comp = ox_kernel::PathComponent::try_new("LMStudio").unwrap();
        snap.insert(
            &oxpath!("config", "gate", "accounts", acct_comp.clone(), "provider"),
            Value::String("LMStudio".into()),
        );
        // Provider: flat-key (no parent Map) — matches TomlFileBacking output.
        snap.insert(
            &oxpath!("config", "gate", "providers", acct_comp.clone(), "dialect"),
            Value::String("openai".into()),
        );
        snap.insert(
            &oxpath!("config", "gate", "providers", acct_comp.clone(), "endpoint"),
            Value::String("http://127.0.0.1:1234".into()),
        );
        snap.insert(
            &oxpath!("config", "gate", "providers", acct_comp.clone(), "auth"),
            Value::String("none".into()),
        );
        snap.insert(
            &oxpath!("config", "gate", "providers", acct_comp, "version"),
            Value::String(String::new()),
        );
        select_account(&mut snap, "LMStudio");

        // First cycle — read_typed::<ProviderConfig> on the parent path
        // returns None (no parent Map), so the cycle synthesizes a default
        // with dialect=acct.provider="LMStudio". options=[anthropic, openai,
        // LMStudio]; idx=2; forward → idx 0 = "anthropic".
        let writes_1 = selector_cycle_protocol_dir(&mut snap, CycleDir::Forward);
        assert_eq!(writes_1.len(), 1, "first cycle must produce one write");
        let prov_comp = ox_kernel::PathComponent::try_new("LMStudio").unwrap();
        assert_eq!(
            writes_1[0].path,
            oxpath!("config", "gate", "providers", prov_comp.clone())
        );
        let pc1: ProviderConfig =
            structfs_serde_store::from_value(writes_1[0].record.as_value().unwrap().clone())
                .unwrap();
        assert_eq!(
            pc1.dialect, "anthropic",
            "first cycle must advance dialect from synthesized 'LMStudio' to 'anthropic'"
        );

        // Apply the write into the snapshot the same way the broker would,
        // then cycle again.
        snap.insert(
            &oxpath!("config", "gate", "providers", prov_comp.clone()),
            writes_1[0].record.as_value().unwrap().clone(),
        );

        // Second cycle — now read_typed::<ProviderConfig> finds the parent
        // Map (from the inserted runtime override). dialect is "anthropic".
        // options=[anthropic, openai]; idx=0; forward → idx 1 = "openai".
        let writes_2 = selector_cycle_protocol_dir(&mut snap, CycleDir::Forward);
        assert_eq!(writes_2.len(), 1, "second cycle must produce one write");
        let pc2: ProviderConfig =
            structfs_serde_store::from_value(writes_2[0].record.as_value().unwrap().clone())
                .unwrap();
        assert_eq!(
            pc2.dialect, "openai",
            "second cycle must advance dialect from 'anthropic' to 'openai'"
        );
    }

    #[test]
    fn cycle_protocol_does_not_treat_provider_record_names_as_options() {
        // Two provider records, both with dialect="openai", with
        // distinguishing names (LMStudio, lm_studio). A connection bound
        // to LMStudio (dialect=openai) cycles forward — the result must
        // be "anthropic" (next dialect after wrapping from idx 1 in
        // [anthropic, openai]), not "lm_studio" (which Slice 1's old
        // semantics would have produced by treating record names as
        // options).
        let mut snap = SettingsSnapshot::empty();
        write_account(&mut snap, "local", "LMStudio");
        let lms_comp = ox_kernel::PathComponent::try_new("LMStudio").unwrap();
        snap.insert(
            &oxpath!("config", "gate", "providers", lms_comp.clone()),
            to_value(&ProviderConfig {
                dialect: "openai".into(),
                endpoint: "http://127.0.0.1:1234".into(),
                version: String::new(),
                auth: Some(AuthScheme::None),
            })
            .unwrap(),
        );
        let other_comp = ox_kernel::PathComponent::try_new("lm_studio").unwrap();
        snap.insert(
            &oxpath!("config", "gate", "providers", other_comp),
            to_value(&ProviderConfig {
                dialect: "openai".into(),
                endpoint: "http://127.0.0.1:1234".into(),
                version: String::new(),
                auth: Some(AuthScheme::None),
            })
            .unwrap(),
        );
        select_account(&mut snap, "local");

        let writes = selector_cycle_protocol_dir(&mut snap, CycleDir::Forward);
        assert_eq!(writes.len(), 1);
        // Mutation lands on LMStudio (the bound record), not on lm_studio.
        assert_eq!(
            writes[0].path,
            oxpath!("config", "gate", "providers", lms_comp)
        );
        let written: ProviderConfig =
            structfs_serde_store::from_value(writes[0].record.as_value().unwrap().clone()).unwrap();
        assert_eq!(written.dialect, "anthropic");
    }

    // -- models.toggle_default --------------------------------------------

    #[test]
    fn toggle_default_adds_to_empty_set() {
        let mut snap = SettingsSnapshot::empty();
        select_model(&mut snap, "alpha", "claude-sonnet-4");
        let writes = run_cmd(&ModelsToggleDefault::new(), &mut snap);
        assert_eq!(writes.len(), 1);
        assert_eq!(
            writes[0].path,
            oxpath!("config", "gate", "completions", "default_available")
        );
        let set: Vec<ModelKey> =
            structfs_serde_store::from_value(writes[0].record.as_value().unwrap().clone()).unwrap();
        assert_eq!(set.len(), 1);
        assert_eq!(set[0].account, "alpha");
        assert_eq!(set[0].model_id, "claude-sonnet-4");
    }

    #[test]
    fn toggle_default_removes_from_set_when_already_present() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("config", "gate", "completions", "default_available"),
            to_value(&vec![ModelKey {
                account: "alpha".into(),
                model_id: "claude-sonnet-4".into(),
            }])
            .unwrap(),
        );
        select_model(&mut snap, "alpha", "claude-sonnet-4");
        let writes = run_cmd(&ModelsToggleDefault::new(), &mut snap);
        assert_eq!(writes.len(), 1);
        // Removing the last entry deletes the record (back to implicit
        // "all cataloged models default-available").
        match &writes[0].record {
            Record::Parsed(Value::Null) => {}
            other => panic!("expected null delete, got {other:?}"),
        }
    }

    #[test]
    fn toggle_default_removes_one_keeps_rest() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("config", "gate", "completions", "default_available"),
            to_value(&vec![
                ModelKey {
                    account: "alpha".into(),
                    model_id: "m1".into(),
                },
                ModelKey {
                    account: "alpha".into(),
                    model_id: "m2".into(),
                },
            ])
            .unwrap(),
        );
        select_model(&mut snap, "alpha", "m1");
        let writes = run_cmd(&ModelsToggleDefault::new(), &mut snap);
        assert_eq!(writes.len(), 1);
        let set: Vec<ModelKey> =
            structfs_serde_store::from_value(writes[0].record.as_value().unwrap().clone()).unwrap();
        assert_eq!(set.len(), 1);
        assert_eq!(set[0].model_id, "m2");
    }

    #[test]
    fn toggle_default_no_op_with_no_selected_model() {
        let mut snap = SettingsSnapshot::empty();
        let writes = run_cmd(&ModelsToggleDefault::new(), &mut snap);
        assert!(writes.is_empty());
    }

    // -- Manual-model mode tests ----------------------------------------------

    use ox_types::settings::ManualModelStage;

    fn write_index_entries_for_manual(snap: &mut SettingsSnapshot) {
        use ox_types::{BadgeSource, SettingsIndexEntry};
        use structfs_core_store::Path;
        snap.insert(
            &oxpath!("settings", "index", "entries", "accounts"),
            to_value(&SettingsIndexEntry {
                id: "accounts".into(),
                label: "Accounts".into(),
                description: String::new(),
                target_cursor: Path::parse("settings/accounts").unwrap(),
                badge: BadgeSource::None,
            })
            .unwrap(),
        );
        snap.insert(
            &oxpath!("settings", "index", "entries", "models"),
            to_value(&SettingsIndexEntry {
                id: "models".into(),
                label: "Models".into(),
                description: String::new(),
                target_cursor: Path::parse("settings/models").unwrap(),
                badge: BadgeSource::None,
            })
            .unwrap(),
        );
    }

    fn seed_manual_stage(snap: &mut SettingsSnapshot, stage: ManualModelStage) {
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "stage"),
            to_value(&stage).unwrap(),
        );
    }

    fn run_with_chord<C: Command>(
        cmd: &C,
        snap: &mut SettingsSnapshot,
        ch: char,
    ) -> Vec<Write> {
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

    #[test]
    fn models_add_manual_seeds_form_from_focused_model_row() {
        // Focus a Model row in the expanded Models section; add_manual
        // reads the row's account and seeds the form at stage Id with
        // an empty buffer. Empty-catalog connections no longer
        // contribute focusable rows — the user reaches manual-model
        // entry from any focused Model row in the section.
        use ox_gate::ModelInfo;
        use ox_types::ModelInfoSource;
        let mut snap = SettingsSnapshot::empty();
        write_index_entries_for_manual(&mut snap);
        let comp = ox_kernel::PathComponent::try_new("alpha").unwrap();
        snap.insert(
            &oxpath!("config", "gate", "accounts", comp.clone()),
            to_value(&AccountConfig {
                provider: "openai".into(),
            })
            .unwrap(),
        );
        snap.insert(
            &oxpath!("config", "gate", "accounts", comp.clone(), "models"),
            to_value(&vec![ModelInfo {
                id: "m1".into(),
                display_name: "M1".into(),
                max_context_size: None,
                max_output_tokens: None,
                source: ModelInfoSource::Server,
            }])
            .unwrap(),
        );
        let expanded = crate::settings::visible_rows::expanded_set_to_value(&[
            "settings/models".to_string(),
        ]);
        snap.insert(&oxpath!("ui", "settings", "expanded"), expanded);
        let m_comp = ox_kernel::PathComponent::try_new("m1").unwrap();
        snap.insert(
            &oxpath!("ui", "settings", "focused"),
            path_to_value(&oxpath!("settings", "models", comp, m_comp)),
        );

        let writes = run_cmd(&ModelsAddManual::new(), &mut snap);
        let by_path: std::collections::BTreeMap<_, _> = writes
            .iter()
            .map(|w| (w.path.to_string(), w.record.as_value().unwrap().clone()))
            .collect();
        assert_eq!(
            by_path.get("ui/settings/manual_model/account").unwrap(),
            &Value::String("alpha".into())
        );
        assert_eq!(
            by_path.get("ui/settings/manual_model/stage").unwrap(),
            &to_value(&ManualModelStage::Id).unwrap()
        );
        assert_eq!(
            by_path.get("ui/settings/manual_model/buffer").unwrap(),
            &Value::String(String::new())
        );
    }

    #[test]
    fn models_add_manual_no_op_without_resolvable_focused_account() {
        // No focused row → no account to compose for; produce no writes.
        let mut snap = SettingsSnapshot::empty();
        let writes = run_cmd(&ModelsAddManual::new(), &mut snap);
        assert!(writes.is_empty());
    }

    #[test]
    fn models_manual_commit_id_advances_to_ctx_with_staged_id() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "account"),
            Value::String("alpha".into()),
        );
        seed_manual_stage(&mut snap, ManualModelStage::Id);
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "buffer"),
            Value::String("custom-model".into()),
        );
        let writes = run_cmd(&ModelsManualCommit::new(), &mut snap);
        let by_path: std::collections::BTreeMap<_, _> = writes
            .iter()
            .map(|w| (w.path.to_string(), w.record.as_value().unwrap().clone()))
            .collect();
        assert_eq!(
            by_path.get("ui/settings/manual_model/stage").unwrap(),
            &to_value(&ManualModelStage::Ctx).unwrap()
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
    fn models_manual_commit_id_rejects_empty_buffer() {
        let mut snap = SettingsSnapshot::empty();
        seed_manual_stage(&mut snap, ManualModelStage::Id);
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "buffer"),
            Value::String("   ".into()),
        );
        let writes = run_cmd(&ModelsManualCommit::new(), &mut snap);
        assert!(writes.is_empty());
    }

    #[test]
    fn models_manual_commit_ctx_advances_to_out_with_parsed_u32() {
        let mut snap = SettingsSnapshot::empty();
        seed_manual_stage(&mut snap, ManualModelStage::Ctx);
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "buffer"),
            Value::String("200000".into()),
        );
        let writes = run_cmd(&ModelsManualCommit::new(), &mut snap);
        let by_path: std::collections::BTreeMap<_, _> = writes
            .iter()
            .map(|w| (w.path.to_string(), w.record.as_value().unwrap().clone()))
            .collect();
        assert_eq!(
            by_path.get("ui/settings/manual_model/stage").unwrap(),
            &to_value(&ManualModelStage::Out).unwrap()
        );
        assert_eq!(
            by_path.get("ui/settings/manual_model/staged_ctx").unwrap(),
            &Value::String("200000".into())
        );
    }

    #[test]
    fn models_manual_commit_ctx_rejects_non_numeric() {
        let mut snap = SettingsSnapshot::empty();
        seed_manual_stage(&mut snap, ManualModelStage::Ctx);
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "buffer"),
            Value::String("not-a-number".into()),
        );
        let writes = run_cmd(&ModelsManualCommit::new(), &mut snap);
        assert!(writes.is_empty());
    }

    #[test]
    fn models_manual_commit_out_writes_full_modelinfo_and_clears_form() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "account"),
            Value::String("alpha".into()),
        );
        seed_manual_stage(&mut snap, ManualModelStage::Out);
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
        let writes = run_cmd(&ModelsManualCommit::new(), &mut snap);
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
        // All form-state paths nulled.
        for sub in ["account", "stage", "buffer", "staged_id", "staged_ctx"] {
            let comp = ox_kernel::PathComponent::try_new(sub).unwrap();
            assert_null_write(&writes, oxpath!("ui", "settings", "manual_model", comp));
        }
    }

    #[test]
    fn models_manual_cancel_clears_all_form_state() {
        let mut snap = SettingsSnapshot::empty();
        seed_manual_stage(&mut snap, ManualModelStage::Ctx);
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "staged_id"),
            Value::String("custom".into()),
        );
        let writes = run_cmd(&ModelsManualCancel::new(), &mut snap);
        // No catalog write; every manual_model sub-path nulled.
        assert!(
            !writes
                .iter()
                .any(|w| w.path.to_string().starts_with("config/gate/accounts"))
        );
        for sub in ["account", "stage", "buffer", "staged_id", "staged_ctx"] {
            let comp = ox_kernel::PathComponent::try_new(sub).unwrap();
            assert_null_write(&writes, oxpath!("ui", "settings", "manual_model", comp));
        }
    }

    #[test]
    fn models_manual_insert_char_id_stage_accepts_any_printable() {
        let mut snap = SettingsSnapshot::empty();
        seed_manual_stage(&mut snap, ManualModelStage::Id);
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "buffer"),
            Value::String("foo".into()),
        );
        let writes = run_with_chord(&ModelsManualInsertChar::new(), &mut snap, '-');
        assert_eq!(writes.len(), 1);
        assert_eq!(
            writes[0].path,
            oxpath!("ui", "settings", "manual_model", "buffer")
        );
        match &writes[0].record {
            Record::Parsed(Value::String(s)) => assert_eq!(s, "foo-"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn models_manual_insert_char_ctx_stage_accepts_digits_only() {
        let mut snap = SettingsSnapshot::empty();
        seed_manual_stage(&mut snap, ManualModelStage::Ctx);
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "buffer"),
            Value::String("100".into()),
        );
        // Digit accepted.
        let writes = run_with_chord(&ModelsManualInsertChar::new(), &mut snap, '5');
        assert_eq!(writes.len(), 1);
        match &writes[0].record {
            Record::Parsed(Value::String(s)) => assert_eq!(s, "1005"),
            other => panic!("unexpected: {other:?}"),
        }
        // Letter rejected.
        let writes = run_with_chord(&ModelsManualInsertChar::new(), &mut snap, 'x');
        assert!(writes.is_empty());
    }

    #[test]
    fn models_manual_insert_char_no_op_without_typed_stage() {
        // Without a typed-shape stage the insert-char must be a no-op —
        // the dispatcher's gating already prevents it from firing, but
        // the helper itself also short-circuits as a defense in depth.
        let mut snap = SettingsSnapshot::empty();
        let writes = run_with_chord(&ModelsManualInsertChar::new(), &mut snap, 'a');
        assert!(writes.is_empty());
    }

    #[test]
    fn models_manual_delete_back_pops_buffer() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "buffer"),
            Value::String("abc".into()),
        );
        let writes = run_cmd(&ModelsManualDeleteBack::new(), &mut snap);
        assert_eq!(writes.len(), 1);
        match &writes[0].record {
            Record::Parsed(Value::String(s)) => assert_eq!(s, "ab"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn models_manual_delete_back_on_empty_is_no_op() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "buffer"),
            Value::String(String::new()),
        );
        let writes = run_cmd(&ModelsManualDeleteBack::new(), &mut snap);
        assert!(writes.is_empty());
    }
}
