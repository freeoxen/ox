//! Ox agent as a Wasm module — drives the kernel loop via host StructFS imports.
//!
//! This crate compiles to `wasm32-unknown-unknown` and produces a `.wasm` file.
//! The guest exports a single `run()` entry point. All I/O flows through three
//! host-provided StructFS functions (`store_read`, `store_write`, `store_result`)
//! imported from the `"ox"` module namespace.

use structfs_core_store::{Error, Path, Reader, Record, Writer};

mod wasm_subscriber;

// ---------------------------------------------------------------------------
// Host function imports (from the "ox" Wasm import module)
// ---------------------------------------------------------------------------

#[link(wasm_import_module = "ox")]
unsafe extern "C" {
    fn store_read(path_ptr: i32, path_len: i32) -> i32;
    fn store_write(path_ptr: i32, path_len: i32, data_ptr: i32, data_len: i32) -> i32;
    fn store_result(buf_ptr: i32);
}

// ---------------------------------------------------------------------------
// Safe wrappers around host imports
// ---------------------------------------------------------------------------

fn host_read(path: &str) -> Result<Option<String>, String> {
    let n = unsafe { store_read(path.as_ptr() as i32, path.len() as i32) };
    if n > 0 {
        let mut buf = vec![0u8; n as usize];
        unsafe { store_result(buf.as_mut_ptr() as i32) };
        String::from_utf8(buf).map(Some).map_err(|e| e.to_string())
    } else if n == 0 {
        Ok(None)
    } else {
        let mut buf = vec![0u8; (-n) as usize];
        unsafe { store_result(buf.as_mut_ptr() as i32) };
        Err(String::from_utf8(buf).unwrap_or_else(|_| "unknown error".into()))
    }
}

fn host_write(path: &str, data: &str) -> Result<String, String> {
    let n = unsafe {
        store_write(
            path.as_ptr() as i32,
            path.len() as i32,
            data.as_ptr() as i32,
            data.len() as i32,
        )
    };
    if n > 0 {
        let mut buf = vec![0u8; n as usize];
        unsafe { store_result(buf.as_mut_ptr() as i32) };
        String::from_utf8(buf).map_err(|e| e.to_string())
    } else {
        let err_len = if n == 0 { 0 } else { (-n) as usize };
        if err_len > 0 {
            let mut buf = vec![0u8; err_len];
            unsafe { store_result(buf.as_mut_ptr() as i32) };
            Err(String::from_utf8(buf).unwrap_or_else(|_| "unknown error".into()))
        } else {
            Err("write failed with unknown error".into())
        }
    }
}

// ---------------------------------------------------------------------------
// HostBridge — implements StructFS Reader + Writer via host calls
// ---------------------------------------------------------------------------

struct HostBridge;

// Safety: wasm32-unknown-unknown is single-threaded; Send+Sync are trivially satisfied.
unsafe impl Send for HostBridge {}
unsafe impl Sync for HostBridge {}

impl Reader for HostBridge {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, Error> {
        let path_str = from.to_string();
        match host_read(&path_str) {
            Ok(Some(json)) => {
                let json_value: serde_json::Value = serde_json::from_str(&json)
                    .map_err(|e| Error::store("HostBridge", "read", e.to_string()))?;
                Ok(Some(Record::parsed(structfs_serde_store::json_to_value(
                    json_value,
                ))))
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

        match host_write(&path_str, &json_str) {
            Ok(canonical) => Path::parse(&canonical)
                .map_err(|e| Error::store("HostBridge", "write", e.to_string())),
            Err(e) => Err(Error::store("HostBridge", "write", e)),
        }
    }
}

// ---------------------------------------------------------------------------
// Exported entry point
// ---------------------------------------------------------------------------

/// Guest entry point. Returns 0 on success, nonzero on error.
#[unsafe(no_mangle)]
pub extern "C" fn run() -> i32 {
    // Install the wasm subscriber so tracing calls from ox-kernel etc.
    // route through the host bridge.
    let _ = tracing::subscriber::set_global_default(wasm_subscriber::WasmSubscriber);

    match agent_main() {
        Ok(()) => 0,
        Err(e) => {
            // Stash the error message where the host can read it back.
            // Must be valid JSON since host_write parses data as JSON.
            let json = serde_json::to_string(&e).unwrap_or_else(|_| "\"unknown error\"".into());
            let _ = host_write("tool_results/__error", &json);
            1
        }
    }
}

fn agent_main() -> Result<(), String> {
    let mut bridge = HostBridge;

    let mut emit = |event: ox_kernel::AgentEvent| {
        let json = ox_kernel::agent_event_to_json(&event);
        let json_str = serde_json::to_string(&json).unwrap_or_default();
        let _ = host_write("events/emit", &json_str);
    };

    ox_kernel::run_turn(&mut bridge, &mut emit)
}
