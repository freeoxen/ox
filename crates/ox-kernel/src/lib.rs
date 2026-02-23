use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Message types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub input: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse(ToolCall),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub tool_use_id: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role")]
pub enum Message {
    #[serde(rename = "user")]
    User { content: String },
    #[serde(rename = "assistant")]
    Assistant { content: Vec<ContentBlock> },
    #[serde(rename = "tool_result")]
    ToolResult { results: Vec<ToolResult> },
}

// ---------------------------------------------------------------------------
// Completion request / response (Anthropic wire-ish format)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionRequest {
    pub model: String,
    pub max_tokens: u32,
    pub system: String,
    pub messages: Vec<serde_json::Value>,
    pub tools: Vec<ToolSchema>,
    #[serde(default)]
    pub stream: bool,
}

// ---------------------------------------------------------------------------
// Stream events (from the transport)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum StreamEvent {
    TextDelta(String),
    ToolUseStart { id: String, name: String },
    ToolUseInputDelta(String),
    MessageStop,
    Error(String),
}

// ---------------------------------------------------------------------------
// Agent events (for observability subscribers)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum AgentEvent {
    TurnStart,
    TextDelta(String),
    ToolCallStart { name: String },
    ToolCallResult { name: String, result: String },
    TurnEnd,
    Error(String),
}

// ---------------------------------------------------------------------------
// Tool trait and registry
// ---------------------------------------------------------------------------

pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> serde_json::Value;
    fn execute(&self, input: serde_json::Value) -> Result<String, String>;
}

pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
        }
    }

    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(|t| t.as_ref())
    }

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
// Transport trait
// ---------------------------------------------------------------------------

/// A transport sends a completion request and returns stream events.
///
/// This is async-agnostic: the caller drives the stream by calling
/// `next_event()` repeatedly until it returns `None`.
pub trait Transport {
    type Stream: EventStream;

    fn send(&self, request: CompletionRequest) -> Result<Self::Stream, String>;
}

pub trait EventStream {
    fn next_event(&mut self) -> Option<StreamEvent>;
}

// ---------------------------------------------------------------------------
// Kernel state machine
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KernelState {
    Idle,
    Streaming,
    Executing,
}

pub struct Kernel {
    state: KernelState,
    model: String,
}

impl Kernel {
    pub fn new(model: String) -> Self {
        Self {
            state: KernelState::Idle,
            model,
        }
    }

    pub fn state(&self) -> KernelState {
        self.state
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// Stream a single completion round: send the request, accumulate the
    /// response into content blocks. Does NOT execute tools or loop.
    ///
    /// Use this when the caller drives the tool-call loop externally (e.g.
    /// the wasm layer which does async fetch between rounds).
    pub fn stream_once<T: Transport>(
        &mut self,
        request: CompletionRequest,
        transport: &T,
        emit: &mut dyn FnMut(AgentEvent),
    ) -> Result<Vec<ContentBlock>, String> {
        assert_eq!(self.state, KernelState::Idle, "kernel must be idle");

        self.state = KernelState::Streaming;
        emit(AgentEvent::TurnStart);

        let mut stream = transport.send(request).inspect_err(|_| {
            self.state = KernelState::Idle;
        })?;

        let content = self.accumulate_response(&mut stream, emit)?;
        self.state = KernelState::Idle;
        Ok(content)
    }

    /// Run one full agentic turn: stream a response from the transport,
    /// execute any tool calls, and loop until the model produces end_turn.
    ///
    /// Returns the sequence of AgentEvents produced during the turn.
    /// The caller provides the completion request and the kernel drives
    /// the loop.
    pub fn run_turn<T: Transport>(
        &mut self,
        request: CompletionRequest,
        transport: &T,
        tools: &ToolRegistry,
        emit: &mut dyn FnMut(AgentEvent),
    ) -> Result<TurnResult, String> {
        assert_eq!(
            self.state,
            KernelState::Idle,
            "kernel must be idle to start a turn"
        );

        let mut messages = request.messages.clone();
        let system = request.system.clone();
        let tool_schemas = request.tools.clone();

        loop {
            // Build the request for this iteration
            let req = CompletionRequest {
                model: self.model.clone(),
                max_tokens: request.max_tokens,
                system: system.clone(),
                messages: messages.clone(),
                tools: tool_schemas.clone(),
                stream: true,
            };

            // Stream phase
            self.state = KernelState::Streaming;
            emit(AgentEvent::TurnStart);

            let mut stream = transport.send(req).inspect_err(|_| {
                self.state = KernelState::Idle;
            })?;

            let assistant_msg = self.accumulate_response(&mut stream, emit)?;

            // Extract tool calls from the assistant message
            let tool_calls: Vec<ToolCall> = assistant_msg
                .iter()
                .filter_map(|block| {
                    if let ContentBlock::ToolUse(tc) = block {
                        Some(tc.clone())
                    } else {
                        None
                    }
                })
                .collect();

            // Append assistant message to conversation
            let assistant_value = serialize_assistant_message(&assistant_msg);
            messages.push(assistant_value);

            if tool_calls.is_empty() {
                // No tool calls — turn is done
                self.state = KernelState::Idle;
                emit(AgentEvent::TurnEnd);
                return Ok(TurnResult {
                    content: assistant_msg,
                    messages,
                });
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

            // Append tool results to conversation
            let tool_results_value = serialize_tool_results(&results);
            messages.push(tool_results_value);

            // Loop: go back to streaming with the updated conversation
            self.state = KernelState::Idle;
        }
    }

    /// Accumulate stream events into content blocks.
    fn accumulate_response(
        &self,
        stream: &mut dyn EventStream,
        emit: &mut dyn FnMut(AgentEvent),
    ) -> Result<Vec<ContentBlock>, String> {
        let mut blocks: Vec<ContentBlock> = Vec::new();
        let mut current_text = String::new();
        let mut current_tool: Option<(String, String, String)> = None; // (id, name, input_json)

        while let Some(event) = stream.next_event() {
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

/// The result of running one turn.
pub struct TurnResult {
    pub content: Vec<ContentBlock>,
    pub messages: Vec<serde_json::Value>,
}

// ---------------------------------------------------------------------------
// Serialization helpers (produce Anthropic Messages API format)
// ---------------------------------------------------------------------------

fn serialize_assistant_message(blocks: &[ContentBlock]) -> serde_json::Value {
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

fn serialize_tool_results(results: &[ToolResult]) -> serde_json::Value {
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
// Demo tool: reverse_text
// ---------------------------------------------------------------------------

pub struct ReverseTextTool;

impl Tool for ReverseTextTool {
    fn name(&self) -> &str {
        "reverse_text"
    }

    fn description(&self) -> &str {
        "Reverse the characters in a string"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "The text to reverse"
                }
            },
            "required": ["text"]
        })
    }

    fn execute(&self, input: serde_json::Value) -> Result<String, String> {
        let text = input
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "missing 'text' parameter".to_string())?;
        Ok(text.chars().rev().collect())
    }
}
