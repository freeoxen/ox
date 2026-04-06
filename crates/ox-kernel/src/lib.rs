//! Core types, state machine, and agentic loop for the ox agent framework.
//!
//! `ox-kernel` provides the foundational building blocks for building LLM agents:
//!
//! - **Message types** — [`Message`], [`ContentBlock`], [`ToolCall`], [`ToolResult`]
//! - **Completion protocol** — [`CompletionRequest`], [`StreamEvent`], [`EventStream`]
//! - **Tool abstraction** — [`Tool`] trait, [`FnTool`], and [`ToolRegistry`]
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
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// StructFS re-exports
// ---------------------------------------------------------------------------

pub use structfs_core_store::{
    self as structfs, Error as StoreError, Path, Reader, Record, Store, Value, Writer, path,
};

pub mod snapshot;

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
    pub content: String,
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
/// callback on [`Kernel::run_turn`].
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
// Tool trait and registry
// ---------------------------------------------------------------------------

/// A tool the agent can invoke. Implement this trait to expose capabilities.
///
/// Most tools can be created directly with [`FnTool::new`]:
///
/// ```ignore
/// let echo = FnTool::new(
///     "echo",
///     "Echoes the input back",
///     serde_json::json!({
///         "type": "object",
///         "properties": { "text": { "type": "string" } },
///         "required": ["text"]
///     }),
///     |input| Ok(input["text"].as_str().unwrap_or("").to_string()),
/// );
/// ```
pub trait Tool: Send + Sync {
    /// A unique name for this tool (e.g. `"get_weather"`).
    fn name(&self) -> &str;
    /// A human-readable description of what the tool does.
    fn description(&self) -> &str;
    /// A JSON Schema object describing the tool's input parameters.
    fn parameters_schema(&self) -> serde_json::Value;
    /// Execute the tool with the given JSON input, returning a string result.
    fn execute(&self, input: serde_json::Value) -> Result<String, String>;
}

/// A closure-backed [`Tool`] implementation.
///
/// This is the canonical way to create tools. All tools — standard distribution,
/// completion delegates, Wasm components — are instances of `FnTool`.
pub struct FnTool {
    name: String,
    description: String,
    schema: serde_json::Value,
    run: Box<dyn Fn(serde_json::Value) -> Result<String, String> + Send + Sync>,
}

impl FnTool {
    /// Create a new tool from a closure.
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        schema: serde_json::Value,
        run: impl Fn(serde_json::Value) -> Result<String, String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            schema,
            run: Box::new(run),
        }
    }
}

impl Tool for FnTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        self.schema.clone()
    }

    fn execute(&self, input: serde_json::Value) -> Result<String, String> {
        (self.run)(input)
    }
}

/// Registry of named tools available to the agent.
///
/// Tools are registered by name and looked up during the agentic loop
/// when the model emits a [`ToolCall`].
pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    /// Register a tool. Replaces any existing tool with the same name.
    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    /// Look up a tool by name.
    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(|t| t.as_ref())
    }

    /// Collect [`ToolSchema`]s for all registered tools.
    pub fn schemas(&self) -> Vec<ToolSchema> {
        self.tools
            .values()
            .map(|t| ToolSchema {
                name: t.name().to_string(),
                description: t.description().to_string(),
                input_schema: t.parameters_schema(),
            })
            .collect()
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
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
/// [`run_turn`](Kernel::run_turn) composes all three in a loop for callers that
/// can provide a synchronous send function.
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
            .map_err(|e| e.to_string())?
            .ok_or("failed to read prompt from context")?;
        let prompt_json = match record {
            Record::Parsed(v) => structfs_serde_store::value_to_json(v),
            _ => return Err("expected parsed prompt record".into()),
        };
        serde_json::from_value(prompt_json).map_err(|e| e.to_string())
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

        let content = self.accumulate_response(events, emit).inspect_err(|_| {
            self.state = KernelState::Idle;
        })?;
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

        Ok(tool_calls)
    }

    // -----------------------------------------------------------------------
    // Composed loop (for callers with a synchronous send function)
    // -----------------------------------------------------------------------

    /// Run the full agentic loop: read prompt, send, accumulate, execute
    /// tools, write results, repeat until no tool calls remain.
    ///
    /// `send` is a synchronous function that takes a [`CompletionRequest`] and
    /// returns parsed [`StreamEvent`]s. For async callers (e.g. wasm), use the
    /// three-phase methods directly instead.
    pub fn run_turn(
        &mut self,
        context: &mut dyn Store,
        send: &dyn Fn(&CompletionRequest) -> Result<Vec<StreamEvent>, String>,
        tools: &ToolRegistry,
        emit: &mut dyn FnMut(AgentEvent),
    ) -> Result<Vec<ContentBlock>, String> {
        loop {
            let request = self.initiate_completion(context)?;
            let events = send(&request)?;
            let content = self.consume_events(events, emit)?;
            let tool_calls = self.complete_turn(context, &content)?;

            if tool_calls.is_empty() {
                emit(AgentEvent::TurnEnd);
                return Ok(content);
            }

            // Execute tools
            self.state = KernelState::Executing;
            let mut results = Vec::new();
            for tc in &tool_calls {
                emit(AgentEvent::ToolCallStart {
                    name: tc.name.clone(),
                });
                let result = match tools.get(&tc.name) {
                    Some(tool) => tool.execute(tc.input.clone()),
                    None => Err(format!("unknown tool: {}", tc.name)),
                };
                let result_str = match result {
                    Ok(r) => r,
                    Err(e) => format!("error: {e}"),
                };
                emit(AgentEvent::ToolCallResult {
                    name: tc.name.clone(),
                    result: result_str.clone(),
                });
                results.push(ToolResult {
                    tool_use_id: tc.id.clone(),
                    content: result_str,
                });
            }

            // Write tool results to history
            let results_json = serialize_tool_results(&results);
            let record = Record::parsed(structfs_serde_store::json_to_value(results_json));
            context
                .write(&path!("history/append"), record)
                .map_err(|e| e.to_string())?;

            self.state = KernelState::Idle;
        }
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
            serde_json::json!({
                "type": "tool_result",
                "tool_use_id": r.tool_use_id,
                "content": r.content,
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

    #[test]
    fn fn_tool_executes_closure() {
        let tool = FnTool::new(
            "echo",
            "Echoes the input",
            serde_json::json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"]
            }),
            |input| Ok(input["text"].as_str().unwrap_or("").to_string()),
        );

        assert_eq!(tool.name(), "echo");
        assert_eq!(tool.description(), "Echoes the input");
        assert_eq!(
            tool.execute(serde_json::json!({"text": "hello"})).unwrap(),
            "hello"
        );
    }

    #[test]
    fn fn_tool_in_registry() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(FnTool::new(
            "noop",
            "Does nothing",
            serde_json::json!({"type": "object"}),
            |_| Ok("ok".into()),
        )));

        assert!(registry.get("noop").is_some());
        assert_eq!(registry.schemas().len(), 1);
        assert_eq!(registry.schemas()[0].name, "noop");
    }
}
