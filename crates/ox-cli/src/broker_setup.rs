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
    provider: String,
    model: String,
    max_tokens: u32,
    api_key: String,
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

    // Mount ConfigStore with base defaults + runtime overrides from CLI args
    {
        use ox_ui::ConfigStore;
        use structfs_core_store::{Record, Value, Writer};

        let mut base = std::collections::BTreeMap::new();
        base.insert(
            "model/id".to_string(),
            Value::String("claude-sonnet-4-20250514".into()),
        );
        base.insert(
            "gate/provider".to_string(),
            Value::String("anthropic".into()),
        );
        base.insert("model/max_tokens".to_string(), Value::Integer(4096));

        let mut config = ConfigStore::new(base);
        config
            .write(
                &path!("gate/provider"),
                Record::parsed(Value::String(provider)),
            )
            .ok();
        config
            .write(&path!("model/id"), Record::parsed(Value::String(model)))
            .ok();
        config
            .write(
                &path!("model/max_tokens"),
                Record::parsed(Value::Integer(max_tokens as i64)),
            )
            .ok();
        config
            .write(
                &path!("gate/api_key"),
                Record::parsed(Value::String(api_key)),
            )
            .ok();

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
        setup(
            test_inbox(),
            bindings,
            test_inbox_root(),
            "anthropic".into(),
            "claude-sonnet-4-20250514".into(),
            4096,
            "test-key".into(),
        )
        .await
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
    async fn thread_model_resolves_through_config() {
        let handle = test_setup().await;
        let client = handle.client();

        // Read model for a thread — should fall through to global config
        let model = client
            .read(&path!("threads/t_test/model/id"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            model.as_value().unwrap(),
            &Value::String("claude-sonnet-4-20250514".into())
        );

        // Change global model
        client
            .write(
                &path!("config/model/id"),
                Record::parsed(Value::String("gpt-4o".into())),
            )
            .await
            .unwrap();

        // Thread should now see the new global model
        let model = client
            .read(&path!("threads/t_test/model/id"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(model.as_value().unwrap(), &Value::String("gpt-4o".into()));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn config_store_mounted_with_defaults() {
        let handle = test_setup().await;
        let client = handle.client();

        let model = client
            .read(&path!("config/model/id"))
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
