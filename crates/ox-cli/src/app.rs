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
pub enum InsertContext {
    /// Composing a new thread from the inbox.
    Compose,
    /// Replying to the active thread.
    Reply,
    /// Filtering the inbox.
    Search,
}

#[derive(Debug, Clone, PartialEq, Default)]
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
    pub thread_views: HashMap<String, ThreadView>,
    // Modal mode
    pub mode: InputMode,
    pub search: SearchState,
    pub selected_row: usize,
    pub inbox_scroll: usize,
    /// Cached visible threads — refreshed once per frame via refresh_visible_threads().
    pub cached_threads: Vec<(String, String, String, Vec<String>, i64)>,
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
            inbox_root,
            event_tx,
            control_tx,
        )?;

        Ok(Self {
            pool,
            active_thread: None,
            thread_views: HashMap::new(),
            mode: InputMode::default(),
            search: SearchState::default(),
            selected_row: 0,
            inbox_scroll: 0,
            cached_threads: Vec::new(),
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

    // -- Mode transitions -----------------------------------------------------

    /// Enter insert mode for composing a new thread (from inbox).
    pub fn enter_compose(&mut self) {
        self.mode = InputMode::Insert(InsertContext::Compose);
        self.input.clear();
        self.cursor = 0;
    }

    /// Enter insert mode for replying to the active thread.
    pub fn enter_reply(&mut self) {
        if self.active_thread.is_some() {
            self.mode = InputMode::Insert(InsertContext::Reply);
            self.input.clear();
            self.cursor = 0;
        }
    }

    /// Enter insert mode for search/filtering (inbox only).
    pub fn enter_search(&mut self) {
        self.mode = InputMode::Insert(InsertContext::Search);
    }

    /// Exit insert mode back to Normal.
    pub fn exit_insert(&mut self) {
        self.mode = InputMode::Normal;
    }

    /// Send the current input, context-dependent on mode.
    ///
    /// - Compose: create a new thread, stay in inbox, back to Normal.
    /// - Reply: send to the active thread, back to Normal.
    /// - Search: save chip, stay in Search insert.
    /// - Normal with text: infer (reply if in thread, compose if inbox).
    pub fn send_input(&mut self) {
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
                self.scroll = 0;
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
            self.scroll = 0;
            self.update_thread_state(&tid, "running");
            self.pool.send_prompt(&tid, input).ok();
        }
    }

    /// Refresh the cached visible threads from ox-inbox. Call once per frame.
    pub fn refresh_visible_threads(&mut self) {
        self.cached_threads = self.get_visible_threads();
        // Clamp selected_row
        if !self.cached_threads.is_empty() {
            self.selected_row = self.selected_row.min(self.cached_threads.len() - 1);
        } else {
            self.selected_row = 0;
        }
    }

    /// Ensure selected_row is visible by adjusting inbox_scroll.
    pub fn ensure_selected_visible(&mut self, viewport_height: usize) {
        let row_height = 2; // 2 lines per inbox row
        let visible_rows = viewport_height / row_height;
        if visible_rows == 0 {
            return;
        }
        if self.selected_row < self.inbox_scroll {
            self.inbox_scroll = self.selected_row;
        } else if self.selected_row >= self.inbox_scroll + visible_rows {
            self.inbox_scroll = self.selected_row - visible_rows + 1;
        }
    }

    /// Open the thread at the currently selected inbox row.
    pub fn open_selected_thread(&mut self) {
        if let Some((id, ..)) = self.cached_threads.get(self.selected_row) {
            let id = id.clone();
            self.thread_views.entry(id.clone()).or_default();
            self.open_thread(id);
        }
    }

    /// Mark the selected inbox thread as done.
    pub fn archive_selected_thread(&mut self) {
        if let Some((id, ..)) = self.cached_threads.get(self.selected_row) {
            let id = id.clone();
            let update_path =
                ox_kernel::Path::from_components(vec!["threads".to_string(), id.clone()]);
            let mut map = std::collections::BTreeMap::new();
            map.insert(
                "inbox_state".to_string(),
                structfs_core_store::Value::String("done".to_string()),
            );
            self.pool
                .inbox()
                .write(
                    &update_path,
                    structfs_core_store::Record::parsed(structfs_core_store::Value::Map(map)),
                )
                .ok();
            // Refresh and clamp
            self.refresh_visible_threads();
        }
    }

    /// Get visible threads filtered by search, sorted by state priority.
    ///
    /// Returns `(id, title, state, labels, token_count)` tuples.
    pub fn get_visible_threads(&mut self) -> Vec<(String, String, String, Vec<String>, i64)> {
        let threads_path = structfs_core_store::path!("threads");
        let raw = match self.pool.inbox().read(&threads_path) {
            Ok(Some(record)) => record,
            _ => return Vec::new(),
        };
        let value = match raw.as_value() {
            Some(v) => v.clone(),
            None => return Vec::new(),
        };
        let arr = match value {
            structfs_core_store::Value::Array(a) => a,
            _ => return Vec::new(),
        };

        let mut result: Vec<(String, String, String, Vec<String>, i64)> = Vec::new();
        for item in &arr {
            if let structfs_core_store::Value::Map(map) = item {
                let id = match map.get("id") {
                    Some(structfs_core_store::Value::String(s)) => s.clone(),
                    _ => continue,
                };
                let title = match map.get("title") {
                    Some(structfs_core_store::Value::String(s)) => s.clone(),
                    _ => String::new(),
                };
                let state = match map.get("thread_state") {
                    Some(structfs_core_store::Value::String(s)) => s.clone(),
                    _ => "running".to_string(),
                };
                let labels = match map.get("labels") {
                    Some(structfs_core_store::Value::Array(arr)) => arr
                        .iter()
                        .filter_map(|v| {
                            if let structfs_core_store::Value::String(s) = v {
                                Some(s.clone())
                            } else {
                                None
                            }
                        })
                        .collect(),
                    _ => Vec::new(),
                };
                let token_count = match map.get("token_count") {
                    Some(structfs_core_store::Value::Integer(n)) => *n,
                    _ => 0,
                };
                if self.search.matches(&title, &labels, &state) {
                    result.push((id, title, state, labels, token_count));
                }
            }
        }

        // Sort by state priority: blocked > errored > waiting > running > completed
        fn state_priority(s: &str) -> u8 {
            match s {
                "blocked_on_approval" => 0,
                "errored" => 1,
                "waiting_for_input" => 2,
                "running" => 3,
                "completed" => 4,
                _ => 5,
            }
        }
        result.sort_by_key(|(_, _, state, _, _)| state_priority(state));
        result
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
    pub fn go_to_inbox(&mut self) {
        self.active_thread = None;
    }

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

    /// Load conversation messages from ox-inbox JSONL into the ThreadView.
    fn load_thread_messages(&mut self, thread_id: &str) {
        let msg_path = ox_kernel::Path::from_components(vec![
            "threads".to_string(),
            thread_id.to_string(),
            "messages".to_string(),
        ]);
        let record = match self.pool.inbox().read(&msg_path) {
            Ok(Some(r)) => r,
            _ => return,
        };
        let Some(structfs_core_store::Value::Array(messages)) = record.as_value() else {
            return;
        };

        let view = self.thread_views.entry(thread_id.to_string()).or_default();
        for msg_val in messages {
            let structfs_core_store::Value::Map(map) = msg_val else {
                continue;
            };
            let role = match map.get("role") {
                Some(structfs_core_store::Value::String(s)) => s.as_str(),
                _ => continue,
            };
            match role {
                "user" => {
                    let content = match map.get("content") {
                        Some(structfs_core_store::Value::String(s)) => s.clone(),
                        _ => continue,
                    };
                    view.messages.push(ChatMessage::User(content));
                }
                "assistant" => {
                    // Assistant content is an array of blocks
                    let blocks = match map.get("content") {
                        Some(structfs_core_store::Value::Array(arr)) => arr,
                        // Could also be a plain string
                        Some(structfs_core_store::Value::String(s)) => {
                            view.messages.push(ChatMessage::AssistantChunk(s.clone()));
                            continue;
                        }
                        _ => continue,
                    };
                    let mut text = String::new();
                    for block in blocks {
                        let structfs_core_store::Value::Map(bmap) = block else {
                            continue;
                        };
                        match bmap.get("type") {
                            Some(structfs_core_store::Value::String(t)) if t == "text" => {
                                if let Some(structfs_core_store::Value::String(s)) =
                                    bmap.get("text")
                                {
                                    text.push_str(s);
                                }
                            }
                            Some(structfs_core_store::Value::String(t)) if t == "tool_use" => {
                                // Flush accumulated text
                                if !text.is_empty() {
                                    view.messages.push(ChatMessage::AssistantChunk(
                                        std::mem::take(&mut text),
                                    ));
                                }
                                let name = match bmap.get("name") {
                                    Some(structfs_core_store::Value::String(s)) => s.clone(),
                                    _ => "unknown".to_string(),
                                };
                                view.messages.push(ChatMessage::ToolCall { name });
                            }
                            _ => {}
                        }
                    }
                    if !text.is_empty() {
                        view.messages.push(ChatMessage::AssistantChunk(text));
                    }
                }
                _ => {
                    // Tool results — role="user" with content array of tool_result blocks
                    // Already handled by the "user" case for plain strings.
                    // For tool_result arrays, show as tool results.
                    if let Some(structfs_core_store::Value::Array(results)) = map.get("content") {
                        for result in results {
                            let structfs_core_store::Value::Map(rmap) = result else {
                                continue;
                            };
                            if let Some(structfs_core_store::Value::String(content)) =
                                rmap.get("content")
                            {
                                view.messages.push(ChatMessage::ToolResult {
                                    name: "tool".to_string(),
                                    output: content.clone(),
                                });
                            }
                        }
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
    #[allow(dead_code)]
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
