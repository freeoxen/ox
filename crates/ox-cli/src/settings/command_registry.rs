//! Settings command registry.
//!
//! A command is a pure function from snapshot to writes. Built-in
//! commands are Rust structs implementing `Command`; the registry stores
//! trait objects keyed by `CommandId`.
//!
//! Per spec §4.4: a command's per-invocation inputs are *only* the
//! snapshot Reader and a narrow `CommandCtx` of non-data services.
//! Ambient configuration (e.g. clock, RNG) is closed-over at command
//! construction, not threaded through `CommandCtx`. Growing `CommandCtx`
//! is therefore a deliberate language extension.

use std::collections::HashMap;

use structfs_core_store::Reader;

use ox_types::subscription::Write;
use ox_types::{CommandDisplay, CommandId, CommandScope, KeyChord};

use super::registry::RendererRegistry;

/// A built-in command: pure function from snapshot to writes.
///
/// **Mutability deviation from spec:** the spec quotes the signature as
/// `run(&self, snapshot: &dyn Reader, ...)` (immutable Reader), but
/// `Reader::read(&mut self, ...)` requires a mutable receiver. We therefore
/// take `&mut dyn Reader`. This matches the same deviation in
/// `RenderCtx::data` (Phase G); commands remain pure with respect to
/// observable application state — the mutation is internal to the
/// Reader (lazy-decode caches in `LiveReader`/`LocalConfig`).
pub trait Command: Send + Sync {
    fn id(&self) -> &CommandId;
    fn display(&self) -> &CommandDisplay;
    fn scope(&self) -> &CommandScope;
    fn run(&self, snapshot: &mut dyn Reader, ctx: &CommandCtx<'_>) -> Vec<Write>;
}

/// Non-data services a command may legitimately need at invocation
/// time. Per spec §4.4 this is intentionally narrow — per-invocation
/// non-data inputs only. Data flows through the snapshot `Reader`;
/// ambient services are closed-over at construction. Growing this
/// struct is a deliberate language extension.
pub struct CommandCtx<'a> {
    pub registry: &'a RendererRegistry,
    pub last_keystroke: Option<KeyChord>,
}

/// Indexes commands by `CommandId`.
pub struct CommandRegistry {
    by_id: HashMap<CommandId, Box<dyn Command>>,
}

impl CommandRegistry {
    pub fn new() -> Self {
        Self {
            by_id: HashMap::new(),
        }
    }

    /// Register a command. Replaces any existing entry with the same id.
    pub fn register(&mut self, command: Box<dyn Command>) {
        let id = command.id().clone();
        self.by_id.insert(id, command);
    }

    /// Look up a command by id. Returns `None` if not registered.
    pub fn lookup(&self, id: &CommandId) -> Option<&dyn Command> {
        self.by_id.get(id).map(|b| b.as_ref())
    }

    /// Iterate over all registered commands. Order is unspecified.
    pub fn iter(&self) -> impl Iterator<Item = &dyn Command> {
        self.by_id.values().map(|b| b.as_ref())
    }
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use ox_path::oxpath;
    use ox_store_util::local_config::LocalConfig;
    use ox_types::Screen;
    use ox_types::key_chord::{KeyCodeRepr, KeyModifierSet};
    use structfs_core_store::{Record, Value};

    /// Command that writes a fixed string to a fixed path. Verifies the
    /// minimal Command surface (id/display/scope/run).
    struct WriteOnePath {
        id: CommandId,
        display: CommandDisplay,
        scope: CommandScope,
    }

    impl WriteOnePath {
        fn new() -> Self {
            Self {
                id: CommandId("test.write_one".to_string()),
                display: CommandDisplay {
                    name: "Write One".to_string(),
                    description: "writes 'hello' to ui/x".to_string(),
                },
                scope: CommandScope { cursor_path: None },
            }
        }
    }

    impl Command for WriteOnePath {
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
                path: oxpath!("ui", "x"),
                record: Record::parsed(Value::String("hello".into())),
            }]
        }
    }

    /// Command that consults `ctx.registry` and writes whether a known
    /// cursor was registered. Verifies the registry field is wired
    /// through `CommandCtx`.
    struct WriteRegistryProbe {
        id: CommandId,
        display: CommandDisplay,
        scope: CommandScope,
    }

    impl WriteRegistryProbe {
        fn new() -> Self {
            Self {
                id: CommandId("test.registry_probe".to_string()),
                display: CommandDisplay {
                    name: "Registry Probe".to_string(),
                    description: "writes ctx.registry.lookup result".to_string(),
                },
                scope: CommandScope { cursor_path: None },
            }
        }
    }

    impl Command for WriteRegistryProbe {
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
            // The empty registry must miss for any cursor; that's the
            // observable signal we're verifying — that the command can
            // *call* the registry through ctx.
            let hit = ctx.registry.lookup(&oxpath!("settings", "ghost")).is_some();
            vec![Write {
                path: oxpath!("ui", "registry_hit"),
                record: Record::parsed(Value::Bool(hit)),
            }]
        }
    }

    /// Command that mirrors `ctx.last_keystroke.is_some()` to a path.
    /// Verifies the `last_keystroke` field is wired through.
    struct WriteIfKeystroke {
        id: CommandId,
        display: CommandDisplay,
        scope: CommandScope,
    }

    impl WriteIfKeystroke {
        fn new() -> Self {
            Self {
                id: CommandId("test.write_if_keystroke".to_string()),
                display: CommandDisplay {
                    name: "Write If Keystroke".to_string(),
                    description: "writes whether ctx.last_keystroke was set".to_string(),
                },
                scope: CommandScope { cursor_path: None },
            }
        }
    }

    impl Command for WriteIfKeystroke {
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
            vec![Write {
                path: oxpath!("ui", "seen_key"),
                record: Record::parsed(Value::Bool(ctx.last_keystroke.is_some())),
            }]
        }
    }

    fn sample_chord() -> KeyChord {
        KeyChord {
            modifiers: KeyModifierSet::default(),
            code: KeyCodeRepr::Char('a'),
        }
    }

    #[test]
    fn trivial_command_registers_and_looks_up() {
        let mut reg = CommandRegistry::new();
        reg.register(Box::new(WriteOnePath::new()));

        let id = CommandId("test.write_one".to_string());
        let cmd = reg.lookup(&id).expect("command should be registered");

        let renderers = RendererRegistry::new();
        let ctx = CommandCtx {
            registry: &renderers,
            last_keystroke: None,
        };
        let mut reader = LocalConfig::default();
        let writes = cmd.run(&mut reader, &ctx);

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "x"));
        match &writes[0].record {
            Record::Parsed(Value::String(s)) => assert_eq!(s, "hello"),
            other => panic!("unexpected record: {other:?}"),
        }
    }

    #[test]
    fn command_can_read_ctx_registry() {
        let mut reg = CommandRegistry::new();
        reg.register(Box::new(WriteRegistryProbe::new()));

        let id = CommandId("test.registry_probe".to_string());
        let cmd = reg.lookup(&id).expect("command should be registered");

        let renderers = RendererRegistry::new();
        let ctx = CommandCtx {
            registry: &renderers,
            last_keystroke: None,
        };
        let mut reader = LocalConfig::default();
        let writes = cmd.run(&mut reader, &ctx);

        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].path, oxpath!("ui", "registry_hit"));
        // Empty renderer registry must miss → the command observed `false`.
        match &writes[0].record {
            Record::Parsed(Value::Bool(b)) => assert!(!*b),
            other => panic!("unexpected record: {other:?}"),
        }
    }

    #[test]
    fn command_handles_optional_last_keystroke() {
        let mut reg = CommandRegistry::new();
        reg.register(Box::new(WriteIfKeystroke::new()));

        let id = CommandId("test.write_if_keystroke".to_string());
        let cmd = reg.lookup(&id).expect("command should be registered");

        let renderers = RendererRegistry::new();
        let mut reader = LocalConfig::default();

        // Some(...) — the command observes `true`.
        let ctx_some = CommandCtx {
            registry: &renderers,
            last_keystroke: Some(sample_chord()),
        };
        let writes = cmd.run(&mut reader, &ctx_some);
        assert_eq!(writes.len(), 1);
        match &writes[0].record {
            Record::Parsed(Value::Bool(b)) => assert!(*b),
            other => panic!("unexpected record: {other:?}"),
        }

        // None — the command observes `false`.
        let ctx_none = CommandCtx {
            registry: &renderers,
            last_keystroke: None,
        };
        let writes = cmd.run(&mut reader, &ctx_none);
        assert_eq!(writes.len(), 1);
        match &writes[0].record {
            Record::Parsed(Value::Bool(b)) => assert!(!*b),
            other => panic!("unexpected record: {other:?}"),
        }
    }

    #[test]
    fn register_replaces_existing_entry() {
        let mut reg = CommandRegistry::new();
        reg.register(Box::new(WriteOnePath::new()));
        reg.register(Box::new(WriteOnePath::new()));

        // Still exactly one entry — the second registration replaced the first.
        assert_eq!(reg.iter().count(), 1);
    }

    #[test]
    fn lookup_misses_return_none() {
        let reg = CommandRegistry::new();
        assert!(reg.lookup(&CommandId("missing".to_string())).is_none());
    }

    #[test]
    fn iter_yields_all_registered() {
        let mut reg = CommandRegistry::new();
        reg.register(Box::new(WriteOnePath::new()));
        reg.register(Box::new(WriteRegistryProbe::new()));
        reg.register(Box::new(WriteIfKeystroke::new()));

        assert_eq!(reg.iter().count(), 3);
    }
}
