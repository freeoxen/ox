use ox_gate::{GateStore, ProviderConfig};
use ox_kernel::{AgentEvent, CompletionRequest, Record, StreamEvent, Value, Writer, path};
use ox_runtime::{AgentModule, AgentRuntime, HostEffects, HostStore};
use ox_tools::completion::CompletionTransport;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::thread;
use structfs_core_store::Reader as _;
use structfs_serde_store::json_to_value;

use crate::policy::PolicyStats;

// ---------------------------------------------------------------------------
// CliCompletionTransport — reqwest-based CompletionTransport for the CLI
// ---------------------------------------------------------------------------

/// Native HTTP transport that wraps [`crate::transport::streaming_fetch`].
///
/// Holds the reqwest client, provider config, and API key. Also holds a broker
/// handle so streaming text deltas and token usage can be written to the TUI
/// in real time.
struct CliCompletionTransport {
    client: reqwest::blocking::Client,
    config: ProviderConfig,
    api_key: String,
    scoped_client: ox_broker::ClientHandle,
    rt_handle: tokio::runtime::Handle,
}

impl CompletionTransport for CliCompletionTransport {
    fn send(
        &self,
        request: &CompletionRequest,
        on_event: &dyn Fn(&StreamEvent),
    ) -> Result<(Vec<StreamEvent>, u32, u32), String> {
        let scoped = self.scoped_client.clone();
        let handle = self.rt_handle.clone();
        let (events, usage) = crate::transport::streaming_fetch(
            &self.client,
            &self.config,
            &self.api_key,
            request,
            &|event| {
                on_event(event);
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
            self.rt_handle
                .block_on(self.scoped_client.write(
                    &path!("history/turn/tokens"),
                    Record::parsed(Value::Map(tmap)),
                ))
                .ok();
        }
        Ok((events, usage.input_tokens, usage.output_tokens))
    }
}

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
        let tid = thread_id.to_string();
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
            tracing::info!(thread_id = %thread_id, title = %title, "agent worker spawned");
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
    // Build ToolStore — primary tool execution backend
    let executor = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|dir| dir.join("ox-tool-exec")))
        .unwrap_or_else(|| PathBuf::from("ox-tool-exec"));
    let sandbox_policy: Arc<dyn ox_tools::sandbox::SandboxPolicy> = if no_policy {
        Arc::new(ox_tools::sandbox::PermissivePolicy)
    } else {
        Arc::new(crate::clash_sandbox::ClashSandboxPolicy::new(
            workspace.clone(),
        ))
    };
    let fs_module =
        ox_tools::fs::FsModule::new(workspace.clone(), executor.clone(), sandbox_policy.clone());
    let os_module = ox_tools::os::OsModule::new(workspace.clone(), executor, sandbox_policy);
    let gate = GateStore::new();
    let completion_module = ox_tools::completion::CompletionModule::new(gate);
    let tool_store = ox_tools::ToolStore::new(fs_module, os_module, completion_module);

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
    if let Ok(val) = structfs_serde_store::to_value(&tool_store.tool_schemas_for_model()) {
        adapter
            .write(&path!("tools/schemas"), Record::parsed(val))
            .ok();
    }

    // Read provider and API key from thread's GateStore (resolves through config handle)
    let default_account = match adapter.read(&path!("gate/defaults/account")) {
        Ok(Some(r)) => match r.as_value() {
            Some(Value::String(s)) => s.clone(),
            _ => "anthropic".to_string(),
        },
        _ => "anthropic".to_string(),
    };
    let provider = match adapter.read(&ox_path::oxpath!(
        "gate",
        "accounts",
        default_account,
        "provider"
    )) {
        Ok(Some(r)) => match r.as_value() {
            Some(Value::String(s)) => s.clone(),
            _ => "anthropic".to_string(),
        },
        _ => "anthropic".to_string(),
    };
    let api_key_for_transport = match adapter.read(&ox_path::oxpath!(
        "gate",
        "accounts",
        default_account,
        "key"
    )) {
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

    // Inject the CLI completion transport into the CompletionModule.
    // This gives CompletionModule the ability to execute LLM completions
    // end-to-end via StructFS write/read, independent of HostEffects.
    let cli_transport = CliCompletionTransport {
        client: reqwest::blocking::Client::new(),
        config: provider_config.clone(),
        api_key: api_key_for_transport.clone(),
        scoped_client: scoped_client.clone(),
        rt_handle: rt_handle.clone(),
    };
    let mut tool_store = tool_store;
    tool_store
        .completions_mut()
        .set_transport(Box::new(cli_transport));

    // Wrap ToolStore in PolicyStore with CliPolicyCheck for permission enforcement.
    let policy_check =
        crate::policy_check::CliPolicyCheck::new(policy, scoped_client.clone(), rt_handle.clone());
    let mut gated_store = ox_tools::policy_store::PolicyStore::new(tool_store, policy_check);

    tracing::info!(
        thread_id = %thread_id,
        default_account = %default_account,
        provider = %provider,
        has_key = !api_key_for_transport.is_empty(),
        "agent worker ready"
    );

    while let Ok(input) = prompt_rx.recv() {
        tracing::debug!(thread_id = %thread_id, input_len = input.len(), "prompt received");

        // Write user message to history
        let user_json = serde_json::json!({"role": "user", "content": input});
        if let Err(e) = adapter.write(
            &path!("history/append"),
            Record::parsed(json_to_value(user_json)),
        ) {
            tracing::error!(thread_id = %thread_id, error = %e, "history append failed");
            continue;
        }

        let effects = CliEffects {
            thread_id: thread_id.clone(),
            gated_store,
            scoped_client: scoped_client.clone(),
            rt_handle: rt_handle.clone(),
            stats: PolicyStats::default(),
        };

        let host_store = HostStore::new(adapter, effects);
        tracing::debug!(thread_id = %thread_id, "running wasm module");
        let (returned_store, result) = module.run(host_store);

        adapter = returned_store.backend;
        gated_store = returned_store.effects.gated_store;

        match &result {
            Ok(()) => tracing::debug!(thread_id = %thread_id, "agent run complete"),
            Err(e) => tracing::error!(thread_id = %thread_id, error = %e, "agent run failed"),
        }

        if let Err(e) = &result {
            // Write error to history before commit
            let msg = serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": format!("error: {e}")}]});
            adapter
                .write(&path!("history/append"), Record::parsed(json_to_value(msg)))
                .ok();
        }

        // Commit the turn — finalizes streaming text into permanent messages.
        // Must happen BEFORE save so the ledger gets committed messages, not
        // duplicated streaming partials.
        adapter
            .write(&path!("history/commit"), Record::parsed(Value::Null))
            .ok();

        // Clear turn state (thinking = false)
        rt_handle
            .block_on(scoped_client.write(
                &path!("history/turn/thinking"),
                Record::parsed(Value::Bool(false)),
            ))
            .ok();

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
                    &ox_path::oxpath!("inbox", "threads"),
                    Record::parsed(Value::Map(update)),
                ))
                .ok();
        }
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
