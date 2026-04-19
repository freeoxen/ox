//! BrokerSetup — create the BrokerStore and mount all stores.
//!
//! This is the single point where the store namespace is assembled.
//! The TUI event loop and agent workers interact through client handles.

use ox_broker::BrokerStore;
use ox_inbox::InboxStore;
use ox_ui::{Binding, InputStore, UiStore};
use structfs_core_store::path;
use tokio::task::JoinHandle;

/// Handles returned from broker setup.
pub struct BrokerHandle {
    pub broker: BrokerStore,
    _servers: Vec<JoinHandle<()>>,
}

impl BrokerHandle {
    pub fn client(&self) -> ox_broker::ClientHandle {
        self.broker.client()
    }
}

/// Create and wire the BrokerStore with all stores mounted.
///
/// Mounts:
/// - `ui/` → UiStore (in-memory state machine)
/// - `input/` → InputStore (key binding translation)
/// - `inbox/` → InboxStore (SQLite-backed thread index)
/// - `threads/` → ThreadRegistry (lazy per-thread store lifecycle)
pub async fn setup(
    inbox: InboxStore,
    bindings: Vec<Binding>,
    inbox_root: std::path::PathBuf,
    config_values: std::collections::BTreeMap<String, structfs_core_store::Value>,
) -> BrokerHandle {
    let broker = BrokerStore::default();
    let mut servers = Vec::new();

    // Mount UiStore with broker-connected command-line dispatcher so
    // the embedded CommandLineStore can forward submit writes to
    // command/exec.
    //
    // The dispatch is fire-and-forget on purpose: the command-line store
    // is composed inside UiStore, so its submit handler runs on UiStore's
    // server task. If we awaited the downstream write here, any command
    // that ultimately routes back to `ui/*` would deadlock against the
    // server that's still handling submit. Spawning decouples them; the
    // submit write returns Ok immediately, and the resolved command
    // reaches its target store on the next scheduling tick.
    {
        let cmdline_dispatch_client = broker.client();
        let mut ui = UiStore::new();
        ui.set_command_line_dispatcher(Box::new(move |target, data| {
            let client = cmdline_dispatch_client.clone();
            let t = target.clone();
            tokio::spawn(async move {
                if let Err(e) = client.write(&t, data).await {
                    tracing::warn!(target = %t, error = %e, "command-line dispatch failed");
                }
            });
            Ok(target.clone())
        }));
        servers.push(broker.mount(path!("ui"), ui).await);
    }

    // Mount CommandStore with broker-connected dispatcher
    {
        let command_dispatch_client = broker.client();
        let mut command_store = ox_ui::CommandStore::from_builtins();
        command_store.set_dispatcher(Box::new(move |target, data| {
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current()
                    .block_on(command_dispatch_client.write(target, data))
            })
        }));
        servers.push(broker.mount(path!("command"), command_store).await);
    }

    // Mount InputStore with broker-connected dispatcher
    let dispatch_client = broker.client();
    let mut input = InputStore::new(bindings);
    input.set_dispatcher(Box::new(move |target, data| {
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(dispatch_client.write(target, data))
        })
    }));
    servers.push(broker.mount(path!("input"), input).await);

    // Mount InboxStore
    servers.push(broker.mount(path!("inbox"), inbox).await);

    // Mount ConfigStore with figment-resolved values + TOML file backing
    {
        let toml_path = inbox_root.join("config.toml");
        let backing = crate::toml_backing::TomlFileBacking::new(toml_path);
        let config = ox_ui::ConfigStore::with_backing(config_values, Box::new(backing));

        servers.push(broker.mount(path!("config"), config).await);
    }

    // Mount ThreadRegistry at threads/ — lazy-mounts per-thread stores from disk
    let mut registry = crate::thread_registry::ThreadRegistry::new(inbox_root);
    registry.set_broker_client(broker.client());
    servers.push(broker.mount_async(path!("threads"), registry).await);

    tracing::info!(stores = servers.len(), "broker setup complete");

    BrokerHandle {
        broker,
        _servers: servers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use structfs_core_store::{Value, path};

    fn test_inbox() -> InboxStore {
        let dir = tempfile::tempdir().unwrap();
        InboxStore::open(dir.path()).unwrap()
    }

    #[allow(deprecated)]
    fn test_inbox_root() -> std::path::PathBuf {
        tempfile::tempdir().unwrap().into_path()
    }

    async fn test_setup() -> BrokerHandle {
        let bindings = crate::bindings::default_bindings();
        let mut config = BTreeMap::new();
        config.insert(
            "gate/defaults/model".to_string(),
            Value::String("claude-sonnet-4-20250514".into()),
        );
        config.insert(
            "gate/defaults/account".to_string(),
            Value::String("anthropic".into()),
        );
        config.insert("gate/defaults/max_tokens".to_string(), Value::Integer(4096));
        config.insert(
            "gate/accounts/anthropic/provider".to_string(),
            Value::String("anthropic".into()),
        );
        config.insert(
            "gate/accounts/anthropic/key".to_string(),
            Value::String("test-key".into()),
        );
        setup(test_inbox(), bindings, test_inbox_root(), config).await
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn command_store_is_mounted() {
        let handle = test_setup().await;
        let client = handle.client();

        let result = client.read(&path!("command/commands")).await.unwrap();
        assert!(result.is_some());
        match result.unwrap().as_value().unwrap() {
            Value::Array(arr) => assert!(!arr.is_empty()),
            _ => panic!("expected array"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn command_store_reads_single_command() {
        let handle = test_setup().await;
        let client = handle.client();

        let result = client.read(&path!("command/commands/quit")).await.unwrap();
        assert!(result.is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn broker_setup_mounts_all_stores() {
        let handle = test_setup().await;
        let client = handle.client();

        // UiStore is mounted — read initial state
        let screen = client.read(&path!("ui/screen")).await.unwrap().unwrap();
        assert_eq!(
            screen.as_value().unwrap(),
            &Value::String("inbox".to_string())
        );

        // InputStore is mounted — read bindings
        let bindings_val = client
            .read(&path!("input/bindings/normal"))
            .await
            .unwrap()
            .unwrap();
        match bindings_val.as_value().unwrap() {
            Value::Array(a) => assert!(!a.is_empty()),
            _ => panic!("expected array"),
        }

        // InboxStore is mounted — read threads (empty initially)
        let threads = client.read(&path!("inbox/threads")).await.unwrap();
        assert!(threads.is_some());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn key_dispatch_through_broker() {
        use ox_types::{InboxCommand, UiCommand};

        let handle = test_setup().await;
        let client = handle.client();

        // Set row count so selection can advance
        client
            .write_typed(
                &path!("ui"),
                &UiCommand::Inbox(InboxCommand::SetRowCount { count: 5 }),
            )
            .await
            .unwrap();

        // Dispatch "j" on inbox screen
        let event = ox_types::InputKeyEvent {
            mode: ox_types::Mode::Normal,
            key: "j".to_string(),
            screen: ox_types::Screen::Inbox,
        };
        client
            .write_typed(&path!("input/key"), &event)
            .await
            .unwrap();

        // Verify UiStore state changed
        let row = client
            .read(&path!("ui/selected_row"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.as_value().unwrap(), &Value::Integer(1));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn screen_specific_binding_routes_correctly() {
        use ox_types::{GlobalCommand, ThreadCommand, UiCommand};

        let handle = test_setup().await;
        let client = handle.client();

        // Open a thread so we're on the thread screen
        client
            .write_typed(
                &path!("ui"),
                &UiCommand::Global(GlobalCommand::Open {
                    thread_id: "t_test".to_string(),
                }),
            )
            .await
            .unwrap();

        // Give the thread some scroll headroom
        client
            .write_typed(
                &path!("ui"),
                &UiCommand::Thread(ThreadCommand::SetScrollMax { max: 100 }),
            )
            .await
            .unwrap();

        // Dispatch "j" on thread screen — should trigger scroll_down (thread-specific),
        // NOT select_next (inbox-specific)
        let event = ox_types::InputKeyEvent {
            mode: ox_types::Mode::Normal,
            key: "j".to_string(),
            screen: ox_types::Screen::Thread,
        };
        client
            .write_typed(&path!("input/key"), &event)
            .await
            .unwrap();

        // Verify we're on thread screen (not inbox)
        let screen = client.read(&path!("ui/screen")).await.unwrap().unwrap();
        assert_eq!(
            screen.as_value().unwrap(),
            &Value::String("thread".to_string())
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn thread_model_reads_from_gate_store() {
        let handle = test_setup().await;
        let client = handle.client();

        // Read model for a thread — uses GateStore default
        let model = client
            .read(&path!("threads/t_test/gate/defaults/model"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            model.as_value().unwrap(),
            &Value::String("claude-sonnet-4-20250514".into())
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn thread_gate_reads_api_key_from_config() {
        let handle = test_setup().await;
        let client = handle.client();

        // The thread's GateStore should read the API key from ConfigStore
        // via its config handle (bootstrap account = anthropic)
        let key = client
            .read(&path!("threads/t_gate/gate/accounts/anthropic/key"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(key.as_value().unwrap(), &Value::String("test-key".into()));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn thread_gate_reads_model_from_config() {
        let handle = test_setup().await;
        let client = handle.client();

        // GateStore config handle reads gate/defaults/model from ConfigStore
        let model = client
            .read(&path!("threads/t_cfg/gate/defaults/model"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            model.as_value().unwrap(),
            &Value::String("claude-sonnet-4-20250514".into())
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn command_line_open_submits_through_command_exec() {
        // Full pipeline: open the command line, type "quit", submit —
        // should route through command/exec to the quit target and land
        // as a ui/quit write observable in UiStore's pending_action.
        use ox_ui::text_input_store::{Edit, EditOp, EditSequence, EditSource};

        let handle = test_setup().await;
        let client = handle.client();

        // Open the command line
        client
            .write(
                &path!("ui/command_line/open"),
                structfs_core_store::Record::parsed(Value::Null),
            )
            .await
            .unwrap();

        // Verify open flag toggled
        let open = client
            .read(&path!("ui/command_line/open"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(open.as_value().unwrap(), &Value::Bool(true));

        // Type "quit" into the buffer
        let seq = EditSequence {
            edits: vec![Edit {
                op: EditOp::Insert {
                    text: "quit".into(),
                },
                at: 0,
                source: EditSource::Key,
                ts_ms: 0,
            }],
            generation: 0,
        };
        client
            .write_typed(&path!("ui/command_line/edit"), &seq)
            .await
            .unwrap();

        // Submit — dispatches quit via command/exec
        client
            .write(
                &path!("ui/command_line/submit"),
                structfs_core_store::Record::parsed(Value::Null),
            )
            .await
            .unwrap();

        // Post-submit: command line is closed and buffer cleared
        let open = client
            .read(&path!("ui/command_line/open"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(open.as_value().unwrap(), &Value::Bool(false));

        // quit set pending_action to Quit
        let pa = client
            .read(&path!("ui/pending_action"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pa.as_value().unwrap(), &Value::String("quit".into()));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn colon_binding_on_inbox_opens_command_line() {
        // The original bug: `:` didn't work on the inbox screen. This
        // test nails the regression: Normal mode on inbox, key ":".
        let handle = test_setup().await;
        let client = handle.client();

        let event = ox_types::InputKeyEvent {
            mode: ox_types::Mode::Normal,
            key: ":".to_string(),
            screen: ox_types::Screen::Inbox,
        };
        client
            .write_typed(&path!("input/key"), &event)
            .await
            .unwrap();

        let open = client
            .read(&path!("ui/command_line/open"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(open.as_value().unwrap(), &Value::Bool(true));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn config_store_mounted_with_defaults() {
        let handle = test_setup().await;
        let client = handle.client();

        let model = client
            .read(&path!("config/gate/defaults/model"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            model.as_value().unwrap(),
            &Value::String("claude-sonnet-4-20250514".into())
        );

        let account = client
            .read(&path!("config/gate/defaults/account"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            account.as_value().unwrap(),
            &Value::String("anthropic".into())
        );
    }
}
