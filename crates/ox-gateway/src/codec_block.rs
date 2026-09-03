//! Host side of the gateway's codec Block.
//!
//! Loads the embedded `ox_gateway_wasm` module (compiled by build.rs) and
//! runs codec jobs through it: one job per `run()`, the Block reading
//! `codec/job` and writing `codec/result` against an in-memory store.
//!
//! The module is compiled once (the expensive part) and shared; each job
//! instantiates fresh guest state from it, which is the pooling shape the
//! Isotope spec settled on — single-threaded instances, no cross-request
//! state, reuse at the compilation layer.

use ox_runtime::{AgentModule, AgentRuntime};
use std::collections::BTreeMap;
use std::sync::OnceLock;
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Store, Value, Writer};

/// The embedded codec Block, produced by build.rs from ox-gateway-wasm.
static MODULE_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/codec_block.wasm"));

static MODULE: OnceLock<Result<AgentModule, String>> = OnceLock::new();

pub(crate) fn module() -> Result<&'static AgentModule, String> {
    MODULE
        .get_or_init(|| {
            let runtime = AgentRuntime::new()?;
            runtime.load_module_from_bytes(MODULE_BYTES)
        })
        .as_ref()
        .map_err(|e| e.clone())
}

/// Minimal in-memory store: the job at `codec/job`, the Block's output
/// lands at `codec/result`.
struct JobStore {
    entries: BTreeMap<String, Value>,
}

impl Reader for JobStore {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        Ok(self
            .entries
            .get(&from.to_string())
            .cloned()
            .map(Record::parsed))
    }
}

impl Writer for JobStore {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        let value = data
            .as_value()
            .ok_or_else(|| StoreError::store("codec_block", "write", "expected parsed record"))?;
        self.entries.insert(to.to_string(), value.clone());
        Ok(to.clone())
    }
}

/// The codec Block emits no agent events and dispatches no tools; the
/// effects surface is inert.
struct NoEffects {
    tools: EmptyToolStore,
}

struct EmptyToolStore;

impl Reader for EmptyToolStore {
    fn read(&mut self, _from: &Path) -> Result<Option<Record>, StoreError> {
        Ok(None)
    }
}

impl Writer for EmptyToolStore {
    fn write(&mut self, _to: &Path, _data: Record) -> Result<Path, StoreError> {
        Err(StoreError::store(
            "codec_block",
            "write",
            "codec block has no tools",
        ))
    }
}

impl ox_runtime::HostEffects for NoEffects {
    fn emit_event(&mut self, _event: ox_kernel::AgentEvent) {}
    fn tool_store(&mut self) -> &mut dyn Store {
        &mut self.tools
    }
}

/// Run one codec job through the Block. `job` is the op envelope the guest
/// understands; the return value is whatever it wrote to `codec/result`.
pub fn run_job(job: serde_json::Value) -> Result<serde_json::Value, String> {
    let module = module()?;

    let mut entries = BTreeMap::new();
    entries.insert(
        "codec/job".to_string(),
        structfs_serde_store::json_to_value(job),
    );
    let backing = JobStore { entries };
    let host_store = ox_runtime::HostStore::new(
        backing,
        NoEffects {
            tools: EmptyToolStore,
        },
    );

    let (mut host_store, outcome) = module.run(host_store);
    outcome?;

    let result = host_store
        .handle_read(&Path::parse("codec/result").map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?
        .and_then(|r| r.as_value().cloned())
        .ok_or_else(|| "codec block wrote no result".to_string())?;
    let json = structfs_serde_store::value_to_json(result);
    if let Some(err) = json.get("error").and_then(|e| e.as_str()) {
        return Err(err.to_string());
    }
    Ok(json)
}
