//! Wasmtime engine, module loader, and instantiation.
//!
//! Provides [`AgentRuntime`] (a configured Wasmtime engine) and
//! [`AgentModule`] (a pre-compiled module ready to run).
//!
//! The module is expected to import three functions from the `"ox"` module
//! (`store_read`, `store_write`, `store_result`) and export a `run() -> i32`
//! function.

use std::path::Path as FilePath;
use std::time::Duration;

use wasmtime::{
    Caller, Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder, UpdateDeadline,
};

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
    limits: StoreLimits,
}

// ---------------------------------------------------------------------------
// AgentRuntime — a configured Wasmtime engine
// ---------------------------------------------------------------------------

/// A Wasmtime `Engine` configured for core Wasm modules.
///
/// Create once, reuse for loading multiple modules.
pub struct AgentRuntime {
    engine: Engine,
    config: AgentRuntimeConfig,
    epoch_ticker: Option<std::sync::Arc<EpochTicker>>,
}

struct EpochTicker {
    stop: Option<std::sync::mpsc::SyncSender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl EpochTicker {
    fn start(engine: Engine, interval: Duration) -> std::sync::Arc<Self> {
        let (stop, stop_rx) = std::sync::mpsc::sync_channel(1);
        let thread = std::thread::spawn(move || {
            loop {
                match stop_rx.recv_timeout(interval) {
                    Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => engine.increment_epoch(),
                }
            }
        });
        std::sync::Arc::new(Self {
            stop: Some(stop),
            thread: Some(thread),
        })
    }
}

impl Drop for EpochTicker {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Per-turn Wasmtime resource controls. `Default` deliberately preserves the
/// local CLI's prior unlimited behavior.
#[derive(Clone, Debug, Default)]
pub struct AgentRuntimeConfig {
    pub max_memory_bytes: Option<usize>,
    pub fuel_per_turn: Option<u64>,
    pub turn_timeout: Option<Duration>,
    pub epoch_poll_interval: Option<Duration>,
}

impl AgentRuntimeConfig {
    pub fn remote() -> Self {
        Self {
            max_memory_bytes: Some(512 * 1024 * 1024),
            fuel_per_turn: Some(10_000_000_000),
            turn_timeout: Some(Duration::from_secs(15 * 60)),
            epoch_poll_interval: Some(Duration::from_millis(10)),
        }
    }
}

impl AgentRuntime {
    /// Create a new runtime.
    pub fn new() -> Result<Self, String> {
        Self::with_config(AgentRuntimeConfig::default())
    }

    pub fn with_config(config: AgentRuntimeConfig) -> Result<Self, String> {
        if config.max_memory_bytes == Some(0) {
            return Err("max_memory_bytes must be non-zero".to_string());
        }
        if config.fuel_per_turn == Some(0) {
            return Err("fuel_per_turn must be non-zero".to_string());
        }
        if config.turn_timeout.is_some_and(|timeout| timeout.is_zero()) {
            return Err("turn_timeout must be non-zero".to_string());
        }
        if config.turn_timeout.is_some() && config.epoch_poll_interval.is_none() {
            return Err("turn_timeout requires epoch_poll_interval".to_string());
        }
        if config
            .epoch_poll_interval
            .is_some_and(|interval| interval.is_zero())
        {
            return Err("epoch_poll_interval must be non-zero".to_string());
        }
        let mut engine_config = wasmtime::Config::new();
        engine_config.consume_fuel(config.fuel_per_turn.is_some());
        engine_config.epoch_interruption(config.epoch_poll_interval.is_some());
        let engine = Engine::new(&engine_config).map_err(|error| error.to_string())?;
        let epoch_ticker = config
            .epoch_poll_interval
            .map(|interval| EpochTicker::start(engine.clone(), interval));
        Ok(Self {
            engine,
            config,
            epoch_ticker,
        })
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
            config: self.config.clone(),
            epoch_ticker: self.epoch_ticker.clone(),
        })
    }

    /// Load a module from in-memory bytes (WAT or Wasm).
    pub fn load_module_from_bytes(&self, bytes: &[u8]) -> Result<AgentModule, String> {
        let module = Module::new(&self.engine, bytes).map_err(|e| e.to_string())?;
        Ok(AgentModule {
            engine: self.engine.clone(),
            module,
            config: self.config.clone(),
            epoch_ticker: self.epoch_ticker.clone(),
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
    config: AgentRuntimeConfig,
    epoch_ticker: Option<std::sync::Arc<EpochTicker>>,
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
        self.run_with_cancellation(host_store, ox_tools::sandbox::ToolCancellation::default())
    }

    /// Run with a cancellation signal shared with sandboxed tool calls.
    pub fn run_with_cancellation<B: Reader + Writer + Send + 'static, E: HostEffects + 'static>(
        &self,
        host_store: HostStore<B, E>,
        cancellation: ox_tools::sandbox::ToolCancellation,
    ) -> (HostStore<B, E>, Result<(), String>) {
        let mut limits = StoreLimitsBuilder::new();
        if let Some(max_memory_bytes) = self.config.max_memory_bytes {
            limits = limits
                .memory_size(max_memory_bytes)
                .trap_on_grow_failure(true);
        }
        let state = AgentState {
            host_store,
            pending_result: None,
            limits: limits.build(),
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
        store.limiter(|state| &mut state.limits);
        if let Some(fuel) = self.config.fuel_per_turn {
            if let Err(error) = store.set_fuel(fuel) {
                let state = store.into_data();
                return (state.host_store, Err(error.to_string()));
            }
        }

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

        tracing::info!("wasm module starting");
        if self.epoch_ticker.is_some() {
            store.set_epoch_deadline(1);
            let deadline = self
                .config
                .turn_timeout
                .map(|timeout| std::time::Instant::now() + timeout);
            store.epoch_deadline_callback(move |_| {
                if cancellation.is_cancelled() {
                    return Err(wasmtime::Error::msg("wasm execution cancelled"));
                }
                if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                    return Err(wasmtime::Error::msg("wasm execution timed out"));
                }
                Ok(UpdateDeadline::Continue(1))
            });
        }
        let call_result = run_func.call(&mut store, ());
        let state = store.into_data();

        match call_result {
            Ok(0) => {
                tracing::info!(exit_code = 0, "wasm module finished");
                (state.host_store, Ok(()))
            }
            Ok(code) => {
                // Try to read the error message the guest stashed before returning.
                let mut hs = state.host_store;
                let detail = hs
                    .handle_read(&structfs_core_store::path!("tool_results/__error"))
                    .ok()
                    .flatten()
                    .and_then(|r| match r.as_value() {
                        Some(structfs_core_store::Value::String(s)) => Some(s.clone()),
                        _ => None,
                    });
                let msg = match detail {
                    Some(d) => {
                        tracing::info!(exit_code = code, error = %d, "wasm module finished with error");
                        d
                    }
                    None => {
                        let m = format!("guest run() returned error code {code}");
                        tracing::info!(exit_code = code, "wasm module finished with error");
                        m
                    }
                };
                (hs, Err(msg))
            }
            Err(e) => {
                tracing::info!(error = %e, "wasm module finished with trap");
                (state.host_store, Err(format!("{e:#}")))
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Helper functions for guest memory access
// ---------------------------------------------------------------------------

/// Get the guest's exported memory.
fn get_memory<B: Reader + Writer + Send, E: HostEffects>(
    caller: &mut Caller<'_, AgentState<B, E>>,
) -> Option<wasmtime::Memory> {
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
fn set_pending<B: Reader + Writer + Send, E: HostEffects>(
    caller: &mut Caller<'_, AgentState<B, E>>,
    bytes: &[u8],
) {
    caller.data_mut().pending_result = Some(bytes.to_vec());
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_store::{HostEffects, HostStore};
    use ox_context::{Namespace, SystemProvider};
    use ox_gate::GateStore;
    use ox_history::HistoryView;
    use ox_kernel::log::{LogStore, SharedLog};
    use ox_kernel::{AgentEvent, CompletionRequest, StreamEvent};
    use ox_tools::completion::CompletionTransport;
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

    #[test]
    fn runtime_rejects_zero_resource_limits() {
        for config in [
            AgentRuntimeConfig {
                max_memory_bytes: Some(0),
                ..AgentRuntimeConfig::default()
            },
            AgentRuntimeConfig {
                fuel_per_turn: Some(0),
                ..AgentRuntimeConfig::default()
            },
            AgentRuntimeConfig {
                turn_timeout: Some(Duration::ZERO),
                epoch_poll_interval: Some(Duration::from_millis(1)),
                ..AgentRuntimeConfig::default()
            },
        ] {
            assert!(AgentRuntime::with_config(config).is_err());
        }
    }

    fn empty_host() -> HostStore<Namespace, MockEffects> {
        HostStore::new(Namespace::new(), MockEffects::new())
    }

    #[test]
    fn memory_limit_traps_growth() {
        let runtime = AgentRuntime::with_config(AgentRuntimeConfig {
            max_memory_bytes: Some(64 * 1024),
            ..AgentRuntimeConfig::default()
        })
        .unwrap();
        let module = runtime
            .load_module_from_bytes(
                br#"(module
                    (memory 1)
                    (func (export "run") (result i32)
                        i32.const 1
                        memory.grow
                        drop
                        i32.const 0))"#,
            )
            .unwrap();
        let (_, result) = module.run(empty_host());
        assert!(
            matches!(result, Err(ref error) if error.contains("grow")),
            "unexpected result: {result:?}"
        );
    }

    #[test]
    fn fuel_limit_traps_cpu_loop() {
        let runtime = AgentRuntime::with_config(AgentRuntimeConfig {
            fuel_per_turn: Some(10_000),
            ..AgentRuntimeConfig::default()
        })
        .unwrap();
        let module = runtime
            .load_module_from_bytes(
                br#"(module
                    (func (export "run") (result i32)
                        (loop $forever br $forever)
                        i32.const 0))"#,
            )
            .unwrap();
        let (_, result) = module.run(empty_host());
        assert!(result.is_err());
    }

    #[test]
    fn epoch_timeout_interrupts_cpu_loop() {
        let runtime = AgentRuntime::with_config(AgentRuntimeConfig {
            turn_timeout: Some(Duration::from_millis(30)),
            epoch_poll_interval: Some(Duration::from_millis(5)),
            ..AgentRuntimeConfig::default()
        })
        .unwrap();
        let module = runtime
            .load_module_from_bytes(
                br#"(module
                    (func (export "run") (result i32)
                        (loop $forever br $forever)
                        i32.const 0))"#,
            )
            .unwrap();
        let (_, result) = module.run(empty_host());
        assert!(
            matches!(result, Err(ref error) if error.contains("timed out")),
            "unexpected result: {result:?}"
        );
    }

    #[test]
    fn cancelling_one_store_does_not_interrupt_another_on_shared_engine() {
        let runtime = AgentRuntime::with_config(AgentRuntimeConfig {
            epoch_poll_interval: Some(Duration::from_millis(2)),
            ..AgentRuntimeConfig::default()
        })
        .unwrap();
        let infinite = runtime
            .load_module_from_bytes(
                br#"(module
                    (func (export "run") (result i32)
                        (loop $forever br $forever)
                        i32.const 0))"#,
            )
            .unwrap();
        let finite = runtime
            .load_module_from_bytes(
                br#"(module
                    (func (export "run") (result i32)
                        (local $i i64)
                        (loop $work
                            local.get $i
                            i64.const 1
                            i64.add
                            local.tee $i
                            i64.const 100000000
                            i64.lt_u
                            br_if $work)
                        i32.const 0))"#,
            )
            .unwrap();
        let cancel_a = ox_tools::sandbox::ToolCancellation::default();
        let run_cancel = cancel_a.clone();
        let a =
            std::thread::spawn(move || infinite.run_with_cancellation(empty_host(), run_cancel).1);
        let b = std::thread::spawn(move || finite.run(empty_host()).1);
        std::thread::sleep(Duration::from_millis(20));
        cancel_a.cancel();
        assert!(a.join().unwrap().is_err());
        assert!(b.join().unwrap().is_ok(), "unrelated store was interrupted");
    }

    // -- Integration test: load and run the real agent.wasm ---------------------

    struct MockEffects {
        events: Vec<String>,
        tool_store: ox_tools::ToolStore,
    }

    impl MockEffects {
        fn new() -> Self {
            Self {
                events: vec![],
                tool_store: ox_tools::ToolStore::empty(),
            }
        }
    }

    impl HostEffects for MockEffects {
        fn emit_event(&mut self, event: AgentEvent) {
            self.events.push(format!("{:?}", event));
        }

        fn tool_store(&mut self) -> &mut dyn structfs_core_store::Store {
            &mut self.tool_store
        }
    }

    struct MockTransport;

    impl CompletionTransport for MockTransport {
        fn send(
            &self,
            _request: &CompletionRequest,
            _on_event: &dyn Fn(&StreamEvent),
        ) -> Result<ox_tools::completion::CompletionOutput, String> {
            Ok(ox_tools::completion::CompletionOutput {
                events: vec![
                    StreamEvent::TextDelta {
                        text: "Hello from the agent!".to_string(),
                    },
                    StreamEvent::MessageStop,
                ],
                input_tokens: 10,
                output_tokens: 5,
                ..Default::default()
            })
        }
    }

    fn make_tool_store() -> ox_tools::ToolStore {
        use ox_tools::completion::CompletionModule;
        use ox_tools::fs::FsModule;
        use ox_tools::os::OsModule;
        use ox_tools::sandbox::PermissivePolicy;
        use std::sync::Arc;

        let policy = Arc::new(PermissivePolicy);
        let workspace = std::path::PathBuf::from("/tmp/test-workspace");
        let executor = std::path::PathBuf::from("/nonexistent/ox-tool-exec");

        let fs = FsModule::new(workspace.clone(), executor.clone(), policy.clone());
        let os = OsModule::new(workspace, executor, policy);
        let completions =
            CompletionModule::new(GateStore::new()).with_transport(Box::new(MockTransport));

        ox_tools::ToolStore::new(fs, os, completions)
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

        // Set up namespace with all required providers. The gate is wired
        // with a config handle that names the primary completion role +
        // per-account model catalog — the post-O2 replacement for the
        // retired `gate/defaults/{account, model, max_tokens}` writes.
        use ox_store_util::LocalConfig;
        use ox_types::{CompletionRole, ModelInfo, ModelInfoSource};

        let role = CompletionRole {
            account: "anthropic".to_string(),
            model_id: "claude-sonnet-4-20250514".to_string(),
        };
        let catalog = vec![ModelInfo {
            id: "claude-sonnet-4-20250514".to_string(),
            display_name: "Claude Sonnet 4".to_string(),
            max_context_size: None,
            max_output_tokens: Some(4096),
            source: ModelInfoSource::Server,
        }];
        let mut config = LocalConfig::new();
        config.set(
            "gate/completions/primary",
            structfs_serde_store::to_value(&role).unwrap(),
        );
        config.set(
            "gate/accounts/anthropic/models",
            structfs_serde_store::to_value(&catalog).unwrap(),
        );

        let gate = GateStore::new().with_config(Box::new(config));

        let shared_log = SharedLog::new();
        let mut ns = Namespace::new();
        ns.mount(
            "system",
            Box::new(SystemProvider::new("You are a test agent.".into())),
        );
        ns.mount("history", Box::new(HistoryView::new(shared_log.clone())));
        ns.mount("tools", Box::new(ox_tools::ToolStore::empty()));
        ns.mount("gate", Box::new(gate));
        ns.mount("log", Box::new(LogStore::from_shared(shared_log)));

        // Write a user message so prompt synthesis has something to work with.
        let user_msg = serde_json::json!({ "role": "user", "content": "Say hello." });
        let user_value = structfs_serde_store::json_to_value(user_msg);
        ns.write(&path!("history/append"), Record::parsed(user_value))
            .expect("failed to write user message");

        // Load and run with ToolStore attached for tools/* routing.
        let runtime = AgentRuntime::new().expect("runtime creation failed");
        let module = runtime
            .load_module_from_file(&wasm_path)
            .expect("failed to load agent.wasm");

        let mut effects = MockEffects::new();
        effects.tool_store = make_tool_store();
        let host_store = HostStore::new(ns, effects);
        let (_returned_store, result) = module.run(host_store);

        match &result {
            Ok(()) => println!("Integration test PASSED: agent ran successfully."),
            Err(e) => println!("Integration test FAILED: {e}"),
        }
        assert!(result.is_ok(), "agent run failed: {:?}", result.err());
    }
}
