use ox_kernel::Message;

/// In-memory message log. No persistence, no branching.
pub struct MessageLog {
    messages: Vec<Message>,
}

impl MessageLog {
    pub fn new() -> Self {
        Self {
            messages: Vec::new(),
        }
    }

    pub fn append(&mut self, message: Message) {
        self.messages.push(message);
    }

    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    pub fn clear(&mut self) {
        self.messages.clear();
    }

    /// Serialize all messages to the Anthropic wire format.
    pub fn to_wire_messages(&self) -> Vec<serde_json::Value> {
        self.messages
            .iter()
            .map(|msg| match msg {
                Message::User { content } => serde_json::json!({
                    "role": "user",
                    "content": content,
                }),
                Message::Assistant { content } => {
                    let blocks: Vec<serde_json::Value> = content
                        .iter()
                        .map(|b| match b {
                            ox_kernel::ContentBlock::Text { text } => serde_json::json!({
                                "type": "text",
                                "text": text,
                            }),
                            ox_kernel::ContentBlock::ToolUse(tc) => serde_json::json!({
                                "type": "tool_use",
                                "id": tc.id,
                                "name": tc.name,
                                "input": tc.input,
                            }),
                        })
                        .collect();
                    serde_json::json!({
                        "role": "assistant",
                        "content": blocks,
                    })
                }
                Message::ToolResult { results } => {
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
            })
            .collect()
    }
}

impl Default for MessageLog {
    fn default() -> Self {
        Self::new()
    }
}
