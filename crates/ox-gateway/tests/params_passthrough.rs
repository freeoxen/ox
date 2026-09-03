//! Generation-parameter passthrough, asserted at the wire: the mock
//! executor records the exact HttpRequest the broker built, so these
//! tests check what an upstream would actually receive.

mod common;

use common::build_test_broker;
use ox_gate::completion_broker::mock::MockSseExecutor;
use ox_types::StreamEvent;
use std::sync::Arc;

async fn serve(executor: Arc<MockSseExecutor>, dialect: &str) -> std::net::SocketAddr {
    executor.push_immediate(StreamEvent::TextDelta { text: "ok".into() });
    executor.push_immediate(StreamEvent::MessageStop);
    let broker = build_test_broker(executor, dialect).await;
    let app = ox_gateway::routes::build_router(broker.client());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn openai_route_forwards_params_to_openai_upstream() {
    let executor = Arc::new(MockSseExecutor::new());
    let addr = serve(executor.clone(), "openai").await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "openai/gpt-4o",
            "messages": [{"role": "user", "content": "hi"}],
            "temperature": 0.2,
            "stop": ["END"],
            "seed": 7,
            "tool_choice": "auto",
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let _ = resp.text().await;

    let seen = executor.requests_seen();
    assert_eq!(seen.len(), 1);
    let body = seen[0].body.as_ref().expect("upstream body");
    assert_eq!(body["temperature"], serde_json::json!(0.2));
    assert_eq!(body["stop"], serde_json::json!(["END"]));
    assert_eq!(body["seed"], serde_json::json!(7));
    assert_eq!(body["tool_choice"], serde_json::json!("auto"));
    assert!(
        body.get("max_tokens").is_none(),
        "omitted cap must not be fabricated"
    );
    assert!(body.get("stop_sequences").is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn anthropic_route_forwards_params_to_anthropic_upstream() {
    let executor = Arc::new(MockSseExecutor::new());
    let addr = serve(executor.clone(), "anthropic").await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/messages"))
        .json(&serde_json::json!({
            "model": "anthropic/claude-sonnet-4-20250514",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hi"}],
            "temperature": 0.0,
            "top_k": 40,
            "stop_sequences": ["HALT"],
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let _ = resp.text().await;

    let seen = executor.requests_seen();
    assert_eq!(seen.len(), 1);
    let body = seen[0].body.as_ref().expect("upstream body");
    assert_eq!(body["temperature"], serde_json::json!(0.0));
    assert_eq!(body["top_k"], serde_json::json!(40));
    assert_eq!(body["stop_sequences"], serde_json::json!(["HALT"]));
    assert_eq!(body["max_tokens"], serde_json::json!(64));
    assert_eq!(body["stream"], serde_json::json!(true));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn anthropic_client_params_cross_to_openai_upstream() {
    let executor = Arc::new(MockSseExecutor::new());
    let addr = serve(executor.clone(), "openai").await;

    // Anthropic-dialect client, openai-dialect provider: shared params
    // translate, anthropic-only params stay off the wire.
    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/messages"))
        .json(&serde_json::json!({
            "model": "openai/gpt-4o",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hi"}],
            "temperature": 0.5,
            "top_k": 40,
            "stop_sequences": ["HALT"],
            "tool_choice": {"type": "any"},
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let _ = resp.text().await;

    let seen = executor.requests_seen();
    assert_eq!(seen.len(), 1);
    let body = seen[0].body.as_ref().expect("upstream body");
    assert_eq!(body["temperature"], serde_json::json!(0.5));
    assert_eq!(body["stop"], serde_json::json!(["HALT"]));
    assert_eq!(body["tool_choice"], serde_json::json!("required"));
    assert!(
        body.get("top_k").is_none(),
        "anthropic-only param must not cross"
    );
    assert!(body.get("stop_sequences").is_none());
}
