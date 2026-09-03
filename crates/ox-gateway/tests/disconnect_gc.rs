//! Client-disconnect lifecycle test: an abandoned streaming request must
//! not leak its inflight entry. axum drops the response stream on
//! disconnect, so the GC relies on the Drop guard in `handle.rs` rather
//! than code after the drain loop.

mod common;

use common::build_test_broker;
use futures::StreamExt;
use ox_gate::completion_broker::CompletionStatus;
use ox_gate::completion_broker::mock::MockSseExecutor;
use ox_types::StreamEvent;
use std::sync::Arc;
use std::time::Duration;
use structfs_core_store::path;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnect_mid_stream_gcs_inflight() {
    let executor = Arc::new(MockSseExecutor::new());
    executor.push_immediate(StreamEvent::InputUsage {
        input_tokens: 10,
        cache_creation: 0,
        cache_read: 0,
    });
    executor.push_immediate(StreamEvent::TextDelta {
        text: "partial".into(),
    });
    // Park the upstream long enough that the client disconnect happens
    // while the request is still in flight.
    executor.push(Duration::from_secs(30), Ok(StreamEvent::MessageStop));

    let broker = build_test_broker(executor, "anthropic").await;
    let observer = broker.client();
    let app = ox_gateway::routes::build_router(broker.client());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("http://{addr}/v1/messages"))
        .json(&serde_json::json!({
            "model": "anthropic/claude-sonnet-4-20250514",
            "max_tokens": 100,
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());

    // Read one chunk so the request is genuinely mid-stream, then hang up.
    let mut body = resp.bytes_stream();
    let first = body.next().await.expect("first SSE chunk").unwrap();
    assert!(!first.is_empty());

    let inflight = path!("gateway/completions/outstanding/0");
    let status: Option<CompletionStatus> = observer.read_typed(&inflight).await.unwrap();
    assert!(status.is_some(), "inflight should exist while streaming");

    drop(body);

    // The Drop guard spawns the GC write; poll until the entry is gone.
    for _ in 0..50 {
        let status: Option<CompletionStatus> = observer.read_typed(&inflight).await.unwrap();
        if status.is_none() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("inflight entry still present 5s after client disconnect");
}
