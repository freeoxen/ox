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

use ox_kernel::{AccountName, PathComponent};
use ox_path::oxpath;
use ox_types::Screen;
use ox_types::settings::{AccountField, ModelField, ModelKey, ValidationErrors};
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
// Per-field metadata: label, kind, draft-state subpath, and canonical order.
// Exhaustive matches over AccountField — adding a variant must fail compile
// here until every helper is updated.
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FieldKind {
    Text,
    Selector,
}

pub(crate) fn field_label(f: AccountField) -> &'static str {
    match f {
        AccountField::Name => "Name",
        AccountField::Protocol => "Protocol",
        AccountField::Endpoint => "Endpoint",
        AccountField::Auth => "Auth",
        AccountField::Key => "Key",
    }
}

pub(crate) fn field_kind(f: AccountField) -> FieldKind {
    match f {
        AccountField::Name | AccountField::Endpoint | AccountField::Key => FieldKind::Text,
        AccountField::Protocol | AccountField::Auth => FieldKind::Selector,
    }
}

pub(crate) fn field_state_subpath(f: AccountField) -> &'static str {
    match f {
        AccountField::Name => "name",
        AccountField::Protocol => "protocol",
        AccountField::Endpoint => "endpoint",
        AccountField::Auth => "auth",
        AccountField::Key => "key",
    }
}

pub(crate) const FIELD_ORDER: [AccountField; 5] = [
    AccountField::Name,
    AccountField::Protocol,
    AccountField::Endpoint,
    AccountField::Auth,
    AccountField::Key,
];

pub(crate) fn focus_next(field: AccountField) -> AccountField {
    let idx = FIELD_ORDER.iter().position(|f| *f == field).expect("variant in FIELD_ORDER");
    FIELD_ORDER[(idx + 1) % FIELD_ORDER.len()]
}

pub(crate) fn focus_prev(field: AccountField) -> AccountField {
    let idx = FIELD_ORDER.iter().position(|f| *f == field).expect("variant in FIELD_ORDER");
    FIELD_ORDER[(idx + FIELD_ORDER.len() - 1) % FIELD_ORDER.len()]
}

/// Synthetic cursor path for the compose-form's focused field.
/// `field_focus_path(Name) = settings/_compose_form/name`. Total: every
/// `field_state_subpath` value is a valid XID identifier by construction.
pub(crate) fn field_focus_path(f: AccountField) -> Path {
    let comp = PathComponent::try_new(field_state_subpath(f))
        .expect("field_state_subpath returns valid identifiers");
    oxpath!("settings", "_compose_form", comp)
}

/// Inverse of `field_focus_path`: map a cursor path back to the
/// compose field it points at. Returns `None` when the cursor is not
/// inside `settings/_compose_form/<field>`.
///
/// The cursor's role under cursor-as-focus is to encode the focused
/// sub-element by virtue of its leaf component. The dispatcher uses
/// this same mapping to decide whether the compose form is "active".
pub(crate) fn cursor_to_field(cursor: &Path) -> Option<AccountField> {
    if cursor.components.len() != 3 {
        return None;
    }
    if cursor.components[0] != "settings" || cursor.components[1] != "_compose_form" {
        return None;
    }
    match cursor.components[2].as_str() {
        "name" => Some(AccountField::Name),
        "protocol" => Some(AccountField::Protocol),
        "endpoint" => Some(AccountField::Endpoint),
        "auth" => Some(AccountField::Auth),
        "key" => Some(AccountField::Key),
        _ => None,
    }
}

/// True iff `cursor`'s prefix is `settings/_compose_form` (either the
/// container itself or any descendant). The dispatcher uses this to
/// decide whether to push the compose-form scope onto the binding
/// scope path under cursor-as-focus.
pub(crate) fn cursor_is_in_compose_form(cursor: &Path) -> bool {
    cursor.components.len() >= 2
        && cursor.components[0] == "settings"
        && cursor.components[1] == "_compose_form"
}

/// Build the progressively-longer prefixes of `path`, ending at
/// `path` itself. `path_ancestors(settings/_compose_form/name) =
/// [settings, settings/_compose_form, settings/_compose_form/name]`.
/// Empty paths yield an empty vec.
pub(crate) fn path_ancestors(path: &Path) -> Vec<Path> {
    (1..=path.components.len())
        .map(|end| path.slice(0, end))
        .collect()
}

/// Synthetic cursor path for the manual-model wizard's focused stage.
/// `manual_model_focus_path(Id) = settings/_manual_model/id`. The
/// lowercase segment matches compose's per-field naming convention; it
/// is intentionally distinct from `ManualModelStage`'s PascalCase serde
/// wire format (the stage no longer goes through serde for routing).
pub(crate) fn manual_model_focus_path(stage: ox_types::settings::ManualModelStage) -> Path {
    use ox_types::settings::ManualModelStage;
    let name = match stage {
        ManualModelStage::Id => "id",
        ManualModelStage::Ctx => "ctx",
        ManualModelStage::Out => "out",
    };
    let comp = PathComponent::try_new(name).expect("manual-model stage names are valid XIDs");
    oxpath!("settings", "_manual_model", comp)
}

/// Inverse of `manual_model_focus_path`: map a cursor path back to
/// the manual-model stage it points at. Returns `None` when the cursor
/// is not inside `settings/_manual_model/<stage>`.
pub(crate) fn cursor_to_manual_model_stage(
    cursor: &Path,
) -> Option<ox_types::settings::ManualModelStage> {
    use ox_types::settings::ManualModelStage;
    if cursor.components.len() != 3 {
        return None;
    }
    if cursor.components[0] != "settings" || cursor.components[1] != "_manual_model" {
        return None;
    }
    match cursor.components[2].as_str() {
        "id" => Some(ManualModelStage::Id),
        "ctx" => Some(ManualModelStage::Ctx),
        "out" => Some(ManualModelStage::Out),
        _ => None,
    }
}

/// True iff `cursor`'s prefix is `settings/_manual_model` (either the
/// container itself or any descendant). The dispatcher uses this to
/// decide whether to push the manual-model form scope onto the binding
/// scope path under cursor-as-focus.
pub(crate) fn cursor_is_in_manual_model(cursor: &Path) -> bool {
    cursor.components.len() >= 2
        && cursor.components[0] == "settings"
        && cursor.components[1] == "_manual_model"
}

/// Synthetic cursor path for the confirm-delete dialog. The
/// confirm-delete widget is single-scope (no sub-form), so the cursor
/// sits at `settings/_confirm_delete` directly — there is no per-field
/// or per-stage leaf below it.
pub(crate) fn confirm_delete_focus_path() -> Path {
    oxpath!("settings", "_confirm_delete")
}

/// True iff `cursor` equals `settings/_confirm_delete`. The
/// dispatcher uses this to engage the confirm-delete scope under
/// cursor-as-focus; the target account being confirmed lives at the
/// separate data path `ui/settings/pending_delete/target_account`.
pub(crate) fn cursor_is_in_confirm_delete(cursor: &Path) -> bool {
    cursor.components.len() == 2
        && cursor.components[0] == "settings"
        && cursor.components[1] == "_confirm_delete"
}

/// True iff `cursor` equals `settings/_edit`. The dispatcher uses
/// this to engage the inline-edit scope under cursor-as-focus; the
/// target field path, edit buffer, and saved pre-open cursor live at
/// the separate data subtree `ui/settings/edit/{target_path, buffer,
/// cursor_saved}`.
pub(crate) fn cursor_is_in_edit(cursor: &Path) -> bool {
    cursor.components.len() == 2
        && cursor.components[0] == "settings"
        && cursor.components[1] == "_edit"
}

// ---------------------------------------------------------------------------
// Per-field validators. Each is pure and total: returns `None` for a valid
// input or `Some(message)` for the first rule that rejects it. Cross-field
// rules (key required iff auth requires key) live inside the relevant
// per-field validator, so the aggregator stays a simple struct-of-Option
// build.
// ---------------------------------------------------------------------------

pub(crate) fn validate_compose_draft(
    name: &str,
    protocol: Option<&str>,
    endpoint: &str,
    auth: Option<&AuthScheme>,
    key: &str,
    existing_accounts: &[String],
) -> ValidationErrors {
    ValidationErrors {
        name: validate_compose_name(name, existing_accounts),
        protocol: validate_compose_protocol(protocol),
        endpoint: validate_compose_endpoint(endpoint),
        auth: validate_compose_auth(auth),
        key: validate_compose_key(key, auth),
    }
}

pub(crate) fn validate_compose_name(name: &str, existing: &[String]) -> Option<String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Some("required".into());
    }
    if trimmed.chars().count() > 256 {
        return Some("too long (max 256 chars)".into());
    }
    // `existing` holds on-disk path components (already namecoded).
    // Encode the proposed name and compare.
    let encoded = namecode::encode(trimmed);
    if existing.iter().any(|n| n == &encoded) {
        return Some(format!("'{trimmed}' already exists"));
    }
    None
}

pub(crate) fn validate_compose_protocol(protocol: Option<&str>) -> Option<String> {
    if protocol.is_none() {
        Some("select a protocol".into())
    } else {
        None
    }
}

/// UX-level endpoint validation: rejects empty/whitespace only.
/// Parseability/reachability checks live at the gate layer
/// (`ox_gate::provider::validate_endpoint`). The split is deliberate:
/// compose mode shouldn't block a user mid-draft on URL formatting
/// quirks — invalid endpoints surface at first use of the connection.
pub(crate) fn validate_compose_endpoint(endpoint: &str) -> Option<String> {
    if endpoint.trim().is_empty() {
        Some("required".into())
    } else {
        None
    }
}

pub(crate) fn validate_compose_auth(auth: Option<&AuthScheme>) -> Option<String> {
    if auth.is_none() {
        Some("select an auth scheme".into())
    } else {
        None
    }
}

pub(crate) fn validate_compose_key(key: &str, auth: Option<&AuthScheme>) -> Option<String> {
    match auth {
        Some(scheme) if scheme.requires_key() && key.trim().is_empty() => {
            Some("required for this auth scheme".into())
        }
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Cursor-shuffle commands (overlays)
// ---------------------------------------------------------------------------

command! {
    struct_name: AccountsComposeOpen,
    id: "accounts.compose.open",
    title: "New connection",
    description: "Initialize the multi-field new-connection draft and enter compose mode.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "accounts")),
    run: |snap, _ctx| accounts_compose_open(snap),
}

// Compose-mode commands. The cursor at `settings/_compose_form/<field>`
// IS the open state — no separate `new_account/active` flag exists.
// The dispatcher walks cursor ancestors so the form scope
// `settings/_compose_form` and the per-field leaf scope
// `settings/_compose_form/<field>` both sit on the binding scope path.
// Lifecycle keys (Esc/Tab/.../Enter) live on the form scope; text-input
// or selector-cycling keys live on the per-field leaf scopes.

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
    struct_name: AccountsComposeCycleForward,
    id: "accounts.compose.cycle_forward",
    title: "Next option",
    description: "Cycle the focused selector to the next option.",
    screen: Screen::Settings,
    cursor: None,
    run: |snap, _ctx| accounts_compose_cycle(snap, CycleDir::Forward),
}

command! {
    struct_name: AccountsComposeCycleBack,
    id: "accounts.compose.cycle_back",
    title: "Previous option",
    description: "Cycle the focused selector to the previous option.",
    screen: Screen::Settings,
    cursor: None,
    run: |snap, _ctx| accounts_compose_cycle(snap, CycleDir::Back),
}

command! {
    struct_name: AccountsComposeFocusNext,
    id: "accounts.compose.focus_next",
    title: "Next field",
    description: "Advance compose-mode focus to the next field.",
    screen: Screen::Settings,
    cursor: None,
    run: |snap, _ctx| accounts_compose_focus_next(snap),
}

command! {
    struct_name: AccountsComposeFocusPrev,
    id: "accounts.compose.focus_prev",
    title: "Previous field",
    description: "Retreat compose-mode focus to the previous field.",
    screen: Screen::Settings,
    cursor: None,
    run: |snap, _ctx| accounts_compose_focus_prev(snap),
}

command! {
    struct_name: AccountsComposeCommit,
    id: "accounts.compose.commit",
    title: "Create connection",
    description: "Validate the compose-mode draft and materialize the new connection.",
    screen: Screen::Settings,
    cursor: None,
    run: |snap, _ctx| accounts_compose_commit(snap),
}

command! {
    struct_name: AccountsComposeCancel,
    id: "accounts.compose.cancel",
    title: "Cancel new connection",
    description: "Discard the new-account draft; exit compose mode.",
    screen: Screen::Settings,
    cursor: None,
    run: |snap, _ctx| accounts_compose_cancel(snap),
}

// Confirm-delete commands. Under cursor-as-focus the cursor at
// `settings/_confirm_delete` IS the open state — there is no separate
// `ui/settings/pending_delete: Option<AccountName>` flag. The target
// account being confirmed lives at the separate data path
// `ui/settings/pending_delete/target_account: String`; the saved
// pre-open cursor lives at `ui/settings/pending_delete/cursor_saved`.

command! {
    struct_name: AccountsConfirmDelete,
    id: "accounts.confirm.delete",
    title: "Confirm delete",
    description: "Delete the target account record; restore the cursor.",
    screen: Screen::Settings,
    cursor: None,
    run: |snap, _ctx| accounts_confirm_delete(snap),
}

command! {
    struct_name: AccountsConfirmCancel,
    id: "accounts.confirm.cancel",
    title: "Cancel delete",
    description: "Dismiss the delete-confirmation banner without deleting; restore the cursor.",
    screen: Screen::Settings,
    cursor: None,
    run: |snap, _ctx| accounts_confirm_cancel(snap),
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

// Manual-model commands. The cursor at
// `settings/_manual_model/<stage>` IS the open state — no separate
// `manual_model/stage` discriminator exists. The dispatcher walks
// cursor ancestors so the form scope `settings/_manual_model` and
// per-stage leaf scope `settings/_manual_model/<stage>` both sit on
// the binding scope path. Lifecycle keys (Esc/Enter) live on the
// form scope; text-input keys live on the per-stage leaf scopes.

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

fn accounts_compose_open(data: &mut dyn Reader) -> Vec<Write> {
    use crate::settings::renderers::util::child_names_under;

    // Cursor-as-focus: the act of moving the cursor INTO the compose
    // form's namespace IS the open. Mode is encoded in cursor's path
    // segments — no separate `new_account/active` discriminator. The
    // dispatcher's `compute_scope_path` walks cursor ancestors so a
    // cursor at `settings/_compose_form/name` naturally activates the
    // form scope plus the name field's leaf scope.
    let existing_accounts = child_names_under(data, "config/gate/accounts");
    let errors = validate_compose_draft("", None, "", None, "", &existing_accounts);

    // Save the prior cursor so cancel can restore it; on commit the
    // target write (new account row) supersedes this save naturally.
    let cursor_saved = read_focused_path(data);

    vec![
        Write {
            path: oxpath!("ui", "settings", "new_account", "cursor_saved"),
            record: Record::parsed(match cursor_saved.as_ref() {
                Some(p) => path_to_value(p),
                None => Value::Null,
            }),
        },
        Write {
            path: oxpath!("ui", "settings", "focused"),
            record: Record::parsed(path_to_value(&field_focus_path(AccountField::Name))),
        },
        Write {
            path: oxpath!("ui", "settings", "new_account", "name"),
            record: Record::parsed(Value::String(String::new())),
        },
        Write {
            path: oxpath!("ui", "settings", "new_account", "protocol"),
            record: Record::parsed(Value::Null),
        },
        Write {
            path: oxpath!("ui", "settings", "new_account", "endpoint"),
            record: Record::parsed(Value::String(String::new())),
        },
        Write {
            path: oxpath!("ui", "settings", "new_account", "auth"),
            record: Record::parsed(Value::Null),
        },
        Write {
            path: oxpath!("ui", "settings", "new_account", "key"),
            record: Record::parsed(Value::String(String::new())),
        },
        Write {
            path: oxpath!("ui", "settings", "new_account", "errors"),
            record: Record::parsed(to_value(&errors).unwrap()),
        },
    ]
}

/// Discard the compose draft and restore the cursor to where it was
/// before `open`. Reads `cursor_saved` BEFORE the Null-cascade write
/// (both writes apply later, in batch order; the snapshot read happens
/// here against the pre-batch state). When no save is present —
/// pathological seed — fall back to `settings/accounts` so the user
/// lands somewhere sensible.
fn accounts_compose_cancel(data: &mut dyn Reader) -> Vec<Write> {
    use crate::settings::commands::navigation::path_from_value;

    let saved: Option<Path> = data
        .read(&oxpath!("ui", "settings", "new_account", "cursor_saved"))
        .ok()
        .flatten()
        .and_then(|r| r.as_value().cloned())
        .and_then(|v| path_from_value(&v));

    let restored = saved.unwrap_or_else(|| oxpath!("settings", "accounts"));

    vec![
        Write {
            path: oxpath!("ui", "settings", "focused"),
            record: Record::parsed(path_to_value(&restored)),
        },
        // Cascade-clear every child field at the subtree root in one
        // write; this also clears `cursor_saved` we just read above.
        Write {
            path: oxpath!("ui", "settings", "new_account"),
            record: Record::parsed(Value::Null),
        },
    ]
}

/// Read the typed-Path value at `ui/settings/focused`. Returns `None`
/// when missing or shape-mismatched. Mirrors `read_cursor` in the
/// renderer; lives here so command bodies can save/restore the cursor
/// without reaching across modules.
fn read_focused_path(data: &mut dyn Reader) -> Option<Path> {
    use crate::settings::commands::navigation::path_from_value;
    let record = data
        .read(&oxpath!("ui", "settings", "focused"))
        .ok()
        .flatten()?;
    let value = record.as_value()?;
    path_from_value(value)
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
    let focused = focused_compose_field(data);
    if field_kind(focused) != FieldKind::Text {
        return Vec::new();
    }
    let path = field_state_path(focused);
    let mut buf: String = read_typed(data, &path).unwrap_or_default();
    buf.push(ch);

    let writes = vec![Write {
        path: path.clone(),
        record: Record::parsed(Value::String(buf.clone())),
    }];
    recompute_errors_writes(data, writes)
}

/// Resolve the full snapshot path of the given field's draft buffer
/// (`ui/settings/new_account/<subpath>`). Total: every variant of
/// `AccountField` maps to a fixed identifier, so the `PathComponent`
/// validation cannot fail.
fn field_state_path(f: AccountField) -> Path {
    let comp = PathComponent::try_new(field_state_subpath(f))
        .expect("field_state_subpath returns valid identifiers");
    oxpath!("ui", "settings", "new_account", comp)
}

/// Read the currently focused compose field from the cursor. Under
/// cursor-as-focus, the cursor's leaf segment IS the focused field;
/// no separate `focused_field` discriminator exists. Defaults to
/// `Name` when the cursor isn't at a form-field path — command bodies
/// only run while the dispatcher has routed via the compose scope, so
/// this only ever falls through during tests that seed an incomplete
/// snapshot.
fn focused_compose_field(data: &mut dyn Reader) -> AccountField {
    read_focused_path(data)
        .as_ref()
        .and_then(cursor_to_field)
        .unwrap_or(AccountField::Name)
}

/// Recompute the validation-errors record from the current draft, append
/// the resulting `Write` to `writes`, and return the augmented vec.
///
/// Each field's value is taken from a pending Write in `writes` if one
/// targets the matching `ui/settings/new_account/<subpath>` path, falling
/// back to the snapshot otherwise. That lets keystroke / cycle handlers
/// reflect their own just-emitted writes in the recomputed errors before
/// the snapshot has applied them — without that override the freshly
/// cycled selector would see the pre-cycle (stale) value and re-emit the
/// "select a …" error one frame after the user picked a value.
fn recompute_errors_writes(data: &mut dyn Reader, mut writes: Vec<Write>) -> Vec<Write> {
    use crate::settings::renderers::util::child_names_under;

    // Look up a write at `path` in the pending vec and return its parsed
    // value, if any. Each field has a single canonical path so a linear
    // scan is fine here (writes is at most a handful of entries).
    let pending_value = |path: &Path| -> Option<Value> {
        writes.iter().find_map(|w| {
            if &w.path == path {
                w.record.as_value().cloned()
            } else {
                None
            }
        })
    };

    let read_text = |data: &mut dyn Reader, f: AccountField| -> String {
        let path = field_state_path(f);
        if let Some(Value::String(s)) = pending_value(&path) {
            return s;
        }
        read_typed::<String>(data, &path).unwrap_or_default()
    };
    let read_protocol = |data: &mut dyn Reader| -> Option<String> {
        let path = field_state_path(AccountField::Protocol);
        match pending_value(&path) {
            Some(Value::String(s)) => Some(s),
            Some(Value::Null) => None,
            // Non-string / non-null pending write: fall back to snapshot
            // rather than guess a coercion.
            Some(_) | None => read_typed(data, &path),
        }
    };
    let read_auth = |data: &mut dyn Reader| -> Option<AuthScheme> {
        let path = field_state_path(AccountField::Auth);
        match pending_value(&path) {
            Some(Value::Null) => None,
            Some(v) => structfs_serde_store::from_value(v).ok(),
            None => read_typed(data, &path),
        }
    };

    let name = read_text(data, AccountField::Name);
    let endpoint = read_text(data, AccountField::Endpoint);
    let key = read_text(data, AccountField::Key);
    let protocol = read_protocol(data);
    let auth = read_auth(data);
    let existing = child_names_under(data, "config/gate/accounts");

    let errors = validate_compose_draft(
        &name,
        protocol.as_deref(),
        &endpoint,
        auth.as_ref(),
        &key,
        &existing,
    );
    writes.push(Write {
        path: oxpath!("ui", "settings", "new_account", "errors"),
        record: Record::parsed(to_value(&errors).unwrap()),
    });
    writes
}

fn accounts_compose_delete_back(data: &mut dyn Reader) -> Vec<Write> {
    let focused = focused_compose_field(data);
    if field_kind(focused) != FieldKind::Text {
        return Vec::new();
    }
    let path = field_state_path(focused);
    let mut buf: String = read_typed(data, &path).unwrap_or_default();
    if buf.pop().is_none() {
        return Vec::new();
    }

    let writes = vec![Write {
        path: path.clone(),
        record: Record::parsed(Value::String(buf.clone())),
    }];
    recompute_errors_writes(data, writes)
}

/// Carousel options for the compose-draft Protocol field. Mirrors the
/// set produced by `resolve_protocol_options(_, "")` (presets table,
/// non-custom, deduped) — kept as a `&[&str]` constant because compose
/// mode has no current dialect-anchor to drive the dynamic version, and
/// the user can't type a custom value through a selector field. If
/// `ox_gate::presets()` gains a new non-custom dialect, extend this list
/// in lockstep.
pub(crate) const PROTOCOL_OPTIONS: &[&str] = &["anthropic", "openai"];

/// Project the compose draft state as a typed `View::Form`. One row per
/// field in `FIELD_ORDER`; text fields surface their live buffer (Key is
/// masked), selector fields surface their option list + current index
/// (or a `(not selected)` placeholder when the user hasn't picked yet);
/// every row threads through the matching slot from the `errors` record.
///
/// The renderer composes this into a `View::Stack { [Form, List] }`
/// only while the cursor sits under `settings/_compose_form/...`;
/// outside compose the form has nothing to project and the caller
/// emits the bare list. `View::Form.focused` is derived from cursor:
/// when the cursor's leaf isn't a known field id the form renders
/// with no row highlighted (defensive — the dispatcher only routes
/// here while cursor is at a field path).
pub(crate) fn compose_form_view(data: &mut dyn Reader) -> horns_core::view::View {
    use horns_core::view::{FormRow, View};
    let errors: ValidationErrors =
        read_typed(data, &oxpath!("ui", "settings", "new_account", "errors"))
            .unwrap_or_default();

    let rows: Vec<FormRow> = FIELD_ORDER
        .iter()
        .map(|f| project_compose_field(data, *f, errors.for_field(*f)))
        .collect();

    let focused = read_focused_path(data)
        .as_ref()
        .and_then(cursor_to_field)
        .and_then(|f| FIELD_ORDER.iter().position(|x| *x == f));
    View::Form { rows, focused }
}

fn project_compose_field(
    data: &mut dyn Reader,
    field: AccountField,
    error: Option<&str>,
) -> horns_core::view::FormRow {
    use horns_core::view::FormRow;

    let label = field_label(field).to_string();
    let error = error.map(String::from);

    let (value, hint) = match field {
        AccountField::Name | AccountField::Endpoint => {
            let v: String = read_typed(data, &field_state_path(field)).unwrap_or_default();
            (text_form_value(v, /*masked=*/ false), None)
        }
        AccountField::Key => {
            let v: String = read_typed(data, &field_state_path(field)).unwrap_or_default();
            // Hint clarifies why the key is required (auth schemes that
            // need one) without leaking the actual value.
            (
                text_form_value(v, /*masked=*/ true),
                Some("required for x-api-key / bearer-token".into()),
            )
        }
        AccountField::Protocol => {
            let current: Option<String> = read_typed(data, &field_state_path(field));
            (selector_form_value(current.as_deref(), PROTOCOL_OPTIONS), None)
        }
        AccountField::Auth => {
            let current: Option<AuthScheme> = read_typed(data, &field_state_path(field));
            // Format the enum through Display so the form's wire-format
            // labels stay aligned with on-disk serialization (the same
            // alignment the carousel commands rely on).
            let auth_strs: Vec<String> = AuthScheme::ALL.iter().map(|a| a.to_string()).collect();
            let current_str = current.map(|a| a.to_string());
            let refs: Vec<&str> = auth_strs.iter().map(|s| s.as_str()).collect();
            (selector_form_value(current_str.as_deref(), &refs), None)
        }
    };

    FormRow {
        label,
        value,
        error,
        hint,
    }
}

fn text_form_value(value: String, masked: bool) -> horns_core::view::FormValue {
    let cursor = value.chars().count() as u32;
    horns_core::view::FormValue::Text {
        value,
        cursor,
        masked,
    }
}

fn selector_form_value(current: Option<&str>, options: &[&str]) -> horns_core::view::FormValue {
    match current.and_then(|c| options.iter().position(|o| *o == c)) {
        // `(not selected)` is the read-only placeholder the form shows
        // for a selector the user hasn't pressed h/l on yet. The
        // ReadOnly variant keeps it visually distinct from a chosen
        // option and means no carousel decoration shows when the row
        // takes focus before a pick.
        None => horns_core::view::FormValue::ReadOnly("(not selected)".into()),
        Some(idx) => horns_core::view::FormValue::Selector {
            options: options.iter().map(|s| s.to_string()).collect(),
            current: idx,
        },
    }
}

/// Advance compose-mode focus to the next field in `FIELD_ORDER`,
/// wrapping past the last entry. Pure: one write to the cursor; no
/// validation recompute (focus changes don't change buffer contents,
/// so the errors record is unaffected).
fn accounts_compose_focus_next(data: &mut dyn Reader) -> Vec<Write> {
    let current = focused_compose_field(data);
    let next = focus_next(current);
    vec![Write {
        path: oxpath!("ui", "settings", "focused"),
        record: Record::parsed(path_to_value(&field_focus_path(next))),
    }]
}

/// Retreat compose-mode focus to the previous field in `FIELD_ORDER`,
/// wrapping past the first entry. See `accounts_compose_focus_next`
/// for the no-recompute rationale.
fn accounts_compose_focus_prev(data: &mut dyn Reader) -> Vec<Write> {
    let current = focused_compose_field(data);
    let prev = focus_prev(current);
    vec![Write {
        path: oxpath!("ui", "settings", "focused"),
        record: Record::parsed(path_to_value(&field_focus_path(prev))),
    }]
}

fn accounts_compose_cycle(data: &mut dyn Reader, dir: CycleDir) -> Vec<Write> {
    let focused = focused_compose_field(data);
    if field_kind(focused) != FieldKind::Selector {
        return Vec::new();
    }
    let writes = match focused {
        AccountField::Protocol => cycle_compose_protocol(data, dir),
        AccountField::Auth => cycle_compose_auth(data, dir),
        AccountField::Name | AccountField::Endpoint | AccountField::Key => Vec::new(),
    };
    // `recompute_errors_writes` scans `writes` for pending field writes so
    // the just-cycled value drives validation immediately — without that
    // override the stale "select a …" error from the pre-cycle snapshot
    // would survive one frame past the user's keystroke.
    recompute_errors_writes(data, writes)
}

fn cycle_compose_protocol(data: &mut dyn Reader, dir: CycleDir) -> Vec<Write> {
    let current: Option<String> =
        read_typed(data, &field_state_path(AccountField::Protocol));
    let next = cycle_str_options(PROTOCOL_OPTIONS, current.as_deref(), dir);
    vec![Write {
        path: field_state_path(AccountField::Protocol),
        record: Record::parsed(Value::String(next.to_string())),
    }]
}

fn cycle_compose_auth(data: &mut dyn Reader, dir: CycleDir) -> Vec<Write> {
    let current: Option<AuthScheme> =
        read_typed(data, &field_state_path(AccountField::Auth));
    let next = cycle_enum_options(&AuthScheme::ALL, current.as_ref(), dir);
    let value = match to_value(&next) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    vec![Write {
        path: field_state_path(AccountField::Auth),
        record: Record::parsed(value),
    }]
}

/// Pick the next option after `current` (forward) or before it (back),
/// wrapping at the ends. When `current` isn't in `options` — including
/// `None` (first press) — forward picks `options[0]`, back picks the
/// last entry. Caller must ensure `options` is non-empty.
fn cycle_str_options<'a>(
    options: &'a [&'a str],
    current: Option<&str>,
    dir: CycleDir,
) -> &'a str {
    match current.and_then(|c| options.iter().position(|o| *o == c)) {
        None => match dir {
            CycleDir::Forward => options[0],
            CycleDir::Back => options[options.len() - 1],
        },
        Some(idx) => {
            let step = match dir {
                CycleDir::Forward => 1,
                CycleDir::Back => options.len() - 1,
            };
            options[(idx + step) % options.len()]
        }
    }
}

/// `cycle_str_options`'s enum cousin. Returns a clone of the next
/// option — `T: Clone` rather than `Copy` so `AuthScheme` (not `Copy`
/// for forward-compat) works without a wrapper type.
fn cycle_enum_options<T: Clone + PartialEq>(
    options: &[T],
    current: Option<&T>,
    dir: CycleDir,
) -> T {
    match current.and_then(|c| options.iter().position(|o| o == c)) {
        None => match dir {
            CycleDir::Forward => options[0].clone(),
            CycleDir::Back => options[options.len() - 1].clone(),
        },
        Some(idx) => {
            let step = match dir {
                CycleDir::Forward => 1,
                CycleDir::Back => options.len() - 1,
            };
            options[(idx + step) % options.len()].clone()
        }
    }
}

/// Materialize the compose draft into per-account config records.
///
/// Reads draft state from `ui/settings/new_account/*`, validates the full
/// draft, and (only if validation passes) emits writes for:
/// - `config/gate/accounts/<path_id>` — `AccountConfig` with `provider =
///   path_id` and `display_name = Some(typed_name)`. Each new connection
///   gets its own `path_id`, so two connections never share a provider
///   record (the bug this commit fixes).
/// - `config/gate/providers/<path_id>` — `ProviderConfig` carrying the
///   draft's dialect/endpoint/auth.
/// - `secret/keys/<path_id>` — `ApiKey`, only when the chosen auth
///   `requires_key()`.
/// - `ui/settings/focused` and `ui/settings/expanded` — focus + expand the
///   new account row.
/// - `ui/settings/new_account` ← `Null` — subtree cascade clears the draft
///   in one write.
///
/// When validation fails the function returns `Vec::new()`: the commit is
/// atomic from the user's perspective, so a partial draft never produces
/// a partial record. The renderer already surfaces per-field error
/// messages from the `errors` record kept current by every keystroke, so
/// no banner is needed here.
///
/// `path_id = namecode::encode(display_name)` encodes the user-typed name
/// into a valid XID identifier suitable as a path component. The encoding
/// is idempotent on names that are already valid XIDs (e.g. `"personal"
/// → "personal"`), so the common case adds no transformation; non-XID
/// inputs (hyphens, spaces, unicode) are bootstring-encoded.
fn accounts_compose_commit(data: &mut dyn Reader) -> Vec<Write> {
    use crate::settings::renderers::util::child_names_under;
    use ox_gate::ApiKey;
    use std::collections::BTreeSet;

    let display_name = read_typed::<String>(data, &field_state_path(AccountField::Name))
        .unwrap_or_default()
        .trim()
        .to_string();
    let protocol: Option<String> =
        read_typed(data, &field_state_path(AccountField::Protocol));
    let endpoint = read_typed::<String>(data, &field_state_path(AccountField::Endpoint))
        .unwrap_or_default()
        .trim()
        .to_string();
    let auth: Option<AuthScheme> = read_typed(data, &field_state_path(AccountField::Auth));
    let key = read_typed::<String>(data, &field_state_path(AccountField::Key))
        .unwrap_or_default();

    let existing = child_names_under(data, "config/gate/accounts");
    let errors = validate_compose_draft(
        &display_name,
        protocol.as_deref(),
        &endpoint,
        auth.as_ref(),
        &key,
        &existing,
    );
    if !errors.is_clean() {
        return Vec::new();
    }

    // Safe: validation rejected `None` for both selectors above.
    let protocol = protocol.expect("validated Some");
    let auth = auth.expect("validated Some");

    // Encode the typed display name into a valid XID path component.
    // Idempotent on already-valid identifiers (`"personal" -> "personal"`),
    // so the common ASCII case stays human-readable; non-XID input
    // (hyphens, spaces, unicode) gets bootstring-encoded.
    let path_id = namecode::encode(&display_name);
    let path_component = PathComponent::try_new(&path_id)
        .expect("namecode::encode produces a valid XID by construction");

    let acct = AccountConfig {
        provider: path_id.clone(),
        display_name: Some(display_name.clone()),
    };
    let provider = ProviderConfig {
        dialect: protocol.clone(),
        endpoint: endpoint.clone(),
        version: protocol_default_version(&protocol),
        auth: Some(auth.clone()),
    };

    let mut writes = vec![
        Write {
            path: oxpath!("config", "gate", "accounts", path_component.clone()),
            record: Record::parsed(to_value(&acct).unwrap()),
        },
        Write {
            path: oxpath!("config", "gate", "providers", path_component.clone()),
            record: Record::parsed(to_value(&provider).unwrap()),
        },
    ];

    if auth.requires_key() {
        // `ApiKey` is `#[serde(transparent)]` over the wrapped String, so
        // this writes a plain `Value::String` at the secret path — same
        // shape `current_api_key` reads back via `read_typed`.
        writes.push(Write {
            path: oxpath!("secret", "keys", path_component.clone()),
            record: Record::parsed(to_value(&ApiKey::new(key.trim().to_string())).unwrap()),
        });
    }

    // Focus + expand the new account row. `expanded` is stored as a
    // sorted set; using BTreeSet locally dedupes a re-expansion of an
    // already-expanded section without an explicit `contains` walk.
    let mut expanded: BTreeSet<String> =
        read_typed::<Vec<String>>(data, &oxpath!("ui", "settings", "expanded"))
            .unwrap_or_default()
            .into_iter()
            .collect();
    expanded.insert("settings/accounts".to_string());
    expanded.insert(format!("settings/accounts/{path_id}"));
    let expanded_vec: Vec<String> = expanded.into_iter().collect();

    writes.push(Write {
        path: oxpath!("ui", "settings", "focused"),
        record: Record::parsed(path_to_value(&oxpath!(
            "settings",
            "accounts",
            path_component.clone()
        ))),
    });
    writes.push(Write {
        path: oxpath!("ui", "settings", "expanded"),
        record: Record::parsed(to_value(&expanded_vec).unwrap()),
    });

    // Clear draft state via subtree Null-cascade — one write at the
    // subtree root rather than per-field cleanup.
    //
    // Intentionally do NOT write `ui/settings/cursor` or
    // `ui/settings/accounts/selected`. Both downstream readers
    // (`read_selected_account`, `accounts_step`) prefer `focused`, which
    // is set above; the legacy commit's writes to those paths were
    // redundant under the new convention.
    writes.push(Write {
        path: oxpath!("ui", "settings", "new_account"),
        record: Record::parsed(Value::Null),
    });

    writes
}

/// API-version header default for the given dialect. Matches the values
/// the corresponding `ProviderConfig` constructors use (e.g.
/// `ProviderConfig::anthropic()` carries `"2023-06-01"`); empty for
/// dialects (OpenAI, custom) that don't pin a version header.
fn protocol_default_version(protocol: &str) -> String {
    match protocol {
        "anthropic" => "2023-06-01".to_string(),
        _ => String::new(),
    }
}

// ---------------------------------------------------------------------------
// Manual-model helpers
// ---------------------------------------------------------------------------

/// Open the manual-model entry form for the focused account. Resolves
/// the focused row to its un-sanitized account name (Model and
/// ModelField rows both carry it), saves the prior cursor for
/// cancel-restore, then writes the cursor into the synthetic form
/// namespace at the Id stage and seeds `manual_model/{account, buffer}`.
///
/// Cursor-as-focus: the act of moving the cursor INTO
/// `settings/_manual_model/<stage>` IS the open. Mode is encoded in
/// the cursor's path segments — no separate `manual_model/stage`
/// discriminator. The dispatcher's `compute_scope_path` walks cursor
/// ancestors so the form scope plus per-stage leaf scope sit on the
/// binding scope path naturally.
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

    // Save the prior cursor so cancel can restore it. Commit walks
    // back to the new model's row directly and supersedes this save.
    let cursor_saved = read_focused_path(data);

    vec![
        Write {
            path: oxpath!("ui", "settings", "manual_model", "account"),
            record: Record::parsed(Value::String(account)),
        },
        Write {
            path: oxpath!("ui", "settings", "manual_model", "buffer"),
            record: Record::parsed(Value::String(String::new())),
        },
        Write {
            path: oxpath!("ui", "settings", "manual_model", "cursor_saved"),
            record: Record::parsed(match cursor_saved.as_ref() {
                Some(p) => path_to_value(p),
                None => Value::Null,
            }),
        },
        Write {
            path: oxpath!("ui", "settings", "focused"),
            record: Record::parsed(path_to_value(&manual_model_focus_path(
                ManualModelStage::Id,
            ))),
        },
    ]
}

/// Read the active manual-model stage from the cursor. Under
/// cursor-as-focus the cursor's leaf segment IS the active stage;
/// `manual_model/stage` is retired. Returns `None` when the cursor
/// isn't at a manual-model stage path — command bodies only run while
/// the dispatcher has routed via the manual-model scope, so this only
/// ever returns `None` for stale snapshots that the dispatcher's gate
/// has already excluded.
fn models_manual_active_stage(data: &mut dyn Reader) -> Option<ox_types::settings::ManualModelStage> {
    read_focused_path(data)
        .as_ref()
        .and_then(cursor_to_manual_model_stage)
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
    let stage = match models_manual_active_stage(data) {
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

    let stage = match models_manual_active_stage(data) {
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
            // Cursor-as-focus: advance the wizard by rewriting the
            // cursor's leaf to the next stage. No `manual_model/stage`
            // write — the cursor IS the stage.
            vec![
                Write {
                    path: oxpath!("ui", "settings", "focused"),
                    record: Record::parsed(path_to_value(&manual_model_focus_path(
                        ManualModelStage::Ctx,
                    ))),
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
            vec![
                Write {
                    path: oxpath!("ui", "settings", "focused"),
                    record: Record::parsed(path_to_value(&manual_model_focus_path(
                        ManualModelStage::Out,
                    ))),
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

            let catalog_path = oxpath!("config", "gate", "accounts", comp.clone(), "models");
            let mut catalog: Vec<ModelInfo> = read_typed(data, &catalog_path).unwrap_or_default();
            catalog.push(ModelInfo {
                id: id.clone(),
                display_name: id.clone(),
                max_context_size: Some(ctx),
                max_output_tokens: Some(out),
                source: ModelInfoSource::UserEntered,
            });
            let catalog_value = match to_value(&catalog) {
                Ok(v) => v,
                Err(_) => return Vec::new(),
            };

            // Move the cursor to the new model's row so the user lands
            // on what they just created. Row paths follow
            // `settings/models/<account>/<safe(model_id)>` — same
            // encoding the visible-rows enumerator uses. Non-XID ids
            // degrade to the parent Models page if the safe form
            // can't be reconstructed as a PathComponent (the
            // visible-rows filter would also have dropped such a row,
            // so the user simply lands on the parent section).
            let safe_id = crate::settings::visible_rows::safe_component(&id);
            let new_cursor = match PathComponent::try_new(&safe_id) {
                Ok(model_comp) => oxpath!("settings", "models", comp.clone(), model_comp),
                Err(_) => oxpath!("settings", "models"),
            };

            // Write the catalog, restore the cursor to the new row,
            // and clear the form state. The Null-cascade at the
            // manual_model root clears every child in one entry
            // (including `cursor_saved` from open).
            vec![
                Write {
                    path: catalog_path,
                    record: Record::parsed(catalog_value),
                },
                Write {
                    path: oxpath!("ui", "settings", "focused"),
                    record: Record::parsed(path_to_value(&new_cursor)),
                },
                Write {
                    path: oxpath!("ui", "settings", "manual_model"),
                    record: Record::parsed(Value::Null),
                },
            ]
        }
    }
}

/// Discard the manual-model draft and restore the cursor to where it
/// was before `open`. Reads `cursor_saved` BEFORE the Null-cascade
/// write applies (the snapshot read happens here; the writes apply in
/// batch order later). When no save is present — pathological seed —
/// fall back to `settings/models` so the user lands somewhere sensible.
fn models_manual_cancel(data: &mut dyn Reader) -> Vec<Write> {
    use crate::settings::commands::navigation::path_from_value;

    let saved: Option<Path> = data
        .read(&oxpath!("ui", "settings", "manual_model", "cursor_saved"))
        .ok()
        .flatten()
        .and_then(|r| r.as_value().cloned())
        .and_then(|v| path_from_value(&v));

    let restored = saved.unwrap_or_else(|| oxpath!("settings", "models"));

    vec![
        Write {
            path: oxpath!("ui", "settings", "focused"),
            record: Record::parsed(path_to_value(&restored)),
        },
        // Cascade-clear every child path at the manual_model root in
        // one write; this also clears `cursor_saved` we just read above.
        Write {
            path: oxpath!("ui", "settings", "manual_model"),
            record: Record::parsed(Value::Null),
        },
    ]
}

/// Open the confirm-delete dialog. Resolves the focused row to its
/// account name, saves the prior cursor for cancel-restore, moves the
/// cursor into the synthetic `_confirm_delete` namespace, and stashes
/// the target account at a child of `ui/settings/pending_delete` so
/// the renderer's banner and the confirm command can read it.
///
/// Cursor-as-focus: the act of moving the cursor INTO
/// `settings/_confirm_delete` IS the open. Mode is encoded in the
/// cursor's path segments — no separate `pending_delete:
/// Option<AccountName>` flag. The dispatcher's `compute_scope_path`
/// engages the confirm-delete scope by virtue of the cursor sitting
/// there.
fn accounts_delete_confirm(data: &mut dyn Reader) -> Vec<Write> {
    let name = match read_selected_account(data) {
        Some(n) => n,
        None => return Vec::new(),
    };

    // Save the prior cursor so cancel can restore it. Confirm also
    // reads cursor_saved to land somewhere sensible after the row
    // disappears.
    let cursor_saved = read_focused_path(data);

    vec![
        Write {
            path: oxpath!("ui", "settings", "pending_delete", "target_account"),
            record: Record::parsed(Value::String(name.into_string())),
        },
        Write {
            path: oxpath!("ui", "settings", "pending_delete", "cursor_saved"),
            record: Record::parsed(match cursor_saved.as_ref() {
                Some(p) => path_to_value(p),
                None => Value::Null,
            }),
        },
        Write {
            path: oxpath!("ui", "settings", "focused"),
            record: Record::parsed(path_to_value(&confirm_delete_focus_path())),
        },
    ]
}

/// Confirm the pending delete. Reads `target_account` from the
/// dedicated data path (the value half of the retired value-flag),
/// emits the canonical Null write at the account record, restores the
/// cursor to the saved pre-open location (typically the row that just
/// got deleted — the accordion will re-anchor naturally), and cascade-
/// clears the entire `ui/settings/pending_delete` subtree.
fn accounts_confirm_delete(data: &mut dyn Reader) -> Vec<Write> {
    use crate::settings::commands::navigation::path_from_value;
    use ox_kernel::PathComponent;

    // Read the target account from the dedicated data path. Defensive
    // no-op when missing — the dispatcher shouldn't have routed here.
    let name: String = read_typed(
        data,
        &oxpath!("ui", "settings", "pending_delete", "target_account"),
    )
    .unwrap_or_default();
    if name.is_empty() {
        // No target — just clear the subtree and bail. Don't restore
        // the cursor from an unknown saved value: fall through to a
        // safe fallback.
        return vec![
            Write {
                path: oxpath!("ui", "settings", "focused"),
                record: Record::parsed(path_to_value(&oxpath!("settings", "accounts"))),
            },
            Write {
                path: oxpath!("ui", "settings", "pending_delete"),
                record: Record::parsed(Value::Null),
            },
        ];
    }
    let comp = match PathComponent::try_new(&name) {
        Ok(c) => c,
        Err(_) => {
            // Target got an invalid name. Clear the subtree
            // defensively so we don't leave the user stuck in
            // confirmation mode; restore cursor to the Accounts header.
            return vec![
                Write {
                    path: oxpath!("ui", "settings", "focused"),
                    record: Record::parsed(path_to_value(&oxpath!("settings", "accounts"))),
                },
                Write {
                    path: oxpath!("ui", "settings", "pending_delete"),
                    record: Record::parsed(Value::Null),
                },
            ];
        }
    };

    // Restore the cursor to where it was before open. The saved cursor
    // typically points at the row that just got deleted; the
    // accordion's `visible_rows` enumeration will no longer surface
    // that path and the focused-row scope falls back gracefully. Fall
    // back to `settings/accounts` when no save is present.
    let saved: Option<Path> = data
        .read(&oxpath!("ui", "settings", "pending_delete", "cursor_saved"))
        .ok()
        .flatten()
        .and_then(|r| r.as_value().cloned())
        .and_then(|v| path_from_value(&v));
    let restored = saved.unwrap_or_else(|| oxpath!("settings", "accounts"));

    vec![
        // The actual delete — Null write to the canonical account
        // path. The AccountDeleteCleanupSubscription watches Prefix
        // for null writes at account-record depth and does the
        // cross-cutting side-data cleanup.
        Write {
            path: oxpath!("config", "gate", "accounts", comp),
            record: Record::parsed(Value::Null),
        },
        // Restore cursor to where the user was before open.
        Write {
            path: oxpath!("ui", "settings", "focused"),
            record: Record::parsed(path_to_value(&restored)),
        },
        // Cascade-clear the entire pending_delete subtree (target_account
        // + cursor_saved) in a single Null write at the subtree root.
        Write {
            path: oxpath!("ui", "settings", "pending_delete"),
            record: Record::parsed(Value::Null),
        },
    ]
}

/// Cancel the confirm-delete dialog. Reads the saved pre-open cursor,
/// restores it, and cascade-clears the `ui/settings/pending_delete`
/// subtree. When no save is present — pathological seed — fall back
/// to `settings/accounts` so the user lands somewhere sensible.
fn accounts_confirm_cancel(data: &mut dyn Reader) -> Vec<Write> {
    use crate::settings::commands::navigation::path_from_value;

    let saved: Option<Path> = data
        .read(&oxpath!("ui", "settings", "pending_delete", "cursor_saved"))
        .ok()
        .flatten()
        .and_then(|r| r.as_value().cloned())
        .and_then(|v| path_from_value(&v));
    let restored = saved.unwrap_or_else(|| oxpath!("settings", "accounts"));

    vec![
        Write {
            path: oxpath!("ui", "settings", "focused"),
            record: Record::parsed(path_to_value(&restored)),
        },
        // Cascade-clear every child at the pending_delete root in one
        // write; this also clears `cursor_saved` we just read above.
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
        ..Default::default()
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
        ..Default::default()
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
    let options = AuthScheme::ALL;
    let idx = options.iter().position(|a| *a == current).unwrap_or(0);
    let next = match dir {
        CycleDir::Forward => options[(idx + 1) % options.len()].clone(),
        CycleDir::Back => options[(idx + options.len() - 1) % options.len()].clone(),
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
    reg.register(Box::new(AccountsComposeOpen::new()));
    reg.register(Box::new(AccountsComposeInsertChar::new()));
    reg.register(Box::new(AccountsComposeDeleteBack::new()));
    reg.register(Box::new(AccountsComposeCycleForward::new()));
    reg.register(Box::new(AccountsComposeCycleBack::new()));
    reg.register(Box::new(AccountsComposeFocusNext::new()));
    reg.register(Box::new(AccountsComposeFocusPrev::new()));
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
                ..Default::default()
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

    /// Return the parsed `Value` of the write whose path stringifies to
    /// `path_str`, or `None` if no such write exists.
    fn writes_value(writes: &[Write], path_str: &str) -> Option<Value> {
        writes.iter().find_map(|w| {
            if w.path.to_string() != path_str {
                return None;
            }
            match &w.record {
                Record::Parsed(v) => Some(v.clone()),
                _ => None,
            }
        })
    }

    fn test_snapshot_with_no_accounts() -> SettingsSnapshot {
        SettingsSnapshot::empty()
    }

    #[test]
    fn accounts_compose_open_initializes_multi_field_draft() {
        let mut snap = test_snapshot_with_no_accounts();
        let writes = run_cmd(&AccountsComposeOpen::new(), &mut snap);

        // Cursor moves to the form's Name field. The cursor's path
        // segments encode the mode (compose-form) and the focused
        // sub-element (name) — no separate `active` discriminator.
        let focused_val = writes_value(&writes, "ui/settings/focused")
            .expect("cursor written");
        let focused_path = super::super::navigation::path_from_value(&focused_val)
            .expect("cursor value decodes as Path");
        assert_eq!(
            focused_path,
            oxpath!("settings", "_compose_form", "name"),
            "open writes cursor to compose form's Name field",
        );

        // Retired paths must NOT be touched.
        assert!(
            writes_value(&writes, "ui/settings/new_account/active").is_none(),
            "retired discriminator must not be written",
        );
        assert!(
            writes_value(&writes, "ui/settings/new_account/focused_field").is_none(),
            "retired focused_field must not be written",
        );

        // Empty buffers for the three text fields.
        for sub in ["name", "endpoint", "key"] {
            assert_eq!(
                writes_value(&writes, &format!("ui/settings/new_account/{sub}")),
                Some(Value::String(String::new())),
                "field {sub}",
            );
        }

        // Null for the two selector fields.
        for sub in ["protocol", "auth"] {
            assert_eq!(
                writes_value(&writes, &format!("ui/settings/new_account/{sub}")),
                Some(Value::Null),
                "field {sub}",
            );
        }

        // Errors record present, all required fields flagged.
        let errors_val = writes_value(&writes, "ui/settings/new_account/errors")
            .expect("errors written");
        let errors: ValidationErrors =
            structfs_serde_store::from_value(errors_val).unwrap();
        assert!(errors.name.is_some());
        assert!(errors.protocol.is_some());
        assert!(errors.endpoint.is_some());
        assert!(errors.auth.is_some());
        // No auth selected → no key required yet.
        assert!(errors.key.is_none());

        // Legacy single-field buffer must NOT be written — a stale signal
        // for any compose-aware code still in the system.
        assert!(
            writes_value(&writes, "ui/settings/new_account/buffer").is_none(),
            "legacy buffer must not be written",
        );
    }

    #[test]
    fn compose_open_saves_prior_cursor() {
        // Before open, the user's cursor sits at an account row. After
        // open, the prior cursor lives at
        // `ui/settings/new_account/cursor_saved` so cancel can restore
        // it. (Commit doesn't restore — it writes cursor to the new
        // account row directly.)
        let mut snap = test_snapshot_with_no_accounts();
        snap.insert(
            &oxpath!("ui", "settings", "focused"),
            super::super::navigation::path_to_value(&oxpath!(
                "settings",
                "accounts",
                "alpha"
            )),
        );
        let writes = run_cmd(&AccountsComposeOpen::new(), &mut snap);
        let saved_val = writes_value(&writes, "ui/settings/new_account/cursor_saved")
            .expect("cursor_saved written");
        let saved =
            super::super::navigation::path_from_value(&saved_val).expect("decode saved cursor");
        assert_eq!(saved, oxpath!("settings", "accounts", "alpha"));
    }

    #[test]
    fn compose_open_writes_cursor_to_name_field() {
        // Pin the cursor-as-focus invariant: open writes
        // `ui/settings/focused = settings/_compose_form/name`.
        let mut snap = test_snapshot_with_no_accounts();
        let writes = run_cmd(&AccountsComposeOpen::new(), &mut snap);
        let focused_val = writes_value(&writes, "ui/settings/focused")
            .expect("focused written");
        let focused = super::super::navigation::path_from_value(&focused_val)
            .expect("decode focused path");
        assert_eq!(focused, oxpath!("settings", "_compose_form", "name"));
    }

    #[test]
    fn accounts_compose_open_name_error_reflects_existing_accounts() {
        // The validator consults `config/gate/accounts` to flag duplicate
        // names; with an empty draft the name error is "required", so this
        // test exists to lock in that `accounts_compose_open` actually
        // reads from the snapshot (not a hard-coded empty list).
        let mut snap = SettingsSnapshot::empty();
        write_account(&mut snap, "alpha", "anthropic");

        let writes = run_cmd(&AccountsComposeOpen::new(), &mut snap);
        let errors_val = writes_value(&writes, "ui/settings/new_account/errors")
            .expect("errors written");
        let errors: ValidationErrors =
            structfs_serde_store::from_value(errors_val).unwrap();
        // Empty name still trips the `required` rule; the duplicate-check
        // is exercised once the user has typed a name. We just check that
        // the existing accounts read succeeded by confirming no panic.
        assert_eq!(errors.name.as_deref(), Some("required"));
    }

    // -- Compose-mode tests -----------------------------------------------------
    //
    // The legacy single-buffer commit tests were removed when commit
    // grew its multi-field shape: validation now runs across the whole
    // draft and a failing field is a silent no-op (the errors record is
    // already current from the keystroke handler, so no banner write is
    // needed). The replacement coverage below pins (a) the no-op contract
    // when validation fails, (b) the happy path with an already-XID
    // display name (the path_id equals the name), and (c) the encoding
    // path for non-XID display names.

    /// Build a snapshot in compose mode with every draft field set to a
    /// concrete value. Mirrors `test_snapshot_with_compose_state` plus a
    /// fully-typed protocol / endpoint / auth / key. Focus lands on the
    /// last field, which doesn't matter for commit (commit reads each
    /// field by name) but keeps the snapshot identical to one you'd get
    /// by typing through the whole form.
    fn test_snapshot_with_compose_full_draft(
        name: &str,
        protocol: &str,
        endpoint: &str,
        auth: AuthScheme,
        key: &str,
    ) -> SettingsSnapshot {
        let mut snap = test_snapshot_with_compose_state(name, "key");
        snap.insert(
            &oxpath!("ui", "settings", "new_account", "protocol"),
            Value::String(protocol.into()),
        );
        snap.insert(
            &oxpath!("ui", "settings", "new_account", "endpoint"),
            Value::String(endpoint.into()),
        );
        snap.insert(
            &oxpath!("ui", "settings", "new_account", "auth"),
            to_value(&auth).unwrap(),
        );
        snap.insert(
            &oxpath!("ui", "settings", "new_account", "key"),
            Value::String(key.into()),
        );
        snap
    }

    #[test]
    fn compose_commit_with_errors_is_noop() {
        // Empty name → validation fails (name "required"); other fields
        // also fail. The contract: commit produces zero writes — no
        // partial record, no banner. The renderer surfaces errors via
        // the live `errors` record that keystrokes maintain.
        let mut snap = test_snapshot_with_compose_state("", "name");
        let writes = run_cmd(&AccountsComposeCommit::new(), &mut snap);
        assert!(
            writes.is_empty(),
            "commit must no-op when errors present; got {writes:?}",
        );
    }

    #[test]
    fn compose_commit_writes_per_account_provider_record_with_valid_xid_name() {
        // "personal" is already a valid XID — namecode encodes it to
        // itself, so the path_id equals the display name. This is the
        // common case and pins that the encoding is idempotent on valid
        // identifiers.
        let mut snap = test_snapshot_with_compose_full_draft(
            "personal",
            "anthropic",
            "https://api.example.com",
            AuthScheme::XApiKey,
            "sk-xxx",
        );
        let writes = run_cmd(&AccountsComposeCommit::new(), &mut snap);

        // Account record at the per-account path.
        let acct_val = writes_value(&writes, "config/gate/accounts/personal")
            .expect("account record written");
        let acct: AccountConfig =
            structfs_serde_store::from_value(acct_val).expect("decode AccountConfig");
        assert_eq!(acct.provider, "personal");
        assert_eq!(acct.display_name.as_deref(), Some("personal"));

        // Provider record at the SAME per-account name (NOT shared
        // `anthropic`). This is the bug-fix invariant: distinct accounts
        // get distinct provider records, named after the account.
        let prov_val = writes_value(&writes, "config/gate/providers/personal")
            .expect("provider record written");
        let prov: ProviderConfig =
            structfs_serde_store::from_value(prov_val).expect("decode ProviderConfig");
        assert_eq!(prov.dialect, "anthropic");
        assert_eq!(prov.endpoint, "https://api.example.com");
        assert_eq!(prov.auth, Some(AuthScheme::XApiKey));

        // The shared-anthropic provider record MUST NOT be touched.
        assert!(
            writes_value(&writes, "config/gate/providers/anthropic").is_none(),
            "compose commit must NOT touch the shared anthropic provider"
        );

        // API key written under the per-account name (x-api-key
        // requires a key, so the key write is present).
        let key_val = writes_value(&writes, "secret/keys/personal")
            .expect("api key written for x-api-key auth");
        assert_eq!(key_val, Value::String("sk-xxx".into()));

        // Compose state cleared at the subtree root (cascade Null-write).
        assert_eq!(
            writes_value(&writes, "ui/settings/new_account"),
            Some(Value::Null),
            "draft state cleared by subtree cascade",
        );

        // Focus moves to the new account row (path-array form).
        let focused_val = writes_value(&writes, "ui/settings/focused")
            .expect("focused path written");
        let focused_path = super::super::navigation::path_from_value(&focused_val)
            .expect("focused value decodes as Path");
        assert_eq!(
            focused_path,
            oxpath!("settings", "accounts", "personal"),
            "focused row should be the new account",
        );
    }

    #[test]
    fn compose_commit_namecodes_non_xid_display_name() {
        // "my-personal" is NOT a valid XID (hyphen); namecode encodes
        // it. The on-disk records land at the encoded path component,
        // but the user-visible display name preserves the original
        // input via `AccountConfig.display_name`.
        let mut snap = test_snapshot_with_compose_full_draft(
            "my-personal",
            "anthropic",
            "https://api.example.com",
            AuthScheme::XApiKey,
            "sk-xxx",
        );
        let writes = run_cmd(&AccountsComposeCommit::new(), &mut snap);

        let path_id = namecode::encode("my-personal");
        assert_ne!(
            path_id, "my-personal",
            "hyphen must force encoding; if namecode is idempotent here \
             this test no longer exercises the encoding path"
        );

        let acct_path = format!("config/gate/accounts/{path_id}");
        let acct_val = writes_value(&writes, &acct_path)
            .expect("account record at encoded path");
        let acct: AccountConfig =
            structfs_serde_store::from_value(acct_val).expect("decode AccountConfig");
        // provider points at the encoded path_id (so the provider
        // record sits alongside the account record at the same name).
        assert_eq!(acct.provider, path_id);
        // display_name preserves the original Unicode-rich input.
        assert_eq!(acct.display_name.as_deref(), Some("my-personal"));

        let provider_path = format!("config/gate/providers/{path_id}");
        assert!(
            writes_value(&writes, &provider_path).is_some(),
            "provider record written at encoded path {provider_path}",
        );

        let key_path = format!("secret/keys/{path_id}");
        assert!(
            writes_value(&writes, &key_path).is_some(),
            "api key written at encoded path {key_path}",
        );
    }

    /// Build a snapshot in compose mode with the focused text field
    /// pre-populated with `name_value`, and the cursor at the form
    /// field named by `focused_field_name` (the snake_case wire name
    /// for an `AccountField`). All other compose-mode fields are
    /// initialized to their open-state defaults (empty for text, Null
    /// for selectors). The cursor's path encodes which field is
    /// focused; no separate `focused_field` discriminator exists.
    fn test_snapshot_with_compose_state(
        name_value: &str,
        focused_field_name: &str,
    ) -> SettingsSnapshot {
        let mut snap = SettingsSnapshot::empty();
        let comp = ox_kernel::PathComponent::try_new(focused_field_name)
            .expect("focused_field_name must be a valid identifier");
        let focused_path = oxpath!("settings", "_compose_form", comp);
        snap.insert(
            &oxpath!("ui", "settings", "focused"),
            super::super::navigation::path_to_value(&focused_path),
        );
        snap.insert(
            &oxpath!("ui", "settings", "new_account", "name"),
            Value::String(name_value.into()),
        );
        snap.insert(
            &oxpath!("ui", "settings", "new_account", "protocol"),
            Value::Null,
        );
        snap.insert(
            &oxpath!("ui", "settings", "new_account", "endpoint"),
            Value::String(String::new()),
        );
        snap.insert(
            &oxpath!("ui", "settings", "new_account", "auth"),
            Value::Null,
        );
        snap.insert(
            &oxpath!("ui", "settings", "new_account", "key"),
            Value::String(String::new()),
        );
        snap
    }

    /// Build a snapshot in compose mode with cursor at the form field
    /// named by `focused_field_name` and all field buffers at their
    /// open-state defaults.
    fn test_snapshot_with_compose_state_focus(focused_field_name: &str) -> SettingsSnapshot {
        test_snapshot_with_compose_state("", focused_field_name)
    }

    /// Owned `CommandCtx` builder that carries a `Char` keystroke. The
    /// returned tuple keeps the renderer registry alive for the borrow
    /// duration of the context.
    fn ctx_with_char(ch: char) -> (RendererRegistry, KeyChord) {
        let registry = RendererRegistry::new();
        let chord = KeyChord {
            modifiers: KeyModifierSet::default(),
            code: KeyCodeRepr::Char(ch),
        };
        (registry, chord)
    }

    #[test]
    fn compose_insert_char_appends_to_focused_text_field() {
        let mut snap = test_snapshot_with_compose_state("my", "name");
        let (registry, chord) = ctx_with_char('p');
        let ctx = CommandCtx {
            registry: &registry,
            last_keystroke: Some(chord),
        };
        let writes = AccountsComposeInsertChar::new().run(&mut snap, &ctx);

        assert_eq!(
            writes_value(&writes, "ui/settings/new_account/name"),
            Some(Value::String("myp".into())),
        );
        // Errors recomputed on every keystroke.
        assert!(
            writes_value(&writes, "ui/settings/new_account/errors").is_some(),
            "errors must be recomputed per-keystroke"
        );
        // Legacy single-field buffer must NOT be written.
        assert!(
            writes_value(&writes, "ui/settings/new_account/buffer").is_none(),
            "legacy buffer must not be written"
        );
    }

    #[test]
    fn compose_insert_char_noop_on_selector_focus() {
        let mut snap = test_snapshot_with_compose_state_focus("protocol");
        let (registry, chord) = ctx_with_char('p');
        let ctx = CommandCtx {
            registry: &registry,
            last_keystroke: Some(chord),
        };
        let writes = AccountsComposeInsertChar::new().run(&mut snap, &ctx);
        assert!(writes.is_empty(), "should be no-op on selector field");
    }

    #[test]
    fn compose_insert_char_recomputes_errors_per_keystroke() {
        let mut snap = test_snapshot_with_compose_state("", "name");
        let (registry, chord) = ctx_with_char('f');
        let ctx = CommandCtx {
            registry: &registry,
            last_keystroke: Some(chord),
        };
        let writes = AccountsComposeInsertChar::new().run(&mut snap, &ctx);

        let errors_val = writes_value(&writes, "ui/settings/new_account/errors")
            .expect("errors written");
        let errors: ValidationErrors =
            structfs_serde_store::from_value(errors_val).unwrap();
        // After typing one valid char, name is no longer "required".
        assert_eq!(errors.name, None);
    }

    #[test]
    fn compose_delete_back_pops_focused_text_field() {
        let mut snap = test_snapshot_with_compose_state("myacc", "name");
        let writes = run_cmd(&AccountsComposeDeleteBack::new(), &mut snap);
        assert_eq!(
            writes_value(&writes, "ui/settings/new_account/name"),
            Some(Value::String("myac".into())),
        );
        // Errors recomputed after popping.
        assert!(
            writes_value(&writes, "ui/settings/new_account/errors").is_some(),
            "errors must be recomputed after delete_back"
        );
        // Legacy single-field buffer must NOT be written.
        assert!(
            writes_value(&writes, "ui/settings/new_account/buffer").is_none(),
            "legacy buffer must not be written"
        );
    }

    #[test]
    fn compose_delete_back_on_empty_is_noop() {
        let mut snap = test_snapshot_with_compose_state("", "name");
        let writes = run_cmd(&AccountsComposeDeleteBack::new(), &mut snap);
        // Empty buffer: nothing to pop, no writes.
        assert!(writes.is_empty());
    }

    #[test]
    fn compose_delete_back_noop_on_selector_focus() {
        let mut snap = test_snapshot_with_compose_state_focus("protocol");
        let writes = run_cmd(&AccountsComposeDeleteBack::new(), &mut snap);
        assert!(writes.is_empty());
    }

    #[test]
    fn compose_cancel_writes_null_to_new_account_root() {
        let mut snap = test_snapshot_with_compose_state("partial", "name");
        let writes = run_cmd(&AccountsComposeCancel::new(), &mut snap);

        // Two writes now: restore cursor + Null at the subtree root.
        // The Null write cascades to clear every child (including
        // `cursor_saved`) in one entry.
        assert_eq!(writes.len(), 2);
        assert!(writes.iter().any(|w| {
            w.path == oxpath!("ui", "settings", "new_account")
                && matches!(&w.record, Record::Parsed(Value::Null))
        }));
        assert!(writes.iter().any(|w| {
            w.path == oxpath!("ui", "settings", "focused")
        }));
    }

    #[test]
    fn compose_cancel_restores_saved_cursor() {
        // After open writes `cursor_saved`, cancel reads it and writes
        // it back at `ui/settings/focused`. The save survives the
        // pre-cancel snapshot (the Null write applies later in the
        // batch, after the read here).
        let mut snap = test_snapshot_with_compose_state("partial", "name");
        snap.insert(
            &oxpath!("ui", "settings", "new_account", "cursor_saved"),
            super::super::navigation::path_to_value(&oxpath!(
                "settings",
                "accounts",
                "alpha"
            )),
        );
        let writes = run_cmd(&AccountsComposeCancel::new(), &mut snap);
        let focused_val = writes_value(&writes, "ui/settings/focused")
            .expect("cursor restored");
        let focused = super::super::navigation::path_from_value(&focused_val).unwrap();
        assert_eq!(focused, oxpath!("settings", "accounts", "alpha"));
    }

    #[test]
    fn compose_cancel_falls_back_to_accounts_when_no_save() {
        // Pathological seed (no `cursor_saved`): cancel still restores
        // a sensible cursor so the user doesn't get stranded.
        let mut snap = test_snapshot_with_compose_state("partial", "name");
        let writes = run_cmd(&AccountsComposeCancel::new(), &mut snap);
        let focused_val = writes_value(&writes, "ui/settings/focused")
            .expect("cursor written");
        let focused = super::super::navigation::path_from_value(&focused_val).unwrap();
        assert_eq!(focused, oxpath!("settings", "accounts"));
    }

    // -- compose.cycle_forward / cycle_back --------------------------------------

    /// `test_snapshot_with_compose_state_focus("protocol")` plus a typed
    /// protocol value at `ui/settings/new_account/protocol`.
    fn test_snapshot_with_compose_protocol(protocol: &str) -> SettingsSnapshot {
        let mut snap = test_snapshot_with_compose_state_focus("protocol");
        snap.insert(
            &oxpath!("ui", "settings", "new_account", "protocol"),
            Value::String(protocol.into()),
        );
        snap
    }

    /// `test_snapshot_with_compose_state_focus("auth")` plus a typed
    /// auth scheme value at `ui/settings/new_account/auth`.
    fn test_snapshot_with_compose_auth(auth: AuthScheme) -> SettingsSnapshot {
        let mut snap = test_snapshot_with_compose_state_focus("auth");
        snap.insert(
            &oxpath!("ui", "settings", "new_account", "auth"),
            to_value(&auth).unwrap(),
        );
        snap
    }

    #[test]
    fn cycle_forward_picks_first_option_when_none_selected() {
        let mut snap = test_snapshot_with_compose_state_focus("protocol");
        let writes = run_cmd(&AccountsComposeCycleForward::new(), &mut snap);

        assert_eq!(
            writes_value(&writes, "ui/settings/new_account/protocol"),
            Some(Value::String(PROTOCOL_OPTIONS[0].into())),
        );
    }

    #[test]
    fn cycle_forward_advances_among_protocol_options() {
        let mut snap = test_snapshot_with_compose_protocol(PROTOCOL_OPTIONS[0]);
        let writes = run_cmd(&AccountsComposeCycleForward::new(), &mut snap);

        assert_eq!(
            writes_value(&writes, "ui/settings/new_account/protocol"),
            Some(Value::String(PROTOCOL_OPTIONS[1].into())),
        );
    }

    #[test]
    fn cycle_forward_wraps_protocol() {
        let last = *PROTOCOL_OPTIONS.last().unwrap();
        let mut snap = test_snapshot_with_compose_protocol(last);
        let writes = run_cmd(&AccountsComposeCycleForward::new(), &mut snap);

        assert_eq!(
            writes_value(&writes, "ui/settings/new_account/protocol"),
            Some(Value::String(PROTOCOL_OPTIONS[0].into())),
        );
    }

    #[test]
    fn cycle_back_retreats_among_auth_options() {
        let mut snap = test_snapshot_with_compose_auth(AuthScheme::ALL[1].clone());
        let writes = run_cmd(&AccountsComposeCycleBack::new(), &mut snap);

        let written = writes_value(&writes, "ui/settings/new_account/auth")
            .and_then(|v| structfs_serde_store::from_value::<AuthScheme>(v).ok());
        assert_eq!(written, Some(AuthScheme::ALL[0].clone()));
    }

    /// Regression: cycling a selector must clear the "select a …" error
    /// for that field in the SAME write batch. The pre-cycle snapshot has
    /// `protocol = Null`, so the snapshot-only validator would re-emit
    /// `errors.protocol = Some("required")` even after the cycle write.
    /// `recompute_errors_writes` must read the just-emitted protocol
    /// write out of `writes` and validate against that value.
    #[test]
    fn cycle_clears_pre_cycle_protocol_error() {
        let mut snap = test_snapshot_with_compose_state_focus("protocol");
        let writes = run_cmd(&AccountsComposeCycleForward::new(), &mut snap);

        // Protocol was written by the cycle command.
        assert_eq!(
            writes_value(&writes, "ui/settings/new_account/protocol"),
            Some(Value::String(PROTOCOL_OPTIONS[0].into())),
        );

        // Errors were recomputed against the new value, not the
        // pre-cycle snapshot (which had protocol = Null).
        let errors_val = writes_value(&writes, "ui/settings/new_account/errors")
            .expect("errors recomputed after cycle");
        let errors: ValidationErrors =
            structfs_serde_store::from_value(errors_val).unwrap();
        assert_eq!(
            errors.protocol, None,
            "after cycling protocol, the stale 'select a protocol' error must clear",
        );
    }

    /// Same regression check on the Auth selector — the cycle write goes
    /// through `to_value(&AuthScheme)` (an enum-as-string), and
    /// `recompute_errors_writes` must round-trip that back through
    /// `from_value` to validate.
    #[test]
    fn cycle_clears_pre_cycle_auth_error() {
        let mut snap = test_snapshot_with_compose_state_focus("auth");
        let writes = run_cmd(&AccountsComposeCycleForward::new(), &mut snap);

        let errors_val = writes_value(&writes, "ui/settings/new_account/errors")
            .expect("errors recomputed after cycle");
        let errors: ValidationErrors =
            structfs_serde_store::from_value(errors_val).unwrap();
        assert_eq!(
            errors.auth, None,
            "after cycling auth, the stale 'select an auth scheme' error must clear",
        );
    }

    // -- compose.focus_next / focus_prev ----------------------------------------

    #[test]
    fn focus_next_command_advances_cursor() {
        let mut snap = test_snapshot_with_compose_state_focus("name");
        let writes = accounts_compose_focus_next(&mut snap);
        let focused_val = writes_value(&writes, "ui/settings/focused")
            .expect("cursor written");
        let focused = super::super::navigation::path_from_value(&focused_val).unwrap();
        assert_eq!(focused, oxpath!("settings", "_compose_form", "protocol"));
    }

    #[test]
    fn focus_prev_command_retreats_cursor() {
        let mut snap = test_snapshot_with_compose_state_focus("name");
        let writes = accounts_compose_focus_prev(&mut snap);
        // Wraps to Key (FIELD_ORDER is Name → Protocol → Endpoint → Auth → Key → Name).
        let focused_val = writes_value(&writes, "ui/settings/focused")
            .expect("cursor written");
        let focused = super::super::navigation::path_from_value(&focused_val).unwrap();
        assert_eq!(focused, oxpath!("settings", "_compose_form", "key"));
    }

    #[test]
    fn focus_change_only_writes_cursor() {
        let mut snap = test_snapshot_with_compose_state("abc", "name");
        let writes = accounts_compose_focus_next(&mut snap);
        // Only the cursor should change; no other state touched
        // (no error recompute either).
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "settings", "focused"));
    }

    // -- compose hierarchical dispatch (capture / target / bubble) --------------
    //
    // These tests pin the user-visible behavior of the three-phase
    // compose dispatcher: `h` / `l` are inserted as literal characters
    // when a text field is focused (target-phase Text binding) and
    // cycle the selector when a selector field is focused (target-phase
    // Selector binding). `Esc` cancels regardless of focused field
    // (capture-phase form binding).
    //
    // `simulate_compose_keystroke` wires the production binding +
    // command registries (`bindings::register` / `commands::register_all`)
    // against `dispatch_settings_key`, so any binding-side regression
    // shows up directly.
    fn simulate_compose_keystroke(snap: &mut SettingsSnapshot, code: KeyCodeRepr) -> Vec<Write> {
        let mut cmds = CommandRegistry::new();
        crate::settings::commands::register_all(&mut cmds);
        let mut bindings = crate::settings::binding_registry::BindingRegistry::new();
        crate::settings::bindings::register(&mut bindings);
        let renderers = RendererRegistry::new();
        // Match terminal behavior: uppercase ASCII letters arrive with
        // the shift modifier set; other keys arrive with no modifiers.
        let modifiers = match code {
            KeyCodeRepr::Char(c) if c.is_ascii_uppercase() => KeyModifierSet {
                shift: true,
                ..KeyModifierSet::default()
            },
            _ => KeyModifierSet::default(),
        };
        let chord = KeyChord { modifiers, code };
        crate::settings::dispatch::dispatch_settings_key(
            snap,
            Screen::Settings,
            &oxpath!("settings", "accounts"),
            None,
            &chord,
            &cmds,
            &bindings,
            &renderers,
        )
    }

    #[test]
    fn h_inserted_when_text_field_focused() {
        let mut snap = test_snapshot_with_compose_state("", "name");
        let writes = simulate_compose_keystroke(&mut snap, KeyCodeRepr::Char('h'));
        // Target-phase: the leaf Text binding for `h` is insert_char,
        // not cycle_back. The char lands in the Name buffer.
        assert_eq!(
            writes_value(&writes, "ui/settings/new_account/name"),
            Some(Value::String("h".into())),
        );
    }

    #[test]
    fn h_cycles_back_when_selector_focused() {
        let mut snap = test_snapshot_with_compose_protocol(PROTOCOL_OPTIONS[1]);
        let writes = simulate_compose_keystroke(&mut snap, KeyCodeRepr::Char('h'));
        assert_eq!(
            writes_value(&writes, "ui/settings/new_account/protocol"),
            Some(Value::String(PROTOCOL_OPTIONS[0].into())),
        );
    }

    #[test]
    fn l_inserted_when_text_field_focused() {
        let mut snap = test_snapshot_with_compose_state("", "name");
        let writes = simulate_compose_keystroke(&mut snap, KeyCodeRepr::Char('l'));
        assert_eq!(
            writes_value(&writes, "ui/settings/new_account/name"),
            Some(Value::String("l".into())),
        );
    }

    #[test]
    fn l_cycles_forward_when_selector_focused() {
        let mut snap = test_snapshot_with_compose_protocol(PROTOCOL_OPTIONS[0]);
        let writes = simulate_compose_keystroke(&mut snap, KeyCodeRepr::Char('l'));
        assert_eq!(
            writes_value(&writes, "ui/settings/new_account/protocol"),
            Some(Value::String(PROTOCOL_OPTIONS[1].into())),
        );
    }

    #[test]
    fn esc_cancels_regardless_of_focused_field() {
        // Esc is form-scope capture: must fire even when focused on a
        // text field that could theoretically have bound Esc at target.
        // Today no field binds Esc; this test pins the contract.
        for field in ["name", "protocol", "endpoint", "auth", "key"] {
            let mut snap = test_snapshot_with_compose_state_focus(field);
            let writes = simulate_compose_keystroke(&mut snap, KeyCodeRepr::Esc);
            // Cancel emits two writes: restore cursor + Null at the
            // new_account root.
            assert_eq!(
                writes.len(),
                2,
                "field {field}: expected two writes, got {writes:?}",
            );
            assert!(
                writes.iter().any(|w| {
                    w.path == oxpath!("ui", "settings", "new_account")
                        && matches!(&w.record, Record::Parsed(Value::Null))
                }),
                "field {field}: expected Null at new_account root",
            );
        }
    }

    #[test]
    fn delete_confirm_open_writes_cursor_to_confirm_delete() {
        // Cursor-as-focus: opening writes `ui/settings/focused =
        // settings/_confirm_delete`. The cursor IS the open state.
        let mut snap = SettingsSnapshot::empty();
        select_account(&mut snap, "alpha");
        let writes = run_cmd(&AccountsDeleteConfirm::new(), &mut snap);
        let focused_write = writes
            .iter()
            .find(|w| w.path == oxpath!("ui", "settings", "focused"))
            .expect("cursor written");
        let cursor = super::super::navigation::path_from_value(
            focused_write.record.as_value().unwrap(),
        )
        .expect("decode cursor");
        assert_eq!(cursor, oxpath!("settings", "_confirm_delete"));
        // The retired value-flag must not be written.
        assert!(
            !writes
                .iter()
                .any(|w| w.path == oxpath!("ui", "settings", "pending_delete")),
            "retired value-flag must not be written at the pending_delete root: {writes:?}",
        );
    }

    #[test]
    fn delete_confirm_open_saves_target_account_and_cursor() {
        // Open seeds two data paths: `target_account` (the account name
        // being confirmed — the value half of the retired value-flag)
        // and `cursor_saved` (the prior cursor, so cancel can restore
        // it).
        let mut snap = SettingsSnapshot::empty();
        select_account(&mut snap, "alpha");
        let prior = oxpath!("settings", "accounts");
        snap.insert(
            &oxpath!("ui", "settings", "focused"),
            path_to_value(&prior),
        );
        let writes = run_cmd(&AccountsDeleteConfirm::new(), &mut snap);

        let target_write = writes
            .iter()
            .find(|w| w.path == oxpath!("ui", "settings", "pending_delete", "target_account"))
            .expect("target_account written");
        match &target_write.record {
            Record::Parsed(Value::String(s)) => assert_eq!(s, "alpha"),
            other => panic!("expected target_account = \"alpha\"; got {other:?}"),
        }

        let saved_write = writes
            .iter()
            .find(|w| w.path == oxpath!("ui", "settings", "pending_delete", "cursor_saved"))
            .expect("cursor_saved written");
        let saved =
            super::super::navigation::path_from_value(saved_write.record.as_value().unwrap())
                .expect("decode saved cursor");
        assert_eq!(saved, prior);
    }

    #[test]
    fn accounts_delete_confirm_inert_without_selection() {
        let mut snap = SettingsSnapshot::empty();
        let writes = run_cmd(&AccountsDeleteConfirm::new(), &mut snap);
        assert!(writes.is_empty());
    }

    #[test]
    fn confirm_cancel_restores_saved_cursor() {
        // Cancel reads `cursor_saved` and restores the cursor; the
        // Null-cascade write applies after the read, so the snapshot
        // here still holds the saved value.
        let mut snap = SettingsSnapshot::empty();
        let prior = oxpath!("settings", "accounts");
        snap.insert(
            &oxpath!("ui", "settings", "pending_delete", "cursor_saved"),
            path_to_value(&prior),
        );
        let writes = run_cmd(&AccountsConfirmCancel::new(), &mut snap);
        let cursor_val = writes
            .iter()
            .find(|w| w.path == oxpath!("ui", "settings", "focused"))
            .map(|w| w.record.as_value().unwrap().clone())
            .expect("cursor restored");
        let cursor = super::super::navigation::path_from_value(&cursor_val).unwrap();
        assert_eq!(cursor, prior);
        // Cascade-clear the entire pending_delete subtree.
        assert_null_write(&writes, oxpath!("ui", "settings", "pending_delete"));
    }

    #[test]
    fn confirm_cancel_falls_back_to_accounts_when_no_save() {
        // Pathological seed (no cursor_saved): cancel still restores a
        // sensible cursor so the user doesn't get stranded.
        let mut snap = SettingsSnapshot::empty();
        let writes = run_cmd(&AccountsConfirmCancel::new(), &mut snap);
        let cursor_val = writes
            .iter()
            .find(|w| w.path == oxpath!("ui", "settings", "focused"))
            .map(|w| w.record.as_value().unwrap().clone())
            .expect("cursor written");
        let cursor = super::super::navigation::path_from_value(&cursor_val).unwrap();
        assert_eq!(cursor, oxpath!("settings", "accounts"));
    }

    #[test]
    fn confirm_delete_clears_pending_delete_subtree() {
        // Confirm-delete emits three writes: the account Null at the
        // canonical path, the cursor restore, and a Null-cascade at
        // the pending_delete subtree root (clears target_account +
        // cursor_saved in one entry).
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "pending_delete", "target_account"),
            Value::String("alpha".into()),
        );
        let prior = oxpath!("settings", "accounts");
        snap.insert(
            &oxpath!("ui", "settings", "pending_delete", "cursor_saved"),
            path_to_value(&prior),
        );
        let writes = run_cmd(&AccountsConfirmDelete::new(), &mut snap);
        // Account Null write at config/gate/accounts/alpha.
        let comp = ox_kernel::PathComponent::try_new("alpha").unwrap();
        assert_null_write(&writes, oxpath!("config", "gate", "accounts", comp));
        // Cursor restored to the saved pre-open path.
        let cursor_val = writes
            .iter()
            .find(|w| w.path == oxpath!("ui", "settings", "focused"))
            .map(|w| w.record.as_value().unwrap().clone())
            .expect("cursor restored");
        let cursor = super::super::navigation::path_from_value(&cursor_val).unwrap();
        assert_eq!(cursor, prior);
        // Cascade-clear at the pending_delete subtree root.
        assert_null_write(&writes, oxpath!("ui", "settings", "pending_delete"));
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

    /// Seed `ui/settings/focused = settings/_manual_model/<stage>` —
    /// the cursor IS the stage discriminator under cursor-as-focus.
    fn seed_manual_stage(snap: &mut SettingsSnapshot, stage: ManualModelStage) {
        let focus_path = manual_model_focus_path(stage);
        snap.insert(
            &oxpath!("ui", "settings", "focused"),
            super::super::navigation::path_to_value(&focus_path),
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
                ..Default::default()
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
        // Cursor-as-focus: opening writes cursor to the Id-stage path.
        // No `manual_model/stage` write — the cursor IS the stage.
        let focused_val = by_path
            .get("ui/settings/focused")
            .expect("cursor written");
        let focused = super::super::navigation::path_from_value(focused_val)
            .expect("cursor value decodes as Path");
        assert_eq!(
            focused,
            oxpath!("settings", "_manual_model", "id"),
            "open writes cursor to the Id stage",
        );
        assert!(
            !by_path.contains_key("ui/settings/manual_model/stage"),
            "retired discriminator must not be written",
        );
        assert_eq!(
            by_path.get("ui/settings/manual_model/buffer").unwrap(),
            &Value::String(String::new())
        );
    }

    #[test]
    fn models_add_manual_saves_prior_cursor() {
        // Before open, the user's cursor sits at a Model row. After
        // open, the prior cursor lives at
        // `ui/settings/manual_model/cursor_saved` so cancel can restore
        // it. (Commit doesn't restore — it writes cursor to the new
        // model's row directly.)
        use ox_gate::ModelInfo;
        use ox_types::ModelInfoSource;
        let mut snap = SettingsSnapshot::empty();
        write_index_entries_for_manual(&mut snap);
        let comp = ox_kernel::PathComponent::try_new("alpha").unwrap();
        snap.insert(
            &oxpath!("config", "gate", "accounts", comp.clone()),
            to_value(&AccountConfig {
                provider: "openai".into(),
                ..Default::default()
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
        let prior_cursor = oxpath!("settings", "models", comp, m_comp);
        snap.insert(
            &oxpath!("ui", "settings", "focused"),
            path_to_value(&prior_cursor),
        );

        let writes = run_cmd(&ModelsAddManual::new(), &mut snap);
        let saved_val = writes
            .iter()
            .find(|w| w.path == oxpath!("ui", "settings", "manual_model", "cursor_saved"))
            .map(|w| w.record.as_value().unwrap().clone())
            .expect("cursor_saved written");
        let saved =
            super::super::navigation::path_from_value(&saved_val).expect("decode saved cursor");
        assert_eq!(saved, prior_cursor);
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
        // Cursor advances to the Ctx-stage path.
        let focused_val = by_path
            .get("ui/settings/focused")
            .expect("cursor advanced");
        let focused = super::super::navigation::path_from_value(focused_val).unwrap();
        assert_eq!(focused, oxpath!("settings", "_manual_model", "ctx"));
        assert!(
            !by_path.contains_key("ui/settings/manual_model/stage"),
            "retired discriminator must not be written",
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
        // Cursor advances to the Out-stage path.
        let focused_val = by_path
            .get("ui/settings/focused")
            .expect("cursor advanced");
        let focused = super::super::navigation::path_from_value(focused_val).unwrap();
        assert_eq!(focused, oxpath!("settings", "_manual_model", "out"));
        assert!(
            !by_path.contains_key("ui/settings/manual_model/stage"),
            "retired discriminator must not be written",
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
        // Single Null write at the manual_model subtree root; the
        // StructFS Null-delete cascade clears every child atomically.
        assert_null_write(&writes, oxpath!("ui", "settings", "manual_model"));
        // Cursor moves to the new model row.
        let cursor_write = writes
            .iter()
            .find(|w| w.path == oxpath!("ui", "settings", "focused"))
            .expect("cursor write to new model row");
        let cursor = super::super::navigation::path_from_value(
            cursor_write.record.as_value().unwrap(),
        )
        .unwrap();
        assert_eq!(
            cursor,
            oxpath!("settings", "models", "alpha", "custom_model"),
            "commit should focus the new model's row",
        );
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
        // No catalog write.
        assert!(
            !writes
                .iter()
                .any(|w| w.path.to_string().starts_with("config/gate/accounts"))
        );
        // Single Null write at the manual_model root cascades to clear
        // every child.
        assert_null_write(&writes, oxpath!("ui", "settings", "manual_model"));
    }

    #[test]
    fn models_manual_cancel_restores_saved_cursor() {
        // After open writes `cursor_saved`, cancel reads it and writes
        // it back at `ui/settings/focused`. The save survives the
        // pre-cancel snapshot (the Null write applies later in the
        // batch, after the read here).
        let mut snap = SettingsSnapshot::empty();
        seed_manual_stage(&mut snap, ManualModelStage::Ctx);
        let prior = oxpath!("settings", "models", "alpha", "m1");
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "cursor_saved"),
            super::super::navigation::path_to_value(&prior),
        );
        let writes = run_cmd(&ModelsManualCancel::new(), &mut snap);
        let cursor_val = writes
            .iter()
            .find(|w| w.path == oxpath!("ui", "settings", "focused"))
            .map(|w| w.record.as_value().unwrap().clone())
            .expect("cursor restored");
        let cursor = super::super::navigation::path_from_value(&cursor_val).unwrap();
        assert_eq!(cursor, prior);
    }

    #[test]
    fn models_manual_cancel_falls_back_to_models_when_no_save() {
        // Pathological seed (no `cursor_saved`): cancel still restores a
        // sensible cursor so the user doesn't get stranded.
        let mut snap = SettingsSnapshot::empty();
        seed_manual_stage(&mut snap, ManualModelStage::Id);
        let writes = run_cmd(&ModelsManualCancel::new(), &mut snap);
        let cursor_val = writes
            .iter()
            .find(|w| w.path == oxpath!("ui", "settings", "focused"))
            .map(|w| w.record.as_value().unwrap().clone())
            .expect("cursor written");
        let cursor = super::super::navigation::path_from_value(&cursor_val).unwrap();
        assert_eq!(cursor, oxpath!("settings", "models"));
    }

    #[test]
    fn manual_model_open_writes_cursor_to_id_stage() {
        // Pin the cursor-as-focus invariant: open writes
        // `ui/settings/focused = settings/_manual_model/id`. Anchors
        // CF-2's "cursor IS the stage" rewrite for future readers.
        use ox_gate::ModelInfo;
        use ox_types::ModelInfoSource;
        let mut snap = SettingsSnapshot::empty();
        write_index_entries_for_manual(&mut snap);
        let comp = ox_kernel::PathComponent::try_new("alpha").unwrap();
        snap.insert(
            &oxpath!("config", "gate", "accounts", comp.clone()),
            to_value(&AccountConfig {
                provider: "openai".into(),
                ..Default::default()
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
        let focused_val = writes
            .iter()
            .find(|w| w.path == oxpath!("ui", "settings", "focused"))
            .map(|w| w.record.as_value().unwrap().clone())
            .expect("focused written");
        let focused =
            super::super::navigation::path_from_value(&focused_val).expect("decode focused path");
        assert_eq!(focused, oxpath!("settings", "_manual_model", "id"));
    }

    #[test]
    fn models_manual_stage_advance_moves_cursor() {
        // Pin the cursor-as-focus invariant for stage advance: starting
        // at Id, commit advances cursor to Ctx; from Ctx to Out. No
        // `manual_model/stage` writes.
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "account"),
            Value::String("alpha".into()),
        );
        seed_manual_stage(&mut snap, ManualModelStage::Id);
        snap.insert(
            &oxpath!("ui", "settings", "manual_model", "buffer"),
            Value::String("model-x".into()),
        );
        let writes = run_cmd(&ModelsManualCommit::new(), &mut snap);
        let focused_val = writes
            .iter()
            .find(|w| w.path == oxpath!("ui", "settings", "focused"))
            .map(|w| w.record.as_value().unwrap().clone())
            .expect("cursor advanced");
        let focused = super::super::navigation::path_from_value(&focused_val).unwrap();
        assert_eq!(focused, oxpath!("settings", "_manual_model", "ctx"));
        assert!(
            !writes
                .iter()
                .any(|w| w.path == oxpath!("ui", "settings", "manual_model", "stage")),
            "stage discriminator must not be written",
        );
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
    fn models_manual_insert_char_no_op_without_cursor_at_stage() {
        // Without a cursor at a manual-model stage path the insert-char
        // must be a no-op — the dispatcher's gating already prevents it
        // from firing, but the helper itself also short-circuits as a
        // defense in depth.
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

    #[test]
    fn field_label_matches_variant() {
        use ox_types::settings::AccountField;
        assert_eq!(field_label(AccountField::Name), "Name");
        assert_eq!(field_label(AccountField::Protocol), "Protocol");
        assert_eq!(field_label(AccountField::Endpoint), "Endpoint");
        assert_eq!(field_label(AccountField::Auth), "Auth");
        assert_eq!(field_label(AccountField::Key), "Key");
    }

    #[test]
    fn field_kind_separates_text_from_selector() {
        use ox_types::settings::AccountField;
        assert_eq!(field_kind(AccountField::Name), FieldKind::Text);
        assert_eq!(field_kind(AccountField::Endpoint), FieldKind::Text);
        assert_eq!(field_kind(AccountField::Key), FieldKind::Text);
        assert_eq!(field_kind(AccountField::Protocol), FieldKind::Selector);
        assert_eq!(field_kind(AccountField::Auth), FieldKind::Selector);
    }

    #[test]
    fn field_state_subpath_matches_spec() {
        use ox_types::settings::AccountField;
        assert_eq!(field_state_subpath(AccountField::Name), "name");
        assert_eq!(field_state_subpath(AccountField::Protocol), "protocol");
        assert_eq!(field_state_subpath(AccountField::Endpoint), "endpoint");
        assert_eq!(field_state_subpath(AccountField::Auth), "auth");
        assert_eq!(field_state_subpath(AccountField::Key), "key");
    }

    #[test]
    fn field_order_lists_every_variant_exactly_once() {
        use std::collections::HashSet;
        use ox_types::settings::AccountField;
        let seen: HashSet<_> = FIELD_ORDER.iter().copied().collect();
        assert_eq!(seen.len(), FIELD_ORDER.len(), "FIELD_ORDER has duplicates");
        for v in [
            AccountField::Name,
            AccountField::Protocol,
            AccountField::Endpoint,
            AccountField::Auth,
            AccountField::Key,
        ] {
            assert!(seen.contains(&v), "FIELD_ORDER missing {v:?}");
        }
    }

    #[test]
    fn focus_next_walks_field_order() {
        use ox_types::settings::AccountField;
        let walk: Vec<_> = std::iter::successors(
            Some(AccountField::Name),
            |f| Some(focus_next(*f)),
        )
        .take(6)
        .collect();
        assert_eq!(
            walk,
            vec![
                AccountField::Name,
                AccountField::Protocol,
                AccountField::Endpoint,
                AccountField::Auth,
                AccountField::Key,
                AccountField::Name, // wraps
            ]
        );
    }

    #[test]
    fn focus_prev_walks_field_order_reversed() {
        use ox_types::settings::AccountField;
        let walk: Vec<_> = std::iter::successors(
            Some(AccountField::Name),
            |f| Some(focus_prev(*f)),
        )
        .take(6)
        .collect();
        assert_eq!(
            walk,
            vec![
                AccountField::Name,
                AccountField::Key,
                AccountField::Auth,
                AccountField::Endpoint,
                AccountField::Protocol,
                AccountField::Name, // wraps
            ]
        );
    }

    #[test]
    fn validate_compose_name_flags_empty_and_duplicate() {
        assert_eq!(validate_compose_name("", &[]), Some("required".into()));

        // Hyphenated names are now ACCEPTED (they get namecode-encoded at commit).
        assert_eq!(validate_compose_name("my-personal", &[]), None);

        // Arbitrary Unicode is accepted.
        assert_eq!(validate_compose_name("Personal 1", &[]), None);

        // Length cap.
        let long = "a".repeat(257);
        assert!(validate_compose_name(&long, &[]).unwrap().contains("too long"));

        // Duplicate check: `existing` holds path components (namecoded form).
        // "anthropic" encodes to "anthropic" (already valid XID).
        assert!(validate_compose_name("anthropic", &["anthropic".into()])
            .unwrap()
            .contains("already exists"));

        // Duplicate check via encoding: "my-personal" namecodes to some encoded form.
        // If we already have that encoded form on disk, the proposal collides.
        let encoded = namecode::encode("my-personal");
        assert!(validate_compose_name("my-personal", &[encoded])
            .unwrap()
            .contains("already exists"));

        // Trim whitespace.
        assert_eq!(validate_compose_name("  foo  ", &[]), None);
    }

    #[test]
    fn validate_compose_protocol_requires_some() {
        assert!(validate_compose_protocol(None).is_some());
        assert!(validate_compose_protocol(Some("anthropic")).is_none());
    }

    #[test]
    fn validate_compose_endpoint_requires_nonempty() {
        assert_eq!(validate_compose_endpoint(""), Some("required".into()));
        assert_eq!(validate_compose_endpoint("  "), Some("required".into()));
        assert_eq!(validate_compose_endpoint("https://x.example"), None);
    }

    #[test]
    fn validate_compose_auth_requires_some() {
        use ox_gate::provider::AuthScheme;
        assert!(validate_compose_auth(None).is_some());
        assert!(validate_compose_auth(Some(&AuthScheme::XApiKey)).is_none());
    }

    #[test]
    fn validate_compose_key_required_only_when_auth_requires_it() {
        use ox_gate::provider::AuthScheme;
        // No auth selected: key is irrelevant
        assert_eq!(validate_compose_key("", None), None);
        // Auth doesn't require key
        assert_eq!(validate_compose_key("", Some(&AuthScheme::None)), None);
        // Auth requires key, empty
        assert!(validate_compose_key("", Some(&AuthScheme::XApiKey)).is_some());
        // Auth requires key, non-empty
        assert_eq!(validate_compose_key("sk-...", Some(&AuthScheme::XApiKey)), None);
    }

    #[test]
    fn validate_compose_draft_collects_all_errors() {
        use ox_gate::provider::AuthScheme;
        let errors = validate_compose_draft(
            "",         // name
            None,       // protocol
            "",         // endpoint
            None,       // auth
            "",         // key
            &[],        // existing accounts
        );
        assert!(errors.name.is_some());
        assert!(errors.protocol.is_some());
        assert!(errors.endpoint.is_some());
        assert!(errors.auth.is_some());
        // Key is not required when auth is None
        assert!(errors.key.is_none());

        let clean = validate_compose_draft(
            "my-account",          // hyphen is now fine
            Some("anthropic"),
            "https://api.example.com",
            Some(&AuthScheme::XApiKey),
            "sk-abc",
            &[],
        );
        assert!(clean.is_clean());
    }
}
