//! Navigation commands — descend into a sub-page; ascend back via the
//! renderer registry's `AscendRule`.
//!
//! - `NavDescendIndex`    — read highlighted entry's `target_cursor`,
//!                          write to `ui/settings/cursor`.
//! - `NavDescendAccounts` — write `ui/settings/cursor ←
//!                          settings/accounts/_detail`.
//! - `NavDescendModels`   — write `ui/settings/cursor ←
//!                          settings/models/_detail`.
//! - `NavAscend`          — consult `ctx.registry.ascend(&cursor)`. On
//!                          `Some(parent)` write parent to cursor; on `None`
//!                          (`AscendRule::ExitScreen`), write `true` to
//!                          `ui/settings/_request_exit` so the dispatch loop
//!                          can react in the next tick (per plan §L2).
//!
//! Per spec §6 binding tables.

use ox_path::oxpath;
use ox_types::SettingsIndexEntry;
use ox_types::Screen;
use ox_types::subscription::Write;
use structfs_core_store::{Path, Reader, Record, Value};

use crate::settings::command_registry::CommandRegistry;
use crate::settings::renderers::util::{child_names_under, read_typed};

/// Encode a `Path` as a `Value` matching the wire shape used by
/// `ox_types::path_serde` (a `Value::Array` of `Value::String` segments).
/// `Path` itself doesn't implement `Serialize`, so we hand-roll the encoding.
pub(crate) fn path_to_value(p: &Path) -> Value {
    Value::Array(p.components.iter().map(|c| Value::String(c.clone())).collect())
}

/// Decode a `Value` previously produced by `path_to_value` back into a
/// `Path`. Returns `None` on any shape mismatch.
pub(crate) fn path_from_value(v: &Value) -> Option<Path> {
    match v {
        Value::Array(items) => {
            let mut components: Vec<String> = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    Value::String(s) => components.push(s.clone()),
                    _ => return None,
                }
            }
            Path::try_from_components(components).ok()
        }
        _ => None,
    }
}

/// Read a `Path` previously written via `path_to_value`. Mirrors
/// `read_typed` but for Paths (which lack a Serialize impl).
fn read_path(data: &mut dyn Reader, path: &Path) -> Option<Path> {
    let record = match data.read(path) {
        Ok(Some(r)) => r,
        _ => return None,
    };
    let value = record.as_value()?;
    path_from_value(value)
}

#[allow(unused_imports)]
use super::command;

command! {
    struct_name: NavDescendIndex,
    id: "nav.descend.index",
    title: "Open Selected Settings Entry",
    description: "Descend into the highlighted index entry's target page.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "index")),
    run: |snap, _ctx| descend_index(snap),
}

command! {
    struct_name: NavDescendAccounts,
    id: "nav.descend.accounts",
    title: "Open Selected Account",
    description: "Descend into the account detail page.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "accounts")),
    run: |_snap, _ctx| vec![Write {
        path: oxpath!("ui", "settings", "cursor"),
        record: Record::parsed(path_to_value(&oxpath!("settings", "accounts", "_detail"))),
    }],
}

command! {
    struct_name: NavDescendModels,
    id: "nav.descend.models",
    title: "Open Selected Model",
    description: "Descend into the model detail page.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "models")),
    run: |_snap, _ctx| vec![Write {
        path: oxpath!("ui", "settings", "cursor"),
        record: Record::parsed(path_to_value(&oxpath!("settings", "models", "_detail"))),
    }],
}

command! {
    struct_name: NavAscend,
    id: "nav.ascend",
    title: "Go Back",
    description: "Ascend to the parent page; exit the screen at the root.",
    screen: Screen::Settings,
    cursor: None,
    run: |snap, ctx| ascend(snap, ctx),
}

fn descend_index(data: &mut dyn Reader) -> Vec<Write> {
    let entry_ids = child_names_under(data, "settings/index/entries");
    if entry_ids.is_empty() {
        return Vec::new();
    }
    let selected: usize =
        read_typed(data, &oxpath!("ui", "settings", "index", "selected")).unwrap_or(0);
    let id = match entry_ids.get(selected) {
        Some(s) => s,
        None => return Vec::new(),
    };
    let comp = match ox_kernel::PathComponent::try_new(id) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    let entry: SettingsIndexEntry =
        match read_typed(data, &oxpath!("settings", "index", "entries", comp)) {
            Some(e) => e,
            None => return Vec::new(),
        };
    vec![Write {
        path: oxpath!("ui", "settings", "cursor"),
        record: Record::parsed(path_to_value(&entry.target_cursor)),
    }]
}

fn ascend(
    data: &mut dyn Reader,
    ctx: &crate::settings::command_registry::CommandCtx<'_>,
) -> Vec<Write> {
    let cursor = match read_path(data, &oxpath!("ui", "settings", "cursor")) {
        Some(c) => c,
        None => return Vec::new(),
    };
    match ctx.registry.ascend(&cursor) {
        Some(parent) => vec![Write {
            path: oxpath!("ui", "settings", "cursor"),
            record: Record::parsed(path_to_value(&parent)),
        }],
        None => vec![Write {
            path: oxpath!("ui", "settings", "_request_exit"),
            record: Record::parsed(Value::Bool(true)),
        }],
    }
}

pub fn register(reg: &mut CommandRegistry) {
    reg.register(Box::new(NavDescendIndex::new()));
    reg.register(Box::new(NavDescendAccounts::new()));
    reg.register(Box::new(NavDescendModels::new()));
    reg.register(Box::new(NavAscend::new()));
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use structfs_serde_store::to_value;

    use ox_types::{BadgeSource, SettingsIndexEntry};

    use crate::settings::command_registry::{Command, CommandCtx};
    use crate::settings::registry::{AscendRule, RenderCtx, Renderer, RendererRegistry};
    use crate::settings::snapshot::SettingsSnapshot;

    /// Stub renderer used purely to seed the registry with an `AscendRule`.
    struct FakeRenderer(AscendRule);
    impl Renderer for FakeRenderer {
        fn render(&self, _ctx: &mut RenderCtx<'_>) -> ox_view::View {
            ox_view::View::Empty
        }
        fn ascend_to(&self) -> AscendRule {
            self.0
        }
    }

    fn run_with_registry<C: Command>(
        cmd: &C,
        snap: &mut SettingsSnapshot,
        registry: &RendererRegistry,
    ) -> Vec<Write> {
        let ctx = CommandCtx {
            registry,
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

    fn assert_path_write(
        writes: &[Write],
        path: structfs_core_store::Path,
        expected_target: structfs_core_store::Path,
    ) {
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, path);
        match &writes[0].record {
            Record::Parsed(v) => {
                let got = super::path_from_value(v).expect("Path");
                assert_eq!(got, expected_target);
            }
            other => panic!("unexpected record: {other:?}"),
        }
    }

    #[test]
    fn descend_index_writes_target_cursor() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("settings", "index", "entries", "accounts"),
            to_value(&entry("accounts", "settings/accounts")).unwrap(),
        );
        snap.insert(
            &oxpath!("ui", "settings", "index", "selected"),
            Value::Integer(0),
        );

        let registry = RendererRegistry::new();
        let writes = run_with_registry(&NavDescendIndex::new(), &mut snap, &registry);
        assert_path_write(
            &writes,
            oxpath!("ui", "settings", "cursor"),
            oxpath!("settings", "accounts"),
        );
    }

    #[test]
    fn descend_accounts_writes_detail_cursor() {
        let mut snap = SettingsSnapshot::empty();
        let registry = RendererRegistry::new();
        let writes = run_with_registry(&NavDescendAccounts::new(), &mut snap, &registry);
        assert_path_write(
            &writes,
            oxpath!("ui", "settings", "cursor"),
            oxpath!("settings", "accounts", "_detail"),
        );
    }

    #[test]
    fn descend_models_writes_detail_cursor() {
        let mut snap = SettingsSnapshot::empty();
        let registry = RendererRegistry::new();
        let writes = run_with_registry(&NavDescendModels::new(), &mut snap, &registry);
        assert_path_write(
            &writes,
            oxpath!("ui", "settings", "cursor"),
            oxpath!("settings", "models", "_detail"),
        );
    }

    #[test]
    fn ascend_at_nearest_registered_writes_parent() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "cursor"),
            super::path_to_value(&oxpath!("settings", "accounts", "_detail")),
        );

        let mut registry = RendererRegistry::new();
        registry.register(
            oxpath!("settings", "accounts"),
            Box::new(FakeRenderer(AscendRule::NearestRegistered)),
        );
        registry.register(
            oxpath!("settings", "accounts", "_detail"),
            Box::new(FakeRenderer(AscendRule::NearestRegistered)),
        );

        let writes = run_with_registry(&NavAscend::new(), &mut snap, &registry);
        assert_path_write(
            &writes,
            oxpath!("ui", "settings", "cursor"),
            oxpath!("settings", "accounts"),
        );
    }

    #[test]
    fn ascend_at_exit_screen_writes_request_exit() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "cursor"),
            super::path_to_value(&oxpath!("settings", "index")),
        );

        let mut registry = RendererRegistry::new();
        registry.register(
            oxpath!("settings", "index"),
            Box::new(FakeRenderer(AscendRule::ExitScreen)),
        );

        let writes = run_with_registry(&NavAscend::new(), &mut snap, &registry);
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "settings", "_request_exit"));
        match &writes[0].record {
            Record::Parsed(Value::Bool(b)) => assert!(*b),
            other => panic!("unexpected record: {other:?}"),
        }
    }
}
