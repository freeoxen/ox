use std::path::PathBuf;
use structfs_core_store::Writer as _;

use crate::agents::AgentPool;

/// TUI-side application state — multi-thread aware.
///
/// Draw functions no longer read from App directly; they consume a `ViewState`
/// snapshot built from the broker + App borrows each frame. App retains only
/// the fields that are mutated by event handling or needed for agent control.
pub struct App {
    pub pool: AgentPool,
    // Input history
    pub input_history: Vec<String>,
    history_cursor: usize,
    input_draft: String,
}

impl App {
    /// Create the App, initializing the AgentPool.
    pub fn new(
        workspace: PathBuf,
        inbox_root: PathBuf,
        no_policy: bool,
        broker: ox_broker::BrokerStore,
        rt_handle: tokio::runtime::Handle,
    ) -> Result<Self, String> {
        let mut inbox = ox_inbox::InboxStore::open(&inbox_root).map_err(|e| e.to_string())?;

        // Load persistent input history from ox.db before moving inbox into pool
        let input_history: Vec<String> = {
            use structfs_core_store::Reader;
            let path = structfs_core_store::Path::parse("inputs/recent/200").unwrap();
            match inbox.read(&path) {
                Ok(Some(record)) => {
                    if let Some(structfs_core_store::Value::Array(arr)) = record.as_value() {
                        arr.iter()
                            .rev() // recent_inputs returns newest first; we want oldest first
                            .filter_map(|v| match v {
                                structfs_core_store::Value::Map(m) => match m.get("text") {
                                    Some(structfs_core_store::Value::String(s)) => Some(s.clone()),
                                    _ => None,
                                },
                                _ => None,
                            })
                            .collect()
                    } else {
                        Vec::new()
                    }
                }
                _ => Vec::new(),
            }
        };

        let pool = AgentPool::new(workspace, no_policy, inbox, inbox_root, broker, rt_handle)?;
        let history_cursor = input_history.len();

        Ok(Self {
            pool,
            input_history,
            history_cursor,
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
        mode: ox_types::Mode,
        insert_context: Option<ox_types::InsertContext>,
        active_thread: Option<&str>,
    ) -> Option<String> {
        use ox_types::{InsertContext, Mode};
        if text.is_empty() {
            return None;
        }
        match (mode, insert_context) {
            (Mode::Insert, Some(InsertContext::Compose)) | (Mode::Normal, None)
                if active_thread.is_none() =>
            {
                self.do_compose(text)
            }
            (Mode::Insert, Some(InsertContext::Reply)) | (Mode::Normal, _)
                if active_thread.is_some() =>
            {
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
                self.update_thread_state(&tid, ox_types::ThreadState::Running);
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

        self.update_thread_state(thread_id, ox_types::ThreadState::Running);
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
    pub fn update_thread_state(&mut self, thread_id: &str, state: ox_types::ThreadState) {
        let tid = match ox_kernel::PathComponent::try_new(thread_id) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "invalid thread id for path");
                return;
            }
        };
        let update_path = ox_path::oxpath!("threads", tid);
        let update = ox_types::UpdateThread {
            id: None,
            thread_state: Some(state),
            inbox_state: None,
            updated_at: None,
        };
        let val = structfs_serde_store::to_value(&update).unwrap();
        self.pool
            .inbox()
            .write(&update_path, structfs_core_store::Record::parsed(val))
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
