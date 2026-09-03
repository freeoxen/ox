//! Navigation commands — ascend back via the renderer registry's
//! `AscendRule`.
//!
//! Cursor model: there is one cursor, `ui/settings/focused`. It's the
//! single source of truth for both dispatch scope and renderer
//! ancestry. When the user focuses a sub-widget (e.g.
//! `settings/_compose_form/name`), `NavAscend` walks the registry to
//! find the innermost registered ancestor (the "page") and then asks
//! the registry to ascend from that page. The "page cursor" is implicit
//! in the focus cursor's ancestry; there is no separate page-cursor
//! state path.
//!
//! - `NavAscend` — read `focused`, resolve its innermost registered
//!   ancestor via `registry.registered_ancestor_or_self`, then consult
//!   `registry.ascend(<that page>)`. On `Some(parent)` write the parent
//!   back to `focused`. On `None` (any rule that resolves to no parent
//!   — `ExitScreen`, an unregistered `Fallback` target, or
//!   `NearestRegistered` at the root) write `true` to
//!   `ui/settings/_request_exit`.

use ox_path::oxpath;
use ox_types::subscription::Write;
use structfs_core_store::{Path, Reader, Record, Value};

use crate::settings::CommandRegistry;

/// Encode a `Path` as a `Value` matching the wire shape used by
/// `ox_types::path_serde` (a `Value::Array` of `Value::String` segments).
/// `Path` itself doesn't implement `Serialize`, so we hand-roll the encoding.
pub fn path_to_value(p: &Path) -> Value {
    Value::Array(p.iter().map(|c| Value::String(c.clone())).collect())
}

/// Decode a `Value` previously produced by `path_to_value` back into a
/// `Path`. Returns `None` on any shape mismatch.
pub fn path_from_value(v: &Value) -> Option<Path> {
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
    struct_name: NavAscend,
    id: "nav.ascend",
    title: "Go Back",
    description: "Ascend to the parent page; exit the screen at the root.",
    cursor: None,
    run: |snap, ctx| ascend(snap, ctx),
}

fn ascend(data: &mut dyn Reader, ctx: &crate::settings::CommandCtx<'_>) -> Vec<Write> {
    let focused = match read_path(data, &oxpath!("ui", "settings", "focused")) {
        Some(c) => c,
        None => return Vec::new(),
    };
    // The focus cursor may sit on a sub-widget below a registered page
    // (e.g. settings/_compose_form/name beneath settings/index). Ask
    // the registry which page owns this focus before applying the
    // page's AscendRule.
    let page = match ctx.registry.registered_ancestor_or_self(&focused) {
        Some(p) => p,
        None => return Vec::new(),
    };
    match ctx.registry.ascend(&page) {
        Some(parent) => vec![Write {
            path: oxpath!("ui", "settings", "focused"),
            record: Record::parsed(path_to_value(&parent)),
        }],
        None => vec![Write {
            path: oxpath!("ui", "settings", "_request_exit"),
            record: Record::parsed(Value::Bool(true)),
        }],
    }
}

command! {
    struct_name: NavExitScreen,
    id: "nav.exit_screen",
    title: "Exit Settings",
    description: "Leave the settings screen unconditionally, regardless of cursor depth.",
    cursor: None,
    run: |_snap, _ctx| exit_screen(),
}

fn exit_screen() -> Vec<Write> {
    vec![Write {
        path: oxpath!("ui", "settings", "_request_exit"),
        record: Record::parsed(Value::Bool(true)),
    }]
}

pub fn register(reg: &mut CommandRegistry) {
    reg.register(Box::new(NavAscend::new()));
    reg.register(Box::new(NavExitScreen::new()));
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use crate::settings::snapshot::SettingsSnapshot;
    use crate::settings::{AscendRule, RenderCtx, Renderer, RendererRegistry};
    use crate::settings::{Command, CommandCtx};

    /// Stub renderer used purely to seed the registry with an `AscendRule`.
    struct FakeRenderer(AscendRule);
    impl Renderer for FakeRenderer {
        fn render(&self, _ctx: &mut RenderCtx<'_>) -> horns_core::view::View {
            horns_core::view::View::Empty
        }
        fn ascend_to(&self) -> AscendRule {
            self.0.clone()
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
    fn ascend_at_nearest_registered_writes_parent() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "focused"),
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
            oxpath!("ui", "settings", "focused"),
            oxpath!("settings", "accounts"),
        );
    }

    #[test]
    fn ascend_from_subwidget_uses_pages_ascend_rule() {
        // Focus on a sub-widget below settings/index (no renderer
        // registered at the sub-widget path). Esc must consult the
        // page's AscendRule, not the sub-widget's missing one.
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "focused"),
            super::path_to_value(&oxpath!("settings", "index", "_compose_form", "name")),
        );

        let mut registry = RendererRegistry::new();
        registry.register(
            oxpath!("settings", "index"),
            Box::new(FakeRenderer(AscendRule::ExitScreen)),
        );

        let writes = run_with_registry(&NavAscend::new(), &mut snap, &registry);
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "settings", "_request_exit"));
    }

    #[test]
    fn ascend_top_level_page_falls_back_to_settings_index() {
        // Cursor at `settings/accounts` (a top-level page) whose renderer
        // declares `Fallback(settings/index)`. Esc here should land on
        // `settings/index`, not exit the screen.
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "focused"),
            super::path_to_value(&oxpath!("settings", "accounts")),
        );

        let mut registry = RendererRegistry::new();
        registry.register(
            oxpath!("settings", "index"),
            Box::new(FakeRenderer(AscendRule::ExitScreen)),
        );
        registry.register(
            oxpath!("settings", "accounts"),
            Box::new(FakeRenderer(AscendRule::Fallback(oxpath!(
                "settings", "index"
            )))),
        );

        let writes = run_with_registry(&NavAscend::new(), &mut snap, &registry);
        assert_path_write(
            &writes,
            oxpath!("ui", "settings", "focused"),
            oxpath!("settings", "index"),
        );
    }

    #[test]
    fn ascend_at_exit_screen_writes_request_exit() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "settings", "focused"),
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
