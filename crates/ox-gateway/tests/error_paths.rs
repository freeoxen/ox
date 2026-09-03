//! Error-path integration tests for the gateway: resolution failures
//! (unknown role) and upstream errors propagate as dialect-shaped error
//! frames in the SSE stream.

mod common;

use common::build_test_broker;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_role_streams_error_frame() {
    // No events scripted — the dispatch task fails before the executor is
    // even called, because the model name doesn't slash-form-parse to a
    // known account and doesn't match any gate/completions/{name}.
    // Use a name without hyphens so it passes PathComponent validation and
    // reaches the substrate lookup, which returns "no role named 'nopesuchrole'".
    let executor = Arc::new(ox_gate::completion_broker::mock::MockSseExecutor::new());

    let broker = build_test_broker(executor, "anthropic").await;
    let app = ox_gateway::routes::build_router(broker.client());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/messages", addr))
        .json(&serde_json::json!({
            "model": "nopesuchrole",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();

    // Body should contain an error frame referencing the unknown role.
    let body = tokio::time::timeout(Duration::from_secs(5), resp.text())
        .await
        .expect("response should arrive within 5s")
        .unwrap();
    assert!(
        body.contains("event: error") || body.contains("\"type\":\"error\""),
        "expected error frame; got body: {body}"
    );
    assert!(
        body.contains("no role named") || body.contains("nopesuchrole"),
        "expected reason mentioning the role name; got: {body}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn upstream_error_propagates_as_sse_error_frame() {
    let executor = Arc::new(ox_gate::completion_broker::mock::MockSseExecutor::new());
    // The executor's first emission is an error. The dispatch task flips
    // status to Failed; the drain emits an error frame.
    executor.push(
        Duration::ZERO,
        Err("HTTP 401 from upstream: invalid key".into()),
    );

    let broker = build_test_broker(executor, "anthropic").await;
    let app = ox_gateway::routes::build_router(broker.client());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let resp = reqwest::Client::new()
        .post(format!("http://{}/v1/messages", addr))
        .json(&serde_json::json!({
            "model": "anthropic/claude-sonnet-4-20250514",
            "max_tokens": 64,
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();

    let body = tokio::time::timeout(Duration::from_secs(5), resp.text())
        .await
        .expect("response should arrive within 5s")
        .unwrap();
    assert!(
        body.contains("event: error") || body.contains("\"type\":\"error\""),
        "expected error frame; got body: {body}"
    );
    assert!(
        body.contains("401") || body.contains("invalid key"),
        "expected reason mentioning upstream error; got: {body}"
    );
}
