use ox_context::{ModelProvider, Namespace, SystemProvider, ToolsProvider};
use ox_gate::GateStore;
use ox_history::HistoryProvider;
use ox_kernel::{
    AgentEvent, CompletionRequest, ContentBlock, Kernel, Record, StreamEvent, ToolRegistry,
    ToolResult, Value, Writer, path, serialize_tool_results,
};
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::thread;
use structfs_serde_store::json_to_value;

const SYSTEM_PROMPT: &str = "\
You are an expert software engineer working in a coding CLI. \
You have tools for reading files, writing files, editing files, \
and running shell commands. \
Always read a file before modifying it. Be concise.";

/// Events flowing from the agent thread to the TUI.
#[derive(Debug, Clone)]
pub enum AppEvent {
    Agent(AgentEvent),
    Done(Result<String, String>),
}

/// A message visible in the conversation.
#[derive(Debug, Clone)]
pub enum ChatMessage {
    User(String),
    AssistantChunk(String),
    ToolCall { name: String },
    ToolResult { name: String, output: String },
    Error(String),
}

/// TUI-side application state.
pub struct App {
    pub messages: Vec<ChatMessage>,
    pub input: String,
    pub cursor: usize,
    pub scroll: u16,
    pub thinking: bool,
    pub should_quit: bool,
    pub model: String,
    pub provider: String,
    prompt_tx: mpsc::Sender<String>,
    pub event_rx: mpsc::Receiver<AppEvent>,
    // Input history
    pub input_history: Vec<String>,
    history_cursor: usize,
    input_draft: String,
}

impl App {
    /// Create the App and spawn the agent background thread.
    pub fn new(
        provider: String,
        model: String,
        max_tokens: u32,
        api_key: String,
        workspace: PathBuf,
    ) -> Self {
        let (prompt_tx, prompt_rx) = mpsc::channel::<String>();
        let (event_tx, event_rx) = mpsc::channel::<AppEvent>();

        let send = crate::transport::make_send_fn(provider.clone(), api_key.clone());
        let tools = crate::tools::standard_tools(workspace);

        let agent_model = model.clone();
        let agent_provider = provider.clone();
        thread::spawn(move || {
            agent_thread(
                agent_model,
                agent_provider,
                max_tokens,
                api_key,
                send,
                tools,
                prompt_rx,
                event_tx,
            );
        });

        Self {
            messages: Vec::new(),
            input: String::new(),
            cursor: 0,
            scroll: 0,
            thinking: false,
            should_quit: false,
            model,
            provider,
            prompt_tx,
            event_rx,
            input_history: Vec::new(),
            history_cursor: 0,
            input_draft: String::new(),
        }
    }

    /// Submit the current input as a user prompt.
    pub fn submit(&mut self) {
        if self.input.is_empty() || self.thinking {
            return;
        }
        let input = std::mem::take(&mut self.input);
        self.cursor = 0;
        self.input_history.push(input.clone());
        self.history_cursor = self.input_history.len();
        self.input_draft.clear();
        self.messages.push(ChatMessage::User(input.clone()));
        self.thinking = true;
        self.scroll = 0;
        self.prompt_tx.send(input).ok();
    }

    /// Navigate input history up (older).
    pub fn history_up(&mut self) {
        if self.input_history.is_empty() {
            return;
        }
        if self.history_cursor == self.input_history.len() {
            self.input_draft = self.input.clone();
        }
        if self.history_cursor > 0 {
            self.history_cursor -= 1;
            self.input = self.input_history[self.history_cursor].clone();
            self.cursor = self.input.len();
        }
    }

    /// Navigate input history down (newer).
    pub fn history_down(&mut self) {
        if self.history_cursor < self.input_history.len() {
            self.history_cursor += 1;
            if self.history_cursor == self.input_history.len() {
                self.input = self.input_draft.clone();
            } else {
                self.input = self.input_history[self.history_cursor].clone();
            }
            self.cursor = self.input.len();
        }
    }

    /// Process a single AppEvent, updating visible state.
    pub fn handle_event(&mut self, event: AppEvent) {
        // Auto-scroll to bottom on new content
        self.scroll = 0;

        match event {
            AppEvent::Agent(AgentEvent::TextDelta(text)) => {
                if let Some(&mut ChatMessage::AssistantChunk(ref mut s)) = self.messages.last_mut()
                {
                    s.push_str(&text);
                } else {
                    self.messages.push(ChatMessage::AssistantChunk(text));
                }
            }
            AppEvent::Agent(AgentEvent::ToolCallStart { name }) => {
                self.messages.push(ChatMessage::ToolCall { name });
            }
            AppEvent::Agent(AgentEvent::ToolCallResult { name, result }) => {
                self.messages.push(ChatMessage::ToolResult {
                    name,
                    output: result,
                });
            }
            AppEvent::Agent(AgentEvent::TurnStart) => {}
            AppEvent::Agent(AgentEvent::TurnEnd) => {}
            AppEvent::Agent(AgentEvent::Error(e)) => {
                self.messages.push(ChatMessage::Error(e));
            }
            AppEvent::Done(Ok(_)) => {
                self.thinking = false;
            }
            AppEvent::Done(Err(e)) => {
                self.messages.push(ChatMessage::Error(e));
                self.thinking = false;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Agent thread — three-phase kernel API with streaming transport
// ---------------------------------------------------------------------------

fn agent_thread(
    model: String,
    provider: String,
    max_tokens: u32,
    api_key: String,
    send: impl Fn(&CompletionRequest) -> Result<Vec<StreamEvent>, String> + Send + Sync + 'static,
    extra_tools: Vec<Box<dyn ox_kernel::Tool>>,
    prompt_rx: mpsc::Receiver<String>,
    event_tx: mpsc::Sender<AppEvent>,
) {
    let send = Arc::new(send);
    let client = reqwest::blocking::Client::new();
    let mut kernel = Kernel::new(model.clone());

    // Build tool registry: standard tools + completion tools from gate
    let mut tools = ToolRegistry::new();
    for tool in extra_tools {
        tools.register(tool);
    }

    let mut gate = GateStore::new();
    gate.write(
        &ox_kernel::Path::from_components(vec![
            "accounts".to_string(),
            provider.clone(),
            "key".to_string(),
        ]),
        Record::parsed(Value::String(api_key.clone())),
    )
    .ok();
    for tool in gate.create_completion_tools(send) {
        tools.register(tool);
    }

    // Build namespace
    let mut context = Namespace::new();
    context.mount(
        "system",
        Box::new(SystemProvider::new(SYSTEM_PROMPT.to_string())),
    );
    context.mount("history", Box::new(HistoryProvider::new()));
    context.mount("tools", Box::new(ToolsProvider::new(tools.schemas())));
    context.mount("model", Box::new(ModelProvider::new(model, max_tokens)));
    context.mount("gate", Box::new(gate));

    while let Ok(input) = prompt_rx.recv() {
        // Write user message to history
        let user_json = serde_json::json!({"role": "user", "content": input});
        if let Err(e) = context.write(
            &path!("history/append"),
            Record::parsed(json_to_value(user_json)),
        ) {
            event_tx.send(AppEvent::Done(Err(e.to_string()))).ok();
            continue;
        }

        let result =
            run_streaming_loop(&mut kernel, &mut context, &tools, &client, &provider, &api_key, &event_tx);
        event_tx.send(AppEvent::Done(result)).ok();
    }
}

/// Drive the agentic loop with streaming HTTP and three-phase kernel API.
fn run_streaming_loop(
    kernel: &mut Kernel,
    context: &mut Namespace,
    tools: &ToolRegistry,
    client: &reqwest::blocking::Client,
    provider: &str,
    api_key: &str,
    event_tx: &mpsc::Sender<AppEvent>,
) -> Result<String, String> {
    loop {
        let request = kernel.initiate_completion(context)?;

        // Stream HTTP — emit TextDelta events in real-time as SSE lines arrive
        let tx = event_tx.clone();
        let events = crate::transport::streaming_fetch(client, provider, api_key, &request, &|event| {
            if let StreamEvent::TextDelta(text) = event {
                tx.send(AppEvent::Agent(AgentEvent::TextDelta(text.clone())))
                    .ok();
            }
        })?;

        // Kernel processes events — suppress TextDelta (already streamed)
        let tx2 = event_tx.clone();
        let mut emit = |event: AgentEvent| {
            if !matches!(event, AgentEvent::TextDelta(_) | AgentEvent::TurnStart) {
                tx2.send(AppEvent::Agent(event)).ok();
            }
        };
        let content = kernel.consume_events(events, &mut emit)?;
        let tool_calls = kernel.complete_turn(context, &content)?;

        if tool_calls.is_empty() {
            event_tx
                .send(AppEvent::Agent(AgentEvent::TurnEnd))
                .ok();
            let text: String = content
                .iter()
                .filter_map(|b| {
                    if let ContentBlock::Text { text } = b {
                        Some(text.as_str())
                    } else {
                        None
                    }
                })
                .collect();
            return Ok(text);
        }

        // Execute tools
        let mut results = Vec::new();
        for tc in &tool_calls {
            event_tx
                .send(AppEvent::Agent(AgentEvent::ToolCallStart {
                    name: tc.name.clone(),
                }))
                .ok();
            let result_str = match tools.get(&tc.name) {
                Some(tool) => tool
                    .execute(tc.input.clone())
                    .unwrap_or_else(|e| format!("error: {e}")),
                None => format!("error: unknown tool '{}'", tc.name),
            };
            event_tx
                .send(AppEvent::Agent(AgentEvent::ToolCallResult {
                    name: tc.name.clone(),
                    result: result_str.clone(),
                }))
                .ok();
            results.push(ToolResult {
                tool_use_id: tc.id.clone(),
                content: result_str,
            });
        }

        // Write tool results to history
        let results_json = serialize_tool_results(&results);
        context
            .write(
                &path!("history/append"),
                Record::parsed(json_to_value(results_json)),
            )
            .map_err(|e| e.to_string())?;
    }
}
