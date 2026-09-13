use std::path::PathBuf;

use ox_executor::ExecutionCore;

/// TUI-side application state — multi-thread aware.
///
/// Draw functions no longer read from App directly; they consume a `ViewState`
/// snapshot built from the broker + App borrows each frame. App retains only
/// the fields that are mutated by event handling or needed for agent control.
pub struct App {
    pub pool: ExecutionCore,
    /// Broker client for all store access (inbox, threads, search).
    pub broker_client: ox_broker::ClientHandle,
    inbox_root: PathBuf,
    /// Offset into input history (0 = at the draft, N = Nth entry from newest).
    history_offset: usize,
    input_draft: String,
}

impl App {
    /// Create the App, initializing the shared execution core.
    ///
    /// `rt_handle` is forwarded to [`ExecutionCore`] — the sync OS-thread
    /// workers it spawns need it to bridge their `block_on` calls
    /// back to the async broker. App itself has no direct need for a
    /// runtime handle: its own broker methods are `async fn`.
    pub fn new(
        workspace: PathBuf,
        inbox_root: PathBuf,
        no_policy: bool,
        broker: ox_broker::BrokerStore,
        rt_handle: tokio::runtime::Handle,
    ) -> Result<Self, String> {
        Self::new_with_transport_factory(workspace, inbox_root, no_policy, broker, rt_handle, None)
    }

    /// Same as [`App::new`], but worker threads are given a test-supplied
    /// completion transport in place of the built-in reqwest transport.
    pub fn new_with_transport_factory(
        workspace: PathBuf,
        inbox_root: PathBuf,
        no_policy: bool,
        broker: ox_broker::BrokerStore,
        rt_handle: tokio::runtime::Handle,
        transport_factory: Option<ox_executor::test_support::TransportFactory>,
    ) -> Result<Self, String> {
        Self::new_with_test_hooks(
            workspace,
            inbox_root,
            no_policy,
            broker,
            rt_handle,
            transport_factory,
            None,
        )
    }

    /// Test-only constructor that additionally accepts a
    /// [`ox_executor::test_support::ToolInjector`] so the crash harness can wire
    /// counter-backed native tools for the Task 3d post-crash-reconfirm
    /// E2E suite. Production callers should use [`App::new`].
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_test_hooks(
        workspace: PathBuf,
        inbox_root: PathBuf,
        no_policy: bool,
        broker: ox_broker::BrokerStore,
        rt_handle: tokio::runtime::Handle,
        transport_factory: Option<ox_executor::test_support::TransportFactory>,
        tool_injector: Option<ox_executor::test_support::ToolInjector>,
    ) -> Result<Self, String> {
        let inbox = ox_inbox::InboxStore::open(&inbox_root).map_err(|e| e.to_string())?;
        let broker_client = broker.client();
        let pool = ExecutionCore::new_with_test_hooks(
            workspace,
            no_policy,
            inbox,
            inbox_root.clone(),
            broker,
            rt_handle,
            transport_factory,
            tool_injector,
        )?;

        Ok(Self {
            pool,
            broker_client,
            inbox_root,
            history_offset: 0,
            input_draft: String::new(),
        })
    }

    // Mode transitions (enter_compose, enter_reply, enter_search, exit_insert,
    // go_to_inbox) are now handled by UiStore commands through the broker.

    /// Send input locally with explicit context from ViewState.
    /// Returns the created thread ID for a new conversation.
    pub async fn send_input_with_text(
        &mut self,
        text: String,
        mode: ox_types::Mode,
        insert_context: Option<ox_types::InsertContext>,
        active_thread: Option<&str>,
    ) -> Option<String> {
        self.send_local_input_with_text(text, mode, insert_context, active_thread)
            .await
            .thread_id
    }

    pub(crate) async fn send_input_with_text_to(
        &mut self,
        text: String,
        mode: ox_types::Mode,
        insert_context: Option<ox_types::InsertContext>,
        active_thread: Option<&str>,
        target: crate::action_executor::SendTarget,
    ) -> SendResult {
        if target == crate::action_executor::SendTarget::Local {
            let accepted = input_context_accepts_local(mode, insert_context, active_thread)
                && !text.is_empty();
            let thread_id = self
                .send_input_with_text(text, mode, insert_context, active_thread)
                .await;
            return SendResult {
                accepted,
                thread_id,
            };
        }

        if text.is_empty() {
            return SendResult::rejected();
        }
        match (mode, insert_context) {
            (ox_types::Mode::Insert, Some(ox_types::InsertContext::Compose))
            | (ox_types::Mode::Normal, None)
                if active_thread.is_none() =>
            {
                self.do_remote_compose(text).await
            }
            _ => SendResult::rejected(),
        }
    }

    async fn send_local_input_with_text(
        &mut self,
        text: String,
        mode: ox_types::Mode,
        insert_context: Option<ox_types::InsertContext>,
        active_thread: Option<&str>,
    ) -> SendResult {
        use ox_types::{InsertContext, Mode};
        if text.is_empty() {
            return SendResult::rejected();
        }
        match (mode, insert_context) {
            (Mode::Insert, Some(InsertContext::Compose)) | (Mode::Normal, None)
                if active_thread.is_none() =>
            {
                self.do_compose(text).await
            }
            (Mode::Insert, Some(InsertContext::Reply)) | (Mode::Normal, _)
                if active_thread.is_some() =>
            {
                self.do_reply(text, active_thread.unwrap()).await;
                SendResult::accepted(None)
            }
            _ => SendResult::rejected(),
        }
    }

    async fn do_compose(&mut self, input: String) -> SendResult {
        self.history_offset = 0;
        self.input_draft.clear();

        let title: String = input.chars().take(40).collect();
        match self.pool.create_thread(&title) {
            Ok(tid) => {
                self.update_thread_state(&tid, ox_types::ThreadState::Running)
                    .await;
                self.pool.send_prompt(&tid, input).ok();
                SendResult::accepted(Some(tid))
            }
            Err(e) => {
                eprintln!("failed to create thread: {e}");
                SendResult::rejected()
            }
        }
    }

    async fn do_remote_compose(&mut self, input: String) -> SendResult {
        let launcher = match crate::remote_cli::prepare_tui_launcher(&self.inbox_root).await {
            Ok(launcher) => launcher,
            Err(error) => {
                self.set_status(format!("remote: {}", clean_status(&error.to_string())))
                    .await;
                return SendResult::rejected();
            }
        };

        self.history_offset = 0;
        self.input_draft.clear();
        self.set_status("remote: launching a fresh exe.dev conversation…".into())
            .await;

        let title: String = input.chars().take(40).collect();
        let client = self.broker_client.clone();
        tokio::spawn(async move {
            let text = match launcher.start_conversation(title, input).await {
                Ok(conversation) => format!(
                    "remote: {} started on {} — attach with `ox remote conversation attach {}`",
                    conversation.conversation_id,
                    conversation.node_id,
                    conversation.conversation_id,
                ),
                Err(error) => format!("remote failed: {}", clean_status(&error.to_string())),
            };
            let _ = client
                .write_typed(
                    &structfs_core_store::path!("ui"),
                    &ox_types::UiCommand::Global(ox_types::GlobalCommand::SetStatus { text }),
                )
                .await;
        });

        SendResult::accepted(None)
    }

    async fn set_status(&self, text: String) {
        let _ = self
            .broker_client
            .write_typed(
                &structfs_core_store::path!("ui"),
                &ox_types::UiCommand::Global(ox_types::GlobalCommand::SetStatus { text }),
            )
            .await;
    }

    async fn do_reply(&mut self, input: String, thread_id: &str) {
        self.history_offset = 0;
        self.input_draft.clear();

        self.update_thread_state(thread_id, ox_types::ThreadState::Running)
            .await;
        self.pool.send_prompt(thread_id, input).ok();
    }

    /// Navigate input history up (older). Reads from ox.db on demand.
    pub async fn history_up(&mut self, current_input: &str) -> Option<(String, usize)> {
        if self.history_offset == 0 {
            self.input_draft = current_input.to_string();
        }
        let target_offset = self.history_offset + 1;
        if let Some(text) = self.read_history_at(target_offset).await {
            self.history_offset = target_offset;
            let cursor = text.len();
            Some((text, cursor))
        } else {
            None // no more history
        }
    }

    /// Navigate input history down (newer). Returns to draft at offset 0.
    pub async fn history_down(&mut self) -> Option<(String, usize)> {
        if self.history_offset == 0 {
            return None;
        }
        self.history_offset -= 1;
        let text = if self.history_offset == 0 {
            self.input_draft.clone()
        } else {
            self.read_history_at(self.history_offset)
                .await
                .unwrap_or_default()
        };
        let cursor = text.len();
        Some((text, cursor))
    }

    /// Read the Nth most recent input from ox.db via broker (1-indexed).
    async fn read_history_at(&self, offset: usize) -> Option<String> {
        use structfs_core_store::Value;
        let path =
            structfs_core_store::Path::parse(&format!("inbox/inputs/recent/{offset}")).ok()?;
        let record = self.broker_client.read(&path).await.ok()??;
        let arr = match record.as_value() {
            Some(Value::Array(a)) => a,
            _ => return None,
        };
        arr.last().and_then(|v| match v {
            Value::Map(m) => match m.get("text") {
                Some(Value::String(s)) => Some(s.clone()),
                _ => None,
            },
            _ => None,
        })
    }

    /// Update a thread's state via broker.
    pub async fn update_thread_state(&self, thread_id: &str, state: ox_types::ThreadState) {
        let tid = match ox_kernel::PathComponent::try_new(thread_id) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "invalid thread id for path");
                return;
            }
        };
        let update_path = structfs_core_store::path!("inbox", "threads", tid);
        let update = ox_types::UpdateThread {
            id: None,
            thread_state: Some(state),
            inbox_state: None,
            updated_at: None,
        };
        let val = structfs_serde_store::to_value(&update).unwrap();
        self.broker_client
            .write(&update_path, structfs_core_store::Record::parsed(val))
            .await
            .ok();
    }
}

fn input_context_accepts_local(
    mode: ox_types::Mode,
    insert_context: Option<ox_types::InsertContext>,
    active_thread: Option<&str>,
) -> bool {
    use ox_types::{InsertContext, Mode};
    matches!(
        (mode, insert_context, active_thread.is_some()),
        (Mode::Insert, Some(InsertContext::Compose), false)
            | (Mode::Normal, None, false)
            | (Mode::Insert, Some(InsertContext::Reply), true)
            | (Mode::Normal, _, true)
    )
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SendResult {
    pub(crate) accepted: bool,
    pub(crate) thread_id: Option<String>,
}

impl SendResult {
    fn accepted(thread_id: Option<String>) -> Self {
        Self {
            accepted: true,
            thread_id,
        }
    }

    fn rejected() -> Self {
        Self {
            accepted: false,
            thread_id: None,
        }
    }
}

fn clean_status(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_control() {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect()
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
