//! Agent composition for the ox framework.
//!
//! `ox-core` wires together [`Namespace`] and the various providers into a
//! single [`Agent`] struct. The kernel is a set of free functions
//! ([`run_turn`], [`synthesize`], etc.) that operate on the namespace.
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
pub use ox_context::{Namespace, SystemProvider};

// --- Re-exports from ox-gate ---
pub use ox_gate::{AccountConfig, GateStore, ProviderConfig};

// --- Re-exports from ox-history ---
pub use ox_history::{HistoryProvider, HistoryView};

// --- Re-exports from ox-tools ---
pub use ox_tools;

// --- Re-exports from ox-kernel (core types, traits, free functions) ---
pub use ox_kernel::{
    AgentEvent, CompletionRequest, ContentBlock, ContextRef, Message, Path, Reader, Record,
    ResolvedContext, Store, StoreError, StreamEvent, ToolCall, ToolResult, ToolSchema, Value,
    Writer, accumulate_response, complete, default_refs, execute_tools, path, read_model_config,
    record_tool_results, record_turn, resolve_refs, run_turn, serialize_assistant_message,
    serialize_tool_results, synthesize,
};

/// The Agent composes a Namespace (with stores) and subscribers.
///
/// It owns the full state of one agent session. Callers drive the kernel
/// via free functions ([`run_turn`], [`synthesize`], etc.) or use the Wasm
/// runtime / custom loops — there is no built-in `prompt()` method.
#[allow(dead_code)] // Fields accessed by consumers via namespace
pub struct Agent {
    context: Namespace,
    subscribers: Vec<Box<dyn FnMut(AgentEvent)>>,
}

impl Agent {
    /// Create a new agent with the given system prompt and tool store.
    ///
    /// Sets up the internal [`Namespace`] with providers for the system
    /// prompt, history, tools, gate, and log. Use [`ox_tools::ToolStore`]
    /// to provide both tool schemas and execution.
    pub fn new(system_prompt: String, tool_store: ox_tools::ToolStore) -> Self {
        let shared_log = ox_kernel::log::SharedLog::new();

        let mut context = Namespace::new();
        context.mount("system", Box::new(SystemProvider::new(system_prompt)));
        context.mount("history", Box::new(HistoryView::new(shared_log.clone())));
        context.mount("tools", Box::new(tool_store));
        context.mount("gate", Box::new(ox_gate::GateStore::new()));
        context.mount(
            "log",
            Box::new(ox_kernel::log::LogStore::from_shared(shared_log)),
        );

        Self {
            context,
            subscribers: Vec::new(),
        }
    }

    /// Register a callback to receive agent events.
    pub fn subscribe(&mut self, callback: Box<dyn FnMut(AgentEvent)>) {
        self.subscribers.push(callback);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ox_kernel::{AgentEvent, CompletionRequest, StreamEvent, run_turn};
    use ox_tools::completion::CompletionTransport;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    // -----------------------------------------------------------------------
    // SequentialTransport — returns canned responses in order
    // -----------------------------------------------------------------------

    struct SequentialTransport {
        responses: Mutex<VecDeque<(Vec<StreamEvent>, u32, u32)>>,
    }

    impl SequentialTransport {
        fn new(responses: Vec<(Vec<StreamEvent>, u32, u32)>) -> Self {
            Self {
                responses: Mutex::new(VecDeque::from(responses)),
            }
        }
    }

    impl CompletionTransport for SequentialTransport {
        fn send(
            &self,
            _request: &CompletionRequest,
            on_event: &dyn Fn(&StreamEvent),
        ) -> Result<(Vec<StreamEvent>, u32, u32), String> {
            let resp = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or("no more canned responses")?;
            for event in &resp.0 {
                on_event(event);
            }
            Ok(resp)
        }
    }

    // -----------------------------------------------------------------------
    // Helper: build a Namespace with the given transport injected
    // -----------------------------------------------------------------------

    fn make_namespace(transport: SequentialTransport) -> Namespace {
        let shared_log = ox_kernel::log::SharedLog::new();
        let mut tool_store = ox_tools::ToolStore::empty();
        tool_store
            .completions_mut()
            .set_transport(Box::new(transport));

        let mut ns = Namespace::new();
        ns.mount(
            "system",
            Box::new(SystemProvider::new("You are a test bot.".into())),
        );
        ns.mount("history", Box::new(HistoryView::new(shared_log.clone())));
        ns.mount("tools", Box::new(tool_store));
        ns.mount("gate", Box::new(ox_gate::GateStore::new()));
        ns.mount(
            "log",
            Box::new(ox_kernel::log::LogStore::from_shared(shared_log)),
        );
        ns
    }

    fn seed_user_message(ns: &mut Namespace, text: &str) {
        ns.write(
            &ox_kernel::path!("history/append"),
            ox_kernel::Record::parsed(structfs_serde_store::json_to_value(
                serde_json::json!({"role": "user", "content": text}),
            )),
        )
        .unwrap();
    }

    // Helper: read an integer value from the namespace
    fn read_count(ns: &mut Namespace, path: &str) -> i64 {
        let p = ox_kernel::Path::parse(path).unwrap();
        let record = ns.read(&p).unwrap().unwrap();
        match record.as_value().unwrap() {
            ox_kernel::Value::Integer(n) => *n,
            other => panic!("expected Integer at {path}, got {:?}", other),
        }
    }

    // -----------------------------------------------------------------------
    // Test 1: text-only response — no tool calls
    // -----------------------------------------------------------------------

    #[test]
    fn run_turn_text_only_response() {
        let transport = SequentialTransport::new(vec![(
            vec![
                StreamEvent::TextDelta("Hello!".into()),
                StreamEvent::MessageStop,
            ],
            10,
            5,
        )]);

        let mut ns = make_namespace(transport);
        seed_user_message(&mut ns, "hi");

        let mut events: Vec<AgentEvent> = Vec::new();
        run_turn(&mut ns, &mut |e| events.push(e)).unwrap();

        // Should have TurnStart and TurnEnd
        assert!(
            events.iter().any(|e| matches!(e, AgentEvent::TurnStart)),
            "expected TurnStart in events"
        );
        assert!(
            events.iter().any(|e| matches!(e, AgentEvent::TurnEnd)),
            "expected TurnEnd in events"
        );

        // history/count == 2: user + assistant
        let hist_count = read_count(&mut ns, "history/count");
        assert_eq!(
            hist_count, 2,
            "expected 2 history messages (user + assistant)"
        );

        // log/count == 2: user entry (via HistoryView) + assistant entry (via record_turn)
        let log_count = read_count(&mut ns, "log/count");
        assert_eq!(log_count, 2, "expected 2 log entries (user + assistant)");
    }

    // -----------------------------------------------------------------------
    // Test 2: tool call followed by text response
    // -----------------------------------------------------------------------

    #[test]
    fn run_turn_with_tool_call() {
        // Two canned responses:
        // 1. Tool call for echo_tool
        // 2. Text response after tool result
        let transport = SequentialTransport::new(vec![
            (
                vec![
                    StreamEvent::ToolUseStart {
                        id: "tc1".into(),
                        name: "echo_tool".into(),
                    },
                    StreamEvent::ToolUseInputDelta(r#"{"text": "ping"}"#.into()),
                    StreamEvent::MessageStop,
                ],
                10,
                5,
            ),
            (
                vec![
                    StreamEvent::TextDelta("pong".into()),
                    StreamEvent::MessageStop,
                ],
                5,
                3,
            ),
        ]);

        // Build a ToolStore with echo_tool registered
        let mut tool_store = ox_tools::ToolStore::empty();
        tool_store
            .completions_mut()
            .set_transport(Box::new(transport));
        tool_store.register_native(Box::new(ox_tools::native::FnTool::new(
            "echo_tool",
            "native/echo_tool",
            "Echoes its input back",
            serde_json::json!({"type": "object", "properties": {"text": {"type": "string"}}}),
            |input| Ok(input),
        )));

        let shared_log = ox_kernel::log::SharedLog::new();
        let mut ns = Namespace::new();
        ns.mount(
            "system",
            Box::new(SystemProvider::new("You are a test bot.".into())),
        );
        ns.mount("history", Box::new(HistoryView::new(shared_log.clone())));
        ns.mount("tools", Box::new(tool_store));
        ns.mount("gate", Box::new(ox_gate::GateStore::new()));
        ns.mount(
            "log",
            Box::new(ox_kernel::log::LogStore::from_shared(shared_log)),
        );

        seed_user_message(&mut ns, "echo ping");

        let mut events: Vec<AgentEvent> = Vec::new();
        run_turn(&mut ns, &mut |e| events.push(e)).unwrap();

        // Should have ToolCallStart and ToolCallResult
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::ToolCallStart { .. })),
            "expected ToolCallStart in events"
        );
        assert!(
            events
                .iter()
                .any(|e| matches!(e, AgentEvent::ToolCallResult { .. })),
            "expected ToolCallResult in events"
        );

        // Should have two TurnStarts (one per loop iteration)
        let turn_start_count = events
            .iter()
            .filter(|e| matches!(e, AgentEvent::TurnStart))
            .count();
        assert_eq!(turn_start_count, 2, "expected 2 TurnStart events");

        // history/count == 4: user + assistant(tool_call) + tool_result + assistant(text)
        let hist_count = read_count(&mut ns, "history/count");
        assert_eq!(
            hist_count, 4,
            "expected 4 history messages (user + assistant(tool_call) + tool_result + assistant)"
        );
    }

    // -----------------------------------------------------------------------
    // Test 3: LLM calls the complete tool (sub-completion via stack)
    // -----------------------------------------------------------------------

    #[test]
    fn run_turn_llm_calls_complete_tool() {
        // Simulate: LLM calls "complete" as a tool in its first response.
        // The kernel pushes a new completion frame, fires the inner completion,
        // pops when it resolves to text, delivers the text as a tool result,
        // then the outer LLM produces a final response.
        let transport = SequentialTransport::new(vec![
            // Outer completion #1: LLM calls the complete tool
            (
                vec![
                    StreamEvent::ToolUseStart {
                        id: "tc1".into(),
                        name: "complete".into(),
                    },
                    StreamEvent::ToolUseInputDelta(
                        serde_json::json!({
                            "account": "anthropic",
                            "refs": [
                                {"type": "system", "path": "system"},
                                {"type": "raw", "content": "Summarize briefly."}
                            ]
                        })
                        .to_string(),
                    ),
                    StreamEvent::MessageStop,
                ],
                10,
                5,
            ),
            // Inner completion (fired by the kernel's stack reactor)
            (
                vec![
                    StreamEvent::TextDelta("Brief summary.".into()),
                    StreamEvent::MessageStop,
                ],
                10,
                5,
            ),
            // Outer completion #2: LLM produces final response using tool result
            (
                vec![
                    StreamEvent::TextDelta("Here is the summary: Brief summary.".into()),
                    StreamEvent::MessageStop,
                ],
                10,
                5,
            ),
        ]);

        let mut ns = make_namespace(transport);
        seed_user_message(&mut ns, "summarize the project");

        let mut events = vec![];
        run_turn(&mut ns, &mut |e| events.push(format!("{e:?}"))).unwrap();

        // 3 TurnStarts: outer #1 + inner + outer #2
        let turn_starts = events.iter().filter(|e| e.contains("TurnStart")).count();
        assert_eq!(turn_starts, 3);

        // The complete tool call should be visible
        assert!(events.iter().any(|e| e.contains("ToolCallStart")));
        assert!(events.iter().any(|e| e.contains("ToolCallResult")));

        // Final response
        assert!(events.iter().any(|e| e.contains("Here is the summary")));
    }
}
