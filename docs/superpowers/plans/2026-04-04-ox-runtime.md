# ox-runtime: Wasmtime Agent Execution Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the `ox-runtime` crate — a Wasmtime-based agent executor where each agent is a Wasm component that communicates exclusively through StructFS read/write calls, and the host intercepts effectful operations (HTTP, tool execution) transparently.

**Architecture:** Agent Wasm components import two host functions (`read` and `write` with JSON-serialized StructFS types) and export a `run()` entry point. The host provides a StructFS middleware layer that routes most operations to an in-memory Namespace, but intercepts writes to `gate/complete` (HTTP fetch) and `tools/execute/{name}` (tool execution with policy). ox-wasi is upgraded from a stub to the actual agent component compiled to `wasm32-wasip2`.

**Tech Stack:** Rust (edition 2024), wasmtime (with component-model), wit-bindgen, ox-kernel, ox-context, ox-history, ox-gate, structfs-serde-store

**Spec:** `docs/superpowers/specs/2026-04-04-ox-inbox-design.md` (Thread Runtime section)

---

### File Structure

| File | Responsibility |
|------|---------------|
| `wit/agent.wit` | WIT interface definition — store imports + agent exports |
| `crates/ox-runtime/Cargo.toml` | Host-side runtime crate manifest |
| `crates/ox-runtime/src/lib.rs` | Public API: AgentRuntime, AgentHandle, re-exports |
| `crates/ox-runtime/src/engine.rs` | Wasmtime Engine/Store setup, component loading |
| `crates/ox-runtime/src/host_store.rs` | StructFS middleware — routes reads/writes, intercepts effects |
| `crates/ox-runtime/src/bridge.rs` | JSON serialization helpers for StructFS types across Wasm boundary |
| `crates/ox-wasi/Cargo.toml` | Updated — add wit-bindgen, remove ox-core dep |
| `crates/ox-wasi/src/lib.rs` | Agent entry point — HostBridge Store impl + run() that drives kernel loop |

---

### Task 1: WIT Definition + Project Scaffolding

**Files:**
- Create: `wit/agent.wit`
- Create: `crates/ox-runtime/Cargo.toml`
- Create: `crates/ox-runtime/src/lib.rs`
- Create: `crates/ox-runtime/src/bridge.rs`
- Modify: `Cargo.toml` (workspace members)

- [ ] **Step 1: Create `wit/agent.wit`**

```wit
package ox:agent;

/// StructFS store operations provided by the host.
/// Paths and records are JSON-serialized strings.
interface store {
    /// Read from a StructFS path.
    /// Returns JSON-serialized Record, or none if path has no data.
    read: func(path: string) -> result<option<string>, string>;

    /// Write a JSON-serialized Record to a StructFS path.
    /// Returns the canonical path where data was written.
    write: func(path: string, data: string) -> result<string, string>;
}

/// An ox agent component.
world agent {
    import store;

    /// Run the agent loop. Called once per conversation turn (after user input).
    /// Returns ok on clean completion, err on failure.
    export run: func() -> result<_, string>;
}
```

- [ ] **Step 2: Create `crates/ox-runtime/Cargo.toml`**

```toml
[package]
name = "ox-runtime"
version = "0.1.0"
edition = "2024"
rust-version = "1.85"
license = "Apache-2.0"
description = "Wasmtime-based agent runtime for ox — loads and executes agent Wasm components"

[dependencies]
ox-kernel = { path = "../ox-kernel" }
ox-context = { path = "../ox-context" }
ox-history = { path = "../ox-history" }
ox-gate = { path = "../ox-gate" }
structfs-core-store = { workspace = true }
structfs-serde-store = { workspace = true }
wasmtime = { version = "29", features = ["component-model"] }
serde_json = "1"

[dev-dependencies]
tempfile = "3"
```

- [ ] **Step 3: Create `crates/ox-runtime/src/bridge.rs`**

This module handles JSON serialization of StructFS types across the Wasm boundary.

```rust
use structfs_core_store::{Error, Path, Record, Value};

/// Serialize a Record to a JSON string for the Wasm boundary.
pub fn record_to_json(record: &Record) -> Result<String, String> {
    let value = record
        .as_value()
        .ok_or_else(|| "cannot serialize raw record".to_string())?;
    let json = structfs_serde_store::value_to_json(value.clone());
    serde_json::to_string(&json).map_err(|e| e.to_string())
}

/// Deserialize a JSON string from the Wasm boundary into a Record.
pub fn json_to_record(json: &str) -> Result<Record, String> {
    let json_value: serde_json::Value =
        serde_json::from_str(json).map_err(|e| e.to_string())?;
    let value = structfs_serde_store::json_to_value(json_value);
    Ok(Record::parsed(value))
}

/// Serialize a StructFS Path to a string.
pub fn path_to_string(path: &Path) -> String {
    path.to_string()
}

/// Deserialize a string into a StructFS Path.
pub fn string_to_path(s: &str) -> Result<Path, String> {
    Path::parse(s).map_err(|e| e.to_string())
}

/// Serialize an Option<Record> to an Option<String> for read results.
pub fn read_result_to_json(result: Option<Record>) -> Result<Option<String>, String> {
    match result {
        Some(record) => Ok(Some(record_to_json(&record)?)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn record_round_trip() {
        let mut map = BTreeMap::new();
        map.insert("title".to_string(), Value::String("hello".to_string()));
        map.insert("count".to_string(), Value::Integer(42));
        let record = Record::parsed(Value::Map(map));

        let json = record_to_json(&record).unwrap();
        let restored = json_to_record(&json).unwrap();
        assert_eq!(record.as_value(), restored.as_value());
    }

    #[test]
    fn path_round_trip() {
        let path = Path::parse("threads/t_abc123/messages").unwrap();
        let s = path_to_string(&path);
        assert_eq!(s, "threads/t_abc123/messages");
        let restored = string_to_path(&s).unwrap();
        assert_eq!(path, restored);
    }

    #[test]
    fn empty_path_round_trip() {
        let path = Path::parse("").unwrap();
        let s = path_to_string(&path);
        let restored = string_to_path(&s).unwrap();
        assert_eq!(path, restored);
    }

    #[test]
    fn read_result_none() {
        let result = read_result_to_json(None).unwrap();
        assert!(result.is_none());
    }
}
```

- [ ] **Step 4: Create `crates/ox-runtime/src/lib.rs`**

```rust
pub mod bridge;

// engine and host_store added in later tasks
```

- [ ] **Step 5: Add to workspace `Cargo.toml`**

Add `"crates/ox-runtime"` to the `[workspace] members` list.

- [ ] **Step 6: Run tests**

Run: `cargo test -p ox-runtime`
Expected: 4 bridge tests pass

- [ ] **Step 7: Commit**

```bash
git add wit/ crates/ox-runtime/ Cargo.toml
git commit -m "feat(ox-runtime): WIT definition + bridge serialization"
```

---

### Task 2: Host Store — StructFS Middleware with Effect Interception

**Files:**
- Create: `crates/ox-runtime/src/host_store.rs`
- Modify: `crates/ox-runtime/src/lib.rs`

The HostStore wraps a Namespace and intercepts reads/writes that require real-world effects. This is the core abstraction — the agent module sees StructFS, but the host does HTTP and tool execution behind the scenes.

- [ ] **Step 1: Define HostStore and effect callback traits**

Create `crates/ox-runtime/src/host_store.rs`:

```rust
use ox_context::Namespace;
use ox_kernel::{
    path, AgentEvent, CompletionRequest, StreamEvent, ToolCall, ToolResult,
};
use structfs_core_store::{Error, Path, Record, Value};

/// Callback for operations that require host-side effects.
/// The host (ox-cli) implements this to handle HTTP, tool execution, and event relay.
pub trait HostEffects: Send {
    /// Perform an LLM completion. Called when the agent writes to `gate/complete`.
    /// Returns (events, input_tokens, output_tokens).
    fn complete(
        &mut self,
        request: &CompletionRequest,
    ) -> Result<(Vec<StreamEvent>, u32, u32), String>;

    /// Execute a tool call. Called when the agent writes to `tools/execute`.
    /// The host handles policy enforcement.
    /// Returns the tool result string.
    fn execute_tool(&mut self, call: &ToolCall) -> Result<String, String>;

    /// Emit an agent event to the TUI. Called when the agent writes to `events/emit`.
    fn emit_event(&mut self, event: AgentEvent);
}

/// StructFS middleware that wraps a Namespace and intercepts effectful paths.
pub struct HostStore<E: HostEffects> {
    pub namespace: Namespace,
    pub effects: E,
    /// Accumulated events from the last completion, consumed by the agent.
    pending_events: Option<Vec<StreamEvent>>,
}

impl<E: HostEffects> HostStore<E> {
    pub fn new(namespace: Namespace, effects: E) -> Self {
        Self {
            namespace,
            effects,
            pending_events: None,
        }
    }

    /// Handle a read call from the agent module.
    pub fn handle_read(&mut self, path: &Path) -> Result<Option<Record>, Error> {
        let components: Vec<&String> = path.iter().collect();

        match components.as_slice() {
            // Read pending completion events
            [g, c] if g.as_str() == "gate" && c.as_str() == "response" => {
                match self.pending_events.take() {
                    Some(events) => {
                        let json = serde_json::to_string(&events)
                            .map_err(|e| Error::store("HostStore", "read", e.to_string()))?;
                        let value = structfs_serde_store::json_to_value(
                            serde_json::Value::String(json),
                        );
                        Ok(Some(Record::parsed(value)))
                    }
                    None => Ok(None),
                }
            }
            // Everything else delegates to the namespace
            _ => self.namespace.read(path),
        }
    }

    /// Handle a write call from the agent module.
    pub fn handle_write(&mut self, path: &Path, data: Record) -> Result<Path, Error> {
        let components: Vec<&String> = path.iter().collect();

        match components.as_slice() {
            // Intercept: LLM completion request
            [g, c] if g.as_str() == "gate" && c.as_str() == "complete" => {
                let value = data
                    .as_value()
                    .ok_or_else(|| Error::store("HostStore", "write", "expected parsed record"))?;
                let json = structfs_serde_store::value_to_json(value.clone());
                let request: CompletionRequest = serde_json::from_value(json)
                    .map_err(|e| Error::store("HostStore", "write", e.to_string()))?;

                let (events, input_tokens, output_tokens) = self
                    .effects
                    .complete(&request)
                    .map_err(|e| Error::store("HostStore", "write", e))?;

                self.effects.emit_event(AgentEvent::TurnStart);
                if input_tokens > 0 || output_tokens > 0 {
                    // Usage tracked via a separate event — host handles this
                }

                self.pending_events = Some(events);
                Path::parse("gate/response").map_err(Error::from)
            }

            // Intercept: tool execution
            [t, e] if t.as_str() == "tools" && e.as_str() == "execute" => {
                let value = data
                    .as_value()
                    .ok_or_else(|| Error::store("HostStore", "write", "expected parsed record"))?;
                let json = structfs_serde_store::value_to_json(value.clone());
                let call: ToolCall = serde_json::from_value(json)
                    .map_err(|e| Error::store("HostStore", "write", e.to_string()))?;

                self.effects
                    .emit_event(AgentEvent::ToolCallStart { name: call.name.clone() });

                let result_str = self
                    .effects
                    .execute_tool(&call)
                    .unwrap_or_else(|e| format!("error: {e}"));

                self.effects.emit_event(AgentEvent::ToolCallResult {
                    name: call.name.clone(),
                    result: result_str.clone(),
                });

                // Return the result as a Value the agent can read
                let result_value = structfs_serde_store::json_to_value(serde_json::json!({
                    "tool_use_id": call.id,
                    "content": result_str,
                }));
                // Write result into namespace at a temp path for the agent to read
                let result_path = Path::parse(&format!("tool_results/{}", call.id))
                    .map_err(Error::from)?;
                self.namespace.write(&result_path, Record::parsed(result_value))?;
                Ok(result_path)
            }

            // Intercept: event emission
            [ev, em] if ev.as_str() == "events" && em.as_str() == "emit" => {
                let value = data
                    .as_value()
                    .ok_or_else(|| Error::store("HostStore", "write", "expected parsed record"))?;
                let json = structfs_serde_store::value_to_json(value.clone());
                if let Ok(event) = serde_json::from_value::<AgentEvent>(json) {
                    self.effects.emit_event(event);
                }
                Path::parse("events/emit").map_err(Error::from)
            }

            // Everything else delegates to the namespace
            _ => self.namespace.write(path, data),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ox_context::{ModelProvider, Namespace, SystemProvider};
    use ox_history::HistoryProvider;

    struct MockEffects {
        complete_calls: Vec<CompletionRequest>,
        tool_calls: Vec<ToolCall>,
        events: Vec<AgentEvent>,
    }

    impl MockEffects {
        fn new() -> Self {
            Self {
                complete_calls: vec![],
                tool_calls: vec![],
                events: vec![],
            }
        }
    }

    impl HostEffects for MockEffects {
        fn complete(
            &mut self,
            request: &CompletionRequest,
        ) -> Result<(Vec<StreamEvent>, u32, u32), String> {
            self.complete_calls.push(request.clone());
            Ok((
                vec![
                    StreamEvent::TextDelta("Hello!".to_string()),
                    StreamEvent::MessageStop,
                ],
                10,
                5,
            ))
        }

        fn execute_tool(&mut self, call: &ToolCall) -> Result<String, String> {
            self.tool_calls.push(call.clone());
            Ok(format!("result for {}", call.name))
        }

        fn emit_event(&mut self, event: AgentEvent) {
            self.events.push(event);
        }
    }

    fn test_namespace() -> Namespace {
        let mut ns = Namespace::new();
        ns.mount("system", Box::new(SystemProvider::new("You are a test agent.".to_string())));
        ns.mount("history", Box::new(HistoryProvider::new()));
        ns.mount("model", Box::new(ModelProvider::new("test-model".to_string(), 1024)));
        ns
    }

    #[test]
    fn read_delegates_to_namespace() {
        let ns = test_namespace();
        let effects = MockEffects::new();
        let mut store = HostStore::new(ns, effects);

        // Reading history/count should delegate to namespace
        let result = store.handle_read(&path!("history/count")).unwrap();
        assert!(result.is_some());
    }

    #[test]
    fn write_to_gate_complete_intercepts() {
        let ns = test_namespace();
        let effects = MockEffects::new();
        let mut store = HostStore::new(ns, effects);

        // Write a mock completion request
        let request = serde_json::json!({
            "model": "test-model",
            "max_tokens": 1024,
            "system": "test",
            "messages": [],
            "tools": [],
            "stream": true
        });
        let record = Record::parsed(structfs_serde_store::json_to_value(request));
        let path = store.handle_write(&path!("gate/complete"), record).unwrap();
        assert_eq!(path.to_string(), "gate/response");

        // Events should be pending
        let response = store.handle_read(&path!("gate/response")).unwrap();
        assert!(response.is_some());

        // Effect was called
        assert_eq!(store.effects.complete_calls.len(), 1);
        assert_eq!(store.effects.events.len(), 1); // TurnStart
    }

    #[test]
    fn write_to_tools_execute_intercepts() {
        let ns = test_namespace();
        let effects = MockEffects::new();
        let mut store = HostStore::new(ns, effects);

        let tool_call = serde_json::json!({
            "id": "tc_1",
            "name": "read_file",
            "input": {"path": "src/main.rs"}
        });
        let record = Record::parsed(structfs_serde_store::json_to_value(tool_call));
        let result_path = store.handle_write(&path!("tools/execute"), record).unwrap();

        // Should have written result to tool_results/tc_1
        assert!(result_path.to_string().starts_with("tool_results/"));

        // Effect was called
        assert_eq!(store.effects.tool_calls.len(), 1);
        assert_eq!(store.effects.tool_calls[0].name, "read_file");
        // Events: ToolCallStart + ToolCallResult
        assert_eq!(store.effects.events.len(), 2);
    }
}
```

- [ ] **Step 2: Add module to `lib.rs`**

```rust
pub mod bridge;
pub mod host_store;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p ox-runtime`
Expected: Bridge tests + host_store tests pass

- [ ] **Step 4: Commit**

```bash
git add crates/ox-runtime/
git commit -m "feat(ox-runtime): HostStore middleware with effect interception"
```

---

### Task 3: Wasmtime Engine + Component Loader

**Files:**
- Create: `crates/ox-runtime/src/engine.rs`
- Modify: `crates/ox-runtime/src/lib.rs`

- [ ] **Step 1: Create `crates/ox-runtime/src/engine.rs`**

```rust
use crate::bridge;
use crate::host_store::{HostEffects, HostStore};
use std::path::Path as FsPath;
use wasmtime::component::{Component, Linker, Val};
use wasmtime::{Config, Engine, Store};

/// State held by the Wasmtime Store for each agent instance.
pub struct AgentState<E: HostEffects> {
    pub host_store: HostStore<E>,
}

/// An agent runtime backed by Wasmtime.
pub struct AgentRuntime {
    engine: Engine,
}

impl AgentRuntime {
    /// Create a new runtime with default Wasmtime configuration.
    pub fn new() -> Result<Self, String> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        let engine = Engine::new(&config).map_err(|e| e.to_string())?;
        Ok(Self { engine })
    }

    /// Pre-compile a component from a Wasm file on disk.
    pub fn load_component(&self, path: &FsPath) -> Result<AgentComponent, String> {
        let component =
            Component::from_file(&self.engine, path).map_err(|e| e.to_string())?;
        Ok(AgentComponent {
            engine: self.engine.clone(),
            component,
        })
    }

    /// Pre-compile a component from in-memory bytes.
    pub fn load_component_bytes(&self, bytes: &[u8]) -> Result<AgentComponent, String> {
        let component =
            Component::from_binary(&self.engine, bytes).map_err(|e| e.to_string())?;
        Ok(AgentComponent {
            engine: self.engine.clone(),
            component,
        })
    }
}

/// A pre-compiled agent component, ready to be instantiated.
pub struct AgentComponent {
    engine: Engine,
    component: Component,
}

impl AgentComponent {
    /// Instantiate and run the agent with the given host store.
    /// This blocks until the agent's run() function returns.
    pub fn run<E: HostEffects + 'static>(
        &self,
        host_store: HostStore<E>,
    ) -> Result<(), String> {
        let mut linker: Linker<AgentState<E>> = Linker::new(&self.engine);

        // Register host functions matching the WIT interface
        let mut root = linker.root();
        let mut store_iface = root
            .instance("ox:agent/store")
            .map_err(|e| e.to_string())?;

        // read: func(path: string) -> result<option<string>, string>
        store_iface
            .func_wrap(
                "read",
                |mut ctx: wasmtime::StoreContextMut<'_, AgentState<E>>,
                 (path_str,): (String,)|
                 -> wasmtime::Result<(Result<Option<String>, String>,)> {
                    let path = bridge::string_to_path(&path_str)
                        .map_err(|e| wasmtime::Error::msg(e))?;
                    let result = ctx
                        .data_mut()
                        .host_store
                        .handle_read(&path)
                        .map_err(|e| e.to_string());
                    match result {
                        Ok(opt_record) => {
                            let json_opt = bridge::read_result_to_json(opt_record)
                                .map_err(|e| wasmtime::Error::msg(e))?;
                            Ok((Ok(json_opt),))
                        }
                        Err(e) => Ok((Err(e),)),
                    }
                },
            )
            .map_err(|e| e.to_string())?;

        // write: func(path: string, data: string) -> result<string, string>
        store_iface
            .func_wrap(
                "write",
                |mut ctx: wasmtime::StoreContextMut<'_, AgentState<E>>,
                 (path_str, data_json): (String, String)|
                 -> wasmtime::Result<(Result<String, String>,)> {
                    let path = bridge::string_to_path(&path_str)
                        .map_err(|e| wasmtime::Error::msg(e))?;
                    let record = bridge::json_to_record(&data_json)
                        .map_err(|e| wasmtime::Error::msg(e))?;
                    let result = ctx
                        .data_mut()
                        .host_store
                        .handle_write(&path, record)
                        .map_err(|e| e.to_string());
                    match result {
                        Ok(canonical_path) => Ok((Ok(bridge::path_to_string(&canonical_path)),)),
                        Err(e) => Ok((Err(e),)),
                    }
                },
            )
            .map_err(|e| e.to_string())?;

        let mut store = Store::new(&self.engine, AgentState { host_store });

        let instance = linker
            .instantiate(&mut store, &self.component)
            .map_err(|e| e.to_string())?;

        // Call the exported run() function
        let run_func = instance
            .get_func(&mut store, "run")
            .ok_or_else(|| "agent component does not export 'run'".to_string())?;

        let mut results = vec![Val::Bool(false)]; // placeholder for result
        run_func
            .call(&mut store, &[], &mut results)
            .map_err(|e| e.to_string())?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_creates_engine() {
        let runtime = AgentRuntime::new().unwrap();
        // Engine should be created with component model enabled
        assert!(runtime.engine.config().wasm_component_model);
    }
}
```

**Note:** The exact Wasmtime component model API for function wrapping may differ across versions. The implementing engineer should consult the `wasmtime::component` docs for the installed version and adjust the `func_wrap` signatures, particularly around how `result<T, E>` types are represented (they may need `(Result<T, E>,)` tuple wrapping or direct returns depending on the version). The pattern above follows wasmtime 29's component model calling convention.

- [ ] **Step 2: Add module and re-exports to `lib.rs`**

```rust
pub mod bridge;
pub mod engine;
pub mod host_store;

pub use engine::{AgentComponent, AgentRuntime, AgentState};
pub use host_store::{HostEffects, HostStore};
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p ox-runtime`
Expected: All tests pass (bridge + host_store + engine)

- [ ] **Step 4: Commit**

```bash
git add crates/ox-runtime/
git commit -m "feat(ox-runtime): Wasmtime engine + component loader"
```

---

### Task 4: ox-wasi — Agent Component (Guest Side)

**Files:**
- Modify: `crates/ox-wasi/Cargo.toml`
- Rewrite: `crates/ox-wasi/src/lib.rs`

This is the guest-side code that runs inside the Wasm component. It implements a `HostBridge` that wraps the imported host functions as a StructFS Store, then drives the kernel loop.

- [ ] **Step 1: Update `crates/ox-wasi/Cargo.toml`**

```toml
[package]
name = "ox-wasi"
version = "0.1.0"
edition = "2024"
rust-version = "1.85"
license = "Apache-2.0"
description = "Ox agent as a WASI component — drives the kernel loop via StructFS host imports"

[dependencies]
ox-kernel = { path = "../ox-kernel" }
structfs-core-store = { workspace = true }
structfs-serde-store = { workspace = true }
serde_json = "1"
wit-bindgen = "0.36"

[lib]
crate-type = ["cdylib"]
```

- [ ] **Step 2: Rewrite `crates/ox-wasi/src/lib.rs`**

```rust
// Generate bindings from the WIT definition
wit_bindgen::generate!({
    world: "agent",
    path: "../../wit/agent.wit",
});

use ox_kernel::{
    path, AgentEvent, ContentBlock, Kernel, StreamEvent, ToolCall, ToolResult,
};
use structfs_core_store::{Error, Path, Reader, Record, Value, Writer};

/// A StructFS Store implementation that delegates to the host's imported functions.
struct HostBridge;

impl Reader for HostBridge {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, Error> {
        let path_str = from.to_string();
        match ox::agent::store::read(&path_str) {
            Ok(Some(json)) => {
                let json_value: serde_json::Value = serde_json::from_str(&json)
                    .map_err(|e| Error::store("HostBridge", "read", e.to_string()))?;
                let value = structfs_serde_store::json_to_value(json_value);
                Ok(Some(Record::parsed(value)))
            }
            Ok(None) => Ok(None),
            Err(e) => Err(Error::store("HostBridge", "read", e)),
        }
    }
}

impl Writer for HostBridge {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, Error> {
        let path_str = to.to_string();
        let value = data
            .as_value()
            .ok_or_else(|| Error::store("HostBridge", "write", "expected parsed record"))?;
        let json = structfs_serde_store::value_to_json(value.clone());
        let json_str = serde_json::to_string(&json)
            .map_err(|e| Error::store("HostBridge", "write", e.to_string()))?;

        match ox::agent::store::write(&path_str, &json_str) {
            Ok(canonical) => {
                Path::parse(&canonical).map_err(|e| Error::store("HostBridge", "write", e.to_string()))
            }
            Err(e) => Err(Error::store("HostBridge", "write", e)),
        }
    }
}

/// The exported agent entry point.
struct Agent;

impl Guest for Agent {
    fn run() -> Result<(), String> {
        let mut bridge = HostBridge;

        // Read model from namespace
        let model = match bridge.read(&path!("model/id")) {
            Ok(Some(record)) => {
                if let Some(Value::String(m)) = record.as_value() {
                    m.clone()
                } else {
                    "unknown".to_string()
                }
            }
            _ => "unknown".to_string(),
        };

        let mut kernel = Kernel::new(model);

        loop {
            // Phase 1: Kernel prepares completion request from namespace
            let request = kernel.initiate_completion(&mut bridge)?;

            // Phase 2: Write request to host for HTTP fetch
            let request_json = serde_json::to_value(&request).map_err(|e| e.to_string())?;
            let request_value = structfs_serde_store::json_to_value(request_json);
            bridge
                .write(&path!("gate/complete"), Record::parsed(request_value))
                .map_err(|e| e.to_string())?;

            // Read the response events
            let events_record = bridge
                .read(&path!("gate/response"))
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "no completion response".to_string())?;
            let events_value = events_record
                .as_value()
                .ok_or_else(|| "expected parsed response".to_string())?;
            let events_json = structfs_serde_store::value_to_json(events_value.clone());
            let events_str = match &events_json {
                serde_json::Value::String(s) => s.as_str(),
                _ => return Err("expected string response".into()),
            };
            let events: Vec<StreamEvent> =
                serde_json::from_str(events_str).map_err(|e| e.to_string())?;

            // Phase 2b: Kernel accumulates events
            let mut emit = |event: AgentEvent| {
                // Relay non-streaming events to host
                if !matches!(event, AgentEvent::TextDelta(_) | AgentEvent::TurnStart) {
                    if let Ok(json) = serde_json::to_value(&event) {
                        let val = structfs_serde_store::json_to_value(json);
                        let _ = bridge.write(&path!("events/emit"), Record::parsed(val));
                    }
                }
            };
            let content = kernel.consume_events(events, &mut emit)?;

            // Phase 3: Complete turn, extract tool calls
            let tool_calls = kernel.complete_turn(&mut bridge, &content)?;

            if tool_calls.is_empty() {
                return Ok(());
            }

            // Execute tools via host
            let mut results = Vec::new();
            for tc in &tool_calls {
                let tc_json = serde_json::to_value(tc).map_err(|e| e.to_string())?;
                let tc_value = structfs_serde_store::json_to_value(tc_json);
                let result_path = bridge
                    .write(&path!("tools/execute"), Record::parsed(tc_value))
                    .map_err(|e| e.to_string())?;

                // Read the tool result from where the host wrote it
                let result_record = bridge
                    .read(&result_path)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| format!("no result for tool {}", tc.name))?;
                let result_value = result_record.as_value().ok_or("expected parsed result")?;
                let result_json = structfs_serde_store::value_to_json(result_value.clone());
                let content_str = result_json
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("error: no content")
                    .to_string();

                results.push(ToolResult {
                    tool_use_id: tc.id.clone(),
                    content: content_str,
                });
            }

            // Write tool results to history
            let results_json = ox_kernel::serialize_tool_results(&results);
            let results_value = structfs_serde_store::json_to_value(results_json);
            bridge
                .write(&path!("history/append"), Record::parsed(results_value))
                .map_err(|e| e.to_string())?;
        }
    }
}

export!(Agent);
```

- [ ] **Step 3: Verify guest-side compiles to wasm32**

Run: `cargo check --target wasm32-wasip2 -p ox-wasi`

**Note:** If `wasm32-wasip2` target is not installed, run `rustup target add wasm32-wasip2` first. The wit-bindgen generate macro needs the WIT file path relative to the crate root. If the path `../../wit/agent.wit` doesn't resolve during build, adjust the path or use `CARGO_MANIFEST_DIR`-relative pathing.

- [ ] **Step 4: Commit**

```bash
git add crates/ox-wasi/
git commit -m "feat(ox-wasi): agent component — HostBridge + kernel loop over StructFS imports"
```

---

### Task 5: Build Pipeline — Compile Agent to Wasm Component

**Files:**
- Create: `scripts/build-agent.sh`

- [ ] **Step 1: Create build script**

```bash
#!/usr/bin/env bash
set -euo pipefail

# Build the ox-wasi agent as a WASI component
# Requires: cargo-component (install with: cargo install cargo-component)
#           or: wasm-tools (install with: cargo install wasm-tools)

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(dirname "$SCRIPT_DIR")"

echo "Building ox-wasi agent component..."

# Option 1: Using cargo-component (preferred)
if command -v cargo-component &>/dev/null; then
    cd "$ROOT/crates/ox-wasi"
    cargo component build --release
    cp "$ROOT/target/wasm32-wasip2/release/ox_wasi.wasm" "$ROOT/target/agent.wasm"
    echo "Built: target/agent.wasm"
    exit 0
fi

# Option 2: Manual build + wasm-tools
echo "cargo-component not found, using manual build + wasm-tools..."
cargo build --target wasm32-wasip2 --release -p ox-wasi
wasm-tools component new \
    "$ROOT/target/wasm32-wasip2/release/ox_wasi.wasm" \
    --adapt wasi_snapshot_preview1="$ROOT/adapters/wasi_snapshot_preview1.reactor.wasm" \
    -o "$ROOT/target/agent.wasm"
echo "Built: target/agent.wasm"
```

- [ ] **Step 2: Make executable**

Run: `chmod +x scripts/build-agent.sh`

- [ ] **Step 3: Install prerequisites and build**

Run: `cargo install cargo-component` (if not already installed)
Run: `rustup target add wasm32-wasip2`
Run: `./scripts/build-agent.sh`
Expected: `target/agent.wasm` produced

- [ ] **Step 4: Commit**

```bash
git add scripts/build-agent.sh
git commit -m "feat: build script for agent Wasm component"
```

---

### Task 6: Integration Test — Load and Run Agent Component

**Files:**
- Modify: `crates/ox-runtime/src/engine.rs` (add integration test)

- [ ] **Step 1: Write integration test**

Add to `crates/ox-runtime/src/engine.rs` tests module:

```rust
#[test]
fn load_and_run_agent_component() {
    use crate::host_store::{HostEffects, HostStore};
    use ox_context::{ModelProvider, Namespace, SystemProvider, ToolsProvider};
    use ox_history::HistoryProvider;
    use ox_kernel::{AgentEvent, CompletionRequest, StreamEvent, ToolCall};
    use std::path::Path as FsPath;

    struct TestEffects {
        events: Vec<AgentEvent>,
    }
    impl HostEffects for TestEffects {
        fn complete(
            &mut self,
            _request: &CompletionRequest,
        ) -> Result<(Vec<StreamEvent>, u32, u32), String> {
            // Return a simple text response with no tool calls
            Ok((
                vec![
                    StreamEvent::TextDelta("Hello from the agent!".to_string()),
                    StreamEvent::MessageStop,
                ],
                10,
                5,
            ))
        }
        fn execute_tool(&mut self, _call: &ToolCall) -> Result<String, String> {
            Ok("tool result".to_string())
        }
        fn emit_event(&mut self, event: AgentEvent) {
            self.events.push(event);
        }
    }

    let agent_wasm = FsPath::new("../../target/agent.wasm");
    if !agent_wasm.exists() {
        eprintln!("Skipping integration test: target/agent.wasm not found. Run scripts/build-agent.sh first.");
        return;
    }

    let runtime = AgentRuntime::new().unwrap();
    let component = runtime.load_component(agent_wasm).unwrap();

    let mut ns = Namespace::new();
    ns.mount("system", Box::new(SystemProvider::new("You are a test agent.".to_string())));
    ns.mount("history", Box::new(HistoryProvider::new()));
    ns.mount("tools", Box::new(ToolsProvider::new(vec![])));
    ns.mount("model", Box::new(ModelProvider::new("test-model".to_string(), 1024)));

    // Write initial user message to history
    let user_msg = structfs_serde_store::json_to_value(serde_json::json!({
        "role": "user",
        "content": "Hello!"
    }));
    ns.write(
        &ox_kernel::path!("history/append"),
        structfs_core_store::Record::parsed(user_msg),
    )
    .unwrap();

    let effects = TestEffects { events: vec![] };
    let host_store = HostStore::new(ns, effects);

    let result = component.run(host_store);
    assert!(result.is_ok(), "Agent run failed: {:?}", result.err());
}
```

- [ ] **Step 2: Build agent component first, then run test**

Run: `./scripts/build-agent.sh && cargo test -p ox-runtime load_and_run_agent_component`
Expected: Test passes — the agent receives "Hello!", sends a completion request via HostStore, gets back "Hello from the agent!", and exits cleanly.

- [ ] **Step 3: Commit**

```bash
git add crates/ox-runtime/
git commit -m "test(ox-runtime): integration test — load and run agent Wasm component"
```

---

### Summary

| Task | What it builds | Tests |
|------|---------------|-------|
| 1 | WIT definition, bridge serialization, crate scaffold | Record/Path round-trips |
| 2 | HostStore middleware with effect interception | Read delegation, gate/complete intercept, tools/execute intercept |
| 3 | Wasmtime engine, component loader, instance runner | Engine creation, (integration in Task 6) |
| 4 | ox-wasi agent component — HostBridge + kernel loop | Compile check to wasm32 |
| 5 | Build script for Wasm component | Build produces target/agent.wasm |
| 6 | Integration test — full round-trip | Load component, mock effects, verify agent completes |

**Key design decisions:**
- JSON strings across the Wasm boundary — simple, works, StructFS already has json_to_value/value_to_json
- HostEffects trait — ox-cli implements this to wire in HTTP transport, tool execution, policy, and TUI channels
- HostStore intercepts 3 paths: `gate/complete`, `tools/execute`, `events/emit` — everything else passes through to Namespace
- ox-wasi is the same kernel loop as ox-cli's `run_streaming_loop`, just driving through StructFS instead of direct function calls

**Next plan needed:**
- **Plan 2: ox-cli inbox TUI** — inbox view, tab management, compose, filter, search. Will consume both ox-inbox (data) and ox-runtime (execution).
