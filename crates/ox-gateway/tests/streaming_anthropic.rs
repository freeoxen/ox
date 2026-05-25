//! End-to-end streaming test for POST /v1/messages.
//!
//! Sets up an in-memory broker + MockSseExecutor, starts axum on a
//! random port, and verifies that the response carries Anthropic-shaped
//! SSE frames matching the scripted event sequence.

use ox_broker::BrokerStore;
use ox_gate::completion_broker::CompletionBrokerStore;
use ox_gate::completion_broker::mock::MockSseExecutor;
use ox_path::oxpath;
use ox_store_util::StoreBacking;
use ox_types::StreamEvent;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use structfs_core_store::{Error as StoreError, Value, path};
use structfs_serde_store::to_value;

/// Minimal in-memory `StoreBacking` for the UsageStore in tests.
/// Supports append by accumulating items in a Vec.
struct MemoryBacking {
    items: Mutex<Vec<Value>>,
}

impl MemoryBacking {
    fn new() -> Self {
        Self {
            items: Mutex::new(Vec::new()),
        }
    }
}

impl StoreBacking for MemoryBacking {
    fn load(&self) -> Result<Option<Value>, StoreError> {
        let items = self.items.lock().unwrap();
        if items.is_empty() {
            Ok(None)
        } else {
            Ok(Some(Value::Array(items.clone())))
        }
    }

    fn save(&self, value: &Value) -> Result<(), StoreError> {
        let mut items = self.items.lock().unwrap();
        *items = match value {
            Value::Array(a) => a.clone(),
            other => vec![other.clone()],
        };
        Ok(())
    }

    fn append(&self, item: &Value) -> Result<(), StoreError> {
        self.items.lock().unwrap().push(item.clone());
        Ok(())
    }
}

/// Build an in-memory broker seeded with gate + secret mounts, then mount
/// UsageStore and CompletionBrokerStore. Returns the ready-to-serve broker.
async fn build_test_broker(executor: Arc<MockSseExecutor>) -> BrokerStore {
    use ox_gate::{AccountConfig, ApiKey, ProviderConfig};
    use ox_store_util::LocalConfig;
    use ox_types::CompletionRole;

    let broker = BrokerStore::new(Duration::from_secs(5));

    // Root mount: gate/accounts, gate/providers (no gate/completions needed
    // because the test POSTs a slash-form model string that dispatches directly).
    let mut gate_config = LocalConfig::new();
    gate_config.set(
        "gate/completions/primary",
        to_value(&CompletionRole {
            account: "anthropic".into(),
            model_id: "claude-sonnet-4-20250514".into(),
        })
        .unwrap(),
    );
    gate_config.set(
        "gate/accounts/anthropic",
        to_value(&AccountConfig {
            provider: "anthropic".into(),
            ..Default::default()
        })
        .unwrap(),
    );
    gate_config.set(
        "gate/providers/anthropic",
        to_value(&ProviderConfig::anthropic()).unwrap(),
    );
    broker.mount(path!(""), gate_config).await;

    // Secret mount at "secret/": dispatch resolves secret/keys/{account}.
    let mut secret = LocalConfig::new();
    secret.set("keys/anthropic", to_value(&ApiKey::new("sk-test")).unwrap());
    broker.mount(oxpath!("secret"), secret).await;

    // UsageStore at gateway/usage.
    let usage_backing = Box::new(MemoryBacking::new());
    let usage_store = ox_gate::UsageStore::new(usage_backing);
    broker.mount(oxpath!("gateway", "usage"), usage_store).await;

    // CompletionBrokerStore at gateway/completions.
    let client = broker.client();
    let usage_writer = client.scoped("gateway/usage");
    let store = CompletionBrokerStore::new(
        client,
        executor,
        usage_writer,
        tokio::runtime::Handle::current(),
    );
    broker
        .mount_async(oxpath!("gateway", "completions"), store)
        .await;

    broker
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_anthropic_messages_endpoint() {
    let executor = Arc::new(MockSseExecutor::new());
    executor.push_immediate(StreamEvent::InputUsage {
        input_tokens: 10,
        cache_creation: 0,
        cache_read: 0,
    });
    executor.push_immediate(StreamEvent::TextDelta { text: "Hello".into() });
    executor.push_immediate(StreamEvent::OutputUsage { output_tokens: 1 });
    executor.push_immediate(StreamEvent::MessageStop);

    let broker = build_test_broker(executor).await;
    let app = ox_gateway::routes::build_router(broker.client());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{}/v1/messages", addr))
        .json(&serde_json::json!({
            "model": "anthropic/claude-sonnet-4-20250514",
            "max_tokens": 100,
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();

    assert!(resp.status().is_success(), "got status {}", resp.status());

    let body = resp.text().await.unwrap();
    assert!(
        body.contains("event: message_start"),
        "missing message_start in body:\n{body}"
    );
    assert!(
        body.contains("event: content_block_start"),
        "missing content_block_start in body:\n{body}"
    );
    assert!(
        body.contains("\"text\":\"Hello\""),
        "missing text content in body:\n{body}"
    );
    assert!(
        body.contains("event: message_stop"),
        "missing message_stop in body:\n{body}"
    );
}
