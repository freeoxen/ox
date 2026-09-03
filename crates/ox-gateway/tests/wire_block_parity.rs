//! Wire-Block parity: the http-in edge + wire Block must reproduce the
//! native routes' behavior — buffered bodies, streaming frames, and
//! dialect-shaped error envelopes — with every dialect decision made
//! inside wasm. The fixture runs the all-wasm configuration: wire Block
//! over broker Block over the upstream mount.

mod common;

use common::MemoryBacking;
use ox_broker::BrokerStore;
use ox_gate::completion_broker::mock::MockSseExecutor;
use ox_path::oxpath;
use ox_types::StreamEvent;
use std::sync::Arc;
use std::time::Duration;
use structfs_core_store::path;
use structfs_serde_store::to_value;

async fn build_all_wasm_broker(executor: Arc<MockSseExecutor>) -> BrokerStore {
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

    let upstream = ox_gate::UpstreamStore::new(executor, tokio::runtime::Handle::current());
    broker.mount_async(oxpath!("upstream"), upstream).await;
    common::install_blocks(&broker, false).await;

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
        input_tokens: 8,
        cache_creation: 0,
        cache_read: 0,
    });
    executor.push_immediate(StreamEvent::TextDelta {
        text: "wire ".into(),
    });
    executor.push_immediate(StreamEvent::TextDelta {
        text: "works".into(),
    });
    executor.push_immediate(StreamEvent::OutputUsage { output_tokens: 2 });
    executor.push_immediate(StreamEvent::MessageStop);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wire_buffered_anthropic() {
    let executor = Arc::new(MockSseExecutor::new());
    script(&executor);
    let broker = build_all_wasm_broker(executor).await;
    let addr = serve(&broker).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/messages"))
        .json(&serde_json::json!({
            "model": "primary",
            "max_tokens": 40,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["content"][0]["text"], "wire works");
    assert_eq!(body["stop_reason"], "end_turn");
    assert_eq!(body["usage"]["input_tokens"], 8);
    assert!(body["id"].as_str().unwrap().starts_with("msg_"));
    assert_eq!(body["model"], "primary");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wire_streaming_openai() {
    let executor = Arc::new(MockSseExecutor::new());
    script(&executor);
    let broker = build_all_wasm_broker(executor).await;
    let addr = serve(&broker).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "anthropic/claude-sonnet-4-20250514",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    assert!(
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .starts_with("text/event-stream")
    );
    let body = resp.text().await.unwrap();
    assert!(body.contains("\"content\":\"wire \""), "body:\n{body}");
    assert!(body.contains("\"content\":\"works\""));
    assert!(body.contains("\"finish_reason\":\"stop\""));
    assert!(body.contains("\"total_tokens\":10"));
    assert!(
        body.trim_end().ends_with("data: [DONE]"),
        "must end with [DONE]"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wire_decode_error_is_dialect_shaped_400() {
    let executor = Arc::new(MockSseExecutor::new());
    let broker = build_all_wasm_broker(executor).await;
    let addr = serve(&broker).await;

    // Anthropic requires max_tokens; its absence is a 400 in the
    // anthropic envelope shape.
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/messages"))
        .json(&serde_json::json!({
            "model": "primary",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["type"], "error");
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("max_tokens")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wire_resolution_failure_buffered_500() {
    let executor = Arc::new(MockSseExecutor::new());
    let broker = build_all_wasm_broker(executor).await;
    let addr = serve(&broker).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "nosuchrole",
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
            .contains("nosuchrole")
    );
    assert_eq!(body["error"]["type"], "api_error");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wire_resolution_failure_streaming_error_frame() {
    let executor = Arc::new(MockSseExecutor::new());
    let broker = build_all_wasm_broker(executor).await;
    let addr = serve(&broker).await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/messages"))
        .json(&serde_json::json!({
            "model": "nosuchrole",
            "max_tokens": 5,
            "stream": true,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_success(),
        "stream starts before resolution"
    );
    let body = resp.text().await.unwrap();
    assert!(body.contains("event: error"), "body:\n{body}");
    assert!(body.contains("nosuchrole"));
}
