use std::path::PathBuf;
use structfs_core_store::Writer as StructWriter;

use crate::agents::AgentPool;

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

/// Per-thread rendering state — built from broker data each frame.
#[derive(Debug, Clone, Default)]
pub struct ThreadView {
    pub messages: Vec<ChatMessage>,
    pub thinking: bool,
}

/// A message visible in the conversation.
#[derive(Debug, Clone)]
pub enum ChatMessage {
    User(String),
    AssistantChunk(String),
    ToolCall {
        name: String,
    },
    ToolResult {
        name: String,
        output: String,
    },
    #[allow(dead_code)]
    Error(String),
}

/// Approval options for the permission dialog.
pub const APPROVAL_OPTIONS: [(&str, &str); 6] = [
    ("Allow once          (y)", "allow_once"),
    ("Allow for session   (s)", "allow_session"),
    ("Allow always        (a)", "allow_always"),
    ("Deny once           (n)", "deny_once"),
    ("Deny for session      ", "deny_session"),
    ("Deny always         (d)", "deny_always"),
];

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

/// TUI-side application state — multi-thread aware.
///
/// Draw functions no longer read from App directly; they consume a `ViewState`
/// snapshot built from the broker + App borrows each frame. App retains only
/// the fields that are mutated by event handling or needed for agent control.
pub struct App {
    pub pool: AgentPool,
    pub active_thread: Option<String>, // None = inbox view
    // Modal mode
    pub mode: InputMode,
    pub search: SearchState,
    // Shared UI state
    pub input: String,
    pub cursor: usize,
    pub model: String,
    pub provider: String,
    // Input history
    pub input_history: Vec<String>,
    history_cursor: usize,
    input_draft: String,
    // Modals
    pub approval_selected: usize,
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
            broker,
            rt_handle,
        )?;

        Ok(Self {
            pool,
            active_thread: None,
            mode: InputMode::default(),
            search: SearchState::default(),
            input: String::new(),
            cursor: 0,
            model,
            provider,
            input_history: Vec::new(),
            history_cursor: 0,
            input_draft: String::new(),
            approval_selected: 0,
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
        if self.input.is_empty() {
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
        if self.input.is_empty() {
            return;
        }
        if let Some(tid) = self.active_thread.clone() {
            let input = std::mem::take(&mut self.input);
            self.cursor = 0;
            self.input_history.push(input.clone());
            self.history_cursor = self.input_history.len();
            self.input_draft.clear();

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

    /// Open a thread view.
    pub fn open_thread(&mut self, thread_id: String) {
        self.active_thread = Some(thread_id);
    }
}

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Check if a clash Node tree's leaf is an allow decision.
#[allow(dead_code)]
pub(crate) fn node_is_allow(node: &clash::policy::match_tree::Node) -> bool {
    match node {
        clash::policy::match_tree::Node::Decision(d) => d.effect() == clash::policy::Effect::Allow,
        clash::policy::match_tree::Node::Condition { children, .. } => {
            children.first().is_some_and(node_is_allow)
        }
    }
}
