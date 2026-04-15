use ox_gate::{GateStore, ProviderConfig};
use ox_kernel::{AgentEvent, CompletionRequest, Record, StreamEvent, Value, path};
use ox_runtime::{AgentModule, AgentRuntime, HostEffects, HostStore};
use ox_tools::completion::CompletionTransport;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::thread;
use structfs_core_store::Reader as _;

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
                    },
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
Always read a file before modifying it. Be concise.\n\n\
IMPORTANT: When you have completed the user's request, respond with your final answer as plain text. \
Do NOT continue making tool calls after you have the information needed to answer. \
If a tool call fails or returns unexpected results, explain the problem to the user \
rather than retrying the same call. Never repeat the same tool call more than once.";

/// Embedded agent Wasm module (built by build.rs from ox-wasm).
pub(crate) const AGENT_WASM: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/agent.wasm"));

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
    adapter
        .write_typed(
            &path!("tools/schemas"),
            &tool_store.tool_schemas_for_model(),
        )
        .ok();

    // Read provider and API key from thread's GateStore (resolves through config handle)
    let default_account = adapter
        .read_typed::<String>(&path!("gate/defaults/account"))
        .ok()
        .flatten()
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
    tool_store
        .completions_mut()
        .set_transport(Box::new(cli_transport));

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
        has_key = !api_key_for_transport.is_empty(),
        "agent worker ready"
    );

    while let Ok(input) = prompt_rx.recv() {
        tracing::debug!(thread_id = %thread_id, input_len = input.len(), "prompt received");

        // Write user message to history
        let user_json = serde_json::json!({"role": "user", "content": input});
        if let Err(e) = adapter.write_typed(&path!("history/append"), &user_json) {
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
            adapter.write_typed(&path!("history/append"), &msg).ok();
        }

        // Clear all ephemeral turn state (streaming text, thinking, tool status).
        // The kernel already wrote the assistant message to log/append.
        adapter.write_typed(&path!("history/turn/clear"), &()).ok();

        // Persist conversation state for restart recovery
        save_thread_state(&mut adapter, &inbox_root, &thread_id, &title);

        // Write inbox metadata updates through broker
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        {
            let new_state = if result.is_ok() {
                ox_types::ThreadState::WaitingForInput
            } else {
                ox_types::ThreadState::Errored
            };
            let tid_comp = match ox_kernel::PathComponent::try_new(&thread_id) {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(error = %e, "invalid thread id for state update path");
                    continue;
                }
            };
            let update = ox_types::UpdateThread {
                id: None,
                thread_state: Some(new_state),
                inbox_state: None,
                updated_at: Some(now),
            };
            rt_handle
                .block_on(
                    broker_client
                        .write_typed(&ox_path::oxpath!("inbox", "threads", tid_comp), &update),
                )
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

    if let Err(e) = ox_inbox::snapshot::save(
        store,
        &thread_dir,
        thread_id,
        title,
        &[],
        now,
        &ox_inbox::snapshot::PARTICIPATING_MOUNTS,
    ) {
        tracing::error!(
            thread_id,
            path = %thread_dir.display(),
            error = %e,
            "failed to save thread snapshot — conversation may be lost on restart"
        );
        // Surface error to the user in the thread view
        let error_msg = serde_json::json!({
            "type": "error",
            "message": format!("Failed to save thread: {e}. Conversation may be lost on restart."),
        });
        let val = structfs_serde_store::json_to_value(error_msg);
        let _ = store.write(
            &structfs_core_store::Path::parse("log/append").unwrap(),
            structfs_core_store::Record::parsed(val),
        );
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
}
