//! Host side of the broker Block.
//!
//! Runs the same embedded wasm artifact as the codec Block, in broker mode:
//! the presence of `block/config` in the namespace tells the guest to drive
//! one completion end-to-end. The backing bridges the guest's sync reads
//! and writes onto the async broker — including the blocking
//! `upstream/.../events/from/{s}` drain, which parks this thread exactly
//! the way the Isotope model expects a Block's blocking read to park.
//!
//! Must run on a blocking thread (the store spawns runners via
//! `spawn_blocking`): `Handle::block_on` from an async worker would
//! deadlock the runtime.

use ox_broker::ClientHandle;
use ox_gate::completion_broker::CancelHandle;
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Store, Value, Writer};

use crate::assembly::WiringTable;
use crate::codec_block;

/// Namespace backing for one Block run: serves the Block's config path
/// locally, routes everything else through the assembly wiring table.
/// A path with no wiring entry is refused — the manifest, not this code,
/// decides what a Block can reach.
struct BlockBacking {
    config_path: &'static str,
    config: Value,
    wiring: WiringTable,
    cancel: CancelHandle,
    client: ClientHandle,
    runtime: tokio::runtime::Handle,
}

impl BlockBacking {
    fn now_unix_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64
    }

    fn resolve(&self, path: &Path, op: &'static str) -> Result<Path, StoreError> {
        let key = path.to_string();
        let Some(target) = self.wiring.resolve(&key) else {
            return Err(StoreError::store(
                "block",
                op,
                format!("path not wired in assembly manifest: {key}"),
            ));
        };
        Path::parse(&target).map_err(|e| StoreError::store("block", op, e.to_string()))
    }
}

impl Reader for BlockBacking {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        let key = from.to_string();
        if key == self.config_path {
            return Ok(Some(Record::parsed(self.config.clone())));
        }
        let target = self.resolve(from, "read")?;
        // Isotope-conventional system path: the guest has no clock. Served
        // host-side, but only when the manifest wires /sys.
        if target.to_string() == "sys/time/now_unix_ms" {
            return Ok(Some(Record::parsed(Value::Integer(Self::now_unix_ms()))));
        }
        // Reads can legitimately park for the whole inter-token gap. When
        // the handle this run serves is GC'd, cancellation fails the read
        // so the guest unwinds and GC's its downstream handles — writes
        // stay open so that teardown can land.
        let r = self.runtime.block_on(async {
            tokio::select! {
                r = self.client.read(&target) => r,
                _ = self.cancel.cancelled() => Err(StoreError::store(
                    "block",
                    "read",
                    "cancelled: handle GC'd",
                )),
            }
        });
        if std::env::var("OX_BROKER_BLOCK_TRACE").is_ok() {
            eprintln!("BB READ {key} -> {:?}", r.as_ref().map(|o| o.is_some()));
        }
        r
    }
}

impl Writer for BlockBacking {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        let target = self.resolve(to, "write")?;
        let r = self.runtime.block_on(self.client.write(&target, data));
        if std::env::var("OX_BROKER_BLOCK_TRACE").is_ok() {
            eprintln!("BB WRITE {to} -> {r:?}");
        }
        // Result paths go back in the guest's namespace, not the
        // substrate's — under aliased wiring the two differ.
        r.and_then(|p| {
            let guest = self.wiring.unresolve(&p.to_string());
            Path::parse(&guest).map_err(|e| StoreError::store("block", "write", e.to_string()))
        })
    }
}

/// Run the broker Block for one inflight request. Blocking; call from the
/// blocking pool.
pub fn run_broker(
    inflight_path: String,
    traffic: bool,
    wiring: WiringTable,
    cancel: CancelHandle,
    client: ClientHandle,
    runtime: tokio::runtime::Handle,
) -> Result<(), String> {
    let config = structfs_serde_store::json_to_value(serde_json::json!({
        "inflight": inflight_path,
        "traffic": traffic,
    }));
    let backing = BlockBacking {
        config_path: "block/config",
        config,
        wiring,
        cancel,
        client,
        runtime,
    };
    let host_store = ox_runtime::HostStore::new(backing, NoEffects { tools: EmptyToolStore });

    let module = codec_block::module()?;
    let (_store, outcome) = module.run(host_store);
    outcome
}

/// Run the wire Block for one HTTP exchange. Blocking; call from the
/// blocking pool. Same backing as the broker Block; the config path is
/// what selects wire mode in the shared artifact.
pub fn run_wire(
    wire_path: String,
    dialect: String,
    wiring: WiringTable,
    cancel: CancelHandle,
    client: ClientHandle,
    runtime: tokio::runtime::Handle,
) -> Result<(), String> {
    let config = structfs_serde_store::json_to_value(serde_json::json!({
        "wire": wire_path,
        "dialect": dialect,
    }));
    let backing = BlockBacking {
        config_path: "block/wire_config",
        config,
        wiring,
        cancel,
        client,
        runtime,
    };
    let host_store = ox_runtime::HostStore::new(backing, NoEffects { tools: EmptyToolStore });
    let module = codec_block::module()?;
    let (_store, outcome) = module.run(host_store);
    outcome
}

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
        Err(StoreError::store("broker_block", "write", "broker block has no tools"))
    }
}

impl ox_runtime::HostEffects for NoEffects {
    fn emit_event(&mut self, _event: ox_kernel::AgentEvent) {}
    fn tool_store(&mut self) -> &mut dyn Store {
        &mut self.tools
    }
}
