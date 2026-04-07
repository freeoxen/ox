use ox_gate::{GateStore, ProviderConfig};
use ox_kernel::{AgentEvent, Reader, Record, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc;
use structfs_core_store::Writer as StructWriter;
use structfs_serde_store::from_value;

use crate::agents::AgentPool;
use crate::policy::PolicyStats;

// ---------------------------------------------------------------------------
// Modal input mode
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
#[allow(dead_code)]
pub enum InsertContext {
    /// Composing a new thread from the inbox.
    Compose,
    /// Replying to the active thread.
    Reply,
    /// Filtering the inbox.
    Search,
}

#[derive(Debug, Clone, PartialEq, Default)]
#[allow(dead_code)]
pub enum InputMode {
    #[default]
    Normal,
    Insert(InsertContext),
}

// ---------------------------------------------------------------------------
// Search / filter state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct SearchState {
    /// Saved filter chips (committed search terms).
    pub chips: Vec<String>,
    /// Current text being typed in the search bar.
    pub live_query: String,
}

impl SearchState {
    /// Commit the live query as a chip (if non-empty).
    pub fn save_chip(&mut self) {
        let trimmed = self.live_query.trim().to_string();
        if !trimmed.is_empty() {
            self.chips.push(trimmed);
        }
        self.live_query.clear();
    }

    /// Remove a chip by index.
    pub fn dismiss_chip(&mut self, idx: usize) {
        if idx < self.chips.len() {
            self.chips.remove(idx);
        }
    }

    /// Whether search has any active filters.
    pub fn is_active(&self) -> bool {
        !self.chips.is_empty() || !self.live_query.is_empty()
    }

    /// Check whether a thread (title, labels, state) matches all chips + live query.
    #[allow(dead_code)]
    pub fn matches(&self, title: &str, labels: &[String], state: &str) -> bool {
        let hay = format!(
            "{} {} {}",
            title.to_lowercase(),
            labels
                .iter()
                .map(|l| l.to_lowercase())
                .collect::<Vec<_>>()
                .join(" "),
            state.to_lowercase()
        );
        for chip in &self.chips {
            if !hay.contains(&chip.to_lowercase()) {
                return false;
            }
        }
        if !self.live_query.is_empty() && !hay.contains(&self.live_query.to_lowercase()) {
            return false;
        }
        true
    }
}

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
    SaveComplete {
        thread_id: String,
        last_seq: i64,
        last_hash: Option<String>,
        updated_at: i64,
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

/// Lightweight streaming state for in-progress agent turns.
#[derive(Debug, Clone, Default)]
pub struct StreamingTurn {
    pub text: String,
    pub tool_name: Option<String>,
    pub thinking: bool,
    pub tokens_in: u32,
    pub tokens_out: u32,
}

/// TUI-side application state — multi-thread aware.
///
/// Draw functions no longer read from App directly; they consume a `ViewState`
/// snapshot built from the broker + App borrows each frame. App retains only
/// the fields that are mutated by event handling or needed for agent control.
pub struct App {
    pub pool: AgentPool,
    pub active_thread: Option<String>, // None = inbox view
    pub thread_views: HashMap<String, ThreadView>,
    pub streaming_turns: HashMap<String, StreamingTurn>,
    // Modal mode
    pub mode: InputMode,
    pub search: SearchState,
    // Shared UI state
    pub input: String,
    pub cursor: usize,
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
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        provider: String,
        model: String,
        max_tokens: u32,
        api_key: String,
        workspace: PathBuf,
        inbox_root: PathBuf,
        no_policy: bool,
        broker: ox_broker::BrokerStore,
        rt_handle: tokio::runtime::Handle,
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
            inbox_root,
            event_tx,
            control_tx,
            broker,
            rt_handle,
        )?;

        Ok(Self {
            pool,
            active_thread: None,
            thread_views: HashMap::new(),
            streaming_turns: HashMap::new(),
            mode: InputMode::default(),
            search: SearchState::default(),
            input: String::new(),
            cursor: 0,
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

    // Mode transitions (enter_compose, enter_reply, enter_search, exit_insert,
    // go_to_inbox) are now handled by UiStore commands through the broker.

    /// Send input with explicit text (from ViewState), context-dependent on mode.
    pub fn send_input_with_text(&mut self, text: String) {
        // Temporarily set self.input so do_compose/do_reply can read it
        self.input = text;
        match self.mode.clone() {
            InputMode::Insert(InsertContext::Search) => {
                self.search.save_chip();
            }
            InputMode::Insert(InsertContext::Compose) => {
                self.do_compose();
                self.mode = InputMode::Normal;
            }
            InputMode::Insert(InsertContext::Reply) => {
                self.do_reply();
                self.mode = InputMode::Normal;
            }
            InputMode::Normal => {
                if !self.input.is_empty() {
                    if self.active_thread.is_some() {
                        self.do_reply();
                    } else {
                        self.do_compose();
                    }
                }
            }
        }
    }

    /// Create a new thread from the current input.
    fn do_compose(&mut self) {
        if self.input.is_empty() || self.active_thinking() {
            return;
        }
        let input = std::mem::take(&mut self.input);
        self.cursor = 0;
        self.input_history.push(input.clone());
        self.history_cursor = self.input_history.len();
        self.input_draft.clear();

        let title: String = input.chars().take(40).collect();
        match self.pool.create_thread(&title) {
            Ok(tid) => {
                let mut view = ThreadView::default();
                view.messages.push(ChatMessage::User(input.clone()));
                view.thinking = true;
                self.thread_views.insert(tid.clone(), view);
                self.open_thread(tid.clone());
                // scroll reset handled by broker
                self.update_thread_state(&tid, "running");
                self.pool.send_prompt(&tid, input).ok();
            }
            Err(e) => {
                eprintln!("failed to create thread: {e}");
            }
        }
    }

    /// Send a reply to the active thread.
    fn do_reply(&mut self) {
        if self.input.is_empty() || self.active_thinking() {
            return;
        }
        if let Some(tid) = self.active_thread.clone() {
            let input = std::mem::take(&mut self.input);
            self.cursor = 0;
            self.input_history.push(input.clone());
            self.history_cursor = self.input_history.len();
            self.input_draft.clear();

            let view = self.thread_views.entry(tid.clone()).or_default();
            view.messages.push(ChatMessage::User(input.clone()));
            view.thinking = true;
            // scroll reset handled by broker
            self.update_thread_state(&tid, "running");
            self.pool.send_prompt(&tid, input).ok();
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
                    // scroll reset handled by broker
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

                // Persist cumulative token count to inbox SQLite
                let total = (view.tokens_in + view.tokens_out) as i64;
                let update_path = ox_kernel::Path::from_components(vec![
                    "threads".to_string(),
                    thread_id.clone(),
                ]);
                let mut map = std::collections::BTreeMap::new();
                map.insert(
                    "token_count".to_string(),
                    structfs_core_store::Value::Integer(total),
                );
                self.pool
                    .inbox()
                    .write(
                        &update_path,
                        structfs_core_store::Record::parsed(structfs_core_store::Value::Map(map)),
                    )
                    .ok();
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

                let new_state = if result.is_ok() {
                    "waiting_for_input"
                } else {
                    "errored"
                };
                self.update_thread_state(thread_id, new_state);
            }
            AppEvent::SaveComplete {
                ref thread_id,
                last_seq,
                ref last_hash,
                updated_at,
            } => {
                let mut update = std::collections::BTreeMap::new();
                update.insert(
                    "last_seq".to_string(),
                    structfs_core_store::Value::Integer(last_seq),
                );
                if let Some(hash) = last_hash {
                    update.insert(
                        "last_hash".to_string(),
                        structfs_core_store::Value::String(hash.clone()),
                    );
                }
                update.insert(
                    "updated_at".to_string(),
                    structfs_core_store::Value::Integer(updated_at),
                );
                let update_path = ox_kernel::Path::from_components(vec![
                    "threads".to_string(),
                    thread_id.clone(),
                ]);
                self.pool
                    .inbox()
                    .write(
                        &update_path,
                        structfs_core_store::Record::parsed(structfs_core_store::Value::Map(
                            update,
                        )),
                    )
                    .ok();
            }
        }
    }

    /// Update a thread's state in ox-inbox.
    pub fn update_thread_state(&mut self, thread_id: &str, state: &str) {
        let update_path =
            ox_kernel::Path::from_components(vec!["threads".to_string(), thread_id.to_string()]);
        let mut map = std::collections::BTreeMap::new();
        map.insert(
            "thread_state".to_string(),
            structfs_core_store::Value::String(state.to_string()),
        );
        self.pool
            .inbox()
            .write(
                &update_path,
                structfs_core_store::Record::parsed(structfs_core_store::Value::Map(map)),
            )
            .ok();
    }

    // -- Tab management -------------------------------------------------------

    /// Switch to inbox view (no active thread).
    /// Open a thread view. Loads conversation history and stats from inbox if needed.
    pub fn open_thread(&mut self, thread_id: String) {
        let view = self.thread_views.entry(thread_id.clone()).or_default();
        if view.messages.is_empty() {
            self.load_thread_messages(&thread_id);
            self.load_thread_stats(&thread_id);
        }
        self.active_thread = Some(thread_id);
    }

    /// Load token count from inbox SQLite into ThreadView.
    fn load_thread_stats(&mut self, thread_id: &str) {
        let thread_path =
            ox_kernel::Path::from_components(vec!["threads".to_string(), thread_id.to_string()]);
        let record = match self.pool.inbox().read(&thread_path) {
            Ok(Some(r)) => r,
            _ => return,
        };
        let Some(structfs_core_store::Value::Map(map)) = record.as_value() else {
            return;
        };
        let token_count = match map.get("token_count") {
            Some(structfs_core_store::Value::Integer(n)) => *n,
            _ => 0,
        };
        if token_count > 0 {
            let view = self.thread_views.entry(thread_id.to_string()).or_default();
            // Split evenly as approximation — we don't store in/out separately in SQLite
            view.tokens_in = (token_count / 2) as u32;
            view.tokens_out = (token_count - token_count / 2) as u32;
        }
    }

    /// Load conversation messages from thread directory into the ThreadView.
    ///
    /// Single source of truth: reads ledger.jsonl first (new format),
    /// falls back to `{thread_id}.jsonl` (legacy format).
    fn load_thread_messages(&mut self, thread_id: &str) {
        let thread_dir = self.pool.inbox_root().join("threads").join(thread_id);
        let view = self.thread_views.entry(thread_id.to_string()).or_default();

        // Try new format: ledger.jsonl
        let ledger_path = thread_dir.join("ledger.jsonl");
        if ledger_path.exists() {
            if let Ok(entries) = ox_inbox::ledger::read_ledger(&ledger_path) {
                for entry in &entries {
                    Self::parse_json_message_into_view(view, &entry.msg);
                }
                return;
            }
        }

        // Legacy fallback: {thread_id}.jsonl
        let jsonl_path = thread_dir.join(format!("{thread_id}.jsonl"));
        if jsonl_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&jsonl_path) {
                for line in content.lines() {
                    if line.is_empty() {
                        continue;
                    }
                    if let Ok(msg) = serde_json::from_str::<serde_json::Value>(line) {
                        Self::parse_json_message_into_view(view, &msg);
                    }
                }
            }
        }
    }

    /// Parse a single JSON message (serde_json::Value) into ChatMessages for display.
    fn parse_json_message_into_view(view: &mut ThreadView, msg: &serde_json::Value) {
        let role = match msg.get("role").and_then(|r| r.as_str()) {
            Some(r) => r,
            None => return,
        };
        match role {
            "user" => {
                // Plain string content
                if let Some(s) = msg.get("content").and_then(|c| c.as_str()) {
                    view.messages.push(ChatMessage::User(s.to_string()));
                    return;
                }
                // Array content (tool_result blocks)
                if let Some(arr) = msg.get("content").and_then(|c| c.as_array()) {
                    for item in arr {
                        if item.get("type").and_then(|t| t.as_str()) == Some("tool_result") {
                            let content = item
                                .get("content")
                                .and_then(|c| c.as_str())
                                .unwrap_or("")
                                .to_string();
                            view.messages.push(ChatMessage::ToolResult {
                                name: "tool".to_string(),
                                output: content,
                            });
                        }
                    }
                }
            }
            "assistant" => {
                // Plain string content
                if let Some(s) = msg.get("content").and_then(|c| c.as_str()) {
                    view.messages
                        .push(ChatMessage::AssistantChunk(s.to_string()));
                    return;
                }
                // Array of content blocks
                if let Some(blocks) = msg.get("content").and_then(|c| c.as_array()) {
                    let mut text = String::new();
                    for block in blocks {
                        match block.get("type").and_then(|t| t.as_str()) {
                            Some("text") => {
                                if let Some(s) = block.get("text").and_then(|t| t.as_str()) {
                                    text.push_str(s);
                                }
                            }
                            Some("tool_use") => {
                                if !text.is_empty() {
                                    view.messages.push(ChatMessage::AssistantChunk(
                                        std::mem::take(&mut text),
                                    ));
                                }
                                let name = block
                                    .get("name")
                                    .and_then(|n| n.as_str())
                                    .unwrap_or("unknown")
                                    .to_string();
                                view.messages.push(ChatMessage::ToolCall { name });
                            }
                            _ => {}
                        }
                    }
                    if !text.is_empty() {
                        view.messages.push(ChatMessage::AssistantChunk(text));
                    }
                }
            }
            _ => {}
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
    #[allow(dead_code)]
    pub fn active_view(&self) -> Option<&ThreadView> {
        self.active_thread
            .as_ref()
            .and_then(|tid| self.thread_views.get(tid))
    }

    /// Drain agent events, updating both streaming_turns and thread_views.
    pub fn drain_agent_events(&mut self) {
        while let Ok(event) = self.event_rx.try_recv() {
            self.update_streaming(&event);
            self.handle_event(event);
        }
    }

    fn update_streaming(&mut self, event: &AppEvent) {
        match event {
            AppEvent::Agent { thread_id, event } => {
                let st = self.streaming_turns.entry(thread_id.clone()).or_default();
                match event {
                    AgentEvent::TurnStart => {
                        st.thinking = true;
                        st.text.clear();
                        st.tool_name = None;
                    }
                    AgentEvent::TextDelta(text) => {
                        st.text.push_str(text);
                    }
                    AgentEvent::ToolCallStart { name } => {
                        st.tool_name = Some(name.clone());
                    }
                    AgentEvent::ToolCallResult { .. } => {
                        st.tool_name = None;
                    }
                    AgentEvent::TurnEnd => {
                        st.thinking = false;
                    }
                    AgentEvent::Error(_) => {
                        st.thinking = false;
                    }
                }
            }
            AppEvent::Usage {
                thread_id,
                input_tokens,
                output_tokens,
            } => {
                let st = self.streaming_turns.entry(thread_id.clone()).or_default();
                st.tokens_in += input_tokens;
                st.tokens_out += output_tokens;
            }
            AppEvent::Done { thread_id, .. } => {
                self.streaming_turns.remove(thread_id);
            }
            _ => {}
        }
    }
}

// ---------------------------------------------------------------------------
// Helper functions (pub(crate) for use by agents.rs)
// ---------------------------------------------------------------------------

/// Read ProviderConfig from a GateStore before it's mounted in the namespace.
#[allow(dead_code)]
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
#[allow(dead_code)]
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
