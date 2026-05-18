//! Inline-edit state machine for the accordion field rows.
//!
//! When the user presses Enter on a focused editable field row, the
//! tree dispatch calls one of the `begin.*` commands here. Begin:
//!
//!   - reads the field's current value off the data path,
//!   - converts it to a `String` (for numeric fields, decimal
//!     digits; empty for `None`),
//!   - writes that string into `ui/settings/edit/buffer`,
//!   - records the field row's path in `ui/settings/edit/target_path`,
//!   - saves the prior focused-row cursor at
//!     `ui/settings/edit/cursor_saved`,
//!   - moves the cursor (`ui/settings/focused`) to `settings/_edit`.
//!
//! Cursor-as-focus: the cursor sitting at `settings/_edit` IS the
//! "edit mode is active" condition — there is no separate
//! `ui/settings/edit_mode: bool` flag. The dispatcher's
//! `compute_scope_path` engages the `_edit` scope by virtue of the
//! cursor being there. The dispatcher routes every printable char to
//! `edit.insert_char` (append to buffer), Backspace to
//! `edit.delete_back` (pop from buffer), Enter to `edit.commit`
//! (parse + write to data path + restore cursor), and Esc to
//! `edit.cancel` (restore cursor without writing). Both commit and
//! cancel cascade-clear the `ui/settings/edit` subtree
//! (target_path + buffer + cursor_saved) in a single Null write.
//!
//! The renderer picks up the cursor being at `settings/_edit` plus
//! `ui/settings/edit/target_path` and substitutes the data value
//! with the live buffer plus a visible cursor block, so the user
//! sees what they're typing.

use ox_path::oxpath;
use ox_types::settings::{AccountField, ModelField, ModelKey};
use ox_types::subscription::Write;
use structfs_core_store::{Path, Reader, Record, Value};
use structfs_serde_store::to_value;

use super::super::visible_rows::{self, RowKind};
use super::navigation::{path_from_value, path_to_value};
use crate::settings::CommandRegistry;

#[allow(unused_imports)]
use super::command;

// ---------------------------------------------------------------------------
// Begin commands — one per field type, called by `tree.activate`.
// ---------------------------------------------------------------------------

command! {
    struct_name: BeginEditAccountEndpoint,
    id: "edit.begin.account_endpoint",
    title: "Edit Endpoint",
    description: "Enter inline edit mode for the focused Connection's endpoint.",
    cursor: None,
    run: |snap, _ctx| begin_edit_account_text(snap, AccountField::Endpoint),
}

command! {
    struct_name: BeginEditAccountKey,
    id: "edit.begin.account_key",
    title: "Edit API Key",
    description: "Enter inline edit mode for the focused Connection's API key.",
    cursor: None,
    run: |snap, _ctx| begin_edit_account_text(snap, AccountField::Key),
}

command! {
    struct_name: BeginEditModelField,
    id: "edit.begin.model_field",
    title: "Edit Model Override",
    description: "Enter inline edit mode for the focused model's numeric override.",
    cursor: None,
    run: |snap, _ctx| begin_edit_model_field_inner(snap),
}

// ---------------------------------------------------------------------------
// Buffer mutations — bound at `Exact(settings/_edit)`.
// ---------------------------------------------------------------------------

command! {
    struct_name: InsertChar,
    id: "edit.insert_char",
    title: "Insert character",
    description: "Append the just-pressed printable char to the edit buffer.",
    cursor: None,
    run: |snap, ctx| insert_char(snap, ctx),
}

command! {
    struct_name: DeleteBack,
    id: "edit.delete_back",
    title: "Backspace",
    description: "Remove the last character from the edit buffer.",
    cursor: None,
    run: |snap, _ctx| delete_back(snap),
}

command! {
    struct_name: Commit,
    id: "edit.commit",
    title: "Commit",
    description: "Persist the edit buffer to the field's data path and exit edit mode.",
    cursor: None,
    run: |snap, _ctx| commit(snap),
}

command! {
    struct_name: Cancel,
    id: "edit.cancel",
    title: "Cancel",
    description: "Discard the edit buffer and exit edit mode.",
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
/// `focused` is unset or names a row that vanished from the
/// visible tree (e.g. an account got deleted under the user's cursor).
fn focused_visible_row(data: &mut dyn Reader) -> Option<visible_rows::VisibleRow> {
    let path = read_focused_path(data)?;
    visible_rows::enumerate(data)
        .into_iter()
        .find(|r| r.path == path)
}

fn read_focused_path(data: &mut dyn Reader) -> Option<Path> {
    let r = data
        .read(&oxpath!("ui", "settings", "focused"))
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
    let row = match focused_visible_row(data) {
        Some(r) => r,
        None => return Vec::new(),
    };
    let account = match row.kind {
        RowKind::AccountField {
            account,
            field: row_field,
        } if row_field == field => account,
        RowKind::AccountField { .. }
        | RowKind::Entry { .. }
        | RowKind::Account { .. }
        | RowKind::Model { .. }
        | RowKind::ModelField { .. } => return Vec::new(),
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
    let row = match focused_visible_row(data) {
        Some(r) => r,
        None => return Vec::new(),
    };
    let (account, model_id, field) = match row.kind {
        RowKind::ModelField {
            account,
            model_id,
            field,
        } => (account, model_id, field),
        RowKind::Entry { .. }
        | RowKind::Account { .. }
        | RowKind::Model { .. }
        | RowKind::AccountField { .. } => return Vec::new(),
    };
    let initial = current_model_override(data, &account, &model_id, field).unwrap_or_default();
    enter_edit_mode(row.path, initial)
}

fn enter_edit_mode(field_path: Path, buffer: String) -> Vec<Write> {
    // Cursor-as-focus: the field row IS the place the user just acted
    // on. Save it so cancel/commit can return there. Move the cursor to
    // `settings/_edit` to engage the dispatcher's `_edit` scope; the
    // edit subtree (target_path + buffer + cursor_saved) holds the
    // mode's data half.
    vec![
        Write {
            path: oxpath!("ui", "settings", "edit", "target_path"),
            record: Record::parsed(path_to_value(&field_path)),
        },
        Write {
            path: oxpath!("ui", "settings", "edit", "buffer"),
            record: Record::parsed(Value::String(buffer)),
        },
        Write {
            path: oxpath!("ui", "settings", "edit", "cursor_saved"),
            record: Record::parsed(path_to_value(&field_path)),
        },
        Write {
            path: oxpath!("ui", "settings", "focused"),
            record: Record::parsed(path_to_value(&oxpath!("settings", "_edit"))),
        },
    ]
}

/// Cascade-clear the edit subtree in one Null write at the subtree
/// root. Clears target_path + buffer + cursor_saved.
fn clear_edit_subtree() -> Write {
    Write {
        path: oxpath!("ui", "settings", "edit"),
        record: Record::parsed(Value::Null),
    }
}

fn insert_char(data: &mut dyn Reader, ctx: &crate::settings::CommandCtx<'_>) -> Vec<Write> {
    use ox_types::key_chord::KeyCodeRepr;

    let chord = match ctx.last_keystroke.as_ref() {
        Some(c) => c,
        None => return Vec::new(),
    };
    let ch = match chord.code {
        KeyCodeRepr::Char(c) => c,
        _ => return Vec::new(),
    };
    insert_char_into_edit_buffer(
        data,
        &oxpath!("ui", "settings", "edit", "buffer"),
        &oxpath!("ui", "settings", "edit", "target_path"),
        ch,
    )
}

/// Read the current buffer + target field path, validate the char
/// against the focused row's RowKind (digits-only for ModelField,
/// anything printable for AccountField), and emit a single write that
/// appends `ch`. The single source of truth shared by `EditInsertChar`
/// (discrete-tier) and `TextInputHandler` (opaque-tier); each tier
/// supplies the char via its own route — `ctx.last_keystroke` for the
/// command, the dispatched `KeyChord` for the handler.
fn insert_char_into_edit_buffer(
    data: &mut dyn Reader,
    buffer_path: &Path,
    target_path: &Path,
    ch: char,
) -> Vec<Write> {
    let current = read_string(data, buffer_path).unwrap_or_default();
    let field_path = match read_path(data, target_path) {
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
        Some(RowKind::Entry { .. })
        | Some(RowKind::Account { .. })
        | Some(RowKind::Model { .. })
        | None => false,
    };
    if !accept {
        return Vec::new();
    }
    let mut next = current;
    next.push(ch);
    vec![Write {
        path: buffer_path.clone(),
        record: Record::parsed(Value::String(next)),
    }]
}

/// Opaque key handler that claims printable chords on the edit scope.
/// Replaces the ~96 discrete `BindingEntry` registrations (one per
/// printable ASCII char) that used to route to `edit.insert_char`.
/// The lifecycle keys (Backspace/Enter/Esc) remain as discrete bindings
/// on the same scope so the help screen can still enumerate them.
///
/// Construction takes the buffer and target-path locations so the
/// handler stays a pure function of its install-time configuration —
/// no hidden coupling to hardcoded edit-subtree paths beyond what the
/// installer provides.
pub struct TextInputHandler {
    buffer_path: Path,
    target_path: Path,
}

impl TextInputHandler {
    pub fn new(buffer_path: Path, target_path: Path) -> Self {
        Self {
            buffer_path,
            target_path,
        }
    }
}

impl horns_core::KeyHandler for TextInputHandler {
    fn handle(
        &self,
        snapshot: &mut dyn Reader,
        key: &horns_core::KeyChord,
        _ctx: &horns_core::CommandCtx<'_>,
    ) -> Option<Vec<Write>> {
        use ox_types::key_chord::KeyCodeRepr;

        // Claim only un-modified printable chars. Shift-only is allowed
        // because terminals report uppercase letters as shift+lowercase
        // char. Lifecycle keys (Backspace, Enter, Esc) are discrete
        // bindings at the same scope+phase and win on tie, so the handler
        // doesn't need to special-case them — non-`Char` codes pass through
        // here naturally.
        if key.modifiers.ctrl || key.modifiers.alt || key.modifiers.super_ {
            return None;
        }
        let ch = match key.code {
            KeyCodeRepr::Char(c) if !c.is_control() => c,
            _ => return None,
        };
        Some(insert_char_into_edit_buffer(
            snapshot,
            &self.buffer_path,
            &self.target_path,
            ch,
        ))
    }
}

fn delete_back(data: &mut dyn Reader) -> Vec<Write> {
    let mut current =
        read_string(data, &oxpath!("ui", "settings", "edit", "buffer")).unwrap_or_default();
    if current.pop().is_none() {
        return Vec::new();
    }
    vec![Write {
        path: oxpath!("ui", "settings", "edit", "buffer"),
        record: Record::parsed(Value::String(current)),
    }]
}

/// Cancel restores the saved pre-open cursor and cascade-clears the
/// edit subtree. Falls back to `settings/accounts` when no save is
/// present (pathological seed).
fn cancel(data: &mut dyn Reader) -> Vec<Write> {
    let saved = read_path(data, &oxpath!("ui", "settings", "edit", "cursor_saved"))
        .unwrap_or_else(|| oxpath!("settings", "accounts"));
    vec![
        Write {
            path: oxpath!("ui", "settings", "focused"),
            record: Record::parsed(path_to_value(&saved)),
        },
        clear_edit_subtree(),
    ]
}

/// Commit writes the buffer to the target field's data path, restores
/// the cursor to the target field row, and cascade-clears the edit
/// subtree.
fn commit(data: &mut dyn Reader) -> Vec<Write> {
    let field_path = match read_path(data, &oxpath!("ui", "settings", "edit", "target_path")) {
        Some(p) => p,
        None => {
            // No target — fall back to the saved cursor, then clear.
            return cancel(data);
        }
    };
    let buffer =
        read_string(data, &oxpath!("ui", "settings", "edit", "buffer")).unwrap_or_default();
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
        Some(RowKind::Entry { .. })
        | Some(RowKind::Account { .. })
        | Some(RowKind::Model { .. })
        | None => Vec::new(),
    };
    // Restore cursor to the field row — the row the user just edited.
    writes.push(Write {
        path: oxpath!("ui", "settings", "focused"),
        record: Record::parsed(path_to_value(&field_path)),
    });
    writes.push(clear_edit_subtree());
    writes
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
        provider_str.map(|p| ox_gate::AccountConfig {
            provider: p,
            ..Default::default()
        })
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
                read_child_string(data, account, "provider").map(|p| ox_gate::AccountConfig {
                    provider: p,
                    ..Default::default()
                })
            })
            .unwrap_or(ox_gate::AccountConfig {
                provider: "anthropic".to_string(),
                ..Default::default()
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
///
/// Cursor-as-focus: "active" means the cursor (`ui/settings/focused`)
/// sits at `settings/_edit`. The target field path and buffer live at
/// `ui/settings/edit/{target_path,buffer}`.
#[derive(Clone, Debug)]
pub struct EditState {
    pub field_path: Path,
    pub buffer: String,
}

pub fn read_edit_state(data: &mut dyn Reader) -> Option<EditState> {
    if !cursor_is_at_edit(data) {
        return None;
    }
    let field_path = read_path(data, &oxpath!("ui", "settings", "edit", "target_path"))?;
    let buffer =
        read_string(data, &oxpath!("ui", "settings", "edit", "buffer")).unwrap_or_default();
    Some(EditState { field_path, buffer })
}

/// True iff `ui/settings/focused` equals `settings/_edit`. The cursor
/// being there IS the "edit mode is active" condition under
/// cursor-as-focus.
pub fn cursor_is_at_edit(data: &mut dyn Reader) -> bool {
    read_focused_path(data)
        .as_ref()
        .is_some_and(|p| p == &oxpath!("settings", "_edit"))
}

#[allow(dead_code)] // re-exported via `read_edit_state`'s field_path; placeholder
fn _force_modelkey_use(_k: ModelKey) {}

#[cfg(test)]
mod tests {
    use super::*;

    use ox_gate::{AccountConfig, ModelInfo, ModelInfoSource};
    use ox_types::{BadgeSource, SettingsIndexEntry};

    use crate::settings::RendererRegistry;
    use crate::settings::commands::navigation::path_to_value;
    use crate::settings::snapshot::SettingsSnapshot;
    use crate::settings::visible_rows::expanded_set_to_value;
    use crate::settings::{Command, CommandCtx};

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
                ..Default::default()
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

    /// Seed the new cursor-as-focus edit subtree shape:
    /// `ui/settings/focused = settings/_edit` plus the edit data at
    /// `ui/settings/edit/{target_path,buffer,cursor_saved}`.
    fn seed_edit_active(snap: &mut SettingsSnapshot, target: &Path, buffer: &str) {
        snap.insert(
            &oxpath!("ui", "settings", "focused"),
            path_to_value(&oxpath!("settings", "_edit")),
        );
        snap.insert(
            &oxpath!("ui", "settings", "edit", "target_path"),
            path_to_value(target),
        );
        snap.insert(
            &oxpath!("ui", "settings", "edit", "buffer"),
            Value::String(buffer.into()),
        );
        snap.insert(
            &oxpath!("ui", "settings", "edit", "cursor_saved"),
            path_to_value(target),
        );
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
        let target = oxpath!("settings", "accounts", "alpha", "endpoint");
        snap.insert(
            &oxpath!("ui", "settings", "focused"),
            path_to_value(&target),
        );

        let writes = run(&BeginEditAccountEndpoint::new(), &mut snap);
        // target_path + buffer + cursor_saved + cursor (focused) = 4 writes
        assert_eq!(writes.len(), 4);
        assert_eq!(
            writes[0].path,
            oxpath!("ui", "settings", "edit", "target_path")
        );
        assert_eq!(writes[1].path, oxpath!("ui", "settings", "edit", "buffer"));
        match &writes[1].record {
            Record::Parsed(Value::String(s)) => {
                assert_eq!(s, "https://api.anthropic.com");
            }
            other => panic!("buffer is not a String: {other:?}"),
        }
        assert_eq!(
            writes[2].path,
            oxpath!("ui", "settings", "edit", "cursor_saved")
        );
        // Final write moves the cursor to `settings/_edit` — the
        // dispatcher's "edit scope is engaged" condition.
        assert_eq!(writes[3].path, oxpath!("ui", "settings", "focused"));
    }

    #[test]
    fn edit_open_writes_cursor_to_edit_scope() {
        // CF-4 invariant: opening edit mode moves the focused cursor
        // to `settings/_edit`. The dispatcher's `compute_scope_path`
        // engages the `_edit` scope from this cursor alone — no
        // separate `edit_mode: bool` flag.
        let mut snap = SettingsSnapshot::empty();
        write_index_with_account(&mut snap, "alpha", "anthropic");
        let provider_comp = ox_kernel::PathComponent::try_new("anthropic").unwrap();
        snap.insert(
            &oxpath!("config", "gate", "providers", provider_comp),
            to_value(&ox_gate::ProviderConfig {
                dialect: "anthropic".into(),
                endpoint: String::new(),
                version: String::new(),
                auth: None,
            })
            .unwrap(),
        );
        snap.insert(
            &oxpath!("ui", "settings", "focused"),
            path_to_value(&oxpath!("settings", "accounts", "alpha", "endpoint")),
        );

        let writes = run(&BeginEditAccountEndpoint::new(), &mut snap);
        // Apply and verify the focused cursor is now at settings/_edit.
        apply_writes(&mut snap, &writes);
        let focused = read_focused_path(&mut snap).unwrap();
        assert_eq!(focused, oxpath!("settings", "_edit"));
    }

    #[test]
    fn edit_open_saves_target_path_buffer_and_cursor() {
        // CF-4 invariant: opening writes target_path, buffer, and
        // cursor_saved at `ui/settings/edit/{...}`. cursor_saved is
        // the prior cursor — for edit-mode, the user IS on the field
        // they're now editing, so cursor_saved == target_path.
        let mut snap = SettingsSnapshot::empty();
        write_index_with_account(&mut snap, "alpha", "anthropic");
        let provider_comp = ox_kernel::PathComponent::try_new("anthropic").unwrap();
        snap.insert(
            &oxpath!("config", "gate", "providers", provider_comp),
            to_value(&ox_gate::ProviderConfig {
                dialect: "anthropic".into(),
                endpoint: "seed".into(),
                version: String::new(),
                auth: None,
            })
            .unwrap(),
        );
        let target = oxpath!("settings", "accounts", "alpha", "endpoint");
        snap.insert(
            &oxpath!("ui", "settings", "focused"),
            path_to_value(&target),
        );

        let writes = run(&BeginEditAccountEndpoint::new(), &mut snap);
        apply_writes(&mut snap, &writes);
        assert_eq!(
            read_path(&mut snap, &oxpath!("ui", "settings", "edit", "target_path")).unwrap(),
            target
        );
        assert_eq!(
            read_string(&mut snap, &oxpath!("ui", "settings", "edit", "buffer")).unwrap(),
            "seed",
        );
        assert_eq!(
            read_path(
                &mut snap,
                &oxpath!("ui", "settings", "edit", "cursor_saved")
            )
            .unwrap(),
            target,
        );
    }

    #[test]
    fn insert_char_appends_to_buffer() {
        let mut snap = SettingsSnapshot::empty();
        write_index_with_account(&mut snap, "alpha", "anthropic");
        seed_edit_active(
            &mut snap,
            &oxpath!("settings", "accounts", "alpha", "endpoint"),
            "hello",
        );

        let writes = run_with_key(&InsertChar::new(), &mut snap, '!');
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "settings", "edit", "buffer"));
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
            &oxpath!("config", "gate", "accounts", comp.clone(), "provider"),
            Value::String("alpha".into()),
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
        seed_edit_active(&mut snap, &field_path, "100");

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
            &oxpath!("ui", "settings", "edit", "buffer"),
            Value::String("hello".into()),
        );
        let writes = run(&DeleteBack::new(), &mut snap);
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "settings", "edit", "buffer"));
        match &writes[0].record {
            Record::Parsed(Value::String(s)) => assert_eq!(s, "hell"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn delete_back_on_empty_buffer_is_inert() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "edit", "buffer"),
            Value::String(String::new()),
        );
        let writes = run(&DeleteBack::new(), &mut snap);
        assert!(writes.is_empty());
    }

    #[test]
    fn edit_cancel_restores_saved_cursor() {
        // CF-4 invariant: cancel reads `cursor_saved` and writes
        // `focused = saved`, then cascade-clears the edit subtree.
        let mut snap = SettingsSnapshot::empty();
        let target = oxpath!("settings", "accounts", "alpha", "endpoint");
        seed_edit_active(&mut snap, &target, "in-progress");

        let writes = run(&Cancel::new(), &mut snap);
        // cursor restore + subtree cascade-null = 2 writes
        assert_eq!(writes.len(), 2);
        assert_eq!(writes[0].path, oxpath!("ui", "settings", "focused"));
        // Restored cursor matches the saved value (= target for edit).
        match &writes[0].record {
            Record::Parsed(v) => {
                let restored = super::path_from_value(v).unwrap();
                assert_eq!(restored, target);
            }
            other => panic!("unexpected: {other:?}"),
        }
        assert_eq!(writes[1].path, oxpath!("ui", "settings", "edit"));
        match &writes[1].record {
            Record::Parsed(Value::Null) => {}
            other => panic!("expected Null cascade clear, got {other:?}"),
        }
    }

    #[test]
    fn edit_commit_writes_buffer_to_target_path_and_restores_cursor() {
        // CF-4 invariant: commit emits the value write at target_path,
        // restores cursor to target_path, and cascade-clears the edit
        // subtree.
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
        let target = oxpath!("settings", "accounts", "alpha", "endpoint");
        seed_edit_active(&mut snap, &target, "https://new.example");

        let writes = run(&Commit::new(), &mut snap);
        // ProviderConfig write + cursor restore + subtree cascade-null = 3 writes
        assert_eq!(writes.len(), 3);
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
        // Cursor restored to the target field row.
        assert_eq!(read_focused_path(&mut snap).unwrap(), target);
        // Edit subtree cleared.
        assert!(read_edit_state(&mut snap).is_none());
    }

    #[test]
    fn commit_with_no_target_path_falls_back_to_cancel() {
        // Pathological: cursor at _edit but no target_path. Commit
        // falls back to cancel — restore from cursor_saved (or
        // settings/accounts) and cascade-clear.
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "focused"),
            path_to_value(&oxpath!("settings", "_edit")),
        );
        let writes = run(&Commit::new(), &mut snap);
        // No data write; cursor-restore + cascade-null = 2 writes
        assert_eq!(writes.len(), 2);
        assert_eq!(writes[0].path, oxpath!("ui", "settings", "focused"));
        assert_eq!(writes[1].path, oxpath!("ui", "settings", "edit"));
    }

    #[test]
    fn read_edit_state_reflects_active_edit() {
        let mut snap = SettingsSnapshot::empty();
        let field_path = oxpath!("settings", "accounts", "alpha", "endpoint");
        seed_edit_active(&mut snap, &field_path, "typing");
        let state = read_edit_state(&mut snap).unwrap();
        assert_eq!(state.field_path, field_path);
        assert_eq!(state.buffer, "typing");
    }

    #[test]
    fn read_edit_state_returns_none_when_cursor_not_at_edit() {
        let mut snap = SettingsSnapshot::empty();
        // Cursor anywhere but `settings/_edit` means edit mode is not
        // active — the cursor IS the discriminator.
        snap.insert(
            &oxpath!("ui", "settings", "focused"),
            path_to_value(&oxpath!("settings", "accounts")),
        );
        assert!(read_edit_state(&mut snap).is_none());
    }

    // -----------------------------------------------------------------
    // TextInputHandler — opaque-tier replacement for the 96 discrete
    // printable-ASCII bindings under `settings/_edit`.
    // -----------------------------------------------------------------

    fn handler() -> TextInputHandler {
        TextInputHandler::new(
            oxpath!("ui", "settings", "edit", "buffer"),
            oxpath!("ui", "settings", "edit", "target_path"),
        )
    }

    fn handler_ctx<'a>(registry: &'a RendererRegistry) -> horns_core::CommandCtx<'a> {
        horns_core::CommandCtx {
            registry,
            last_keystroke: None,
        }
    }

    fn chord(
        modifiers: ox_types::key_chord::KeyModifierSet,
        code: ox_types::key_chord::KeyCodeRepr,
    ) -> ox_types::KeyChord {
        ox_types::KeyChord { modifiers, code }
    }

    #[test]
    fn text_input_handler_claims_printable_and_appends_to_buffer() {
        use horns_core::KeyHandler;
        use ox_types::key_chord::{KeyCodeRepr, KeyModifierSet};

        // Same seeding shape as `insert_char_appends_to_buffer`: an
        // account text field is the target, so the handler accepts any
        // printable char (no digit-only restriction).
        let mut snap = SettingsSnapshot::empty();
        write_index_with_account(&mut snap, "alpha", "anthropic");
        seed_edit_active(
            &mut snap,
            &oxpath!("settings", "accounts", "alpha", "endpoint"),
            "hello",
        );

        let registry = RendererRegistry::new();
        let ctx = handler_ctx(&registry);
        let key = chord(KeyModifierSet::default(), KeyCodeRepr::Char('!'));
        let writes = handler()
            .handle(&mut snap, &key, &ctx)
            .expect("printable chord must be claimed");

        // Exactly the same write shape as EditInsertChar emits today —
        // one buffer write with the appended char.
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "settings", "edit", "buffer"));
        match &writes[0].record {
            Record::Parsed(Value::String(s)) => assert_eq!(s, "hello!"),
            other => panic!("unexpected record: {other:?}"),
        }
    }

    #[test]
    fn text_input_handler_passes_on_non_printable() {
        use horns_core::KeyHandler;
        use ox_types::key_chord::{KeyCodeRepr, KeyModifierSet};

        let mut snap = SettingsSnapshot::empty();
        let registry = RendererRegistry::new();
        let ctx = handler_ctx(&registry);
        let key = chord(KeyModifierSet::default(), KeyCodeRepr::Enter);

        let result = handler().handle(&mut snap, &key, &ctx);
        assert!(
            result.is_none(),
            "Enter is a lifecycle key — handler must pass so the discrete `edit.commit` binding fires"
        );
    }

    #[test]
    fn text_input_handler_passes_on_ctrl_modifier() {
        use horns_core::KeyHandler;
        use ox_types::key_chord::{KeyCodeRepr, KeyModifierSet};

        let mut snap = SettingsSnapshot::empty();
        let registry = RendererRegistry::new();
        let ctx = handler_ctx(&registry);
        let mods = KeyModifierSet {
            ctrl: true,
            ..KeyModifierSet::default()
        };
        let key = chord(mods, KeyCodeRepr::Char('a'));

        let result = handler().handle(&mut snap, &key, &ctx);
        assert!(
            result.is_none(),
            "Ctrl+letter is reserved for chord commands — must not be claimed as text input"
        );
    }

    #[test]
    fn text_input_handler_claims_shift_only_uppercase_letter() {
        // Terminals report uppercase letters as shift+lowercase char.
        // The handler must claim these so capital letters reach the
        // buffer instead of falling through to outer-scope bindings.
        use horns_core::KeyHandler;
        use ox_types::key_chord::{KeyCodeRepr, KeyModifierSet};

        let mut snap = SettingsSnapshot::empty();
        write_index_with_account(&mut snap, "alpha", "anthropic");
        seed_edit_active(
            &mut snap,
            &oxpath!("settings", "accounts", "alpha", "endpoint"),
            "ab",
        );

        let registry = RendererRegistry::new();
        let ctx = handler_ctx(&registry);
        let mods = KeyModifierSet {
            shift: true,
            ..KeyModifierSet::default()
        };
        let key = chord(mods, KeyCodeRepr::Char('Z'));

        let writes = handler()
            .handle(&mut snap, &key, &ctx)
            .expect("shift+letter must be claimed");
        assert_eq!(writes.len(), 1);
        match &writes[0].record {
            Record::Parsed(Value::String(s)) => assert_eq!(s, "abZ"),
            other => panic!("unexpected record: {other:?}"),
        }
    }
}
