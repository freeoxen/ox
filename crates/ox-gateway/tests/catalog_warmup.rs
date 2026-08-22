//! Catalog warm-up lifecycle: mirrors main.rs's production wiring
//! (ConfigStore at config/, secrets, GateStore, gate subscriptions) and
//! proves a refresh_now trigger flows through catalog_refresh into a
//! non-empty GET /v1/models.

use ox_broker::{BrokerStore, SyncClientAdapter};
use ox_gate::subscriptions::util::testing::MockTransport;
use ox_path::oxpath;
use ox_types::ModelInfo;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use structfs_core_store::{path, Record, Value};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn refresh_trigger_populates_v1_models() {
    let broker = BrokerStore::new(Duration::from_secs(5));

    // config/ — flat map in the same shape OxConfig::to_flat_map emits.
    let mut base = BTreeMap::new();
    base.insert(
        "gate/providers/localbox/dialect".to_string(),
        Value::String("anthropic".into()),
    );
    base.insert(
        "gate/providers/localbox/endpoint".to_string(),
        Value::String("http://127.0.0.1:1".into()),
    );
    base.insert(
        "gate/providers/localbox/version".to_string(),
        Value::String("2023-06-01".into()),
    );
    base.insert(
        "gate/accounts/localbox/provider".to_string(),
        Value::String("localbox".into()),
    );
    broker
        .mount(oxpath!("config"), ox_ui::ConfigStore::new(base))
        .await;

    // secret/ — key for the anthropic account.
    let mut secret_base = BTreeMap::new();
    secret_base.insert(
        "keys/localbox".to_string(),
        Value::String("sk-test".into()),
    );
    broker
        .mount(oxpath!("secret"), ox_ui::ConfigStore::new(secret_base))
        .await;

    // gate/ — wired to config + secret exactly like main.rs.
    let rt = tokio::runtime::Handle::current();
    let config_adapter = SyncClientAdapter::new(broker.client().scoped("config"), rt.clone());
    let secret_adapter = SyncClientAdapter::new(broker.client().scoped("secret"), rt.clone());
    let gate = ox_gate::GateStore::new()
        .with_config(Box::new(config_adapter))
        .with_secrets(Box::new(secret_adapter));
    broker.mount(oxpath!("gate"), gate).await;

    // Subscriptions with a mock transport serving one model.
    let catalog = vec![ModelInfo {
        id: "claude-warm".into(),
        display_name: "Claude Warm".into(),
        max_context_size: None,
        max_output_tokens: None,
        source: ox_types::ModelInfoSource::Server,
    }];
    let transport = Arc::new(MockTransport::new().with_catalog(Ok(catalog)));
    ox_gate::subscriptions::register_all(&broker, transport);

    // The same startup trigger main.rs writes.
    let client = broker.client();
    client
        .write(
            &path!("config/gate/accounts/localbox/refresh_now"),
            Record::parsed(Value::Null),
        )
        .await
        .expect("trigger write");

    // Stage 1: the subscription lands the catalog in the substrate.
    let mut catalog_seen = false;
    for _ in 0..50 {
        let models: Option<Vec<ModelInfo>> = client
            .read_typed(&path!("config/gate/accounts/localbox/models"))
            .await
            .ok()
            .flatten();
        if models.as_ref().is_some_and(|m| !m.is_empty()) {
            catalog_seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(catalog_seen, "catalog_refresh never wrote the catalog");

    // Stage 2: GET /v1/models serves it.
    let app = ox_gateway::routes::build_router(broker.client());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let resp: serde_json::Value = reqwest::Client::new()
        .get(format!("http://{addr}/v1/models"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ids: Vec<&str> = resp["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["id"].as_str())
        .collect();
    assert!(
        ids.contains(&"localbox/claude-warm"),
        "expected refreshed model in /v1/models, got {ids:?}"
    );
}
