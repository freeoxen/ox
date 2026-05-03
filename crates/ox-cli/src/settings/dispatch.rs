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

use ox_types::subscription::Write;
use ox_types::{KeyChord, Mode, Screen};

use super::binding_registry::BindingRegistry;
use super::command_registry::{CommandCtx, CommandRegistry};
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
    // Two-pass binding lookup: try the focused-row scope first
    // (so per-row `Prefix(settings/{accounts,models})` bindings
    // fire when the user has focused a row), then fall back to the
    // page-level cursor (`settings/index`, `_detail`, etc.). The
    // accordion's tree commands live at `Exact(settings/index)` and
    // catch the second pass; per-row commands live at `Prefix(...)`
    // matching descendant focused-row paths and catch the first.
    //
    // A miss in pass one followed by a hit in pass two is the common
    // case for tree navigation (j/k/Enter/Esc); a hit in pass one is
    // the per-row action case (t/r/P/d on a focused leaf).
    let cmd_id = read_focused_row(snapshot)
        .as_ref()
        .and_then(|focus| bindings.lookup(screen, focus, mode, key))
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

/// Read `ui/settings/focused_row` from the dispatch snapshot. Used as
/// the first-pass binding-scope cursor so per-row bindings can fire
/// while the page cursor sits at `settings/index`.
fn read_focused_row(snapshot: &mut dyn Reader) -> Option<Path> {
    use ox_path::oxpath;
    let record = snapshot
        .read(&oxpath!("ui", "settings", "focused_row"))
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
    use structfs_core_store::{Record, Value};

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
}
