//! Agent composition for the ox framework.
//!
//! `ox-core` wires together [`Kernel`], [`Namespace`], and the various
//! providers into a single [`Agent`] struct.
//!
//! This is the main entry point for native (non-Wasm) consumers. It
//! re-exports all public types from `ox-kernel`, `ox-context`, `ox-gate`,
//! and `ox-history` so downstream crates only need to depend on `ox-core`.
//!
//! ```ignore
//! let mut agent = Agent::new(
//!     "You are helpful.".into(),
//!     tool_store,
//! );
//! ```

// --- Re-exports from ox-context ---
pub use ox_context::{Namespace, SystemProvider, ToolsProvider};

// --- Re-exports from ox-gate ---
pub use ox_gate::{AccountConfig, GateStore, ProviderConfig};

// --- Re-exports from ox-history ---
pub use ox_history::HistoryProvider;

// --- Re-exports from ox-tools ---
pub use ox_tools;

// --- Re-exports from ox-kernel (core types, traits, state machine) ---
pub use ox_kernel::{
    AgentEvent, CompletionRequest, ContentBlock, Kernel, Message, Path, Reader, Record,
    Store, StoreError, StreamEvent, ToolCall, ToolResult, ToolSchema, Value,
    Writer, path, serialize_assistant_message, serialize_tool_results,
};

/// The Agent composes a Kernel, Namespace (with stores), and a ToolStore.
///
/// It owns the full state of one agent session. Callers drive the kernel
/// directly or use the Wasm runtime / custom loops — there is no built-in
/// `prompt()` method.
pub struct Agent {
    kernel: Kernel,
    context: Namespace,
    subscribers: Vec<Box<dyn FnMut(AgentEvent)>>,
}

impl Agent {
    /// Create a new agent with the given system prompt and tool store.
    ///
    /// Sets up the internal [`Namespace`] with providers for the system
    /// prompt, history, and tools. Use [`ox_tools::ToolStore`] to provide
    /// both tool schemas and execution.
    pub fn new(system_prompt: String, tool_store: ox_tools::ToolStore) -> Self {
        let mut context = Namespace::new();
        context.mount("system", Box::new(SystemProvider::new(system_prompt)));
        context.mount("history", Box::new(HistoryProvider::new()));
        context.mount("tools", Box::new(tool_store));

        Self {
            kernel: Kernel::new("default".into()),
            context,
            subscribers: Vec::new(),
        }
    }

    /// Register a callback to receive agent events.
    pub fn subscribe(&mut self, callback: Box<dyn FnMut(AgentEvent)>) {
        self.subscribers.push(callback);
    }
}
