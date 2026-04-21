//! Core types and agentic loop for the ox agent framework.
//!
//! `ox-kernel` provides the foundational building blocks for building LLM agents:
//!
//! - **Message types** — [`Message`], [`ContentBlock`], [`ToolCall`], [`ToolResult`]
//! - **Completion protocol** — [`CompletionRequest`], [`StreamEvent`]
//! - **Stateless kernel functions** — [`run_turn`], [`synthesize`],
//!   [`accumulate_response`], [`record_turn`], [`execute_tools`],
//!   [`record_tool_results`]
//! - **StructFS re-exports** — [`Reader`], [`Writer`], [`Store`], [`Path`], [`Value`],
//!   [`Record`], [`path!`] for building stores that compose into a namespace
//!
//! The kernel is deliberately synchronous and transport-agnostic. The caller
//! provides events (however obtained) and drives the loop — this keeps the
//! kernel portable across native, Wasm, and WASI targets.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// StructFS re-exports
// ---------------------------------------------------------------------------

pub use structfs_core_store::{
    self as structfs, Error as StoreError, Path, Reader, Record, Store, Value, Writer, path,
};

pub use ox_path::oxpath;

mod path_component;
pub use path_component::PathComponent;

pub mod snapshot;

pub mod backing;
pub use backing::StoreBacking;

pub mod log;

pub mod resume;
pub use resume::{ThreadResumeState, classify};

pub mod run;
pub use run::{
    ContextRef, ResolvedContext, accumulate_response, agent_event_to_json, complete, default_refs,
    deserialize_events, execute_tools, json_to_stream_event, read_model_config,
    record_tool_results, record_turn, resolve_refs, run_turn, stream_event_to_json, synthesize,
};

// ---------------------------------------------------------------------------
// Message types
// ---------------------------------------------------------------------------

/// A tool invocation requested by the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    /// Unique identifier for this tool use, used to correlate results.
    pub id: String,
    /// Name of the tool to invoke (must match a registered [`Tool::name`]).
    pub name: String,
    /// JSON arguments to pass to the tool.
    pub input: serde_json::Value,
}

/// A single block of content in an assistant message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    /// A text segment of the assistant's response.
    #[serde(rename = "text")]
    Text {
        /// The text content.
        text: String,
    },
    /// A tool invocation the assistant wants to execute.
    #[serde(rename = "tool_use")]
    ToolUse(ToolCall),
}

/// The result of executing a tool, sent back to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    /// The [`ToolCall::id`] this result corresponds to.
    pub tool_use_id: String,
    /// The tool's output (or error message).
    pub content: serde_json::Value,
}

/// A conversation message — user text, assistant response, or tool results.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role")]
pub enum Message {
    /// A user message with plain text content.
    #[serde(rename = "user")]
    User {
        /// The user's text.
        content: String,
    },
    /// An assistant response containing text and/or tool invocations.
    #[serde(rename = "assistant")]
    Assistant {
        /// Content blocks (text and/or tool use).
        content: Vec<ContentBlock>,
    },
    /// Tool execution results returned to the model.
    #[serde(rename = "tool_result")]
    ToolResult {
        /// One result per tool call.
        results: Vec<ToolResult>,
    },
}

// ---------------------------------------------------------------------------
// Completion request / response (Anthropic wire-ish format)
// ---------------------------------------------------------------------------

/// JSON Schema description of a tool, sent to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    /// The tool's unique name (e.g. `"get_weather"`).
    pub name: String,
    /// Human-readable description of what the tool does.
    pub description: String,
    /// JSON Schema object describing the tool's parameters.
    pub input_schema: serde_json::Value,
}

/// A fully-assembled completion request ready to send to a transport.
///
/// Typically synthesized by reading `path!("prompt")` from a [`Namespace`](crate::Store),
/// not constructed directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionRequest {
    /// Model identifier (e.g. `"claude-sonnet-4-20250514"`).
    pub model: String,
    /// Maximum tokens to generate.
    pub max_tokens: u32,
    /// System prompt.
    pub system: String,
    /// Conversation history in wire format.
    pub messages: Vec<serde_json::Value>,
    /// Available tools.
    pub tools: Vec<ToolSchema>,
    /// Whether to use streaming responses.
    #[serde(default)]
    pub stream: bool,
}

// ---------------------------------------------------------------------------
// Stream events (from the transport)
// ---------------------------------------------------------------------------

/// A single event from a streaming completion response.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    /// A chunk of text from the assistant.
    TextDelta(String),
    /// A new tool invocation has started.
    ToolUseStart {
        /// Unique ID for this tool use.
        id: String,
        /// Name of the tool.
        name: String,
    },
    /// A chunk of JSON input for the current tool invocation.
    ToolUseInputDelta(String),
    /// The model has finished its response.
    MessageStop,
    /// An error occurred during streaming.
    Error(String),
}

// ---------------------------------------------------------------------------
// Agent events (for observability subscribers)
// ---------------------------------------------------------------------------

/// High-level agent lifecycle events for observability subscribers.
///
/// Subscribe to these via `Agent::subscribe` (in `ox-core`) or the `emit`
/// callback on [`run_turn`].
#[derive(Debug, Clone)]
pub enum AgentEvent {
    /// A new completion round is starting.
    TurnStart,
    /// A chunk of assistant text was received.
    TextDelta(String),
    /// A tool is about to be executed.
    ToolCallStart {
        /// Name of the tool.
        name: String,
    },
    /// A tool has finished executing.
    ToolCallResult {
        /// Name of the tool.
        name: String,
        /// The tool's output.
        result: String,
    },
    /// The turn completed (no more tool calls).
    TurnEnd,
    /// An error occurred.
    Error(String),
}

// ---------------------------------------------------------------------------
// Model catalog
// ---------------------------------------------------------------------------

/// A model entry in a provider's catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    /// Model identifier (e.g. `"claude-sonnet-4-20250514"`).
    pub id: String,
    /// Human-readable name (e.g. `"Claude Sonnet 4"`).
    pub display_name: String,
}

// ---------------------------------------------------------------------------
// Serialization helpers (produce Anthropic Messages API format)
// ---------------------------------------------------------------------------

/// Serialize assistant content blocks into Anthropic Messages API wire format.
pub fn serialize_assistant_message(blocks: &[ContentBlock]) -> serde_json::Value {
    let content: Vec<serde_json::Value> = blocks
        .iter()
        .map(|b| match b {
            ContentBlock::Text { text } => serde_json::json!({
                "type": "text",
                "text": text,
            }),
            ContentBlock::ToolUse(tc) => serde_json::json!({
                "type": "tool_use",
                "id": tc.id,
                "name": tc.name,
                "input": tc.input,
            }),
        })
        .collect();

    serde_json::json!({
        "role": "assistant",
        "content": content,
    })
}

/// Serialize tool results into Anthropic Messages API wire format.
pub fn serialize_tool_results(results: &[ToolResult]) -> serde_json::Value {
    let content: Vec<serde_json::Value> = results
        .iter()
        .map(|r| {
            let content_str = match &r.content {
                serde_json::Value::String(s) => s.clone(),
                other => serde_json::to_string(other).unwrap_or_default(),
            };
            serde_json::json!({
                "type": "tool_result",
                "tool_use_id": r.tool_use_id,
                "content": content_str,
            })
        })
        .collect();

    serde_json::json!({
        "role": "user",
        "content": content,
    })
}
