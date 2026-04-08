use ox_gate::{GateStore, ProviderConfig};
use ox_kernel::{
    AgentEvent, CompletionRequest, Record, StreamEvent, ToolCall, ToolRegistry, Value, Writer, path,
};
use ox_runtime::{AgentModule, AgentRuntime, HostEffects, HostStore};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::thread;
use structfs_core_store::Reader as _;
use structfs_serde_store::json_to_value;

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
    workspace: PathBuf,
    no_policy: bool,
    inbox: ox_inbox::InboxStore,
    inbox_root: PathBuf,
    broker: ox_broker::BrokerStore,
    rt_handle: tokio::runtime::Handle,
}

impl AgentPool {
    /// Create a new pool: initializes the Wasm runtime and loads the agent module.
    pub fn new(
        workspace: PathBuf,
        no_policy: bool,
        inbox: ox_inbox::InboxStore,
        inbox_root: PathBuf,
        broker: ox_broker::BrokerStore,
        rt_handle: tokio::runtime::Handle,
    ) -> Result<Self, String> {
        let runtime = AgentRuntime::new()?;
        let module = runtime.load_module_from_bytes(AGENT_WASM)?;
        Ok(Self {
            module,
            threads: HashMap::new(),
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
    #[allow(dead_code)]
    pub fn inbox_root(&self) -> &std::path::Path {
        &self.inbox_root
    }

    fn read_thread_title(&mut self, thread_id: &str) -> Option<String> {
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
        let workspace = self.workspace.clone();
        let no_policy = self.no_policy;
        let inbox_root = self.inbox_root.clone();
        let broker = self.broker.clone();
        let rt_handle = self.rt_handle.clone();

        thread::spawn(move || {
            agent_worker(
                thread_id, title, module, workspace, no_policy, inbox_root, prompt_rx, broker,
                rt_handle,
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
    workspace: PathBuf,
    no_policy: bool,
    inbox_root: PathBuf,
    prompt_rx: mpsc::Receiver<String>,
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

    // Create scoped client + SyncClientAdapter
    // The first write through the adapter triggers ThreadRegistry's lazy-mount,
    // which restores history/system/model from disk if a snapshot exists.
    let scoped_client = broker.client().scoped(&format!("threads/{thread_id}"));
    let mut adapter = ox_broker::SyncClientAdapter::new(scoped_client.clone(), rt_handle.clone());

    // Unscoped broker client for inbox writes and global config reads
    let broker_client = broker.client();

    // Write tool schemas via adapter (triggers ThreadRegistry lazy-mount from disk)
    if let Ok(val) = structfs_serde_store::to_value(&tools.schemas()) {
        adapter
            .write(&path!("tools/schemas"), Record::parsed(val))
            .ok();
    }

    // Read provider and API key from thread's GateStore (resolves through config handle)
    let bootstrap = match adapter.read(&path!("gate/bootstrap")) {
        Ok(Some(r)) => match r.as_value() {
            Some(Value::String(s)) => s.clone(),
            _ => "anthropic".to_string(),
        },
        _ => "anthropic".to_string(),
    };
    let provider = match adapter.read(&structfs_core_store::Path::from_components(vec![
        "gate".into(),
        "accounts".into(),
        bootstrap.clone(),
        "provider".into(),
    ])) {
        Ok(Some(r)) => match r.as_value() {
            Some(Value::String(s)) => s.clone(),
            _ => "anthropic".to_string(),
        },
        _ => "anthropic".to_string(),
    };
    let api_key_for_transport =
        match adapter.read(&structfs_core_store::Path::from_components(vec![
            "gate".into(),
            "accounts".into(),
            bootstrap.clone(),
            "key".into(),
        ])) {
            Ok(Some(r)) => match r.as_value() {
                Some(Value::String(s)) => s.clone(),
                _ => String::new(),
            },
            _ => String::new(),
        };
    let provider_config = match provider.as_str() {
        "openai" => ProviderConfig::openai(),
        _ => ProviderConfig::anthropic(),
    };

    // Register completion tools using a temporary GateStore with the resolved key
    let mut gate_for_tools = GateStore::new();
    gate_for_tools
        .write(
            &ox_kernel::Path::from_components(vec![
                "accounts".to_string(),
                bootstrap.clone(),
                "key".to_string(),
            ]),
            Record::parsed(Value::String(api_key_for_transport.clone())),
        )
        .ok();
    let send = Arc::new(crate::transport::make_send_fn(
        provider_config.clone(),
        api_key_for_transport.clone(),
    ));
    for tool in gate_for_tools.create_completion_tools(send) {
        tools.register(tool);
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
            eprintln!("history append failed for thread {thread_id}: {e}");
            continue;
        }

        let effects = CliEffects {
            thread_id: thread_id.clone(),
            client,
            config: provider_config.clone(),
            api_key: api_key_for_transport.clone(),
            tools,
            policy,
            scoped_client: scoped_client.clone(),
            rt_handle: rt_handle.clone(),
            stats: PolicyStats::default(),
        };

        let host_store = HostStore::new(adapter, effects);
        let (returned_store, result) = module.run(host_store);

        adapter = returned_store.backend;
        client = returned_store.effects.client;
        tools = returned_store.effects.tools;
        policy = returned_store.effects.policy;

        // Persist conversation state for restart recovery
        save_thread_state(&mut adapter, &inbox_root, &thread_id, &title);

        // Write inbox metadata updates through broker
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        {
            let new_state = if result.is_ok() {
                "waiting_for_input"
            } else {
                "errored"
            };
            let mut update = BTreeMap::new();
            update.insert("id".to_string(), Value::String(thread_id.clone()));
            update.insert(
                "thread_state".to_string(),
                Value::String(new_state.to_string()),
            );
            update.insert("updated_at".to_string(), Value::Integer(now));
            rt_handle
                .block_on(broker_client.write(
                    &ox_kernel::Path::from_components(vec![
                        "inbox".to_string(),
                        "threads".to_string(),
                    ]),
                    Record::parsed(Value::Map(update)),
                ))
                .ok();
        }

        // Clear turn state (thinking = false)
        rt_handle
            .block_on(scoped_client.write(
                &path!("history/turn/thinking"),
                Record::parsed(Value::Bool(false)),
            ))
            .ok();

        if let Err(e) = &result {
            // Write error to history
            let msg = serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": format!("error: {e}")}]});
            adapter
                .write(&path!("history/append"), Record::parsed(json_to_value(msg)))
                .ok();
        }

        // Commit the turn
        adapter
            .write(&path!("history/commit"), Record::parsed(Value::Null))
            .ok();
    }

    // Worker exit — ThreadRegistry retains thread state in memory until process exit.
    // No explicit unmount needed.
}

/// Save the conversation state from the store to the thread directory.
fn save_thread_state(
    store: &mut dyn structfs_core_store::Store,
    inbox_root: &std::path::Path,
    thread_id: &str,
    title: &str,
) {
    let thread_dir = inbox_root.join("threads").join(thread_id);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);

    ox_inbox::snapshot::save(
        store,
        &thread_dir,
        thread_id,
        title,
        &[],
        now,
        &ox_inbox::snapshot::PARTICIPATING_MOUNTS,
    )
    .ok();
}

// ---------------------------------------------------------------------------
// CliEffects — HostEffects impl for ox-runtime Wasm execution
// ---------------------------------------------------------------------------

/// Host-side effects for a CLI agent worker, owning tools and policy so they
/// can be transferred into/out of the HostStore each turn.
pub(crate) struct CliEffects {
    #[allow(dead_code)]
    pub(crate) thread_id: String,
    pub(crate) client: reqwest::blocking::Client,
    config: ProviderConfig,
    api_key: String,
    pub(crate) tools: ToolRegistry,
    pub(crate) policy: crate::policy::PolicyGuard,
    scoped_client: ox_broker::ClientHandle,
    rt_handle: tokio::runtime::Handle,
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
    fn complete(
        &mut self,
        request: &CompletionRequest,
    ) -> Result<(Vec<StreamEvent>, u32, u32), String> {
        let scoped = self.scoped_client.clone();
        let handle = self.rt_handle.clone();
        let (events, usage) = crate::transport::streaming_fetch(
            &self.client,
            &self.config,
            &self.api_key,
            request,
            &|event| {
                if let StreamEvent::TextDelta(text) = event {
                    handle
                        .block_on(scoped.write(
                            &path!("history/turn/streaming"),
                            Record::parsed(Value::String(text.clone())),
                        ))
                        .ok();
                }
            },
        )?;
        if usage.input_tokens > 0 || usage.output_tokens > 0 {
            let mut tmap = BTreeMap::new();
            tmap.insert("in".to_string(), Value::Integer(usage.input_tokens as i64));
            tmap.insert(
                "out".to_string(),
                Value::Integer(usage.output_tokens as i64),
            );
            self.broker_write(&path!("history/turn/tokens"), Value::Map(tmap));
        }
        Ok((events, usage.input_tokens, usage.output_tokens))
    }

    fn execute_tool(&mut self, call: &ToolCall) -> Result<String, String> {
        let decision = self.policy.check(&call.name, &call.input);
        match decision {
            crate::policy::CheckResult::Allow => {
                self.stats.allowed += 1;
                self.execute_tool_inner(call)
            }
            crate::policy::CheckResult::Deny(reason) => {
                self.stats.denied += 1;
                Err(format!("denied: {reason}"))
            }
            crate::policy::CheckResult::Ask {
                tool,
                input_preview,
                ..
            } => {
                self.stats.asked += 1;

                // Write approval request through broker — blocks until TUI responds
                let mut req = BTreeMap::new();
                req.insert("tool_name".to_string(), Value::String(tool));
                req.insert("input_preview".to_string(), Value::String(input_preview));
                let result = self.rt_handle.block_on(
                    self.scoped_client
                        .write(&path!("approval/request"), Record::parsed(Value::Map(req))),
                );

                if result.is_ok() {
                    // Read the response
                    let resp = self
                        .rt_handle
                        .block_on(self.scoped_client.read(&path!("approval/response")))
                        .ok()
                        .flatten()
                        .and_then(|r| match r.as_value() {
                            Some(Value::String(s)) => Some(s.clone()),
                            _ => None,
                        })
                        .unwrap_or_else(|| "deny_once".to_string());

                    match resp.as_str() {
                        "allow_once" => {
                            self.stats.allowed += 1;
                            self.execute_tool_inner(call)
                        }
                        "allow_session" => {
                            self.policy.session_allow(&call.name, &call.input);
                            self.stats.allowed += 1;
                            self.execute_tool_inner(call)
                        }
                        "allow_always" => {
                            self.policy.persist_allow(&call.name, &call.input);
                            self.stats.allowed += 1;
                            self.execute_tool_inner(call)
                        }
                        "deny_session" => {
                            self.policy.session_deny(&call.name, &call.input);
                            self.stats.denied += 1;
                            Err("denied by user".into())
                        }
                        "deny_always" => {
                            self.policy.persist_deny(&call.name, &call.input);
                            self.stats.denied += 1;
                            Err("denied by user".into())
                        }
                        _ => {
                            // "deny_once" or unknown
                            self.stats.denied += 1;
                            Err("denied by user".into())
                        }
                    }
                } else {
                    self.stats.denied += 1;
                    Err("denied: approval timeout".into())
                }
            }
        }
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
                let mut map = BTreeMap::new();
                map.insert("name".to_string(), Value::String(name));
                map.insert("status".to_string(), Value::String("running".to_string()));
                self.broker_write(&path!("history/turn/tool"), Value::Map(map));
            }
            AgentEvent::ToolCallResult { .. } => {
                self.broker_write(&path!("history/turn/tool"), Value::Null);
            }
            AgentEvent::TurnEnd => {
                self.broker_write(&path!("history/turn/thinking"), Value::Bool(false));
            }
            AgentEvent::Error(e) => {
                let msg = serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": format!("error: {e}")}]});
                self.broker_write(&path!("history/append"), json_to_value(msg));
                self.broker_write(&path!("history/turn/thinking"), Value::Bool(false));
            }
        }
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
