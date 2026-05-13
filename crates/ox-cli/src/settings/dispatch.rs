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
//!    leaf didn't consume (e.g. compose Enter). At each scope the
//!    pass tries `Phase::Bubble` first, then falls back to
//!    `Phase::Target` so legacy outer-scope bindings (page-cursor
//!    `j`/`k`, focused-row `a`/`t`) still fire when the leaf misses.
//!    The fallback retires once those scopes' bindings are migrated
//!    to declare `Phase::Bubble` explicitly.
//!
//! `compute_scope_path` reads UI-state discriminators to assemble the
//! path. Adding a new compound widget = extend `compute_scope_path` +
//! register bindings under the new scope with appropriate phases. No
//! dispatcher changes.

use structfs_core_store::{Path, Reader};

use ox_types::settings::AccountField;
use ox_types::subscription::Write;
use ox_types::{BindingScope, KeyChord, Mode, Phase, Screen};

use super::binding_registry::BindingRegistry;
use super::command_registry::{CommandCtx, CommandRegistry};
use super::commands::account_model::{FieldKind, field_kind};
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
    // consume. The Target fallback per scope is the bridge that keeps
    // unmigrated outer-scope bindings (page-cursor `j`/`k`, focused-row
    // `a`/`t`) reachable while S3–S5 migrate them to declare
    // `Phase::Bubble` explicitly.
    if cmd_id_opt.is_none() {
        for scope_path_entry in scope_path.iter().rev() {
            let Some(p) = scope_path_entry.keyed_path() else {
                continue;
            };
            if let Some(hit) = bindings.lookup(screen, p, mode, key, Phase::Bubble) {
                cmd_id_opt = Some(hit);
                break;
            }
            if let Some(hit) = bindings.lookup(screen, p, mode, key, Phase::Target) {
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
fn compute_scope_path(snapshot: &mut dyn Reader, cursor: &Path) -> Vec<BindingScope> {
    let mut path = vec![BindingScope::Exact(cursor.clone())];

    // Focused row is the widget the user has navigated to inside the
    // page. When set and different from the cursor, it sits between
    // the cursor (outer container) and any compound widget (innermost).
    if let Some(focused) = read_focused(snapshot) {
        if &focused != cursor {
            path.push(BindingScope::Exact(focused));
        }
    }

    let pending_delete = read_pending_delete(snapshot).is_some();
    let manual_model_stage = read_manual_model_stage(snapshot);
    let compose_active = read_compose_active(snapshot);
    let edit_mode_active = read_edit_mode_active(snapshot);

    // Compound-widget modes are mutually exclusive by design. Violating
    // this invariant isn't just hygiene: `compute_scope_path` would push
    // both compound widgets' scopes, the second-pushed becomes the leaf,
    // and the first widget's Target bindings get bypassed (Capture and
    // Bubble still reach them, but the Target phase — the leaf claim —
    // skips its semantics). Net effect: keystrokes route to the wrong
    // widget.
    debug_assert!(
        [
            pending_delete,
            manual_model_stage.is_some(),
            compose_active,
            edit_mode_active,
        ]
        .iter()
        .filter(|b| **b)
        .count()
            <= 1,
        "at most one compound-widget mode active at a time; violation routes keys to wrong widget",
    );

    if pending_delete {
        path.push(BindingScope::Exact(ox_path::oxpath!(
            "settings",
            "_pending_delete"
        )));
    }
    if let Some(stage) = manual_model_stage {
        path.push(BindingScope::Exact(ox_path::oxpath!(
            "settings",
            "_manual_model"
        )));
        // Per-stage leaf scope. S4 migrates per-stage bindings here;
        // for now `_manual_model/<stage>` has no entries but
        // pre-allocating it in the path keeps S4 a pure bindings.rs
        // change. The path component is a string literal per stage so
        // the `oxpath!` macro can validate at compile time.
        use ox_types::settings::ManualModelStage;
        let stage_scope = match stage {
            ManualModelStage::Id => ox_path::oxpath!("settings", "_manual_model", "Id"),
            ManualModelStage::Ctx => ox_path::oxpath!("settings", "_manual_model", "Ctx"),
            ManualModelStage::Out => ox_path::oxpath!("settings", "_manual_model", "Out"),
        };
        path.push(BindingScope::Exact(stage_scope));
    }
    if compose_active {
        path.push(BindingScope::Exact(ox_path::oxpath!(
            "settings",
            "_compose_form"
        )));
        let leaf = match field_kind(read_focused_compose_field(snapshot)) {
            FieldKind::Text => ox_path::oxpath!("settings", "_compose_field_text"),
            FieldKind::Selector => ox_path::oxpath!("settings", "_compose_field_selector"),
        };
        path.push(BindingScope::Exact(leaf));
    }
    if edit_mode_active {
        path.push(BindingScope::Exact(ox_path::oxpath!(
            "settings",
            "_edit_mode"
        )));
    }

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

/// Read the inline edit-mode flag.
fn read_edit_mode_active(snapshot: &mut dyn Reader) -> bool {
    use ox_path::oxpath;
    use structfs_core_store::Value;
    snapshot
        .read(&oxpath!("ui", "settings", "edit_mode"))
        .ok()
        .flatten()
        .and_then(|r| match r.as_value() {
            Some(Value::Bool(b)) => Some(*b),
            _ => None,
        })
        .unwrap_or(false)
}

/// Read `ui/settings/new_account/focused_field` as an `AccountField`,
/// defaulting to `Name` when missing or untyped. Mirrors the helper of
/// the same purpose in `commands/account_model.rs` but inlined here so
/// the dispatcher doesn't have to import a private snapshot reader.
fn read_focused_compose_field(snapshot: &mut dyn Reader) -> AccountField {
    use ox_path::oxpath;
    let record = match snapshot
        .read(&oxpath!("ui", "settings", "new_account", "focused_field"))
        .ok()
        .flatten()
    {
        Some(r) => r,
        None => return AccountField::Name,
    };
    let value = match record.as_value() {
        Some(v) => v.clone(),
        None => return AccountField::Name,
    };
    structfs_serde_store::from_value::<AccountField>(value).unwrap_or(AccountField::Name)
}

/// Read the compose-mode discriminator at
/// `ui/settings/new_account/active`. Returns `true` only when the
/// stored value is `Bool(true)`; any other shape (missing, wrong type,
/// `false`) reads as inactive. Compose state lives in the
/// `new_account/*` subtree as a whole form; this flag is the single
/// signal the dispatcher keys on.
fn read_compose_active(snapshot: &mut dyn Reader) -> bool {
    use ox_path::oxpath;
    use structfs_core_store::Value;
    snapshot
        .read(&oxpath!("ui", "settings", "new_account", "active"))
        .ok()
        .flatten()
        .and_then(|r| match r.as_value() {
            Some(Value::Bool(b)) => Some(*b),
            _ => None,
        })
        .unwrap_or(false)
}

/// Read `ui/settings/pending_delete`. Returns `Some(_)` when the user
/// is being asked to confirm a delete (pending-delete mode).
fn read_pending_delete(snapshot: &mut dyn Reader) -> Option<String> {
    use ox_path::oxpath;
    use structfs_core_store::Value;
    let record = snapshot
        .read(&oxpath!("ui", "settings", "pending_delete"))
        .ok()
        .flatten()?;
    match record.as_value()? {
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// Read `ui/settings/manual_model/stage` as a typed `ManualModelStage`.
/// Returns `Some(stage)` only when the stored value matches the typed
/// wire shape (`"Id"` / `"Ctx"` / `"Out"`).
///
/// The legacy stringly-typed write site stores `"id"` / `"ctx"` /
/// `"out"` (snake_case); those fail the typed deserialize, so the
/// caller treats them as "manual-model mode not active" and the
/// dispatcher falls through to the remaining scope-path walks. That
/// coexistence is what keeps the new pass dormant until the entry
/// point is rewired.
fn read_manual_model_stage(
    snapshot: &mut dyn Reader,
) -> Option<ox_types::settings::ManualModelStage> {
    use ox_path::oxpath;
    use ox_types::settings::ManualModelStage;
    let record = snapshot
        .read(&oxpath!("ui", "settings", "manual_model", "stage"))
        .ok()
        .flatten()?;
    let value = record.as_value()?.clone();
    structfs_serde_store::from_value::<ManualModelStage>(value).ok()
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
    fn dispatcher_enters_compose_scope_when_active_is_true() {
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        // `'a'` is a target-phase key on a text field; bind it under the
        // text-field leaf scope so the dispatcher's three-phase walk
        // picks it up at target.
        bindings.register(BindingEntry {
            screen: Screen::Settings,
            scope: ox_types::BindingScope::Exact(oxpath!("settings", "_compose_field_text")),
            mode: None,
            key: key_char('a'),
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Target,
        });

        let renderers = RendererRegistry::new();
        let mut reader = LocalConfig::default();
        // Seed the form snapshot the compose flow writes on open: an
        // explicit `active = true` discriminator plus the focused-field
        // and name fields the View::Form renders from.
        reader
            .write(
                &oxpath!("ui", "settings", "new_account", "active"),
                Record::parsed(Value::Bool(true)),
            )
            .unwrap();
        reader
            .write(
                &oxpath!("ui", "settings", "new_account", "focused_field"),
                Record::parsed(Value::String("name".into())),
            )
            .unwrap();
        reader
            .write(
                &oxpath!("ui", "settings", "new_account", "name"),
                Record::parsed(Value::String(String::new())),
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
    fn dispatcher_skips_compose_scope_when_active_absent() {
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        // Bind ONLY at the compose-field scope — should not match
        // because `active` is unset (no fallthrough scope picks 'a' up).
        bindings.register(BindingEntry {
            screen: Screen::Settings,
            scope: ox_types::BindingScope::Exact(oxpath!("settings", "_compose_field_text")),
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
        // compose mode under the new discriminator — only an explicit
        // `active == true` opens the synthetic scope.
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            screen: Screen::Settings,
            scope: ox_types::BindingScope::Exact(oxpath!("settings", "_compose_field_text")),
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
    fn pending_delete_routes_to_pending_delete_scope_when_set() {
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            screen: Screen::Settings,
            scope: ox_types::BindingScope::Exact(oxpath!("settings", "_pending_delete")),
            mode: None,
            key: key_char('y'),
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Target,
        });

        let renderers = RendererRegistry::new();
        let mut reader = LocalConfig::default();
        reader
            .write(
                &oxpath!("ui", "settings", "pending_delete"),
                Record::parsed(Value::String("alpha".into())),
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
        // Esc on the pending-delete confirmation dialog is a lifecycle
        // key — the container claims it at Capture before any leaf sees
        // it. A binding registered at Phase::Capture on the
        // `_pending_delete` scope must fire when Esc is pressed while
        // pending_delete is set.
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            screen: Screen::Settings,
            scope: ox_types::BindingScope::Exact(oxpath!("settings", "_pending_delete")),
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
        reader
            .write(
                &oxpath!("ui", "settings", "pending_delete"),
                Record::parsed(Value::String("alpha".into())),
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
        // `_pending_delete` must fire when `y` is pressed while
        // pending_delete is set.
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            screen: Screen::Settings,
            scope: ox_types::BindingScope::Exact(oxpath!("settings", "_pending_delete")),
            mode: None,
            key: key_char('y'),
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Target,
        });

        let renderers = RendererRegistry::new();
        let mut reader = LocalConfig::default();
        reader
            .write(
                &oxpath!("ui", "settings", "pending_delete"),
                Record::parsed(Value::String("alpha".into())),
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
    fn manual_model_routes_to_manual_model_scope_when_typed_stage_set() {
        use ox_types::settings::ManualModelStage;

        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            screen: Screen::Settings,
            scope: ox_types::BindingScope::Exact(oxpath!("settings", "_manual_model")),
            mode: None,
            key: key_char('a'),
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Target,
        });

        let renderers = RendererRegistry::new();
        let mut reader = LocalConfig::default();
        reader
            .write(
                &oxpath!("ui", "settings", "manual_model", "stage"),
                Record::parsed(structfs_serde_store::to_value(&ManualModelStage::Id).unwrap()),
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
    fn manual_model_falls_through_when_legacy_stringly_stage() {
        // A stale Value::String("id") at the stage path (the legacy
        // wire format) must not engage the manual-model scope — the
        // dispatcher's discriminator deserializes as ManualModelStage
        // and only fires the pass on success. Falls through to the
        // remaining dispatch passes; with no other binding registered,
        // result is empty.
        let cmds = CommandRegistry::new();
        let mut bindings = BindingRegistry::new();
        bindings.register(BindingEntry {
            screen: Screen::Settings,
            scope: ox_types::BindingScope::Exact(oxpath!("settings", "_manual_model")),
            mode: None,
            key: key_char('a'),
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Target,
        });

        let renderers = RendererRegistry::new();
        let mut reader = LocalConfig::default();
        reader
            .write(
                &oxpath!("ui", "settings", "manual_model", "stage"),
                Record::parsed(Value::String("id".into())),
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

        // The new pass shouldn't fire — the value isn't the typed shape.
        // Bindings would fall through; with no other binding registered,
        // result is empty.
        assert!(writes.is_empty());
    }

    #[test]
    fn page_cursor_target_binding_fires_via_bubble_when_no_compound_widget_active() {
        // The Bubble loop falls back to Phase::Target per scope. This is
        // a transitional bridge that keeps unmigrated page-cursor and
        // focused-row bindings reachable until S5.5 migrates them to
        // declare Phase::Bubble explicitly. Pin the behavior so that
        // removal is a controlled diff rather than a silent break.
        //
        // Setup: cursor at settings/index with a focused row at
        // settings/accounts, no compound widget active. The scope path
        // is then [Exact(settings/index), Exact(settings/accounts)] —
        // leaf is `settings/accounts`, not the cursor. With the page
        // cursor's `j` binding still at Phase::Target (current state),
        // it reaches via the Bubble loop's Target fallback at the outer
        // (cursor) scope. After S5.5 the same binding will declare
        // Phase::Bubble and fire on the Bubble pass directly; either
        // way this test passes.
        let mut cmds = CommandRegistry::new();
        cmds.register(Box::new(WriteSentinel::new()));

        let mut bindings = BindingRegistry::new();
        // Page-cursor `j` lives on the outer scope (`settings/index`)
        // at Phase::Target — the shape the dispatcher's Bubble→Target
        // fallback is the only route for, given a deeper leaf scope.
        bindings.register(BindingEntry {
            screen: Screen::Settings,
            scope: ox_types::BindingScope::Exact(oxpath!("settings", "index")),
            mode: None,
            key: key_char('j'),
            command_id: cmd_id("test.sentinel"),
            phase: Phase::Target,
        });

        let renderers = RendererRegistry::new();
        let mut reader = LocalConfig::default();
        // Seed the focused-row scope so the leaf differs from the cursor;
        // otherwise Target phase at the leaf would hit directly and the
        // fallback wouldn't be exercised.
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

        // The sentinel fires only if the Bubble loop's Target fallback
        // routes `j` from the outer (cursor) scope. Asserting the write
        // pins the fallback: removing it today would flip this test red.
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "sentinel"));
    }
}
