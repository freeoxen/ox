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

use structfs_core_store::{Path, Reader};

use ox_types::key_chord::KeyCodeRepr;
use ox_types::settings::AccountField;
use ox_types::subscription::Write;
use ox_types::{CommandId, KeyChord, Mode, Screen};

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
    // Six-pass binding lookup ordered by specificity of context:
    //
    //   1. Pending-delete mode — when `ui/settings/pending_delete` is
    //      `Some(_)`, the user is being asked to confirm a delete.
    //      Bindings live at `Exact(settings/_pending_delete)` and
    //      capture y / n / Esc. Highest priority among modes because
    //      "ready-to-take-action" (y/n) should win deterministically
    //      if any other mode flag was somehow also set; the modes are
    //      mutually exclusive by design.
    //
    //   2. Manual-model mode — when `ui/settings/manual_model/stage`
    //      holds a typed `ManualModelStage` value (PascalCase wire
    //      shape), the user is filling the three-stage manual-model
    //      form. Bindings live at `Exact(settings/_manual_model)` and
    //      capture printable / Backspace / Enter / Esc. The typed
    //      shape is the discriminator: legacy stringly-typed values
    //      ("id"/"ctx"/"out") fail the typed deserialize and fall
    //      through to the edit-mode pass — that lets the new flow
    //      land before the old one retires.
    //
    //   3. Compose mode — when `ui/settings/new_account/active` is
    //      `true`, the user is composing a new connection. The compose
    //      form is a compound widget: lifecycle keys (Esc, Tab,
    //      Shift+Tab, Up, Down, Enter) belong to the form regardless
    //      of focus, while focus-kind-specific keys (printable ASCII
    //      for Text fields; h / l / Left / Right for Selector fields)
    //      belong to the focused leaf. To mirror DOM event-flow shape,
    //      dispatch walks the form + field scopes in three phases —
    //      capture (form lifecycle keys), target (leaf), bubble
    //      (form Enter) — implemented inline below. See
    //      `docs/ui_framework/architecture.md` "Hierarchical dispatch".
    //
    //      The explicit boolean discriminator is single-purpose: it
    //      doesn't entangle with the per-field draft values
    //      (name/provider/...) the way the legacy `buffer` Option
    //      did, so a half-typed field can't accidentally drop us out
    //      of compose. Compose and edit mode are mutually exclusive
    //      (per the spec's mutual-exclusion invariant) but we let
    //      compose win first so a stale `edit_mode = true` flag can't
    //      shadow a legitimate compose.
    //
    //   4. Edit mode — when `ui/settings/edit_mode = true`, the
    //      dispatcher routes printable chars and Backspace to
    //      `edit.insert_char` / `edit.delete_back` and Enter/Esc to
    //      `edit.commit` / `edit.cancel`. These bindings live under
    //      `Exact(settings/_edit_mode)`. The synthetic cursor lets us
    //      reuse the regular registry/lookup machinery without a
    //      special branch — it's data, not code.
    //
    //   5. Focused-row scope — `Prefix(settings/{accounts,models})`
    //      bindings fire on whichever row the user has focused. This
    //      is the per-row action surface (t/r/P/d on a focused leaf).
    //
    //   6. Page cursor — the accordion's tree commands at
    //      `Exact(settings/index)`, plus the legacy `_detail` field
    //      bindings at their own exact cursors.
    let pending_delete_active = read_pending_delete(snapshot).is_some();
    let pending_delete_scope = ox_path::oxpath!("settings", "_pending_delete");
    let manual_model_active = read_manual_model_active(snapshot);
    let manual_model_scope = ox_path::oxpath!("settings", "_manual_model");
    let compose_active = read_compose_active(snapshot);
    let edit_mode_active = read_edit_mode(snapshot);
    let edit_scope = ox_path::oxpath!("settings", "_edit_mode");
    let cmd_id = if pending_delete_active {
        bindings.lookup(screen, &pending_delete_scope, mode, key)
    } else {
        None
    }
    .or_else(|| {
        if manual_model_active {
            bindings.lookup(screen, &manual_model_scope, mode, key)
        } else {
            None
        }
    })
    .or_else(|| {
        if compose_active {
            lookup_compose(bindings, screen, mode, key, snapshot)
        } else {
            None
        }
    })
    .or_else(|| {
        if edit_mode_active {
            bindings.lookup(screen, &edit_scope, mode, key)
        } else {
            None
        }
    })
    .or_else(|| {
        read_focused(snapshot)
            .as_ref()
            .and_then(|focus| bindings.lookup(screen, focus, mode, key))
    })
    .or_else(|| bindings.lookup(screen, cursor, mode, key));
    let Some(cmd_id) = cmd_id else {
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

/// Read `ui/settings/focused` from the dispatch snapshot. Used as
/// the focused-widget binding-scope cursor so per-row bindings can fire
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
fn read_edit_mode(snapshot: &mut dyn Reader) -> bool {
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

/// Hierarchical compose-mode dispatch: walk the form + field synthetic
/// scopes in three phases (capture → target → bubble) and return the
/// first matching `CommandId`.
///
/// Pattern (b) from the T10b plan: a single `BindingRegistry` holds
/// all bindings; this helper queries the *right scope* per phase based
/// on the keystroke's role:
///
/// - **Capture**: lifecycle keys the form claims regardless of focus
///   (Esc, Tab, BackTab, Up, Down). Looked up under
///   `settings/_compose_form`.
/// - **Target**: focus-kind-specific keys the leaf claims. The leaf
///   scope is `settings/_compose_field_text` when the focused field is
///   a Text variant (Name / Endpoint / Key) or
///   `settings/_compose_field_selector` when it's a Selector variant
///   (Protocol / Auth). The dispatcher picks the leaf scope by reading
///   `ui/settings/new_account/focused_field` and consulting
///   `field_kind`.
/// - **Bubble**: keys the leaf didn't claim, caught by the form
///   (Enter). Looked up under `settings/_compose_form` again.
///
/// `BindingEntry` has no `phase` field today; phase classification is
/// compose-pass-local rather than registry-wide. Generalizing into the
/// `BindingEntry` shape is convergence work tracked separately.
fn lookup_compose<'a>(
    bindings: &'a BindingRegistry,
    screen: Screen,
    mode: Option<Mode>,
    key: &KeyChord,
    snapshot: &mut dyn Reader,
) -> Option<&'a CommandId> {
    let form = ox_path::oxpath!("settings", "_compose_form");
    let field_text = ox_path::oxpath!("settings", "_compose_field_text");
    let field_selector = ox_path::oxpath!("settings", "_compose_field_selector");
    let leaf = match field_kind(read_focused_compose_field(snapshot)) {
        FieldKind::Text => &field_text,
        FieldKind::Selector => &field_selector,
    };
    // Phase 1 — Capture: lifecycle keys queried on the form scope only.
    if is_capture_key(&key.code) {
        if let Some(hit) = bindings.lookup(screen, &form, mode, key) {
            return Some(hit);
        }
    }
    // Phase 2 — Target: leaf scope only, for any non-capture key.
    if !is_capture_key(&key.code) {
        if let Some(hit) = bindings.lookup(screen, leaf, mode, key) {
            return Some(hit);
        }
    }
    // Phase 3 — Bubble: bubble keys queried on the form scope, only if
    // the leaf didn't claim them.
    if is_bubble_key(&key.code) {
        if let Some(hit) = bindings.lookup(screen, &form, mode, key) {
            return Some(hit);
        }
    }
    None
}

/// Capture-phase keys: lifecycle controls the form owns regardless of
/// which field is focused.
fn is_capture_key(code: &KeyCodeRepr) -> bool {
    matches!(
        code,
        KeyCodeRepr::Esc
            | KeyCodeRepr::Tab
            | KeyCodeRepr::BackTab
            | KeyCodeRepr::Up
            | KeyCodeRepr::Down,
    )
}

/// Bubble-phase keys: keys the form claims only after the leaf has had
/// a chance to consume them. Day-one this is just Enter (compose
/// commit). A future multiline text leaf could bind Enter at target
/// (newline insert), in which case the leaf lookup runs first and
/// shadows this phase.
fn is_bubble_key(code: &KeyCodeRepr) -> bool {
    matches!(code, KeyCodeRepr::Enter)
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
/// stored value is `Bool(true)`; any other shape (missing, wrong
/// type, `false`) reads as inactive. Compose state lives in the
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

/// Read `ui/settings/pending_delete`. Returns `Some(_)` when the
/// user is being asked to confirm a delete (pending-delete mode).
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

/// Discriminate manual-model mode by attempting a typed deserialize at
/// `ui/settings/manual_model/stage`. Returns `true` only when the
/// stored value matches the new typed `ManualModelStage` shape (wire
/// format `"Id"` / `"Ctx"` / `"Out"`).
///
/// The legacy stringly-typed write site stores `"id"` / `"ctx"` /
/// `"out"` (snake_case strings); those fail the typed deserialize, so
/// this returns `false` and the dispatcher falls through to the
/// compose / edit-mode passes that the old flow already routes
/// through. That coexistence is what keeps the new pass dormant until
/// the entry point is rewired.
fn read_manual_model_active(snapshot: &mut dyn Reader) -> bool {
    use ox_path::oxpath;
    use ox_types::settings::ManualModelStage;
    let record = match snapshot
        .read(&oxpath!("ui", "settings", "manual_model", "stage"))
        .ok()
        .flatten()
    {
        Some(r) => r,
        None => return false,
    };
    let value = match record.as_value() {
        Some(v) => v.clone(),
        None => return false,
    };
    structfs_serde_store::from_value::<ManualModelStage>(value).is_ok()
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
}
