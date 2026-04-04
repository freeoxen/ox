use ox_context::{ModelProvider, Namespace, SystemProvider, ToolsProvider};
use ox_gate::{GateStore, ProviderConfig};
use ox_history::HistoryProvider;
use ox_kernel::{
    AgentEvent, ContentBlock, Kernel, Reader, Record, StreamEvent, ToolRegistry,
    ToolResult, Value, Writer, path, serialize_tool_results,
};
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::thread;
use structfs_serde_store::{from_value, json_to_value, value_to_json};

const SYSTEM_PROMPT: &str = "\
You are an expert software engineer working in a coding CLI. \
You have tools for reading files, writing files, editing files, \
and running shell commands. \
Always read a file before modifying it. Be concise.";

use crate::policy::PolicyStats;

/// Events flowing from the agent thread to the TUI.
#[derive(Debug, Clone)]
pub enum AppEvent {
    Agent(AgentEvent),
    Usage { input_tokens: u32, output_tokens: u32 },
    PolicyStats(PolicyStats),
    Done(Result<String, String>),
}

/// Non-Clone event — carries the oneshot response channel.
pub enum AppControl {
    PermissionRequest {
        tool: String,
        input_preview: String,
        respond: mpsc::Sender<ApprovalResponse>,
    },
}

/// User's response to a permission prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalResponse {
    AllowOnce,
    AllowSession,
    AllowAlways,
    DenyOnce,
    DenySession,
    DenyAlways,
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

/// State for the permission approval dialog.
pub struct ApprovalState {
    pub tool: String,
    pub input_preview: String,
    pub selected: usize,
    pub respond: mpsc::Sender<ApprovalResponse>,
}

impl ApprovalState {
    pub const OPTIONS: [(&str, ApprovalResponse); 6] = [
        ("Allow once", ApprovalResponse::AllowOnce),
        ("Allow for session", ApprovalResponse::AllowSession),
        ("Allow always (add rule)", ApprovalResponse::AllowAlways),
        ("Deny once", ApprovalResponse::DenyOnce),
        ("Deny for session", ApprovalResponse::DenySession),
        ("Deny always (add rule)", ApprovalResponse::DenyAlways),
    ];
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
    pub control_rx: mpsc::Receiver<AppControl>,
    // Input history
    pub input_history: Vec<String>,
    history_cursor: usize,
    input_draft: String,
    // Token usage (cumulative)
    pub tokens_in: u32,
    pub tokens_out: u32,
    // Policy
    pub policy_stats: PolicyStats,
    pub pending_approval: Option<ApprovalState>,
}

impl App {
    /// Create the App and spawn the agent background thread.
    pub fn new(
        provider: String,
        model: String,
        max_tokens: u32,
        api_key: String,
        workspace: PathBuf,
        session_path: Option<PathBuf>,
        no_policy: bool,
    ) -> Self {
        let (prompt_tx, prompt_rx) = mpsc::channel::<String>();
        let (event_tx, event_rx) = mpsc::channel::<AppEvent>();
        let (control_tx, control_rx) = mpsc::channel::<AppControl>();

        let tools = crate::tools::standard_tools(workspace.clone());
        let policy = if no_policy {
            crate::policy::PolicyGuard::permissive()
        } else {
            crate::policy::PolicyGuard::load(&workspace)
        };

        let agent_model = model.clone();
        let agent_provider = provider.clone();
        thread::spawn(move || {
            agent_thread(
                agent_model,
                agent_provider,
                max_tokens,
                api_key,
                tools,
                policy,
                session_path,
                prompt_rx,
                event_tx,
                control_tx,
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
            control_rx,
            input_history: Vec::new(),
            history_cursor: 0,
            input_draft: String::new(),
            tokens_in: 0,
            tokens_out: 0,
            policy_stats: PolicyStats::default(),
            pending_approval: None,
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
        self.scroll = 0; // auto-scroll on new content

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
            AppEvent::Usage {
                input_tokens,
                output_tokens,
            } => {
                self.tokens_in += input_tokens;
                self.tokens_out += output_tokens;
            }
            AppEvent::PolicyStats(stats) => {
                self.policy_stats = stats;
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
// Agent thread — StructFS-native transport, three-phase kernel API
// ---------------------------------------------------------------------------

fn agent_thread(
    model: String,
    provider: String,
    max_tokens: u32,
    api_key: String,
    extra_tools: Vec<Box<dyn ox_kernel::Tool>>,
    mut policy: crate::policy::PolicyGuard,
    session_path: Option<PathBuf>,
    prompt_rx: mpsc::Receiver<String>,
    event_tx: mpsc::Sender<AppEvent>,
    control_tx: mpsc::Sender<AppControl>,
) {
    let client = reqwest::blocking::Client::new();
    let mut kernel = Kernel::new(model.clone());

    // Build tool registry
    let mut tools = ToolRegistry::new();
    for tool in extra_tools {
        tools.register(tool);
    }

    // Set up GateStore with the CLI-provided key
    let mut gate = GateStore::new();
    gate.write(
        &ox_kernel::Path::from_components(vec![
            "accounts".to_string(),
            provider.clone(),
            "key".to_string(),
        ]),
        Record::parsed(Value::String(api_key)),
    )
    .ok();

    // Read provider config from gate (before mounting) for the non-streaming send fn
    let provider_config = read_provider_config_from_gate(&mut gate, &provider)
        .unwrap_or_else(|_| ProviderConfig::anthropic());
    let send_config = provider_config.clone();
    let send_key = read_account_key(&mut gate, &provider).unwrap_or_default();
    let send = Arc::new(crate::transport::make_send_fn(send_config, send_key));

    // Register completion tools for keyed accounts
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

    // Restore session history if a session file exists
    if let Some(ref path) = session_path {
        if path.exists() {
            match crate::session::load(path) {
                Ok(messages) => {
                    for msg in messages {
                        context
                            .write(
                                &path!("history/append"),
                                Record::parsed(json_to_value(msg)),
                            )
                            .ok();
                    }
                }
                Err(e) => {
                    event_tx
                        .send(AppEvent::Agent(AgentEvent::Error(format!(
                            "failed to load session: {e}"
                        ))))
                        .ok();
                }
            }
        }
    }

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

        let result = run_streaming_loop(
            &mut kernel,
            &mut context,
            &tools,
            &mut policy,
            &client,
            &event_tx,
            &control_tx,
        );
        event_tx.send(AppEvent::Done(result)).ok();
    }

    // Save session on shutdown (prompt_rx disconnected)
    if let Some(ref path) = session_path {
        save_session(&mut context, path);
    }
}

/// Read history from namespace and write to session file.
fn save_session(context: &mut Namespace, path: &std::path::Path) {
    let messages = match context.read(&path!("history/messages")) {
        Ok(Some(Record::Parsed(v))) => value_to_json(v),
        _ => return,
    };
    let messages = match messages.as_array() {
        Some(arr) => arr.clone(),
        None => return,
    };
    if messages.is_empty() {
        return;
    }
    if let Err(e) = crate::session::save(path, &messages) {
        eprintln!("warning: failed to save session: {e}");
    }
}

/// Read ProviderConfig from a GateStore before it's mounted in the namespace.
fn read_provider_config_from_gate(
    gate: &mut GateStore,
    account_name: &str,
) -> Result<ProviderConfig, String> {
    // Read account's provider name
    let provider_path = ox_kernel::Path::from_components(vec![
        "accounts".to_string(),
        account_name.to_string(),
        "provider".to_string(),
    ]);
    let provider_name = match gate.read(&provider_path) {
        Ok(Some(Record::Parsed(Value::String(s)))) => s,
        _ => account_name.to_string(),
    };
    // Read provider config
    let config_path = ox_kernel::Path::from_components(vec![
        "providers".to_string(),
        provider_name,
    ]);
    match gate.read(&config_path) {
        Ok(Some(Record::Parsed(v))) => from_value(v).map_err(|e| e.to_string()),
        _ => Err("provider config not found".into()),
    }
}

/// Read API key from a GateStore before it's mounted.
fn read_account_key(gate: &mut GateStore, account_name: &str) -> Result<String, String> {
    let key_path = ox_kernel::Path::from_components(vec![
        "accounts".to_string(),
        account_name.to_string(),
        "key".to_string(),
    ]);
    match gate.read(&key_path) {
        Ok(Some(Record::Parsed(Value::String(s)))) => Ok(s),
        _ => Err("no key".into()),
    }
}

/// Read provider config from the namespace (gate mounted at "gate/").
fn read_gate_config(context: &mut Namespace) -> Result<(ProviderConfig, String), String> {
    let bootstrap = match context.read(&path!("gate/bootstrap")) {
        Ok(Some(Record::Parsed(Value::String(s)))) => s,
        _ => "anthropic".to_string(),
    };

    let key_path = ox_kernel::Path::from_components(vec![
        "gate".into(),
        "accounts".into(),
        bootstrap.clone(),
        "key".into(),
    ]);
    let api_key = match context.read(&key_path) {
        Ok(Some(Record::Parsed(Value::String(s)))) if !s.is_empty() => s,
        _ => return Err(format!("no API key for account '{bootstrap}'")),
    };

    let provider_path = ox_kernel::Path::from_components(vec![
        "gate".into(),
        "accounts".into(),
        bootstrap,
        "provider".into(),
    ]);
    let provider_name = match context.read(&provider_path) {
        Ok(Some(Record::Parsed(Value::String(s)))) => s,
        _ => "anthropic".to_string(),
    };

    let config_path = ox_kernel::Path::from_components(vec![
        "gate".into(),
        "providers".into(),
        provider_name,
    ]);
    let config: ProviderConfig = match context.read(&config_path) {
        Ok(Some(Record::Parsed(v))) => from_value(v).map_err(|e| format!("bad provider config: {e}"))?,
        _ => return Err("provider config not found in namespace".into()),
    };

    Ok((config, api_key))
}

/// Drive the agentic loop: read config from namespace, stream with retry, track tokens.
fn run_streaming_loop(
    kernel: &mut Kernel,
    context: &mut Namespace,
    tools: &ToolRegistry,
    policy: &mut crate::policy::PolicyGuard,
    client: &reqwest::blocking::Client,
    event_tx: &mpsc::Sender<AppEvent>,
    control_tx: &mpsc::Sender<AppControl>,
) -> Result<String, String> {
    let mut stats = PolicyStats::default();

    loop {
        // Read provider config from the namespace each iteration
        let (config, api_key) = read_gate_config(context)?;

        let request = kernel.initiate_completion(context)?;

        // Stream HTTP with real-time TextDelta emission and retry
        let tx = event_tx.clone();
        let (events, usage) =
            crate::transport::streaming_fetch(client, &config, &api_key, &request, &|event| {
                if let StreamEvent::TextDelta(text) = event {
                    tx.send(AppEvent::Agent(AgentEvent::TextDelta(text.clone())))
                        .ok();
                }
            })?;

        // Report token usage
        if usage.input_tokens > 0 || usage.output_tokens > 0 {
            event_tx
                .send(AppEvent::Usage {
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                })
                .ok();
        }

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

        // Execute tools with policy enforcement
        let mut results = Vec::new();
        for tc in &tool_calls {
            // Policy check before execution
            let decision = policy.check(&tc.name, &tc.input);
            let proceed = match decision {
                crate::policy::PolicyDecision::Allow => {
                    stats.allowed += 1;
                    true
                }
                crate::policy::PolicyDecision::Deny(reason) => {
                    stats.denied += 1;
                    results.push(ToolResult {
                        tool_use_id: tc.id.clone(),
                        content: format!("denied: {reason}"),
                    });
                    event_tx.send(AppEvent::PolicyStats(stats.clone())).ok();
                    false
                }
                crate::policy::PolicyDecision::Ask { tool, input_preview } => {
                    stats.asked += 1;
                    event_tx.send(AppEvent::PolicyStats(stats.clone())).ok();
                    // Send request to TUI and block on response
                    let (resp_tx, resp_rx) = mpsc::channel();
                    control_tx
                        .send(AppControl::PermissionRequest {
                            tool,
                            input_preview,
                            respond: resp_tx,
                        })
                        .ok();
                    match resp_rx.recv() {
                        Ok(ApprovalResponse::AllowOnce) => {
                            stats.allowed += 1;
                            true
                        }
                        Ok(ApprovalResponse::AllowSession) => {
                            policy.session_allow(&tc.name, &tc.input);
                            stats.allowed += 1;
                            true
                        }
                        Ok(ApprovalResponse::AllowAlways) => {
                            policy.persist_allow(&tc.name, &tc.input);
                            stats.allowed += 1;
                            true
                        }
                        Ok(ApprovalResponse::DenyOnce) => {
                            stats.denied += 1;
                            results.push(ToolResult {
                                tool_use_id: tc.id.clone(),
                                content: "denied by user".into(),
                            });
                            false
                        }
                        Ok(ApprovalResponse::DenySession) => {
                            policy.session_deny(&tc.name, &tc.input);
                            stats.denied += 1;
                            results.push(ToolResult {
                                tool_use_id: tc.id.clone(),
                                content: "denied by user".into(),
                            });
                            false
                        }
                        Ok(ApprovalResponse::DenyAlways) => {
                            policy.persist_deny(&tc.name, &tc.input);
                            stats.denied += 1;
                            results.push(ToolResult {
                                tool_use_id: tc.id.clone(),
                                content: "denied by user".into(),
                            });
                            false
                        }
                        Err(_) => {
                            results.push(ToolResult {
                                tool_use_id: tc.id.clone(),
                                content: "denied: TUI disconnected".into(),
                            });
                            false
                        }
                    }
                }
            };

            if !proceed {
                continue;
            }

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
            event_tx.send(AppEvent::PolicyStats(stats.clone())).ok();
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
