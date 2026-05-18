//! Highlight commands — per-area selection cycling (next/prev with wrap).
//!
//! Three areas, two directions each:
//!
//! - **Index** — `ui/settings/index/selected: usize`, count from
//!   children of `settings/index/entries`.
//! - **Accounts** — `ui/settings/accounts/selected: Option<String>`, list
//!   from children of `config/gate/accounts`.
//! - **Models** — `ui/settings/models/selected: Option<ModelKey>`, list
//!   flattened from every account's `…/{name}/models`.
//!
//! Per spec §6.1 / §6.2 / §6.6. Empty-list cases are inert (`vec![]`); a
//! prev/next with no current selection starts at the first list element.

use ox_path::oxpath;
use ox_types::ModelKey;
use ox_types::subscription::Write;
use structfs_core_store::{Reader, Record};
use structfs_serde_store::to_value;

use crate::settings::CommandRegistry;
use crate::settings::renderers::util::{child_names_under, read_typed};

#[allow(unused_imports)]
use super::command;

// -- Index ------------------------------------------------------------------

command! {
    struct_name: HighlightIndexNext,
    id: "highlight.index.next",
    title: "Highlight Next (Index)",
    description: "Move the index selection to the next entry (wrap).",
    cursor: Some(oxpath!("settings", "index")),
    run: |snap, _ctx| index_step(snap, Direction::Next),
}

command! {
    struct_name: HighlightIndexPrev,
    id: "highlight.index.prev",
    title: "Highlight Prev (Index)",
    description: "Move the index selection to the previous entry (wrap).",
    cursor: Some(oxpath!("settings", "index")),
    run: |snap, _ctx| index_step(snap, Direction::Prev),
}

// -- Accounts ---------------------------------------------------------------

command! {
    struct_name: HighlightAccountsNext,
    id: "highlight.accounts.next",
    title: "Highlight Next (Connections)",
    description: "Move the Connections selection to the next Connection (wrap).",
    cursor: Some(oxpath!("settings", "accounts")),
    run: |snap, _ctx| accounts_step(snap, Direction::Next),
}

command! {
    struct_name: HighlightAccountsPrev,
    id: "highlight.accounts.prev",
    title: "Highlight Prev (Connections)",
    description: "Move the Connections selection to the previous Connection (wrap).",
    cursor: Some(oxpath!("settings", "accounts")),
    run: |snap, _ctx| accounts_step(snap, Direction::Prev),
}

// -- Models -----------------------------------------------------------------

command! {
    struct_name: HighlightModelsNext,
    id: "highlight.models.next",
    title: "Highlight Next (Models)",
    description: "Move the models selection to the next (account, model) pair (wrap).",
    cursor: Some(oxpath!("settings", "models")),
    run: |snap, _ctx| models_step(snap, Direction::Next),
}

command! {
    struct_name: HighlightModelsPrev,
    id: "highlight.models.prev",
    title: "Highlight Prev (Models)",
    description: "Move the models selection to the previous (account, model) pair (wrap).",
    cursor: Some(oxpath!("settings", "models")),
    run: |snap, _ctx| models_step(snap, Direction::Prev),
}

// -- Shared helpers ---------------------------------------------------------

#[derive(Clone, Copy)]
enum Direction {
    Next,
    Prev,
}

/// Compute the next/prev index in a 0..count circle starting from `current`.
/// Caller guarantees `count > 0`.
fn step_index(current: usize, count: usize, direction: Direction) -> usize {
    debug_assert!(count > 0);
    let current = current.min(count - 1);
    match direction {
        Direction::Next => (current + 1) % count,
        Direction::Prev => (current + count - 1) % count,
    }
}

fn index_step(data: &mut dyn Reader, direction: Direction) -> Vec<Write> {
    let count = child_names_under(data, "settings/index/entries").len();
    if count == 0 {
        return Vec::new();
    }
    let current: usize =
        read_typed(data, &oxpath!("ui", "settings", "index", "selected")).unwrap_or(0);
    let next = step_index(current, count, direction);
    let value = match to_value(&next) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "highlight: failed to encode index selection");
            return Vec::new();
        }
    };
    vec![Write {
        path: oxpath!("ui", "settings", "index", "selected"),
        record: Record::parsed(value),
    }]
}

fn accounts_step(data: &mut dyn Reader, direction: Direction) -> Vec<Write> {
    let names = child_names_under(data, "config/gate/accounts");
    if names.is_empty() {
        return Vec::new();
    }
    let current =
        read_typed::<Option<String>>(data, &oxpath!("ui", "settings", "accounts", "selected"))
            .flatten();
    let current_idx = current
        .as_ref()
        .and_then(|n| names.iter().position(|x| x == n))
        .unwrap_or(0);
    let next_idx = if current.is_none() {
        // No selection yet — land on the first.
        0
    } else {
        step_index(current_idx, names.len(), direction)
    };
    let chosen = names[next_idx].clone();
    let value = match to_value(&Some(chosen)) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "highlight: failed to encode accounts selection");
            return Vec::new();
        }
    };
    vec![Write {
        path: oxpath!("ui", "settings", "accounts", "selected"),
        record: Record::parsed(value),
    }]
}

/// Flatten every `(account, model_id)` pair across the snapshot. Order is
/// account-order (per `child_names_under`) then per-account model-list order.
fn flatten_model_keys(data: &mut dyn Reader) -> Vec<ModelKey> {
    let names = child_names_under(data, "config/gate/accounts");
    let mut out: Vec<ModelKey> = Vec::new();
    for name in &names {
        let comp = match ox_kernel::PathComponent::try_new(name) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let models: Vec<ox_gate::ModelInfo> =
            read_typed(data, &oxpath!("config", "gate", "accounts", comp, "models"))
                .unwrap_or_default();
        for m in models {
            out.push(ModelKey {
                account: name.clone(),
                model_id: m.id,
            });
        }
    }
    out
}

fn models_step(data: &mut dyn Reader, direction: Direction) -> Vec<Write> {
    let keys = flatten_model_keys(data);
    if keys.is_empty() {
        return Vec::new();
    }
    let current =
        read_typed::<Option<ModelKey>>(data, &oxpath!("ui", "settings", "models", "selected"))
            .flatten();
    let current_idx = current
        .as_ref()
        .and_then(|k| {
            keys.iter()
                .position(|x| x.account == k.account && x.model_id == k.model_id)
        })
        .unwrap_or(0);
    let next_idx = if current.is_none() {
        0
    } else {
        step_index(current_idx, keys.len(), direction)
    };
    let chosen = keys[next_idx].clone();
    let value = match to_value(&Some(chosen)) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "highlight: failed to encode models selection");
            return Vec::new();
        }
    };
    vec![Write {
        path: oxpath!("ui", "settings", "models", "selected"),
        record: Record::parsed(value),
    }]
}

// -- Registration -----------------------------------------------------------

pub fn register(reg: &mut CommandRegistry) {
    reg.register(Box::new(HighlightIndexNext::new()));
    reg.register(Box::new(HighlightIndexPrev::new()));
    reg.register(Box::new(HighlightAccountsNext::new()));
    reg.register(Box::new(HighlightAccountsPrev::new()));
    reg.register(Box::new(HighlightModelsNext::new()));
    reg.register(Box::new(HighlightModelsPrev::new()));
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use structfs_core_store::Value;
    use structfs_serde_store::to_value;

    use ox_gate::AccountConfig;
    use ox_gate::{ModelInfo, ModelInfoSource};
    use ox_types::SettingsIndexEntry;

    use crate::settings::RendererRegistry;
    use crate::settings::snapshot::SettingsSnapshot;
    use crate::settings::{Command, CommandCtx};

    fn empty_ctx<'a>(registry: &'a RendererRegistry) -> CommandCtx<'a> {
        CommandCtx {
            registry,
            last_keystroke: None,
        }
    }

    fn run<C: Command>(cmd: &C, snap: &mut SettingsSnapshot) -> Vec<Write> {
        let renderers = RendererRegistry::new();
        let ctx = empty_ctx(&renderers);
        cmd.run(snap, &ctx)
    }

    fn entry(id: &str, target: &str) -> SettingsIndexEntry {
        SettingsIndexEntry {
            id: id.to_string(),
            label: id.to_string(),
            description: String::new(),
            target_cursor: structfs_core_store::Path::parse(target).unwrap(),
            badge: ox_types::BadgeSource::None,
        }
    }

    fn write_three_entries(snap: &mut SettingsSnapshot) {
        snap.insert(
            &oxpath!("settings", "index", "entries", "a"),
            to_value(&entry("a", "settings/a")).unwrap(),
        );
        snap.insert(
            &oxpath!("settings", "index", "entries", "b"),
            to_value(&entry("b", "settings/b")).unwrap(),
        );
        snap.insert(
            &oxpath!("settings", "index", "entries", "c"),
            to_value(&entry("c", "settings/c")).unwrap(),
        );
    }

    fn write_account(snap: &mut SettingsSnapshot, name: &str) {
        let comp = ox_kernel::PathComponent::try_new(name).unwrap();
        snap.insert(
            &oxpath!("config", "gate", "accounts", comp),
            to_value(&AccountConfig {
                provider: name.into(),
                ..Default::default()
            })
            .unwrap(),
        );
    }

    fn write_account_with_models(snap: &mut SettingsSnapshot, name: &str, model_ids: &[&str]) {
        write_account(snap, name);
        let comp = ox_kernel::PathComponent::try_new(name).unwrap();
        let models: Vec<ModelInfo> = model_ids
            .iter()
            .map(|id| ModelInfo {
                id: (*id).to_string(),
                display_name: (*id).to_string(),
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

    fn assert_usize_write(writes: &[Write], path: structfs_core_store::Path, expected: usize) {
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, path);
        match &writes[0].record {
            Record::Parsed(v) => {
                let got: usize = structfs_serde_store::from_value(v.clone()).expect("usize");
                assert_eq!(got, expected);
            }
            other => panic!("unexpected record: {other:?}"),
        }
    }

    fn assert_optstr_write(writes: &[Write], path: structfs_core_store::Path, expected: &str) {
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, path);
        match &writes[0].record {
            Record::Parsed(v) => {
                let got: Option<String> =
                    structfs_serde_store::from_value(v.clone()).expect("Option<String>");
                assert_eq!(got.as_deref(), Some(expected));
            }
            other => panic!("unexpected record: {other:?}"),
        }
    }

    fn assert_modelkey_write(
        writes: &[Write],
        path: structfs_core_store::Path,
        expected_account: &str,
        expected_model: &str,
    ) {
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, path);
        match &writes[0].record {
            Record::Parsed(v) => {
                let got: Option<ModelKey> =
                    structfs_serde_store::from_value(v.clone()).expect("Option<ModelKey>");
                let got = got.expect("Some");
                assert_eq!(got.account, expected_account);
                assert_eq!(got.model_id, expected_model);
            }
            other => panic!("unexpected record: {other:?}"),
        }
    }

    // -- Index ------------------------------------------------------------------

    #[test]
    fn next_wraps_at_end_for_index() {
        let mut snap = SettingsSnapshot::empty();
        write_three_entries(&mut snap);
        snap.insert(
            &oxpath!("ui", "settings", "index", "selected"),
            Value::Integer(2),
        );

        let writes = run(&HighlightIndexNext::new(), &mut snap);
        assert_usize_write(&writes, oxpath!("ui", "settings", "index", "selected"), 0);
    }

    #[test]
    fn prev_wraps_at_start_for_index() {
        let mut snap = SettingsSnapshot::empty();
        write_three_entries(&mut snap);
        snap.insert(
            &oxpath!("ui", "settings", "index", "selected"),
            Value::Integer(0),
        );

        let writes = run(&HighlightIndexPrev::new(), &mut snap);
        assert_usize_write(&writes, oxpath!("ui", "settings", "index", "selected"), 2);
    }

    #[test]
    fn next_is_noop_on_empty_index() {
        let mut snap = SettingsSnapshot::empty();
        let writes = run(&HighlightIndexNext::new(), &mut snap);
        assert!(writes.is_empty());
    }

    // -- Accounts ---------------------------------------------------------------

    #[test]
    fn next_cycles_account_names() {
        let mut snap = SettingsSnapshot::empty();
        write_account(&mut snap, "alpha");
        write_account(&mut snap, "beta");
        write_account(&mut snap, "gamma");
        snap.insert(
            &oxpath!("ui", "settings", "accounts", "selected"),
            to_value(&Some("alpha".to_string())).unwrap(),
        );
        let writes = run(&HighlightAccountsNext::new(), &mut snap);
        assert_optstr_write(
            &writes,
            oxpath!("ui", "settings", "accounts", "selected"),
            "beta",
        );
    }

    #[test]
    fn prev_cycles_account_names() {
        let mut snap = SettingsSnapshot::empty();
        write_account(&mut snap, "alpha");
        write_account(&mut snap, "beta");
        write_account(&mut snap, "gamma");
        snap.insert(
            &oxpath!("ui", "settings", "accounts", "selected"),
            to_value(&Some("alpha".to_string())).unwrap(),
        );
        let writes = run(&HighlightAccountsPrev::new(), &mut snap);
        assert_optstr_write(
            &writes,
            oxpath!("ui", "settings", "accounts", "selected"),
            "gamma",
        );
    }

    #[test]
    fn next_starts_at_first_when_no_selection() {
        let mut snap = SettingsSnapshot::empty();
        write_account(&mut snap, "alpha");
        write_account(&mut snap, "beta");
        // No selection set.
        let writes = run(&HighlightAccountsNext::new(), &mut snap);
        assert_optstr_write(
            &writes,
            oxpath!("ui", "settings", "accounts", "selected"),
            "alpha",
        );
    }

    // -- Models -----------------------------------------------------------------

    #[test]
    fn next_cycles_model_keys() {
        let mut snap = SettingsSnapshot::empty();
        write_account_with_models(&mut snap, "alpha", &["m1"]);
        write_account_with_models(&mut snap, "beta", &["m2", "m3"]);
        snap.insert(
            &oxpath!("ui", "settings", "models", "selected"),
            to_value(&Some(ModelKey {
                account: "alpha".into(),
                model_id: "m1".into(),
            }))
            .unwrap(),
        );
        let writes = run(&HighlightModelsNext::new(), &mut snap);
        assert_modelkey_write(
            &writes,
            oxpath!("ui", "settings", "models", "selected"),
            "beta",
            "m2",
        );
    }

    #[test]
    fn prev_cycles_model_keys() {
        let mut snap = SettingsSnapshot::empty();
        write_account_with_models(&mut snap, "alpha", &["m1"]);
        write_account_with_models(&mut snap, "beta", &["m2", "m3"]);
        snap.insert(
            &oxpath!("ui", "settings", "models", "selected"),
            to_value(&Some(ModelKey {
                account: "alpha".into(),
                model_id: "m1".into(),
            }))
            .unwrap(),
        );
        let writes = run(&HighlightModelsPrev::new(), &mut snap);
        assert_modelkey_write(
            &writes,
            oxpath!("ui", "settings", "models", "selected"),
            "beta",
            "m3",
        );
    }

    #[test]
    fn next_starts_at_first_model_when_no_selection() {
        let mut snap = SettingsSnapshot::empty();
        write_account_with_models(&mut snap, "alpha", &["m1"]);
        write_account_with_models(&mut snap, "beta", &["m2"]);
        // No selection set.
        let writes = run(&HighlightModelsNext::new(), &mut snap);
        assert_modelkey_write(
            &writes,
            oxpath!("ui", "settings", "models", "selected"),
            "alpha",
            "m1",
        );
    }
}
