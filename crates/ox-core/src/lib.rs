//! Agent composition for the ox framework.
//!
//! `ox-core` wires together [`Kernel`], [`Namespace`], and the various
//! providers into a single [`Agent`] struct with a simple `prompt()` method.
//!
//! This is the main entry point for native (non-Wasm) consumers. It
//! re-exports all public types from `ox-kernel`, `ox-context`, `ox-gate`,
//! and `ox-history` so downstream crates only need to depend on `ox-core`.
//!
//! ```ignore
//! let mut agent = Agent::new(
//!     "You are helpful.".into(),
//!     "claude-sonnet-4-20250514".into(),
//!     4096,
//!     |req| my_send(req),
//!     ToolRegistry::new(),
//! );
//! let reply = agent.prompt("Hello")?;
//! ```

// --- Re-exports from ox-context ---
pub use ox_context::{Namespace, SystemProvider, ToolsProvider};

// --- Re-exports from ox-gate ---
pub use ox_gate::{AccountConfig, GateStore, ProviderConfig, completion_tool};

// --- Re-exports from ox-history ---
pub use ox_history::HistoryProvider;

// --- Re-exports from ox-kernel (core types, traits, state machine) ---
pub use ox_kernel::{
    AgentEvent, CompletionRequest, ContentBlock, FnTool, Kernel, Message, Path, Reader, Record,
    Store, StoreError, StreamEvent, Tool, ToolCall, ToolRegistry, ToolResult, ToolSchema, Value,
    Writer, path, serialize_assistant_message, serialize_tool_results,
};

use std::sync::Arc;
use structfs_serde_store::json_to_value;

/// A synchronous send function: takes a completion request, returns parsed events.
pub type SendFn = dyn Fn(&CompletionRequest) -> Result<Vec<StreamEvent>, String> + Send + Sync;

/// The Agent composes a Kernel, Namespace (with stores), and ToolRegistry.
///
/// It owns the full state of one agent session and exposes a simple
/// `prompt()` method that drives the agentic loop. Completion tools from
/// the [`GateStore`] are automatically registered when accounts have keys set.
pub struct Agent {
    kernel: Kernel,
    context: Namespace,
    tools: ToolRegistry,
    send: Arc<SendFn>,
    subscribers: Vec<Box<dyn FnMut(AgentEvent)>>,
}

impl Agent {
    /// Create a new agent with the given configuration.
    ///
    /// Sets up the internal [`Namespace`] with providers for the system
    /// prompt, history, tools, model, and gate. The `tools` registry is
    /// used for both schema generation (sent to the model) and execution.
    ///
    /// Completion tools (e.g. `complete_openai`) are automatically created
    /// for any gate account with an API key set and registered alongside
    /// the caller-provided tools.
    ///
    /// `send` is a synchronous function that sends a [`CompletionRequest`]
    /// and returns parsed [`StreamEvent`]s.
    pub fn new(
        system_prompt: String,
        model: String,
        max_tokens: u32,
        send: impl Fn(&CompletionRequest) -> Result<Vec<StreamEvent>, String> + Send + Sync + 'static,
        mut tools: ToolRegistry,
    ) -> Self {
        let send: Arc<SendFn> = Arc::new(send);
        let mut gate = GateStore::new();

        // Register completion tools for keyed accounts
        for tool in gate.create_completion_tools(send.clone()) {
            tools.register(tool);
        }

        let mut context = Namespace::new();
        context.mount("system", Box::new(SystemProvider::new(system_prompt)));
        context.mount("history", Box::new(HistoryProvider::new()));
        context.mount("tools", Box::new(ToolsProvider::new(tools.schemas())));
        context.mount("gate", Box::new(gate));

        context
            .write(
                &path!("gate/defaults/model"),
                Record::parsed(Value::String(model.clone())),
            )
            .ok();
        context
            .write(
                &path!("gate/defaults/max_tokens"),
                Record::parsed(Value::Integer(max_tokens as i64)),
            )
            .ok();

        Self {
            kernel: Kernel::new(model),
            context,
            tools,
            send,
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
        let user_json = serde_json::json!({
            "role": "user",
            "content": input,
        });
        let record = Record::parsed(json_to_value(user_json));
        self.context
            .write(&path!("history/append"), record)
            .map_err(|e| e.to_string())?;

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
                .run_turn(&mut self.context, &*self.send, &self.tools, &mut emit)?;

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
