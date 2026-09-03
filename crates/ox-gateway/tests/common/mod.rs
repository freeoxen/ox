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
use structfs_core_store::{Error as StoreError, Record, Value, path};
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
#[allow(dead_code)]
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

    let upstream = ox_gate::UpstreamStore::new(executor, tokio::runtime::Handle::current());
    broker.mount_async(oxpath!("upstream"), upstream).await;
    install_blocks(&broker, false).await;

    broker
}

/// Mount the completion substrate and wire store with their Block runners,
/// namespaced by the embedded assembly manifest — the same stack main.rs
/// assembles. Expects gate/secret/usage/upstream (and traffic when
/// `traffic` is set) to be mounted already.
#[allow(dead_code)]
pub async fn install_blocks(broker: &BrokerStore, traffic: bool) {
    let manifest = ox_gateway::assembly::Manifest::embedded().unwrap();
    let bindings = ox_gateway::assembly::standard_bindings();
    let broker_wiring = manifest.wiring_for("broker", &bindings).unwrap();
    let wire_wiring = manifest.wiring_for("wire", &bindings).unwrap();
    let stats_wiring = manifest.wiring_for("stats", &bindings).unwrap();

    let client = broker.client();
    let runtime = tokio::runtime::Handle::current();
    let store = CompletionBrokerStore::new(
        runtime.clone(),
        Arc::new(move |id, cancel| {
            if let Err(e) = ox_gateway::broker_block::run_broker(
                format!("gateway/completions/outstanding/{id}"),
                traffic,
                broker_wiring.clone(),
                cancel,
                client.clone(),
                runtime.clone(),
            ) {
                eprintln!("BROKER BLOCK ERROR: {e}");
            }
        }),
    );
    broker
        .mount_async(oxpath!("gateway", "completions"), store)
        .await;

    let wire_client = broker.client();
    let wire_runtime = tokio::runtime::Handle::current();
    let wire = ox_gateway::wire_store::WireStore::new(
        tokio::runtime::Handle::current(),
        Arc::new(move |id, cancel| {
            let path = format!("wire/outstanding/{id}");
            let dialect = wire_runtime
                .block_on(async {
                    wire_client
                        .read(
                            &structfs_core_store::Path::parse(&format!("{path}/inbound")).unwrap(),
                        )
                        .await
                        .ok()
                        .flatten()
                        .and_then(|r| r.as_value().cloned())
                        .map(structfs_serde_store::value_to_json)
                })
                .and_then(|j| j["dialect"].as_str().map(|s| s.to_string()))
                .unwrap_or_else(|| "anthropic".into());
            if let Err(e) = ox_gateway::broker_block::run_wire(
                path,
                dialect,
                wire_wiring.clone(),
                cancel,
                wire_client.clone(),
                wire_runtime.clone(),
            ) {
                eprintln!("WIRE BLOCK ERROR: {e}");
            }
        }),
    );
    broker.mount_async(oxpath!("wire"), wire).await;

    let stats_client = broker.client();
    let stats_runtime = tokio::runtime::Handle::current();
    let telemetry = ox_gateway::telemetry_store::TelemetryStore::new(
        tokio::runtime::Handle::current(),
        Arc::new(move |id, cancel| {
            if let Err(e) = ox_gateway::broker_block::run_stats(
                format!("gateway/telemetry/outstanding/{id}"),
                stats_wiring.clone(),
                cancel,
                stats_client.clone(),
                stats_runtime.clone(),
            ) {
                eprintln!("STATS BLOCK ERROR: {e}");
            }
        }),
    );
    broker
        .mount_async(oxpath!("gateway", "telemetry"), telemetry)
        .await;
}

/// Build a broker preloaded with two accounts ("anthropic" + "openai") each
/// pointing at the matching provider. Catalogs are populated via
/// `GateStore::write`.
///
/// Used by the /v1/models aggregation test. No mock executor or UsageStore is
/// needed because that test only calls GET /v1/models which reads from the
/// gate directly; no upstream LLM call is made.
#[allow(dead_code)]
pub async fn build_test_broker_two_accounts() -> BrokerStore {
    use ox_gate::{ApiKey, ModelInfo, ModelInfoSource};
    use ox_store_util::LocalConfig;
    use structfs_core_store::Writer;

    let broker = BrokerStore::new(Duration::from_secs(5));

    // GateStore::new() already carries built-in anthropic + openai accounts
    // and providers. We only need to add the per-provider model catalogs.
    let mut gate = ox_gate::GateStore::new();

    let claude = ModelInfo {
        id: "claude-sonnet-4-20250514".into(),
        display_name: "Claude Sonnet 4".into(),
        max_context_size: None,
        max_output_tokens: None,
        source: ModelInfoSource::Server,
    };
    let gpt = ModelInfo {
        id: "gpt-4o".into(),
        display_name: "GPT-4o".into(),
        max_context_size: None,
        max_output_tokens: None,
        source: ModelInfoSource::Server,
    };

    gate.write(
        &path!("providers/anthropic/models"),
        Record::parsed(to_value(&vec![claude]).unwrap()),
    )
    .expect("write anthropic catalog");
    gate.write(
        &path!("providers/openai/models"),
        Record::parsed(to_value(&vec![gpt]).unwrap()),
    )
    .expect("write openai catalog");

    // Mount GateStore at "gate/" so that gate/snapshot/state is served by
    // GateStore's own Reader (not a raw LocalConfig key).
    broker.mount(oxpath!("gate"), gate).await;

    // Secrets aren't needed for /v1/models (no upstream call), but populate
    // them so any key-read path that fires doesn't panic on a missing mount.
    let mut secret = LocalConfig::new();
    secret.set(
        "keys/anthropic",
        to_value(&ApiKey::new("sk-anth-test")).unwrap(),
    );
    secret.set(
        "keys/openai",
        to_value(&ApiKey::new("sk-oai-test")).unwrap(),
    );
    broker.mount(oxpath!("secret"), secret).await;

    broker
}
