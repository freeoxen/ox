//! End-to-end streaming test for POST /v1/messages.
//!
//! Sets up an in-memory broker + MockSseExecutor, starts axum on a
//! random port, and verifies that the response carries Anthropic-shaped
//! SSE frames matching the scripted event sequence.

mod common;

use common::build_test_broker;
use ox_gate::completion_broker::mock::MockSseExecutor;
use ox_types::StreamEvent;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_anthropic_messages_endpoint() {
    let executor = Arc::new(MockSseExecutor::new());
    executor.push_immediate(StreamEvent::InputUsage {
        input_tokens: 10,
        cache_creation: 0,
        cache_read: 0,
    });
    executor.push_immediate(StreamEvent::TextDelta {
        text: "Hello".into(),
    });
    executor.push_immediate(StreamEvent::OutputUsage { output_tokens: 1 });
    executor.push_immediate(StreamEvent::MessageStop);

    let broker = build_test_broker(executor, "anthropic").await;
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
