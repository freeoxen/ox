use ox_gate::{GateStore, ProviderConfig};
use ox_kernel::{AgentEvent, CompletionRequest, Record, StreamEvent, Value, path};
use ox_runtime::{AgentModule, AgentRuntime, AgentRuntimeConfig, HostEffects, HostStore};
use ox_tools::completion::CompletionTransport;
use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::thread;
use structfs_core_store::{Reader as _, Writer as _};

use crate::ingress::*;
use crate::policy::PolicyStats;

// ---------------------------------------------------------------------------
// CliCompletionTransport — reqwest-based CompletionTransport for the CLI
// ---------------------------------------------------------------------------

/// Native HTTP transport that wraps [`ox_gate::transport::streaming_fetch`].
///
/// Holds the reqwest client, provider config, and API key. Also holds a broker
/// handle so streaming text deltas and token usage can be written to the TUI
/// in real time.
struct CliCompletionTransport {
    client: reqwest::blocking::Client,
    config: ProviderConfig,
    api_key: String,
    /// Account name routed through this transport — surfaced in error
    /// messages so users see which account a failing call belonged to.
    account: String,
    /// Provider the account points at (e.g. "anthropic", "lm-studio").
    provider: String,
    scoped_client: ox_broker::ClientHandle,
    rt_handle: tokio::runtime::Handle,
}

impl CompletionTransport for CliCompletionTransport {
    fn send(
        &self,
        request: &CompletionRequest,
        on_event: &dyn Fn(&StreamEvent),
    ) -> Result<ox_tools::completion::CompletionOutput, String> {
        let scoped = self.scoped_client.clone();
        let handle = self.rt_handle.clone();
        let ctx = ox_gate::transport::CallContext::new(&self.provider).with_account(&self.account);
        let (events, usage) = ox_gate::transport::streaming_fetch(
            &self.client,
            &self.config,
            &self.api_key,
            request,
            &ctx,
            &|event| {
                on_event(event);
                if let StreamEvent::TextDelta { text } = event {
                    handle
                        .block_on(scoped.write_typed(&path!("history/turn/streaming"), text))
                        .ok();
                }
            },
        )?;
        if usage.input_tokens > 0 || usage.output_tokens > 0 {
            self.rt_handle
                .block_on(self.scoped_client.write_typed(
                    &path!("history/turn/tokens"),
                    &ox_types::TokenUsage {
                        input_tokens: usage.input_tokens,
                        output_tokens: usage.output_tokens,
                        cache_creation_input_tokens: usage.cache_creation_input_tokens,
                        cache_read_input_tokens: usage.cache_read_input_tokens,
                    },
                ))
                .ok();
        }
        Ok(ox_tools::completion::CompletionOutput {
            events,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_creation_input_tokens: usage.cache_creation_input_tokens,
            cache_read_input_tokens: usage.cache_read_input_tokens,
        })
    }
}

pub const SYSTEM_PROMPT: &str = "\
You are an expert software engineer working in a coding CLI. \
You have tools for reading files, writing files, editing files, \
and running shell commands. \
Always read a file before modifying it. Be concise.\n\n\
IMPORTANT: When you have completed the user's request, respond with your final answer as plain text. \
Do NOT continue making tool calls after you have the information needed to answer. \
If a tool call fails or returns unexpected results, explain the problem to the user \
rather than retrying the same call. Never repeat the same tool call more than once.";

/// Embedded agent Wasm module (built by build.rs from ox-wasm).
pub(crate) const AGENT_WASM: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/agent.wasm"));

/// Per-thread prompt sender.
struct ThreadHandle {
    prompt_tx: mpsc::Sender<WorkerCommand>,
    execution: ThreadExecutionConfig,
    cancellation: ox_tools::sandbox::ToolCancellation,
    pending_cancel: Arc<std::sync::Mutex<std::collections::VecDeque<IngressCancel>>>,
}

enum WorkerCommand {
    Prompt(WorkerPrompt),
    CancelWake,
}

#[derive(Clone, Debug)]
struct WorkerPrompt {
    content: String,
    ingress: Option<IngressPrompt>,
}

#[derive(Clone, Debug)]
struct IngressPrompt {
    kind: ox_inbox::worker_ingress::IntentKind,
    operation: &'static str,
    semantic_id: String,
    request_hash: String,
    accepted_seq: i64,
}

#[derive(Clone, Debug)]
struct IngressCancel {
    cancel_id: String,
    request_hash: String,
    reason: Option<String>,
    accepted_seq: i64,
}

/// Tool-policy profile selected for one conversation worker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PolicyProfile {
    /// Load and enforce the workspace's Clash policy.
    Enforced,
    /// Require kernel enforcement; any unsupported/setup path rejects tools.
    RemoteEnforced,
    /// Preserve the CLI's `--no-policy` behavior.
    Permissive,
}

/// Node-wide hardening controls layered onto the shared local executor.
#[derive(Clone, Debug)]
pub struct ExecutorConfig {
    pub runtime: AgentRuntimeConfig,
    pub local_tool_execution: ox_tools::sandbox::SandboxedExecOptions,
    pub remote_tool_execution: ox_tools::sandbox::SandboxedExecOptions,
    pub max_active_turns: usize,
    pub remote_native_tool_allowlist: BTreeSet<String>,
    pub ingress_failpoints: IngressFailpoints,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum IngressBoundary {
    AfterCreateActionBeforeMark,
    AfterMessageMarkerBeforeUser,
    AfterMessageUserBeforeTurn,
    AfterMessageTurnBeforeMark,
    AfterDecisionResponseBeforeMark,
    AfterCancelAbortBeforeMark,
}

#[derive(Clone, Debug, Default)]
pub struct IngressFailpoints {
    armed: Arc<std::sync::Mutex<BTreeSet<IngressBoundary>>>,
}

impl IngressFailpoints {
    pub fn arm(&self, boundary: IngressBoundary) {
        self.armed
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(boundary);
    }

    pub(crate) fn take(&self, boundary: IngressBoundary) -> bool {
        self.armed
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&boundary)
    }
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self {
            runtime: AgentRuntimeConfig::default(),
            local_tool_execution: ox_tools::sandbox::SandboxedExecOptions::local_compatible(),
            remote_tool_execution: ox_tools::sandbox::SandboxedExecOptions::remote(),
            max_active_turns: usize::MAX,
            remote_native_tool_allowlist: BTreeSet::new(),
            ingress_failpoints: IngressFailpoints::default(),
        }
    }
}

impl ExecutorConfig {
    pub fn remote(max_active_turns: usize) -> Result<Self, String> {
        if max_active_turns == 0 {
            return Err("max_active_turns must be non-zero".to_string());
        }
        Ok(Self {
            runtime: AgentRuntimeConfig::remote(),
            max_active_turns,
            ..Self::default()
        })
    }
}

struct TurnLimiter {
    max: usize,
    active: std::sync::Mutex<usize>,
    available: std::sync::Condvar,
}

impl TurnLimiter {
    fn new(max: usize) -> Result<Arc<Self>, String> {
        if max == 0 {
            return Err("max_active_turns must be non-zero".to_string());
        }
        Ok(Arc::new(Self {
            max,
            active: std::sync::Mutex::new(0),
            available: std::sync::Condvar::new(),
        }))
    }

    fn acquire(self: &Arc<Self>) -> TurnPermit {
        let mut active = self
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        while *active >= self.max {
            active = self
                .available
                .wait(active)
                .unwrap_or_else(|error| error.into_inner());
        }
        *active += 1;
        TurnPermit {
            limiter: self.clone(),
        }
    }
}

struct TurnPermit {
    limiter: Arc<TurnLimiter>,
}

impl Drop for TurnPermit {
    fn drop(&mut self) {
        let mut active = self
            .limiter
            .active
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        *active -= 1;
        self.limiter.available.notify_one();
    }
}

/// Execution settings that belong to one conversation, not to the process.
///
/// The interactive CLI supplies the same workspace for every thread. A
/// headless host can instead give each thread a conversation-owned workspace
/// without creating another executor or changing the worker loop.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ThreadExecutionConfig {
    pub workspace: PathBuf,
    pub policy_profile: PolicyProfile,
}

impl ThreadExecutionConfig {
    pub fn new(workspace: PathBuf, policy_profile: PolicyProfile) -> Self {
        Self {
            workspace,
            policy_profile,
        }
    }
}

/// Manages agent threads — spawns workers and routes prompts.
pub struct AgentPool {
    module: AgentModule,
    threads: HashMap<String, ThreadHandle>,
    default_thread_config: ThreadExecutionConfig,
    inbox: ox_inbox::InboxStore,
    inbox_root: PathBuf,
    broker: ox_broker::BrokerStore,
    rt_handle: tokio::runtime::Handle,
    /// Test-only: when `Some`, workers install this transport into their
    /// `CompletionModule` instead of the reqwest-backed `CliCompletionTransport`.
    transport_factory: Option<crate::test_support::TransportFactory>,
    /// Test-only: when `Some`, workers install these native tools into
    /// their `ToolStore` before the prompt loop starts. See
    /// `test_support::ToolInjector`.
    tool_injector: Option<crate::test_support::ToolInjector>,
    executor_config: ExecutorConfig,
    turn_limiter: Arc<TurnLimiter>,
    dispatching_decisions: Arc<std::sync::Mutex<BTreeSet<String>>>,
}

impl AgentPool {
    /// Create a pool. `transport_factory` is usually `None`; the crash
    /// harness (`tests/crash_harness/`) passes `Some(...)` to script LLM
    /// responses without hitting the network.
    #[allow(dead_code)]
    pub fn new_with_transport_factory(
        workspace: PathBuf,
        no_policy: bool,
        inbox: ox_inbox::InboxStore,
        inbox_root: PathBuf,
        broker: ox_broker::BrokerStore,
        rt_handle: tokio::runtime::Handle,
        transport_factory: Option<crate::test_support::TransportFactory>,
    ) -> Result<Self, String> {
        Self::new_with_test_hooks(
            workspace,
            no_policy,
            inbox,
            inbox_root,
            broker,
            rt_handle,
            transport_factory,
            None,
        )
    }

    /// Test-only constructor that also accepts a `ToolInjector` so the
    /// crash harness can register counter-backed tools for the
    /// post-crash-reconfirm suite (Task 3d Step 6). Not used by the
    /// production binary.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_test_hooks(
        workspace: PathBuf,
        no_policy: bool,
        inbox: ox_inbox::InboxStore,
        inbox_root: PathBuf,
        broker: ox_broker::BrokerStore,
        rt_handle: tokio::runtime::Handle,
        transport_factory: Option<crate::test_support::TransportFactory>,
        tool_injector: Option<crate::test_support::ToolInjector>,
    ) -> Result<Self, String> {
        Self::new_with_config_and_test_hooks(
            workspace,
            no_policy,
            inbox,
            inbox_root,
            broker,
            rt_handle,
            ExecutorConfig::default(),
            transport_factory,
            tool_injector,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_config_and_test_hooks(
        workspace: PathBuf,
        no_policy: bool,
        inbox: ox_inbox::InboxStore,
        inbox_root: PathBuf,
        broker: ox_broker::BrokerStore,
        rt_handle: tokio::runtime::Handle,
        executor_config: ExecutorConfig,
        transport_factory: Option<crate::test_support::TransportFactory>,
        tool_injector: Option<crate::test_support::ToolInjector>,
    ) -> Result<Self, String> {
        let runtime = AgentRuntime::with_config(executor_config.runtime.clone())?;
        let module = runtime.load_module_from_bytes(AGENT_WASM)?;
        let turn_limiter = TurnLimiter::new(executor_config.max_active_turns)?;
        Ok(Self {
            module,
            threads: HashMap::new(),
            default_thread_config: ThreadExecutionConfig::new(
                workspace,
                if no_policy {
                    PolicyProfile::Permissive
                } else {
                    PolicyProfile::Enforced
                },
            ),
            inbox,
            inbox_root,
            broker,
            rt_handle,
            transport_factory,
            tool_injector,
            executor_config,
            turn_limiter,
            dispatching_decisions: Arc::new(std::sync::Mutex::new(BTreeSet::new())),
        })
    }

    /// Create a new thread in the inbox and spawn its agent worker.
    /// Returns the thread_id.
    pub fn create_thread(&mut self, title: &str) -> Result<String, String> {
        self.create_thread_with_config(title, self.default_thread_config.clone())
    }

    /// Create a thread whose tools execute in the supplied workspace.
    pub fn create_thread_with_config(
        &mut self,
        title: &str,
        execution: ThreadExecutionConfig,
    ) -> Result<String, String> {
        use structfs_core_store::{Writer, path};

        let create = ox_types::CreateThread {
            title: title.to_string(),
            parent_id: None,
        };
        let val = structfs_serde_store::to_value(&create).map_err(|e| e.to_string())?;
        let path = self
            .inbox
            .write(&path!("threads"), Record::parsed(val))
            .map_err(|e| e.to_string())?;
        let thread_id = path
            .iter()
            .nth(1)
            .ok_or_else(|| "inbox did not return thread_id".to_string())?
            .clone();

        self.spawn_worker(thread_id.clone(), title.to_string(), execution);
        Ok(thread_id)
    }

    /// Ensure a worker is spawned for this thread without sending any
    /// prompt. The spawned worker will inspect the mount-time
    /// `shell/resume_needed` signal and drive one `run_turn` invocation
    /// if the flag is set, then block on `prompt_rx` for further input.
    ///
    /// Called from the production thread-open hook (`event_loop` open
    /// handler) and from tests. A user opening a thread that was
    /// blocked on an approval at exit must see the modal reappear
    /// without typing — that requires the worker to be alive to read
    /// the resume flag the mount lifecycle wrote, which requires
    /// spawning here even though no prompt has been sent.
    pub fn ensure_worker(&mut self, thread_id: &str) -> Result<(), String> {
        self.ensure_worker_with_config(thread_id, self.default_thread_config.clone())
    }

    /// Ensure a worker is running with conversation-specific execution config.
    pub fn ensure_worker_with_config(
        &mut self,
        thread_id: &str,
        execution: ThreadExecutionConfig,
    ) -> Result<(), String> {
        if execution.policy_profile == PolicyProfile::RemoteEnforced {
            std::fs::create_dir_all(&execution.workspace).map_err(|error| {
                format!(
                    "failed to materialize remote workspace '{}': {error}",
                    execution.workspace.display()
                )
            })?;
        }
        if let Some(handle) = self.threads.get(thread_id) {
            return validate_execution_config(thread_id, &handle.execution, &execution);
        }
        let title = self
            .read_thread_title(thread_id)
            .unwrap_or_else(|| "Thread".to_string());
        self.spawn_worker(thread_id.to_string(), title, execution);
        Ok(())
    }

    /// Send a prompt to a thread. Spawns a worker if one doesn't exist
    /// (e.g., for threads from a previous session).
    pub fn send_prompt(&mut self, thread_id: &str, prompt: String) -> Result<(), String> {
        self.send_prompt_with_config(thread_id, prompt, self.default_thread_config.clone())
    }

    /// Send a prompt, using `execution` if this call has to spawn the worker.
    pub fn send_prompt_with_config(
        &mut self,
        thread_id: &str,
        prompt: String,
        execution: ThreadExecutionConfig,
    ) -> Result<(), String> {
        // Auto-spawn worker for threads from previous sessions. Once spawned,
        // workspace and policy are pinned for the worker's lifetime.
        if let Some(handle) = self.threads.get(thread_id) {
            validate_execution_config(thread_id, &handle.execution, &execution)?;
        } else {
            let title = self
                .read_thread_title(thread_id)
                .unwrap_or_else(|| "Thread".to_string());
            self.spawn_worker(thread_id.to_string(), title, execution);
        }
        let handle = self
            .threads
            .get(thread_id)
            .ok_or_else(|| format!("no thread {thread_id}"))?;
        handle
            .prompt_tx
            .send(WorkerCommand::Prompt(WorkerPrompt {
                content: prompt,
                ingress: None,
            }))
            .map_err(|_| "thread channel closed".to_string())
    }

    fn enqueue_worker_prompt(
        &mut self,
        thread_id: &str,
        envelope: ox_inbox::worker_ingress::PromptEnvelope,
        request_hash: String,
        accepted_seq: i64,
    ) -> Result<(), String> {
        if !self.threads.contains_key(thread_id) {
            self.ensure_worker_with_config(
                thread_id,
                ThreadExecutionConfig::new(
                    self.inbox_root.join("workspaces").join(thread_id),
                    PolicyProfile::RemoteEnforced,
                ),
            )?;
        }
        self.threads
            .get(thread_id)
            .ok_or_else(|| format!("no thread {thread_id}"))?
            .prompt_tx
            .send(WorkerCommand::Prompt(WorkerPrompt {
                content: envelope.content,
                ingress: Some(IngressPrompt {
                    kind: ox_inbox::worker_ingress::IntentKind::Message,
                    operation: "message",
                    semantic_id: envelope.message_id,
                    request_hash,
                    accepted_seq,
                }),
            }))
            .map_err(|_| "thread channel closed".to_string())
    }

    /// Path to the inbox root directory (for direct file reads).
    #[allow(dead_code)]
    pub fn inbox_root(&self) -> &std::path::Path {
        &self.inbox_root
    }

    fn read_thread_title(&mut self, thread_id: &str) -> Option<String> {
        let tid = ox_kernel::PathComponent::try_new(thread_id).ok()?;
        let path = ox_path::oxpath!("threads", tid);
        let record = self.inbox.read(&path).ok()??;
        let value = record.as_value()?;
        match value {
            Value::Map(map) => match map.get("title") {
                Some(Value::String(s)) => Some(s.clone()),
                _ => None,
            },
            _ => None,
        }
    }

    fn spawn_worker(&mut self, thread_id: String, title: String, execution: ThreadExecutionConfig) {
        let (prompt_tx, prompt_rx) = mpsc::channel::<WorkerCommand>();
        let cancellation = ox_tools::sandbox::ToolCancellation::default();
        let pending_cancel = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
        self.threads.insert(
            thread_id.clone(),
            ThreadHandle {
                prompt_tx,
                execution: execution.clone(),
                cancellation: cancellation.clone(),
                pending_cancel: pending_cancel.clone(),
            },
        );

        let module = self.module.clone();
        let workspace = execution.workspace;
        let policy_profile = execution.policy_profile;
        let inbox_root = self.inbox_root.clone();
        let broker = self.broker.clone();
        let rt_handle = self.rt_handle.clone();
        let transport_factory = self.transport_factory.clone();
        let tool_injector = self.tool_injector.clone();
        let executor_config = self.executor_config.clone();
        let turn_limiter = self.turn_limiter.clone();

        thread::spawn(move || {
            // Attach a `thread_id`-scoped span so every tracing event
            // emitted from the worker (including kernel-side events like
            // `ApprovalReRequested`, `PostCrashReconfirmDecision`, and
            // `TurnAbortedUserCanceled` — plan Task 3 Step 7) inherits
            // the thread_id as an attribute. The kernel is
            // surface-neutral and does not know its thread id; the span
            // is where the correlation lives.
            let span = tracing::info_span!("agent_worker", thread_id = %thread_id);
            let _enter = span.enter();
            tracing::info!(title = %title, "agent worker spawned");
            agent_worker(
                thread_id,
                title,
                module,
                workspace,
                policy_profile,
                inbox_root,
                prompt_rx,
                broker,
                rt_handle,
                transport_factory,
                tool_injector,
                executor_config,
                cancellation,
                pending_cancel,
                turn_limiter,
            );
        });
    }

    pub fn cancel_thread(&self, thread_id: &str) -> Result<(), String> {
        let handle = self
            .threads
            .get(thread_id)
            .ok_or_else(|| format!("no running thread {thread_id}"))?;
        handle.cancellation.cancel();
        Ok(())
    }

    /// Drain accepted-but-unapplied worker intents through the existing pool.
    /// Calls are idempotent; startup and every public ingress write may invoke
    /// this without allocating a second conversation or executor.
    pub fn dispatch_worker_ingress(&mut self) -> Result<usize, String> {
        use ox_inbox::worker_ingress::{IntentKind, PromptEnvelope};
        let pending = self
            .inbox
            .pending_worker_intents()
            .map_err(|error| error.to_string())?;
        let mut dispatched = 0;
        for intent in pending {
            match intent.kind {
                IntentKind::Create => {
                    let envelope = intent
                        .decode::<ox_inbox::worker_ingress::CreateEnvelope>()
                        .map_err(|error| error.to_string())?;
                    let thread_id = self
                        .inbox
                        .apply_worker_create(&intent.semantic_id)
                        .map_err(|error| error.to_string())?;
                    self.ensure_worker_with_config(
                        &thread_id,
                        ThreadExecutionConfig::new(
                            self.inbox_root.join("workspaces").join(&thread_id),
                            PolicyProfile::RemoteEnforced,
                        ),
                    )?;
                    if self
                        .executor_config
                        .ingress_failpoints
                        .take(IngressBoundary::AfterCreateActionBeforeMark)
                    {
                        return Err("injected crash after create action before applied mark".into());
                    }
                    self.threads
                        .get(&thread_id)
                        .ok_or_else(|| format!("no thread {thread_id}"))?
                        .prompt_tx
                        .send(WorkerCommand::Prompt(WorkerPrompt {
                            content: envelope.prompt,
                            ingress: Some(IngressPrompt {
                                kind: IntentKind::Create,
                                operation: "create",
                                semantic_id: intent.semantic_id,
                                request_hash: intent.request_hash,
                                accepted_seq: intent.accepted_seq,
                            }),
                        }))
                        .map_err(|_| "thread channel closed".to_string())?;
                    dispatched += 1;
                }
                IntentKind::Message => {
                    let envelope = intent
                        .decode::<PromptEnvelope>()
                        .map_err(|error| error.to_string())?;
                    let thread_id = intent
                        .thread_id
                        .as_deref()
                        .ok_or_else(|| "accepted message has no thread id".to_string())?;
                    self.enqueue_worker_prompt(
                        thread_id,
                        envelope,
                        intent.request_hash,
                        intent.accepted_seq,
                    )?;
                    dispatched += 1;
                }
                IntentKind::Decision => {
                    if self.dispatch_worker_decision(&intent)? {
                        dispatched += 1;
                    }
                }
                IntentKind::Cancel => {
                    self.dispatch_worker_cancel(&intent)?;
                    dispatched += 1;
                }
            }
        }
        Ok(dispatched)
    }

    fn dispatch_worker_decision(
        &mut self,
        intent: &ox_inbox::worker_ingress::AcceptedIntent,
    ) -> Result<bool, String> {
        use ox_inbox::worker_ingress::DecisionEnvelope;
        let thread_id = intent
            .thread_id
            .as_deref()
            .ok_or_else(|| "accepted decision has no thread id".to_string())?;
        let envelope = intent
            .decode::<DecisionEnvelope>()
            .map_err(|e| e.to_string())?;
        if !self.threads.contains_key(thread_id) {
            self.ensure_worker_with_config(
                thread_id,
                ThreadExecutionConfig::new(
                    self.inbox_root.join("workspaces").join(thread_id),
                    PolicyProfile::RemoteEnforced,
                ),
            )?;
        }
        let client = self.broker.client();
        let thread_id = thread_id.to_string();
        let semantic_id = intent.semantic_id.clone();
        let request_hash = intent.request_hash.clone();
        let ingress_failpoints = self.executor_config.ingress_failpoints.clone();
        {
            let mut dispatching = self
                .dispatching_decisions
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            if !dispatching.insert(semantic_id.clone()) {
                return Ok(false);
            }
        }
        let dispatching_decisions = self.dispatching_decisions.clone();
        let dispatch_key = semantic_id.clone();
        self.rt_handle.spawn(async move {
            dispatch_worker_decision_task(
                client,
                thread_id,
                semantic_id,
                request_hash,
                envelope,
                ingress_failpoints,
            )
            .await;
            dispatching_decisions
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .remove(&dispatch_key);
        });
        Ok(true)
    }

    fn dispatch_worker_cancel(
        &mut self,
        intent: &ox_inbox::worker_ingress::AcceptedIntent,
    ) -> Result<(), String> {
        use ox_inbox::worker_ingress::{CancelEnvelope, IntentKind};
        let thread_id = intent
            .thread_id
            .as_deref()
            .ok_or_else(|| "accepted cancel has no thread id".to_string())?;
        let envelope = intent
            .decode::<CancelEnvelope>()
            .map_err(|e| e.to_string())?;
        if !self.threads.contains_key(thread_id) {
            self.ensure_worker_with_config(
                thread_id,
                ThreadExecutionConfig::new(
                    self.inbox_root.join("workspaces").join(thread_id),
                    PolicyProfile::RemoteEnforced,
                ),
            )?;
        }
        let scoped = self.broker.client().scoped(&format!("threads/{thread_id}"));
        let mut adapter = ox_broker::SyncClientAdapter::new(scoped.clone(), self.rt_handle.clone());
        let entries = adapter
            .read_typed::<Vec<ox_kernel::log::LogEntry>>(&path!("log/entries"))
            .map_err(|error| error.to_string())?
            .unwrap_or_default();
        if ingress_cancel_evidence(&entries, &intent.semantic_id, &intent.request_hash)?
            == IngressControlEvidence::Applied
        {
            self.set_thread_interrupted(thread_id)?;
            self.inbox
                .mark_worker_intent_applied(
                    IntentKind::Cancel,
                    &intent.semantic_id,
                    &format!(
                        "conversations/{thread_id}/control/cancel/{}",
                        intent.semantic_id
                    ),
                )
                .map_err(|error| error.to_string())?;
            return Ok(());
        }
        let handle = self
            .threads
            .get(thread_id)
            .ok_or_else(|| format!("no running thread {thread_id}"))?;
        let mut cancels = handle
            .pending_cancel
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !cancels
            .iter()
            .any(|cancel| cancel.cancel_id == envelope.cancel_id)
        {
            cancels.push_back(IngressCancel {
                cancel_id: envelope.cancel_id,
                request_hash: intent.request_hash.clone(),
                reason: envelope.reason,
                accepted_seq: intent.accepted_seq,
            });
        }
        drop(cancels);
        handle.cancellation.cancel();
        // A worker parked in ApprovalStore is blocked on its oneshot rather
        // than polling the runtime cancellation token. Resolve that exact
        // pending request so Wasm can return and the worker can append the
        // terminal UserCanceled evidence. If a human decision won the race,
        // the pending read is null and we do not send a second response.
        let pending = self
            .rt_handle
            .block_on(scoped.read(&path!("approval/pending")))
            .map_err(|error| error.to_string())?;
        if pending.as_ref().and_then(Record::as_value) != Some(&Value::Null) && pending.is_some() {
            self.rt_handle
                .block_on(scoped.write_typed(
                    &path!("approval/response"),
                    &ox_types::ApprovalResponse {
                        decision: ox_types::Decision::CancelTurn,
                    },
                ))
                .map_err(|error| error.to_string())?;
        }
        handle
            .prompt_tx
            .send(WorkerCommand::CancelWake)
            .map_err(|_| "thread channel closed".to_string())?;
        Ok(())
    }

    fn set_thread_interrupted(&self, thread_id: &str) -> Result<(), String> {
        let mut state = std::collections::BTreeMap::new();
        state.insert(
            "thread_state".to_string(),
            Value::String("interrupted".to_string()),
        );
        self.rt_handle
            .block_on(
                self.broker.client().write(
                    &structfs_core_store::Path::parse(&format!("inbox/threads/{thread_id}"))
                        .map_err(|error| error.to_string())?,
                    Record::parsed(Value::Map(state)),
                ),
            )
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}

fn validate_execution_config(
    thread_id: &str,
    running: &ThreadExecutionConfig,
    requested: &ThreadExecutionConfig,
) -> Result<(), String> {
    if running == requested {
        Ok(())
    } else {
        Err(format!(
            "thread {thread_id} is already running with execution config {running:?}; \
             refusing conflicting config {requested:?}"
        ))
    }
}

/// The reusable execution core. It deliberately contains the existing
/// [`AgentPool`] rather than layering a second supervisor over it.
pub struct ExecutionCore {
    pool: AgentPool,
}

impl ExecutionCore {
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_config(
        workspace: PathBuf,
        no_policy: bool,
        inbox: ox_inbox::InboxStore,
        inbox_root: PathBuf,
        broker: ox_broker::BrokerStore,
        rt_handle: tokio::runtime::Handle,
        executor_config: ExecutorConfig,
    ) -> Result<Self, String> {
        Self::new_with_config_and_test_hooks(
            workspace,
            no_policy,
            inbox,
            inbox_root,
            broker,
            rt_handle,
            executor_config,
            None,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_test_hooks(
        workspace: PathBuf,
        no_policy: bool,
        inbox: ox_inbox::InboxStore,
        inbox_root: PathBuf,
        broker: ox_broker::BrokerStore,
        rt_handle: tokio::runtime::Handle,
        transport_factory: Option<crate::test_support::TransportFactory>,
        tool_injector: Option<crate::test_support::ToolInjector>,
    ) -> Result<Self, String> {
        Self::new_with_config_and_test_hooks(
            workspace,
            no_policy,
            inbox,
            inbox_root,
            broker,
            rt_handle,
            ExecutorConfig::default(),
            transport_factory,
            tool_injector,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_config_and_test_hooks(
        workspace: PathBuf,
        no_policy: bool,
        inbox: ox_inbox::InboxStore,
        inbox_root: PathBuf,
        broker: ox_broker::BrokerStore,
        rt_handle: tokio::runtime::Handle,
        executor_config: ExecutorConfig,
        transport_factory: Option<crate::test_support::TransportFactory>,
        tool_injector: Option<crate::test_support::ToolInjector>,
    ) -> Result<Self, String> {
        Ok(Self {
            pool: AgentPool::new_with_config_and_test_hooks(
                workspace,
                no_policy,
                inbox,
                inbox_root,
                broker,
                rt_handle,
                executor_config,
                transport_factory,
                tool_injector,
            )?,
        })
    }

    pub fn new_with_transport_factory(
        workspace: PathBuf,
        no_policy: bool,
        inbox: ox_inbox::InboxStore,
        inbox_root: PathBuf,
        broker: ox_broker::BrokerStore,
        rt_handle: tokio::runtime::Handle,
        transport_factory: Option<crate::test_support::TransportFactory>,
    ) -> Result<Self, String> {
        Self::new_with_test_hooks(
            workspace,
            no_policy,
            inbox,
            inbox_root,
            broker,
            rt_handle,
            transport_factory,
            None,
        )
    }

    pub fn create_thread(&mut self, title: &str) -> Result<String, String> {
        self.pool.create_thread(title)
    }

    pub fn create_thread_with_config(
        &mut self,
        title: &str,
        execution: ThreadExecutionConfig,
    ) -> Result<String, String> {
        self.pool.create_thread_with_config(title, execution)
    }

    pub fn ensure_worker(&mut self, thread_id: &str) -> Result<(), String> {
        self.pool.ensure_worker(thread_id)
    }

    pub fn ensure_worker_with_config(
        &mut self,
        thread_id: &str,
        execution: ThreadExecutionConfig,
    ) -> Result<(), String> {
        self.pool.ensure_worker_with_config(thread_id, execution)
    }

    pub fn send_prompt(&mut self, thread_id: &str, prompt: String) -> Result<(), String> {
        self.pool.send_prompt(thread_id, prompt)
    }

    pub fn send_prompt_with_config(
        &mut self,
        thread_id: &str,
        prompt: String,
        execution: ThreadExecutionConfig,
    ) -> Result<(), String> {
        self.pool
            .send_prompt_with_config(thread_id, prompt, execution)
    }

    pub fn inbox_root(&self) -> &std::path::Path {
        self.pool.inbox_root()
    }

    pub fn cancel_thread(&self, thread_id: &str) -> Result<(), String> {
        self.pool.cancel_thread(thread_id)
    }

    pub fn dispatch_worker_ingress(&mut self) -> Result<usize, String> {
        self.pool.dispatch_worker_ingress()
    }

    /// Move the core onto one control thread behind a bounded queue.
    pub fn into_handle(self, command_capacity: usize) -> ExecutionHandle {
        assert!(
            command_capacity > 0,
            "execution command capacity must be non-zero"
        );
        let client = self.pool.broker.client();
        let (commands, receiver) = mpsc::sync_channel(command_capacity);
        #[cfg(test)]
        let control_exited = Arc::new(std::sync::atomic::AtomicBool::new(false));
        #[cfg(test)]
        let exit_signal = control_exited.clone();
        let control_thread = thread::Builder::new()
            .name("ox-executor-control".to_string())
            .spawn(move || {
                execution_control_loop(self, receiver);
                #[cfg(test)]
                exit_signal.store(true, std::sync::atomic::Ordering::Release);
            })
            .expect("failed to spawn ox executor control thread");
        ExecutionHandle {
            commands: Some(commands),
            client,
            control_thread: Some(control_thread),
            #[cfg(test)]
            control_exited,
        }
    }
}

enum ExecutionCommand {
    Create {
        title: String,
        execution: ThreadExecutionConfig,
        reply: mpsc::Sender<Result<String, String>>,
    },
    Open {
        thread_id: String,
        execution: ThreadExecutionConfig,
        reply: mpsc::Sender<Result<(), String>>,
    },
    Prompt {
        thread_id: String,
        prompt: String,
        execution: ThreadExecutionConfig,
        reply: mpsc::Sender<Result<(), String>>,
    },
    Cancel {
        thread_id: String,
        reply: mpsc::Sender<Result<(), String>>,
    },
    DispatchWorkerIngress {
        reply: mpsc::Sender<Result<usize, String>>,
    },
    Shutdown,
}

fn execution_control_loop(mut core: ExecutionCore, receiver: mpsc::Receiver<ExecutionCommand>) {
    if let Err(error) = core.dispatch_worker_ingress() {
        tracing::error!(%error, "failed to recover accepted worker ingress at startup");
    }
    while let Ok(command) = receiver.recv() {
        match command {
            ExecutionCommand::Create {
                title,
                execution,
                reply,
            } => {
                let _ = reply.send(core.create_thread_with_config(&title, execution));
            }
            ExecutionCommand::Open {
                thread_id,
                execution,
                reply,
            } => {
                let _ = reply.send(core.ensure_worker_with_config(&thread_id, execution));
            }
            ExecutionCommand::Prompt {
                thread_id,
                prompt,
                execution,
                reply,
            } => {
                let _ = reply.send(core.send_prompt_with_config(&thread_id, prompt, execution));
            }
            ExecutionCommand::Cancel { thread_id, reply } => {
                let _ = reply.send(core.cancel_thread(&thread_id));
            }
            ExecutionCommand::DispatchWorkerIngress { reply } => {
                let _ = reply.send(core.dispatch_worker_ingress());
            }
            ExecutionCommand::Shutdown => break,
        }
    }
}

/// Failure to enqueue or complete an executor control operation.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ExecutionCommandError {
    QueueFull,
    Stopped,
    Executor(String),
}

impl std::fmt::Display for ExecutionCommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::QueueFull => f.write_str("execution command queue is full"),
            Self::Stopped => f.write_str("execution core has stopped"),
            Self::Executor(error) => f.write_str(error),
        }
    }
}

impl std::error::Error for ExecutionCommandError {}

/// Bounded headless control surface for one [`ExecutionCore`].
pub struct ExecutionHandle {
    commands: Option<mpsc::SyncSender<ExecutionCommand>>,
    client: ox_broker::ClientHandle,
    control_thread: Option<thread::JoinHandle<()>>,
    #[cfg(test)]
    control_exited: Arc<std::sync::atomic::AtomicBool>,
}

impl ExecutionHandle {
    /// Broker access is the inspect/approval surface; it addresses the same
    /// stores used by the interactive CLI and does not mirror their state.
    pub fn client(&self) -> ox_broker::ClientHandle {
        self.client.clone()
    }

    pub fn create_thread(
        &self,
        title: impl Into<String>,
        execution: ThreadExecutionConfig,
    ) -> Result<String, ExecutionCommandError> {
        let (reply, response) = mpsc::channel();
        self.enqueue(ExecutionCommand::Create {
            title: title.into(),
            execution,
            reply,
        })?;
        response
            .recv()
            .map_err(|_| ExecutionCommandError::Stopped)?
            .map_err(ExecutionCommandError::Executor)
    }

    pub fn open_thread(
        &self,
        thread_id: impl Into<String>,
        execution: ThreadExecutionConfig,
    ) -> Result<(), ExecutionCommandError> {
        let (reply, response) = mpsc::channel();
        self.enqueue(ExecutionCommand::Open {
            thread_id: thread_id.into(),
            execution,
            reply,
        })?;
        response
            .recv()
            .map_err(|_| ExecutionCommandError::Stopped)?
            .map_err(ExecutionCommandError::Executor)
    }

    pub fn send_prompt(
        &self,
        thread_id: impl Into<String>,
        prompt: impl Into<String>,
        execution: ThreadExecutionConfig,
    ) -> Result<(), ExecutionCommandError> {
        let (reply, response) = mpsc::channel();
        self.enqueue(ExecutionCommand::Prompt {
            thread_id: thread_id.into(),
            prompt: prompt.into(),
            execution,
            reply,
        })?;
        response
            .recv()
            .map_err(|_| ExecutionCommandError::Stopped)?
            .map_err(ExecutionCommandError::Executor)
    }

    /// Interrupt an active Wasm turn and any sandboxed tool process sharing
    /// its cancellation token. Control admission is independent of turn permits.
    pub fn cancel_thread(&self, thread_id: impl Into<String>) -> Result<(), ExecutionCommandError> {
        let (reply, response) = mpsc::channel();
        self.enqueue(ExecutionCommand::Cancel {
            thread_id: thread_id.into(),
            reply,
        })?;
        response
            .recv()
            .map_err(|_| ExecutionCommandError::Stopped)?
            .map_err(ExecutionCommandError::Executor)
    }

    /// Dispatch all durable accepted ingress rows. Safe to call after every
    /// acceptance and once during worker startup.
    pub fn dispatch_worker_ingress(&self) -> Result<usize, ExecutionCommandError> {
        let (reply, response) = mpsc::channel();
        self.enqueue(ExecutionCommand::DispatchWorkerIngress { reply })?;
        response
            .recv()
            .map_err(|_| ExecutionCommandError::Stopped)?
            .map_err(ExecutionCommandError::Executor)
    }

    fn enqueue(&self, command: ExecutionCommand) -> Result<(), ExecutionCommandError> {
        self.commands
            .as_ref()
            .ok_or(ExecutionCommandError::Stopped)?
            .try_send(command)
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => ExecutionCommandError::QueueFull,
                mpsc::TrySendError::Disconnected(_) => ExecutionCommandError::Stopped,
            })
    }

    pub fn shutdown(mut self) -> Result<(), ExecutionCommandError> {
        let send_result = self
            .commands
            .take()
            .ok_or(ExecutionCommandError::Stopped)?
            .send(ExecutionCommand::Shutdown)
            .map_err(|_| ExecutionCommandError::Stopped);
        if let Some(control_thread) = self.control_thread.take() {
            control_thread
                .join()
                .map_err(|_| ExecutionCommandError::Stopped)?;
        }
        send_result
    }
}

impl Drop for ExecutionHandle {
    fn drop(&mut self) {
        if let Some(commands) = self.commands.take() {
            // If the bounded queue is full, dropping its final sender still
            // makes the control loop exit after draining queued work.
            let _ = commands.try_send(ExecutionCommand::Shutdown);
            drop(commands);
        }
        if let Some(control_thread) = self.control_thread.take() {
            let _ = control_thread.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Agent worker — one per thread, runs on its own OS thread
// ---------------------------------------------------------------------------

/// Single channel a worker uses to surface startup-time failures to
/// the user (synthesized assistant turn in the thread's history) and
/// to the operator (`tracing::error!`). Constructed at the very top
/// of `agent_worker` so every failure path after it has a consistent
/// place to write to — no per-error-site `tmp_adapter` construction.
struct WorkerErrorChannel {
    thread_id: String,
    adapter: ox_broker::SyncClientAdapter,
}

impl WorkerErrorChannel {
    fn new(
        thread_id: String,
        broker: &ox_broker::BrokerStore,
        rt_handle: &tokio::runtime::Handle,
    ) -> Self {
        let scoped = broker.client().scoped(&format!("threads/{thread_id}"));
        let adapter = ox_broker::SyncClientAdapter::new(scoped, rt_handle.clone());
        Self { thread_id, adapter }
    }

    /// Report a startup-time failure: log with tracing AND write a
    /// synthesized assistant turn to the thread's history so the user
    /// sees what happened in the TUI conversation view. `cause` names
    /// the failing subsystem (e.g. "policy.json") for the tracing
    /// event; `display_error` is the user-facing message.
    fn report_startup_failure(&mut self, cause: &str, display_error: &str, remediation: &str) {
        tracing::error!(
            thread_id = %self.thread_id,
            cause = %cause,
            error = %display_error,
            "agent worker refusing to start",
        );
        let msg = serde_json::json!({
            "role": "assistant",
            "content": [{
                "type": "text",
                "text": format!(
                    "⚠ Agent failed to start: {display_error}\n\n{remediation}",
                ),
            }],
        });
        if let Err(write_err) = self.adapter.write_typed(&path!("history/append"), &msg) {
            tracing::error!(
                thread_id = %self.thread_id,
                cause = %cause,
                error = %display_error,
                history_write_error = %write_err,
                "startup failure AND history append failed; user will see no in-TUI indication",
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn agent_worker(
    thread_id: String,
    title: String,
    module: AgentModule,
    workspace: PathBuf,
    policy_profile: PolicyProfile,
    inbox_root: PathBuf,
    prompt_rx: mpsc::Receiver<WorkerCommand>,
    broker: ox_broker::BrokerStore,
    rt_handle: tokio::runtime::Handle,
    transport_factory: Option<crate::test_support::TransportFactory>,
    tool_injector: Option<crate::test_support::ToolInjector>,
    executor_config: ExecutorConfig,
    cancellation: ox_tools::sandbox::ToolCancellation,
    pending_cancel: Arc<std::sync::Mutex<std::collections::VecDeque<IngressCancel>>>,
    turn_limiter: Arc<TurnLimiter>,
) {
    let no_policy = policy_profile == PolicyProfile::Permissive;
    let remote = policy_profile == PolicyProfile::RemoteEnforced;
    // Build the error channel FIRST, before anything that can fail.
    // Every startup-failure path below uses `err_channel.report_*`
    // so there's one consistent place to write to — no per-site
    // adapter construction. The "real" adapter (used for the main
    // worker loop) is built downstream.
    let mut err_channel = WorkerErrorChannel::new(thread_id.clone(), &broker, &rt_handle);

    // Build ToolStore — primary tool execution backend
    let executor = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|dir| dir.join("ox-tool-exec")))
        .unwrap_or_else(|| PathBuf::from("ox-tool-exec"));
    let sandbox_policy: Arc<dyn ox_tools::sandbox::SandboxPolicy> = if no_policy {
        Arc::new(ox_tools::sandbox::PermissivePolicy)
    } else if remote {
        Arc::new(crate::clash_sandbox::ClashSandboxPolicy::required(
            workspace.clone(),
        ))
    } else {
        Arc::new(crate::clash_sandbox::ClashSandboxPolicy::new(
            workspace.clone(),
        ))
    };
    let mut tool_options = if remote {
        executor_config.remote_tool_execution.clone()
    } else {
        executor_config.local_tool_execution.clone()
    };
    tool_options.cancellation = cancellation.clone();
    let fs_module =
        ox_tools::fs::FsModule::new(workspace.clone(), executor.clone(), sandbox_policy.clone())
            .with_exec_options(tool_options.clone());
    let os_module = ox_tools::os::OsModule::new(workspace.clone(), executor, sandbox_policy)
        .with_exec_options(tool_options);
    let gate = GateStore::new();
    let completion_module = ox_tools::completion::CompletionModule::new(gate);
    let mut tool_store = ox_tools::ToolStore::new(fs_module, os_module, completion_module);

    // Register get_tool_output — redirect tool for retrieving abbreviated results
    tool_store.register_redirect(ox_tools::RedirectTool {
        wire_name: "get_tool_output".into(),
        internal_path: "redirect/get_tool_output".into(),
        description: "Retrieve the full or partial output of a previous tool call. \
                      Use this when a tool result was abbreviated in the conversation."
            .into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "tool_use_id": {
                    "type": "string",
                    "description": "The tool_use_id from the abbreviated result"
                },
                "offset": {
                    "type": "integer",
                    "description": "0-based line offset to start from (default: 0)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of lines to return (default: all)"
                }
            },
            "required": ["tool_use_id"]
        }),
        build_path: Box::new(|input| {
            let id = input
                .get("tool_use_id")
                .and_then(|v| v.as_str())
                .ok_or("missing tool_use_id")?;
            let offset = input.get("offset").and_then(|v| v.as_u64());
            let limit = input.get("limit").and_then(|v| v.as_u64());
            match (offset, limit) {
                (Some(o), Some(l)) => Ok(format!("log/results/{id}/lines/{o}/{l}")),
                (Some(o), None) => Ok(format!("log/results/{id}/lines/{o}/999999")),
                (None, Some(l)) => Ok(format!("log/results/{id}/lines/0/{l}")),
                (None, None) => Ok(format!("log/results/{id}")),
            }
        }),
    });

    // Test-only: inject native tools supplied by the crash harness
    // BEFORE the tool schemas get written to the adapter below.
    // Production workers pass `None` here.
    if let Some(injector) = &tool_injector {
        for tool in injector() {
            let wire_name = tool.schema().wire_name;
            if remote
                && !executor_config
                    .remote_native_tool_allowlist
                    .contains(&wire_name)
            {
                err_channel.report_startup_failure(
                    "native tool allowlist",
                    &format!("native tool '{wire_name}' is not trusted for remote execution"),
                    "Audit the in-process adapter and add its wire name to the worker allowlist.",
                );
                return;
            }
            tool_store.register_native(tool);
        }
    }

    let policy = if no_policy {
        crate::policy::PolicyGuard::permissive()
    } else {
        match crate::policy::PolicyGuard::load(&workspace) {
            Ok(p) => p,
            Err(e) => {
                // A user with a `.clash/policy.json` made an explicit
                // assertion about tool-execution policy. Falling back
                // to permissive defaults would silently give them more
                // access than they asked for. Refuse to start; the
                // error channel constructed at the top of the worker
                // writes to thread history so the user sees what
                // happened in the TUI conversation view.
                err_channel.report_startup_failure(
                    "policy.json",
                    &format!("{e}"),
                    "Fix the policy file or remove it to use defaults, then retry.",
                );
                return;
            }
        }
    };

    // Create scoped client + SyncClientAdapter
    // The first write through the adapter triggers ThreadRegistry's lazy-mount,
    // which restores history/system/model from disk if a snapshot exists.
    let scoped_client = broker.client().scoped(&format!("threads/{thread_id}"));
    let mut adapter = ox_broker::SyncClientAdapter::new(scoped_client.clone(), rt_handle.clone());

    // Unscoped broker client for inbox writes and global config reads
    let broker_client = broker.client();

    // Write tool schemas via adapter (triggers ThreadRegistry lazy-mount from disk)
    adapter
        .write_typed(
            &path!("tools/schemas"),
            &tool_store.tool_schemas_for_model(),
        )
        .ok();

    // Read provider and API key from thread's GateStore (resolves through config handle).
    // The default account flows out of the primary CompletionRole at
    // `gate/completions/primary` — pre-O2 this was a separate
    // `gate/defaults/account` read.
    let default_account = adapter
        .read_typed::<ox_types::CompletionRole>(&path!("gate/completions/primary"))
        .ok()
        .flatten()
        .map(|r| r.account)
        .unwrap_or_else(|| "anthropic".to_string());
    let (provider, api_key_for_transport) = match ox_kernel::PathComponent::try_new(
        default_account.as_str(),
    ) {
        Ok(acct_comp) => {
            let prov = adapter
                .read_typed::<String>(&ox_path::oxpath!(
                    "gate",
                    "accounts",
                    acct_comp.clone(),
                    "provider"
                ))
                .ok()
                .flatten()
                .unwrap_or_else(|| "anthropic".to_string());
            let key = adapter
                .read_typed::<String>(&ox_path::oxpath!("gate", "accounts", acct_comp, "key"))
                .ok()
                .flatten()
                .unwrap_or_default();
            (prov, key)
        }
        Err(e) => {
            tracing::warn!(error = %e, account = %default_account, "invalid account name for path");
            ("anthropic".to_string(), String::new())
        }
    };

    // Resolve dialect/endpoint/version from gate/providers/{provider}. GateStore
    // is the source of truth — built-in providers (anthropic, openai) are seeded
    // there at construction; user-defined providers (e.g. lm-studio) live there
    // too. Falls back to ProviderConfig::anthropic() only when the lookup fails.
    let provider_config = match ox_kernel::PathComponent::try_new(provider.as_str()) {
        Ok(prov_comp) => adapter
            .read_typed::<ProviderConfig>(&ox_path::oxpath!("gate", "providers", prov_comp))
            .ok()
            .flatten()
            .unwrap_or_else(|| {
                tracing::warn!(
                    provider = %provider,
                    "no gate/providers entry; falling back to anthropic defaults"
                );
                ProviderConfig::anthropic()
            }),
        Err(e) => {
            tracing::warn!(error = %e, provider = %provider, "invalid provider name for path");
            ProviderConfig::anthropic()
        }
    };

    // Inject the CLI completion transport into the CompletionModule.
    // This gives CompletionModule the ability to execute LLM completions
    // end-to-end via StructFS write/read, independent of HostEffects.
    //
    // When a test-only `transport_factory` is supplied, it overrides the
    // built-in reqwest transport. The provider/api_key reads above are still
    // performed so the worker's tracing logs continue to describe its intent.
    let transport: Box<dyn CompletionTransport> = match &transport_factory {
        Some(factory) => factory(),
        None => Box::new(CliCompletionTransport {
            client: reqwest::blocking::Client::new(),
            config: provider_config.clone(),
            api_key: api_key_for_transport.clone(),
            account: default_account.clone(),
            provider: provider.clone(),
            scoped_client: scoped_client.clone(),
            rt_handle: rt_handle.clone(),
        }),
    };
    tool_store.completions_mut().set_transport(transport);

    // Wrap ToolStore in PolicyStore with CliPolicyCheck for permission enforcement.
    let policy_check = crate::policy_check::CliPolicyCheck::new(
        policy,
        scoped_client.clone(),
        broker_client.clone(),
        thread_id.clone(),
        rt_handle.clone(),
    );
    let mut gated_store = ox_tools::policy_store::PolicyStore::new(tool_store, policy_check);

    tracing::info!(
        thread_id = %thread_id,
        default_account = %default_account,
        provider = %provider,
        endpoint = %provider_config.endpoint,
        has_key = !api_key_for_transport.is_empty(),
        "agent worker ready"
    );

    // ---- Resume-signal check ---------------------------------------------
    //
    // The `tools/schemas` write above is the first adapter hit, which
    // triggers lazy-mount inside `ThreadRegistry::ensure_mounted`. If the
    // mount classifier detected `AwaitingApproval` or `AwaitingToolResult`,
    // the `ShellConfigStore` at `shell/resume_needed` now holds `true`. We
    // drive a single `run_turn` invocation so the kernel's resume prologue
    // (see `ox-kernel::run::inspect_log_for_resume`) can re-request
    // approval with `post_crash_reconfirm: true` before any user input.
    //
    // The flag is cleared *before* `run_turn` is invoked (one-shot
    // semantics). A crash between clear and run is safe: the next mount's
    // classifier re-sets it. A crash *during* run is also safe: the flag
    // is already cleared, and the ledger tail (with the Assistant
    // tool_use and/or the mount's ToolAborted marker) still produces a
    // valid resume shape that the next mount will re-detect and re-set.
    let resume_needed = adapter
        .read_typed::<bool>(&path!("shell/resume_needed"))
        .ok()
        .flatten()
        .unwrap_or(false);
    if resume_needed {
        // Clear first, then run. `ShellConfigStore` accepts `Value::Bool`
        // writes and the signal toggles the worker's behavior — the flag
        // is allowed to race with the run, but not with another mount.
        adapter
            .write_typed(&path!("shell/resume_needed"), &false)
            .ok();
        tracing::info!(
            thread_id = %thread_id,
            "resume signal observed — driving one run_turn for post-crash re-approval"
        );
        let (ret_adapter, ret_gated, _result) = run_one_turn(
            &module,
            adapter,
            gated_store,
            &thread_id,
            &title,
            &inbox_root,
            &scoped_client,
            &broker_client,
            &rt_handle,
            &cancellation,
            &turn_limiter,
        );
        adapter = ret_adapter;
        gated_store = ret_gated;
    }

    let ingress_failpoints = executor_config.ingress_failpoints.clone();

    while let Ok(command) = prompt_rx.recv() {
        let WorkerCommand::Prompt(prompt) = command else {
            if finalize_pending_cancels(
                &mut adapter,
                &broker_client,
                &rt_handle,
                &thread_id,
                &pending_cancel,
                None,
                &ingress_failpoints,
            ) {
                cancellation.reset();
            }
            continue;
        };
        let prompt_seq = prompt.ingress.as_ref().map(|ingress| ingress.accepted_seq);
        if finalize_pending_cancels(
            &mut adapter,
            &broker_client,
            &rt_handle,
            &thread_id,
            &pending_cancel,
            prompt_seq,
            &ingress_failpoints,
        ) {
            cancellation.reset();
        }
        let input = prompt.content;
        tracing::debug!(thread_id = %thread_id, input_len = input.len(), "prompt received");

        if let Some(ingress) = &prompt.ingress {
            match ingress_prompt_state(
                &mut adapter,
                ingress.operation,
                &ingress.semantic_id,
                &ingress.request_hash,
            ) {
                Ok(IngressPromptState::Terminal) => {
                    mark_ingress_prompt_applied(
                        &broker_client,
                        &rt_handle,
                        &thread_id,
                        ingress.kind,
                        &ingress.semantic_id,
                    );
                    continue;
                }
                Ok(IngressPromptState::InFlight) => {
                    // Mount-time recovery runs before this mailbox. If an
                    // in-flight segment remains, never start a second turn;
                    // the accepted intent stays available for reconciliation.
                    tracing::info!(
                        semantic_id = %ingress.semantic_id,
                        "worker ingress turn remains in flight after recovery"
                    );
                    continue;
                }
                Ok(IngressPromptState::MarkerOnly) => {}
                Ok(IngressPromptState::UserWithoutTurn) => {
                    // Exact crash boundary: durable User, no TurnStart. Drive
                    // the existing turn without appending the User again.
                }
                Ok(IngressPromptState::Missing) => {
                    if let Err(error) = append_ingress_marker(
                        &mut adapter,
                        ingress.operation,
                        &ingress.semantic_id,
                        &ingress.request_hash,
                    ) {
                        tracing::error!(semantic_id = %ingress.semantic_id, %error, "ingress marker append failed");
                        continue;
                    }
                    if ingress_failpoints.take(IngressBoundary::AfterMessageMarkerBeforeUser) {
                        return;
                    }
                }
                Ok(IngressPromptState::Conflict) => {
                    tracing::error!(
                        semantic_id = %ingress.semantic_id,
                        "conflict: durable ingress marker has a different request hash"
                    );
                    continue;
                }
                Err(error) => {
                    tracing::error!(semantic_id = %ingress.semantic_id, %error, "ingress evidence read failed");
                    continue;
                }
            }

            if matches!(
                ingress_prompt_state(
                    &mut adapter,
                    ingress.operation,
                    &ingress.semantic_id,
                    &ingress.request_hash,
                ),
                Ok(IngressPromptState::Missing | IngressPromptState::MarkerOnly)
            ) {
                let user_json = serde_json::json!({"role": "user", "content": &input});
                if let Err(e) = adapter.write_typed(&path!("history/append"), &user_json) {
                    tracing::error!(thread_id = %thread_id, error = %e, "history append failed");
                    continue;
                }
                if ingress_failpoints.take(IngressBoundary::AfterMessageUserBeforeTurn) {
                    return;
                }
            }
        } else {
            // Local CLI prompt path stays unchanged.
            let user_json = serde_json::json!({"role": "user", "content": &input});
            if let Err(e) = adapter.write_typed(&path!("history/append"), &user_json) {
                tracing::error!(thread_id = %thread_id, error = %e, "history append failed");
                continue;
            }
        }

        // A cancel accepted after this prompt but before its turn starts owns
        // the terminal outcome: preserve the durable User, finalize cancel,
        // and do not start Wasm. A later prompt (higher accepted_seq) remains
        // valid new work after that cancellation.
        if has_pending_cancel_after(&pending_cancel, prompt_seq)
            && finalize_pending_cancels(
                &mut adapter,
                &broker_client,
                &rt_handle,
                &thread_id,
                &pending_cancel,
                None,
                &ingress_failpoints,
            )
        {
            cancellation.reset();
            if let Some(ingress) = &prompt.ingress {
                mark_ingress_prompt_applied(
                    &broker_client,
                    &rt_handle,
                    &thread_id,
                    ingress.kind,
                    &ingress.semantic_id,
                );
            }
            continue;
        }

        // Record input in the search database for Ctrl+R history
        {
            let input_record = structfs_serde_store::json_to_value(serde_json::json!({
                "text": &input,
                "thread_id": &thread_id,
                "context": "reply",
            }));
            rt_handle
                .block_on(broker_client.write(
                    &ox_path::oxpath!("inbox", "inputs"),
                    Record::parsed(input_record),
                ))
                .ok();
        }

        let (ret_adapter, ret_gated, _result) = run_one_turn(
            &module,
            adapter,
            gated_store,
            &thread_id,
            &title,
            &inbox_root,
            &scoped_client,
            &broker_client,
            &rt_handle,
            &cancellation,
            &turn_limiter,
        );
        adapter = ret_adapter;
        gated_store = ret_gated;

        // Cancellation is finalized only after Wasm/tool execution has
        // returned and all ordinary run bookkeeping has completed. The abort
        // marker and Interrupted state are therefore terminal.
        if finalize_pending_cancels(
            &mut adapter,
            &broker_client,
            &rt_handle,
            &thread_id,
            &pending_cancel,
            None,
            &ingress_failpoints,
        ) {
            cancellation.reset();
        }

        if let Some(ingress) = &prompt.ingress {
            if ingress_failpoints.take(IngressBoundary::AfterMessageTurnBeforeMark) {
                return;
            }
            if matches!(
                ingress_prompt_state(
                    &mut adapter,
                    ingress.operation,
                    &ingress.semantic_id,
                    &ingress.request_hash,
                ),
                Ok(IngressPromptState::Terminal)
            ) {
                mark_ingress_prompt_applied(
                    &broker_client,
                    &rt_handle,
                    &thread_id,
                    ingress.kind,
                    &ingress.semantic_id,
                );
            }
        }
    }

    // Worker exit — ThreadRegistry retains thread state in memory until process exit.
    // No explicit unmount needed.
}

fn has_pending_cancel_after(
    pending_cancel: &Arc<std::sync::Mutex<std::collections::VecDeque<IngressCancel>>>,
    prompt_seq: Option<i64>,
) -> bool {
    let Some(prompt_seq) = prompt_seq else {
        return false;
    };
    pending_cancel
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .iter()
        .any(|cancel| cancel.accepted_seq > prompt_seq)
}

fn finalize_pending_cancels(
    adapter: &mut ox_broker::SyncClientAdapter,
    broker_client: &ox_broker::ClientHandle,
    rt_handle: &tokio::runtime::Handle,
    thread_id: &str,
    pending_cancel: &Arc<std::sync::Mutex<std::collections::VecDeque<IngressCancel>>>,
    before_seq: Option<i64>,
    failpoints: &IngressFailpoints,
) -> bool {
    let cancels = {
        let mut queue = pending_cancel
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut selected = Vec::new();
        let mut retained = std::collections::VecDeque::new();
        while let Some(cancel) = queue.pop_front() {
            if before_seq.is_none_or(|limit| cancel.accepted_seq < limit) {
                selected.push(cancel);
            } else {
                retained.push_back(cancel);
            }
        }
        *queue = retained;
        selected.sort_by_key(|cancel| cancel.accepted_seq);
        selected
    };
    if cancels.is_empty() {
        return false;
    }
    for cancel in cancels {
        finalize_one_cancel(
            adapter,
            broker_client,
            rt_handle,
            thread_id,
            cancel,
            failpoints,
        );
    }
    true
}

fn finalize_one_cancel(
    adapter: &mut ox_broker::SyncClientAdapter,
    broker_client: &ox_broker::ClientHandle,
    rt_handle: &tokio::runtime::Handle,
    thread_id: &str,
    cancel: IngressCancel,
    failpoints: &IngressFailpoints,
) {
    let entries = match adapter.read_typed::<Vec<ox_kernel::log::LogEntry>>(&path!("log/entries")) {
        Ok(Some(entries)) => entries,
        Ok(None) => Vec::new(),
        Err(error) => {
            tracing::error!(cancel_id = %cancel.cancel_id, %error, "cancel evidence read failed");
            return;
        }
    };
    let evidence = match ingress_cancel_evidence(&entries, &cancel.cancel_id, &cancel.request_hash)
    {
        Ok(evidence) => evidence,
        Err(error) => {
            tracing::error!(cancel_id = %cancel.cancel_id, %error, "cancel ingress conflict");
            return;
        }
    };
    if evidence == IngressControlEvidence::Missing
        && let Err(error) =
            append_ingress_marker(adapter, "cancel", &cancel.cancel_id, &cancel.request_hash)
    {
        tracing::error!(cancel_id = %cancel.cancel_id, %error, "cancel marker append failed");
        return;
    }
    if evidence != IngressControlEvidence::Applied {
        let outcome = serde_json::json!({
            "type": "meta",
            "data": {
                "kind": "worker_cancel_outcome",
                "cancel_id": cancel.cancel_id,
                "outcome": "interrupted",
                "reason": cancel.reason,
            }
        });
        if let Err(error) = adapter.write_typed(&path!("log/append"), &outcome) {
            tracing::error!(cancel_id = %cancel.cancel_id, %error, "cancel outcome append failed");
            return;
        }
        let aborted = serde_json::json!({
            "type": "turn_aborted",
            "reason": "user_canceled",
        });
        if let Err(error) = adapter.write_typed(&path!("log/append"), &aborted) {
            tracing::error!(cancel_id = %cancel.cancel_id, %error, "cancel abort append failed");
            return;
        }
    }
    if failpoints.take(IngressBoundary::AfterCancelAbortBeforeMark) {
        return;
    }
    let mut state = std::collections::BTreeMap::new();
    state.insert(
        "thread_state".to_string(),
        Value::String("interrupted".to_string()),
    );
    if let Ok(path) = structfs_core_store::Path::parse(&format!("inbox/threads/{thread_id}")) {
        if let Err(error) =
            rt_handle.block_on(broker_client.write(&path, Record::parsed(Value::Map(state))))
        {
            tracing::error!(cancel_id = %cancel.cancel_id, %error, "cancel state update failed");
            return;
        }
    }
    rt_handle.block_on(mark_ingress_control_applied(
        broker_client,
        "cancels",
        thread_id,
        &cancel.cancel_id,
        "control/cancel",
    ));
}

/// Drive one `module.run(host_store)` invocation plus its per-run
/// bookkeeping (token accounting, config snapshot, inbox state).
///
/// Takes the `adapter` and `gated_store` by ownership so they can be
/// moved into `CliEffects` + `HostStore`, and returns them back so the
/// caller can keep using them for subsequent turns. The third tuple
/// element is the run result (propagated so callers can log on failure;
/// most flows ignore it because errors are already surfaced to history
/// by this helper).
///
/// Factored out of `agent_worker` so the post-crash resume turn
/// (driven from a one-shot `shell/resume_needed` flag before the
/// prompt loop) goes through the same bookkeeping as a user-initiated
/// turn — including the `save_config_snapshot` write that captures the
/// post-resume `context.json`.
#[allow(clippy::too_many_arguments)]
fn run_one_turn(
    module: &AgentModule,
    mut adapter: ox_broker::SyncClientAdapter,
    gated_store: ox_tools::policy_store::PolicyStore<
        ox_tools::ToolStore,
        crate::policy_check::CliPolicyCheck,
    >,
    thread_id: &str,
    title: &str,
    inbox_root: &std::path::Path,
    scoped_client: &ox_broker::ClientHandle,
    broker_client: &ox_broker::ClientHandle,
    rt_handle: &tokio::runtime::Handle,
    cancellation: &ox_tools::sandbox::ToolCancellation,
    turn_limiter: &Arc<TurnLimiter>,
) -> (
    ox_broker::SyncClientAdapter,
    ox_tools::policy_store::PolicyStore<ox_tools::ToolStore, crate::policy_check::CliPolicyCheck>,
    Result<(), String>,
) {
    // Snapshot session tokens before the run for per-run delta and streaming cost.
    let pre_run_session: ox_types::TokenUsage = adapter
        .read_typed(&path!("history/turn/session_tokens"))
        .ok()
        .flatten()
        .unwrap_or_default();
    adapter
        .write_typed(&path!("history/turn/run_start"), &pre_run_session)
        .ok();

    cancellation.reset();
    let effects = CliEffects {
        thread_id: thread_id.to_string(),
        gated_store,
        scoped_client: scoped_client.clone(),
        rt_handle: rt_handle.clone(),
        stats: PolicyStats::default(),
    };

    let host_store = HostStore::new(adapter, effects);
    tracing::debug!(thread_id = %thread_id, "running wasm module");
    let _turn_permit = turn_limiter.acquire();
    let (returned_store, result) = module.run_with_cancellation(host_store, cancellation.clone());
    drop(_turn_permit);

    let mut adapter = returned_store.backend;
    let gated_store = returned_store.effects.gated_store;

    match &result {
        Ok(()) => tracing::debug!(thread_id = %thread_id, "agent run complete"),
        Err(e) => tracing::error!(thread_id = %thread_id, error = %e, "agent run failed"),
    }

    if let Err(e) = &result {
        // Write error to history before commit. The error itself already
        // surfaced via AgentEvent / TUI; this write durably records it so
        // remount/replay shows what happened. If recording fails, the
        // user's conversation log will be missing the error message on
        // replay — tracing::error gives the operator the diagnostic
        // (matches the user-message write at line ~564).
        let msg = serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": format!("error: {e}")}]});
        if let Err(write_err) = adapter.write_typed(&path!("history/append"), &msg) {
            tracing::error!(
                thread_id = %thread_id,
                turn_error = %e,
                write_error = %write_err,
                "history append failed while recording turn error; \
                 the error WILL surface to the user via AgentEvent but \
                 WILL NOT be visible on replay",
            );
        }
    }

    // Read model for per-model tracking (may differ from worker-init if
    // changed mid-session). Sourced from the primary CompletionRole's
    // `model_id` — the post-O2 replacement for the retired
    // `gate/defaults/model` path.
    let run_model: String = adapter
        .read_typed::<ox_types::CompletionRole>(&path!("gate/completions/primary"))
        .ok()
        .flatten()
        .map(|r| r.model_id)
        .unwrap_or_default();

    // Compute per-run token usage and write to turn state.
    let post_run_session: ox_types::TokenUsage = adapter
        .read_typed(&path!("history/turn/session_tokens"))
        .ok()
        .flatten()
        .unwrap_or_default();
    let last_run = ox_types::TokenUsage {
        input_tokens: post_run_session
            .input_tokens
            .saturating_sub(pre_run_session.input_tokens),
        output_tokens: post_run_session
            .output_tokens
            .saturating_sub(pre_run_session.output_tokens),
        cache_creation_input_tokens: post_run_session
            .cache_creation_input_tokens
            .saturating_sub(pre_run_session.cache_creation_input_tokens),
        cache_read_input_tokens: post_run_session
            .cache_read_input_tokens
            .saturating_sub(pre_run_session.cache_read_input_tokens),
    };
    adapter
        .write_typed(&path!("history/turn/last_run"), &last_run)
        .ok();

    // Accumulate per-model usage for the dialog breakdown.
    if last_run.input_tokens > 0 || last_run.output_tokens > 0 {
        let per_model_entry = serde_json::json!({
            "model": run_model,
            "usage": last_run,
        });
        let val = structfs_serde_store::json_to_value(per_model_entry);
        adapter
            .write(
                &path!("history/turn/per_model_add"),
                structfs_core_store::Record::parsed(val),
            )
            .ok();
    }

    // Clear all ephemeral turn state (streaming text, thinking, tool status).
    // The kernel already wrote the assistant message to log/append.
    adapter.write_typed(&path!("history/turn/clear"), &()).ok();

    // Persist conversation state for restart recovery. This only writes
    // `context.json`; per-append durability (via LedgerWriter on the
    // SharedLog) handles the ledger, and the CommitDrain task propagates
    // inbox index freshness.
    if let Err(e) = save_config_snapshot(&mut adapter, inbox_root, thread_id, title) {
        tracing::warn!(
            thread_id = %thread_id,
            error = %e,
            "save_config_snapshot (post-turn) failed"
        );
    }

    // Index conversation content for full-text search
    if let Ok(tid_comp) = ox_kernel::PathComponent::try_new(thread_id) {
        rt_handle
            .block_on(broker_client.write(
                &ox_path::oxpath!("inbox", "index", tid_comp),
                Record::parsed(Value::Null),
            ))
            .ok();
    }

    // Write inbox metadata updates through broker
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    if let Ok(tid_comp) = ox_kernel::PathComponent::try_new(thread_id) {
        let new_state = if result.is_ok() {
            ox_types::ThreadState::WaitingForInput
        } else {
            ox_types::ThreadState::Errored
        };
        let update = ox_types::UpdateThread {
            id: None,
            thread_state: Some(new_state),
            inbox_state: None,
            updated_at: Some(now),
        };
        rt_handle
            .block_on(
                broker_client.write_typed(&ox_path::oxpath!("inbox", "threads", tid_comp), &update),
            )
            .ok();
    } else {
        tracing::warn!("invalid thread id for state update path");
    }

    (adapter, gated_store, result)
}

/// Narrow CLI-side wrapper over [`ox_inbox::snapshot::save_config_snapshot`].
///
/// Writes `context.json` for the thread dir; does **not** touch the ledger
/// — per-append durability via [`ox_inbox::ledger_writer::LedgerWriter`] is
/// the single writer of `ledger.jsonl`.
///
/// On failure, logs an error and writes a `LogEntry::Error` into the thread's
/// log so the user sees it in the thread view.
fn save_config_snapshot(
    store: &mut dyn structfs_core_store::Store,
    inbox_root: &std::path::Path,
    thread_id: &str,
    title: &str,
) -> Result<(), String> {
    let thread_dir = inbox_root.join("threads").join(thread_id);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    match ox_inbox::snapshot::save_config_snapshot(
        store,
        &thread_dir,
        thread_id,
        title,
        &[],
        now,
        &ox_inbox::snapshot::PARTICIPATING_MOUNTS,
    ) {
        Ok(()) => Ok(()),
        Err(e) => {
            tracing::error!(
                thread_id,
                path = %thread_dir.display(),
                error = %e,
                "failed to save thread config snapshot — conversation config may be lost on restart"
            );
            // Surface error to the user in the thread view
            let error_msg = serde_json::json!({
                "type": "error",
                "message": format!("Failed to save thread config: {e}. Conversation config may be lost on restart."),
            });
            let val = structfs_serde_store::json_to_value(error_msg);
            let _ = store.write(
                &structfs_core_store::Path::parse("log/append").unwrap(),
                structfs_core_store::Record::parsed(val),
            );
            Err(e)
        }
    }
}

/// Propagate a `SaveResult` to the broker's inbox index so listings
/// show live `message_count` / `last_seq` counts instead of the stale
/// values from last startup reconcile.
///
/// Called from the per-thread [`crate::commit_drain::CommitDrainHandle`]
/// task (Task 1c), which observes the `LedgerWriter`'s latest-wins slot
/// and invokes this helper only when the commit sequence advances. The
/// `broker_setup::tests::write_save_result_to_inbox_updates_live_counts`
/// test exercises the same helper directly to pin the rollup contract.
pub async fn write_save_result_to_inbox(
    broker_client: &ox_broker::ClientHandle,
    thread_id: &str,
    result: &ox_inbox::snapshot::SaveResult,
) {
    let mut update = std::collections::BTreeMap::new();
    update.insert(
        "last_seq".to_string(),
        structfs_core_store::Value::Integer(result.last_seq),
    );
    if let Some(ref hash) = result.last_hash {
        update.insert(
            "last_hash".to_string(),
            structfs_core_store::Value::String(hash.clone()),
        );
    }
    update.insert(
        "message_count".to_string(),
        structfs_core_store::Value::Integer(result.message_count),
    );
    let path_str = format!("inbox/threads/{thread_id}");
    if let Ok(path) = structfs_core_store::Path::parse(&path_str) {
        broker_client
            .write(
                &path,
                structfs_core_store::Record::parsed(structfs_core_store::Value::Map(update)),
            )
            .await
            .ok();
    }
}

// ---------------------------------------------------------------------------
// CliEffects — HostEffects impl for ox-runtime Wasm execution
// ---------------------------------------------------------------------------

/// Host-side effects for a CLI agent worker, owning tools and policy so they
/// can be transferred into/out of the HostStore each turn.
pub(crate) struct CliEffects {
    #[allow(dead_code)]
    pub(crate) thread_id: String,
    pub(crate) gated_store: ox_tools::policy_store::PolicyStore<
        ox_tools::ToolStore,
        crate::policy_check::CliPolicyCheck,
    >,
    scoped_client: ox_broker::ClientHandle,
    rt_handle: tokio::runtime::Handle,
    #[allow(dead_code)]
    pub(crate) stats: PolicyStats,
}

impl CliEffects {
    /// Write a value to the broker through the scoped client (blocking).
    fn broker_write(&self, path: &structfs_core_store::Path, value: Value) {
        self.rt_handle
            .block_on(self.scoped_client.write(path, Record::parsed(value)))
            .ok();
    }
}

impl HostEffects for CliEffects {
    fn tool_store(&mut self) -> &mut dyn structfs_core_store::Store {
        &mut self.gated_store
    }

    fn emit_event(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::TurnStart => {
                self.broker_write(&path!("history/turn/thinking"), Value::Bool(true));
            }
            AgentEvent::TextDelta(text) => {
                self.broker_write(&path!("history/turn/streaming"), Value::String(text));
            }
            AgentEvent::ToolCallStart { name } => {
                self.rt_handle
                    .block_on(self.scoped_client.write_typed(
                        &path!("history/turn/tool"),
                        &ox_types::ToolStatus {
                            name,
                            status: "running".to_string(),
                        },
                    ))
                    .ok();
            }
            AgentEvent::ToolCallResult { .. } => {
                self.broker_write(&path!("history/turn/tool"), Value::Null);
            }
            AgentEvent::TurnEnd => {
                self.broker_write(&path!("history/turn/thinking"), Value::Bool(false));
            }
            AgentEvent::Error(_) => {
                // Don't write to history here — the outer agent_worker loop
                // writes the error after run_turn returns Err. Writing here
                // too would produce duplicate entries in the SharedLog.
                self.broker_write(&path!("history/turn/thinking"), Value::Bool(false));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_wasm_is_valid() {
        // Verify build.rs produced a real wasm module
        assert!(
            AGENT_WASM.len() > 1024,
            "agent.wasm is {} bytes — too small to be a real module",
            AGENT_WASM.len()
        );
        assert_eq!(
            &AGENT_WASM[..4],
            b"\0asm",
            "agent.wasm missing wasm magic header"
        );
        // Version 1
        assert_eq!(
            AGENT_WASM[4..8],
            [1, 0, 0, 0],
            "agent.wasm has unexpected wasm version"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn execution_handle_uses_the_existing_inbox_and_thread_registry() {
        let inbox_root = tempfile::tempdir().expect("inbox tempdir");
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let broker = ox_broker::BrokerStore::default();
        let mounted_inbox = ox_inbox::InboxStore::open(inbox_root.path()).expect("open inbox");
        crate::mount_execution_stores(
            &broker,
            mounted_inbox,
            inbox_root.path().to_path_buf(),
            ox_store_util::LocalConfig::new(),
            ox_store_util::LocalConfig::new(),
        )
        .await;

        let core = ExecutionCore::new_with_test_hooks(
            workspace.path().to_path_buf(),
            true,
            ox_inbox::InboxStore::open(inbox_root.path()).expect("open pool inbox"),
            inbox_root.path().to_path_buf(),
            broker.clone(),
            tokio::runtime::Handle::current(),
            None,
            None,
        )
        .expect("construct execution core");
        let handle = core.into_handle(4);
        let execution =
            ThreadExecutionConfig::new(workspace.path().to_path_buf(), PolicyProfile::Permissive);
        let thread_id = handle
            .create_thread("headless parity", execution.clone())
            .expect("create thread through bounded handle");

        handle
            .open_thread(thread_id.clone(), execution.clone())
            .expect("matching execution config reuses worker");
        handle
            .cancel_thread(thread_id.clone())
            .expect("public control surface reaches the worker cancellation token");

        let other_workspace = tempfile::tempdir().expect("other workspace tempdir");
        let workspace_error = handle
            .open_thread(
                thread_id.clone(),
                ThreadExecutionConfig::new(
                    other_workspace.path().to_path_buf(),
                    PolicyProfile::Permissive,
                ),
            )
            .expect_err("conflicting workspace must be rejected");
        assert!(
            matches!(workspace_error, ExecutionCommandError::Executor(ref message)
                if message.contains("already running") && message.contains("conflicting config")),
            "unexpected workspace conflict: {workspace_error}"
        );

        let policy_error = handle
            .send_prompt(
                thread_id.clone(),
                "must not be delivered",
                ThreadExecutionConfig::new(workspace.path().to_path_buf(), PolicyProfile::Enforced),
            )
            .expect_err("conflicting policy must be rejected");
        assert!(
            matches!(policy_error, ExecutionCommandError::Executor(ref message)
                if message.contains("already running") && message.contains("Enforced")),
            "unexpected policy conflict: {policy_error}"
        );

        let rows = handle
            .client()
            .read(&path!("inbox/threads"))
            .await
            .expect("read inbox")
            .expect("inbox rows");
        let contains_thread = match rows.as_value() {
            Some(Value::Array(rows)) => rows.iter().any(|row| match row {
                Value::Map(map) => map.get("id") == Some(&Value::String(thread_id.clone())),
                _ => false,
            }),
            _ => false,
        };
        assert!(contains_thread, "handle must use the mounted InboxStore");

        let control_exited = handle.control_exited.clone();
        drop(handle);
        assert!(
            control_exited.load(std::sync::atomic::Ordering::Acquire),
            "ExecutionHandle::drop must join its control thread"
        );
    }

    #[test]
    fn turn_permit_is_released_on_drop_and_unwind() {
        let limiter = TurnLimiter::new(1).unwrap();
        let first = limiter.acquire();
        drop(first);
        {
            let _after_success = limiter.acquire();
        }

        let unwind_limiter = limiter.clone();
        let _ = std::panic::catch_unwind(move || {
            let _permit = unwind_limiter.acquire();
            panic!("simulated Wasm trap path");
        });
        let _after_unwind = limiter.acquire();
    }

    fn ingress_marker(operation: &str, id: &str, hash: &str) -> ox_kernel::log::LogEntry {
        ox_kernel::log::LogEntry::Meta {
            data: serde_json::json!({
                "kind": "worker_ingress",
                "operation": operation,
                "semantic_id": id,
                "request_hash": hash,
            }),
        }
    }

    #[test]
    fn message_crash_boundaries_are_classified_without_duplicate_user() {
        use ox_kernel::log::LogEntry;
        let marker = ingress_marker("message", "message-1", "hash-1");
        assert_eq!(
            classify_ingress_prompt(&[], "message", "message-1", "hash-1"),
            IngressPromptState::Missing
        );
        assert_eq!(
            classify_ingress_prompt(
                std::slice::from_ref(&marker),
                "message",
                "message-1",
                "hash-1",
            ),
            IngressPromptState::MarkerOnly
        );
        let user_tail = vec![
            marker.clone(),
            LogEntry::User {
                content: "hello".into(),
                scope: None,
            },
        ];
        assert_eq!(
            classify_ingress_prompt(&user_tail, "message", "message-1", "hash-1"),
            IngressPromptState::UserWithoutTurn,
            "durable User without TurnStart must run without another User append"
        );
        let mut in_flight = user_tail.clone();
        in_flight.push(LogEntry::TurnStart { scope: None });
        assert_eq!(
            classify_ingress_prompt(&in_flight, "message", "message-1", "hash-1"),
            IngressPromptState::InFlight
        );
        in_flight.push(LogEntry::TurnEnd {
            scope: None,
            model: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        });
        assert_eq!(
            classify_ingress_prompt(&in_flight, "message", "message-1", "hash-1"),
            IngressPromptState::Terminal
        );
        assert_eq!(
            classify_ingress_prompt(&in_flight, "message", "message-1", "different"),
            IngressPromptState::Conflict
        );
    }

    #[test]
    fn decision_and_cancel_evidence_require_exact_semantics() {
        use ox_kernel::log::{LogEntry, TurnAbortReason};
        let decision_log = vec![
            ingress_marker("decision", "approval-1", "decision-hash"),
            LogEntry::ApprovalResolved {
                tool_name: "bash".into(),
                decision: ox_types::Decision::DenyOnce,
            },
        ];
        assert_eq!(
            ingress_decision_evidence(
                &decision_log,
                "approval-1",
                "decision-hash",
                ox_types::Decision::DenyOnce,
            )
            .unwrap(),
            IngressControlEvidence::Applied
        );
        assert!(
            ingress_decision_evidence(
                &decision_log,
                "approval-1",
                "decision-hash",
                ox_types::Decision::AllowOnce,
            )
            .unwrap_err()
            .contains("different decision")
        );

        let cancel_log = vec![
            ingress_marker("cancel", "cancel-1", "cancel-hash"),
            LogEntry::TurnAborted {
                reason: TurnAbortReason::UserCanceled,
            },
        ];
        assert_eq!(
            ingress_cancel_evidence(&cancel_log, "cancel-1", "cancel-hash").unwrap(),
            IngressControlEvidence::Applied
        );
        assert!(ingress_cancel_evidence(&cancel_log, "cancel-1", "wrong").is_err());
    }

    #[test]
    fn ingress_failpoints_are_injectable_and_one_shot() {
        let failpoints = IngressFailpoints::default();
        failpoints.arm(IngressBoundary::AfterMessageUserBeforeTurn);
        assert!(failpoints.take(IngressBoundary::AfterMessageUserBeforeTurn));
        assert!(!failpoints.take(IngressBoundary::AfterMessageUserBeforeTurn));
    }
}
