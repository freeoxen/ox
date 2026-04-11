//! Core types, state machine, and agentic loop for the ox agent framework.
//!
//! `ox-kernel` provides the foundational building blocks for building LLM agents:
//!
//! - **Message types** — [`Message`], [`ContentBlock`], [`ToolCall`], [`ToolResult`]
//! - **Completion protocol** — [`CompletionRequest`], [`StreamEvent`], [`EventStream`]
//! - **State machine** — [`Kernel`] drives the agentic loop via three composable
//!   phases: [`initiate_completion`](Kernel::initiate_completion),
//!   [`consume_events`](Kernel::consume_events), and
//!   [`complete_turn`](Kernel::complete_turn)
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

pub mod run;
pub use run::{
    accumulate_response, agent_event_to_json, deserialize_events, execute_tools,
    json_to_stream_event, record_tool_results, record_turn, run_turn, stream_event_to_json,
    synthesize,
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
/// callback on [`Kernel::consume_events`].
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
// Kernel state machine
// ---------------------------------------------------------------------------

/// The kernel's current phase in the agentic loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelState {
    /// Ready to start a new turn.
    Idle,
    /// Receiving a streaming response from the transport.
    Streaming,
    /// Executing tool calls.
    Executing,
}

/// The agentic loop state machine.
///
/// Exposes three composable phases — [`initiate_completion`](Kernel::initiate_completion),
/// [`consume_events`](Kernel::consume_events), and [`complete_turn`](Kernel::complete_turn) —
/// so the caller controls the transport (sync fetch, async fetch, mock, etc.).
///
pub struct Kernel {
    state: KernelState,
    model: String,
}

impl Kernel {
    /// Create a new kernel for the given model.
    pub fn new(model: String) -> Self {
        Self {
            state: KernelState::Idle,
            model,
        }
    }

    /// The kernel's current state.
    pub fn state(&self) -> KernelState {
        self.state
    }

    /// The model identifier this kernel was created with.
    pub fn model(&self) -> &str {
        &self.model
    }

    // -----------------------------------------------------------------------
    // Three-phase API
    // -----------------------------------------------------------------------

    /// Phase 1: Read the prompt from the context namespace and return a
    /// ready-to-send [`CompletionRequest`].
    ///
    /// The caller is responsible for sending this request to an LLM (sync or
    /// async) and parsing the response into [`StreamEvent`]s.
    pub fn initiate_completion(
        &mut self,
        context: &mut dyn Store,
    ) -> Result<CompletionRequest, String> {
        assert_eq!(self.state, KernelState::Idle, "kernel must be idle");

        let record = context
            .read(&path!("prompt"))
            .map_err(|e| {
                tracing::error!(error = %e, "failed to read prompt from context");
                e.to_string()
            })?
            .ok_or("failed to read prompt from context")?;
        let prompt_json = match record {
            Record::Parsed(v) => structfs_serde_store::value_to_json(v),
            _ => return Err("expected parsed prompt record".into()),
        };
        let request: CompletionRequest =
            serde_json::from_value(prompt_json).map_err(|e| e.to_string())?;
        tracing::debug!(model = %request.model, "initiating completion");
        Ok(request)
    }

    /// Phase 2: Accumulate pre-parsed [`StreamEvent`]s into content blocks.
    ///
    /// Call this after obtaining events from an LLM response (however
    /// transported). Emits [`AgentEvent`]s for observability.
    pub fn consume_events(
        &mut self,
        events: Vec<StreamEvent>,
        emit: &mut dyn FnMut(AgentEvent),
    ) -> Result<Vec<ContentBlock>, String> {
        assert_eq!(self.state, KernelState::Idle, "kernel must be idle");

        self.state = KernelState::Streaming;
        emit(AgentEvent::TurnStart);

        let content = self.accumulate_response(events, emit).inspect_err(|e| {
            tracing::error!(error = %e, "stream accumulation failed");
            self.state = KernelState::Idle;
        })?;
        let tool_count = content
            .iter()
            .filter(|b| matches!(b, ContentBlock::ToolUse(_)))
            .count();
        let event_count = content.len();
        tracing::debug!(event_count, tool_count, "consumed events");
        self.state = KernelState::Idle;
        Ok(content)
    }

    /// Phase 3: Write the assistant message to history and extract tool calls.
    ///
    /// Returns the tool calls the model requested. If empty, the turn is done.
    /// The caller is responsible for executing tools and writing results to
    /// `history/append` before looping back to [`initiate_completion`](Self::initiate_completion).
    pub fn complete_turn(
        &mut self,
        context: &mut dyn Store,
        content: &[ContentBlock],
    ) -> Result<Vec<ToolCall>, String> {
        // Write assistant message to history
        let assistant_json = serialize_assistant_message(content);
        let record = Record::parsed(structfs_serde_store::json_to_value(assistant_json));
        context
            .write(&path!("history/append"), record)
            .map_err(|e| e.to_string())?;

        // Extract tool calls
        let tool_calls: Vec<ToolCall> = content
            .iter()
            .filter_map(|block| {
                if let ContentBlock::ToolUse(tc) = block {
                    Some(tc.clone())
                } else {
                    None
                }
            })
            .collect();

        if !tool_calls.is_empty() {
            let names: Vec<&str> = tool_calls.iter().map(|tc| tc.name.as_str()).collect();
            tracing::debug!(tools = ?names, "tool calls extracted");
        }

        Ok(tool_calls)
    }

    /// Accumulate stream events into content blocks.
    fn accumulate_response(
        &self,
        events: Vec<StreamEvent>,
        emit: &mut dyn FnMut(AgentEvent),
    ) -> Result<Vec<ContentBlock>, String> {
        let mut blocks: Vec<ContentBlock> = Vec::new();
        let mut current_text = String::new();
        let mut current_tool: Option<(String, String, String)> = None; // (id, name, input_json)

        for event in events {
            match event {
                StreamEvent::TextDelta(text) => {
                    // Flush any pending tool
                    if let Some((id, name, input_json)) = current_tool.take() {
                        let input: serde_json::Value =
                            serde_json::from_str(&input_json).unwrap_or(serde_json::Value::Null);
                        blocks.push(ContentBlock::ToolUse(ToolCall { id, name, input }));
                    }
                    current_text.push_str(&text);
                    emit(AgentEvent::TextDelta(text));
                }
                StreamEvent::ToolUseStart { id, name } => {
                    // Flush any pending text
                    if !current_text.is_empty() {
                        blocks.push(ContentBlock::Text {
                            text: std::mem::take(&mut current_text),
                        });
                    }
                    // Flush any prior tool
                    if let Some((prev_id, prev_name, input_json)) = current_tool.take() {
                        let input: serde_json::Value =
                            serde_json::from_str(&input_json).unwrap_or(serde_json::Value::Null);
                        blocks.push(ContentBlock::ToolUse(ToolCall {
                            id: prev_id,
                            name: prev_name,
                            input,
                        }));
                    }
                    current_tool = Some((id, name, String::new()));
                }
                StreamEvent::ToolUseInputDelta(delta) => {
                    if let Some((_, _, ref mut input_json)) = current_tool {
                        input_json.push_str(&delta);
                    }
                }
                StreamEvent::MessageStop => {
                    break;
                }
                StreamEvent::Error(e) => {
                    self.flush_pending(&mut blocks, &mut current_text, &mut current_tool);
                    emit(AgentEvent::Error(e.clone()));
                    return Err(e);
                }
            }
        }

        // Flush remaining
        self.flush_pending(&mut blocks, &mut current_text, &mut current_tool);

        Ok(blocks)
    }

    fn flush_pending(
        &self,
        blocks: &mut Vec<ContentBlock>,
        current_text: &mut String,
        current_tool: &mut Option<(String, String, String)>,
    ) {
        if !current_text.is_empty() {
            blocks.push(ContentBlock::Text {
                text: std::mem::take(current_text),
            });
        }
        if let Some((id, name, input_json)) = current_tool.take() {
            let input: serde_json::Value =
                serde_json::from_str(&input_json).unwrap_or(serde_json::Value::Null);
            blocks.push(ContentBlock::ToolUse(ToolCall { id, name, input }));
        }
    }
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

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    // -----------------------------------------------------------------------
    // Mock store for kernel tests
    // -----------------------------------------------------------------------

    /// A simple in-memory store that responds to `prompt` reads with a
    /// pre-configured CompletionRequest and captures `history/append` writes.
    struct MockStore {
        prompt: Option<Value>,
        appended: Vec<Value>,
        data: BTreeMap<String, Value>,
    }

    impl MockStore {
        /// Create a store pre-loaded with a CompletionRequest at `prompt`.
        fn with_prompt(request: &CompletionRequest) -> Self {
            let json = serde_json::to_value(request).unwrap();
            let value = structfs_serde_store::json_to_value(json);
            Self {
                prompt: Some(value),
                appended: Vec::new(),
                data: BTreeMap::new(),
            }
        }

        /// Create a store with no prompt (for error-path tests).
        fn empty() -> Self {
            Self {
                prompt: None,
                appended: Vec::new(),
                data: BTreeMap::new(),
            }
        }
    }

    impl Reader for MockStore {
        fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
            if from == &path!("prompt") {
                return Ok(self.prompt.as_ref().map(|v| Record::parsed(v.clone())));
            }
            let key = from.to_string();
            Ok(self.data.get(&key).map(|v| Record::parsed(v.clone())))
        }
    }

    impl Writer for MockStore {
        fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
            if to == &path!("history/append") {
                if let Record::Parsed(v) = &data {
                    self.appended.push(v.clone());
                }
                return Ok(to.clone());
            }
            let value = match data {
                Record::Parsed(v) => v,
                _ => {
                    return Err(StoreError::store("mock", "write", "expected parsed record"));
                }
            };
            self.data.insert(to.to_string(), value);
            Ok(to.clone())
        }
    }

    // -----------------------------------------------------------------------
    // Test helpers
    // -----------------------------------------------------------------------

    fn make_request() -> CompletionRequest {
        CompletionRequest {
            model: "test-model".into(),
            max_tokens: 1024,
            system: "You are a test agent.".into(),
            messages: vec![serde_json::json!({
                "role": "user",
                "content": "Hello",
            })],
            tools: vec![],
            stream: true,
        }
    }

    fn make_request_with_tools() -> CompletionRequest {
        CompletionRequest {
            model: "test-model".into(),
            max_tokens: 1024,
            system: "You are a test agent.".into(),
            messages: vec![serde_json::json!({
                "role": "user",
                "content": "What is the weather?",
            })],
            tools: vec![ToolSchema {
                name: "get_weather".into(),
                description: "Gets the weather".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": { "city": { "type": "string" } },
                    "required": ["city"]
                }),
            }],
            stream: true,
        }
    }

    // -----------------------------------------------------------------------
    // Kernel constructor and state tests
    // -----------------------------------------------------------------------

    #[test]
    fn kernel_new_is_idle() {
        let kernel = Kernel::new("test-model".into());
        assert_eq!(kernel.state(), KernelState::Idle);
        assert_eq!(kernel.model(), "test-model");
    }

    // -----------------------------------------------------------------------
    // Phase 1: initiate_completion
    // -----------------------------------------------------------------------

    #[test]
    fn initiate_completion_happy_path() {
        let mut kernel = Kernel::new("test-model".into());
        let request = make_request();
        let mut store = MockStore::with_prompt(&request);

        let result = kernel.initiate_completion(&mut store).unwrap();
        assert_eq!(result.model, "test-model");
        assert_eq!(result.max_tokens, 1024);
        assert_eq!(result.system, "You are a test agent.");
        assert_eq!(result.messages.len(), 1);
        assert!(result.stream);
    }

    #[test]
    fn initiate_completion_preserves_tools() {
        let mut kernel = Kernel::new("test-model".into());
        let request = make_request_with_tools();
        let mut store = MockStore::with_prompt(&request);

        let result = kernel.initiate_completion(&mut store).unwrap();
        assert_eq!(result.tools.len(), 1);
        assert_eq!(result.tools[0].name, "get_weather");
    }

    #[test]
    fn initiate_completion_no_prompt_errors() {
        let mut kernel = Kernel::new("test-model".into());
        let mut store = MockStore::empty();

        let result = kernel.initiate_completion(&mut store);
        assert!(result.is_err());
        // The error message should indicate the prompt couldn't be read
        let err = result.unwrap_err();
        assert!(
            err.contains("prompt") || err.contains("failed"),
            "error should mention prompt: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // Phase 2: consume_events
    // -----------------------------------------------------------------------

    #[test]
    fn consume_events_text_only() {
        let mut kernel = Kernel::new("test-model".into());
        let events = vec![
            StreamEvent::TextDelta("Hello, ".into()),
            StreamEvent::TextDelta("world!".into()),
            StreamEvent::MessageStop,
        ];
        let mut agent_events = Vec::new();
        let content = kernel
            .consume_events(events, &mut |e| agent_events.push(e))
            .unwrap();

        assert_eq!(content.len(), 1);
        match &content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "Hello, world!"),
            _ => panic!("expected text block"),
        }
    }

    #[test]
    fn consume_events_emits_turn_start() {
        let mut kernel = Kernel::new("test-model".into());
        let events = vec![
            StreamEvent::TextDelta("hi".into()),
            StreamEvent::MessageStop,
        ];
        let mut agent_events = Vec::new();
        kernel
            .consume_events(events, &mut |e| agent_events.push(e))
            .unwrap();

        assert!(matches!(agent_events[0], AgentEvent::TurnStart));
    }

    #[test]
    fn consume_events_emits_text_deltas() {
        let mut kernel = Kernel::new("test-model".into());
        let events = vec![
            StreamEvent::TextDelta("chunk1".into()),
            StreamEvent::TextDelta("chunk2".into()),
            StreamEvent::MessageStop,
        ];
        let mut agent_events = Vec::new();
        kernel
            .consume_events(events, &mut |e| agent_events.push(e))
            .unwrap();

        // TurnStart, TextDelta("chunk1"), TextDelta("chunk2")
        assert!(matches!(&agent_events[1], AgentEvent::TextDelta(t) if t == "chunk1"));
        assert!(matches!(&agent_events[2], AgentEvent::TextDelta(t) if t == "chunk2"));
    }

    #[test]
    fn consume_events_tool_call() {
        let mut kernel = Kernel::new("test-model".into());
        let events = vec![
            StreamEvent::ToolUseStart {
                id: "call_1".into(),
                name: "get_weather".into(),
            },
            StreamEvent::ToolUseInputDelta(r#"{"city":"#.into()),
            StreamEvent::ToolUseInputDelta(r#""NYC"}"#.into()),
            StreamEvent::MessageStop,
        ];
        let mut agent_events = Vec::new();
        let content = kernel
            .consume_events(events, &mut |e| agent_events.push(e))
            .unwrap();

        assert_eq!(content.len(), 1);
        match &content[0] {
            ContentBlock::ToolUse(tc) => {
                assert_eq!(tc.id, "call_1");
                assert_eq!(tc.name, "get_weather");
                assert_eq!(tc.input, serde_json::json!({"city": "NYC"}));
            }
            _ => panic!("expected tool use block"),
        }
    }

    #[test]
    fn consume_events_mixed_text_and_tool() {
        let mut kernel = Kernel::new("test-model".into());
        let events = vec![
            StreamEvent::TextDelta("Let me check the weather.".into()),
            StreamEvent::ToolUseStart {
                id: "call_1".into(),
                name: "get_weather".into(),
            },
            StreamEvent::ToolUseInputDelta(r#"{"city":"NYC"}"#.into()),
            StreamEvent::MessageStop,
        ];
        let mut agent_events = Vec::new();
        let content = kernel
            .consume_events(events, &mut |e| agent_events.push(e))
            .unwrap();

        assert_eq!(content.len(), 2);
        match &content[0] {
            ContentBlock::Text { text } => {
                assert_eq!(text, "Let me check the weather.");
            }
            _ => panic!("expected text block first"),
        }
        match &content[1] {
            ContentBlock::ToolUse(tc) => {
                assert_eq!(tc.name, "get_weather");
            }
            _ => panic!("expected tool use block second"),
        }
    }

    #[test]
    fn consume_events_multiple_tool_calls() {
        let mut kernel = Kernel::new("test-model".into());
        let events = vec![
            StreamEvent::ToolUseStart {
                id: "call_1".into(),
                name: "tool_a".into(),
            },
            StreamEvent::ToolUseInputDelta(r#"{}"#.into()),
            StreamEvent::ToolUseStart {
                id: "call_2".into(),
                name: "tool_b".into(),
            },
            StreamEvent::ToolUseInputDelta(r#"{"x": 1}"#.into()),
            StreamEvent::MessageStop,
        ];
        let mut agent_events = Vec::new();
        let content = kernel
            .consume_events(events, &mut |e| agent_events.push(e))
            .unwrap();

        assert_eq!(content.len(), 2);
        match &content[0] {
            ContentBlock::ToolUse(tc) => {
                assert_eq!(tc.id, "call_1");
                assert_eq!(tc.name, "tool_a");
            }
            _ => panic!("expected first tool use"),
        }
        match &content[1] {
            ContentBlock::ToolUse(tc) => {
                assert_eq!(tc.id, "call_2");
                assert_eq!(tc.name, "tool_b");
                assert_eq!(tc.input, serde_json::json!({"x": 1}));
            }
            _ => panic!("expected second tool use"),
        }
    }

    #[test]
    fn consume_events_empty() {
        let mut kernel = Kernel::new("test-model".into());
        let events = vec![];
        let mut agent_events = Vec::new();
        let content = kernel
            .consume_events(events, &mut |e| agent_events.push(e))
            .unwrap();

        assert!(content.is_empty());
    }

    #[test]
    fn consume_events_message_stop_only() {
        let mut kernel = Kernel::new("test-model".into());
        let events = vec![StreamEvent::MessageStop];
        let mut agent_events = Vec::new();
        let content = kernel
            .consume_events(events, &mut |e| agent_events.push(e))
            .unwrap();

        assert!(content.is_empty());
    }

    #[test]
    fn consume_events_error_returns_err() {
        let mut kernel = Kernel::new("test-model".into());
        let events = vec![
            StreamEvent::TextDelta("partial".into()),
            StreamEvent::Error("server error".into()),
        ];
        let mut agent_events = Vec::new();
        let result = kernel.consume_events(events, &mut |e| agent_events.push(e));

        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), "server error");
        // Kernel should be back to Idle after error
        assert_eq!(kernel.state(), KernelState::Idle);
    }

    #[test]
    fn consume_events_error_emits_error_event() {
        let mut kernel = Kernel::new("test-model".into());
        let events = vec![StreamEvent::Error("boom".into())];
        let mut agent_events = Vec::new();
        let _ = kernel.consume_events(events, &mut |e| agent_events.push(e));

        // Should have TurnStart then Error
        assert!(
            agent_events
                .iter()
                .any(|e| matches!(e, AgentEvent::Error(msg) if msg == "boom"))
        );
    }

    #[test]
    fn consume_events_error_flushes_partial_text() {
        let mut kernel = Kernel::new("test-model".into());
        // Text before error should be flushed into content blocks
        // (even though the result is Err, the flush happens internally)
        let events = vec![
            StreamEvent::TextDelta("partial ".into()),
            StreamEvent::TextDelta("text".into()),
            StreamEvent::Error("fail".into()),
        ];
        let mut agent_events = Vec::new();
        let result = kernel.consume_events(events, &mut |e| agent_events.push(e));
        assert!(result.is_err());
    }

    #[test]
    fn consume_events_tool_with_invalid_json_input() {
        let mut kernel = Kernel::new("test-model".into());
        let events = vec![
            StreamEvent::ToolUseStart {
                id: "call_1".into(),
                name: "bad_tool".into(),
            },
            StreamEvent::ToolUseInputDelta("not valid json".into()),
            StreamEvent::MessageStop,
        ];
        let mut agent_events = Vec::new();
        let content = kernel
            .consume_events(events, &mut |e| agent_events.push(e))
            .unwrap();

        // Invalid JSON input should fall back to Null
        assert_eq!(content.len(), 1);
        match &content[0] {
            ContentBlock::ToolUse(tc) => {
                assert_eq!(tc.input, serde_json::Value::Null);
            }
            _ => panic!("expected tool use block"),
        }
    }

    #[test]
    fn consume_events_tool_with_empty_input() {
        let mut kernel = Kernel::new("test-model".into());
        let events = vec![
            StreamEvent::ToolUseStart {
                id: "call_1".into(),
                name: "no_args_tool".into(),
            },
            // No ToolUseInputDelta events
            StreamEvent::MessageStop,
        ];
        let mut agent_events = Vec::new();
        let content = kernel
            .consume_events(events, &mut |e| agent_events.push(e))
            .unwrap();

        assert_eq!(content.len(), 1);
        match &content[0] {
            ContentBlock::ToolUse(tc) => {
                // Empty string parses to Null
                assert_eq!(tc.input, serde_json::Value::Null);
            }
            _ => panic!("expected tool use block"),
        }
    }

    #[test]
    fn consume_events_text_after_tool_via_delta() {
        // Regression: TextDelta after a tool should flush the tool first
        let mut kernel = Kernel::new("test-model".into());
        let events = vec![
            StreamEvent::ToolUseStart {
                id: "call_1".into(),
                name: "tool_a".into(),
            },
            StreamEvent::ToolUseInputDelta(r#"{}"#.into()),
            StreamEvent::TextDelta("Some trailing text".into()),
            StreamEvent::MessageStop,
        ];
        let mut agent_events = Vec::new();
        let content = kernel
            .consume_events(events, &mut |e| agent_events.push(e))
            .unwrap();

        assert_eq!(content.len(), 2);
        assert!(matches!(&content[0], ContentBlock::ToolUse(_)));
        match &content[1] {
            ContentBlock::Text { text } => assert_eq!(text, "Some trailing text"),
            _ => panic!("expected text block after tool"),
        }
    }

    #[test]
    fn consume_events_resets_to_idle() {
        let mut kernel = Kernel::new("test-model".into());
        let events = vec![
            StreamEvent::TextDelta("done".into()),
            StreamEvent::MessageStop,
        ];
        let mut noop = |_: AgentEvent| {};
        kernel.consume_events(events, &mut noop).unwrap();
        assert_eq!(kernel.state(), KernelState::Idle);
    }

    #[test]
    fn consume_events_can_be_called_twice() {
        let mut kernel = Kernel::new("test-model".into());
        let mut noop = |_: AgentEvent| {};

        let events1 = vec![
            StreamEvent::TextDelta("first".into()),
            StreamEvent::MessageStop,
        ];
        let content1 = kernel.consume_events(events1, &mut noop).unwrap();
        assert_eq!(content1.len(), 1);

        let events2 = vec![
            StreamEvent::TextDelta("second".into()),
            StreamEvent::MessageStop,
        ];
        let content2 = kernel.consume_events(events2, &mut noop).unwrap();
        assert_eq!(content2.len(), 1);
        match &content2[0] {
            ContentBlock::Text { text } => assert_eq!(text, "second"),
            _ => panic!("expected text"),
        }
    }

    // -----------------------------------------------------------------------
    // Phase 3: complete_turn
    // -----------------------------------------------------------------------

    #[test]
    fn complete_turn_no_tool_calls() {
        let mut kernel = Kernel::new("test-model".into());
        let request = make_request();
        let mut store = MockStore::with_prompt(&request);

        let content = vec![ContentBlock::Text {
            text: "Hello!".into(),
        }];
        let tool_calls = kernel.complete_turn(&mut store, &content).unwrap();

        assert!(tool_calls.is_empty());
    }

    #[test]
    fn complete_turn_writes_assistant_message_to_history() {
        let mut kernel = Kernel::new("test-model".into());
        let request = make_request();
        let mut store = MockStore::with_prompt(&request);

        let content = vec![ContentBlock::Text {
            text: "Hi there!".into(),
        }];
        kernel.complete_turn(&mut store, &content).unwrap();

        // Verify something was appended to history
        assert_eq!(store.appended.len(), 1);

        // Verify the appended value is a valid assistant message
        let json = structfs_serde_store::value_to_json(store.appended[0].clone());
        assert_eq!(json["role"], "assistant");
        assert!(json["content"].is_array());
        assert_eq!(json["content"][0]["type"], "text");
        assert_eq!(json["content"][0]["text"], "Hi there!");
    }

    #[test]
    fn complete_turn_with_tool_calls() {
        let mut kernel = Kernel::new("test-model".into());
        let request = make_request_with_tools();
        let mut store = MockStore::with_prompt(&request);

        let content = vec![
            ContentBlock::Text {
                text: "Let me check.".into(),
            },
            ContentBlock::ToolUse(ToolCall {
                id: "call_1".into(),
                name: "get_weather".into(),
                input: serde_json::json!({"city": "NYC"}),
            }),
        ];
        let tool_calls = kernel.complete_turn(&mut store, &content).unwrap();

        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id, "call_1");
        assert_eq!(tool_calls[0].name, "get_weather");
        assert_eq!(tool_calls[0].input, serde_json::json!({"city": "NYC"}));
    }

    #[test]
    fn complete_turn_multiple_tool_calls() {
        let mut kernel = Kernel::new("test-model".into());
        let request = make_request();
        let mut store = MockStore::with_prompt(&request);

        let content = vec![
            ContentBlock::ToolUse(ToolCall {
                id: "call_1".into(),
                name: "tool_a".into(),
                input: serde_json::json!({}),
            }),
            ContentBlock::ToolUse(ToolCall {
                id: "call_2".into(),
                name: "tool_b".into(),
                input: serde_json::json!({"x": 42}),
            }),
        ];
        let tool_calls = kernel.complete_turn(&mut store, &content).unwrap();

        assert_eq!(tool_calls.len(), 2);
        assert_eq!(tool_calls[0].name, "tool_a");
        assert_eq!(tool_calls[1].name, "tool_b");
    }

    #[test]
    fn complete_turn_writes_tool_use_to_history() {
        let mut kernel = Kernel::new("test-model".into());
        let request = make_request();
        let mut store = MockStore::with_prompt(&request);

        let content = vec![ContentBlock::ToolUse(ToolCall {
            id: "call_1".into(),
            name: "echo".into(),
            input: serde_json::json!({"text": "hi"}),
        })];
        kernel.complete_turn(&mut store, &content).unwrap();

        // The assistant message should include the tool_use block
        let json = structfs_serde_store::value_to_json(store.appended[0].clone());
        assert_eq!(json["role"], "assistant");
        assert_eq!(json["content"][0]["type"], "tool_use");
        assert_eq!(json["content"][0]["name"], "echo");
        assert_eq!(json["content"][0]["id"], "call_1");
    }

    #[test]
    fn complete_turn_empty_content() {
        let mut kernel = Kernel::new("test-model".into());
        let request = make_request();
        let mut store = MockStore::with_prompt(&request);

        let content: Vec<ContentBlock> = vec![];
        let tool_calls = kernel.complete_turn(&mut store, &content).unwrap();

        assert!(tool_calls.is_empty());
        // Should still write an assistant message (with empty content)
        assert_eq!(store.appended.len(), 1);
        let json = structfs_serde_store::value_to_json(store.appended[0].clone());
        assert_eq!(json["role"], "assistant");
        assert!(json["content"].as_array().unwrap().is_empty());
    }

    // -----------------------------------------------------------------------
    // Full loop: initiate -> consume -> complete
    // -----------------------------------------------------------------------

    #[test]
    fn full_loop_text_response() {
        let mut kernel = Kernel::new("test-model".into());
        let request = make_request();
        let mut store = MockStore::with_prompt(&request);

        // Phase 1: initiate
        let req = kernel.initiate_completion(&mut store).unwrap();
        assert_eq!(req.model, "test-model");

        // Phase 2: consume (simulate LLM response)
        let events = vec![
            StreamEvent::TextDelta("I'm doing great!".into()),
            StreamEvent::MessageStop,
        ];
        let mut agent_events = Vec::new();
        let content = kernel
            .consume_events(events, &mut |e| agent_events.push(e))
            .unwrap();

        // Phase 3: complete
        let tool_calls = kernel.complete_turn(&mut store, &content).unwrap();
        assert!(tool_calls.is_empty());

        // Verify history was updated
        assert_eq!(store.appended.len(), 1);
        let json = structfs_serde_store::value_to_json(store.appended[0].clone());
        assert_eq!(json["role"], "assistant");
        assert_eq!(json["content"][0]["text"], "I'm doing great!");

        // Verify agent events
        assert!(matches!(agent_events[0], AgentEvent::TurnStart));
        assert!(matches!(&agent_events[1], AgentEvent::TextDelta(t) if t == "I'm doing great!"));
    }

    #[test]
    fn full_loop_tool_call_response() {
        let mut kernel = Kernel::new("test-model".into());
        let request = make_request_with_tools();
        let mut store = MockStore::with_prompt(&request);

        // Phase 1
        let _req = kernel.initiate_completion(&mut store).unwrap();

        // Phase 2: LLM wants to use a tool
        let events = vec![
            StreamEvent::TextDelta("Let me look that up.".into()),
            StreamEvent::ToolUseStart {
                id: "toolu_01".into(),
                name: "get_weather".into(),
            },
            StreamEvent::ToolUseInputDelta(r#"{"city":"NYC"}"#.into()),
            StreamEvent::MessageStop,
        ];
        let mut agent_events = Vec::new();
        let content = kernel
            .consume_events(events, &mut |e| agent_events.push(e))
            .unwrap();

        assert_eq!(content.len(), 2);

        // Phase 3
        let tool_calls = kernel.complete_turn(&mut store, &content).unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].name, "get_weather");

        // Verify history has the assistant message
        assert_eq!(store.appended.len(), 1);
        let json = structfs_serde_store::value_to_json(store.appended[0].clone());
        assert_eq!(json["content"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn full_loop_can_repeat_phases() {
        // Simulate: initiate -> consume -> complete (tool call) ->
        //           write tool results -> initiate -> consume -> complete (done)
        let mut kernel = Kernel::new("test-model".into());
        let request = make_request_with_tools();
        let mut store = MockStore::with_prompt(&request);
        let mut noop = |_: AgentEvent| {};

        // Turn 1: tool call
        let _req = kernel.initiate_completion(&mut store).unwrap();
        let events = vec![
            StreamEvent::ToolUseStart {
                id: "toolu_01".into(),
                name: "get_weather".into(),
            },
            StreamEvent::ToolUseInputDelta(r#"{"city":"NYC"}"#.into()),
            StreamEvent::MessageStop,
        ];
        let content = kernel.consume_events(events, &mut noop).unwrap();
        let tool_calls = kernel.complete_turn(&mut store, &content).unwrap();
        assert_eq!(tool_calls.len(), 1);

        // Write tool results (simulating what the caller loop does)
        let results = vec![ToolResult {
            tool_use_id: "toolu_01".into(),
            content: serde_json::Value::String("Sunny, 72F".into()),
        }];
        let results_json = serialize_tool_results(&results);
        let record = Record::parsed(structfs_serde_store::json_to_value(results_json));
        store.write(&path!("history/append"), record).unwrap();

        // Turn 2: final text response
        let _req2 = kernel.initiate_completion(&mut store).unwrap();
        let events2 = vec![
            StreamEvent::TextDelta("The weather in NYC is sunny, 72F.".into()),
            StreamEvent::MessageStop,
        ];
        let content2 = kernel.consume_events(events2, &mut noop).unwrap();
        let tool_calls2 = kernel.complete_turn(&mut store, &content2).unwrap();
        assert!(tool_calls2.is_empty());

        // 3 appends: assistant (tool call), tool results, assistant (final)
        assert_eq!(store.appended.len(), 3);
    }

    // -----------------------------------------------------------------------
    // Serialization helpers
    // -----------------------------------------------------------------------

    #[test]
    fn serialize_assistant_message_text_only() {
        let blocks = vec![ContentBlock::Text {
            text: "Hello!".into(),
        }];
        let json = serialize_assistant_message(&blocks);
        assert_eq!(json["role"], "assistant");
        assert_eq!(json["content"][0]["type"], "text");
        assert_eq!(json["content"][0]["text"], "Hello!");
    }

    #[test]
    fn serialize_assistant_message_tool_use() {
        let blocks = vec![ContentBlock::ToolUse(ToolCall {
            id: "call_1".into(),
            name: "echo".into(),
            input: serde_json::json!({"text": "hi"}),
        })];
        let json = serialize_assistant_message(&blocks);
        assert_eq!(json["role"], "assistant");
        assert_eq!(json["content"][0]["type"], "tool_use");
        assert_eq!(json["content"][0]["id"], "call_1");
        assert_eq!(json["content"][0]["name"], "echo");
        assert_eq!(json["content"][0]["input"]["text"], "hi");
    }

    #[test]
    fn serialize_assistant_message_mixed() {
        let blocks = vec![
            ContentBlock::Text {
                text: "Here:".into(),
            },
            ContentBlock::ToolUse(ToolCall {
                id: "c1".into(),
                name: "t1".into(),
                input: serde_json::json!({}),
            }),
        ];
        let json = serialize_assistant_message(&blocks);
        let content = json["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "tool_use");
    }

    #[test]
    fn serialize_assistant_message_empty() {
        let blocks: Vec<ContentBlock> = vec![];
        let json = serialize_assistant_message(&blocks);
        assert_eq!(json["role"], "assistant");
        assert!(json["content"].as_array().unwrap().is_empty());
    }

    #[test]
    fn serialize_tool_results_single() {
        let results = vec![ToolResult {
            tool_use_id: "call_1".into(),
            content: serde_json::Value::String("result text".into()),
        }];
        let json = serialize_tool_results(&results);
        assert_eq!(json["role"], "user");
        assert_eq!(json["content"][0]["type"], "tool_result");
        assert_eq!(json["content"][0]["tool_use_id"], "call_1");
        assert_eq!(json["content"][0]["content"], "result text");
    }

    #[test]
    fn serialize_tool_results_multiple() {
        let results = vec![
            ToolResult {
                tool_use_id: "c1".into(),
                content: serde_json::Value::String("r1".into()),
            },
            ToolResult {
                tool_use_id: "c2".into(),
                content: serde_json::Value::String("r2".into()),
            },
        ];
        let json = serialize_tool_results(&results);
        let content = json["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["tool_use_id"], "c1");
        assert_eq!(content[1]["tool_use_id"], "c2");
    }

    #[test]
    fn serialize_tool_results_empty() {
        let results: Vec<ToolResult> = vec![];
        let json = serialize_tool_results(&results);
        assert_eq!(json["role"], "user");
        assert!(json["content"].as_array().unwrap().is_empty());
    }
}
