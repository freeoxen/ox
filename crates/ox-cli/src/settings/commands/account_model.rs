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

use serde::{Deserialize, Serialize};

use ox_path::oxpath;
use ox_types::Screen;
use ox_types::key_chord::KeyCodeRepr;
use ox_types::settings::{AccountField, ModelField, ModelKey};
use ox_types::subscription::Write;
use structfs_core_store::{Path, Reader, Record, Value};
use structfs_serde_store::to_value;

use ox_gate::{AccountConfig, ApiKey, AuthScheme, CompletionRole, ProviderConfig};

use crate::settings::command_registry::{CommandCtx, CommandRegistry};
use crate::settings::renderers::util::read_typed;

#[allow(unused_imports)]
use super::command;
use super::navigation::path_to_value;

// ---------------------------------------------------------------------------
// Shared payload types
// ---------------------------------------------------------------------------

/// Request payload for the account-create subscription. Phase N6
/// (`AccountCreateSubscription`) will deserialize this from
/// `config/gate/accounts/_create_now` and validate `name` as a
/// `PathComponent` before allocating the account.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateAccountRequest {
    pub name: String,
}

// ---------------------------------------------------------------------------
// Cursor-shuffle commands (overlays)
// ---------------------------------------------------------------------------

command! {
    struct_name: AccountsAdd,
    id: "accounts.add",
    title: "Add Account",
    description: "Open the new-account overlay.",
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
    title: "Delete Account…",
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
    description: "Dismiss the current overlay; return to the accounts list.",
    screen: Screen::Settings,
    cursor: None,
    run: |_snap, _ctx| vec![Write {
        path: oxpath!("ui", "settings", "cursor"),
        record: Record::parsed(path_to_value(&oxpath!("settings", "accounts"))),
    }],
}

// ---------------------------------------------------------------------------
// Subscription-request commands
// ---------------------------------------------------------------------------

command! {
    struct_name: AccountsCreate,
    id: "accounts.create",
    title: "Create Account",
    description: "Submit the new-account name; the subscription does the rest.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "accounts", "_new")),
    run: |snap, _ctx| accounts_create(snap),
}

command! {
    struct_name: AccountsDelete,
    id: "accounts.delete",
    title: "Confirm Delete",
    description: "Submit the delete request for the selected account.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "accounts", "_delete")),
    run: |snap, _ctx| accounts_delete(snap),
}

command! {
    struct_name: AccountTest,
    id: "account.test",
    title: "Test Connection",
    description: "Trigger a connection test for the selected account.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "accounts", "_detail")),
    run: |snap, _ctx| account_test(snap),
}

command! {
    struct_name: AccountRefresh,
    id: "account.refresh",
    title: "Refresh Catalog",
    description: "Re-fetch the model catalog for the selected model's account.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "models")),
    run: |snap, _ctx| account_refresh(snap),
}

command! {
    struct_name: ModelsSetPrimary,
    id: "models.set_primary",
    title: "Set as Primary",
    description: "Bind config/completions/primary to the selected (account, model).",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "models")),
    run: |snap, _ctx| models_set_primary(snap),
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
    description: "Cycle the focused account-detail field forward.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "accounts", "_detail")),
    run: |snap, _ctx| field_account_step(snap, 1),
}

command! {
    struct_name: FieldAccountPrev,
    id: "field.account.prev",
    title: "Previous Field",
    description: "Cycle the focused account-detail field backward.",
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
// Text-field edits
// ---------------------------------------------------------------------------
//
// `FieldInsert` and `FieldDeleteBack` mutate one of the two editable text
// fields on the Account detail page: `Endpoint` (stored on the
// provider config) or `Key` (stored at `secret/keys/{name}: ApiKey`).
// `Name` is immutable post-creation per spec §6.4 — no insert path.
// `Protocol` and `Auth` are selectors, not text fields, so they live on
// the `selector.cycle.*` commands below.

command! {
    struct_name: FieldInsert,
    id: "field.insert",
    title: "Insert Character",
    description: "Insert the keystroke character into the focused text field.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "accounts", "_detail")),
    run: |snap, ctx| field_insert(snap, ctx),
}

command! {
    struct_name: FieldDeleteBack,
    id: "field.delete_back",
    title: "Backspace",
    description: "Delete the character before the cursor in the focused text field.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "accounts", "_detail")),
    run: |snap, _ctx| field_delete_back(snap),
}

// ---------------------------------------------------------------------------
// Selector cycling (Protocol, Auth)
// ---------------------------------------------------------------------------

command! {
    struct_name: SelectorCycleProtocol,
    id: "selector.cycle.protocol",
    title: "Cycle Protocol",
    description: "Advance the account's protocol/dialect selector.",
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

// ---------------------------------------------------------------------------
// Implementation helpers
// ---------------------------------------------------------------------------

fn read_selected_account(data: &mut dyn Reader) -> Option<String> {
    read_typed::<Option<String>>(data, &oxpath!("ui", "settings", "accounts", "selected")).flatten()
}

fn read_selected_model(data: &mut dyn Reader) -> Option<ModelKey> {
    read_typed::<Option<ModelKey>>(data, &oxpath!("ui", "settings", "models", "selected")).flatten()
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
    let name: String = match read_typed(data, &oxpath!("ui", "settings", "new_account", "name_input")) {
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

fn models_set_primary(data: &mut dyn Reader) -> Vec<Write> {
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
            tracing::warn!(error = %e, "models.set_primary: failed to encode CompletionRole");
            return Vec::new();
        }
    };
    vec![Write {
        path: oxpath!("config", "completions", "primary"),
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
    let idx = ACCOUNT_FIELDS.iter().position(|f| *f == current).unwrap_or(0);
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
    let current: ModelField =
        read_typed(data, &oxpath!("ui", "settings", "model_detail", "field"))
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

/// Field-aware text mutation. Returns the writes needed to apply
/// `update(current_text, cursor) -> (new_text, new_cursor)`. Inert when no
/// editable field is focused or the precondition fails.
fn mutate_focused_text<F>(data: &mut dyn Reader, update: F) -> Vec<Write>
where
    F: FnOnce(String, u32) -> Option<(String, u32)>,
{
    let field: AccountField = read_typed(
        data,
        &oxpath!("ui", "settings", "account_detail", "field"),
    )
    .unwrap_or(AccountField::Name);
    let selected = match read_selected_account(data) {
        Some(s) => s,
        None => return Vec::new(),
    };
    let name_comp = match ox_kernel::PathComponent::try_new(&selected) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let cursor: u32 = read_typed(data, &oxpath!("ui", "settings", "edit_cursor")).unwrap_or(0);

    match field {
        AccountField::Endpoint => {
            let acct: AccountConfig = match read_typed(
                data,
                &oxpath!("config", "gate", "accounts", name_comp.clone()),
            ) {
                Some(a) => a,
                None => return Vec::new(),
            };
            let provider_comp = match ox_kernel::PathComponent::try_new(&acct.provider) {
                Ok(c) => c,
                Err(_) => return Vec::new(),
            };
            let provider_path = oxpath!("config", "gate", "providers", provider_comp);
            let mut provider: ProviderConfig = match read_typed(data, &provider_path) {
                Some(p) => p,
                None => return Vec::new(),
            };
            let (new_text, new_cursor) = match update(provider.endpoint.clone(), cursor) {
                Some(t) => t,
                None => return Vec::new(),
            };
            provider.endpoint = new_text;
            let value = match to_value(&provider) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, "field.text: failed to encode ProviderConfig");
                    return Vec::new();
                }
            };
            vec![
                Write {
                    path: provider_path,
                    record: Record::parsed(value),
                },
                cursor_write(new_cursor),
            ]
        }
        AccountField::Key => {
            let key_path = oxpath!("secret", "keys", name_comp);
            let current_text: String = read_typed::<ApiKey>(data, &key_path)
                .map(|k| k.expose().to_string())
                .unwrap_or_default();
            let (new_text, new_cursor) = match update(current_text, cursor) {
                Some(t) => t,
                None => return Vec::new(),
            };
            let value = match to_value(&ApiKey::new(new_text)) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, "field.text: failed to encode ApiKey");
                    return Vec::new();
                }
            };
            vec![
                Write {
                    path: key_path,
                    record: Record::parsed(value),
                },
                cursor_write(new_cursor),
            ]
        }
        AccountField::Name | AccountField::Protocol | AccountField::Auth => Vec::new(),
    }
}

fn cursor_write(new_cursor: u32) -> Write {
    Write {
        path: oxpath!("ui", "settings", "edit_cursor"),
        record: Record::parsed(Value::Integer(new_cursor as i64)),
    }
}

fn field_insert(data: &mut dyn Reader, ctx: &CommandCtx<'_>) -> Vec<Write> {
    let chord = match ctx.last_keystroke.as_ref() {
        Some(c) => c,
        None => return Vec::new(),
    };
    let ch = match chord.code {
        KeyCodeRepr::Char(c) => c,
        _ => return Vec::new(),
    };
    mutate_focused_text(data, |text, cursor| {
        let cur = (cursor as usize).min(text.len());
        let mut buf = String::with_capacity(text.len() + ch.len_utf8());
        buf.push_str(&text[..cur]);
        buf.push(ch);
        buf.push_str(&text[cur..]);
        Some((buf, cursor + 1))
    })
}

fn field_delete_back(data: &mut dyn Reader) -> Vec<Write> {
    mutate_focused_text(data, |text, cursor| {
        if cursor == 0 {
            return None;
        }
        let cur = (cursor as usize).min(text.len());
        // Walk back one char-boundary so we don't split a multi-byte char.
        let mut prev = cur.saturating_sub(1);
        while prev > 0 && !text.is_char_boundary(prev) {
            prev -= 1;
        }
        let mut buf = String::with_capacity(text.len());
        buf.push_str(&text[..prev]);
        buf.push_str(&text[cur..]);
        Some((buf, cursor - 1))
    })
}

fn selector_cycle_protocol(data: &mut dyn Reader) -> Vec<Write> {
    let selected = match read_selected_account(data) {
        Some(s) => s,
        None => return Vec::new(),
    };
    let name_comp = match ox_kernel::PathComponent::try_new(&selected) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let acct_path = oxpath!("config", "gate", "accounts", name_comp);
    let mut acct: AccountConfig = match read_typed(data, &acct_path) {
        Some(a) => a,
        None => return Vec::new(),
    };
    const OPTIONS: &[&str] = &["anthropic", "openai"];
    let idx = OPTIONS
        .iter()
        .position(|o| *o == acct.provider)
        .unwrap_or(0);
    let next = OPTIONS[(idx + 1) % OPTIONS.len()];
    acct.provider = next.to_string();
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

const AUTH_OPTIONS: [AuthScheme; 3] = [
    AuthScheme::XApiKey,
    AuthScheme::BearerToken,
    AuthScheme::None,
];

fn selector_cycle_auth(data: &mut dyn Reader) -> Vec<Write> {
    let selected = match read_selected_account(data) {
        Some(s) => s,
        None => return Vec::new(),
    };
    let name_comp = match ox_kernel::PathComponent::try_new(&selected) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let acct: AccountConfig = match read_typed(
        data,
        &oxpath!("config", "gate", "accounts", name_comp.clone()),
    ) {
        Some(a) => a,
        None => return Vec::new(),
    };
    let provider_comp = match ox_kernel::PathComponent::try_new(&acct.provider) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let provider_path = oxpath!("config", "gate", "providers", provider_comp);
    let mut provider: ProviderConfig = match read_typed(data, &provider_path) {
        Some(p) => p,
        None => return Vec::new(),
    };
    let current = provider.resolved_auth();
    let idx = AUTH_OPTIONS
        .iter()
        .position(|a| *a == current)
        .unwrap_or(0);
    let next = AUTH_OPTIONS[(idx + 1) % AUTH_OPTIONS.len()].clone();
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
    reg.register(Box::new(ModelsSetPrimary::new()));
    reg.register(Box::new(AppSave::new()));
    reg.register(Box::new(FieldAccountNext::new()));
    reg.register(Box::new(FieldAccountPrev::new()));
    reg.register(Box::new(FieldModelNext::new()));
    reg.register(Box::new(FieldModelPrev::new()));
    reg.register(Box::new(FieldInsert::new()));
    reg.register(Box::new(FieldDeleteBack::new()));
    reg.register(Box::new(SelectorCycleProtocol::new()));
    reg.register(Box::new(SelectorCycleAuth::new()));
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use ox_types::key_chord::{KeyChord, KeyCodeRepr, KeyModifierSet};

    use crate::settings::command_registry::Command;
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
                    Record::Parsed(v) => super::super::navigation::path_from_value(v)
                        == Some(expected_target.clone()),
                    _ => false,
                }
        }));
    }

    fn assert_null_write(writes: &[Write], expected_path: structfs_core_store::Path) {
        let hit = writes.iter().any(|w| {
            w.path == expected_path
                && matches!(&w.record, Record::Parsed(Value::Null))
        });
        assert!(hit, "expected Null write at {expected_path}, got {writes:?}");
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
    fn accounts_cancel_writes_accounts_cursor() {
        let mut snap = SettingsSnapshot::empty();
        let writes = run_cmd(&AccountsCancel::new(), &mut snap);
        assert_eq!(writes.len(), 1);
        assert_cursor_write(&writes, oxpath!("settings", "accounts"));
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
        assert_eq!(writes[0].path, oxpath!("config", "gate", "accounts", "_create_now"));
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
    fn models_set_primary_writes_completion_role() {
        let mut snap = SettingsSnapshot::empty();
        select_model(&mut snap, "alpha", "m1");
        let writes = run_cmd(&ModelsSetPrimary::new(), &mut snap);
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("config", "completions", "primary"));
        match &writes[0].record {
            Record::Parsed(v) => {
                let role: CompletionRole =
                    structfs_serde_store::from_value(v.clone()).unwrap();
                assert_eq!(role.account, "alpha");
                assert_eq!(role.model_id, "m1");
            }
            other => panic!("unexpected record: {other:?}"),
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

    #[test]
    fn field_insert_writes_updated_endpoint_and_cursor() {
        let mut snap = SettingsSnapshot::empty();
        setup_endpoint_edit(&mut snap, "https://api.example.com", 23);
        // Append 'X' at the end.
        let writes = run_cmd_with_keystroke(
            &FieldInsert::new(),
            &mut snap,
            Some(keystroke_char('X')),
        );
        // Expect a provider write + cursor write.
        let provider_path = oxpath!(
            "config",
            "gate",
            "providers",
            ox_kernel::PathComponent::try_new("anthropic").unwrap()
        );
        let provider_write = writes
            .iter()
            .find(|w| w.path == provider_path)
            .expect("provider write");
        match &provider_write.record {
            Record::Parsed(v) => {
                let pc: ProviderConfig =
                    structfs_serde_store::from_value(v.clone()).unwrap();
                assert_eq!(pc.endpoint, "https://api.example.comX");
            }
            other => panic!("unexpected record: {other:?}"),
        }
        let cursor_write = writes
            .iter()
            .find(|w| w.path == oxpath!("ui", "settings", "edit_cursor"))
            .expect("cursor write");
        match &cursor_write.record {
            Record::Parsed(Value::Integer(n)) => assert_eq!(*n, 24),
            other => panic!("unexpected record: {other:?}"),
        }
    }

    #[test]
    fn field_insert_inert_when_no_keystroke() {
        let mut snap = SettingsSnapshot::empty();
        setup_endpoint_edit(&mut snap, "https://api.example.com", 23);
        let writes = run_cmd_with_keystroke(&FieldInsert::new(), &mut snap, None);
        assert!(writes.is_empty());
    }

    #[test]
    fn field_insert_inert_when_keystroke_not_char() {
        let mut snap = SettingsSnapshot::empty();
        setup_endpoint_edit(&mut snap, "https://api.example.com", 23);
        let writes = run_cmd_with_keystroke(
            &FieldInsert::new(),
            &mut snap,
            Some(KeyChord {
                modifiers: KeyModifierSet::default(),
                code: KeyCodeRepr::Tab,
            }),
        );
        assert!(writes.is_empty());
    }

    #[test]
    fn field_delete_back_removes_char_and_decrements_cursor() {
        let mut snap = SettingsSnapshot::empty();
        setup_endpoint_edit(&mut snap, "https://api.example.com", 23);
        let writes = run_cmd(&FieldDeleteBack::new(), &mut snap);
        let provider_path = oxpath!(
            "config",
            "gate",
            "providers",
            ox_kernel::PathComponent::try_new("anthropic").unwrap()
        );
        let provider_write = writes
            .iter()
            .find(|w| w.path == provider_path)
            .expect("provider write");
        match &provider_write.record {
            Record::Parsed(v) => {
                let pc: ProviderConfig =
                    structfs_serde_store::from_value(v.clone()).unwrap();
                assert_eq!(pc.endpoint, "https://api.example.co");
            }
            other => panic!("unexpected record: {other:?}"),
        }
        let cursor_write = writes
            .iter()
            .find(|w| w.path == oxpath!("ui", "settings", "edit_cursor"))
            .expect("cursor write");
        match &cursor_write.record {
            Record::Parsed(Value::Integer(n)) => assert_eq!(*n, 22),
            other => panic!("unexpected record: {other:?}"),
        }
    }

    #[test]
    fn field_delete_back_noop_at_cursor_zero() {
        let mut snap = SettingsSnapshot::empty();
        setup_endpoint_edit(&mut snap, "https://api.example.com", 0);
        let writes = run_cmd(&FieldDeleteBack::new(), &mut snap);
        assert!(writes.is_empty());
    }

    // -- Selectors --------------------------------------------------------------

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
                let acct: AccountConfig =
                    structfs_serde_store::from_value(v.clone()).unwrap();
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
                let pc: ProviderConfig =
                    structfs_serde_store::from_value(v.clone()).unwrap();
                assert_eq!(pc.auth, Some(AuthScheme::BearerToken));
            }
            other => panic!("unexpected record: {other:?}"),
        }
    }
}
