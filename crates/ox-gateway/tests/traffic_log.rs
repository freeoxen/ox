//! Traffic logging: the dispatch task appends one full record per
//! completion, and the ledger sink re-emits it as ox conversation-ledger
//! entries in a daily thread dir readable by ox-cli.

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

async fn build_broker_with_traffic(
    executor: Arc<MockSseExecutor>,
    threads_dir: std::path::PathBuf,
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

    let traffic = ox_gateway::traffic::TrafficLogStore::new(
        Box::new(MemoryBacking::new()),
        Some(threads_dir),
    );
    broker.mount(oxpath!("gateway", "traffic"), traffic).await;

    let client = broker.client();
    let upstream = ox_gate::UpstreamStore::new(executor, tokio::runtime::Handle::current());
    broker.mount_async(oxpath!("upstream"), upstream).await;
    let store = CompletionBrokerStore::new(
        client.clone(),
        client.scoped("upstream"),
        client.scoped("gateway/usage"),
        tokio::runtime::Handle::current(),
    )
    .with_traffic_writer(client.scoped("gateway/traffic"));
    broker
        .mount_async(oxpath!("gateway", "completions"), store)
        .await;

    broker
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn completion_logs_full_record_and_ledger_thread() {
    let tmp = tempfile::tempdir().unwrap();
    let threads_dir = tmp.path().to_path_buf();

    let executor = Arc::new(MockSseExecutor::new());
    executor.push_immediate(StreamEvent::InputUsage {
        input_tokens: 9,
        cache_creation: 0,
        cache_read: 0,
    });
    executor.push_immediate(StreamEvent::TextDelta { text: "Hello ".into() });
    executor.push_immediate(StreamEvent::TextDelta { text: "there".into() });
    executor.push_immediate(StreamEvent::OutputUsage { output_tokens: 2 });
    executor.push_immediate(StreamEvent::MessageStop);

    let broker = build_broker_with_traffic(executor, threads_dir.clone()).await;
    let app = ox_gateway::routes::build_router(broker.client());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/messages"))
        .json(&serde_json::json!({
            "model": "primary",
            "max_tokens": 40,
            "temperature": 0.1,
            "messages": [
                {"role": "user", "content": "earlier turn"},
                {"role": "assistant", "content": "earlier reply"},
                {"role": "user", "content": "What is up?"}
            ]
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());

    // The dispatch task writes the traffic record after the route's drain
    // completes; poll briefly.
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
    assert_eq!(records.len(), 1, "one completion record expected");
    let rec = &records[0];

    // Full detail: decoded request (with extras), upstream body, events, status.
    assert_eq!(rec["kind"], "completion");
    assert_eq!(rec["request"]["model"], "primary");
    assert_eq!(rec["request"]["temperature"], 0.1);
    assert_eq!(rec["request"]["messages"].as_array().unwrap().len(), 3);
    assert_eq!(rec["upstream_body"]["model"], "claude-sonnet-4-20250514");
    assert_eq!(rec["upstream_body"]["temperature"], 0.1);
    assert_eq!(rec["status"]["state"], "complete");
    let ev_types: Vec<&str> = rec["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["type"].as_str())
        .collect();
    assert_eq!(
        ev_types,
        ["input_usage", "text_delta", "text_delta", "output_usage", "message_stop"]
    );

    // Ledger thread: dir, context, chained entries in ox message order.
    let thread = std::fs::read_dir(&threads_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| e.file_name().to_string_lossy().starts_with("t_gateway_"))
        .expect("gateway thread dir created");
    let ctx: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(thread.path().join("context.json")).unwrap())
            .unwrap();
    assert_eq!(ctx["version"], 1);
    assert!(ctx["title"].as_str().unwrap().starts_with("Gateway traffic"));
    assert!(thread.path().join("view.json").exists());

    let ledger = std::fs::read_to_string(thread.path().join("ledger.jsonl")).unwrap();
    let entries: Vec<serde_json::Value> = ledger
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let types: Vec<&str> = entries
        .iter()
        .filter_map(|e| e["msg"]["type"].as_str())
        .collect();
    assert_eq!(
        types,
        ["user", "turn_start", "completion_end", "assistant", "turn_end"]
    );
    // Hash chain holds.
    for pair in entries.windows(2) {
        assert_eq!(pair[1]["parent"], pair[0]["hash"], "broken hash chain");
    }
    assert_eq!(entries[0]["msg"]["content"], "What is up?");
    assert_eq!(entries[2]["msg"]["input_tokens"], 9);
    assert_eq!(entries[3]["msg"]["content"][0]["text"], "Hello there");
    assert_eq!(entries[4]["msg"]["output_tokens"], 2);
    assert_eq!(entries[4]["msg"]["model"], "claude-sonnet-4-20250514");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_completion_still_logs_with_reason() {
    let tmp = tempfile::tempdir().unwrap();
    let executor = Arc::new(MockSseExecutor::new());
    let broker = build_broker_with_traffic(executor, tmp.path().to_path_buf()).await;
    let app = ox_gateway::routes::build_router(broker.client());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let _ = reqwest::Client::new()
        .post(format!("http://{addr}/v1/messages"))
        .json(&serde_json::json!({
            "model": "nosuchrole",
            "max_tokens": 10,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();

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
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["status"]["state"], "failed");
    assert!(records[0]["status"]["reason"]
        .as_str()
        .unwrap()
        .contains("nosuchrole"));

    // Ledger renders the failure as visible assistant text.
    let thread = std::fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .find(|e| e.file_name().to_string_lossy().starts_with("t_gateway_"))
        .expect("thread dir");
    let ledger = std::fs::read_to_string(thread.path().join("ledger.jsonl")).unwrap();
    assert!(ledger.contains("[gateway error]"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_middleware_appends_access_records() {
    let tmp = tempfile::tempdir().unwrap();
    let executor = Arc::new(MockSseExecutor::new());
    let broker = build_broker_with_traffic(executor, tmp.path().to_path_buf()).await;
    let app = ox_gateway::routes::build_router(broker.client()).layer(
        axum::middleware::from_fn_with_state(
            broker.client(),
            ox_gateway::traffic::http_log_middleware,
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let http = reqwest::Client::new();
    let _ = http.get(format!("http://{addr}/v1/models")).send().await.unwrap();
    let _ = http.get(format!("http://{addr}/stats")).send().await.unwrap();

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
    assert_eq!(records.len(), 1, "models logged, /stats excluded: {records:?}");
    assert_eq!(records[0]["kind"], "http");
    assert_eq!(records[0]["method"], "GET");
    assert_eq!(records[0]["path"], "/v1/models");
    assert_eq!(records[0]["status"], 200);
}
