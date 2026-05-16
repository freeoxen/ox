//! Settings key dispatch — binding lookup → command lookup → run.
//!
//! Pure: no async, no I/O. Inert when no binding matches, or when the
//! binding references a `CommandId` not present in the command registry
//! (defensive — the command registry is the source of truth; a binding
//! is just a reference by id).
//!
//! **Mutability deviation from spec:** `snapshot` is `&mut dyn Reader`
//! rather than the `&dyn Reader` quoted in the spec, because
//! `Reader::read(&mut self, ...)` requires a mutable receiver. Matches
//! the same deviation in `Command::run` (Phase H1) and `RenderCtx::data`
//! (Phase G).
//!
//! ## Hierarchical dispatch
//!
//! The dispatcher resolves a keystroke by walking a *scope path* — the
//! chain of nested scopes the user is currently "inside", from outer
//! (whole page) to inner (innermost active compound widget). Three
//! phases run in order:
//!
//! 1. **Capture** (outer → inner): container-owned lifecycle keys that
//!    fire before the focused leaf sees them (e.g. compose Esc/Tab).
//! 2. **Target** (leaf only): the focused leaf claims the key.
//! 3. **Bubble** (inner → outer): container fallbacks for keys the
//!    leaf didn't consume (e.g. compose Enter, page-cursor j/k,
//!    focused-row a/t/r/d). Each scope is queried at `Phase::Bubble`
//!    only — outer-scope defaults declare Bubble explicitly.
//!
//! `compute_scope_path` is the cursor's ancestor chain. Adding a new
//! compound widget = move the cursor into its namespace and register
//! bindings under the new scope with appropriate phases. No dispatcher
//! changes.

use structfs_core_store::{Path, Reader};

use ox_types::subscription::Write;
use ox_types::{BindingScope, KeyChord, Phase};

use super::binding_registry::BindingRegistry;
use super::command_registry::{CommandCtx, CommandRegistry};
use super::commands::account_model::path_ancestors;
use super::registry::RendererRegistry;

/// Resolve `(cursor, key)` to a sequence of writes by looking up the
/// binding, then the command, then running it. Returns `vec![]` (inert)
/// on any miss.
pub fn dispatch_settings_key(
    snapshot: &mut dyn Reader,
    _cursor: &Path,
    key: &KeyChord,
    cmds: &CommandRegistry,
    bindings: &BindingRegistry,
    renderers: &RendererRegistry,
) -> Vec<Write> {
    let scope_path = compute_scope_path(snapshot);

    // Capture (outer → inner): containers claim lifecycle keys before
    // the leaf sees them.
    let mut cmd_id_opt = None;
    for scope_path_entry in &scope_path {
        if let Some(p) = scope_path_entry.keyed_path() {
            if let Some(hit) = bindings.lookup(p, key, Phase::Capture) {
                cmd_id_opt = Some(hit);
                break;
            }
        }
    }

    // Target (leaf only): the innermost scope claims the key.
    if cmd_id_opt.is_none() {
        if let Some(leaf) = scope_path.last().and_then(BindingScope::keyed_path) {
            cmd_id_opt = bindings.lookup(leaf, key, Phase::Target);
        }
    }

    // Bubble (inner → outer): containers handle keys the leaf didn't
    // consume. Outer-scope defaults (page-cursor `j`/`k`, focused-row
    // `a`/`t`, whole-screen `?`, compose Enter, ...) declare
    // `Phase::Bubble` directly — no per-scope Target fallback.
    if cmd_id_opt.is_none() {
        for scope_path_entry in scope_path.iter().rev() {
            let Some(p) = scope_path_entry.keyed_path() else {
                continue;
            };
            if let Some(hit) = bindings.lookup(p, key, Phase::Bubble) {
                cmd_id_opt = Some(hit);
                break;
            }
        }
    }

    let Some(cmd_id) = cmd_id_opt else {
        return vec![];
    };
    let Some(command) = cmds.lookup(cmd_id) else {
        return vec![];
    };
    let ctx = CommandCtx {
        registry: renderers,
        last_keystroke: Some(key.clone()),
    };
    command.run(snapshot, &ctx)
}

/// The scope path the dispatcher walks is the cursor's ancestor chain:
/// for cursor `settings/_compose_form/name` the path is `[settings,
/// settings/_compose_form, settings/_compose_form/name]`. Outer →
/// inner; each entry on the chain is a `BindingScope::Exact`.
///
/// Cursor-as-focus is the only source of scope: which compound widget
/// (if any) is engaged is encoded by the cursor sitting under that
/// widget's namespace. Mutual exclusion is structural — the cursor is
/// a single path; only one widget's prefix can be on its ancestry.
///
/// `pub(crate)` so the dispatcher's tests can assert the returned
/// ordering directly.
pub(crate) fn compute_scope_path(snapshot: &mut dyn Reader) -> Vec<BindingScope> {
    use ox_path::oxpath;
    match read_cursor(snapshot) {
        Some(cursor) => path_ancestors(&cursor)
            .into_iter()
            .map(BindingScope::Exact)
            .collect(),
        // No cursor set (e.g., first entry into the settings screen before
        // any row has been focused). Fall back to the screen-root scope so
        // page-level Bubble bindings (j/k navigation, etc.) remain
        // reachable. Pressing j once writes `focused` to the first
        // navigable row; from then on the cursor's ancestor chain
        // supplies the scope path naturally.
        None => vec![BindingScope::Exact(oxpath!("settings"))],
    }
}

/// Read `ui/settings/focused` from the dispatch snapshot — the cursor
/// under cursor-as-focus. Distinct from `ui/settings/cursor`: that
/// stores the page-level scope (always `settings/index` on the
/// accordion) while `focused` tracks which widget the user is in.
fn read_cursor(snapshot: &mut dyn Reader) -> Option<Path> {
    use ox_path::oxpath;
    let record = snapshot
        .read(&oxpath!("ui", "settings", "focused"))
        .ok()
        .flatten()?;
    let value = record.as_value()?;
    crate::settings::commands::navigation::path_from_value(value)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use ox_path::oxpath;
    use ox_store_util::local_config::LocalConfig;
    use ox_types::key_chord::{KeyCodeRepr, KeyModifierSet};
    use ox_types::{BindingEntry, CommandDisplay, CommandId, CommandScope};
    // Phase is already in scope via the parent use at the top of the file.
    use structfs_core_store::{Record, Value, Writer};

    use super::super::command_registry::Command;

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
            // Encode the dispatched key as a string. If the chord wasn't
            // a `Char(_)`, write a marker; if `last_keystroke` was None,
            // write "none".
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

    #[test]
    fn registered_binding_dispatches_to_command_writes() {
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            scope: ox_types::BindingScope::Anywhere,
            key: key_char('a'),
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Target,
        });

        let renderers = RendererRegistry::new();
        let mut reader = LocalConfig::default();
        // Seed a cursor under cursor-as-focus: scope_path is empty when
        // no cursor is set, so even an `Anywhere` binding needs at least
        // one scope on the path to be checked at lookup time.
        use super::super::commands::navigation::path_to_value;
        reader
            .write(
                &oxpath!("ui", "settings", "focused"),
                Record::parsed(path_to_value(&oxpath!("settings"))),
            )
            .unwrap();

        let writes = dispatch_settings_key(
            &mut reader,
            &oxpath!(),
            &key_char('a'),
            &cmds,
            &bindings,
            &renderers,
        );

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
        let mut reader = LocalConfig::default();

        let writes = dispatch_settings_key(
            &mut reader,
            &oxpath!(),
            &key_char('a'),
            &cmds,
            &bindings,
            &renderers,
        );
        assert!(writes.is_empty());
    }

    #[test]
    fn missing_command_returns_empty() {
        // Defensive: a binding referencing a CommandId that isn't in the
        // command registry must not panic — dispatch returns empty.
        let cmds = CommandRegistry::new();

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            scope: ox_types::BindingScope::Anywhere,
            key: key_char('a'),
            command_id: cmd_id("not.registered"),
            phase: Phase::Target,
        });

        let renderers = RendererRegistry::new();
        let mut reader = LocalConfig::default();

        let writes = dispatch_settings_key(
            &mut reader,
            &oxpath!(),
            &key_char('a'),
            &cmds,
            &bindings,
            &renderers,
        );
        assert!(writes.is_empty());
    }

    #[test]
    fn dispatcher_enters_compose_scope_when_cursor_at_field() {
        // Cursor-as-focus: the cursor sitting at
        // `settings/_compose_form/name` is what activates the compose
        // scope. The dispatcher's three-phase walk picks up an
        // 'a' binding at the per-field leaf at Target.
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            scope: ox_types::BindingScope::Exact(oxpath!("settings", "_compose_form", "name")),
            key: key_char('a'),
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Target,
        });

        let renderers = RendererRegistry::new();
        let mut reader = LocalConfig::default();
        use super::super::commands::navigation::path_to_value;
        reader
            .write(
                &oxpath!("ui", "settings", "focused"),
                Record::parsed(path_to_value(&oxpath!("settings", "_compose_form", "name"))),
            )
            .unwrap();

        let writes = dispatch_settings_key(
            &mut reader,
            &oxpath!("settings", "index"),
            &key_char('a'),
            &cmds,
            &bindings,
            &renderers,
        );

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "sentinel"));
    }

    #[test]
    fn dispatcher_skips_compose_scope_when_cursor_not_in_compose_form() {
        // No cursor under `settings/_compose_form/...` → the
        // per-field scope is never on the path. A binding registered
        // there alone is unreachable.
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            scope: ox_types::BindingScope::Exact(oxpath!("settings", "_compose_form", "name")),
            key: key_char('a'),
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Target,
        });

        let renderers = RendererRegistry::new();
        let mut reader = LocalConfig::default();

        let writes = dispatch_settings_key(
            &mut reader,
            &oxpath!("settings", "index"),
            &key_char('a'),
            &cmds,
            &bindings,
            &renderers,
        );

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
            scope: ox_types::BindingScope::Exact(oxpath!("settings", "_compose_form", "name")),
            key: key_char('a'),
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Target,
        });

        let renderers = RendererRegistry::new();
        let mut reader = LocalConfig::default();
        reader
            .write(
                &oxpath!("ui", "settings", "new_account", "buffer"),
                Record::parsed(Value::String("partial".into())),
            )
            .unwrap();

        let writes = dispatch_settings_key(
            &mut reader,
            &oxpath!("settings", "index"),
            &key_char('a'),
            &cmds,
            &bindings,
            &renderers,
        );

        assert!(writes.is_empty());
    }

    #[test]
    fn pending_delete_routes_to_confirm_delete_scope_when_cursor_at_it() {
        // Cursor-as-focus: the cursor sitting at
        // `settings/_confirm_delete` is what activates the confirm-delete
        // scope. A Target binding at that leaf fires.
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            scope: ox_types::BindingScope::Exact(oxpath!("settings", "_confirm_delete")),
            key: key_char('y'),
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Target,
        });

        let renderers = RendererRegistry::new();
        let mut reader = LocalConfig::default();
        use super::super::commands::navigation::path_to_value;
        reader
            .write(
                &oxpath!("ui", "settings", "focused"),
                Record::parsed(path_to_value(&oxpath!("settings", "_confirm_delete"))),
            )
            .unwrap();

        let writes = dispatch_settings_key(
            &mut reader,
            &oxpath!("settings", "index"),
            &key_char('y'),
            &cmds,
            &bindings,
            &renderers,
        );

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "sentinel"));
    }

    #[test]
    fn pending_delete_esc_routes_via_capture() {
        // Esc on the confirm-delete dialog is a lifecycle key — the
        // scope claims it at Capture before any leaf sees it. A binding
        // registered at Phase::Capture on `_confirm_delete` must fire
        // when Esc is pressed while the cursor sits there.
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            scope: ox_types::BindingScope::Exact(oxpath!("settings", "_confirm_delete")),
            key: KeyChord {
                modifiers: KeyModifierSet::default(),
                code: KeyCodeRepr::Esc,
            },
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Capture,
        });

        let renderers = RendererRegistry::new();
        let mut reader = LocalConfig::default();
        use super::super::commands::navigation::path_to_value;
        reader
            .write(
                &oxpath!("ui", "settings", "focused"),
                Record::parsed(path_to_value(&oxpath!("settings", "_confirm_delete"))),
            )
            .unwrap();

        let writes = dispatch_settings_key(
            &mut reader,
            &oxpath!("settings", "accounts"),
            &KeyChord {
                modifiers: KeyModifierSet::default(),
                code: KeyCodeRepr::Esc,
            },
            &cmds,
            &bindings,
            &renderers,
        );

        // Sentinel fires → Capture-phase lookup at the pending-delete
        // scope worked.
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "sentinel"));
    }

    #[test]
    fn pending_delete_y_routes_via_target() {
        // Symmetric to the Esc-Capture test: `y` is a semantic action on
        // the focused dialog (Target), and a Phase::Target binding on
        // `_confirm_delete` must fire when `y` is pressed while the
        // cursor sits there.
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            scope: ox_types::BindingScope::Exact(oxpath!("settings", "_confirm_delete")),
            key: key_char('y'),
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Target,
        });

        let renderers = RendererRegistry::new();
        let mut reader = LocalConfig::default();
        use super::super::commands::navigation::path_to_value;
        reader
            .write(
                &oxpath!("ui", "settings", "focused"),
                Record::parsed(path_to_value(&oxpath!("settings", "_confirm_delete"))),
            )
            .unwrap();

        let writes = dispatch_settings_key(
            &mut reader,
            &oxpath!("settings", "accounts"),
            &key_char('y'),
            &cmds,
            &bindings,
            &renderers,
        );

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "sentinel"));
    }

    #[test]
    fn command_sees_dispatched_keystroke() {
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(ReportKeystroke::new()));

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            scope: ox_types::BindingScope::Anywhere,
            key: key_char('z'),
            command_id: cmd_id("test.report_keystroke"),
            phase: Phase::Target,
        });

        let renderers = RendererRegistry::new();
        let mut reader = LocalConfig::default();
        // Seed a cursor: scope_path under cursor-as-focus is the
        // cursor's ancestor chain, so even `Anywhere` bindings need at
        // least one scope on the path to be looked up.
        use super::super::commands::navigation::path_to_value;
        reader
            .write(
                &oxpath!("ui", "settings", "focused"),
                Record::parsed(path_to_value(&oxpath!("settings"))),
            )
            .unwrap();

        let writes = dispatch_settings_key(
            &mut reader,
            &oxpath!(),
            &key_char('z'),
            &cmds,
            &bindings,
            &renderers,
        );

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "last_key"));
        match &writes[0].record {
            Record::Parsed(Value::String(s)) => assert_eq!(s, "z"),
            other => panic!("unexpected record: {other:?}"),
        }
    }

    #[test]
    fn manual_model_routes_to_manual_model_scope_when_cursor_at_stage() {
        // Cursor-as-focus: the cursor sitting at
        // `settings/_manual_model/id` is what activates the manual-model
        // scope. A Bubble binding at the form scope fires — the form
        // scope is an outer container, not the leaf (the leaf is
        // `_manual_model/<stage>`), so Bubble is the right phase to
        // pin.
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            scope: ox_types::BindingScope::Exact(oxpath!("settings", "_manual_model")),
            key: key_char('a'),
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Bubble,
        });

        let renderers = RendererRegistry::new();
        let mut reader = LocalConfig::default();
        use super::super::commands::navigation::path_to_value;
        reader
            .write(
                &oxpath!("ui", "settings", "focused"),
                Record::parsed(path_to_value(&oxpath!("settings", "_manual_model", "id"))),
            )
            .unwrap();

        let writes = dispatch_settings_key(
            &mut reader,
            &oxpath!("settings", "index"),
            &key_char('a'),
            &cmds,
            &bindings,
            &renderers,
        );

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "sentinel"));
    }

    #[test]
    fn manual_model_esc_routes_via_capture() {
        // Esc on the manual-model wizard is a lifecycle key — the form
        // scope claims it at Capture before any per-stage leaf sees it.
        // Mirrors pending-delete's Esc shape.
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            scope: ox_types::BindingScope::Exact(oxpath!("settings", "_manual_model")),
            key: KeyChord {
                modifiers: KeyModifierSet::default(),
                code: KeyCodeRepr::Esc,
            },
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Capture,
        });

        let renderers = RendererRegistry::new();
        let mut reader = LocalConfig::default();
        use super::super::commands::navigation::path_to_value;
        reader
            .write(
                &oxpath!("ui", "settings", "focused"),
                Record::parsed(path_to_value(&oxpath!("settings", "_manual_model", "id"))),
            )
            .unwrap();

        let writes = dispatch_settings_key(
            &mut reader,
            &oxpath!("settings", "models"),
            &KeyChord {
                modifiers: KeyModifierSet::default(),
                code: KeyCodeRepr::Esc,
            },
            &cmds,
            &bindings,
            &renderers,
        );

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "sentinel"));
    }

    #[test]
    fn manual_model_enter_routes_via_bubble() {
        // Enter advances the wizard at the form scope on Bubble: leaf
        // stages get first crack at Enter (Target) so a future
        // multi-line stage could insert a newline; nothing claims it
        // there today, so a Bubble binding at `_manual_model` fires.
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            scope: ox_types::BindingScope::Exact(oxpath!("settings", "_manual_model")),
            key: KeyChord {
                modifiers: KeyModifierSet::default(),
                code: KeyCodeRepr::Enter,
            },
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Bubble,
        });

        let renderers = RendererRegistry::new();
        let mut reader = LocalConfig::default();
        use super::super::commands::navigation::path_to_value;
        reader
            .write(
                &oxpath!("ui", "settings", "focused"),
                Record::parsed(path_to_value(&oxpath!("settings", "_manual_model", "id"))),
            )
            .unwrap();

        let writes = dispatch_settings_key(
            &mut reader,
            &oxpath!("settings", "models"),
            &KeyChord {
                modifiers: KeyModifierSet::default(),
                code: KeyCodeRepr::Enter,
            },
            &cmds,
            &bindings,
            &renderers,
        );

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "sentinel"));
    }

    #[test]
    fn manual_model_printable_routes_via_target_at_stage_leaf() {
        // Printable ASCII on the manual-model wizard targets the active
        // stage's leaf scope (`_manual_model/<stage>`), not the form
        // scope. A Target binding at `_manual_model/id` must fire when
        // the cursor sits at the Id stage.
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            scope: ox_types::BindingScope::Exact(oxpath!("settings", "_manual_model", "id")),
            key: key_char('x'),
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Target,
        });

        let renderers = RendererRegistry::new();
        let mut reader = LocalConfig::default();
        use super::super::commands::navigation::path_to_value;
        reader
            .write(
                &oxpath!("ui", "settings", "focused"),
                Record::parsed(path_to_value(&oxpath!("settings", "_manual_model", "id"))),
            )
            .unwrap();

        let writes = dispatch_settings_key(
            &mut reader,
            &oxpath!("settings", "models"),
            &key_char('x'),
            &cmds,
            &bindings,
            &renderers,
        );

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "sentinel"));
    }

    #[test]
    fn manual_model_skips_scope_when_cursor_not_in_manual_model() {
        // No cursor under `settings/_manual_model/...` → the
        // per-stage scope is never on the path. A binding registered
        // there alone is unreachable.
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            scope: ox_types::BindingScope::Exact(oxpath!("settings", "_manual_model", "id")),
            key: key_char('a'),
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Target,
        });

        let renderers = RendererRegistry::new();
        let mut reader = LocalConfig::default();

        let writes = dispatch_settings_key(
            &mut reader,
            &oxpath!("settings", "index"),
            &key_char('a'),
            &cmds,
            &bindings,
            &renderers,
        );

        // No cursor in `_manual_model/*` → scope never on the path.
        assert!(writes.is_empty());
    }

    #[test]
    fn outer_scope_binding_fires_via_bubble_from_inner_focused_cursor() {
        // Page-cursor bindings (j/k row navigation, the page's *default*
        // row-nav handlers) declare Phase::Bubble. The dispatcher's
        // Bubble pass walks inner → outer through the cursor's ancestor
        // chain and finds them at the outer scope, even though the leaf
        // is a deeper focused-row scope.
        //
        // Setup: cursor (`ui/settings/focused`) at `settings/accounts`.
        // Ancestor chain: [Exact(settings), Exact(settings/accounts)].
        // The leaf has no Bubble binding for `j`; the Bubble walk
        // continues outward and finds `j` on the outer `settings` scope.
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        // Outer-scope `j` lives at `Exact(settings)` Phase::Bubble — the
        // ancestor of every cursor on the settings screen. The
        // dispatcher's Bubble pass walks inner → outer and finds it
        // without any per-scope Target fallback.
        bindings.register(BindingEntry {
            scope: ox_types::BindingScope::Exact(oxpath!("settings")),
            key: key_char('j'),
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Bubble,
        });

        let renderers = RendererRegistry::new();
        let mut reader = LocalConfig::default();
        // Seed a focused-row cursor under `settings` so the leaf differs
        // from the outer scope. The Bubble pass still finds `j` at the
        // ancestor.
        use super::super::commands::navigation::path_to_value;
        reader
            .write(
                &oxpath!("ui", "settings", "focused"),
                Record::parsed(path_to_value(&oxpath!("settings", "accounts"))),
            )
            .unwrap();

        let writes = dispatch_settings_key(
            &mut reader,
            &oxpath!("settings", "index"),
            &key_char('j'),
            &cmds,
            &bindings,
            &renderers,
        );

        // The sentinel fires because the Bubble pass walks outward from
        // the leaf to the ancestor scope where the binding lives.
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "sentinel"));
    }

    /// Seed `ui/settings/focused = settings/_edit` (plus the target
    /// field path the edit machinery commits to) so the dispatcher's
    /// `compute_scope_path` pushes the `_edit` leaf via its
    /// focused-row push. Mirrors the confirm-delete seed shape.
    fn seed_edit_mode(reader: &mut LocalConfig, field_path: Path) {
        use super::super::commands::navigation::path_to_value;
        reader
            .write(
                &oxpath!("ui", "settings", "focused"),
                Record::parsed(path_to_value(&oxpath!("settings", "_edit"))),
            )
            .unwrap();
        reader
            .write(
                &oxpath!("ui", "settings", "edit", "target_path"),
                Record::parsed(path_to_value(&field_path)),
            )
            .unwrap();
    }

    #[test]
    fn edit_mode_esc_routes_via_capture() {
        // Esc on inline edit-mode is a lifecycle key — the `_edit`
        // scope claims it at Capture before any leaf sees it. A binding
        // registered at Phase::Capture on `_edit` must fire when Esc
        // is pressed while the cursor sits at `settings/_edit`.
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
        });

        let renderers = RendererRegistry::new();
        let mut reader = LocalConfig::default();
        seed_edit_mode(
            &mut reader,
            oxpath!("config", "gate", "providers", "alpha", "endpoint"),
        );

        let writes = dispatch_settings_key(
            &mut reader,
            &oxpath!("settings", "accounts"),
            &KeyChord {
                modifiers: KeyModifierSet::default(),
                code: KeyCodeRepr::Esc,
            },
            &cmds,
            &bindings,
            &renderers,
        );

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "sentinel"));
    }

    #[test]
    fn edit_mode_enter_routes_via_bubble() {
        // Enter commits the edit buffer at Bubble: leaves (none today,
        // but a future multi-line text editor at Target) get first crack
        // at Enter. A Bubble binding at `_edit` fires.
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
        });

        let renderers = RendererRegistry::new();
        let mut reader = LocalConfig::default();
        seed_edit_mode(
            &mut reader,
            oxpath!("config", "gate", "providers", "alpha", "endpoint"),
        );

        let writes = dispatch_settings_key(
            &mut reader,
            &oxpath!("settings", "accounts"),
            &KeyChord {
                modifiers: KeyModifierSet::default(),
                code: KeyCodeRepr::Enter,
            },
            &cmds,
            &bindings,
            &renderers,
        );

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "sentinel"));
    }

    #[test]
    fn edit_mode_printable_routes_via_target() {
        // Printable ASCII on inline edit-mode mutates the buffer — the
        // leaf claim. A Target binding at `_edit` must fire when a
        // printable char is pressed while the cursor sits at
        // `settings/_edit`.
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            scope: BindingScope::Exact(oxpath!("settings", "_edit")),
            key: key_char('x'),
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Target,
        });

        let renderers = RendererRegistry::new();
        let mut reader = LocalConfig::default();
        seed_edit_mode(
            &mut reader,
            oxpath!("config", "gate", "providers", "alpha", "endpoint"),
        );

        let writes = dispatch_settings_key(
            &mut reader,
            &oxpath!("settings", "accounts"),
            &key_char('x'),
            &cmds,
            &bindings,
            &renderers,
        );

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "sentinel"));
    }

    // -----------------------------------------------------------------
    // `compute_scope_path` structural ordering tests
    //
    // The dispatcher's three-phase walk (Capture outer→inner, Target on
    // the leaf only, Bubble inner→outer) depends on the scope path
    // being assembled in a fixed order:
    //
    //   cursor → focused-row → compound-widget-container → compound-widget-leaf
    //
    // Today that ordering is convention enforced by reading
    // `compute_scope_path`. These tests pin it position-by-position so
    // a reorder that quietly swaps inner / outer trips a unit test
    // instead of a runtime keystroke routing to the wrong widget.
    // -----------------------------------------------------------------

    /// Seed `ui/settings/focused = <path>` so `compute_scope_path` pushes
    /// the focused-row scope between the cursor and any compound widget.
    fn seed_focused_row(reader: &mut LocalConfig, focused: Path) {
        use super::super::commands::navigation::path_to_value;
        reader
            .write(
                &oxpath!("ui", "settings", "focused"),
                Record::parsed(path_to_value(&focused)),
            )
            .unwrap();
    }

    /// Seed `ui/settings/focused = settings/_compose_form/<field>` —
    /// under cursor-as-focus this single write puts the user inside
    /// the compose form on the named field. The dispatcher's
    /// `compute_scope_path` walks the cursor's ancestors to derive
    /// both the form scope and the per-field leaf scope.
    fn seed_compose_cursor(reader: &mut LocalConfig, field: &str) {
        use super::super::commands::navigation::path_to_value;
        let comp = ox_kernel::PathComponent::try_new(field).unwrap();
        let path = oxpath!("settings", "_compose_form", comp);
        reader
            .write(
                &oxpath!("ui", "settings", "focused"),
                Record::parsed(path_to_value(&path)),
            )
            .unwrap();
    }

    /// Seed `ui/settings/focused = settings/_manual_model/<stage>` —
    /// under cursor-as-focus this single write puts the user inside
    /// the manual-model wizard at the named stage. The dispatcher's
    /// `compute_scope_path` walks the cursor's ancestors to derive
    /// both the form scope and the per-stage leaf scope.
    fn seed_manual_model_cursor(
        reader: &mut LocalConfig,
        stage: ox_types::settings::ManualModelStage,
    ) {
        use super::super::commands::account_model::manual_model_focus_path;
        use super::super::commands::navigation::path_to_value;
        let path = manual_model_focus_path(stage);
        reader
            .write(
                &oxpath!("ui", "settings", "focused"),
                Record::parsed(path_to_value(&path)),
            )
            .unwrap();
    }

    /// Seed `ui/settings/focused = settings/_confirm_delete` — under
    /// cursor-as-focus this single write engages confirm-delete mode.
    /// The dispatcher's `compute_scope_path` pushes the focused row
    /// onto the scope path, which IS the leaf for this single-scope
    /// widget.
    fn seed_confirm_delete_cursor(reader: &mut LocalConfig) {
        use super::super::commands::navigation::path_to_value;
        reader
            .write(
                &oxpath!("ui", "settings", "focused"),
                Record::parsed(path_to_value(&oxpath!("settings", "_confirm_delete"))),
            )
            .unwrap();
    }

    #[test]
    fn scope_path_falls_back_to_screen_root_when_no_cursor_seeded() {
        // With no `ui/settings/focused` written, `read_cursor` returns
        // None. The scope path falls back to the screen-root scope
        // (`settings`) so page-level Bubble bindings remain reachable —
        // critical for j/k navigation to work on first entry, before any
        // row has been focused.
        let mut reader = LocalConfig::default();

        let path = compute_scope_path(&mut reader);

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
        let mut reader = LocalConfig::default();
        seed_focused_row(&mut reader, oxpath!("settings", "accounts", "alpha"));

        let path = compute_scope_path(&mut reader);

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
        let mut reader = LocalConfig::default();
        seed_focused_row(&mut reader, oxpath!("settings", "accounts"));

        let path = compute_scope_path(&mut reader);

        assert_eq!(path.len(), 2);
        assert_eq!(path[0], BindingScope::Exact(oxpath!("settings")));
        assert_eq!(
            path[1],
            BindingScope::Exact(oxpath!("settings", "accounts"))
        );
    }

    #[test]
    fn scope_path_for_compose_is_settings_then_form_then_field_leaf() {
        // Compose mode with the Name field focused: the cursor sits at
        // `settings/_compose_form/name`; its ancestor chain is
        // [settings, settings/_compose_form, settings/_compose_form/name].
        // The per-field leaf sits at the inner end so Target fires on it.
        let mut reader = LocalConfig::default();
        seed_compose_cursor(&mut reader, "name");

        let path = compute_scope_path(&mut reader);

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
        // Same shape with the Protocol field focused — leaf flips to
        // the protocol-field scope. The container scope is unchanged.
        let mut reader = LocalConfig::default();
        seed_compose_cursor(&mut reader, "protocol");

        let path = compute_scope_path(&mut reader);

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
        // Regression for the cursor-as-focus invariant: the form scope
        // and per-field leaf must appear on the path when the cursor
        // sits at a compose field path.
        let mut reader = LocalConfig::default();
        seed_compose_cursor(&mut reader, "name");
        let path = compute_scope_path(&mut reader);
        assert!(path.contains(&BindingScope::Exact(oxpath!("settings", "_compose_form"))));
        assert!(path.contains(&BindingScope::Exact(oxpath!(
            "settings",
            "_compose_form",
            "name"
        ))));
    }

    #[test]
    fn cursor_at_account_row_does_not_include_compose_form_scope() {
        // No compose engaged when cursor sits at a regular account row.
        let mut reader = LocalConfig::default();
        seed_focused_row(&mut reader, oxpath!("settings", "accounts", "alpha"));
        let path = compute_scope_path(&mut reader);
        assert!(
            !path.contains(&BindingScope::Exact(oxpath!("settings", "_compose_form"))),
            "no compose form scope when cursor is on an account row: {path:?}",
        );
    }

    #[test]
    fn scope_path_for_manual_model_is_settings_then_form_then_stage_leaf() {
        // Manual-model mode with stage Ctx: cursor at
        // `settings/_manual_model/ctx`; ancestors are
        // [settings, settings/_manual_model, settings/_manual_model/ctx].
        use ox_types::settings::ManualModelStage;
        let mut reader = LocalConfig::default();
        seed_manual_model_cursor(&mut reader, ManualModelStage::Ctx);

        let path = compute_scope_path(&mut reader);

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
        // Spot-check the cursor's leaf segment drives the binding scope.
        // With cursor at `settings/_manual_model/id` the leaf must be
        // `_manual_model/id`, not a shared singleton.
        use ox_types::settings::ManualModelStage;
        let mut reader = LocalConfig::default();
        seed_manual_model_cursor(&mut reader, ManualModelStage::Id);

        let path = compute_scope_path(&mut reader);

        assert_eq!(
            path.last().unwrap(),
            &BindingScope::Exact(oxpath!("settings", "_manual_model", "id"))
        );
    }

    #[test]
    fn compute_scope_path_includes_manual_model_when_cursor_at_stage() {
        // Regression for cursor-as-focus: the form scope and per-stage
        // leaf must appear on the path when the cursor sits at a
        // manual-model stage path.
        use ox_types::settings::ManualModelStage;
        let mut reader = LocalConfig::default();
        seed_manual_model_cursor(&mut reader, ManualModelStage::Id);
        let path = compute_scope_path(&mut reader);
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
        // Confirm-delete is a single-leaf compound widget (no sub-form):
        // cursor at `settings/_confirm_delete`; ancestors are
        // [settings, settings/_confirm_delete]. The leaf is innermost so
        // Target / Capture bindings on the dialog get first crack at the
        // key.
        let mut reader = LocalConfig::default();
        seed_confirm_delete_cursor(&mut reader);

        let path = compute_scope_path(&mut reader);

        assert_eq!(path.len(), 2);
        assert_eq!(path[0], BindingScope::Exact(oxpath!("settings")));
        assert_eq!(
            path[1],
            BindingScope::Exact(oxpath!("settings", "_confirm_delete"))
        );
    }

    #[test]
    fn compute_scope_path_includes_confirm_delete_when_cursor_at_it() {
        // Regression for cursor-as-focus: the `_confirm_delete` scope
        // must appear on the path when the cursor sits there.
        let mut reader = LocalConfig::default();
        seed_confirm_delete_cursor(&mut reader);
        let path = compute_scope_path(&mut reader);
        assert!(
            path.contains(&BindingScope::Exact(oxpath!("settings", "_confirm_delete"))),
            "expected confirm-delete leaf on path: {path:?}",
        );
    }

    #[test]
    fn scope_path_for_edit_mode_ends_at_edit_scope() {
        // Inline edit-mode: cursor at `settings/_edit`; ancestors are
        // [settings, settings/_edit]. The leaf is innermost so Target
        // bindings on the edit scope get first crack at printable keys.
        let mut reader = LocalConfig::default();
        seed_edit_mode(
            &mut reader,
            oxpath!("config", "gate", "providers", "alpha", "endpoint"),
        );

        let path = compute_scope_path(&mut reader);

        assert_eq!(path.len(), 2);
        assert_eq!(path[0], BindingScope::Exact(oxpath!("settings")));
        assert_eq!(path[1], BindingScope::Exact(oxpath!("settings", "_edit")));
    }

    #[test]
    fn compute_scope_path_includes_edit_when_cursor_at_it() {
        // Regression for cursor-as-focus: the `_edit` scope must appear
        // on the path when the cursor sits there.
        let mut reader = LocalConfig::default();
        seed_edit_mode(
            &mut reader,
            oxpath!("settings", "accounts", "alpha", "endpoint"),
        );
        let path = compute_scope_path(&mut reader);
        assert!(
            path.contains(&BindingScope::Exact(oxpath!("settings", "_edit"))),
            "expected edit leaf on path: {path:?}",
        );
    }

    // -----------------------------------------------------------------
    // `path_ancestors` shape tests
    //
    // `path_ancestors` is the cornerstone of `compute_scope_path` under
    // cursor-as-focus. Pin the edge cases so a future refactor that
    // tweaks the prefix-walk shape (e.g. starts at depth 0 instead of
    // depth 1, or drops the full-path tail) trips a unit test instead
    // of routing keystrokes to wrong scopes.
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

    // Note: the legacy "focused row + compose both engaged" test was
    // retired with cursor-as-focus — the focused row IS the compose
    // field under the new model, so the four-level ordering no longer
    // applies.
}
