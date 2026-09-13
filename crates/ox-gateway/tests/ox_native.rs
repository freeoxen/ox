//! Native completion clients cross the same migrated guest/ownership boundary.

mod common;

use futures::StreamExt;
use ox_broker::{BrokerStore, ClientHandle};
use ox_gate::completion_broker::mock::MockSseExecutor;
use ox_types::StreamEvent;
use std::{sync::Arc, time::Duration};
use structfs_core_store::path;

fn request(stream: bool) -> serde_json::Value {
    serde_json::json!({
        "model": "anthropic/claude-sonnet-4-20250514",
        "max_tokens": 64,
        "system": "",
        "messages": [{"role": "user", "content": "hi"}],
        "tools": [],
        "stream": stream
    })
}

async fn serve(broker: &BrokerStore) -> String {
    let app = ox_gateway::routes::build_router(broker.client());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{address}/completions")
}

async fn assert_collected(client: &ClientHandle) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while client
            .read(&path!("gateway/completions/outstanding/0"))
            .await
            .unwrap()
            .is_some()
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("completion handle must be collected");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn buffered_native_response_preserves_events_and_collects_handle() {
    let executor = Arc::new(MockSseExecutor::new());
    let events = vec![
        StreamEvent::TextDelta {
            text: "hello".into(),
        },
        StreamEvent::OutputUsage { output_tokens: 2 },
        StreamEvent::MessageStop,
    ];
    for event in &events {
        executor.push_immediate(event.clone());
    }
    let broker = common::build_test_broker(executor, "anthropic").await;
    let response = reqwest::Client::new()
        .post(serve(&broker).await)
        .json(&request(false))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    let body: serde_json::Value = tokio::time::timeout(Duration::from_secs(5), response.json())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(body["status"]["state"], "complete");
    assert_eq!(body["events"], serde_json::to_value(events).unwrap());
    assert_collected(&broker.client()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_stream_preserves_partial_events_and_reports_provider_failure() {
    let executor = Arc::new(MockSseExecutor::new());
    let event = StreamEvent::TextDelta {
        text: "partial".into(),
    };
    executor.push_immediate(event.clone());
    executor.push_error("provider disconnected");
    let broker = common::build_test_broker(executor, "anthropic").await;
    let response = reqwest::Client::new()
        .post(serve(&broker).await)
        .json(&request(true))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_success());
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let body = tokio::time::timeout(Duration::from_secs(5), response.text())
        .await
        .unwrap()
        .unwrap();
    let event_frame = format!("data: {}\n\n", serde_json::to_string(&event).unwrap());
    let event_at = body.find(&event_frame).expect("partial event preserved");
    let error_at = body.find("event: error").expect("terminal error frame");
    assert!(event_at < error_at);
    assert!(body.contains("provider disconnected"));
    assert_collected(&broker.client()).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_client_disconnect_collects_a_running_completion() {
    let executor = Arc::new(MockSseExecutor::new());
    executor.push_immediate(StreamEvent::TextDelta {
        text: "partial".into(),
    });
    executor.push(Duration::from_secs(30), Ok(StreamEvent::MessageStop));
    let broker = common::build_test_broker(executor, "anthropic").await;
    let response = reqwest::Client::new()
        .post(serve(&broker).await)
        .json(&request(true))
        .send()
        .await
        .unwrap();
    let mut stream = response.bytes_stream();
    let first = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(!first.is_empty());
    assert!(
        broker
            .client()
            .read(&path!("gateway/completions/outstanding/0"))
            .await
            .unwrap()
            .is_some()
    );
    drop(stream);
    assert_collected(&broker.client()).await;
}
