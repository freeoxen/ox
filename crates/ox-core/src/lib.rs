pub use ox_context::{ModelProvider, Namespace, SystemProvider, ToolsProvider};
pub use ox_history::HistoryProvider;
pub use ox_kernel::{
    AgentEvent, CompletionRequest, ContentBlock, EventStream, Kernel, Message, Provider,
    ReverseTextTool, StreamEvent, Tool, ToolCall, ToolRegistry, ToolResult, Transport, Value,
    serialize_assistant_message, serialize_tool_results,
};

/// The Agent composes a Kernel, Namespace (with providers), and ToolRegistry.
///
/// It owns the full state of one agent session and exposes a simple
/// `prompt()` method that drives the agentic loop.
pub struct Agent<T: Transport> {
    kernel: Kernel,
    context: Namespace,
    tools: ToolRegistry,
    transport: T,
    subscribers: Vec<Box<dyn FnMut(AgentEvent)>>,
}

impl<T: Transport> Agent<T> {
    pub fn new(system_prompt: String, model: String, max_tokens: u32, transport: T) -> Self {
        let mut tools = ToolRegistry::new();
        tools.register(Box::new(ReverseTextTool));

        let mut context = Namespace::new();
        context.mount("system", Box::new(SystemProvider::new(system_prompt)));
        context.mount("history", Box::new(HistoryProvider::new()));
        context.mount("tools", Box::new(ToolsProvider::new(tools.schemas())));
        context.mount(
            "model",
            Box::new(ModelProvider::new(model.clone(), max_tokens)),
        );

        Self {
            kernel: Kernel::new(model),
            context,
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
        // Write user message to the namespace
        let user_wire = serde_json::json!({
            "role": "user",
            "content": input,
        });
        self.context.write("history/append", user_wire)?;

        // Capture subscribers so we can pass a mutable closure to run_turn
        let subscribers = &mut self.subscribers;
        let mut emit = |event: AgentEvent| {
            for sub in subscribers.iter_mut() {
                sub(event.clone());
            }
        };

        // Run the agentic loop — kernel reads/writes the namespace
        let content =
            self.kernel
                .run_turn(&mut self.context, &self.transport, &self.tools, &mut emit)?;

        // Extract final text from the assistant response
        let text = content
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
