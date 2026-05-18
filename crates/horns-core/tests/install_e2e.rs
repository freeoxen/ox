//! End-to-end integration test: a key written to the broker triggers
//! a horns `KeyDispatchSubscription`, which dispatches a command that
//! writes to a witness path. Validates the install API actually wires
//! a working horns instance onto a real broker.
//!
//! Requires the multi-thread tokio runtime because subscription
//! handlers use `block_in_place` to read from the broker (the broker's
//! sync `Reader` adapter under `tokio::task::block_in_place`).

use std::collections::HashMap;
use std::time::Duration;

use horns_core::install::{InstallOptions, build_install_bundle};
use horns_core::{
    BindingEntry, BindingId, BindingScope, Command, CommandCtx, CommandDisplay, CommandId,
    CommandScope, KeyChord, KeyCodeRepr, KeyModifierSet, Phase, Write,
};
use ox_broker::BrokerStore;
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};

fn p(s: &str) -> Path {
    Path::parse(s).expect("path parse")
}

/// Simple in-memory store used to back the broker prefixes for the test.
struct MemoryStore {
    data: std::collections::BTreeMap<String, Value>,
}

impl MemoryStore {
    fn new() -> Self {
        Self {
            data: std::collections::BTreeMap::new(),
        }
    }
}

impl Reader for MemoryStore {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        Ok(self
            .data
            .get(&from.to_string())
            .map(|v| Record::parsed(v.clone())))
    }
}

impl Writer for MemoryStore {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        if let Some(value) = data.as_value() {
            self.data.insert(to.to_string(), value.clone());
        }
        Ok(to.clone())
    }
}

/// Test command that writes "ran" to a fixed witness path.
struct WriteWitness {
    id: CommandId,
    display: CommandDisplay,
    scope: CommandScope,
    witness_path: Path,
}

impl Command for WriteWitness {
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
            path: self.witness_path.clone(),
            record: Record::parsed(Value::String("ran".into())),
        }]
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn key_write_triggers_command_dispatch_through_broker() {
    let broker = BrokerStore::new(Duration::from_secs(5));

    // Mount memory stores for the prefixes the test touches.
    let _ui = broker.mount(p("ui"), MemoryStore::new()).await;
    let _horns = broker.mount(p("horns"), MemoryStore::new()).await;

    let client = broker.client();

    // Seed cursor first so the dispatch path has a scope to walk.
    // Cursor encoding: Value::Array of Value::String segments
    // (matches the read_cursor helper in install.rs).
    client
        .write(
            &p("ui/test/focused"),
            Record::parsed(Value::Array(vec![Value::String("test".into())])),
        )
        .await
        .expect("seed cursor");

    // Build the install bundle.
    let witness_path = p("ui/test/witness");
    let cmd_id = CommandId("test_witness_cmd".into());

    let mut commands: HashMap<CommandId, Box<dyn Command>> = HashMap::new();
    commands.insert(
        cmd_id.clone(),
        Box::new(WriteWitness {
            id: cmd_id.clone(),
            display: CommandDisplay {
                name: "Witness".into(),
                description: "writes witness".into(),
            },
            scope: CommandScope { cursor_path: None },
            witness_path: witness_path.clone(),
        }),
    );

    let chord = KeyChord {
        modifiers: KeyModifierSet::default(),
        code: KeyCodeRepr::Char('z'),
    };

    let opts = InstallOptions {
        cursor_path: p("ui/test/focused"),
        input_path: p("ui/test/input"),
        render_tick_path: p("ui/test/render/tick"),
        render_output_path: p("ui/test/render/output"),
        bindings_prefix: p("horns/test/bindings"),
        commands_prefix: p("horns/test/commands"),
        renderers_prefix: p("horns/test/renderers"),
        handlers_prefix: p("horns/test/handlers"),
        theme_path: p("ui/test/theme"),
        commands,
        renderers: HashMap::new(),
        handlers: HashMap::new(),
        bindings: vec![(
            BindingId("test_z".into()),
            BindingEntry {
                scope: BindingScope::Anywhere,
                key: chord.clone(),
                phase: Phase::Target,
                command_id: cmd_id.clone(),
            },
        )],
        handler_metadata: vec![],
        theme: serde_json::json!({}),
    };

    let bundle = build_install_bundle(opts);

    // Apply metadata writes (skip ones outside the mounted prefixes).
    for (path, record) in bundle.metadata_writes {
        let _ = client.write(&path, record).await;
    }

    // Register subscriptions on the broker.
    for sub in bundle.subscriptions {
        broker.register_subscription(sub);
    }

    // Write the key chord — this should fire the KeyDispatchSubscription,
    // which dispatches against the registered binding and runs the command.
    client
        .write_typed(&p("ui/test/input/key"), &chord)
        .await
        .expect("write key chord");

    // The dispatcher returned its write through the subscription mechanism;
    // the broker's dispatcher should have applied it. Read the witness.
    let witness = client.read(&witness_path).await.expect("read witness");
    assert!(
        witness.is_some(),
        "witness path should have been written by the dispatched command",
    );
    match witness.unwrap().as_value() {
        Some(Value::String(s)) => assert_eq!(s, "ran"),
        other => panic!("unexpected witness value: {other:?}"),
    }
}
