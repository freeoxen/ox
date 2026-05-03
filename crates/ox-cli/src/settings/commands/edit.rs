//! Inline-edit lifecycle commands.
//!
//! While the user is on the accordion (`settings/index`), focusing an
//! editable field row and pressing Enter switches to edit mode in
//! place — `ui/settings/edit_mode = true`. The dispatcher routes
//! printable characters and Backspace to the existing
//! `field.insert` / `field.delete_back` commands, which mutate the
//! underlying data path directly (no separate buffer). Pressing
//! Enter or Esc while in edit mode runs `edit.exit`, which clears
//! the flag and the page returns to ordinary tree navigation.

use ox_path::oxpath;
use ox_types::Screen;
use ox_types::subscription::Write;
use structfs_core_store::{Record, Value};

use super::super::command_registry::CommandRegistry;

#[allow(unused_imports)]
use super::command;

command! {
    struct_name: BeginEditAccountEndpoint,
    id: "edit.begin.account_endpoint",
    title: "Edit Endpoint",
    description: "Enter inline edit mode for the focused account's endpoint.",
    screen: Screen::Settings,
    cursor: None,
    run: |snap, _ctx| begin_edit_account(snap, ox_types::AccountField::Endpoint),
}

command! {
    struct_name: BeginEditAccountKey,
    id: "edit.begin.account_key",
    title: "Edit API Key",
    description: "Enter inline edit mode for the focused account's API key.",
    screen: Screen::Settings,
    cursor: None,
    run: |snap, _ctx| begin_edit_account(snap, ox_types::AccountField::Key),
}

command! {
    struct_name: ExitEdit,
    id: "edit.exit",
    title: "Done Editing",
    description: "Leave inline edit mode; the field's value is already persisted.",
    screen: Screen::Settings,
    cursor: None,
    run: |_snap, _ctx| vec![Write {
        path: oxpath!("ui", "settings", "edit_mode"),
        record: Record::parsed(Value::Bool(false)),
    }],
}

fn begin_edit_account(
    data: &mut dyn structfs_core_store::Reader,
    field: ox_types::AccountField,
) -> Vec<Write> {
    use crate::settings::visible_rows::{self, RowKind};

    // Resolve the focused row to the account whose field we're
    // editing. The accordion's per-row prefix dispatch puts us here
    // only when the user is focused on an account-field row, but we
    // still locate the row by `path == focused_row` so we get the
    // un-sanitized account name (the row paths run identifiers
    // through `safe_component`).
    let focused = match read_focused_row(data) {
        Some(f) => f,
        None => return Vec::new(),
    };
    let rows = visible_rows::enumerate(data);
    let account_name = rows.into_iter().find_map(|r| match r.kind {
        RowKind::AccountField {
            account,
            field: row_field,
        } if r.path == focused && row_field == field => Some(account),
        _ => None,
    });
    let Some(account_name) = account_name else {
        return Vec::new();
    };

    let mut writes: Vec<Write> = Vec::new();
    if let Ok(value) = structfs_serde_store::to_value(&Some(account_name)) {
        writes.push(Write {
            path: oxpath!("ui", "settings", "accounts", "selected"),
            record: Record::parsed(value),
        });
    }
    if let Ok(value) = structfs_serde_store::to_value(&field) {
        writes.push(Write {
            path: oxpath!("ui", "settings", "account_detail", "field"),
            record: Record::parsed(value),
        });
    }
    // Reset the column cursor to the end of the field; the existing
    // text-edit logic in `account_model.rs` honors this for
    // delete-back/insert-at-cursor positioning.
    writes.push(Write {
        path: oxpath!("ui", "settings", "edit_cursor"),
        record: Record::parsed(Value::Integer(i64::MAX)),
    });
    writes.push(Write {
        path: oxpath!("ui", "settings", "edit_mode"),
        record: Record::parsed(Value::Bool(true)),
    });
    writes
}

fn read_focused_row(
    data: &mut dyn structfs_core_store::Reader,
) -> Option<structfs_core_store::Path> {
    let r = data
        .read(&oxpath!("ui", "settings", "focused_row"))
        .ok()
        .flatten()?;
    crate::settings::commands::navigation::path_from_value(r.as_value()?)
}

pub fn register(reg: &mut CommandRegistry) {
    reg.register(Box::new(BeginEditAccountEndpoint::new()));
    reg.register(Box::new(BeginEditAccountKey::new()));
    reg.register(Box::new(ExitEdit::new()));
}

#[cfg(test)]
mod tests {
    use super::*;

    use ox_gate::AccountConfig;
    use ox_path::oxpath;
    use ox_types::AccountField;
    use ox_types::{BadgeSource, SettingsIndexEntry};
    use structfs_serde_store::{from_value, to_value};

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

    fn write_index_with_account(snap: &mut SettingsSnapshot, name: &str) {
        snap.insert(
            &oxpath!("settings", "index", "entries", "accounts"),
            to_value(&SettingsIndexEntry {
                id: "accounts".to_string(),
                label: "Accounts".to_string(),
                description: String::new(),
                target_cursor: structfs_core_store::Path::parse("settings/accounts").unwrap(),
                badge: BadgeSource::None,
            })
            .unwrap(),
        );
        let comp = ox_kernel::PathComponent::try_new(name).unwrap();
        snap.insert(
            &oxpath!("config", "gate", "accounts", comp),
            to_value(&AccountConfig {
                provider: name.into(),
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

    #[test]
    fn begin_edit_account_endpoint_sets_edit_mode_and_field() {
        let mut snap = SettingsSnapshot::empty();
        write_index_with_account(&mut snap, "alpha");
        snap.insert(
            &oxpath!("ui", "settings", "focused_row"),
            path_to_value(&oxpath!("settings", "accounts", "alpha", "endpoint")),
        );
        let writes = run(&BeginEditAccountEndpoint::new(), &mut snap);
        // selected + field + edit_cursor + edit_mode = 4 writes
        assert_eq!(writes.len(), 4);
        assert_eq!(
            writes[0].path,
            oxpath!("ui", "settings", "accounts", "selected")
        );
        assert_eq!(
            writes[1].path,
            oxpath!("ui", "settings", "account_detail", "field")
        );
        assert_eq!(writes[3].path, oxpath!("ui", "settings", "edit_mode"));
        match &writes[3].record {
            Record::Parsed(Value::Bool(true)) => {}
            other => panic!("expected edit_mode=true, got {other:?}"),
        }
        // The field write encodes the AccountField variant.
        let field: AccountField =
            from_value(writes[1].record.as_value().unwrap().clone()).unwrap();
        assert_eq!(field, AccountField::Endpoint);
    }

    #[test]
    fn begin_edit_with_no_focus_is_inert() {
        let mut snap = SettingsSnapshot::empty();
        write_index_with_account(&mut snap, "alpha");
        // No focused_row written.
        let writes = run(&BeginEditAccountEndpoint::new(), &mut snap);
        assert!(writes.is_empty());
    }

    #[test]
    fn begin_edit_with_mismatched_focus_is_inert() {
        // Focused row points at a different field than the command
        // is targeting — fall through inert.
        let mut snap = SettingsSnapshot::empty();
        write_index_with_account(&mut snap, "alpha");
        snap.insert(
            &oxpath!("ui", "settings", "focused_row"),
            path_to_value(&oxpath!("settings", "accounts", "alpha", "name")),
        );
        let writes = run(&BeginEditAccountEndpoint::new(), &mut snap);
        assert!(writes.is_empty());
    }

    #[test]
    fn exit_edit_clears_edit_mode() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "edit_mode"),
            Value::Bool(true),
        );
        let writes = run(&ExitEdit::new(), &mut snap);
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "settings", "edit_mode"));
        match &writes[0].record {
            Record::Parsed(Value::Bool(false)) => {}
            other => panic!("expected edit_mode=false, got {other:?}"),
        }
    }
}
