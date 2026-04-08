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

    // Mount UiStore
    servers.push(broker.mount(path!("ui"), UiStore::new()).await);

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

    BrokerHandle {
        broker,
        _servers: servers,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use structfs_core_store::{Record, Value, path};

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
            "gate/model".to_string(),
            Value::String("claude-sonnet-4-20250514".into()),
        );
        config.insert(
            "gate/provider".to_string(),
            Value::String("anthropic".into()),
        );
        config.insert("gate/max_tokens".to_string(), Value::Integer(4096));
        config.insert("gate/api_key".to_string(), Value::String("test-key".into()));
        setup(test_inbox(), bindings, test_inbox_root(), config).await
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
        let handle = test_setup().await;
        let client = handle.client();

        // Set row count so selection can advance
        let mut count_cmd = BTreeMap::new();
        count_cmd.insert("count".to_string(), Value::Integer(5));
        client
            .write(
                &path!("ui/set_row_count"),
                Record::parsed(Value::Map(count_cmd)),
            )
            .await
            .unwrap();

        // Dispatch "j" on inbox screen
        let mut event = BTreeMap::new();
        event.insert("mode".to_string(), Value::String("normal".to_string()));
        event.insert("key".to_string(), Value::String("j".to_string()));
        event.insert("screen".to_string(), Value::String("inbox".to_string()));
        client
            .write(&path!("input/key"), Record::parsed(Value::Map(event)))
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
        let handle = test_setup().await;
        let client = handle.client();

        // Open a thread so we're on the thread screen
        let mut open_cmd = BTreeMap::new();
        open_cmd.insert("thread_id".to_string(), Value::String("t_test".to_string()));
        client
            .write(&path!("ui/open"), Record::parsed(Value::Map(open_cmd)))
            .await
            .unwrap();

        // Set row count for inbox selection
        let mut count_cmd = BTreeMap::new();
        count_cmd.insert("count".to_string(), Value::Integer(5));
        client
            .write(
                &path!("ui/set_row_count"),
                Record::parsed(Value::Map(count_cmd)),
            )
            .await
            .unwrap();

        // Dispatch "j" on thread screen — should scroll, NOT select
        let mut event = BTreeMap::new();
        event.insert("mode".to_string(), Value::String("normal".to_string()));
        event.insert("key".to_string(), Value::String("j".to_string()));
        event.insert("screen".to_string(), Value::String("thread".to_string()));
        client
            .write(&path!("input/key"), Record::parsed(Value::Map(event)))
            .await
            .unwrap();

        // selected_row should NOT have changed (still 0)
        let row = client
            .read(&path!("ui/selected_row"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.as_value().unwrap(), &Value::Integer(0));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn thread_model_reads_from_gate_store() {
        let handle = test_setup().await;
        let client = handle.client();

        // Read model for a thread — uses GateStore default
        let model = client
            .read(&path!("threads/t_test/gate/model"))
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

        // GateStore config handle reads gate/model from ConfigStore
        let model = client
            .read(&path!("threads/t_cfg/gate/model"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            model.as_value().unwrap(),
            &Value::String("claude-sonnet-4-20250514".into())
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn config_store_mounted_with_defaults() {
        let handle = test_setup().await;
        let client = handle.client();

        let model = client
            .read(&path!("config/gate/model"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            model.as_value().unwrap(),
            &Value::String("claude-sonnet-4-20250514".into())
        );

        let provider = client
            .read(&path!("config/gate/provider"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            provider.as_value().unwrap(),
            &Value::String("anthropic".into())
        );
    }
}
