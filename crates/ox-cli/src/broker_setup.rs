//! BrokerSetup — create the BrokerStore and mount all stores.
//!
//! This is the single point where the store namespace is assembled.
//! The TUI event loop and agent workers interact through client handles.

use ox_broker::BrokerStore;
use ox_inbox::InboxStore;
use ox_ui::{ApprovalStore, Binding, InputStore, UiStore};
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
pub async fn setup(inbox: InboxStore, bindings: Vec<Binding>) -> BrokerHandle {
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

    // Mount ApprovalStore (per-app for now; per-thread in C3c)
    servers.push(broker.mount(path!("approval"), ApprovalStore::new()).await);

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

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn broker_setup_mounts_all_stores() {
        let bindings = crate::bindings::default_bindings();
        let handle = setup(test_inbox(), bindings).await;
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
        let bindings = crate::bindings::default_bindings();
        let handle = setup(test_inbox(), bindings).await;
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
        let bindings = crate::bindings::default_bindings();
        let handle = setup(test_inbox(), bindings).await;
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
}
