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
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Store, Value, Writer};

use crate::codec_block;

/// Namespace backing for one broker-Block run: serves `block/config` and
/// `sys/time/*` locally, bridges everything else to the substrate.
struct BrokerBlockBacking {
    config: Value,
    client: ClientHandle,
    runtime: tokio::runtime::Handle,
}

impl BrokerBlockBacking {
    fn now_unix_ms() -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64
    }
}

impl Reader for BrokerBlockBacking {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        let key = from.to_string();
        if key == "block/config" {
            return Ok(Some(Record::parsed(self.config.clone())));
        }
        // Isotope-conventional system path: the guest has no clock.
        if key == "sys/time/now_unix_ms" {
            return Ok(Some(Record::parsed(Value::Integer(Self::now_unix_ms()))));
        }
        let r = self.runtime.block_on(self.client.read(from));
        if std::env::var("OX_BROKER_BLOCK_TRACE").is_ok() {
            eprintln!("BB READ {key} -> {:?}", r.as_ref().map(|o| o.is_some()));
        }
        r
    }
}

impl Writer for BrokerBlockBacking {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        let r = self.runtime.block_on(self.client.write(to, data));
        if std::env::var("OX_BROKER_BLOCK_TRACE").is_ok() {
            eprintln!("BB WRITE {to} -> {r:?}");
        }
        r
    }
}

/// Run the broker Block for one inflight request. Blocking; call from the
/// blocking pool.
pub fn run_broker(
    inflight_path: String,
    traffic: bool,
    client: ClientHandle,
    runtime: tokio::runtime::Handle,
) -> Result<(), String> {
    let config = structfs_serde_store::json_to_value(serde_json::json!({
        "inflight": inflight_path,
        "traffic": traffic,
    }));
    let backing = BrokerBlockBacking { config, client, runtime };
    let host_store = ox_runtime::HostStore::new(backing, NoEffects { tools: EmptyToolStore });

    let module = codec_block::module()?;
    let (_store, outcome) = module.run(host_store);
    outcome
}

/// Run the wire Block for one HTTP exchange. Blocking; call from the
/// blocking pool. The same backing as the broker Block, with the config
/// served at block/wire_config instead.
pub fn run_wire(
    wire_path: String,
    dialect: String,
    client: ClientHandle,
    runtime: tokio::runtime::Handle,
) -> Result<(), String> {
    let config = structfs_serde_store::json_to_value(serde_json::json!({
        "wire": wire_path,
        "dialect": dialect,
    }));
    let backing = WireBlockBacking {
        config,
        client,
        runtime,
    };
    let host_store = ox_runtime::HostStore::new(backing, NoEffects { tools: EmptyToolStore });
    let module = codec_block::module()?;
    let (_store, outcome) = module.run(host_store);
    outcome
}

/// Same bridge as BrokerBlockBacking, serving block/wire_config.
struct WireBlockBacking {
    config: Value,
    client: ClientHandle,
    runtime: tokio::runtime::Handle,
}

impl Reader for WireBlockBacking {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        let key = from.to_string();
        if key == "block/wire_config" {
            return Ok(Some(Record::parsed(self.config.clone())));
        }
        if key == "sys/time/now_unix_ms" {
            return Ok(Some(Record::parsed(Value::Integer(
                BrokerBlockBacking::now_unix_ms(),
            ))));
        }
        self.runtime.block_on(self.client.read(from))
    }
}

impl Writer for WireBlockBacking {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        self.runtime.block_on(self.client.write(to, data))
    }
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
