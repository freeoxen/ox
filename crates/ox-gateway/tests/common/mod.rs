//! Shared test fixture for ox-gateway integration tests.
//!
//! Builds an in-memory broker preloaded with one account (provider controlled
//! via the `provider_dialect` arg) plus an in-memory UsageStore. Returns the
//! broker; the caller stands up axum and the reqwest client.

use ox_broker::BrokerStore;
use ox_gate::completion_broker::CompletionBrokerStore;
use ox_path::oxpath;
use ox_store_util::StoreBacking;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use structfs_core_store::{Error as StoreError, Value, path};
use structfs_serde_store::to_value;

/// Minimal in-memory `StoreBacking` for the UsageStore in tests.
/// Supports append by accumulating items in a Vec.
pub struct MemoryBacking {
    items: Mutex<Vec<Value>>,
}

impl MemoryBacking {
    pub fn new() -> Self {
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

/// Build a broker preloaded with gate config, secrets, UsageStore, and
/// CompletionBrokerStore for the given `provider_dialect` ("anthropic" or
/// "openai"). The executor is mounted at `gateway/completions`.
pub async fn build_test_broker(
    executor: Arc<ox_gate::completion_broker::mock::MockSseExecutor>,
    provider_dialect: &str,
) -> BrokerStore {
    use ox_gate::{AccountConfig, ApiKey, ProviderConfig};
    use ox_store_util::LocalConfig;
    use ox_types::CompletionRole;

    let broker = BrokerStore::new(Duration::from_secs(5));

    let default_model = match provider_dialect {
        "openai" => "gpt-4o",
        _ => "claude-sonnet-4-20250514",
    };

    let mut gate_config = LocalConfig::new();
    gate_config.set(
        "gate/completions/primary",
        to_value(&CompletionRole {
            account: provider_dialect.into(),
            model_id: default_model.into(),
        })
        .unwrap(),
    );
    gate_config.set(
        &format!("gate/accounts/{}", provider_dialect),
        to_value(&AccountConfig {
            provider: provider_dialect.into(),
            ..Default::default()
        })
        .unwrap(),
    );
    let provider = match provider_dialect {
        "openai" => ProviderConfig::openai(),
        _ => ProviderConfig::anthropic(),
    };
    gate_config.set(
        &format!("gate/providers/{}", provider_dialect),
        to_value(&provider).unwrap(),
    );
    broker.mount(path!(""), gate_config).await;

    let mut secret = LocalConfig::new();
    secret.set(
        &format!("keys/{}", provider_dialect),
        to_value(&ApiKey::new("sk-test")).unwrap(),
    );
    broker.mount(oxpath!("secret"), secret).await;

    let usage_backing = Box::new(MemoryBacking::new());
    let usage_store = ox_gate::UsageStore::new(usage_backing);
    broker.mount(oxpath!("gateway", "usage"), usage_store).await;

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
