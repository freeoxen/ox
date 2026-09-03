//! End-to-end streaming test for POST /v1/chat/completions.
//!
//! Sets up an in-memory broker + MockSseExecutor, starts axum on a
//! random port, and verifies that the response carries OpenAI-shaped
//! SSE frames matching the scripted event sequence.

mod common;

use common::build_test_broker;
use ox_gate::completion_broker::mock::MockSseExecutor;
use ox_types::StreamEvent;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn streaming_openai_chat_completions_endpoint() {
    let executor = Arc::new(MockSseExecutor::new());
    executor.push_immediate(StreamEvent::TextDelta { text: "Hi".into() });
    executor.push_immediate(StreamEvent::OutputUsage { output_tokens: 1 });
    executor.push_immediate(StreamEvent::MessageStop);

    let broker = build_test_broker(executor, "openai").await;
    let app = ox_gateway::routes::build_router(broker.client());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{}/v1/chat/completions", addr))
        .json(&serde_json::json!({
            "model": "openai/gpt-4o",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();

    assert!(resp.status().is_success(), "got status {}", resp.status());
    let body = resp.text().await.unwrap();
    assert!(
        body.contains("\"role\":\"assistant\""),
        "missing role: {body}"
    );
    assert!(
        body.contains("\"content\":\"Hi\""),
        "missing content: {body}"
    );
    assert!(
        body.contains("data: [DONE]"),
        "missing [DONE] terminator: {body}"
    );
}
