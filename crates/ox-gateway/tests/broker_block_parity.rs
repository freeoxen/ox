//! Broker-Block parity: the wasm dispatch path must behave identically to
//! the native task across the full request lifecycle — streaming and
//! buffered completions, resolution failures, usage records, and traffic
//! records. Fixtures mirror the production wiring with the block runner
//! installed.

mod common;

use common::MemoryBacking;
use ox_broker::BrokerStore;
use ox_gate::completion_broker::mock::MockSseExecutor;
use ox_gate::completion_broker::CompletionBrokerStore;
use ox_path::oxpath;
use ox_types::StreamEvent;
use std::sync::Arc;
use std::time::Duration;
use structfs_core_store::{path, Value};
use structfs_serde_store::to_value;

async fn build_block_broker(executor: Arc<MockSseExecutor>, traffic: bool) -> BrokerStore {
    // The block runs against the manifest-derived namespace, same as prod.
    let wiring = ox_gateway::assembly::Manifest::embedded()
        .unwrap()
        .wiring_for("broker", &ox_gateway::assembly::standard_bindings())
        .unwrap();
    build_block_broker_with(executor, traffic, wiring).await
}

async fn build_block_broker_with(
    executor: Arc<MockSseExecutor>,
    traffic: bool,
    wiring: ox_gateway::assembly::WiringTable,
) -> BrokerStore {
    use ox_gate::{AccountConfig, ApiKey, ProviderConfig};
    use ox_store_util::LocalConfig;
    use ox_types::CompletionRole;

    let broker = BrokerStore::new(Duration::from_secs(5));

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

    let mut secret = LocalConfig::new();
    secret.set("keys/anthropic", to_value(&ApiKey::new("sk-test")).unwrap());
    broker.mount(oxpath!("secret"), secret).await;

    let usage = ox_gate::UsageStore::new(Box::new(MemoryBacking::new()));
    broker.mount(oxpath!("gateway", "usage"), usage).await;

    if traffic {
        let traffic_store =
            ox_gateway::traffic::TrafficLogStore::new(Box::new(MemoryBacking::new()), None);
        broker.mount(oxpath!("gateway", "traffic"), traffic_store).await;
    }

    let client = broker.client();
    let upstream = ox_gate::UpstreamStore::new(executor, tokio::runtime::Handle::current());
    broker.mount_async(oxpath!("upstream"), upstream).await;

    let runner_client = client.clone();
    let runtime = tokio::runtime::Handle::current();
    let mut store = CompletionBrokerStore::new(
        client.clone(),
        client.scoped("upstream"),
        client.scoped("gateway/usage"),
        tokio::runtime::Handle::current(),
    )
    .with_block_runner(Arc::new(move |id| {
        if let Err(e) = ox_gateway::broker_block::run_broker(
            format!("gateway/completions/outstanding/{id}"),
            traffic,
            wiring.clone(),
            runner_client.clone(),
            runtime.clone(),
        ) {
            eprintln!("BROKER BLOCK ERROR: {e}");
        }
    }));
    if traffic {
        store = store.with_traffic_writer(client.scoped("gateway/traffic"));
    }
    broker
        .mount_async(oxpath!("gateway", "completions"), store)
        .await;

    broker
}

async fn serve(broker: &BrokerStore) -> std::net::SocketAddr {
    let app = ox_gateway::routes::build_router(broker.client());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

fn script(executor: &MockSseExecutor) {
    executor.push_immediate(StreamEvent::InputUsage {
        input_tokens: 10,
        cache_creation: 0,
        cache_read: 2,
    });
    executor.push_immediate(StreamEvent::TextDelta { text: "Hello ".into() });
    executor.push_immediate(StreamEvent::TextDelta { text: "block".into() });
    executor.push_immediate(StreamEvent::OutputUsage { output_tokens: 3 });
    executor.push_immediate(StreamEvent::MessageStop);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn block_dispatch_serves_buffered_completion() {
    let executor = Arc::new(MockSseExecutor::new());
    script(&executor);
    let broker = build_block_broker(executor, false).await;
    let addr = serve(&broker).await;

    let resp: serde_json::Value = reqwest::Client::new()
        .post(format!("http://{addr}/v1/messages"))
        .json(&serde_json::json!({
            "model": "primary",
            "max_tokens": 40,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    assert_eq!(resp["content"][0]["text"], "Hello block");
    assert_eq!(resp["stop_reason"], "end_turn");
    assert_eq!(resp["usage"]["input_tokens"], 10);
    assert_eq!(resp["usage"]["output_tokens"], 3);
    assert_eq!(resp["usage"]["cache_read_input_tokens"], 2);

    // Usage record landed with the native record's shape.
    let usage: serde_json::Value = structfs_serde_store::value_to_json(
        broker
            .client()
            .read(&oxpath!("gateway", "usage"))
            .await
            .unwrap()
            .unwrap()
            .as_value()
            .cloned()
            .unwrap(),
    );
    let records = usage.as_array().unwrap();
    assert_eq!(records.len(), 1);
    let r = &records[0];
    assert_eq!(r["account"], "anthropic");
    assert_eq!(r["model_id"], "claude-sonnet-4-20250514");
    assert_eq!(r["dialect"], "unknown");
    assert_eq!(r["upstream_dialect"], "anthropic");
    assert_eq!(r["input_tokens"], 10);
    assert_eq!(r["output_tokens"], 3);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn block_dispatch_streams() {
    let executor = Arc::new(MockSseExecutor::new());
    script(&executor);
    let broker = build_block_broker(executor, false).await;
    let addr = serve(&broker).await;

    let body = reqwest::Client::new()
        .post(format!("http://{addr}/v1/messages"))
        .json(&serde_json::json!({
            "model": "anthropic/claude-sonnet-4-20250514",
            "max_tokens": 40,
            "stream": true,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();

    assert!(body.contains("event: message_start"));
    assert!(body.contains("\"text\":\"Hello \""));
    assert!(body.contains("\"text\":\"block\""));
    assert!(body.contains("\"stop_reason\":\"end_turn\""));
    assert!(body.contains("event: message_stop"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn block_dispatch_reports_resolution_failure() {
    let executor = Arc::new(MockSseExecutor::new());
    let broker = build_block_broker(executor, false).await;
    let addr = serve(&broker).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/messages"))
        .json(&serde_json::json!({
            "model": "nosuchrole",
            "max_tokens": 10,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 500);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("nosuchrole"),
        "unexpected error body: {body}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn block_dispatch_writes_traffic_record() {
    let executor = Arc::new(MockSseExecutor::new());
    script(&executor);
    let broker = build_block_broker(executor, true).await;
    let addr = serve(&broker).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/messages"))
        .json(&serde_json::json!({
            "model": "primary",
            "max_tokens": 40,
            "temperature": 0.1,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());

    let client = broker.client();
    let mut records: Vec<serde_json::Value> = Vec::new();
    for _ in 0..50 {
        let raw = client
            .read(&oxpath!("gateway", "traffic"))
            .await
            .unwrap()
            .and_then(|r| r.as_value().cloned())
            .unwrap_or(Value::Array(vec![]));
        let json = structfs_serde_store::value_to_json(raw);
        records = json.as_array().cloned().unwrap_or_default();
        if !records.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(records.len(), 1, "one completion traffic record expected");
    let rec = &records[0];
    assert_eq!(rec["kind"], "completion");
    assert_eq!(rec["request"]["temperature"], 0.1);
    assert_eq!(rec["upstream_body"]["model"], "claude-sonnet-4-20250514");
    assert_eq!(rec["upstream_body"]["temperature"], 0.1);
    assert_eq!(rec["status"]["state"], "complete");
    assert_eq!(rec["events"].as_array().unwrap().len(), 5);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn manifest_wiring_is_load_bearing() {
    // Strip the broker's /secret wiring from the real manifest: the Block
    // must lose the ability to read keys, and the request must fail with
    // the namespace refusal — proving the manifest governs the namespace
    // rather than documenting it.
    let text = include_str!("../gateway.assembly.yaml")
        .replace("  - \"broker:/secret -> $secret\"\n", "");
    let manifest = ox_gateway::assembly::Manifest::parse(&text).unwrap();
    let wiring = manifest
        .wiring_for("broker", &ox_gateway::assembly::standard_bindings())
        .unwrap();
    assert_eq!(wiring.resolve("secret/keys/anthropic"), None);

    let executor = Arc::new(MockSseExecutor::new());
    let broker = build_block_broker_with(executor, false, wiring).await;
    let addr = serve(&broker).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/messages"))
        .json(&serde_json::json!({
            "model": "primary",
            "max_tokens": 10,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 500);
    let body: serde_json::Value = resp.json().await.unwrap();
    let msg = body["error"]["message"].as_str().unwrap();
    assert!(msg.contains("not wired"), "unexpected error body: {body}");
    assert!(msg.contains("secret/keys"), "unexpected error body: {body}");
}
