//! Integration test for GET /v1/models — verifies that the endpoint aggregates
//! model catalogs across both the anthropic and openai accounts and prefixes
//! each model id with its account name.

mod common;

use common::build_test_broker_two_accounts;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn models_aggregates_across_accounts() {
    let broker = build_test_broker_two_accounts().await;
    let app = ox_gateway::routes::build_router(broker.client());

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let resp = reqwest::Client::new()
        .get(format!("http://{}/v1/models", addr))
        .send()
        .await
        .unwrap();

    assert!(resp.status().is_success(), "status: {}", resp.status());
    let body: serde_json::Value = resp.json().await.unwrap();
    let ids: Vec<String> = body["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap().to_string())
        .collect();

    assert!(
        ids.contains(&"anthropic/claude-sonnet-4-20250514".to_string()),
        "missing anthropic model in {ids:?}"
    );
    assert!(
        ids.contains(&"openai/gpt-4o".to_string()),
        "missing openai model in {ids:?}"
    );
}
