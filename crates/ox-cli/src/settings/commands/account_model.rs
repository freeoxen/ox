//! Account/model/field commands — the bulk of day-one settings actions.
//!
//! These commands cover: cursor moves to overlay pages, account create /
//! delete (via subscription requests), test/refresh triggers, primary-model
//! binding, an app-save trigger, field focus cycling, in-place text editing,
//! and selector cycling for Protocol / Auth.
//!
//! All commands are pure (`run` returns `Vec<Write>`); subscriptions in
//! Phase N pick up the request paths (`…/_create_now`, `…/delete_now`,
//! `…/test_now`, `…/refresh_now`, `config/save`) and do the I/O.

use ox_path::oxpath;
use ox_types::Screen;
use ox_types::settings::{AccountField, CreateAccountRequest, ModelField, ModelKey};
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
    description: "Open the new-connection overlay.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "accounts")),
    run: |_snap, _ctx| vec![Write {
        path: oxpath!("ui", "settings", "cursor"),
        record: Record::parsed(path_to_value(&oxpath!("settings", "accounts", "_new"))),
    }],
}

command! {
    struct_name: AccountsDeleteConfirm,
    id: "accounts.delete_confirm",
    title: "Delete Connection…",
    description: "Open the delete-confirmation overlay.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "accounts")),
    run: |_snap, _ctx| vec![Write {
        path: oxpath!("ui", "settings", "cursor"),
        record: Record::parsed(path_to_value(&oxpath!("settings", "accounts", "_delete"))),
    }],
}

command! {
    struct_name: AccountsCancel,
    id: "accounts.cancel",
    title: "Cancel",
    description: "Dismiss the current overlay; return to the accordion.",
    screen: Screen::Settings,
    cursor: None,
    run: |_snap, _ctx| vec![Write {
        path: oxpath!("ui", "settings", "cursor"),
        record: Record::parsed(path_to_value(&oxpath!("settings", "index"))),
    }],
}

// ---------------------------------------------------------------------------
// Subscription-request commands
// ---------------------------------------------------------------------------

command! {
    struct_name: AccountsCreate,
    id: "accounts.create",
    title: "Create Connection",
    description: "Submit the new-connection name; the subscription does the rest.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "accounts", "_new")),
    run: |snap, _ctx| accounts_create(snap),
}

command! {
    struct_name: AccountsDelete,
    id: "accounts.delete",
    title: "Confirm Delete",
    description: "Submit the delete request for the selected Connection.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "accounts", "_delete")),
    run: |snap, _ctx| accounts_delete(snap),
}

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

// ---------------------------------------------------------------------------
// Implementation helpers
// ---------------------------------------------------------------------------

/// Resolve the active account name. The accordion's per-row `Prefix`
/// bindings fire while the focused row sits anywhere under
/// `settings/accounts`, so we honor the focused row first; the
/// legacy `_detail` page (still used by editing flows) writes its
/// selection to `ui/settings/accounts/selected` and we fall back to
/// that if no focus is set.
fn read_selected_account(data: &mut dyn Reader) -> Option<String> {
    if let Some(name) = focused_account(data) {
        return Some(name);
    }
    read_typed::<Option<String>>(data, &oxpath!("ui", "settings", "accounts", "selected")).flatten()
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
/// enumeration to find the row whose path matches `focused_row`
/// recovers the original.
fn focused_account(data: &mut dyn Reader) -> Option<String> {
    use crate::settings::visible_rows::{self, RowKind};
    let path = focused_path(data)?;
    let rows = visible_rows::enumerate(data);
    rows.into_iter().find_map(|r| {
        if r.path != path {
            return None;
        }
        match r.kind {
            RowKind::Account { name } => Some(name),
            // Field rows under an expanded account also count — the
            // user has focused a field that belongs to that account.
            RowKind::AccountField { account, .. } => Some(account),
            _ => None,
        }
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
            _ => None,
        }
    })
}

fn focused_path(data: &mut dyn Reader) -> Option<Path> {
    let r = data
        .read(&oxpath!("ui", "settings", "focused_row"))
        .ok()
        .flatten()?;
    super::navigation::path_from_value(r.as_value()?)
}

fn account_request_path(name: &str, suffix: &str) -> Option<Path> {
    let acct = ox_kernel::PathComponent::try_new(name).ok()?;
    let suf = ox_kernel::PathComponent::try_new(suffix).ok()?;
    Some(oxpath!("config", "gate", "accounts", acct, suf))
}

fn null_write(path: Path) -> Write {
    Write {
        path,
        record: Record::parsed(Value::Null),
    }
}

fn accounts_create(data: &mut dyn Reader) -> Vec<Write> {
    let name: String = match read_typed(
        data,
        &oxpath!("ui", "settings", "new_account", "name_input"),
    ) {
        Some(s) => s,
        None => return Vec::new(),
    };
    if name.is_empty() {
        return Vec::new();
    }
    let value = match to_value(&CreateAccountRequest { name }) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "accounts.create: failed to encode request");
            return Vec::new();
        }
    };
    vec![Write {
        path: oxpath!("config", "gate", "accounts", "_create_now"),
        record: Record::parsed(value),
    }]
}

fn accounts_delete(data: &mut dyn Reader) -> Vec<Write> {
    let name = match read_selected_account(data) {
        Some(n) => n,
        None => return Vec::new(),
    };
    match account_request_path(&name, "delete_now") {
        Some(p) => vec![null_write(p)],
        None => Vec::new(),
    }
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
    let key = match read_selected_model(data) {
        Some(k) => k,
        None => return Vec::new(),
    };
    match account_request_path(&key.account, "refresh_now") {
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
/// Built-in presets first (declaration order), then user-configured
/// providers (lexicographic), then the current value if it isn't already
/// in either set. The current-value tail guarantees that cycling from a
/// custom-provider account visits every option without silently snapping
/// the account to a preset value it never had.
pub fn resolve_protocol_options(data: &mut dyn Reader, current: &str) -> Vec<String> {
    use crate::settings::renderers::util::child_names_under;

    let mut options: Vec<String> = ox_gate::presets()
        .iter()
        .filter(|p| !p.custom)
        .map(|p| p.id.to_string())
        .collect();

    let mut user = child_names_under(data, "config/gate/providers");
    user.sort();
    user.retain(|n| !options.contains(n));
    options.append(&mut user);

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
        None => return Vec::new(),
    };
    let name_comp = match ox_kernel::PathComponent::try_new(&selected) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let acct_path = oxpath!("config", "gate", "accounts", name_comp);
    // TOML-loaded accounts may not have a parent `AccountConfig` leaf
    // — only child fields. Synthesize one (using a child `provider`
    // string if present) so the cycle can advance and the first
    // cycle write creates the leaf.
    let mut acct: AccountConfig = read_typed(data, &acct_path).unwrap_or_else(|| AccountConfig {
        provider: read_account_child_string(data, &selected, "provider")
            .unwrap_or_else(|| "anthropic".to_string()),
    });

    // Resolve options *for the current value*: the helper guarantees the
    // current provider appears in the list, so position_of can never
    // silently fall through to idx 0 and overwrite a custom provider.
    let options = resolve_protocol_options(data, &acct.provider);
    if options.is_empty() {
        return Vec::new();
    }
    let idx = options
        .iter()
        .position(|o| o == &acct.provider)
        .unwrap_or(0);
    let next = match dir {
        CycleDir::Forward => options[(idx + 1) % options.len()].clone(),
        CycleDir::Back => options[(idx + options.len() - 1) % options.len()].clone(),
    };
    acct.provider = next;

    let value = match to_value(&acct) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "selector.cycle.protocol: failed to encode AccountConfig");
            return Vec::new();
        }
    };
    vec![Write {
        path: acct_path,
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
        _ => Vec::new(),
    }
}

/// Read a child string under `config/gate/accounts/{name}/{child}`
/// — the shape TOML-loaded accounts produce when there's no
/// AccountConfig leaf at the parent.
fn read_account_child_string(data: &mut dyn Reader, account: &str, child: &str) -> Option<String> {
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
    let name_comp = match ox_kernel::PathComponent::try_new(&selected) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
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
    let acct_comp = match ox_kernel::PathComponent::try_new(&selected) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
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
        if n == &selected {
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
    reg.register(Box::new(AccountsDeleteConfirm::new()));
    reg.register(Box::new(AccountsCancel::new()));
    reg.register(Box::new(AccountsCreate::new()));
    reg.register(Box::new(AccountsDelete::new()));
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

    fn run_cmd_with_keystroke<C: Command>(
        cmd: &C,
        snap: &mut SettingsSnapshot,
        chord: Option<KeyChord>,
    ) -> Vec<Write> {
        let registry = RendererRegistry::new();
        let ctx = CommandCtx {
            registry: &registry,
            last_keystroke: chord,
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
    fn accounts_add_writes_new_cursor() {
        let mut snap = SettingsSnapshot::empty();
        let writes = run_cmd(&AccountsAdd::new(), &mut snap);
        assert_eq!(writes.len(), 1);
        assert_cursor_write(&writes, oxpath!("settings", "accounts", "_new"));
    }

    #[test]
    fn accounts_delete_confirm_writes_delete_cursor() {
        let mut snap = SettingsSnapshot::empty();
        let writes = run_cmd(&AccountsDeleteConfirm::new(), &mut snap);
        assert_eq!(writes.len(), 1);
        assert_cursor_write(&writes, oxpath!("settings", "accounts", "_delete"));
    }

    #[test]
    fn accounts_cancel_returns_to_accordion_index() {
        // Cancel from the new- or delete-account overlay returns to
        // the accordion. The legacy `settings/accounts` list page is
        // gone, so the cursor must land on `settings/index` instead.
        let mut snap = SettingsSnapshot::empty();
        let writes = run_cmd(&AccountsCancel::new(), &mut snap);
        assert_eq!(writes.len(), 1);
        assert_cursor_write(&writes, oxpath!("settings", "index"));
    }

    // -- Subscription requests --------------------------------------------------

    #[test]
    fn accounts_create_writes_request_when_name_present() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "new_account", "name_input"),
            Value::String("alpha".into()),
        );
        let writes = run_cmd(&AccountsCreate::new(), &mut snap);
        assert_eq!(writes.len(), 1);
        assert_eq!(
            writes[0].path,
            oxpath!("config", "gate", "accounts", "_create_now")
        );
        match &writes[0].record {
            Record::Parsed(v) => {
                let req: CreateAccountRequest =
                    structfs_serde_store::from_value(v.clone()).unwrap();
                assert_eq!(req.name, "alpha");
            }
            other => panic!("unexpected record: {other:?}"),
        }
    }

    #[test]
    fn accounts_create_inert_when_name_empty_or_missing() {
        // Missing.
        let mut snap = SettingsSnapshot::empty();
        let writes = run_cmd(&AccountsCreate::new(), &mut snap);
        assert!(writes.is_empty());
        // Empty.
        snap.insert(
            &oxpath!("ui", "settings", "new_account", "name_input"),
            Value::String(String::new()),
        );
        let writes = run_cmd(&AccountsCreate::new(), &mut snap);
        assert!(writes.is_empty());
    }

    #[test]
    fn accounts_delete_writes_delete_now_when_selected() {
        let mut snap = SettingsSnapshot::empty();
        select_account(&mut snap, "alpha");
        let writes = run_cmd(&AccountsDelete::new(), &mut snap);
        let comp = ox_kernel::PathComponent::try_new("alpha").unwrap();
        assert_null_write(
            &writes,
            oxpath!("config", "gate", "accounts", comp, "delete_now"),
        );
    }

    #[test]
    fn accounts_delete_inert_without_selection() {
        let mut snap = SettingsSnapshot::empty();
        let writes = run_cmd(&AccountsDelete::new(), &mut snap);
        assert!(writes.is_empty());
    }

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
    fn selector_cycle_protocol_synthesizes_account_config_for_toml_loaded() {
        // TOML-loaded accounts have no parent `AccountConfig` leaf —
        // only child fields like `…/{name}/provider`. The cycle must
        // still advance and write back; the first cycle creates the
        // leaf.
        let mut snap = SettingsSnapshot::empty();
        let comp = ox_kernel::PathComponent::try_new("alpha").unwrap();
        // Child-only `provider` field, no parent leaf.
        snap.insert(
            &oxpath!("config", "gate", "accounts", comp.clone(), "provider"),
            Value::String("anthropic".into()),
        );
        snap.insert(
            &oxpath!("ui", "settings", "accounts", "selected"),
            to_value(&Some("alpha".to_string())).unwrap(),
        );
        let writes = run_cmd(&SelectorCycleProtocol::new(), &mut snap);
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("config", "gate", "accounts", comp));
        // anthropic → openai
        match &writes[0].record {
            Record::Parsed(v) => {
                let acct: AccountConfig = structfs_serde_store::from_value(v.clone()).unwrap();
                assert_eq!(acct.provider, "openai");
            }
            other => panic!("unexpected record: {other:?}"),
        }
    }

    #[test]
    fn selector_cycle_protocol_advances_then_writes_account() {
        let mut snap = SettingsSnapshot::empty();
        write_account(&mut snap, "alpha", "anthropic");
        select_account(&mut snap, "alpha");
        let writes = run_cmd(&SelectorCycleProtocol::new(), &mut snap);
        let comp = ox_kernel::PathComponent::try_new("alpha").unwrap();
        let acct_write = writes
            .iter()
            .find(|w| w.path == oxpath!("config", "gate", "accounts", comp))
            .expect("account write");
        match &acct_write.record {
            Record::Parsed(v) => {
                let acct: AccountConfig = structfs_serde_store::from_value(v.clone()).unwrap();
                assert_eq!(acct.provider, "openai");
            }
            other => panic!("unexpected record: {other:?}"),
        }
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
    fn resolve_protocol_options_appends_user_providers() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("config", "gate", "providers", "lm_studio", "dialect"),
            Value::String("openai".into()),
        );
        snap.insert(
            &oxpath!("config", "gate", "providers", "lm_studio", "endpoint"),
            Value::String("http://127.0.0.1:1234".into()),
        );
        let opts = resolve_protocol_options(&mut snap, "anthropic");
        assert_eq!(
            opts,
            vec![
                "anthropic".to_string(),
                "openai".to_string(),
                "lm_studio".to_string()
            ]
        );
    }

    #[test]
    fn resolve_protocol_options_dedupes_user_provider_named_like_preset() {
        // A user provider literally named "anthropic" must not appear twice.
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("config", "gate", "providers", "anthropic", "dialect"),
            Value::String("anthropic".into()),
        );
        let opts = resolve_protocol_options(&mut snap, "anthropic");
        assert_eq!(opts, vec!["anthropic".to_string(), "openai".to_string()]);
    }

    #[test]
    fn resolve_protocol_options_appends_current_when_absent() {
        // Account whose provider isn't in presets and isn't a configured
        // provider record either (an orphan binding). The current value must
        // still appear so cycling can find it and advance honestly.
        let mut snap = SettingsSnapshot::empty();
        let opts = resolve_protocol_options(&mut snap, "LMStudio");
        assert_eq!(
            opts,
            vec![
                "anthropic".to_string(),
                "openai".to_string(),
                "LMStudio".to_string()
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
    fn cycle_protocol_forward_from_custom_provider_does_not_snap_to_anthropic() {
        // The cycle's option list must include the account's current
        // provider — otherwise position_of returns None, the index falls
        // back to 0, and the first cycle silently overwrites the custom
        // provider with whatever sits at idx 1 of the preset list.
        //
        // With resolve_protocol_options the list is
        // ["anthropic", "openai", "LMStudio"]; idx of "LMStudio" is 2;
        // forward wraps to idx 0 = "anthropic".
        let mut snap = SettingsSnapshot::empty();
        write_account(&mut snap, "local", "LMStudio");
        select_account(&mut snap, "local");

        let writes = selector_cycle_protocol_dir(&mut snap, CycleDir::Forward);
        assert_eq!(writes.len(), 1);
        let written: AccountConfig =
            structfs_serde_store::from_value(writes[0].record.as_value().unwrap().clone()).unwrap();
        assert_eq!(written.provider, "anthropic");
    }

    #[test]
    fn cycle_protocol_back_from_custom_provider_lands_on_previous_option() {
        // With options [anthropic, openai, LMStudio], back from idx 2 = "openai".
        let mut snap = SettingsSnapshot::empty();
        write_account(&mut snap, "local", "LMStudio");
        select_account(&mut snap, "local");

        let writes = selector_cycle_protocol_dir(&mut snap, CycleDir::Back);
        assert_eq!(writes.len(), 1);
        let written: AccountConfig =
            structfs_serde_store::from_value(writes[0].record.as_value().unwrap().clone()).unwrap();
        assert_eq!(written.provider, "openai");
    }

    #[test]
    fn cycle_protocol_forward_includes_user_configured_provider() {
        // An account bound to "openai" cycles forward through any
        // user-configured provider entries before wrapping back to anthropic.
        // Here: configure a "lm_studio" provider, then forward from "openai"
        // (idx 1 in [anthropic, openai, lm_studio]) lands on "lm_studio".
        let mut snap = SettingsSnapshot::empty();
        write_account(&mut snap, "alpha", "openai");
        write_provider(
            &mut snap,
            "lm_studio",
            "http://127.0.0.1:1234",
            AuthScheme::None,
        );
        select_account(&mut snap, "alpha");

        let writes = selector_cycle_protocol_dir(&mut snap, CycleDir::Forward);
        assert_eq!(writes.len(), 1);
        let written: AccountConfig =
            structfs_serde_store::from_value(writes[0].record.as_value().unwrap().clone()).unwrap();
        assert_eq!(written.provider, "lm_studio");
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
}
