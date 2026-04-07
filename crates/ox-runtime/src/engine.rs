//! Wasmtime engine, module loader, and instantiation.
//!
//! Provides [`AgentRuntime`] (a configured Wasmtime engine) and
//! [`AgentModule`] (a pre-compiled module ready to run).
//!
//! The module is expected to import three functions from the `"ox"` module
//! (`store_read`, `store_write`, `store_result`) and export a `run() -> i32`
//! function.

use std::path::Path as FilePath;

use wasmtime::{Caller, Engine, Linker, Module, Store};

use structfs_core_store::{Reader, Writer};

use crate::bridge;
use crate::host_store::{HostEffects, HostStore};

// ---------------------------------------------------------------------------
// AgentState — the host state threaded through Wasmtime's Store
// ---------------------------------------------------------------------------

/// Host-side state accessible to imported functions during module execution.
pub struct AgentState<B: Reader + Writer + Send, E: HostEffects> {
    /// The HostStore that mediates all reads/writes for the guest.
    pub host_store: HostStore<B, E>,
    /// The pending result bytes from the last store_read or store_write.
    pending_result: Option<Vec<u8>>,
}

// ---------------------------------------------------------------------------
// AgentRuntime — a configured Wasmtime engine
// ---------------------------------------------------------------------------

/// A Wasmtime `Engine` configured for core Wasm modules.
///
/// Create once, reuse for loading multiple modules.
pub struct AgentRuntime {
    engine: Engine,
}

impl AgentRuntime {
    /// Create a new runtime.
    pub fn new() -> Result<Self, String> {
        let engine = Engine::default();
        Ok(Self { engine })
    }

    /// Access the underlying engine.
    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Load a module from a file path.
    pub fn load_module_from_file(&self, path: impl AsRef<FilePath>) -> Result<AgentModule, String> {
        let module = Module::from_file(&self.engine, path).map_err(|e| e.to_string())?;
        Ok(AgentModule {
            engine: self.engine.clone(),
            module,
        })
    }

    /// Load a module from in-memory bytes (WAT or Wasm).
    pub fn load_module_from_bytes(&self, bytes: &[u8]) -> Result<AgentModule, String> {
        let module = Module::new(&self.engine, bytes).map_err(|e| e.to_string())?;
        Ok(AgentModule {
            engine: self.engine.clone(),
            module,
        })
    }
}

// ---------------------------------------------------------------------------
// AgentModule — a pre-compiled module ready to instantiate and run
// ---------------------------------------------------------------------------

/// A pre-compiled Wasm module conforming to the ox agent ABI.
///
/// Call [`run`](AgentModule::run) to instantiate and execute the agent loop.
#[derive(Clone)]
pub struct AgentModule {
    engine: Engine,
    module: Module,
}

impl AgentModule {
    /// Instantiate the module with the given host store and run its
    /// exported `run` function.
    ///
    /// Returns the `HostStore` back to the caller (so the namespace and
    /// effects survive across calls) along with the result of execution.
    ///
    /// The host's `store_read`, `store_write`, and `store_result` functions
    /// are linked as imports in the `"ox"` module namespace.
    pub fn run<B: Reader + Writer + Send + 'static, E: HostEffects + 'static>(
        &self,
        host_store: HostStore<B, E>,
    ) -> (HostStore<B, E>, Result<(), String>) {
        let state = AgentState {
            host_store,
            pending_result: None,
        };

        // -- Linker: register host imports ------------------------------------
        let mut linker: Linker<AgentState<B, E>> = Linker::new(&self.engine);

        // store_read(path_ptr, path_len) -> i32
        if let Err(e) = linker.func_wrap(
            "ox",
            "store_read",
            |mut caller: Caller<'_, AgentState<B, E>>, path_ptr: i32, path_len: i32| -> i32 {
                let memory = match get_memory(&mut caller) {
                    Some(m) => m,
                    None => {
                        set_pending(&mut caller, b"no exported memory");
                        return -17; // len of "no exported memory"
                    }
                };

                let path_str = match read_guest_string(&caller, &memory, path_ptr, path_len) {
                    Ok(s) => s,
                    Err(msg) => {
                        let len = msg.len() as i32;
                        set_pending(&mut caller, msg.as_bytes());
                        return -len;
                    }
                };

                let path = match bridge::string_to_path(&path_str) {
                    Ok(p) => p,
                    Err(msg) => {
                        let len = msg.len() as i32;
                        set_pending(&mut caller, msg.as_bytes());
                        return -len;
                    }
                };

                let state = caller.data_mut();
                match state.host_store.handle_read(&path) {
                    Ok(Some(record)) => match bridge::record_to_json(&record) {
                        Ok(json) => {
                            let len = json.len() as i32;
                            state.pending_result = Some(json.into_bytes());
                            len
                        }
                        Err(msg) => {
                            let len = msg.len() as i32;
                            state.pending_result = Some(msg.into_bytes());
                            -len
                        }
                    },
                    Ok(None) => {
                        state.pending_result = None;
                        0
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        let len = msg.len() as i32;
                        state.pending_result = Some(msg.into_bytes());
                        -len
                    }
                }
            },
        ) {
            return (state.host_store, Err(e.to_string()));
        }

        // store_write(path_ptr, path_len, data_ptr, data_len) -> i32
        if let Err(e) = linker.func_wrap(
            "ox",
            "store_write",
            |mut caller: Caller<'_, AgentState<B, E>>,
             path_ptr: i32,
             path_len: i32,
             data_ptr: i32,
             data_len: i32|
             -> i32 {
                let memory = match get_memory(&mut caller) {
                    Some(m) => m,
                    None => {
                        set_pending(&mut caller, b"no exported memory");
                        return -17;
                    }
                };

                let path_str = match read_guest_string(&caller, &memory, path_ptr, path_len) {
                    Ok(s) => s,
                    Err(msg) => {
                        let len = msg.len() as i32;
                        set_pending(&mut caller, msg.as_bytes());
                        return -len;
                    }
                };

                let data_str = match read_guest_string(&caller, &memory, data_ptr, data_len) {
                    Ok(s) => s,
                    Err(msg) => {
                        let len = msg.len() as i32;
                        set_pending(&mut caller, msg.as_bytes());
                        return -len;
                    }
                };

                let path = match bridge::string_to_path(&path_str) {
                    Ok(p) => p,
                    Err(msg) => {
                        let len = msg.len() as i32;
                        set_pending(&mut caller, msg.as_bytes());
                        return -len;
                    }
                };

                let record = match bridge::json_to_record(&data_str) {
                    Ok(r) => r,
                    Err(msg) => {
                        let len = msg.len() as i32;
                        set_pending(&mut caller, msg.as_bytes());
                        return -len;
                    }
                };

                let state = caller.data_mut();
                match state.host_store.handle_write(&path, record) {
                    Ok(result_path) => {
                        let canonical = bridge::path_to_string(&result_path);
                        let len = canonical.len() as i32;
                        state.pending_result = Some(canonical.into_bytes());
                        len
                    }
                    Err(e) => {
                        let msg = e.to_string();
                        let len = msg.len() as i32;
                        state.pending_result = Some(msg.into_bytes());
                        -len
                    }
                }
            },
        ) {
            return (state.host_store, Err(e.to_string()));
        }

        // store_result(buf_ptr)
        if let Err(e) = linker.func_wrap(
            "ox",
            "store_result",
            |mut caller: Caller<'_, AgentState<B, E>>, buf_ptr: i32| {
                let pending = caller.data_mut().pending_result.take().unwrap_or_default();
                let memory = match get_memory(&mut caller) {
                    Some(m) => m,
                    None => return,
                };
                let start = buf_ptr as usize;
                let end = start + pending.len();
                if let Some(slice) = memory.data_mut(&mut caller).get_mut(start..end) {
                    slice.copy_from_slice(&pending);
                }
            },
        ) {
            return (state.host_store, Err(e.to_string()));
        }

        // -- Instantiate and call run -----------------------------------------
        let mut store = Store::new(&self.engine, state);

        let instance = match linker.instantiate(&mut store, &self.module) {
            Ok(i) => i,
            Err(e) => {
                let state = store.into_data();
                return (state.host_store, Err(e.to_string()));
            }
        };

        let run_func = match instance.get_typed_func::<(), i32>(&mut store, "run") {
            Ok(f) => f,
            Err(e) => {
                let state = store.into_data();
                return (state.host_store, Err(e.to_string()));
            }
        };

        let call_result = run_func.call(&mut store, ());
        let state = store.into_data();

        match call_result {
            Ok(0) => (state.host_store, Ok(())),
            Ok(code) => (
                state.host_store,
                Err(format!("guest run() returned error code {code}")),
            ),
            Err(e) => (state.host_store, Err(e.to_string())),
        }
    }
}

// ---------------------------------------------------------------------------
// Helper functions for guest memory access
// ---------------------------------------------------------------------------

/// Get the guest's exported memory.
fn get_memory<B: Reader + Writer + Send, E: HostEffects>(caller: &mut Caller<'_, AgentState<B, E>>) -> Option<wasmtime::Memory> {
    caller
        .get_export("memory")
        .and_then(|ext| ext.into_memory())
}

/// Read a UTF-8 string from guest linear memory.
fn read_guest_string<B: Reader + Writer + Send, E: HostEffects>(
    caller: &Caller<'_, AgentState<B, E>>,
    memory: &wasmtime::Memory,
    ptr: i32,
    len: i32,
) -> Result<String, String> {
    let start = ptr as usize;
    let end = start + len as usize;
    let data = memory.data(caller);
    let bytes = data
        .get(start..end)
        .ok_or_else(|| "guest memory access out of bounds".to_string())?;
    std::str::from_utf8(bytes)
        .map(|s| s.to_string())
        .map_err(|e| e.to_string())
}

/// Set the pending result bytes on the agent state.
fn set_pending<B: Reader + Writer + Send, E: HostEffects>(caller: &mut Caller<'_, AgentState<B, E>>, bytes: &[u8]) {
    caller.data_mut().pending_result = Some(bytes.to_vec());
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_store::{HostEffects, HostStore};
    use ox_context::{ModelProvider, Namespace, SystemProvider, ToolsProvider};
    use ox_history::HistoryProvider;
    use ox_kernel::{AgentEvent, CompletionRequest, StreamEvent, ToolCall};
    use structfs_core_store::{Record, Writer, path};

    #[test]
    fn runtime_creates_engine() {
        let runtime = AgentRuntime::new();
        assert!(runtime.is_ok(), "AgentRuntime::new() should succeed");
        let runtime = runtime.unwrap();
        let _engine_ref = runtime.engine();
    }

    #[test]
    fn load_invalid_bytes_fails() {
        let runtime = AgentRuntime::new().unwrap();
        let result = runtime.load_module_from_bytes(b"not valid wasm");
        assert!(result.is_err(), "invalid bytes should fail to load");
    }

    #[test]
    fn load_nonexistent_file_fails() {
        let runtime = AgentRuntime::new().unwrap();
        let result = runtime.load_module_from_file("/tmp/does-not-exist.wasm");
        assert!(result.is_err(), "missing file should fail to load");
    }

    // -- Integration test: load and run the real agent.wasm ---------------------

    struct MockEffects {
        events: Vec<String>,
    }

    impl MockEffects {
        fn new() -> Self {
            Self { events: vec![] }
        }
    }

    impl HostEffects for MockEffects {
        fn complete(
            &mut self,
            _request: &CompletionRequest,
        ) -> Result<(Vec<StreamEvent>, u32, u32), String> {
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
            Ok("mock result".to_string())
        }

        fn emit_event(&mut self, event: AgentEvent) {
            self.events.push(format!("{:?}", event));
        }
    }

    #[test]
    fn load_and_run_agent_wasm() {
        let wasm_path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/agent.wasm");

        if !wasm_path.exists() {
            println!(
                "SKIPPED: agent.wasm not found at {}. Run scripts/build-agent.sh first.",
                wasm_path.display()
            );
            return;
        }

        // Set up namespace with all required providers.
        let mut ns = Namespace::new();
        ns.mount(
            "system",
            Box::new(SystemProvider::new("You are a test agent.".into())),
        );
        ns.mount("history", Box::new(HistoryProvider::new()));
        ns.mount("tools", Box::new(ToolsProvider::new(vec![])));
        ns.mount(
            "model",
            Box::new(ModelProvider::new("test-model".into(), 1024)),
        );

        // Write a user message so prompt synthesis has something to work with.
        let user_msg = serde_json::json!({ "role": "user", "content": "Say hello." });
        let user_value = structfs_serde_store::json_to_value(user_msg);
        ns.write(&path!("history/append"), Record::parsed(user_value))
            .expect("failed to write user message");

        // Load and run.
        let runtime = AgentRuntime::new().expect("runtime creation failed");
        let module = runtime
            .load_module_from_file(&wasm_path)
            .expect("failed to load agent.wasm");

        let host_store = HostStore::new(ns, MockEffects::new());
        let (_returned_store, result) = module.run(host_store);

        match &result {
            Ok(()) => println!("Integration test PASSED: agent ran successfully."),
            Err(e) => println!("Integration test FAILED: {e}"),
        }
        assert!(result.is_ok(), "agent run failed: {:?}", result.err());
    }
}
