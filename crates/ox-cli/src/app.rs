use std::path::PathBuf;
use structfs_core_store::Writer as StructWriter;

use crate::agents::AgentPool;

/// TUI-side application state — multi-thread aware.
///
/// Draw functions no longer read from App directly; they consume a `ViewState`
/// snapshot built from the broker + App borrows each frame. App retains only
/// the fields that are mutated by event handling or needed for agent control.
pub struct App {
    pub pool: AgentPool,
    pub model: String,
    pub provider: String,
    // Input history
    pub input_history: Vec<String>,
    history_cursor: usize,
    input_draft: String,
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
            model,
            provider,
            input_history: Vec::new(),
            history_cursor: 0,
            input_draft: String::new(),
        })
    }

    // Mode transitions (enter_compose, enter_reply, enter_search, exit_insert,
    // go_to_inbox) are now handled by UiStore commands through the broker.

    /// Send input with explicit context from ViewState.
    /// Returns Some(thread_id) if a new thread was composed.
    pub fn send_input_with_text(
        &mut self,
        text: String,
        mode: &str,
        insert_context: Option<&str>,
        active_thread: Option<&str>,
    ) -> Option<String> {
        if text.is_empty() {
            return None;
        }
        match (mode, insert_context) {
            ("insert", Some("compose")) | ("normal", None) if active_thread.is_none() => {
                self.do_compose(text)
            }
            ("insert", Some("reply")) | ("normal", _) if active_thread.is_some() => {
                self.do_reply(text, active_thread.unwrap());
                None
            }
            _ => None,
        }
    }

    fn do_compose(&mut self, input: String) -> Option<String> {
        self.input_history.push(input.clone());
        self.history_cursor = self.input_history.len();
        self.input_draft.clear();

        let title: String = input.chars().take(40).collect();
        match self.pool.create_thread(&title) {
            Ok(tid) => {
                self.update_thread_state(&tid, "running");
                self.pool.send_prompt(&tid, input).ok();
                Some(tid)
            }
            Err(e) => {
                eprintln!("failed to create thread: {e}");
                None
            }
        }
    }

    fn do_reply(&mut self, input: String, thread_id: &str) {
        self.input_history.push(input.clone());
        self.history_cursor = self.input_history.len();
        self.input_draft.clear();

        self.update_thread_state(thread_id, "running");
        self.pool.send_prompt(thread_id, input).ok();
    }

    /// Navigate input history up (older). Returns (new_input, new_cursor) or None.
    pub fn history_up(&mut self, current_input: &str) -> Option<(String, usize)> {
        if self.input_history.is_empty() {
            return None;
        }
        if self.history_cursor == self.input_history.len() {
            self.input_draft = current_input.to_string();
        }
        if self.history_cursor > 0 {
            self.history_cursor -= 1;
            let text = self.input_history[self.history_cursor].clone();
            let cursor = text.len();
            Some((text, cursor))
        } else {
            None
        }
    }

    /// Navigate input history down (newer). Returns (new_input, new_cursor) or None.
    pub fn history_down(&mut self) -> Option<(String, usize)> {
        if self.history_cursor < self.input_history.len() {
            self.history_cursor += 1;
            let text = if self.history_cursor == self.input_history.len() {
                self.input_draft.clone()
            } else {
                self.input_history[self.history_cursor].clone()
            };
            let cursor = text.len();
            Some((text, cursor))
        } else {
            None
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
