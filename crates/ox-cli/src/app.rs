use ox_gate::{GateStore, ProviderConfig};
use ox_kernel::{AgentEvent, Reader, Record, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;
use structfs_serde_store::from_value;

use crate::agents::AgentPool;
use crate::policy::PolicyStats;

/// Events flowing from agent workers to the TUI, tagged with thread_id.
#[derive(Debug, Clone)]
pub enum AppEvent {
    Agent {
        thread_id: String,
        event: AgentEvent,
    },
    Usage {
        thread_id: String,
        input_tokens: u32,
        output_tokens: u32,
    },
    PolicyStats {
        thread_id: String,
        stats: PolicyStats,
    },
    Done {
        thread_id: String,
        result: Result<String, String>,
    },
}

/// Non-Clone control event — carries the oneshot response channel.
pub enum AppControl {
    PermissionRequest {
        thread_id: String,
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
    /// Thread that requested this approval (used for routing in multi-thread).
    #[allow(dead_code)]
    pub thread_id: String,
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

/// Per-thread UI view state.
#[derive(Debug, Clone, Default)]
pub struct ThreadView {
    pub messages: Vec<ChatMessage>,
    pub thinking: bool,
    pub tokens_in: u32,
    pub tokens_out: u32,
    pub policy_stats: PolicyStats,
}

/// TUI-side application state — multi-thread aware.
pub struct App {
    pub pool: AgentPool,
    pub active_thread: Option<String>, // None = inbox view
    pub tabs: Vec<String>,             // open thread tabs
    pub thread_views: HashMap<String, ThreadView>,
    // Shared UI state
    pub input: String,
    pub cursor: usize,
    pub scroll: u16,
    pub should_quit: bool,
    pub model: String,
    pub provider: String,
    pub event_rx: mpsc::Receiver<AppEvent>,
    pub control_rx: mpsc::Receiver<AppControl>,
    // Input history
    pub input_history: Vec<String>,
    history_cursor: usize,
    input_draft: String,
    // Modals
    pub pending_approval: Option<ApprovalState>,
    pub pending_customize: Option<CustomizeState>,
}

impl App {
    /// Create the App, initializing the AgentPool.
    pub fn new(
        provider: String,
        model: String,
        max_tokens: u32,
        api_key: String,
        workspace: PathBuf,
        inbox_root: PathBuf,
        no_policy: bool,
    ) -> Result<Self, String> {
        let (event_tx, event_rx) = mpsc::channel::<AppEvent>();
        let (control_tx, control_rx) = mpsc::channel::<AppControl>();

        let inbox = ox_inbox::InboxStore::open(&inbox_root).map_err(|e| e.to_string())?;

        let pool = AgentPool::new(
            model.clone(),
            provider.clone(),
            max_tokens,
            api_key,
            workspace,
            no_policy,
            inbox,
            event_tx,
            control_tx,
        )?;

        Ok(Self {
            pool,
            active_thread: None,
            tabs: Vec::new(),
            thread_views: HashMap::new(),
            input: String::new(),
            cursor: 0,
            scroll: 0,
            should_quit: false,
            model,
            provider,
            event_rx,
            control_rx,
            input_history: Vec::new(),
            history_cursor: 0,
            input_draft: String::new(),
            pending_approval: None,
            pending_customize: None,
        })
    }

    /// Submit the current input as a user prompt.
    ///
    /// If an active thread is selected, sends to that thread.
    /// If on the inbox view (active_thread is None), creates a new thread
    /// and opens it as a tab.
    pub fn submit(&mut self) {
        if self.input.is_empty() || self.active_thinking() {
            return;
        }
        let input = std::mem::take(&mut self.input);
        self.cursor = 0;
        self.input_history.push(input.clone());
        self.history_cursor = self.input_history.len();
        self.input_draft.clear();

        match &self.active_thread {
            Some(tid) => {
                let view = self.thread_views.entry(tid.clone()).or_default();
                view.messages.push(ChatMessage::User(input.clone()));
                view.thinking = true;
                self.scroll = 0;
                self.pool.send_prompt(tid, input).ok();
            }
            None => {
                // Create a new thread: use first 40 chars of input as title
                let title: String = input.chars().take(40).collect();
                match self.pool.create_thread(&title) {
                    Ok(tid) => {
                        let mut view = ThreadView::default();
                        view.messages.push(ChatMessage::User(input.clone()));
                        view.thinking = true;
                        self.thread_views.insert(tid.clone(), view);
                        self.open_thread(tid.clone());
                        self.scroll = 0;
                        self.pool.send_prompt(&tid, input).ok();
                    }
                    Err(e) => {
                        // Show error in a transient way — push to a dummy view?
                        // For now, just ignore; the user will see nothing happen.
                        eprintln!("failed to create thread: {e}");
                    }
                }
            }
        }
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

    /// Process a single AppEvent, routing to the correct ThreadView.
    pub fn handle_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::Agent {
                ref thread_id,
                ref event,
            } => {
                let view = self.thread_views.entry(thread_id.clone()).or_default();
                if self.active_thread.as_ref() == Some(thread_id) {
                    self.scroll = 0;
                }
                match event {
                    AgentEvent::TextDelta(text) => {
                        if let Some(&mut ChatMessage::AssistantChunk(ref mut s)) =
                            view.messages.last_mut()
                        {
                            s.push_str(text);
                        } else {
                            view.messages
                                .push(ChatMessage::AssistantChunk(text.clone()));
                        }
                    }
                    AgentEvent::ToolCallStart { name } => {
                        view.messages
                            .push(ChatMessage::ToolCall { name: name.clone() });
                    }
                    AgentEvent::ToolCallResult { name, result } => {
                        view.messages.push(ChatMessage::ToolResult {
                            name: name.clone(),
                            output: result.clone(),
                        });
                    }
                    AgentEvent::TurnStart | AgentEvent::TurnEnd => {}
                    AgentEvent::Error(e) => {
                        view.messages.push(ChatMessage::Error(e.clone()));
                    }
                }
            }
            AppEvent::Usage {
                ref thread_id,
                input_tokens,
                output_tokens,
            } => {
                let view = self.thread_views.entry(thread_id.clone()).or_default();
                view.tokens_in += input_tokens;
                view.tokens_out += output_tokens;
            }
            AppEvent::PolicyStats {
                ref thread_id,
                ref stats,
            } => {
                let view = self.thread_views.entry(thread_id.clone()).or_default();
                view.policy_stats = stats.clone();
            }
            AppEvent::Done {
                ref thread_id,
                ref result,
            } => {
                let view = self.thread_views.entry(thread_id.clone()).or_default();
                if let Err(e) = result {
                    view.messages.push(ChatMessage::Error(e.clone()));
                }
                view.thinking = false;
            }
        }
    }

    // -- Tab management -------------------------------------------------------

    /// Switch to inbox view (no active thread).
    pub fn go_to_inbox(&mut self) {
        self.active_thread = None;
    }

    /// Open a thread as the active tab. Adds to tabs if not already present.
    pub fn open_thread(&mut self, thread_id: String) {
        if !self.tabs.contains(&thread_id) {
            self.tabs.push(thread_id.clone());
        }
        self.active_thread = Some(thread_id);
    }

    /// Close the currently active tab and switch to the next or inbox.
    pub fn close_current_tab(&mut self) {
        if let Some(ref tid) = self.active_thread {
            if let Some(idx) = self.tabs.iter().position(|t| t == tid) {
                self.tabs.remove(idx);
                if self.tabs.is_empty() {
                    self.active_thread = None;
                } else {
                    let new_idx = idx.min(self.tabs.len() - 1);
                    self.active_thread = Some(self.tabs[new_idx].clone());
                }
            } else {
                self.active_thread = None;
            }
        }
    }

    /// Switch to the next tab (wraps around). If on inbox, goes to first tab.
    pub fn next_tab(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        match &self.active_thread {
            None => {
                self.active_thread = Some(self.tabs[0].clone());
            }
            Some(tid) => {
                if let Some(idx) = self.tabs.iter().position(|t| t == tid) {
                    let next = (idx + 1) % self.tabs.len();
                    self.active_thread = Some(self.tabs[next].clone());
                }
            }
        }
    }

    /// Switch to the previous tab (wraps around). If on first tab, goes to inbox.
    pub fn prev_tab(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        match &self.active_thread {
            None => {
                self.active_thread = Some(self.tabs[self.tabs.len() - 1].clone());
            }
            Some(tid) => {
                if let Some(idx) = self.tabs.iter().position(|t| t == tid) {
                    if idx == 0 {
                        self.active_thread = None;
                    } else {
                        self.active_thread = Some(self.tabs[idx - 1].clone());
                    }
                }
            }
        }
    }

    /// Whether the active thread is currently thinking.
    pub fn active_thinking(&self) -> bool {
        match &self.active_thread {
            Some(tid) => self.thread_views.get(tid).is_some_and(|v| v.thinking),
            None => false,
        }
    }

    /// Get the active thread's view (if any).
    pub fn active_view(&self) -> Option<&ThreadView> {
        self.active_thread
            .as_ref()
            .and_then(|tid| self.thread_views.get(tid))
    }
}

// ---------------------------------------------------------------------------
// Helper functions (pub(crate) for use by agents.rs)
// ---------------------------------------------------------------------------

/// Read ProviderConfig from a GateStore before it's mounted in the namespace.
pub(crate) fn read_provider_config_from_gate(
    gate: &mut GateStore,
    account_name: &str,
) -> Result<ProviderConfig, String> {
    let provider_path = ox_kernel::Path::from_components(vec![
        "accounts".to_string(),
        account_name.to_string(),
        "provider".to_string(),
    ]);
    let provider_name = match gate.read(&provider_path) {
        Ok(Some(Record::Parsed(Value::String(s)))) => s,
        _ => account_name.to_string(),
    };
    let config_path =
        ox_kernel::Path::from_components(vec!["providers".to_string(), provider_name]);
    match gate.read(&config_path) {
        Ok(Some(Record::Parsed(v))) => from_value(v).map_err(|e| e.to_string()),
        _ => Err("provider config not found".into()),
    }
}

/// Read API key from a GateStore before it's mounted.
pub(crate) fn read_account_key(gate: &mut GateStore, account_name: &str) -> Result<String, String> {
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

/// Check if a clash Node tree's leaf is an allow decision.
pub(crate) fn node_is_allow(node: &clash::policy::match_tree::Node) -> bool {
    match node {
        clash::policy::match_tree::Node::Decision(d) => d.effect() == clash::policy::Effect::Allow,
        clash::policy::match_tree::Node::Condition { children, .. } => {
            children.first().is_some_and(node_is_allow)
        }
    }
}
