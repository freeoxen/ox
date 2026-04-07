use ox_gate::{GateStore, ProviderConfig};
use ox_kernel::{
    AgentEvent, CompletionRequest, Record, StreamEvent, ToolCall, ToolRegistry, Value, Writer, path,
};
use ox_runtime::{AgentModule, AgentRuntime, HostEffects, HostStore};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::thread;
use structfs_serde_store::json_to_value;

use crate::app::{AppControl, AppEvent, ApprovalResponse};
use crate::policy::PolicyStats;

pub(crate) const SYSTEM_PROMPT: &str = "\
You are an expert software engineer working in a coding CLI. \
You have tools for reading files, writing files, editing files, \
and running shell commands. \
Always read a file before modifying it. Be concise.";

/// Embedded agent Wasm module (built by `scripts/build-agent.sh`).
pub(crate) const AGENT_WASM: &[u8] = include_bytes!("../../../target/agent.wasm");

/// Per-thread prompt sender.
struct ThreadHandle {
    prompt_tx: mpsc::Sender<String>,
}

/// Manages agent threads — spawns workers and routes prompts.
pub struct AgentPool {
    module: AgentModule,
    threads: HashMap<String, ThreadHandle>,
    event_tx: mpsc::Sender<AppEvent>,
    control_tx: mpsc::Sender<AppControl>,
    // Config cloned into each worker
    model: String,
    provider: String,
    max_tokens: u32,
    api_key: String,
    workspace: PathBuf,
    no_policy: bool,
    inbox: ox_inbox::InboxStore,
    inbox_root: PathBuf,
    broker: ox_broker::BrokerStore,
    rt_handle: tokio::runtime::Handle,
}

impl AgentPool {
    /// Create a new pool: initializes the Wasm runtime and loads the agent module.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        model: String,
        provider: String,
        max_tokens: u32,
        api_key: String,
        workspace: PathBuf,
        no_policy: bool,
        inbox: ox_inbox::InboxStore,
        inbox_root: PathBuf,
        event_tx: mpsc::Sender<AppEvent>,
        control_tx: mpsc::Sender<AppControl>,
        broker: ox_broker::BrokerStore,
        rt_handle: tokio::runtime::Handle,
    ) -> Result<Self, String> {
        let runtime = AgentRuntime::new()?;
        let module = runtime.load_module_from_bytes(AGENT_WASM)?;
        Ok(Self {
            module,
            threads: HashMap::new(),
            event_tx,
            control_tx,
            model,
            provider,
            max_tokens,
            api_key,
            workspace,
            no_policy,
            inbox,
            inbox_root,
            broker,
            rt_handle,
        })
    }

    /// Create a new thread in the inbox and spawn its agent worker.
    /// Returns the thread_id.
    pub fn create_thread(&mut self, title: &str) -> Result<String, String> {
        use structfs_core_store::{Writer, path};

        let mut map = std::collections::BTreeMap::new();
        map.insert(
            "title".to_string(),
            structfs_core_store::Value::String(title.to_string()),
        );
        let path = self
            .inbox
            .write(&path!("threads"), Record::parsed(Value::Map(map)))
            .map_err(|e| e.to_string())?;
        let thread_id = path
            .iter()
            .nth(1)
            .ok_or_else(|| "inbox did not return thread_id".to_string())?
            .clone();

        self.spawn_worker(thread_id.clone(), title.to_string());
        Ok(thread_id)
    }

    /// Send a prompt to a thread. Spawns a worker if one doesn't exist
    /// (e.g., for threads from a previous session).
    pub fn send_prompt(&mut self, thread_id: &str, prompt: String) -> Result<(), String> {
        // Auto-spawn worker for threads from previous sessions
        if !self.threads.contains_key(thread_id) {
            let title = self
                .read_thread_title(thread_id)
                .unwrap_or_else(|| "Thread".to_string());
            self.spawn_worker(thread_id.to_string(), title);
        }
        let handle = self
            .threads
            .get(thread_id)
            .ok_or_else(|| format!("no thread {thread_id}"))?;
        handle
            .prompt_tx
            .send(prompt)
            .map_err(|_| "thread channel closed".to_string())
    }

    /// Borrow the inbox store (mutable — StructFS Reader requires &mut self).
    pub fn inbox(&mut self) -> &mut ox_inbox::InboxStore {
        &mut self.inbox
    }

    /// Path to the inbox root directory (for direct file reads).
    pub fn inbox_root(&self) -> &std::path::Path {
        &self.inbox_root
    }

    fn read_thread_title(&mut self, thread_id: &str) -> Option<String> {
        use structfs_core_store::Reader as _;
        let path =
            ox_kernel::Path::from_components(vec!["threads".to_string(), thread_id.to_string()]);
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

    fn spawn_worker(&mut self, thread_id: String, title: String) {
        let (prompt_tx, prompt_rx) = mpsc::channel::<String>();
        self.threads
            .insert(thread_id.clone(), ThreadHandle { prompt_tx });

        let module = self.module.clone();
        let event_tx = self.event_tx.clone();
        let control_tx = self.control_tx.clone();
        let model = self.model.clone();
        let provider = self.provider.clone();
        let max_tokens = self.max_tokens;
        let api_key = self.api_key.clone();
        let workspace = self.workspace.clone();
        let no_policy = self.no_policy;
        let inbox_root = self.inbox_root.clone();
        let broker = self.broker.clone();
        let rt_handle = self.rt_handle.clone();

        thread::spawn(move || {
            agent_worker(
                thread_id, title, module, model, provider, max_tokens, api_key, workspace,
                no_policy, inbox_root, prompt_rx, event_tx, control_tx, broker, rt_handle,
            );
        });
    }
}

// ---------------------------------------------------------------------------
// Agent worker — one per thread, runs on its own OS thread
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn agent_worker(
    thread_id: String,
    title: String,
    module: AgentModule,
    model: String,
    provider: String,
    max_tokens: u32,
    api_key: String,
    workspace: PathBuf,
    no_policy: bool,
    inbox_root: PathBuf,
    prompt_rx: mpsc::Receiver<String>,
    event_tx: mpsc::Sender<AppEvent>,
    control_tx: mpsc::Sender<AppControl>,
    broker: ox_broker::BrokerStore,
    rt_handle: tokio::runtime::Handle,
) {
    // Build tool registry
    let extra_tools = crate::tools::standard_tools(workspace.clone());
    let mut tools = ToolRegistry::new();
    for tool in extra_tools {
        tools.register(tool);
    }

    let policy = if no_policy {
        crate::policy::PolicyGuard::permissive()
    } else {
        crate::policy::PolicyGuard::load(&workspace)
    };

    // Mount thread stores in the broker
    let config = crate::thread_mount::ThreadConfig {
        system_prompt: SYSTEM_PROMPT.to_string(),
        model: model.clone(),
        max_tokens,
        tool_schemas: tools.schemas(),
        provider: provider.clone(),
        api_key: api_key.clone(),
    };
    let _mount_handles = match rt_handle.block_on(crate::thread_mount::mount_thread(
        &broker, &thread_id, config,
    )) {
        Ok(handles) => handles,
        Err(e) => {
            event_tx
                .send(AppEvent::Done {
                    thread_id: thread_id.clone(),
                    result: Err::<String, _>(format!("mount failed: {e}")),
                })
                .ok();
            return;
        }
    };

    // Create scoped client + SyncClientAdapter
    let scoped_client = broker.client().scoped(&format!("threads/{thread_id}"));
    let mut adapter = ox_broker::SyncClientAdapter::new(scoped_client, rt_handle.clone());

    // Construct ProviderConfig directly (no GateStore read needed)
    let provider_config = match provider.as_str() {
        "openai" => ProviderConfig::openai(),
        _ => ProviderConfig::anthropic(),
    };
    let api_key_for_transport = api_key.clone();

    // Register completion tools using a temporary GateStore
    let mut gate_for_tools = GateStore::new();
    gate_for_tools
        .write(
            &ox_kernel::Path::from_components(vec![
                "accounts".to_string(),
                provider.clone(),
                "key".to_string(),
            ]),
            Record::parsed(Value::String(api_key)),
        )
        .ok();
    let send = Arc::new(crate::transport::make_send_fn(
        provider_config.clone(),
        api_key_for_transport.clone(),
    ));
    for tool in gate_for_tools.create_completion_tools(send) {
        tools.register(tool);
    }

    // Restore conversation state through the adapter
    crate::thread_mount::restore_thread_state(&mut adapter, &inbox_root, &thread_id).ok();

    // Legacy format: restore from raw JSONL if no snapshot exists
    let thread_dir = inbox_root.join("threads").join(&thread_id);
    if !thread_dir.join("context.json").exists() {
        let jsonl_path = thread_dir.join(format!("{thread_id}.jsonl"));
        if jsonl_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&jsonl_path) {
                for line in content.lines() {
                    if line.is_empty() {
                        continue;
                    }
                    if let Ok(json) = serde_json::from_str::<serde_json::Value>(line) {
                        adapter
                            .write(
                                &path!("history/append"),
                                Record::parsed(json_to_value(json)),
                            )
                            .ok();
                    }
                }
            }
        }
    }

    // Ownership ping-pong state
    let mut tools = tools;
    let mut policy = policy;
    let mut client = reqwest::blocking::Client::new();

    while let Ok(input) = prompt_rx.recv() {
        // Write user message to history
        let user_json = serde_json::json!({"role": "user", "content": input});
        if let Err(e) = adapter.write(
            &path!("history/append"),
            Record::parsed(json_to_value(user_json)),
        ) {
            event_tx
                .send(AppEvent::Done {
                    thread_id: thread_id.clone(),
                    result: Err::<String, _>(e.to_string()),
                })
                .ok();
            continue;
        }

        let effects = CliEffects {
            thread_id: thread_id.clone(),
            client,
            config: provider_config.clone(),
            api_key: api_key_for_transport.clone(),
            tools,
            policy,
            event_tx: event_tx.clone(),
            control_tx: control_tx.clone(),
            stats: PolicyStats::default(),
        };

        let host_store = HostStore::new(adapter, effects);
        let (returned_store, result) = module.run(host_store);

        adapter = returned_store.backend;
        client = returned_store.effects.client;
        tools = returned_store.effects.tools;
        let stats = returned_store.effects.stats.clone();
        policy = returned_store.effects.policy;

        // Persist conversation state for restart recovery
        save_thread_state(&mut adapter, &inbox_root, &thread_id, &title, &event_tx);

        event_tx
            .send(AppEvent::PolicyStats {
                thread_id: thread_id.clone(),
                stats,
            })
            .ok();

        let done_result = match result {
            Ok(()) => Ok(String::new()),
            Err(e) => Err(e),
        };
        event_tx
            .send(AppEvent::Done {
                thread_id: thread_id.clone(),
                result: done_result,
            })
            .ok();
    }

    // Unmount thread stores on worker exit
    rt_handle.block_on(crate::thread_mount::unmount_thread(&broker, &thread_id));
}

/// Save the conversation state from the store to the thread directory.
/// Sends a `SaveComplete` event so the main thread can write-through to SQLite.
fn save_thread_state(
    store: &mut dyn structfs_core_store::Store,
    inbox_root: &std::path::Path,
    thread_id: &str,
    title: &str,
    event_tx: &mpsc::Sender<AppEvent>,
) {
    let thread_dir = inbox_root.join("threads").join(thread_id);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    let result = ox_inbox::snapshot::save(
        store,
        &thread_dir,
        thread_id,
        title,
        &[],
        now,
        &ox_inbox::snapshot::PARTICIPATING_MOUNTS,
    );

    if let Ok(save_result) = result {
        event_tx
            .send(AppEvent::SaveComplete {
                thread_id: thread_id.to_string(),
                last_seq: save_result.last_seq,
                last_hash: save_result.last_hash,
                updated_at: now,
            })
            .ok();
    }
}

// ---------------------------------------------------------------------------
// CliEffects — HostEffects impl for ox-runtime Wasm execution
// ---------------------------------------------------------------------------

/// Host-side effects for a CLI agent worker, owning tools and policy so they
/// can be transferred into/out of the HostStore each turn.
pub(crate) struct CliEffects {
    pub(crate) thread_id: String,
    pub(crate) client: reqwest::blocking::Client,
    config: ProviderConfig,
    api_key: String,
    pub(crate) tools: ToolRegistry,
    pub(crate) policy: crate::policy::PolicyGuard,
    event_tx: mpsc::Sender<AppEvent>,
    control_tx: mpsc::Sender<AppControl>,
    pub(crate) stats: PolicyStats,
}

impl HostEffects for CliEffects {
    fn complete(
        &mut self,
        request: &CompletionRequest,
    ) -> Result<(Vec<StreamEvent>, u32, u32), String> {
        let tx = self.event_tx.clone();
        let tid = self.thread_id.clone();
        let (events, usage) = crate::transport::streaming_fetch(
            &self.client,
            &self.config,
            &self.api_key,
            request,
            &|event| {
                if let StreamEvent::TextDelta(text) = event {
                    tx.send(AppEvent::Agent {
                        thread_id: tid.clone(),
                        event: AgentEvent::TextDelta(text.clone()),
                    })
                    .ok();
                }
            },
        )?;
        if usage.input_tokens > 0 || usage.output_tokens > 0 {
            self.event_tx
                .send(AppEvent::Usage {
                    thread_id: self.thread_id.clone(),
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
                    .send(AppEvent::PolicyStats {
                        thread_id: self.thread_id.clone(),
                        stats: self.stats.clone(),
                    })
                    .ok();
                self.execute_tool_inner(call)
            }
            crate::policy::CheckResult::Deny(reason) => {
                self.stats.denied += 1;
                self.event_tx
                    .send(AppEvent::PolicyStats {
                        thread_id: self.thread_id.clone(),
                        stats: self.stats.clone(),
                    })
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
                    .send(AppEvent::PolicyStats {
                        thread_id: self.thread_id.clone(),
                        stats: self.stats.clone(),
                    })
                    .ok();
                let (resp_tx, resp_rx) = mpsc::channel();
                self.control_tx
                    .send(AppControl::PermissionRequest {
                        thread_id: self.thread_id.clone(),
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
                        let is_allow = crate::app::node_is_allow(&node);
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
        self.event_tx
            .send(AppEvent::Agent {
                thread_id: self.thread_id.clone(),
                event,
            })
            .ok();
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
