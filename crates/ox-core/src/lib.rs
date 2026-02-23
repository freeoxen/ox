pub use ox_context::ContextManager;
pub use ox_history::MessageLog;
pub use ox_kernel::{
    AgentEvent, CompletionRequest, ContentBlock, EventStream, Kernel, Message, ReverseTextTool,
    StreamEvent, Tool, ToolCall, ToolRegistry, ToolResult, Transport,
};

/// The Agent composes a Kernel, ContextManager, MessageLog, and ToolRegistry.
///
/// It owns the full state of one agent session and exposes a simple
/// `prompt()` method that drives the agentic loop.
pub struct Agent<T: Transport> {
    kernel: Kernel,
    context: ContextManager,
    history: MessageLog,
    tools: ToolRegistry,
    transport: T,
    subscribers: Vec<Box<dyn FnMut(AgentEvent)>>,
}

impl<T: Transport> Agent<T> {
    pub fn new(system_prompt: String, model: String, max_tokens: u32, transport: T) -> Self {
        let mut tools = ToolRegistry::new();
        tools.register(Box::new(ReverseTextTool));

        Self {
            kernel: Kernel::new(model.clone()),
            context: ContextManager::new(system_prompt, model, max_tokens),
            history: MessageLog::new(),
            tools,
            transport,
            subscribers: Vec::new(),
        }
    }

    /// Register a callback to receive agent events.
    pub fn subscribe(&mut self, callback: Box<dyn FnMut(AgentEvent)>) {
        self.subscribers.push(callback);
    }

    /// Send a user prompt and run the full agentic loop until the model
    /// produces an end_turn response (no more tool calls).
    ///
    /// Returns the final assistant text content.
    pub fn prompt(&mut self, input: &str) -> Result<String, String> {
        // Append user message to history
        self.history.append(Message::User {
            content: input.to_string(),
        });

        // Build the completion request from context + history
        let messages = self.history.to_wire_messages();
        let request = self.context.build_request(messages, &self.tools);

        // Capture subscribers so we can pass a mutable closure to run_turn
        let subscribers = &mut self.subscribers;
        let mut emit = |event: AgentEvent| {
            for sub in subscribers.iter_mut() {
                sub(event.clone());
            }
        };

        // Run the agentic loop
        let result = self
            .kernel
            .run_turn(request, &self.transport, &self.tools, &mut emit)?;

        // Update history with the conversation that happened during the turn.
        // The kernel returns the full message list including the original
        // messages. We need to extract just the new messages (after our
        // original history) and append them.
        let original_len = self.history.messages().len();
        let new_messages = &result.messages[original_len..];

        // Parse the new wire-format messages back into our Message types
        for wire_msg in new_messages {
            let role = wire_msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
            match role {
                "assistant" => {
                    let content = parse_assistant_content(wire_msg);
                    self.history.append(Message::Assistant { content });
                }
                "user" => {
                    // Could be tool results or a user message
                    if let Some(content_arr) = wire_msg.get("content").and_then(|c| c.as_array())
                        && content_arr
                            .first()
                            .and_then(|c| c.get("type"))
                            .and_then(|t| t.as_str())
                            == Some("tool_result")
                    {
                        let results = content_arr
                            .iter()
                            .filter_map(|item| {
                                let tool_use_id = item.get("tool_use_id")?.as_str()?.to_string();
                                let content = item.get("content")?.as_str()?.to_string();
                                Some(ToolResult {
                                    tool_use_id,
                                    content,
                                })
                            })
                            .collect();
                        self.history.append(Message::ToolResult { results });
                    }
                }
                _ => {}
            }
        }

        // Extract final text from the assistant response
        let text = result
            .content
            .iter()
            .filter_map(|block| {
                if let ContentBlock::Text { text } = block {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("");

        Ok(text)
    }
}

fn parse_assistant_content(wire_msg: &serde_json::Value) -> Vec<ContentBlock> {
    let content_arr = match wire_msg.get("content").and_then(|c| c.as_array()) {
        Some(arr) => arr,
        None => return Vec::new(),
    };

    content_arr
        .iter()
        .filter_map(|item| {
            let typ = item.get("type")?.as_str()?;
            match typ {
                "text" => {
                    let text = item.get("text")?.as_str()?.to_string();
                    Some(ContentBlock::Text { text })
                }
                "tool_use" => {
                    let id = item.get("id")?.as_str()?.to_string();
                    let name = item.get("name")?.as_str()?.to_string();
                    let input = item.get("input")?.clone();
                    Some(ContentBlock::ToolUse(ToolCall { id, name, input }))
                }
                _ => None,
            }
        })
        .collect()
}
