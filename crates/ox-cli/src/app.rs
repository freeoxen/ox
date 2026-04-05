use ox_context::{ModelProvider, Namespace, SystemProvider, ToolsProvider};
use ox_gate::{GateStore, ProviderConfig};
use ox_history::HistoryProvider;
use ox_kernel::{
    AgentEvent, CompletionRequest, Reader, Record, StreamEvent, ToolCall, ToolRegistry, Value,
    Writer, path,
};
use ox_runtime::{AgentRuntime, HostEffects, HostStore};
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
    Usage {
        input_tokens: u32,
        output_tokens: u32,
    },
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
#[derive(Debug, Clone)]
pub enum ApprovalResponse {
    AllowOnce,
    AllowSession,
    AllowAlways,
    DenyOnce,
    DenySession,
    DenyAlways,
    /// A custom rule — carries a clash Node + optional sandbox for the agent thread.
    CustomNode {
        node: Box<clash::policy::match_tree::Node>,
        sandbox: Option<(String, clash::policy::sandbox_types::SandboxPolicy)>,
        scope: String, // "once", "session", or "always"
    },
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
        ("Allow once          (y)", ApprovalResponse::AllowOnce),
        ("Allow for session   (s)", ApprovalResponse::AllowSession),
        ("Allow always        (a)", ApprovalResponse::AllowAlways),
        ("Deny once           (n)", ApprovalResponse::DenyOnce),
        ("Deny for session      ", ApprovalResponse::DenySession),
        ("Deny always         (d)", ApprovalResponse::DenyAlways),
    ];
}

/// State for the rule customization editor.
/// Builds a clash Node + optional SandboxPolicy on submit.
pub struct CustomizeState {
    pub tool: String,
    /// Positional argument patterns (for shell: each word; for file tools: single path).
    pub args: Vec<String>,
    pub arg_cursor: usize,
    /// 0 = allow, 1 = deny.
    pub effect_idx: usize,
    /// 0 = once, 1 = session, 2 = always.
    pub scope_idx: usize,
    pub focus: usize,
    pub respond: mpsc::Sender<ApprovalResponse>,
    // Sandbox
    /// 0 = deny, 1 = allow, 2 = localhost
    pub network_idx: usize,
    /// Filesystem sandbox rules: (path, Cap bitflags as rwcdx booleans)
    pub fs_rules: Vec<FsRuleState>,
    pub fs_sub_focus: usize,
    pub fs_path_cursor: usize,
}

/// Editable state for one filesystem sandbox rule.
pub struct FsRuleState {
    pub path: String,
    pub read: bool,
    pub write: bool,
    pub create: bool,
    pub delete: bool,
    pub execute: bool,
}

impl CustomizeState {
    pub fn add_arg_field(&self) -> usize {
        self.args.len()
    }
    pub fn effect_field(&self) -> usize {
        self.args.len() + 1
    }
    pub fn scope_field(&self) -> usize {
        self.args.len() + 2
    }
    pub fn network_field(&self) -> usize {
        self.args.len() + 3
    }
    pub fn fs_start(&self) -> usize {
        self.args.len() + 4
    }
    pub fn add_fs_field(&self) -> usize {
        self.fs_start() + self.fs_rules.len()
    }
    pub fn total_fields(&self) -> usize {
        self.add_fs_field() + 1
    }
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
    pub pending_customize: Option<CustomizeState>,
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
            pending_customize: None,
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
// Agent thread — delegates to ox-runtime Wasm execution
// ---------------------------------------------------------------------------

/// Embedded agent Wasm module (built by `scripts/build-agent.sh`).
const AGENT_WASM: &[u8] = include_bytes!("../../../target/agent.wasm");

#[allow(clippy::too_many_arguments)]
fn agent_thread(
    model: String,
    provider: String,
    max_tokens: u32,
    api_key: String,
    extra_tools: Vec<Box<dyn ox_kernel::Tool>>,
    policy: crate::policy::PolicyGuard,
    session_path: Option<PathBuf>,
    prompt_rx: mpsc::Receiver<String>,
    event_tx: mpsc::Sender<AppEvent>,
    control_tx: mpsc::Sender<AppControl>,
) {
    // Load the Wasm agent runtime + module
    let runtime = match AgentRuntime::new() {
        Ok(r) => r,
        Err(e) => {
            event_tx
                .send(AppEvent::Done(Err(format!("runtime init failed: {e}"))))
                .ok();
            return;
        }
    };
    let module = match runtime.load_module_from_bytes(AGENT_WASM) {
        Ok(m) => m,
        Err(e) => {
            event_tx
                .send(AppEvent::Done(Err(format!("agent load failed: {e}"))))
                .ok();
            return;
        }
    };

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

    // Read provider config from gate (before mounting) for transport
    let provider_config = read_provider_config_from_gate(&mut gate, &provider)
        .unwrap_or_else(|_| ProviderConfig::anthropic());
    let api_key_for_transport = read_account_key(&mut gate, &provider).unwrap_or_default();

    // Register completion tools for keyed accounts
    let send_config = provider_config.clone();
    let send_key = api_key_for_transport.clone();
    let send = Arc::new(crate::transport::make_send_fn(send_config, send_key));
    for tool in gate.create_completion_tools(send) {
        tools.register(tool);
    }

    // Build namespace
    let mut namespace = Namespace::new();
    namespace.mount(
        "system",
        Box::new(SystemProvider::new(SYSTEM_PROMPT.to_string())),
    );
    namespace.mount("history", Box::new(HistoryProvider::new()));
    namespace.mount("tools", Box::new(ToolsProvider::new(tools.schemas())));
    namespace.mount("model", Box::new(ModelProvider::new(model, max_tokens)));
    namespace.mount("gate", Box::new(gate));

    // Restore session history if a session file exists
    if let Some(ref path) = session_path {
        if path.exists() {
            match crate::session::load(path) {
                Ok(messages) => {
                    for msg in messages {
                        namespace
                            .write(&path!("history/append"), Record::parsed(json_to_value(msg)))
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

    // Main prompt loop — move tools/policy/client in and out of CliEffects each turn
    let mut tools = tools;
    let mut policy = policy;
    let mut client = reqwest::blocking::Client::new();

    while let Ok(input) = prompt_rx.recv() {
        // Write user message to history
        let user_json = serde_json::json!({"role": "user", "content": input});
        if let Err(e) = namespace.write(
            &path!("history/append"),
            Record::parsed(json_to_value(user_json)),
        ) {
            event_tx.send(AppEvent::Done(Err(e.to_string()))).ok();
            continue;
        }

        // Build effects for this turn, transferring ownership
        let effects = CliEffects {
            client,
            config: provider_config.clone(),
            api_key: api_key_for_transport.clone(),
            tools,
            policy,
            event_tx: event_tx.clone(),
            control_tx: control_tx.clone(),
            stats: PolicyStats::default(),
        };

        let host_store = HostStore::new(namespace, effects);
        let (returned_store, result) = module.run(host_store);

        // Reclaim namespace, tools, policy, and client for the next turn
        namespace = returned_store.namespace;
        client = returned_store.effects.client;
        tools = returned_store.effects.tools;
        let stats = returned_store.effects.stats.clone();
        policy = returned_store.effects.policy;

        event_tx.send(AppEvent::PolicyStats(stats)).ok();

        let done_result = match result {
            Ok(()) => Ok(String::new()),
            Err(e) => Err(e),
        };
        event_tx.send(AppEvent::Done(done_result)).ok();
    }

    // Save session on shutdown (prompt_rx disconnected)
    if let Some(ref path) = session_path {
        save_session(&mut namespace, path);
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
    let config_path =
        ox_kernel::Path::from_components(vec!["providers".to_string(), provider_name]);
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

// ---------------------------------------------------------------------------
// CliEffects — HostEffects impl for ox-runtime Wasm execution
// ---------------------------------------------------------------------------

/// Host-side effects for the CLI agent, owning tools and policy so they
/// can be transferred into/out of the HostStore each turn.
struct CliEffects {
    client: reqwest::blocking::Client,
    config: ProviderConfig,
    api_key: String,
    tools: ToolRegistry,
    policy: crate::policy::PolicyGuard,
    event_tx: mpsc::Sender<AppEvent>,
    control_tx: mpsc::Sender<AppControl>,
    stats: PolicyStats,
}

impl HostEffects for CliEffects {
    fn complete(
        &mut self,
        request: &CompletionRequest,
    ) -> Result<(Vec<StreamEvent>, u32, u32), String> {
        let tx = self.event_tx.clone();
        let (events, usage) = crate::transport::streaming_fetch(
            &self.client,
            &self.config,
            &self.api_key,
            request,
            &|event| {
                if let StreamEvent::TextDelta(text) = event {
                    tx.send(AppEvent::Agent(AgentEvent::TextDelta(text.clone())))
                        .ok();
                }
            },
        )?;
        if usage.input_tokens > 0 || usage.output_tokens > 0 {
            self.event_tx
                .send(AppEvent::Usage {
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                })
                .ok();
        }
        Ok((events, usage.input_tokens, usage.output_tokens))
    }

    fn execute_tool(&mut self, call: &ToolCall) -> Result<String, String> {
        let decision = self.policy.check(&call.name, &call.input);
        match decision {
            crate::policy::CheckResult::Allow => {
                self.stats.allowed += 1;
                self.event_tx
                    .send(AppEvent::PolicyStats(self.stats.clone()))
                    .ok();
                self.execute_tool_inner(call)
            }
            crate::policy::CheckResult::Deny(reason) => {
                self.stats.denied += 1;
                self.event_tx
                    .send(AppEvent::PolicyStats(self.stats.clone()))
                    .ok();
                Err(format!("denied: {reason}"))
            }
            crate::policy::CheckResult::Ask {
                tool,
                input_preview,
                ..
            } => {
                self.stats.asked += 1;
                self.event_tx
                    .send(AppEvent::PolicyStats(self.stats.clone()))
                    .ok();
                let (resp_tx, resp_rx) = mpsc::channel();
                self.control_tx
                    .send(AppControl::PermissionRequest {
                        tool,
                        input_preview,
                        respond: resp_tx,
                    })
                    .ok();
                match resp_rx.recv() {
                    Ok(ApprovalResponse::AllowOnce) => {
                        self.stats.allowed += 1;
                        self.execute_tool_inner(call)
                    }
                    Ok(ApprovalResponse::AllowSession) => {
                        self.policy.session_allow(&call.name, &call.input);
                        self.stats.allowed += 1;
                        self.execute_tool_inner(call)
                    }
                    Ok(ApprovalResponse::AllowAlways) => {
                        self.policy.persist_allow(&call.name, &call.input);
                        self.stats.allowed += 1;
                        self.execute_tool_inner(call)
                    }
                    Ok(ApprovalResponse::DenyOnce) => {
                        self.stats.denied += 1;
                        Err("denied by user".into())
                    }
                    Ok(ApprovalResponse::DenySession) => {
                        self.policy.session_deny(&call.name, &call.input);
                        self.stats.denied += 1;
                        Err("denied by user".into())
                    }
                    Ok(ApprovalResponse::DenyAlways) => {
                        self.policy.persist_deny(&call.name, &call.input);
                        self.stats.denied += 1;
                        Err("denied by user".into())
                    }
                    Ok(ApprovalResponse::CustomNode {
                        node,
                        sandbox,
                        scope,
                    }) => {
                        let is_allow = node_is_allow(&node);
                        match scope.as_str() {
                            "always" => {
                                if let Some((name, sb)) = sandbox {
                                    self.policy.add_sandbox(&name, sb, true);
                                }
                                self.policy.add_persistent_node(*node);
                            }
                            "session" => {
                                if let Some((name, sb)) = sandbox {
                                    self.policy.add_sandbox(&name, sb, false);
                                }
                                self.policy.add_session_node(*node);
                            }
                            _ => {} // "once"
                        }
                        if is_allow {
                            self.stats.allowed += 1;
                            self.execute_tool_inner(call)
                        } else {
                            self.stats.denied += 1;
                            Err("denied by custom rule".into())
                        }
                    }
                    Err(_) => Err("denied: TUI disconnected".into()),
                }
            }
        }
    }

    fn emit_event(&mut self, event: AgentEvent) {
        self.event_tx.send(AppEvent::Agent(event)).ok();
    }
}

impl CliEffects {
    fn execute_tool_inner(&self, call: &ToolCall) -> Result<String, String> {
        match self.tools.get(&call.name) {
            Some(tool) => tool
                .execute(call.input.clone())
                .map_err(|e| format!("error: {e}")),
            None => Err(format!("unknown tool '{}'", call.name)),
        }
    }
}

/// Check if a clash Node tree's leaf is an allow decision.
fn node_is_allow(node: &clash::policy::match_tree::Node) -> bool {
    match node {
        clash::policy::match_tree::Node::Decision(d) => d.effect() == clash::policy::Effect::Allow,
        clash::policy::match_tree::Node::Condition { children, .. } => {
            children.first().is_some_and(node_is_allow)
        }
    }
}
