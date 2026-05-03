//! Tree navigation commands for the accordion settings view.
//!
//! These commands operate on the *visible* row list — the flat
//! enumeration of currently-visible rows that
//! [`super::super::visible_rows::enumerate`] computes. The renderer
//! consults the same enumeration when drawing, so navigation and
//! display can never disagree about which rows are visible or what
//! order they're in.
//!
//! Cursor convention: `ui/settings/cursor` holds the path of the
//! currently-focused row. When the row collapses out from under the
//! cursor, the next read of the visible list won't find it; the
//! navigation commands clamp to the first visible row.

use ox_path::oxpath;
use ox_types::subscription::Write;
use ox_types::{AccountField, Screen};
use structfs_core_store::{Reader, Record, Value};

use super::super::command_registry::CommandRegistry;
use super::super::visible_rows::{
    self, RowKind, expanded_set_to_value, path_to_string, read_expanded_set,
};
use super::navigation::{path_from_value, path_to_value};

#[allow(unused_imports)]
use super::command;

command! {
    struct_name: TreeNext,
    id: "tree.next",
    title: "Next Row",
    description: "Move the focus to the next visible row in the settings tree.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "index")),
    run: |snap, _ctx| step(snap, Direction::Next),
}

command! {
    struct_name: TreePrev,
    id: "tree.prev",
    title: "Previous Row",
    description: "Move the focus to the previous visible row in the settings tree.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "index")),
    run: |snap, _ctx| step(snap, Direction::Prev),
}

command! {
    struct_name: TreeActivate,
    id: "tree.activate",
    title: "Open / Toggle Row",
    description: "Toggle expansion on a category row; descend into a leaf row.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "index")),
    run: |snap, _ctx| activate(snap),
}

command! {
    struct_name: TreeCollapseOrAscend,
    id: "tree.collapse_or_ascend",
    title: "Collapse / Back",
    description: "Collapse the focused row if expanded; otherwise exit the screen.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "index")),
    run: |snap, _ctx| collapse_or_ascend(snap),
}

#[derive(Clone, Copy)]
enum Direction {
    Next,
    Prev,
}

/// Read the focused-row path. This is intentionally NOT
/// `ui/settings/cursor`: cursor identifies the active page (the
/// renderer + binding-scope), which on the accordion screen is always
/// `settings/index`. The focused row inside that page lives at
/// `ui/settings/focused_row`. Conflating the two breaks binding
/// dispatch (the binding lookup uses cursor as its scope key).
fn read_focused(data: &mut dyn Reader) -> Option<structfs_core_store::Path> {
    let record = data
        .read(&oxpath!("ui", "settings", "focused_row"))
        .ok()
        .flatten()?;
    let value = record.as_value()?;
    path_from_value(value)
}

fn step(data: &mut dyn Reader, direction: Direction) -> Vec<Write> {
    let rows = visible_rows::enumerate(data);
    if rows.is_empty() {
        return Vec::new();
    }
    let cursor = read_focused(data);
    let current_idx = cursor
        .as_ref()
        .and_then(|c| visible_rows::position_of(&rows, c))
        .unwrap_or(0);
    let next_idx = match direction {
        Direction::Next => (current_idx + 1) % rows.len(),
        Direction::Prev => (current_idx + rows.len() - 1) % rows.len(),
    };
    let target = &rows[next_idx].path;
    vec![Write {
        path: oxpath!("ui", "settings", "focused_row"),
        record: Record::parsed(path_to_value(target)),
    }]
}

fn activate(data: &mut dyn Reader) -> Vec<Write> {
    let rows = visible_rows::enumerate(data);
    if rows.is_empty() {
        return Vec::new();
    }
    let cursor = match read_focused(data) {
        Some(c) => c,
        None => return Vec::new(),
    };
    let row = match visible_rows::position_of(&rows, &cursor).map(|i| &rows[i]) {
        Some(r) => r,
        None => return Vec::new(),
    };
    if row.expandable {
        // Toggle membership in the expanded set.
        let mut set = read_expanded_set(data);
        let path_str = path_to_string(&row.path);
        if let Some(pos) = set.iter().position(|s| s == &path_str) {
            set.remove(pos);
        } else {
            set.push(path_str);
        }
        vec![Write {
            path: oxpath!("ui", "settings", "expanded"),
            record: Record::parsed(expanded_set_to_value(&set)),
        }]
    } else {
        // Leaf field row. Two paths:
        //
        // - **Text-editable account fields (Endpoint, Key)** stay on
        //   the accordion and switch to inline edit mode: write the
        //   account selection + the focused-field state + flip
        //   `edit_mode = true`. The dispatcher's edit-mode pass routes
        //   subsequent printable-char keystrokes to `field.insert`,
        //   which mutates the underlying data path directly.
        //
        // - **Selectors and read-only fields (Name / Protocol / Auth,
        //   plus all model fields for now)** drill into the legacy
        //   `_detail` editor with the targeted field pre-focused.
        //   Their edit semantics (cycle through provider options,
        //   numeric override) need separate inline handling that's
        //   deferred to a follow-up.
        match &row.kind {
            RowKind::AccountField {
                account,
                field: AccountField::Endpoint,
            } => begin_inline_edit_account(
                account.clone(),
                AccountField::Endpoint,
            ),
            RowKind::AccountField {
                account,
                field: AccountField::Key,
            } => begin_inline_edit_account(account.clone(), AccountField::Key),
            RowKind::AccountField { account, field } => {
                let mut writes = Vec::new();
                if let Ok(value) = structfs_serde_store::to_value(&Some(account.clone())) {
                    writes.push(Write {
                        path: oxpath!("ui", "settings", "accounts", "selected"),
                        record: Record::parsed(value),
                    });
                }
                if let Ok(value) = structfs_serde_store::to_value(field) {
                    writes.push(Write {
                        path: oxpath!("ui", "settings", "account_detail", "field"),
                        record: Record::parsed(value),
                    });
                }
                writes.push(Write {
                    path: oxpath!("ui", "settings", "cursor"),
                    record: Record::parsed(path_to_value(&oxpath!(
                        "settings",
                        "accounts",
                        "_detail"
                    ))),
                });
                writes
            }
            RowKind::ModelField {
                account,
                model_id,
                field,
            } => {
                let mut writes = Vec::new();
                let key = ox_types::settings::ModelKey {
                    account: account.clone(),
                    model_id: model_id.clone(),
                };
                if let Ok(value) = structfs_serde_store::to_value(&Some(key)) {
                    writes.push(Write {
                        path: oxpath!("ui", "settings", "models", "selected"),
                        record: Record::parsed(value),
                    });
                }
                if let Ok(value) = structfs_serde_store::to_value(field) {
                    writes.push(Write {
                        path: oxpath!("ui", "settings", "model_detail", "field"),
                        record: Record::parsed(value),
                    });
                }
                writes.push(Write {
                    path: oxpath!("ui", "settings", "cursor"),
                    record: Record::parsed(path_to_value(&oxpath!(
                        "settings",
                        "models",
                        "_detail"
                    ))),
                });
                writes
            }
            // The expandable arm above already handles every Entry,
            // Account, and Model row.
            RowKind::Entry { .. } | RowKind::Account { .. } | RowKind::Model { .. } => {
                Vec::new()
            }
        }
    }
}

/// Switch the focused account field into inline edit mode on the
/// accordion. Sets `account_detail/field`, `accounts/selected`,
/// resets `edit_cursor` to the end of the current value, and flips
/// `edit_mode = true`. After this, the dispatcher's edit-mode pass
/// routes printable chars / Backspace / Enter / Esc to the
/// edit-mode bindings; the page cursor stays at `settings/index`,
/// so the user never leaves the tree.
fn begin_inline_edit_account(account: String, field: AccountField) -> Vec<Write> {
    let mut writes: Vec<Write> = Vec::new();
    if let Ok(value) = structfs_serde_store::to_value(&Some(account)) {
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
    // `i64::MAX` clamps to end-of-string in the existing
    // `field.delete_back` / `field.insert` cursor math.
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

fn collapse_or_ascend(data: &mut dyn Reader) -> Vec<Write> {
    let rows = visible_rows::enumerate(data);
    let cursor = read_focused(data);

    // Collapse the focused row if it's an expanded entry.
    if let Some(c) = &cursor {
        if let Some(row) = visible_rows::position_of(&rows, c).map(|i| &rows[i]) {
            if row.expandable && row.expanded {
                let mut set = read_expanded_set(data);
                let path_str = path_to_string(&row.path);
                set.retain(|s| s != &path_str);
                return vec![Write {
                    path: oxpath!("ui", "settings", "expanded"),
                    record: Record::parsed(expanded_set_to_value(&set)),
                }];
            }
        }
    }

    // If the focused row is a leaf inside an expanded entry, walk up
    // to its parent entry and move the cursor there. Same shape as
    // a plain "back to parent" — keeps the user oriented.
    if let Some(c) = &cursor {
        if let Some(row) = visible_rows::position_of(&rows, c).map(|i| &rows[i]) {
            if row.depth > 0 {
                // Find the most recent entry above this row.
                let pos = visible_rows::position_of(&rows, c).unwrap();
                for upstream in rows[..pos].iter().rev() {
                    if upstream.depth == 0 {
                        return vec![Write {
                            path: oxpath!("ui", "settings", "focused_row"),
                            record: Record::parsed(path_to_value(&upstream.path)),
                        }];
                    }
                }
            }
        }
    }

    // Top-level focus with nothing expanded → exit screen.
    vec![Write {
        path: oxpath!("ui", "settings", "_request_exit"),
        record: Record::parsed(Value::Bool(true)),
    }]
}

pub fn register(reg: &mut CommandRegistry) {
    reg.register(Box::new(TreeNext::new()));
    reg.register(Box::new(TreePrev::new()));
    reg.register(Box::new(TreeActivate::new()));
    reg.register(Box::new(TreeCollapseOrAscend::new()));
}

#[cfg(test)]
mod tests {
    use super::*;

    use ox_gate::{AccountConfig, ModelInfo, ModelInfoSource};
    use ox_types::{BadgeSource, SettingsIndexEntry};
    use structfs_serde_store::to_value;

    use crate::settings::command_registry::{Command, CommandCtx};
    use crate::settings::registry::RendererRegistry;
    use crate::settings::snapshot::SettingsSnapshot;

    fn run<C: Command>(cmd: &C, snap: &mut SettingsSnapshot) -> Vec<Write> {
        let registry = RendererRegistry::new();
        let ctx = CommandCtx {
            registry: &registry,
            last_keystroke: None,
        };
        cmd.run(snap, &ctx)
    }

    fn entry(id: &str, target: &str) -> SettingsIndexEntry {
        SettingsIndexEntry {
            id: id.to_string(),
            label: id.to_string(),
            description: String::new(),
            target_cursor: structfs_core_store::Path::parse(target).unwrap(),
            badge: BadgeSource::None,
        }
    }

    fn write_index(snap: &mut SettingsSnapshot) {
        snap.insert(
            &oxpath!("settings", "index", "entries", "accounts"),
            to_value(&entry("accounts", "settings/accounts")).unwrap(),
        );
        snap.insert(
            &oxpath!("settings", "index", "entries", "models"),
            to_value(&entry("models", "settings/models")).unwrap(),
        );
    }

    fn write_account(snap: &mut SettingsSnapshot, name: &str) {
        let comp = ox_kernel::PathComponent::try_new(name).unwrap();
        snap.insert(
            &oxpath!("config", "gate", "accounts", comp),
            to_value(&AccountConfig {
                provider: name.into(),
            })
            .unwrap(),
        );
    }

    fn write_account_with_models(snap: &mut SettingsSnapshot, name: &str, ids: &[&str]) {
        write_account(snap, name);
        let comp = ox_kernel::PathComponent::try_new(name).unwrap();
        let models: Vec<ModelInfo> = ids
            .iter()
            .map(|id| ModelInfo {
                id: (*id).into(),
                display_name: (*id).into(),
                max_context_size: None,
                max_output_tokens: None,
                source: ModelInfoSource::Server,
            })
            .collect();
        snap.insert(
            &oxpath!("config", "gate", "accounts", comp, "models"),
            to_value(&models).unwrap(),
        );
    }

    fn set_focused(snap: &mut SettingsSnapshot, target: &str) {
        snap.insert(
            &oxpath!("ui", "settings", "focused_row"),
            path_to_value(&structfs_core_store::Path::parse(target).unwrap()),
        );
    }

    fn read_focused_raw(snap: &mut SettingsSnapshot) -> Option<structfs_core_store::Path> {
        let r = snap
            .read(&oxpath!("ui", "settings", "focused_row"))
            .ok()
            .flatten()?;
        path_from_value(r.as_value()?)
    }

    fn read_expanded_raw(snap: &mut SettingsSnapshot) -> Vec<String> {
        read_expanded_set(snap)
    }

    // -- step ------------------------------------------------------------

    #[test]
    fn next_with_no_cursor_lands_on_first_row() {
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        let writes = run(&TreeNext::new(), &mut snap);
        // No cursor → current_idx = 0 → next = 1 (Models).
        assert_eq!(writes.len(), 1);
        let target = path_from_value(writes[0].record.as_value().unwrap()).unwrap();
        assert_eq!(target.to_string(), "settings/models");
    }

    #[test]
    fn next_wraps_at_end() {
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        set_focused(&mut snap, "settings/models");
        let writes = run(&TreeNext::new(), &mut snap);
        let target = path_from_value(writes[0].record.as_value().unwrap()).unwrap();
        assert_eq!(target.to_string(), "settings/accounts");
    }

    #[test]
    fn prev_wraps_at_start() {
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        set_focused(&mut snap, "settings/accounts");
        let writes = run(&TreePrev::new(), &mut snap);
        let target = path_from_value(writes[0].record.as_value().unwrap()).unwrap();
        assert_eq!(target.to_string(), "settings/models");
    }

    #[test]
    fn step_with_no_visible_rows_is_inert() {
        let mut snap = SettingsSnapshot::empty();
        let writes = run(&TreeNext::new(), &mut snap);
        assert!(writes.is_empty());
    }

    #[test]
    fn step_through_expanded_section() {
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        write_account(&mut snap, "alpha");
        write_account(&mut snap, "beta");
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&["settings/accounts".to_string()]),
        );
        // Visible: [Accounts, alpha, beta, Models]
        set_focused(&mut snap, "settings/accounts");
        let writes = run(&TreeNext::new(), &mut snap);
        let target = path_from_value(writes[0].record.as_value().unwrap()).unwrap();
        assert_eq!(target.to_string(), "settings/accounts/alpha");
    }

    #[test]
    fn step_with_stale_cursor_clamps_to_first() {
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        set_focused(&mut snap, "settings/nowhere");
        let writes = run(&TreeNext::new(), &mut snap);
        let target = path_from_value(writes[0].record.as_value().unwrap()).unwrap();
        // current_idx=0 (clamped), next=1
        assert_eq!(target.to_string(), "settings/models");
    }

    // -- activate --------------------------------------------------------

    #[test]
    fn activate_on_collapsed_entry_expands_it() {
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        set_focused(&mut snap, "settings/accounts");
        let writes = run(&TreeActivate::new(), &mut snap);
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "settings", "expanded"));
        // Apply the write so we can read it back.
        snap.insert(
            &writes[0].path,
            writes[0].record.as_value().unwrap().clone(),
        );
        let set = read_expanded_raw(&mut snap);
        assert_eq!(set, vec!["settings/accounts".to_string()]);
    }

    #[test]
    fn activate_on_expanded_entry_collapses_it() {
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&["settings/accounts".to_string()]),
        );
        set_focused(&mut snap, "settings/accounts");
        let writes = run(&TreeActivate::new(), &mut snap);
        snap.insert(
            &writes[0].path,
            writes[0].record.as_value().unwrap().clone(),
        );
        let set = read_expanded_raw(&mut snap);
        assert!(set.is_empty());
    }

    #[test]
    fn activate_on_account_row_toggles_expansion() {
        // Accounts are now expandable: Enter expands them inline to
        // reveal field rows, rather than descending into _detail.
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        write_account(&mut snap, "alpha");
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&["settings/accounts".to_string()]),
        );
        set_focused(&mut snap, "settings/accounts/alpha");
        let writes = run(&TreeActivate::new(), &mut snap);
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "settings", "expanded"));
        snap.insert(&writes[0].path, writes[0].record.as_value().unwrap().clone());
        let set = read_expanded_raw(&mut snap);
        assert!(set.contains(&"settings/accounts/alpha".to_string()));
    }

    #[test]
    fn activate_on_endpoint_field_enters_inline_edit_mode() {
        // Text-edit-able account fields (Endpoint, Key) switch to
        // inline edit mode: writes selection + field + edit_cursor +
        // edit_mode=true, and stays on `settings/index` (no cursor
        // write).
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        write_account(&mut snap, "alpha");
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&[
                "settings/accounts".to_string(),
                "settings/accounts/alpha".to_string(),
            ]),
        );
        set_focused(&mut snap, "settings/accounts/alpha/endpoint");
        let writes = run(&TreeActivate::new(), &mut snap);
        assert_eq!(writes.len(), 4);
        assert_eq!(
            writes[0].path,
            oxpath!("ui", "settings", "accounts", "selected")
        );
        assert_eq!(
            writes[1].path,
            oxpath!("ui", "settings", "account_detail", "field")
        );
        assert_eq!(writes[2].path, oxpath!("ui", "settings", "edit_cursor"));
        assert_eq!(writes[3].path, oxpath!("ui", "settings", "edit_mode"));
        match &writes[3].record {
            Record::Parsed(Value::Bool(true)) => {}
            other => panic!("expected edit_mode=true, got {other:?}"),
        }
        // Critically: no `cursor` write — the page stays at
        // `settings/index` while the user edits inline.
        for w in &writes {
            assert_ne!(w.path, oxpath!("ui", "settings", "cursor"));
        }
    }

    #[test]
    fn activate_on_protocol_field_drills_with_field_focused() {
        // Selectors (Name / Protocol / Auth) still drill into the
        // legacy detail editor — they need cycle-through edit logic
        // that doesn't fit the inline-text model.
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        write_account(&mut snap, "alpha");
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&[
                "settings/accounts".to_string(),
                "settings/accounts/alpha".to_string(),
            ]),
        );
        set_focused(&mut snap, "settings/accounts/alpha/protocol");
        let writes = run(&TreeActivate::new(), &mut snap);
        assert_eq!(writes.len(), 3);
        assert_eq!(
            writes[0].path,
            oxpath!("ui", "settings", "accounts", "selected")
        );
        assert_eq!(
            writes[1].path,
            oxpath!("ui", "settings", "account_detail", "field")
        );
        assert_eq!(writes[2].path, oxpath!("ui", "settings", "cursor"));
        let target = path_from_value(writes[2].record.as_value().unwrap()).unwrap();
        assert_eq!(target.to_string(), "settings/accounts/_detail");
    }

    #[test]
    fn activate_on_model_row_toggles_expansion() {
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        write_account_with_models(&mut snap, "alpha", &["m1"]);
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&["settings/models".to_string()]),
        );
        set_focused(&mut snap, "settings/models/alpha/m1");
        let writes = run(&TreeActivate::new(), &mut snap);
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "settings", "expanded"));
    }

    #[test]
    fn activate_on_model_field_drills_with_field_focused() {
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        write_account_with_models(&mut snap, "alpha", &["m1"]);
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&[
                "settings/models".to_string(),
                "settings/models/alpha/m1".to_string(),
            ]),
        );
        set_focused(&mut snap, "settings/models/alpha/m1/max_context_size");
        let writes = run(&TreeActivate::new(), &mut snap);
        assert_eq!(writes.len(), 3);
        assert_eq!(
            writes[0].path,
            oxpath!("ui", "settings", "models", "selected")
        );
        assert_eq!(
            writes[1].path,
            oxpath!("ui", "settings", "model_detail", "field")
        );
        assert_eq!(writes[2].path, oxpath!("ui", "settings", "cursor"));
        let target = path_from_value(writes[2].record.as_value().unwrap()).unwrap();
        assert_eq!(target.to_string(), "settings/models/_detail");
    }

    #[test]
    fn activate_with_no_cursor_is_inert() {
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        let writes = run(&TreeActivate::new(), &mut snap);
        assert!(writes.is_empty());
    }

    #[test]
    fn activate_with_no_visible_rows_is_inert() {
        let mut snap = SettingsSnapshot::empty();
        set_focused(&mut snap, "settings/accounts");
        let writes = run(&TreeActivate::new(), &mut snap);
        assert!(writes.is_empty());
    }

    #[test]
    fn activate_with_stale_cursor_is_inert() {
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        set_focused(&mut snap, "settings/nowhere");
        let writes = run(&TreeActivate::new(), &mut snap);
        assert!(writes.is_empty());
    }

    // -- collapse_or_ascend ---------------------------------------------

    #[test]
    fn collapse_focused_expanded_entry() {
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&["settings/accounts".to_string()]),
        );
        set_focused(&mut snap, "settings/accounts");
        let writes = run(&TreeCollapseOrAscend::new(), &mut snap);
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "settings", "expanded"));
        snap.insert(
            &writes[0].path,
            writes[0].record.as_value().unwrap().clone(),
        );
        assert!(read_expanded_raw(&mut snap).is_empty());
    }

    #[test]
    fn ascend_from_leaf_to_parent_entry() {
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        write_account(&mut snap, "alpha");
        snap.insert(
            &oxpath!("ui", "settings", "expanded"),
            expanded_set_to_value(&["settings/accounts".to_string()]),
        );
        set_focused(&mut snap, "settings/accounts/alpha");
        let writes = run(&TreeCollapseOrAscend::new(), &mut snap);
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "settings", "focused_row"));
        let target = path_from_value(writes[0].record.as_value().unwrap()).unwrap();
        assert_eq!(target.to_string(), "settings/accounts");
    }

    #[test]
    fn ascend_at_top_level_requests_exit() {
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        set_focused(&mut snap, "settings/accounts");
        let writes = run(&TreeCollapseOrAscend::new(), &mut snap);
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "settings", "_request_exit"));
        match &writes[0].record {
            Record::Parsed(Value::Bool(b)) => assert!(*b),
            other => panic!("unexpected record: {other:?}"),
        }
    }

    #[test]
    fn ascend_with_no_cursor_requests_exit() {
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        let writes = run(&TreeCollapseOrAscend::new(), &mut snap);
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "settings", "_request_exit"));
    }

    #[test]
    fn ascend_with_stale_cursor_requests_exit() {
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        set_focused(&mut snap, "settings/nowhere");
        let writes = run(&TreeCollapseOrAscend::new(), &mut snap);
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "settings", "_request_exit"));
    }

    // -- composite end-to-end -------------------------------------------

    #[test]
    fn expand_then_navigate_then_collapse_round_trip() {
        let mut snap = SettingsSnapshot::empty();
        write_index(&mut snap);
        write_account(&mut snap, "alpha");
        write_account(&mut snap, "beta");
        set_focused(&mut snap, "settings/accounts");

        // Expand
        let w = run(&TreeActivate::new(), &mut snap);
        snap.insert(&w[0].path, w[0].record.as_value().unwrap().clone());

        // j to first account
        let w = run(&TreeNext::new(), &mut snap);
        snap.insert(&w[0].path, w[0].record.as_value().unwrap().clone());
        assert_eq!(
            read_focused_raw(&mut snap).unwrap().to_string(),
            "settings/accounts/alpha"
        );

        // j to second account
        let w = run(&TreeNext::new(), &mut snap);
        snap.insert(&w[0].path, w[0].record.as_value().unwrap().clone());
        assert_eq!(
            read_focused_raw(&mut snap).unwrap().to_string(),
            "settings/accounts/beta"
        );

        // Esc → up to parent entry (we're on a leaf inside expanded section)
        let w = run(&TreeCollapseOrAscend::new(), &mut snap);
        snap.insert(&w[0].path, w[0].record.as_value().unwrap().clone());
        assert_eq!(
            read_focused_raw(&mut snap).unwrap().to_string(),
            "settings/accounts"
        );

        // Esc again → collapse the entry
        let w = run(&TreeCollapseOrAscend::new(), &mut snap);
        snap.insert(&w[0].path, w[0].record.as_value().unwrap().clone());
        assert!(read_expanded_raw(&mut snap).is_empty());

        // Esc again → exit screen
        let w = run(&TreeCollapseOrAscend::new(), &mut snap);
        assert_eq!(w[0].path, oxpath!("ui", "settings", "_request_exit"));
    }
}
