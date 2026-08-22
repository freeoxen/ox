//! GET /stats aggregates the usage ledger; GET /dashboard serves the page.

mod common;

use common::build_test_broker;
use ox_gate::completion_broker::mock::MockSseExecutor;
use ox_types::StreamEvent;
use std::sync::Arc;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stats_reflect_completed_requests() {
    let executor = Arc::new(MockSseExecutor::new());
    executor.push_immediate(StreamEvent::InputUsage {
        input_tokens: 11,
        cache_creation: 0,
        cache_read: 3,
    });
    executor.push_immediate(StreamEvent::TextDelta { text: "hi".into() });
    executor.push_immediate(StreamEvent::OutputUsage { output_tokens: 5 });
    executor.push_immediate(StreamEvent::MessageStop);

    let broker = build_test_broker(executor, "anthropic").await;
    let app = ox_gateway::routes::build_router(broker.client());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let http = reqwest::Client::new();

    // Empty ledger first: zeros, not errors.
    let empty: serde_json::Value = http
        .get(format!("http://{addr}/stats"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(empty["totals"]["requests"], 0);
    assert_eq!(empty["by_hour"].as_array().unwrap().len(), 24);
    assert!(empty["totals"]["estimated_cost_usd"].is_null());

    // Drive one completion through, then re-read.
    let resp = http
        .post(format!("http://{addr}/v1/messages"))
        .json(&serde_json::json!({
            "model": "anthropic/claude-sonnet-4-20250514",
            "max_tokens": 50,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());

    let stats: serde_json::Value = http
        .get(format!("http://{addr}/stats"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(stats["totals"]["requests"], 1);
    assert_eq!(stats["totals"]["input_tokens"], 11);
    assert_eq!(stats["totals"]["output_tokens"], 5);
    assert_eq!(stats["totals"]["cache_read_input_tokens"], 3);
    assert_eq!(stats["today"]["requests"], 1);
    assert_eq!(stats["in_flight"], 0, "completed request must be GC'd");

    let models = stats["by_model"].as_array().unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0]["account"], "anthropic");
    assert_eq!(models[0]["model_id"], "claude-sonnet-4-20250514");
    assert_eq!(models[0]["input_tokens"], 11);

    let hours = stats["by_hour"].as_array().unwrap();
    let bucketed: u64 = hours.iter().map(|h| h["requests"].as_u64().unwrap()).sum();
    assert_eq!(bucketed, 1, "the completion must land in an hour bucket");

    let recent = stats["recent"].as_array().unwrap();
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0]["model_id"], "claude-sonnet-4-20250514");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dashboard_serves_html() {
    let executor = Arc::new(MockSseExecutor::new());
    let broker = build_test_broker(executor, "anthropic").await;
    let app = ox_gateway::routes::build_router(broker.client());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let resp = reqwest::get(format!("http://{addr}/dashboard")).await.unwrap();
    assert!(resp.status().is_success());
    let ct = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(ct.starts_with("text/html"), "content-type was {ct}");
    let body = resp.text().await.unwrap();
    assert!(body.contains("Tokens by model"));
    assert!(body.contains("fetch(\"/stats\")"));
}
