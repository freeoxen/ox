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
//! `compute_scope_path` reads UI-state discriminators to assemble the
//! path. Adding a new compound widget = extend `compute_scope_path` +
//! register bindings under the new scope with appropriate phases. No
//! dispatcher changes.

use structfs_core_store::{Path, Reader};

use ox_types::subscription::Write;
use ox_types::{BindingScope, KeyChord, Mode, Phase, Screen};

use super::binding_registry::BindingRegistry;
use super::command_registry::{CommandCtx, CommandRegistry};
use super::commands::account_model::{
    cursor_is_in_compose_form, cursor_is_in_confirm_delete, cursor_is_in_edit,
    cursor_is_in_manual_model, path_ancestors,
};
use super::registry::RendererRegistry;

/// Resolve `(screen, cursor, mode, key)` to a sequence of writes by
/// looking up the binding, then the command, then running it. Returns
/// `vec![]` (inert) on any miss.
//
// 8-parameter signature is spec-prescribed (settings-screen-redesign
// plan, Phase H Task H3). Each argument is independently varied by
// callers, so packing them into a struct would be ceremony without
// information gain.
#[allow(clippy::too_many_arguments)]
pub fn dispatch_settings_key(
    snapshot: &mut dyn Reader,
    screen: Screen,
    cursor: &Path,
    mode: Option<Mode>,
    key: &KeyChord,
    cmds: &CommandRegistry,
    bindings: &BindingRegistry,
    renderers: &RendererRegistry,
) -> Vec<Write> {
    let scope_path = compute_scope_path(snapshot, cursor);

    // Capture (outer → inner): containers claim lifecycle keys before
    // the leaf sees them.
    let mut cmd_id_opt = None;
    for scope_path_entry in &scope_path {
        if let Some(p) = scope_path_entry.keyed_path() {
            if let Some(hit) = bindings.lookup(screen, p, mode, key, Phase::Capture) {
                cmd_id_opt = Some(hit);
                break;
            }
        }
    }

    // Target (leaf only): the innermost scope claims the key.
    if cmd_id_opt.is_none() {
        if let Some(leaf) = scope_path.last().and_then(BindingScope::keyed_path) {
            cmd_id_opt = bindings.lookup(screen, leaf, mode, key, Phase::Target);
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
            if let Some(hit) = bindings.lookup(screen, p, mode, key, Phase::Bubble) {
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

/// Assemble the outer → inner scope path the dispatcher walks. The
/// page cursor is always outermost; the focused-row scope (when set
/// and distinct from the cursor) sits one level deeper; the active
/// compound widget pushes its own scope(s) on top.
///
/// Compound widgets are mutually exclusive by design — a `debug_assert`
/// enforces that at most one is active at a time.
///
/// `pub(crate)` so the dispatcher's tests can assert the returned
/// ordering directly. The structural ordering — cursor → focused-row →
/// compound-widget-container → compound-widget-leaf — is the contract
/// every phase-walk in the dispatcher depends on; the tests in this
/// file pin it.
pub(crate) fn compute_scope_path(snapshot: &mut dyn Reader, cursor: &Path) -> Vec<BindingScope> {
    let mut path = vec![BindingScope::Exact(cursor.clone())];

    // Focused row is the widget the user has navigated to inside the
    // page. When set and different from the cursor, it sits between
    // the cursor (outer container) and any compound widget (innermost).
    //
    // Under cursor-as-focus, the compose form's focused row IS at
    // `settings/_compose_form/<field>`, so this push naturally produces
    // the per-field leaf scope on the path — no separate discriminator
    // branch needed. The intermediate container scope
    // (`settings/_compose_form`) is added by the cursor-ancestor walk
    // below.
    let focused = read_focused(snapshot);
    let focused_for_compose = focused.clone();
    if let Some(f) = focused {
        if &f != cursor {
            path.push(BindingScope::Exact(f));
        }
    }

    let compose_focused = focused_for_compose
        .as_ref()
        .is_some_and(cursor_is_in_compose_form);
    let manual_model_focused = focused_for_compose
        .as_ref()
        .is_some_and(cursor_is_in_manual_model);
    let confirm_delete_focused = focused_for_compose
        .as_ref()
        .is_some_and(cursor_is_in_confirm_delete);
    let edit_mode_active = focused_for_compose.as_ref().is_some_and(cursor_is_in_edit);

    // Compound-widget modes are mutually exclusive by design. Violating
    // this invariant isn't just hygiene: `compute_scope_path` would push
    // both compound widgets' scopes, the second-pushed becomes the leaf,
    // and the first widget's Target bindings get bypassed (Capture and
    // Bubble still reach them, but the Target phase — the leaf claim —
    // skips its semantics). Net effect: keystrokes route to the wrong
    // widget.
    debug_assert!(
        [
            confirm_delete_focused,
            manual_model_focused,
            compose_focused,
            edit_mode_active,
        ]
        .iter()
        .filter(|b| **b)
        .count()
            <= 1,
        "at most one compound-widget mode active at a time; violation routes keys to wrong widget",
    );

    if let Some(compose_cursor) = focused_for_compose
        .clone()
        .filter(cursor_is_in_compose_form)
    {
        // Cursor-as-focus: derive the compose-form scope chain from the
        // cursor's ancestors. The per-field leaf scope is already on
        // the path (pushed via `read_focused` above). What's missing
        // is the intermediate container scope `settings/_compose_form`
        // — inject it between the existing focused-row leaf and the
        // outer page cursor.
        let ancestors = path_ancestors(&compose_cursor);
        // Inject every ancestor that isn't already in the path. In
        // practice this is just the form scope (cursor itself is at
        // depth 3; depth-1 `settings` is too generic to want pushed).
        // Insert before the focused-row leaf so the ordering stays
        // outer → inner.
        let mut to_insert: Vec<BindingScope> = ancestors
            .into_iter()
            .filter(|a| a.components.len() == 2) // settings/_compose_form
            .map(BindingScope::Exact)
            .collect();
        // Insert just before the last element (the per-field leaf).
        // If the path's tail is the per-field leaf (always true here),
        // pop it, append intermediates, then push the leaf back.
        if let Some(leaf) = path.pop() {
            path.append(&mut to_insert);
            path.push(leaf);
        }
    }
    if let Some(manual_cursor) = focused_for_compose.filter(cursor_is_in_manual_model) {
        // Cursor-as-focus: same pattern as compose. The cursor sits at
        // `settings/_manual_model/<stage>` and is already on the path
        // as the focused-row leaf; we inject the intermediate
        // container scope `settings/_manual_model` between the leaf
        // and the outer page cursor.
        let ancestors = path_ancestors(&manual_cursor);
        let mut to_insert: Vec<BindingScope> = ancestors
            .into_iter()
            .filter(|a| a.components.len() == 2) // settings/_manual_model
            .map(BindingScope::Exact)
            .collect();
        if let Some(leaf) = path.pop() {
            path.append(&mut to_insert);
            path.push(leaf);
        }
    }
    // edit_mode_active: like confirm-delete, the cursor at
    // `settings/_edit` IS the leaf; `read_focused` already pushed it
    // onto the path via the focused-row push. No separate container
    // scope to inject.

    path
}

/// Read `ui/settings/focused` from the dispatch snapshot. Used as the
/// focused-widget binding-scope cursor so per-row bindings can fire
/// while the page cursor sits at `settings/index`.
fn read_focused(snapshot: &mut dyn Reader) -> Option<Path> {
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
                scope: CommandScope {
                    screen: Screen::Settings,
                    cursor_path: None,
                },
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
                scope: CommandScope {
                    screen: Screen::Settings,
                    cursor_path: None,
                },
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
            screen: Screen::Settings,
            scope: ox_types::BindingScope::Anywhere,
            mode: None,
            key: key_char('a'),
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Target,
        });

        let renderers = RendererRegistry::new();
        let mut reader = LocalConfig::default();

        let writes = dispatch_settings_key(
            &mut reader,
            Screen::Settings,
            &oxpath!(),
            None,
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
            Screen::Settings,
            &oxpath!(),
            None,
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
            screen: Screen::Settings,
            scope: ox_types::BindingScope::Anywhere,
            mode: None,
            key: key_char('a'),
            command_id: cmd_id("not.registered"),
            phase: Phase::Target,
        });

        let renderers = RendererRegistry::new();
        let mut reader = LocalConfig::default();

        let writes = dispatch_settings_key(
            &mut reader,
            Screen::Settings,
            &oxpath!(),
            None,
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
            screen: Screen::Settings,
            scope: ox_types::BindingScope::Exact(oxpath!(
                "settings",
                "_compose_form",
                "name"
            )),
            mode: None,
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
                Record::parsed(path_to_value(&oxpath!(
                    "settings",
                    "_compose_form",
                    "name"
                ))),
            )
            .unwrap();

        let writes = dispatch_settings_key(
            &mut reader,
            Screen::Settings,
            &oxpath!("settings", "index"),
            None,
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
            screen: Screen::Settings,
            scope: ox_types::BindingScope::Exact(oxpath!(
                "settings",
                "_compose_form",
                "name"
            )),
            mode: None,
            key: key_char('a'),
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Target,
        });

        let renderers = RendererRegistry::new();
        let mut reader = LocalConfig::default();

        let writes = dispatch_settings_key(
            &mut reader,
            Screen::Settings,
            &oxpath!("settings", "index"),
            None,
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
            screen: Screen::Settings,
            scope: ox_types::BindingScope::Exact(oxpath!(
                "settings",
                "_compose_form",
                "name"
            )),
            mode: None,
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
            Screen::Settings,
            &oxpath!("settings", "index"),
            None,
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
            screen: Screen::Settings,
            scope: ox_types::BindingScope::Exact(oxpath!("settings", "_confirm_delete")),
            mode: None,
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
            Screen::Settings,
            &oxpath!("settings", "index"),
            None,
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
            screen: Screen::Settings,
            scope: ox_types::BindingScope::Exact(oxpath!("settings", "_confirm_delete")),
            mode: None,
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
            Screen::Settings,
            &oxpath!("settings", "accounts"),
            None,
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
            screen: Screen::Settings,
            scope: ox_types::BindingScope::Exact(oxpath!("settings", "_confirm_delete")),
            mode: None,
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
            Screen::Settings,
            &oxpath!("settings", "accounts"),
            None,
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
            screen: Screen::Settings,
            scope: ox_types::BindingScope::Anywhere,
            mode: None,
            key: key_char('z'),
            command_id: cmd_id("test.report_keystroke"),
            phase: Phase::Target,
        });

        let renderers = RendererRegistry::new();
        let mut reader = LocalConfig::default();

        let writes = dispatch_settings_key(
            &mut reader,
            Screen::Settings,
            &oxpath!(),
            None,
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
            screen: Screen::Settings,
            scope: ox_types::BindingScope::Exact(oxpath!("settings", "_manual_model")),
            mode: None,
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
                Record::parsed(path_to_value(&oxpath!(
                    "settings",
                    "_manual_model",
                    "id"
                ))),
            )
            .unwrap();

        let writes = dispatch_settings_key(
            &mut reader,
            Screen::Settings,
            &oxpath!("settings", "index"),
            None,
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
            screen: Screen::Settings,
            scope: ox_types::BindingScope::Exact(oxpath!("settings", "_manual_model")),
            mode: None,
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
                Record::parsed(path_to_value(&oxpath!(
                    "settings",
                    "_manual_model",
                    "id"
                ))),
            )
            .unwrap();

        let writes = dispatch_settings_key(
            &mut reader,
            Screen::Settings,
            &oxpath!("settings", "models"),
            None,
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
            screen: Screen::Settings,
            scope: ox_types::BindingScope::Exact(oxpath!("settings", "_manual_model")),
            mode: None,
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
                Record::parsed(path_to_value(&oxpath!(
                    "settings",
                    "_manual_model",
                    "id"
                ))),
            )
            .unwrap();

        let writes = dispatch_settings_key(
            &mut reader,
            Screen::Settings,
            &oxpath!("settings", "models"),
            None,
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
            screen: Screen::Settings,
            scope: ox_types::BindingScope::Exact(oxpath!("settings", "_manual_model", "id")),
            mode: None,
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
                Record::parsed(path_to_value(&oxpath!(
                    "settings",
                    "_manual_model",
                    "id"
                ))),
            )
            .unwrap();

        let writes = dispatch_settings_key(
            &mut reader,
            Screen::Settings,
            &oxpath!("settings", "models"),
            None,
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
            screen: Screen::Settings,
            scope: ox_types::BindingScope::Exact(oxpath!("settings", "_manual_model", "id")),
            mode: None,
            key: key_char('a'),
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Target,
        });

        let renderers = RendererRegistry::new();
        let mut reader = LocalConfig::default();

        let writes = dispatch_settings_key(
            &mut reader,
            Screen::Settings,
            &oxpath!("settings", "index"),
            None,
            &key_char('a'),
            &cmds,
            &bindings,
            &renderers,
        );

        // No cursor in `_manual_model/*` → scope never on the path.
        assert!(writes.is_empty());
    }

    #[test]
    fn page_cursor_binding_fires_via_bubble_when_no_compound_widget_active() {
        // Page-cursor bindings (j/k navigation at settings/index, the
        // page's *default* row-nav handlers) declare Phase::Bubble. The
        // dispatcher's Bubble pass walks inner → outer and finds them at
        // the outer (cursor) scope, even though the leaf is a deeper
        // focused-row scope.
        //
        // Setup: cursor at settings/index with a focused row at
        // settings/accounts, no compound widget active. Scope path:
        // [Exact(settings/index), Exact(settings/accounts)]. The leaf
        // (`settings/accounts`) has no Bubble binding for `j`; the Bubble
        // walk continues outward to the cursor scope where `j` is
        // registered.
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        // Page-cursor `j` lives on the outer scope (`settings/index`) at
        // Phase::Bubble — the dispatcher's Bubble pass walks inner → outer
        // and finds it without any per-scope Target fallback.
        bindings.register(BindingEntry {
            screen: Screen::Settings,
            scope: ox_types::BindingScope::Exact(oxpath!("settings", "index")),
            mode: None,
            key: key_char('j'),
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Bubble,
        });

        let renderers = RendererRegistry::new();
        let mut reader = LocalConfig::default();
        // Seed the focused-row scope so the leaf differs from the cursor.
        // The Bubble pass still finds `j` at the outer (cursor) scope.
        use super::super::commands::navigation::path_to_value;
        reader
            .write(
                &oxpath!("ui", "settings", "focused"),
                Record::parsed(path_to_value(&oxpath!("settings", "accounts"))),
            )
            .unwrap();

        let writes = dispatch_settings_key(
            &mut reader,
            Screen::Settings,
            &oxpath!("settings", "index"),
            None,
            &key_char('j'),
            &cmds,
            &bindings,
            &renderers,
        );

        // The sentinel fires because the Bubble pass walks outward from
        // the leaf to the cursor scope where the binding lives.
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
            screen: Screen::Settings,
            scope: BindingScope::Exact(oxpath!("settings", "_edit")),
            mode: None,
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
            Screen::Settings,
            &oxpath!("settings", "accounts"),
            None,
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
            screen: Screen::Settings,
            scope: BindingScope::Exact(oxpath!("settings", "_edit")),
            mode: None,
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
            Screen::Settings,
            &oxpath!("settings", "accounts"),
            None,
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
            screen: Screen::Settings,
            scope: BindingScope::Exact(oxpath!("settings", "_edit")),
            mode: None,
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
            Screen::Settings,
            &oxpath!("settings", "accounts"),
            None,
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
    fn scope_path_outer_to_inner_with_no_compound_widget() {
        // Cursor with no focused row and no compound widget yields a
        // one-element path — just the cursor.
        let mut reader = LocalConfig::default();
        let cursor = oxpath!("settings", "accounts");

        let path = compute_scope_path(&mut reader, &cursor);

        assert_eq!(path.len(), 1, "expected only the cursor scope");
        assert_eq!(path[0], BindingScope::Exact(cursor));
    }

    #[test]
    fn scope_path_includes_focused_row_after_cursor() {
        // Focused row distinct from the cursor sits between the cursor
        // (outer) and any compound widget (inner). With no compound
        // widget active, the path is [cursor, focused-row].
        let mut reader = LocalConfig::default();
        let cursor = oxpath!("settings", "accounts");
        seed_focused_row(&mut reader, oxpath!("settings", "accounts", "alpha"));

        let path = compute_scope_path(&mut reader, &cursor);

        assert_eq!(path.len(), 2);
        assert_eq!(path[0], BindingScope::Exact(cursor));
        assert_eq!(
            path[1],
            BindingScope::Exact(oxpath!("settings", "accounts", "alpha"))
        );
    }

    #[test]
    fn scope_path_omits_focused_row_when_same_as_cursor() {
        // The focused-row scope is only pushed when distinct from the
        // cursor — otherwise the path would carry a duplicate entry and
        // the Bubble walk would hit the same scope twice.
        let mut reader = LocalConfig::default();
        let cursor = oxpath!("settings", "accounts");
        seed_focused_row(&mut reader, cursor.clone());

        let path = compute_scope_path(&mut reader, &cursor);

        assert_eq!(path.len(), 1);
        assert_eq!(path[0], BindingScope::Exact(cursor));
    }

    #[test]
    fn scope_path_for_compose_is_cursor_then_form_then_field_leaf() {
        // Compose mode with the Name field focused: the path is
        // cursor → _compose_form → _compose_form/name. The per-field
        // leaf sits at the inner end so Target fires on it.
        let mut reader = LocalConfig::default();
        let cursor = oxpath!("settings", "accounts");
        seed_compose_cursor(&mut reader, "name");

        let path = compute_scope_path(&mut reader, &cursor);

        assert_eq!(path.len(), 3);
        assert_eq!(path[0], BindingScope::Exact(cursor));
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
        let cursor = oxpath!("settings", "accounts");
        seed_compose_cursor(&mut reader, "protocol");

        let path = compute_scope_path(&mut reader, &cursor);

        assert_eq!(path.len(), 3);
        assert_eq!(path[0], BindingScope::Exact(cursor));
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
        // Regression for the cursor-as-focus invariant: ALL of cursor,
        // form scope, and per-field leaf must appear on the path when
        // the cursor sits at a compose field path.
        let mut reader = LocalConfig::default();
        seed_compose_cursor(&mut reader, "name");
        let path = compute_scope_path(&mut reader, &oxpath!("settings", "accounts"));
        assert!(
            path.contains(&BindingScope::Exact(oxpath!(
                "settings",
                "_compose_form"
            )))
        );
        assert!(
            path.contains(&BindingScope::Exact(oxpath!(
                "settings",
                "_compose_form",
                "name"
            )))
        );
    }

    #[test]
    fn cursor_at_account_row_does_not_include_compose_form_scope() {
        // No compose engaged when cursor sits at a regular account row.
        let mut reader = LocalConfig::default();
        seed_focused_row(&mut reader, oxpath!("settings", "accounts", "alpha"));
        let path = compute_scope_path(&mut reader, &oxpath!("settings", "accounts"));
        assert!(
            !path.contains(&BindingScope::Exact(oxpath!(
                "settings",
                "_compose_form"
            ))),
            "no compose form scope when cursor is on an account row: {path:?}",
        );
    }

    #[test]
    fn scope_path_for_manual_model_is_cursor_then_form_then_stage_leaf() {
        // Manual-model mode with stage Ctx: path is cursor → _manual_model
        // → _manual_model/ctx. The per-stage leaf sits innermost so
        // stage-specific Target bindings see the key first.
        use ox_types::settings::ManualModelStage;
        let mut reader = LocalConfig::default();
        let cursor = oxpath!("settings", "models");
        seed_manual_model_cursor(&mut reader, ManualModelStage::Ctx);

        let path = compute_scope_path(&mut reader, &cursor);

        assert_eq!(path.len(), 3);
        assert_eq!(path[0], BindingScope::Exact(cursor));
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

        let path = compute_scope_path(&mut reader, &oxpath!("settings", "models"));

        assert_eq!(
            path.last().unwrap(),
            &BindingScope::Exact(oxpath!("settings", "_manual_model", "id"))
        );
    }

    #[test]
    fn compute_scope_path_includes_manual_model_when_cursor_at_stage() {
        // Regression for cursor-as-focus: ALL of cursor, form scope,
        // and per-stage leaf must appear on the path when the cursor
        // sits at a manual-model stage path.
        use ox_types::settings::ManualModelStage;
        let mut reader = LocalConfig::default();
        seed_manual_model_cursor(&mut reader, ManualModelStage::Id);
        let path = compute_scope_path(&mut reader, &oxpath!("settings", "models"));
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
    fn scope_path_for_confirm_delete_appends_confirm_delete_scope() {
        // Confirm-delete mode is a single-leaf compound widget (no
        // sub-form). Under cursor-as-focus the cursor sitting at
        // `settings/_confirm_delete` IS the leaf — `read_focused`
        // pushes it onto the path. The leaf must be innermost so
        // Target / Capture bindings on the dialog get first crack at
        // the key.
        let mut reader = LocalConfig::default();
        seed_confirm_delete_cursor(&mut reader);

        let path = compute_scope_path(&mut reader, &oxpath!("settings", "accounts"));

        assert_eq!(
            path.last().unwrap(),
            &BindingScope::Exact(oxpath!("settings", "_confirm_delete"))
        );
        // Cursor + confirm-delete leaf — no other scopes.
        assert_eq!(path.len(), 2);
    }

    #[test]
    fn compute_scope_path_includes_confirm_delete_when_cursor_at_it() {
        // Regression for cursor-as-focus: the `_confirm_delete` scope
        // must appear on the path when the cursor sits there. This is
        // the entry into the dispatcher's hierarchical walk for the
        // confirm-delete dialog's bindings (y/n/Esc).
        let mut reader = LocalConfig::default();
        seed_confirm_delete_cursor(&mut reader);
        let path = compute_scope_path(&mut reader, &oxpath!("settings", "accounts"));
        assert!(
            path.contains(&BindingScope::Exact(oxpath!("settings", "_confirm_delete"))),
            "expected confirm-delete leaf on path: {path:?}",
        );
    }

    #[test]
    fn scope_path_for_edit_mode_appends_edit_scope() {
        // Inline edit-mode pushes a single `_edit` leaf at the inner
        // end. Mirrors the confirm-delete shape: no separate form
        // scope, the mode is one leaf — the cursor sitting at
        // `settings/_edit` is what `read_focused` pushes onto the path.
        let mut reader = LocalConfig::default();
        seed_edit_mode(
            &mut reader,
            oxpath!("config", "gate", "providers", "alpha", "endpoint"),
        );

        let path = compute_scope_path(&mut reader, &oxpath!("settings", "accounts"));

        assert_eq!(
            path.last().unwrap(),
            &BindingScope::Exact(oxpath!("settings", "_edit"))
        );
        // Cursor + edit leaf — no other scopes.
        assert_eq!(path.len(), 2);
    }

    #[test]
    fn compute_scope_path_includes_edit_when_cursor_at_it() {
        // Regression for cursor-as-focus: the `_edit` scope must appear
        // on the path when the cursor sits there. This is the entry
        // into the dispatcher's hierarchical walk for the edit
        // mode's bindings (printable chars / Backspace / Enter / Esc).
        let mut reader = LocalConfig::default();
        seed_edit_mode(
            &mut reader,
            oxpath!("settings", "accounts", "alpha", "endpoint"),
        );
        let path = compute_scope_path(&mut reader, &oxpath!("settings", "accounts"));
        assert!(
            path.contains(&BindingScope::Exact(oxpath!("settings", "_edit"))),
            "expected edit leaf on path: {path:?}",
        );
    }

    // Note: the legacy "focused row + compose both engaged" test was
    // retired with cursor-as-focus — the focused row IS the compose
    // field under the new model, so the four-level ordering no longer
    // applies.
}
