//! Hierarchical key dispatch over the cursor's ancestor chain.
//!
//! The [`Dispatcher`] resolves a keystroke by walking a *scope path* — the
//! chain of nested scopes from the outermost (the page) to the innermost
//! (the focused leaf widget). Three phases run in order:
//!
//! 1. **Capture** (outer → inner): container-owned lifecycle keys that
//!    fire before the focused leaf sees them.
//! 2. **Target** (leaf only): the focused leaf claims the key.
//! 3. **Bubble** (inner → outer): container fallbacks that fire only
//!    when the leaf didn't consume the key.
//!
//! First match wins. Inert (empty Vec) on no match.
//!
//! The scope path is the cursor's ancestor chain: for cursor
//! `settings/_compose_form/name` the path is `[settings,
//! settings/_compose_form, settings/_compose_form/name]`. Cursor-as-focus
//! is the only source of scope; mutual exclusion between compound widgets
//! is structural (the cursor is a single path; only one widget's prefix
//! can be on its ancestry).
//!
//! When the focus cursor is not yet seeded, the scope path falls back to
//! the immediate parent of `cursor_path` (the host's screen-root scope)
//! so page-level Bubble bindings (j/k navigation, etc.) remain reachable
//! on first entry, before any row has been focused. Pressing j once writes
//! a real cursor; from then on the ancestor chain supplies the scope path
//! naturally.

use structfs_core_store::{Path, Reader, Value};

use crate::binding::{BindingRegistry, BindingScope, Phase};
use crate::command::{CommandCtx, CommandRegistry};
use crate::key::KeyChord;
use crate::render::RendererRegistry;
use crate::write::Write;

/// One horns instance's dispatcher. Constructed at install time with
/// the cursor path it watches; `dispatch` reads that path on every call
/// to derive the scope path.
pub struct Dispatcher {
    cursor_path: Path,
}

impl Dispatcher {
    /// Build a dispatcher that reads its focus cursor from `cursor_path`.
    /// The path's parent is used as the screen-root scope fallback when
    /// no cursor has been seeded yet.
    pub fn new(cursor_path: Path) -> Self {
        Self { cursor_path }
    }

    /// The cursor path this dispatcher reads on every dispatch call.
    pub fn cursor_path(&self) -> &Path {
        &self.cursor_path
    }

    /// Resolve `(focus, key)` to a sequence of writes by looking up the
    /// binding, then the command, then running it. Returns `vec![]`
    /// (inert) on any miss. Three-phase walk: Capture (outer → inner),
    /// Target (leaf only), Bubble (inner → outer).
    pub fn dispatch(
        &self,
        snapshot: &mut dyn Reader,
        key: &KeyChord,
        bindings: &BindingRegistry,
        commands: &CommandRegistry,
        renderers: &RendererRegistry,
    ) -> Vec<Write> {
        let scope_path = self.compute_scope_path(snapshot);

        // Capture (outer → inner): containers claim lifecycle keys
        // before the leaf sees them. At each scope, the discrete tier
        // is asked first; on miss, the handler tier is asked at the
        // same scope+phase.
        let mut cmd_id_opt = None;
        for scope in &scope_path {
            if let Some(p) = scope.keyed_path() {
                if let Some(hit) = bindings.lookup(p, key, Phase::Capture) {
                    cmd_id_opt = Some(hit);
                    break;
                }
                if let Some(h) = bindings.lookup_handler(p, key, Phase::Capture) {
                    let ctx = CommandCtx {
                        registry: renderers,
                        last_keystroke: Some(key.clone()),
                    };
                    if let Some(writes) = h.handle(snapshot, key, &ctx) {
                        return writes;
                    }
                }
            }
        }

        // Target (leaf only): the innermost scope claims the key.
        // Discrete first, then handler.
        if cmd_id_opt.is_none() {
            if let Some(leaf) = scope_path.last().and_then(BindingScope::keyed_path) {
                cmd_id_opt = bindings.lookup(leaf, key, Phase::Target);
                if cmd_id_opt.is_none() {
                    if let Some(h) = bindings.lookup_handler(leaf, key, Phase::Target) {
                        let ctx = CommandCtx {
                            registry: renderers,
                            last_keystroke: Some(key.clone()),
                        };
                        if let Some(writes) = h.handle(snapshot, key, &ctx) {
                            return writes;
                        }
                    }
                }
            }
        }

        // Bubble (inner → outer): containers handle keys the leaf didn't
        // consume. Outer-scope defaults (page-cursor j/k, focused-row
        // a/t/r/d, whole-screen ?, compose Enter, ...) declare
        // Phase::Bubble directly — no per-scope Target fallback.
        if cmd_id_opt.is_none() {
            for scope in scope_path.iter().rev() {
                let Some(p) = scope.keyed_path() else {
                    continue;
                };
                if let Some(hit) = bindings.lookup(p, key, Phase::Bubble) {
                    cmd_id_opt = Some(hit);
                    break;
                }
                if let Some(h) = bindings.lookup_handler(p, key, Phase::Bubble) {
                    let ctx = CommandCtx {
                        registry: renderers,
                        last_keystroke: Some(key.clone()),
                    };
                    if let Some(writes) = h.handle(snapshot, key, &ctx) {
                        return writes;
                    }
                }
            }
        }

        let Some(cmd_id) = cmd_id_opt else {
            return vec![];
        };
        let Some(command) = commands.lookup(cmd_id) else {
            return vec![];
        };
        let ctx = CommandCtx {
            registry: renderers,
            last_keystroke: Some(key.clone()),
        };
        command.run(snapshot, &ctx)
    }

    /// The scope path the dispatcher walks: each entry is a
    /// `BindingScope::Exact` over a progressively-longer prefix of the
    /// focus cursor.
    ///
    /// `pub(crate)` so the dispatcher's tests can assert the returned
    /// ordering directly.
    pub(crate) fn compute_scope_path(&self, snapshot: &mut dyn Reader) -> Vec<BindingScope> {
        match read_focus_cursor(snapshot, &self.cursor_path) {
            Some(cursor) => path_ancestors(&cursor)
                .into_iter()
                .map(BindingScope::Exact)
                .collect(),
            // No cursor set (e.g., first entry into a screen before any
            // row has been focused). Fall back to the screen-root scope
            // — the parent of the watched cursor path — so page-level
            // Bubble bindings (j/k navigation, etc.) remain reachable.
            // Pressing j once writes a real cursor; from then on the
            // ancestor chain supplies the scope path naturally.
            None => screen_root_fallback(&self.cursor_path),
        }
    }
}

/// Read the focus cursor at `cursor_path` from the snapshot. The cursor
/// is encoded as `Value::Array` of `Value::String` segments — the wire
/// shape produced by the navigation commands' `path_to_value` helper.
fn read_focus_cursor(snapshot: &mut dyn Reader, cursor_path: &Path) -> Option<Path> {
    let record = snapshot.read(cursor_path).ok().flatten()?;
    let value = record.as_value()?;
    path_from_value(value)
}

/// Decode a `Value` produced by `path_to_value` (a `Value::Array` of
/// `Value::String` segments) back into a `Path`. Returns `None` on any
/// shape mismatch.
fn path_from_value(v: &Value) -> Option<Path> {
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

/// The screen-root scope used when no focus cursor has been seeded. By
/// convention the focus cursor is one level deeper than the screen root
/// (e.g. `ui/settings/focused` → screen root is `settings`); we recover
/// the screen root from the cursor path's tail.
///
/// The fallback is `[Exact(screen_root)]` — a single-entry path so the
/// Bubble walk has at least one scope to check. Empty `cursor_path` →
/// empty fallback (treated as no dispatch context).
fn screen_root_fallback(cursor_path: &Path) -> Vec<BindingScope> {
    // `ui/settings/focused` → `settings`. The last component is the
    // focus key (e.g. `focused`); the middle ones are the screen
    // namespace; the first (`ui`) is the host's UI prefix. We want the
    // screen segment — the second-to-last component — as the root scope.
    // For a generic dispatcher we instead derive it as "the cursor path
    // minus the host prefix and the focus key", which today is the
    // single middle component.
    //
    // Concretely: for `ui/<screen>/focused`, return `[Exact(<screen>)]`.
    // For shapes that don't fit this template, return `[]` — the
    // dispatch becomes inert when no cursor is seeded, which is the
    // safe default.
    if cursor_path.components.len() < 2 {
        return Vec::new();
    }
    let screen = &cursor_path.components[cursor_path.components.len() - 2];
    match Path::try_from_components(vec![screen.clone()]) {
        Ok(p) => vec![BindingScope::Exact(p)],
        Err(_) => Vec::new(),
    }
}

/// Build the progressively-longer prefixes of `path`, ending at `path`
/// itself. `path_ancestors(settings/_compose_form/name) = [settings,
/// settings/_compose_form, settings/_compose_form/name]`. Empty paths
/// yield an empty vec.
pub fn path_ancestors(path: &Path) -> Vec<Path> {
    (1..=path.components.len())
        .map(|end| path.slice(0, end))
        .collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::HashMap;

    use ox_path::oxpath;
    use structfs_core_store::{Error, Record, Value};

    use crate::binding::{BindingEntry, BindingScope, Phase};
    use crate::command::{Command, CommandCtx, CommandDisplay, CommandId, CommandScope};
    use crate::key::{KeyChord, KeyCodeRepr, KeyModifierSet};
    use crate::render::RendererRegistry;
    use crate::write::Write;

    // -----------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------

    /// Minimal in-process Reader+Writer that backs tests for the
    /// dispatcher without pulling in `ox_store_util::LocalConfig`. Keeps
    /// records by their full `Path::components` key.
    #[derive(Default)]
    struct MapReader {
        records: HashMap<Vec<String>, Record>,
    }

    impl MapReader {
        fn insert(&mut self, path: &Path, record: Record) {
            self.records.insert(path.components.clone(), record);
        }
    }

    impl Reader for MapReader {
        fn read(&mut self, path: &Path) -> Result<Option<Record>, Error> {
            Ok(self.records.get(&path.components).cloned())
        }
    }

    /// Encode a `Path` as a `Value` matching the wire shape used by the
    /// navigation commands' `path_to_value` helper (a `Value::Array` of
    /// `Value::String` segments). Mirrors the helper in
    /// `ox-cli::settings::commands::navigation` so the tests exercise the
    /// same encoding the dispatcher reads.
    fn path_to_value(p: &Path) -> Value {
        Value::Array(
            p.components
                .iter()
                .map(|c| Value::String(c.clone()))
                .collect(),
        )
    }

    /// The focus cursor path used by every dispatcher test. Matches the
    /// settings screen's `ui/settings/focused` so the screen-root
    /// fallback resolves to `settings`.
    fn focus_path() -> Path {
        oxpath!("ui", "settings", "focused")
    }

    fn dispatcher() -> Dispatcher {
        Dispatcher::new(focus_path())
    }

    fn seed_focused(reader: &mut MapReader, focused: &Path) {
        reader.insert(&focus_path(), Record::parsed(path_to_value(focused)));
    }

    fn key_char(c: char) -> KeyChord {
        KeyChord {
            modifiers: KeyModifierSet::default(),
            code: KeyCodeRepr::Char(c),
        }
    }

    fn cmd_id(s: &str) -> CommandId {
        CommandId(s.to_string())
    }

    /// Marker command that writes a fixed sentinel string to a fixed path.
    struct WriteSentinel {
        id: CommandId,
        display: CommandDisplay,
        scope: CommandScope,
    }

    impl WriteSentinel {
        fn new() -> Self {
            Self {
                id: cmd_id("test.sentinel"),
                display: CommandDisplay {
                    name: "Sentinel".to_string(),
                    description: "writes sentinel".to_string(),
                },
                scope: CommandScope { cursor_path: None },
            }
        }
    }

    impl Command for WriteSentinel {
        fn id(&self) -> &CommandId {
            &self.id
        }
        fn display(&self) -> &CommandDisplay {
            &self.display
        }
        fn scope(&self) -> &CommandScope {
            &self.scope
        }
        fn run(&self, _snapshot: &mut dyn Reader, _ctx: &CommandCtx<'_>) -> Vec<Write> {
            vec![Write {
                path: oxpath!("ui", "sentinel"),
                record: Record::parsed(Value::String("ran".into())),
            }]
        }
    }

    /// Command that mirrors the dispatched keystroke into a path so the
    /// test can verify dispatch wired `last_keystroke` through.
    struct ReportKeystroke {
        id: CommandId,
        display: CommandDisplay,
        scope: CommandScope,
    }

    impl ReportKeystroke {
        fn new() -> Self {
            Self {
                id: cmd_id("test.report_keystroke"),
                display: CommandDisplay {
                    name: "Report Keystroke".to_string(),
                    description: "writes ctx.last_keystroke char".to_string(),
                },
                scope: CommandScope { cursor_path: None },
            }
        }
    }

    impl Command for ReportKeystroke {
        fn id(&self) -> &CommandId {
            &self.id
        }
        fn display(&self) -> &CommandDisplay {
            &self.display
        }
        fn scope(&self) -> &CommandScope {
            &self.scope
        }
        fn run(&self, _snapshot: &mut dyn Reader, ctx: &CommandCtx<'_>) -> Vec<Write> {
            let s = match &ctx.last_keystroke {
                Some(KeyChord {
                    code: KeyCodeRepr::Char(c),
                    ..
                }) => c.to_string(),
                Some(_) => "non_char".to_string(),
                None => "none".to_string(),
            };
            vec![Write {
                path: oxpath!("ui", "last_key"),
                record: Record::parsed(Value::String(s)),
            }]
        }
    }

    // -----------------------------------------------------------------
    // Dispatch-routing tests
    // -----------------------------------------------------------------

    #[test]
    fn registered_binding_dispatches_to_command_writes() {
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            scope: BindingScope::Anywhere,
            key: key_char('a'),
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Target,
            priority: 200,
        });

        let renderers = RendererRegistry::new();
        let mut reader = MapReader::default();
        // Seed a cursor under cursor-as-focus: scope_path is empty when
        // no cursor is set, so even an `Anywhere` binding needs at least
        // one scope on the path to be checked at lookup time.
        seed_focused(&mut reader, &oxpath!("settings"));

        let writes =
            dispatcher().dispatch(&mut reader, &key_char('a'), &bindings, &cmds, &renderers);

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "sentinel"));
        match &writes[0].record {
            Record::Parsed(Value::String(s)) => assert_eq!(s, "ran"),
            other => panic!("unexpected record: {other:?}"),
        }
    }

    #[test]
    fn missing_binding_returns_empty() {
        let cmds = CommandRegistry::new();
        let bindings = BindingRegistry::new();
        let renderers = RendererRegistry::new();
        let mut reader = MapReader::default();

        let writes =
            dispatcher().dispatch(&mut reader, &key_char('a'), &bindings, &cmds, &renderers);
        assert!(writes.is_empty());
    }

    #[test]
    fn missing_command_returns_empty() {
        // Defensive: a binding referencing a CommandId that isn't in the
        // command registry must not panic — dispatch returns empty.
        let cmds = CommandRegistry::new();

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            scope: BindingScope::Anywhere,
            key: key_char('a'),
            command_id: cmd_id("not.registered"),
            phase: Phase::Target,
            priority: 200,
        });

        let renderers = RendererRegistry::new();
        let mut reader = MapReader::default();

        let writes =
            dispatcher().dispatch(&mut reader, &key_char('a'), &bindings, &cmds, &renderers);
        assert!(writes.is_empty());
    }

    #[test]
    fn dispatcher_enters_compose_scope_when_cursor_at_field() {
        // Cursor-as-focus: the cursor sitting at
        // `settings/_compose_form/name` is what activates the compose
        // scope. The dispatcher's three-phase walk picks up an 'a'
        // binding at the per-field leaf at Target.
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            scope: BindingScope::Exact(oxpath!("settings", "_compose_form", "name")),
            key: key_char('a'),
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Target,
            priority: 200,
        });

        let renderers = RendererRegistry::new();
        let mut reader = MapReader::default();
        seed_focused(&mut reader, &oxpath!("settings", "_compose_form", "name"));

        let writes =
            dispatcher().dispatch(&mut reader, &key_char('a'), &bindings, &cmds, &renderers);

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "sentinel"));
    }

    #[test]
    fn dispatcher_skips_compose_scope_when_cursor_not_in_compose_form() {
        // No cursor under `settings/_compose_form/...` → the per-field
        // scope is never on the path. A binding registered there alone is
        // unreachable.
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            scope: BindingScope::Exact(oxpath!("settings", "_compose_form", "name")),
            key: key_char('a'),
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Target,
            priority: 200,
        });

        let renderers = RendererRegistry::new();
        let mut reader = MapReader::default();

        let writes =
            dispatcher().dispatch(&mut reader, &key_char('a'), &bindings, &cmds, &renderers);

        assert!(writes.is_empty());
    }

    #[test]
    fn dispatcher_skips_compose_scope_when_legacy_buffer_alone() {
        // Legacy stale state at `new_account/buffer` must not engage
        // compose mode under cursor-as-focus — only the cursor sitting
        // under `settings/_compose_form/...` opens the scope.
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            scope: BindingScope::Exact(oxpath!("settings", "_compose_form", "name")),
            key: key_char('a'),
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Target,
            priority: 200,
        });

        let renderers = RendererRegistry::new();
        let mut reader = MapReader::default();
        reader.insert(
            &oxpath!("ui", "settings", "new_account", "buffer"),
            Record::parsed(Value::String("partial".into())),
        );

        let writes =
            dispatcher().dispatch(&mut reader, &key_char('a'), &bindings, &cmds, &renderers);

        assert!(writes.is_empty());
    }

    #[test]
    fn pending_delete_routes_to_confirm_delete_scope_when_cursor_at_it() {
        // Cursor-as-focus: the cursor sitting at
        // `settings/_confirm_delete` activates the confirm-delete scope.
        // A Target binding at that leaf fires.
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            scope: BindingScope::Exact(oxpath!("settings", "_confirm_delete")),
            key: key_char('y'),
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Target,
            priority: 200,
        });

        let renderers = RendererRegistry::new();
        let mut reader = MapReader::default();
        seed_focused(&mut reader, &oxpath!("settings", "_confirm_delete"));

        let writes =
            dispatcher().dispatch(&mut reader, &key_char('y'), &bindings, &cmds, &renderers);

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "sentinel"));
    }

    #[test]
    fn pending_delete_esc_routes_via_capture() {
        // Esc on the confirm-delete dialog is a lifecycle key — the
        // scope claims it at Capture before any leaf sees it.
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            scope: BindingScope::Exact(oxpath!("settings", "_confirm_delete")),
            key: KeyChord {
                modifiers: KeyModifierSet::default(),
                code: KeyCodeRepr::Esc,
            },
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Capture,
            priority: 200,
        });

        let renderers = RendererRegistry::new();
        let mut reader = MapReader::default();
        seed_focused(&mut reader, &oxpath!("settings", "_confirm_delete"));

        let writes = dispatcher().dispatch(
            &mut reader,
            &KeyChord {
                modifiers: KeyModifierSet::default(),
                code: KeyCodeRepr::Esc,
            },
            &bindings,
            &cmds,
            &renderers,
        );

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "sentinel"));
    }

    #[test]
    fn pending_delete_y_routes_via_target() {
        // Symmetric to the Esc-Capture test: `y` is a semantic action on
        // the focused dialog (Target).
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            scope: BindingScope::Exact(oxpath!("settings", "_confirm_delete")),
            key: key_char('y'),
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Target,
            priority: 200,
        });

        let renderers = RendererRegistry::new();
        let mut reader = MapReader::default();
        seed_focused(&mut reader, &oxpath!("settings", "_confirm_delete"));

        let writes =
            dispatcher().dispatch(&mut reader, &key_char('y'), &bindings, &cmds, &renderers);

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "sentinel"));
    }

    #[test]
    fn command_sees_dispatched_keystroke() {
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(ReportKeystroke::new()));

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            scope: BindingScope::Anywhere,
            key: key_char('z'),
            command_id: cmd_id("test.report_keystroke"),
            phase: Phase::Target,
            priority: 200,
        });

        let renderers = RendererRegistry::new();
        let mut reader = MapReader::default();
        seed_focused(&mut reader, &oxpath!("settings"));

        let writes =
            dispatcher().dispatch(&mut reader, &key_char('z'), &bindings, &cmds, &renderers);

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "last_key"));
        match &writes[0].record {
            Record::Parsed(Value::String(s)) => assert_eq!(s, "z"),
            other => panic!("unexpected record: {other:?}"),
        }
    }

    #[test]
    fn manual_model_routes_to_manual_model_scope_when_cursor_at_stage() {
        // A Bubble binding at the form scope fires — the form scope is
        // an outer container, not the leaf (the leaf is
        // `_manual_model/<stage>`), so Bubble is the right phase.
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            scope: BindingScope::Exact(oxpath!("settings", "_manual_model")),
            key: key_char('a'),
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Bubble,
            priority: 200,
        });

        let renderers = RendererRegistry::new();
        let mut reader = MapReader::default();
        seed_focused(&mut reader, &oxpath!("settings", "_manual_model", "id"));

        let writes =
            dispatcher().dispatch(&mut reader, &key_char('a'), &bindings, &cmds, &renderers);

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "sentinel"));
    }

    #[test]
    fn manual_model_esc_routes_via_capture() {
        // Esc on the manual-model wizard is a lifecycle key — the form
        // scope claims it at Capture before any per-stage leaf sees it.
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            scope: BindingScope::Exact(oxpath!("settings", "_manual_model")),
            key: KeyChord {
                modifiers: KeyModifierSet::default(),
                code: KeyCodeRepr::Esc,
            },
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Capture,
            priority: 200,
        });

        let renderers = RendererRegistry::new();
        let mut reader = MapReader::default();
        seed_focused(&mut reader, &oxpath!("settings", "_manual_model", "id"));

        let writes = dispatcher().dispatch(
            &mut reader,
            &KeyChord {
                modifiers: KeyModifierSet::default(),
                code: KeyCodeRepr::Esc,
            },
            &bindings,
            &cmds,
            &renderers,
        );

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "sentinel"));
    }

    #[test]
    fn manual_model_enter_routes_via_bubble() {
        // Enter advances the wizard at the form scope on Bubble.
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            scope: BindingScope::Exact(oxpath!("settings", "_manual_model")),
            key: KeyChord {
                modifiers: KeyModifierSet::default(),
                code: KeyCodeRepr::Enter,
            },
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Bubble,
            priority: 200,
        });

        let renderers = RendererRegistry::new();
        let mut reader = MapReader::default();
        seed_focused(&mut reader, &oxpath!("settings", "_manual_model", "id"));

        let writes = dispatcher().dispatch(
            &mut reader,
            &KeyChord {
                modifiers: KeyModifierSet::default(),
                code: KeyCodeRepr::Enter,
            },
            &bindings,
            &cmds,
            &renderers,
        );

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "sentinel"));
    }

    #[test]
    fn manual_model_printable_routes_via_target_at_stage_leaf() {
        // Printable ASCII on the manual-model wizard targets the active
        // stage's leaf scope (`_manual_model/<stage>`), not the form
        // scope.
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            scope: BindingScope::Exact(oxpath!("settings", "_manual_model", "id")),
            key: key_char('x'),
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Target,
            priority: 200,
        });

        let renderers = RendererRegistry::new();
        let mut reader = MapReader::default();
        seed_focused(&mut reader, &oxpath!("settings", "_manual_model", "id"));

        let writes =
            dispatcher().dispatch(&mut reader, &key_char('x'), &bindings, &cmds, &renderers);

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "sentinel"));
    }

    #[test]
    fn manual_model_skips_scope_when_cursor_not_in_manual_model() {
        // No cursor under `settings/_manual_model/...` → the per-stage
        // scope is never on the path.
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            scope: BindingScope::Exact(oxpath!("settings", "_manual_model", "id")),
            key: key_char('a'),
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Target,
            priority: 200,
        });

        let renderers = RendererRegistry::new();
        let mut reader = MapReader::default();

        let writes =
            dispatcher().dispatch(&mut reader, &key_char('a'), &bindings, &cmds, &renderers);

        assert!(writes.is_empty());
    }

    #[test]
    fn dispatcher_routes_to_handler_when_discrete_misses() {
        // No discrete binding for 'x' at the Target leaf; a handler at
        // the same scope+phase claims char keys.
        use std::sync::Arc;

        use crate::binding::{HandlerEntry, KeyHandler};

        struct EatChar;
        impl KeyHandler for EatChar {
            fn handle(
                &self,
                _: &mut dyn Reader,
                k: &KeyChord,
                _: &CommandCtx<'_>,
            ) -> Option<Vec<Write>> {
                match k.code {
                    KeyCodeRepr::Char(c) => Some(vec![Write {
                        path: oxpath!("ui", "handler_seen"),
                        record: Record::parsed(Value::String(c.to_string())),
                    }]),
                    _ => None,
                }
            }
        }

        let cmds = CommandRegistry::new();
        let mut bindings = BindingRegistry::new();
        bindings.register_handler(HandlerEntry {
            scope: BindingScope::Exact(oxpath!("settings", "_edit")),
            phase: Phase::Target,
            handler: Arc::new(EatChar),
        });

        let renderers = RendererRegistry::new();
        let mut reader = MapReader::default();
        seed_focused(&mut reader, &oxpath!("settings", "_edit"));

        let writes =
            dispatcher().dispatch(&mut reader, &key_char('x'), &bindings, &cmds, &renderers);

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "handler_seen"));
        match &writes[0].record {
            Record::Parsed(Value::String(s)) => assert_eq!(s, "x"),
            other => panic!("unexpected record: {other:?}"),
        }
    }

    #[test]
    fn dispatcher_handler_returns_empty_writes_is_distinct_claim() {
        // `Some(vec![])` is a legitimate claim — distinct from `None`
        // (pass). The dispatcher must return the empty writes rather
        // than falling through to other tiers/phases.
        use std::sync::Arc;

        use crate::binding::{HandlerEntry, KeyHandler};

        struct SwallowAll;
        impl KeyHandler for SwallowAll {
            fn handle(
                &self,
                _: &mut dyn Reader,
                _: &KeyChord,
                _: &CommandCtx<'_>,
            ) -> Option<Vec<Write>> {
                Some(vec![])
            }
        }

        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        // A Bubble binding at the outer scope that would fire if the
        // Target handler didn't claim.
        bindings.register(BindingEntry {
            scope: BindingScope::Exact(oxpath!("settings")),
            key: key_char('x'),
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Bubble,
            priority: 200,
        });
        bindings.register_handler(HandlerEntry {
            scope: BindingScope::Exact(oxpath!("settings", "_edit")),
            phase: Phase::Target,
            handler: Arc::new(SwallowAll),
        });

        let renderers = RendererRegistry::new();
        let mut reader = MapReader::default();
        seed_focused(&mut reader, &oxpath!("settings", "_edit"));

        let writes =
            dispatcher().dispatch(&mut reader, &key_char('x'), &bindings, &cmds, &renderers);

        // Handler claimed with empty writes — bubble fallback must not
        // have fired.
        assert!(writes.is_empty());
    }

    #[test]
    fn dispatcher_handler_returns_none_passes_to_next_tier() {
        // A handler that returns `None` means "didn't claim"; the
        // dispatcher must continue walking and let a later phase/scope
        // claim the key.
        use std::sync::Arc;

        use crate::binding::{HandlerEntry, KeyHandler};

        struct PassAll;
        impl KeyHandler for PassAll {
            fn handle(
                &self,
                _: &mut dyn Reader,
                _: &KeyChord,
                _: &CommandCtx<'_>,
            ) -> Option<Vec<Write>> {
                None
            }
        }

        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            scope: BindingScope::Exact(oxpath!("settings")),
            key: key_char('x'),
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Bubble,
            priority: 200,
        });
        bindings.register_handler(HandlerEntry {
            scope: BindingScope::Exact(oxpath!("settings", "_edit")),
            phase: Phase::Target,
            handler: Arc::new(PassAll),
        });

        let renderers = RendererRegistry::new();
        let mut reader = MapReader::default();
        seed_focused(&mut reader, &oxpath!("settings", "_edit"));

        let writes =
            dispatcher().dispatch(&mut reader, &key_char('x'), &bindings, &cmds, &renderers);

        // Handler passed; bubble binding at `settings` fired the sentinel.
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "sentinel"));
    }

    #[test]
    fn dispatcher_prefers_discrete_over_handler_at_same_scope_and_phase() {
        // Discrete tier wins over handler tier at the same (scope, phase).
        // A discrete binding for 'x' at Target on `settings/_edit` and
        // a Target handler at the same scope that would also claim — the
        // discrete binding fires the sentinel, the handler is never asked.
        use std::sync::Arc;

        use crate::binding::{HandlerEntry, KeyHandler};

        struct ShouldNotFire;
        impl KeyHandler for ShouldNotFire {
            fn handle(
                &self,
                _: &mut dyn Reader,
                _: &KeyChord,
                _: &CommandCtx<'_>,
            ) -> Option<Vec<Write>> {
                // Returning a distinguishable sentinel write so a test
                // failure would surface here rather than silently agree.
                Some(vec![Write {
                    path: oxpath!("ui", "handler_should_not_have_fired"),
                    record: Record::parsed(Value::Bool(true)),
                }])
            }
        }

        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            scope: BindingScope::Exact(oxpath!("settings", "_edit")),
            key: key_char('x'),
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Target,
            priority: 200,
        });
        bindings.register_handler(HandlerEntry {
            scope: BindingScope::Exact(oxpath!("settings", "_edit")),
            phase: Phase::Target,
            handler: Arc::new(ShouldNotFire),
        });

        let renderers = RendererRegistry::new();
        let mut reader = MapReader::default();
        seed_focused(&mut reader, &oxpath!("settings", "_edit"));

        let writes =
            dispatcher().dispatch(&mut reader, &key_char('x'), &bindings, &cmds, &renderers);

        // Discrete tier fired — sentinel write, not the handler's marker.
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "sentinel"));
    }

    #[test]
    fn outer_scope_binding_fires_via_bubble_from_inner_focused_cursor() {
        // Page-cursor bindings declare Phase::Bubble. The dispatcher's
        // Bubble pass walks inner → outer through the cursor's ancestor
        // chain and finds them at the outer scope, even though the leaf
        // is a deeper focused-row scope.
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            scope: BindingScope::Exact(oxpath!("settings")),
            key: key_char('j'),
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Bubble,
            priority: 200,
        });

        let renderers = RendererRegistry::new();
        let mut reader = MapReader::default();
        seed_focused(&mut reader, &oxpath!("settings", "accounts"));

        let writes =
            dispatcher().dispatch(&mut reader, &key_char('j'), &bindings, &cmds, &renderers);

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "sentinel"));
    }

    /// Seed `<focus_path> = settings/_edit` (plus the target field path)
    /// so the dispatcher's `compute_scope_path` builds the `_edit` leaf.
    fn seed_edit_mode(reader: &mut MapReader, field_path: Path) {
        seed_focused(reader, &oxpath!("settings", "_edit"));
        reader.insert(
            &oxpath!("ui", "settings", "edit", "target_path"),
            Record::parsed(path_to_value(&field_path)),
        );
    }

    #[test]
    fn edit_mode_esc_routes_via_capture() {
        // Esc on inline edit-mode is a lifecycle key — the `_edit` scope
        // claims it at Capture before any leaf sees it.
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            scope: BindingScope::Exact(oxpath!("settings", "_edit")),
            key: KeyChord {
                modifiers: KeyModifierSet::default(),
                code: KeyCodeRepr::Esc,
            },
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Capture,
            priority: 200,
        });

        let renderers = RendererRegistry::new();
        let mut reader = MapReader::default();
        seed_edit_mode(
            &mut reader,
            oxpath!("config", "gate", "providers", "alpha", "endpoint"),
        );

        let writes = dispatcher().dispatch(
            &mut reader,
            &KeyChord {
                modifiers: KeyModifierSet::default(),
                code: KeyCodeRepr::Esc,
            },
            &bindings,
            &cmds,
            &renderers,
        );

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "sentinel"));
    }

    #[test]
    fn edit_mode_enter_routes_via_bubble() {
        // Enter commits the edit buffer at Bubble.
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            scope: BindingScope::Exact(oxpath!("settings", "_edit")),
            key: KeyChord {
                modifiers: KeyModifierSet::default(),
                code: KeyCodeRepr::Enter,
            },
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Bubble,
            priority: 200,
        });

        let renderers = RendererRegistry::new();
        let mut reader = MapReader::default();
        seed_edit_mode(
            &mut reader,
            oxpath!("config", "gate", "providers", "alpha", "endpoint"),
        );

        let writes = dispatcher().dispatch(
            &mut reader,
            &KeyChord {
                modifiers: KeyModifierSet::default(),
                code: KeyCodeRepr::Enter,
            },
            &bindings,
            &cmds,
            &renderers,
        );

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "sentinel"));
    }

    #[test]
    fn edit_mode_printable_routes_via_target() {
        // Printable ASCII on inline edit-mode mutates the buffer — the
        // leaf claim.
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            scope: BindingScope::Exact(oxpath!("settings", "_edit")),
            key: key_char('x'),
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Target,
            priority: 200,
        });

        let renderers = RendererRegistry::new();
        let mut reader = MapReader::default();
        seed_edit_mode(
            &mut reader,
            oxpath!("config", "gate", "providers", "alpha", "endpoint"),
        );

        let writes =
            dispatcher().dispatch(&mut reader, &key_char('x'), &bindings, &cmds, &renderers);

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "sentinel"));
    }

    // -----------------------------------------------------------------
    // `compute_scope_path` structural ordering tests
    // -----------------------------------------------------------------

    #[test]
    fn scope_path_falls_back_to_screen_root_when_no_cursor_seeded() {
        // With no focus written, `read_focus_cursor` returns None. The
        // scope path falls back to the screen-root scope so page-level
        // Bubble bindings remain reachable on first entry.
        let mut reader = MapReader::default();
        let path = dispatcher().compute_scope_path(&mut reader);
        assert_eq!(
            path,
            vec![BindingScope::Exact(oxpath!("settings"))],
            "no-cursor fallback should be the screen-root scope",
        );
    }

    #[test]
    fn scope_path_is_cursor_ancestor_chain_for_row_cursor() {
        // Cursor at a row path → scope path is the cursor's ancestor
        // chain (each progressively-longer prefix, outer → inner).
        let mut reader = MapReader::default();
        seed_focused(&mut reader, &oxpath!("settings", "accounts", "alpha"));

        let path = dispatcher().compute_scope_path(&mut reader);

        assert_eq!(path.len(), 3);
        assert_eq!(path[0], BindingScope::Exact(oxpath!("settings")));
        assert_eq!(
            path[1],
            BindingScope::Exact(oxpath!("settings", "accounts"))
        );
        assert_eq!(
            path[2],
            BindingScope::Exact(oxpath!("settings", "accounts", "alpha"))
        );
    }

    #[test]
    fn scope_path_for_cursor_at_section_header_is_two_levels() {
        // Cursor at the section header (`settings/accounts`) → ancestor
        // chain is [settings, settings/accounts].
        let mut reader = MapReader::default();
        seed_focused(&mut reader, &oxpath!("settings", "accounts"));

        let path = dispatcher().compute_scope_path(&mut reader);

        assert_eq!(path.len(), 2);
        assert_eq!(path[0], BindingScope::Exact(oxpath!("settings")));
        assert_eq!(
            path[1],
            BindingScope::Exact(oxpath!("settings", "accounts"))
        );
    }

    #[test]
    fn scope_path_for_compose_is_settings_then_form_then_field_leaf() {
        // Compose mode with the Name field focused: ancestor chain is
        // [settings, settings/_compose_form, settings/_compose_form/name].
        let mut reader = MapReader::default();
        seed_focused(&mut reader, &oxpath!("settings", "_compose_form", "name"));

        let path = dispatcher().compute_scope_path(&mut reader);

        assert_eq!(path.len(), 3);
        assert_eq!(path[0], BindingScope::Exact(oxpath!("settings")));
        assert_eq!(
            path[1],
            BindingScope::Exact(oxpath!("settings", "_compose_form"))
        );
        assert_eq!(
            path[2],
            BindingScope::Exact(oxpath!("settings", "_compose_form", "name"))
        );
    }

    #[test]
    fn scope_path_for_compose_selector_focus_uses_per_field_leaf() {
        // Same shape with the Protocol field focused — leaf flips.
        let mut reader = MapReader::default();
        seed_focused(
            &mut reader,
            &oxpath!("settings", "_compose_form", "protocol"),
        );

        let path = dispatcher().compute_scope_path(&mut reader);

        assert_eq!(path.len(), 3);
        assert_eq!(path[0], BindingScope::Exact(oxpath!("settings")));
        assert_eq!(
            path[1],
            BindingScope::Exact(oxpath!("settings", "_compose_form"))
        );
        assert_eq!(
            path[2],
            BindingScope::Exact(oxpath!("settings", "_compose_form", "protocol"))
        );
    }

    #[test]
    fn compute_scope_path_includes_compose_form_when_cursor_at_field() {
        let mut reader = MapReader::default();
        seed_focused(&mut reader, &oxpath!("settings", "_compose_form", "name"));
        let path = dispatcher().compute_scope_path(&mut reader);
        assert!(path.contains(&BindingScope::Exact(oxpath!("settings", "_compose_form"))));
        assert!(path.contains(&BindingScope::Exact(oxpath!(
            "settings",
            "_compose_form",
            "name"
        ))));
    }

    #[test]
    fn cursor_at_account_row_does_not_include_compose_form_scope() {
        let mut reader = MapReader::default();
        seed_focused(&mut reader, &oxpath!("settings", "accounts", "alpha"));
        let path = dispatcher().compute_scope_path(&mut reader);
        assert!(
            !path.contains(&BindingScope::Exact(oxpath!("settings", "_compose_form"))),
            "no compose form scope when cursor is on an account row: {path:?}",
        );
    }

    #[test]
    fn scope_path_for_manual_model_is_settings_then_form_then_stage_leaf() {
        let mut reader = MapReader::default();
        seed_focused(&mut reader, &oxpath!("settings", "_manual_model", "ctx"));

        let path = dispatcher().compute_scope_path(&mut reader);

        assert_eq!(path.len(), 3);
        assert_eq!(path[0], BindingScope::Exact(oxpath!("settings")));
        assert_eq!(
            path[1],
            BindingScope::Exact(oxpath!("settings", "_manual_model"))
        );
        assert_eq!(
            path[2],
            BindingScope::Exact(oxpath!("settings", "_manual_model", "ctx"))
        );
    }

    #[test]
    fn scope_path_for_manual_model_id_stage_uses_id_leaf() {
        let mut reader = MapReader::default();
        seed_focused(&mut reader, &oxpath!("settings", "_manual_model", "id"));

        let path = dispatcher().compute_scope_path(&mut reader);

        assert_eq!(
            path.last().unwrap(),
            &BindingScope::Exact(oxpath!("settings", "_manual_model", "id"))
        );
    }

    #[test]
    fn compute_scope_path_includes_manual_model_when_cursor_at_stage() {
        let mut reader = MapReader::default();
        seed_focused(&mut reader, &oxpath!("settings", "_manual_model", "id"));
        let path = dispatcher().compute_scope_path(&mut reader);
        assert!(
            path.contains(&BindingScope::Exact(oxpath!("settings", "_manual_model"))),
            "expected form scope on path: {path:?}",
        );
        assert!(
            path.contains(&BindingScope::Exact(oxpath!(
                "settings",
                "_manual_model",
                "id"
            ))),
            "expected per-stage leaf scope on path: {path:?}",
        );
    }

    #[test]
    fn scope_path_for_confirm_delete_ends_at_confirm_delete_scope() {
        let mut reader = MapReader::default();
        seed_focused(&mut reader, &oxpath!("settings", "_confirm_delete"));

        let path = dispatcher().compute_scope_path(&mut reader);

        assert_eq!(path.len(), 2);
        assert_eq!(path[0], BindingScope::Exact(oxpath!("settings")));
        assert_eq!(
            path[1],
            BindingScope::Exact(oxpath!("settings", "_confirm_delete"))
        );
    }

    #[test]
    fn compute_scope_path_includes_confirm_delete_when_cursor_at_it() {
        let mut reader = MapReader::default();
        seed_focused(&mut reader, &oxpath!("settings", "_confirm_delete"));
        let path = dispatcher().compute_scope_path(&mut reader);
        assert!(
            path.contains(&BindingScope::Exact(oxpath!("settings", "_confirm_delete"))),
            "expected confirm-delete leaf on path: {path:?}",
        );
    }

    #[test]
    fn scope_path_for_edit_mode_ends_at_edit_scope() {
        let mut reader = MapReader::default();
        seed_edit_mode(
            &mut reader,
            oxpath!("config", "gate", "providers", "alpha", "endpoint"),
        );

        let path = dispatcher().compute_scope_path(&mut reader);

        assert_eq!(path.len(), 2);
        assert_eq!(path[0], BindingScope::Exact(oxpath!("settings")));
        assert_eq!(path[1], BindingScope::Exact(oxpath!("settings", "_edit")));
    }

    #[test]
    fn compute_scope_path_includes_edit_when_cursor_at_it() {
        let mut reader = MapReader::default();
        seed_edit_mode(
            &mut reader,
            oxpath!("settings", "accounts", "alpha", "endpoint"),
        );
        let path = dispatcher().compute_scope_path(&mut reader);
        assert!(
            path.contains(&BindingScope::Exact(oxpath!("settings", "_edit"))),
            "expected edit leaf on path: {path:?}",
        );
    }

    // -----------------------------------------------------------------
    // `path_ancestors` shape tests
    // -----------------------------------------------------------------

    #[test]
    fn path_ancestors_empty_path_returns_empty_vec() {
        let ancestors = path_ancestors(&oxpath!());
        assert!(ancestors.is_empty(), "expected empty vec: {ancestors:?}");
    }

    #[test]
    fn path_ancestors_single_segment_returns_self() {
        let p = oxpath!("settings");
        let ancestors = path_ancestors(&p);
        assert_eq!(ancestors, vec![p]);
    }

    #[test]
    fn path_ancestors_multi_segment_returns_progressively_longer_prefixes() {
        let p = oxpath!("settings", "_compose_form", "name");
        let ancestors = path_ancestors(&p);
        assert_eq!(
            ancestors,
            vec![
                oxpath!("settings"),
                oxpath!("settings", "_compose_form"),
                oxpath!("settings", "_compose_form", "name"),
            ],
        );
    }
}
