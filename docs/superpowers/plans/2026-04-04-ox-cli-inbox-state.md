# ox-cli Inbox State + Multi-Agent Orchestration (Plan 2a)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restructure ox-cli's App state to manage multiple concurrent agent threads via ox-inbox metadata + ox-runtime Wasm instances, with multiplexed event channels and a thread pool for active agents.

**Architecture:** Replace the single `agent_thread` with an `AgentPool` that manages Wasmtime instances per-thread. One shared Engine + Module (loaded once). Per-agent: a Namespace + HostStore created on demand, run on a thread pool worker when active. All agents send tagged events through one shared channel. ox-inbox provides persistent thread metadata (SQLite + JSONL). The TUI routes events by thread ID.

**Tech Stack:** Rust (edition 2024), ox-inbox, ox-runtime, ratatui 0.29, crossterm 0.28

**Spec:** `docs/superpowers/specs/2026-04-04-ox-inbox-design.md`

**Depends on:** ox-inbox crate (Plan 1), ox-runtime crate (Plan 3) — both committed.

---

### File Structure

| File | Responsibility |
|------|---------------|
| `crates/ox-cli/src/agents.rs` | **NEW.** AgentPool — manages Wasm instances, thread pool, event multiplexing |
| `crates/ox-cli/src/app.rs` | **MODIFY.** Multi-thread App state, tab model, event routing by thread ID |
| `crates/ox-cli/src/tui.rs` | **MODIFY.** Event loop drains tagged events, routes to active thread |
| `crates/ox-cli/src/main.rs` | **MODIFY.** Pass inbox root + workspace to App |
| `crates/ox-cli/Cargo.toml` | **MODIFY.** Add ox-inbox dependency |

---

### Task 1: Tagged Events + Thread ID Model

**Files:**
- Modify: `crates/ox-cli/src/app.rs`

The foundational change: every event from an agent carries the thread ID it belongs to, so the TUI can route it.

- [ ] **Step 1: Add thread ID to AppEvent and AppControl**

In `crates/ox-cli/src/app.rs`, modify the event enums:

```rust
/// Events flowing from agent threads to the TUI, tagged with thread ID.
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

/// Non-Clone event — carries the oneshot response channel, tagged with thread ID.
pub enum AppControl {
    PermissionRequest {
        thread_id: String,
        tool: String,
        input_preview: String,
        respond: mpsc::Sender<ApprovalResponse>,
    },
}
```

- [ ] **Step 2: Update handle_event to route by thread_id**

Replace the `handle_event` method. For now, it still updates the single-thread state (messages, tokens, etc.) — multi-thread routing comes in Task 3. The key change is destructuring the new enum shapes:

```rust
pub fn handle_event(&mut self, event: AppEvent) {
    self.scroll = 0;
    match event {
        AppEvent::Agent { event: AgentEvent::TextDelta(text), .. } => {
            if let Some(ChatMessage::AssistantChunk(ref mut s)) = self.messages.last_mut() {
                s.push_str(&text);
            } else {
                self.messages.push(ChatMessage::AssistantChunk(text));
            }
        }
        AppEvent::Agent { event: AgentEvent::ToolCallStart { name }, .. } => {
            self.messages.push(ChatMessage::ToolCall { name });
        }
        AppEvent::Agent { event: AgentEvent::ToolCallResult { name, result }, .. } => {
            self.messages.push(ChatMessage::ToolResult { name, output: result });
        }
        AppEvent::Agent { event: AgentEvent::TurnStart, .. } => {}
        AppEvent::Agent { event: AgentEvent::TurnEnd, .. } => {}
        AppEvent::Agent { event: AgentEvent::Error(e), .. } => {
            self.messages.push(ChatMessage::Error(e));
        }
        AppEvent::Usage { input_tokens, output_tokens, .. } => {
            self.tokens_in += input_tokens;
            self.tokens_out += output_tokens;
        }
        AppEvent::PolicyStats { stats, .. } => {
            self.policy_stats = stats;
        }
        AppEvent::Done { result: Ok(_), .. } => {
            self.thinking = false;
        }
        AppEvent::Done { result: Err(e), .. } => {
            self.messages.push(ChatMessage::Error(e));
            self.thinking = false;
        }
    }
}
```

- [ ] **Step 3: Update CliEffects to tag events with thread_id**

Add `thread_id: String` field to `CliEffects`. Update all `event_tx.send()` calls in `HostEffects` impl and `agent_thread` to use the tagged variants:

```rust
struct CliEffects {
    thread_id: String,
    client: reqwest::blocking::Client,
    // ... rest unchanged
}

impl HostEffects for CliEffects {
    fn complete(&mut self, request: &CompletionRequest) -> Result<(Vec<StreamEvent>, u32, u32), String> {
        let thread_id = self.thread_id.clone();
        let tx = self.event_tx.clone();
        let (events, usage) = crate::transport::streaming_fetch(
            &self.client, &self.config, &self.api_key, request,
            &|event| {
                if let StreamEvent::TextDelta(text) = event {
                    tx.send(AppEvent::Agent {
                        thread_id: thread_id.clone(),
                        event: AgentEvent::TextDelta(text.clone()),
                    }).ok();
                }
            },
        )?;
        if usage.input_tokens > 0 || usage.output_tokens > 0 {
            self.event_tx.send(AppEvent::Usage {
                thread_id: self.thread_id.clone(),
                input_tokens: usage.input_tokens,
                output_tokens: usage.output_tokens,
            }).ok();
        }
        Ok((events, usage.input_tokens, usage.output_tokens))
    }

    fn execute_tool(&mut self, call: &ToolCall) -> Result<String, String> {
        // Same policy logic, but tag PolicyStats events:
        // self.event_tx.send(AppEvent::PolicyStats {
        //     thread_id: self.thread_id.clone(),
        //     stats: self.stats.clone(),
        // }).ok();
        // ... rest of policy enforcement unchanged
        todo!("update all send() calls with thread_id tagging")
    }

    fn emit_event(&mut self, event: AgentEvent) {
        self.event_tx.send(AppEvent::Agent {
            thread_id: self.thread_id.clone(),
            event,
        }).ok();
    }
}
```

Update ALL `event_tx.send()` calls in the `execute_tool` match arms and in `agent_thread` (the Done events, error events, etc.) to include `thread_id`.

In `agent_thread`, set the thread_id. For now, use a fixed ID like `"main"` — it becomes dynamic in Task 2:

```rust
fn agent_thread(
    thread_id: String,  // NEW parameter
    model: String,
    // ... rest unchanged
) {
    // ... setup ...
    let effects = CliEffects {
        thread_id: thread_id.clone(),
        client,
        // ...
    };
    // ...
    event_tx.send(AppEvent::Done {
        thread_id: thread_id.clone(),
        result: done_result,
    }).ok();
}
```

Update `App::new()` to pass `"main".to_string()` as thread_id to `agent_thread`.

- [ ] **Step 4: Update tui.rs for new AppControl shape**

In `tui.rs`, the permission request destructuring needs the thread_id field:

```rust
if let Ok(AppControl::PermissionRequest {
    thread_id: _,  // ignored for now — approval applies to active thread
    tool,
    input_preview,
    respond,
}) = app.control_rx.try_recv()
```

- [ ] **Step 5: Verify compilation**

Run: `cargo check -p ox-cli`
Expected: Clean compilation with new tagged event types.

- [ ] **Step 6: Commit**

```bash
git add crates/ox-cli/src/app.rs crates/ox-cli/src/tui.rs
git commit -m "refactor(ox-cli): tag all agent events with thread_id"
```

---

### Task 2: AgentPool — Multi-Agent Orchestration

**Files:**
- Create: `crates/ox-cli/src/agents.rs`
- Modify: `crates/ox-cli/src/app.rs`
- Modify: `crates/ox-cli/src/main.rs`
- Modify: `crates/ox-cli/Cargo.toml`

The AgentPool manages multiple Wasm agent instances. One shared Engine + Module. Per-thread state created on demand. Thread pool workers execute active agents.

- [ ] **Step 1: Add ox-inbox dependency**

In `crates/ox-cli/Cargo.toml`, add:
```toml
ox-inbox = { path = "../ox-inbox" }
```

- [ ] **Step 2: Create `crates/ox-cli/src/agents.rs`**

```rust
use crate::app::{AppControl, AppEvent, ApprovalResponse, CliEffects};
use crate::policy::PolicyStats;
use ox_context::{ModelProvider, Namespace, SystemProvider, ToolsProvider};
use ox_gate::{GateStore, ProviderConfig};
use ox_history::HistoryProvider;
use ox_inbox::InboxStore;
use ox_kernel::{
    path, AgentEvent, CompletionRequest, Reader, Record, StreamEvent, ToolCall, ToolRegistry,
    Value, Writer,
};
use ox_runtime::{AgentModule, AgentRuntime, HostStore};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{mpsc, Arc};
use structfs_serde_store::json_to_value;

const SYSTEM_PROMPT: &str = "\
You are an expert software engineer working in a coding CLI. \
You have tools for reading files, writing files, editing files, \
and running shell commands. \
Always read a file before modifying it. Be concise.";

/// Embedded agent Wasm module.
const AGENT_WASM: &[u8] = include_bytes!("../../../target/agent.wasm");

/// Per-thread state held by the pool.
struct ThreadState {
    /// Channel to send prompts to this thread's worker.
    prompt_tx: mpsc::Sender<String>,
}

/// Manages multiple concurrent Wasm agent instances.
pub struct AgentPool {
    runtime: AgentRuntime,
    module: AgentModule,
    /// Per-thread prompt channels.
    threads: HashMap<String, ThreadState>,
    /// Shared event channel — all agents send here, tagged with thread_id.
    event_tx: mpsc::Sender<AppEvent>,
    /// Shared control channel — all agents send permission requests here.
    control_tx: mpsc::Sender<AppControl>,
    /// Inbox store for thread metadata.
    inbox: InboxStore,
    // Config shared across all agents
    model: String,
    max_tokens: u32,
    provider: String,
    api_key: String,
    workspace: PathBuf,
    no_policy: bool,
}

impl AgentPool {
    pub fn new(
        model: String,
        max_tokens: u32,
        provider: String,
        api_key: String,
        workspace: PathBuf,
        no_policy: bool,
        inbox: InboxStore,
        event_tx: mpsc::Sender<AppEvent>,
        control_tx: mpsc::Sender<AppControl>,
    ) -> Result<Self, String> {
        let runtime = AgentRuntime::new()?;
        let module = runtime.load_module_from_bytes(AGENT_WASM)?;
        Ok(Self {
            runtime,
            module,
            threads: HashMap::new(),
            event_tx,
            control_tx,
            inbox,
            model,
            max_tokens,
            provider,
            api_key,
            workspace,
            no_policy,
        })
    }

    /// Create a new thread in the inbox and spawn its agent worker.
    /// Returns the thread ID.
    pub fn create_thread(&mut self, title: &str, labels: &[&str]) -> Result<String, String> {
        // Write to inbox store
        let mut map = std::collections::BTreeMap::new();
        map.insert("title".to_string(), Value::String(title.to_string()));
        if !labels.is_empty() {
            map.insert(
                "labels".to_string(),
                Value::Array(labels.iter().map(|l| Value::String(l.to_string())).collect()),
            );
        }
        let path = self
            .inbox
            .write(
                &path!("threads"),
                Record::parsed(Value::Map(map)),
            )
            .map_err(|e| e.to_string())?;
        let thread_id = path.iter().nth(1).unwrap().clone();

        self.spawn_worker(thread_id.clone());
        Ok(thread_id)
    }

    /// Send a prompt to an existing thread.
    pub fn send_prompt(&self, thread_id: &str, prompt: &str) -> Result<(), String> {
        let state = self
            .threads
            .get(thread_id)
            .ok_or_else(|| format!("no thread {thread_id}"))?;
        state
            .prompt_tx
            .send(prompt.to_string())
            .map_err(|e| e.to_string())
    }

    /// Spawn a background worker for a thread.
    fn spawn_worker(&mut self, thread_id: String) {
        let (prompt_tx, prompt_rx) = mpsc::channel::<String>();
        self.threads.insert(
            thread_id.clone(),
            ThreadState { prompt_tx },
        );

        let model = self.model.clone();
        let max_tokens = self.max_tokens;
        let provider = self.provider.clone();
        let api_key = self.api_key.clone();
        let workspace = self.workspace.clone();
        let no_policy = self.no_policy;
        let event_tx = self.event_tx.clone();
        let control_tx = self.control_tx.clone();
        let module = self.module.clone();

        std::thread::spawn(move || {
            agent_worker(
                thread_id,
                model,
                provider,
                max_tokens,
                api_key,
                workspace,
                no_policy,
                module,
                prompt_rx,
                event_tx,
                control_tx,
            );
        });
    }

    /// Get a reference to the inbox store.
    pub fn inbox(&mut self) -> &mut InboxStore {
        &mut self.inbox
    }
}

/// Worker function for a single agent thread.
/// Runs on a thread pool worker (currently: dedicated OS thread).
#[allow(clippy::too_many_arguments)]
fn agent_worker(
    thread_id: String,
    model: String,
    provider: String,
    max_tokens: u32,
    api_key: String,
    workspace: PathBuf,
    no_policy: bool,
    module: AgentModule,
    prompt_rx: mpsc::Receiver<String>,
    event_tx: mpsc::Sender<AppEvent>,
    control_tx: mpsc::Sender<AppControl>,
) {
    let tools_vec = crate::tools::standard_tools(workspace.clone());
    let mut tools = ToolRegistry::new();
    for tool in tools_vec {
        tools.register(tool);
    }

    let mut policy = if no_policy {
        crate::policy::PolicyGuard::permissive()
    } else {
        crate::policy::PolicyGuard::load(&workspace)
    };

    // Set up GateStore
    let mut gate = GateStore::new();
    gate.write(
        &ox_kernel::Path::from_components(vec![
            "accounts".to_string(),
            provider.clone(),
            "key".to_string(),
        ]),
        Record::parsed(Value::String(api_key.clone())),
    )
    .ok();

    let provider_config = crate::app::read_provider_config_from_gate(&mut gate, &provider)
        .unwrap_or_else(|_| ProviderConfig::anthropic());
    let api_key_for_transport =
        crate::app::read_account_key(&mut gate, &provider).unwrap_or_default();

    let send_config = provider_config.clone();
    let send_key = api_key_for_transport.clone();
    let send = Arc::new(crate::transport::make_send_fn(send_config, send_key));
    for tool in gate.create_completion_tools(send) {
        tools.register(tool);
    }

    // Build namespace
    let mut namespace = Namespace::new();
    namespace.mount(
        "system",
        Box::new(SystemProvider::new(SYSTEM_PROMPT.to_string())),
    );
    namespace.mount("history", Box::new(HistoryProvider::new()));
    namespace.mount("tools", Box::new(ToolsProvider::new(tools.schemas())));
    namespace.mount(
        "model",
        Box::new(ModelProvider::new(model, max_tokens)),
    );
    namespace.mount("gate", Box::new(gate));

    let mut client = reqwest::blocking::Client::new();

    // Wait for prompts
    while let Ok(input) = prompt_rx.recv() {
        // Write user message
        let user_json = serde_json::json!({"role": "user", "content": input});
        if let Err(e) = namespace.write(
            &path!("history/append"),
            Record::parsed(json_to_value(user_json)),
        ) {
            event_tx
                .send(AppEvent::Done {
                    thread_id: thread_id.clone(),
                    result: Err(e.to_string()),
                })
                .ok();
            continue;
        }

        // Update inbox state to running
        // (inbox store is on the main thread — would need channel to update)
        // For now, the TUI infers state from events.

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

        let host_store = HostStore::new(namespace, effects);
        let (returned_store, result) = module.run(host_store);

        namespace = returned_store.namespace;
        client = returned_store.effects.client;
        tools = returned_store.effects.tools;
        let stats = returned_store.effects.stats.clone();
        policy = returned_store.effects.policy;

        event_tx
            .send(AppEvent::PolicyStats {
                thread_id: thread_id.clone(),
                stats,
            })
            .ok();

        event_tx
            .send(AppEvent::Done {
                thread_id: thread_id.clone(),
                result: match result {
                    Ok(()) => Ok(String::new()),
                    Err(e) => Err(e),
                },
            })
            .ok();
    }
}
```

- [ ] **Step 3: Make helper functions public in app.rs**

The `read_provider_config_from_gate` and `read_account_key` functions need to be `pub(crate)` so `agents.rs` can use them:

```rust
pub(crate) fn read_provider_config_from_gate(...) -> ... { ... }
pub(crate) fn read_account_key(...) -> ... { ... }
```

Also make `CliEffects` and its fields `pub(crate)`.

- [ ] **Step 4: Restructure App to use AgentPool**

Replace the single-agent App::new with one that creates an AgentPool:

```rust
pub struct App {
    // -- Multi-thread state --
    pub pool: AgentPool,
    /// Currently focused thread ID (if viewing a thread tab).
    pub active_thread: Option<String>,
    /// Open tabs: thread IDs in order.
    pub tabs: Vec<String>,
    /// Per-thread conversation state for rendering.
    pub thread_views: HashMap<String, ThreadView>,

    // -- Shared UI state --
    pub input: String,
    pub cursor: usize,
    pub scroll: u16,
    pub should_quit: bool,
    pub model: String,
    pub provider: String,
    pub event_rx: mpsc::Receiver<AppEvent>,
    pub control_rx: mpsc::Receiver<AppControl>,
    pub input_history: Vec<String>,
    history_cursor: usize,
    input_draft: String,
    pub pending_approval: Option<ApprovalState>,
    pub pending_customize: Option<CustomizeState>,
}

/// Per-thread rendering state.
#[derive(Debug, Clone, Default)]
pub struct ThreadView {
    pub messages: Vec<ChatMessage>,
    pub thinking: bool,
    pub tokens_in: u32,
    pub tokens_out: u32,
    pub policy_stats: PolicyStats,
}
```

- [ ] **Step 5: Update App::new to create AgentPool**

```rust
impl App {
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

        let inbox = InboxStore::open(&inbox_root)
            .map_err(|e| e.to_string())?;

        let pool = AgentPool::new(
            model.clone(),
            max_tokens,
            provider.clone(),
            api_key,
            workspace,
            no_policy,
            inbox,
            event_tx,
            control_tx,
        )?;

        Ok(Self {
            pool,
            active_thread: None,
            tabs: Vec::new(),
            thread_views: HashMap::new(),
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
```

- [ ] **Step 6: Update handle_event to route to ThreadView**

```rust
pub fn handle_event(&mut self, event: AppEvent) {
    self.scroll = 0;
    match &event {
        AppEvent::Agent { thread_id, event: agent_event } => {
            let view = self.thread_views.entry(thread_id.clone()).or_default();
            match agent_event {
                AgentEvent::TextDelta(text) => {
                    if let Some(ChatMessage::AssistantChunk(ref mut s)) = view.messages.last_mut() {
                        s.push_str(text);
                    } else {
                        view.messages.push(ChatMessage::AssistantChunk(text.clone()));
                    }
                }
                AgentEvent::ToolCallStart { name } => {
                    view.messages.push(ChatMessage::ToolCall { name: name.clone() });
                }
                AgentEvent::ToolCallResult { name, result } => {
                    view.messages.push(ChatMessage::ToolResult {
                        name: name.clone(),
                        output: result.clone(),
                    });
                }
                AgentEvent::Error(e) => {
                    view.messages.push(ChatMessage::Error(e.clone()));
                }
                _ => {}
            }
        }
        AppEvent::Usage { thread_id, input_tokens, output_tokens } => {
            let view = self.thread_views.entry(thread_id.clone()).or_default();
            view.tokens_in += input_tokens;
            view.tokens_out += output_tokens;
        }
        AppEvent::PolicyStats { thread_id, stats } => {
            let view = self.thread_views.entry(thread_id.clone()).or_default();
            view.policy_stats = stats.clone();
        }
        AppEvent::Done { thread_id, result } => {
            let view = self.thread_views.entry(thread_id.clone()).or_default();
            if let Err(e) = result {
                view.messages.push(ChatMessage::Error(e.clone()));
            }
            view.thinking = false;
        }
    }
}
```

- [ ] **Step 7: Update submit to target active thread**

```rust
pub fn submit(&mut self) {
    if self.input.is_empty() {
        return;
    }
    let input = std::mem::take(&mut self.input);
    self.cursor = 0;
    self.input_history.push(input.clone());
    self.history_cursor = self.input_history.len();
    self.input_draft.clear();
    self.scroll = 0;

    if let Some(ref thread_id) = self.active_thread {
        // Send to existing thread
        let view = self.thread_views.entry(thread_id.clone()).or_default();
        view.messages.push(ChatMessage::User(input.clone()));
        view.thinking = true;
        self.pool.send_prompt(thread_id, &input).ok();
    } else {
        // Create new thread from inbox view
        match self.pool.create_thread(&input, &[]) {
            Ok(thread_id) => {
                let view = self.thread_views.entry(thread_id.clone()).or_default();
                view.messages.push(ChatMessage::User(input.clone()));
                view.thinking = true;
                self.pool.send_prompt(&thread_id, &input).ok();
                // Open as tab and switch to it
                if !self.tabs.contains(&thread_id) {
                    self.tabs.push(thread_id.clone());
                }
                self.active_thread = Some(thread_id);
            }
            Err(e) => {
                // Show error in... somewhere. For now, ignore.
                eprintln!("failed to create thread: {e}");
            }
        }
    }
}
```

- [ ] **Step 8: Update main.rs**

```rust
let inbox_root = dirs::home_dir()
    .unwrap_or_else(|| PathBuf::from("."))
    .join(".ox");

let mut app = app::App::new(
    cli.provider,
    model,
    cli.max_tokens,
    api_key,
    workspace,
    inbox_root,
    cli.no_policy,
).map_err(|e| format!("failed to initialize: {e}"))?;
```

Remove the `session_path` logic — sessions are now managed by ox-inbox threads. Add a `dirs` dependency or use a simple home dir lookup.

- [ ] **Step 9: Add module declaration**

In `main.rs`, add `mod agents;`.

- [ ] **Step 10: Update tui.rs rendering for active thread**

The `draw` function needs to read from the active thread's `ThreadView` instead of `app.messages`:

```rust
fn draw(frame: &mut Frame, app: &App, theme: &Theme) {
    // Get messages for the active thread (or empty if on inbox)
    let (messages, thinking, tokens_in, tokens_out, policy_stats) =
        if let Some(ref tid) = app.active_thread {
            if let Some(view) = app.thread_views.get(tid) {
                (&view.messages, view.thinking, view.tokens_in, view.tokens_out, &view.policy_stats)
            } else {
                (&Vec::new() as &Vec<ChatMessage>, false, 0, 0, &PolicyStats::default())
            }
        } else {
            // Inbox view — no messages, show thread list instead
            // (handled in Plan 2b)
            (&Vec::new() as &Vec<ChatMessage>, false, 0, 0, &PolicyStats::default())
        };

    // ... rest of rendering uses these locals instead of app.messages, app.thinking, etc.
}
```

- [ ] **Step 11: Remove old agent_thread and AGENT_WASM from app.rs**

Delete the `agent_thread` function, `AGENT_WASM` constant, `SYSTEM_PROMPT` constant, and the single-thread channel setup from `app.rs`. These now live in `agents.rs`.

Also remove the old `App::new` agent thread spawn logic.

- [ ] **Step 12: Clean up imports and verify compilation**

Run: `cargo check -p ox-cli`
Expected: Clean compilation. The TUI works but only shows one thread at a time.

- [ ] **Step 13: Commit**

```bash
git add crates/ox-cli/
git commit -m "feat(ox-cli): multi-agent orchestration via AgentPool + ox-inbox"
```

---

### Task 3: Tab Management

**Files:**
- Modify: `crates/ox-cli/src/app.rs`
- Modify: `crates/ox-cli/src/tui.rs`

Add tab switching, opening, and closing. The tab bar renders in Plan 2b — this task adds the state and key bindings.

- [ ] **Step 1: Add tab navigation methods to App**

```rust
impl App {
    /// Switch to inbox view (no active thread).
    pub fn go_to_inbox(&mut self) {
        self.active_thread = None;
        self.scroll = 0;
    }

    /// Open a thread as a tab and switch to it.
    pub fn open_thread(&mut self, thread_id: String) {
        if !self.tabs.contains(&thread_id) {
            self.tabs.push(thread_id.clone());
        }
        self.active_thread = Some(thread_id);
        self.scroll = 0;
    }

    /// Close the current tab (thread keeps running).
    pub fn close_current_tab(&mut self) {
        if let Some(ref tid) = self.active_thread {
            self.tabs.retain(|t| t != tid);
        }
        // Switch to last tab or inbox
        self.active_thread = self.tabs.last().cloned();
        self.scroll = 0;
    }

    /// Switch to next tab.
    pub fn next_tab(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        match &self.active_thread {
            None => {
                self.active_thread = self.tabs.first().cloned();
            }
            Some(tid) => {
                let idx = self.tabs.iter().position(|t| t == tid).unwrap_or(0);
                if idx + 1 < self.tabs.len() {
                    self.active_thread = Some(self.tabs[idx + 1].clone());
                } else {
                    self.active_thread = None; // wrap to inbox
                }
            }
        }
        self.scroll = 0;
    }

    /// Switch to previous tab.
    pub fn prev_tab(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        match &self.active_thread {
            None => {
                self.active_thread = self.tabs.last().cloned();
            }
            Some(tid) => {
                let idx = self.tabs.iter().position(|t| t == tid).unwrap_or(0);
                if idx > 0 {
                    self.active_thread = Some(self.tabs[idx - 1].clone());
                } else {
                    self.active_thread = None; // wrap to inbox
                }
            }
        }
        self.scroll = 0;
    }
}
```

- [ ] **Step 2: Add tab key bindings**

In `tui.rs`, add to `handle_normal_key`:

```rust
// Tab navigation
(KeyModifiers::CONTROL, KeyCode::Char('t')) => app.go_to_inbox(),
(KeyModifiers::CONTROL, KeyCode::Char('w')) => app.close_current_tab(),
(KeyModifiers::CONTROL, KeyCode::Right) => app.next_tab(),
(KeyModifiers::CONTROL, KeyCode::Left) => app.prev_tab(),
```

- [ ] **Step 3: Verify compilation**

Run: `cargo check -p ox-cli`

- [ ] **Step 4: Commit**

```bash
git add crates/ox-cli/src/app.rs crates/ox-cli/src/tui.rs
git commit -m "feat(ox-cli): tab management — open, close, switch"
```

---

### Task 4: Permission Routing by Thread

**Files:**
- Modify: `crates/ox-cli/src/tui.rs`
- Modify: `crates/ox-cli/src/app.rs`

Permission requests need to route to the correct thread tab and only display when that tab is active.

- [ ] **Step 1: Tag ApprovalState with thread_id**

In `app.rs`:

```rust
pub struct ApprovalState {
    pub thread_id: String,
    pub tool: String,
    pub input_preview: String,
    pub selected: usize,
    pub respond: mpsc::Sender<ApprovalResponse>,
}
```

- [ ] **Step 2: Update permission request handling in tui.rs**

```rust
// Check for permission requests — only show if matching active thread
if app.pending_approval.is_none() && app.pending_customize.is_none() {
    if let Ok(AppControl::PermissionRequest {
        thread_id,
        tool,
        input_preview,
        respond,
    }) = app.control_rx.try_recv()
    {
        // Auto-switch to the thread requesting permission
        app.open_thread(thread_id.clone());
        app.pending_approval = Some(ApprovalState {
            thread_id,
            tool,
            input_preview,
            selected: 0,
            respond,
        });
    }
}
```

- [ ] **Step 3: Verify and commit**

Run: `cargo check -p ox-cli`

```bash
git add crates/ox-cli/
git commit -m "feat(ox-cli): route permission requests to thread tabs"
```

---

### Summary

| Task | What it builds |
|------|---------------|
| 1 | Tagged events — every AppEvent/AppControl carries thread_id |
| 2 | AgentPool + agent_worker — multi-agent Wasm orchestration with ox-inbox |
| 3 | Tab management — open, close, switch between threads |
| 4 | Permission routing — approval dialogs auto-switch to requesting thread |

After Plan 2a, ox-cli can:
- Create multiple agent threads (each a Wasm instance)
- Route events to per-thread state
- Switch between threads via tabs
- Handle permission requests per-thread

The TUI still renders ONE view at a time (the active thread or inbox). **Plan 2b** adds: inbox view rendering, tab bar widget, compose UX, search/filter bar.

**Note:** `AgentModule::clone()` is required for `agents.rs` — verify that `wasmtime::Module` is `Clone`. If not, wrap in `Arc<AgentModule>`.
