//! SDK-strict wire-compatibility tests.
//!
//! Official Anthropic/OpenAI SDKs deserialize responses into models whose
//! identity fields (`id`, `model`, `created`, stop reasons) are required.
//! These tests mirror those models as strict serde structs — every field a
//! real SDK needs is non-optional here — so a regression back to placeholder
//! or missing fields fails the build instead of breaking clients at runtime.

mod common;

use common::build_test_broker;
use ox_gate::completion_broker::mock::MockSseExecutor;
use ox_types::StreamEvent;
use serde::Deserialize;
use std::sync::Arc;

// --- Anthropic SDK model mirror -------------------------------------------

#[derive(Deserialize)]
struct AnthropicMessage {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    role: String,
    model: String,
    content: Vec<AnthropicContentBlock>,
    stop_reason: Option<String>,
    stop_sequence: Option<String>,
    usage: AnthropicUsage,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum AnthropicContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
}

#[derive(Deserialize)]
struct AnthropicUsage {
    input_tokens: u64,
    output_tokens: u64,
}

#[derive(Deserialize)]
struct AnthropicMessageDelta {
    delta: AnthropicDeltaBody,
    usage: AnthropicDeltaUsage,
}

#[derive(Deserialize)]
struct AnthropicDeltaBody {
    stop_reason: String,
    stop_sequence: Option<String>,
}

#[derive(Deserialize)]
struct AnthropicDeltaUsage {
    output_tokens: u64,
}

// --- OpenAI SDK model mirror ----------------------------------------------

#[derive(Deserialize)]
struct OpenAiChunk {
    id: String,
    object: String,
    created: u64,
    model: String,
    choices: Vec<OpenAiChunkChoice>,
}

#[derive(Deserialize)]
struct OpenAiChunkChoice {
    index: u64,
    delta: serde_json::Value,
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct OpenAiCompletion {
    id: String,
    object: String,
    created: u64,
    model: String,
    choices: Vec<OpenAiChoice>,
    usage: OpenAiUsage,
}

#[derive(Deserialize)]
struct OpenAiChoice {
    index: u64,
    message: OpenAiMessage,
    finish_reason: String,
}

#[derive(Deserialize)]
struct OpenAiMessage {
    role: String,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<serde_json::Value>>,
}

#[derive(Deserialize)]
struct OpenAiUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
}

// --- Harness ---------------------------------------------------------------

async fn serve(executor: Arc<MockSseExecutor>, dialect: &str) -> std::net::SocketAddr {
    let broker = build_test_broker(executor, dialect).await;
    let app = ox_gateway::routes::build_router(broker.client());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    addr
}

fn text_then_tool_events(executor: &MockSseExecutor) {
    executor.push_immediate(StreamEvent::InputUsage {
        input_tokens: 10,
        cache_creation: 0,
        cache_read: 0,
    });
    executor.push_immediate(StreamEvent::TextDelta {
        text: "Reading.".into(),
    });
    executor.push_immediate(StreamEvent::ToolUseStart {
        id: "toolu_1".into(),
        name: "read_file".into(),
    });
    executor.push_immediate(StreamEvent::ToolUseInputDelta {
        delta: r#"{"path":"/etc/hosts"}"#.into(),
    });
    executor.push_immediate(StreamEvent::OutputUsage { output_tokens: 7 });
    executor.push_immediate(StreamEvent::MessageStop);
}

/// Split an SSE body into (event-name, data-json) pairs. The event name is
/// empty for OpenAI-style frames that carry only a `data:` line.
fn parse_sse(body: &str) -> Vec<(String, String)> {
    let mut frames = Vec::new();
    let mut event = String::new();
    for line in body.lines() {
        if let Some(name) = line.strip_prefix("event: ") {
            event = name.to_string();
        } else if let Some(data) = line.strip_prefix("data: ") {
            frames.push((std::mem::take(&mut event), data.to_string()));
        }
    }
    frames
}

// --- Anthropic dialect -----------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn anthropic_stream_deserializes_into_sdk_models() {
    let executor = Arc::new(MockSseExecutor::new());
    text_then_tool_events(&executor);
    let addr = serve(executor, "anthropic").await;

    let resp = reqwest::Client::new()
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
    let body = resp.text().await.unwrap();
    let frames = parse_sse(&body);

    let start = frames
        .iter()
        .find(|(e, _)| e == "message_start")
        .expect("no message_start frame");
    let start_json: serde_json::Value = serde_json::from_str(&start.1).unwrap();
    let msg: AnthropicMessage =
        serde_json::from_value(start_json["message"].clone()).expect("SDK-shape message_start");
    assert!(
        msg.id.starts_with("msg_"),
        "id {:?} lacks msg_ prefix",
        msg.id
    );
    assert_eq!(msg.kind, "message");
    assert_eq!(msg.role, "assistant");
    assert_eq!(msg.model, "anthropic/claude-sonnet-4-20250514");
    assert!(msg.stop_reason.is_none());
    assert!(msg.stop_sequence.is_none());
    assert!(msg.content.is_empty());
    assert_eq!(msg.usage.input_tokens, 10);
    assert_eq!(msg.usage.output_tokens, 0);

    let delta = frames
        .iter()
        .find(|(e, _)| e == "message_delta")
        .expect("no message_delta frame");
    let delta: AnthropicMessageDelta =
        serde_json::from_str(&delta.1).expect("SDK-shape message_delta");
    assert_eq!(delta.delta.stop_reason, "tool_use");
    assert!(delta.delta.stop_sequence.is_none());
    assert_eq!(delta.usage.output_tokens, 7);

    assert!(frames.iter().any(|(e, _)| e == "message_stop"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn anthropic_buffered_response_deserializes_into_sdk_model() {
    let executor = Arc::new(MockSseExecutor::new());
    text_then_tool_events(&executor);
    let addr = serve(executor, "anthropic").await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/messages"))
        .json(&serde_json::json!({
            "model": "anthropic/claude-sonnet-4-20250514",
            "max_tokens": 100,
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());

    let msg: AnthropicMessage = resp.json().await.expect("SDK-shape message");
    assert!(msg.id.starts_with("msg_"));
    assert_eq!(msg.kind, "message");
    assert_eq!(msg.model, "anthropic/claude-sonnet-4-20250514");
    assert_eq!(msg.stop_reason.as_deref(), Some("tool_use"));
    assert_eq!(msg.usage.input_tokens, 10);
    assert_eq!(msg.usage.output_tokens, 7);
    assert_eq!(msg.content.len(), 2);
    match &msg.content[0] {
        AnthropicContentBlock::Text { text } => assert_eq!(text, "Reading."),
        _ => panic!("first block should be text"),
    }
    match &msg.content[1] {
        AnthropicContentBlock::ToolUse { id, name, input } => {
            assert_eq!(id, "toolu_1");
            assert_eq!(name, "read_file");
            assert_eq!(input["path"], "/etc/hosts");
        }
        _ => panic!("second block should be tool_use"),
    }
}

// --- OpenAI dialect --------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn openai_stream_deserializes_into_sdk_models() {
    let executor = Arc::new(MockSseExecutor::new());
    text_then_tool_events(&executor);
    let addr = serve(executor, "openai").await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "openai/gpt-4o",
            "messages": [{"role": "user", "content": "hi"}],
            "stream": true
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let body = resp.text().await.unwrap();
    let frames = parse_sse(&body);

    let (done, chunks): (Vec<_>, Vec<_>) = frames.iter().partition(|(_, d)| d == "[DONE]");
    assert_eq!(done.len(), 1, "stream must end with data: [DONE]");
    assert!(!chunks.is_empty());

    let mut ids = std::collections::HashSet::new();
    let mut finish_reasons = Vec::new();
    for (_, data) in &chunks {
        let chunk: OpenAiChunk = serde_json::from_str(data)
            .unwrap_or_else(|e| panic!("chunk failed SDK-shape parse: {e}\n{data}"));
        assert!(chunk.id.starts_with("chatcmpl-"), "id {:?}", chunk.id);
        assert_eq!(chunk.object, "chat.completion.chunk");
        assert!(chunk.created > 0, "created must be a real unix timestamp");
        assert_eq!(chunk.model, "openai/gpt-4o");
        ids.insert(chunk.id);
        for choice in chunk.choices {
            assert_eq!(choice.index, 0);
            let _ = choice.delta;
            if let Some(reason) = choice.finish_reason {
                finish_reasons.push(reason);
            }
        }
    }
    assert_eq!(ids.len(), 1, "all chunks must share one completion id");
    assert_eq!(finish_reasons, vec!["tool_calls".to_string()]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn openai_buffered_response_deserializes_into_sdk_model() {
    let executor = Arc::new(MockSseExecutor::new());
    text_then_tool_events(&executor);
    let addr = serve(executor, "openai").await;

    let resp = reqwest::Client::new()
        .post(format!("http://{addr}/v1/chat/completions"))
        .json(&serde_json::json!({
            "model": "openai/gpt-4o",
            "messages": [{"role": "user", "content": "hi"}]
        }))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());

    let completion: OpenAiCompletion = resp.json().await.expect("SDK-shape completion");
    assert!(completion.id.starts_with("chatcmpl-"));
    assert_eq!(completion.object, "chat.completion");
    assert!(completion.created > 0);
    assert_eq!(completion.model, "openai/gpt-4o");
    assert_eq!(completion.usage.prompt_tokens, 10);
    assert_eq!(completion.usage.completion_tokens, 7);
    assert_eq!(completion.usage.total_tokens, 17);
    assert_eq!(completion.choices.len(), 1);
    let choice = &completion.choices[0];
    assert_eq!(choice.index, 0);
    assert_eq!(choice.finish_reason, "tool_calls");
    assert_eq!(choice.message.role, "assistant");
    assert_eq!(choice.message.content.as_deref(), Some("Reading."));
    let tool_calls = choice
        .message
        .tool_calls
        .as_ref()
        .expect("tool_calls present");
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0]["function"]["name"], "read_file");
}
