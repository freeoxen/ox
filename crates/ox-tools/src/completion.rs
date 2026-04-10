//! CompletionModule — wraps [`GateStore`] for StructFS-native LLM transport.
//!
//! Preserves all ox-gate infrastructure: providers, accounts, codecs, config
//! handle, catalogs, snapshots, and usage tracking.  Delegates Reader/Writer
//! to the inner GateStore.

use crate::ToolSchemaEntry;
use ox_gate::GateStore;
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};

/// Thin wrapper around [`GateStore`] that exposes it as a StructFS store
/// while adding tool-schema generation hooks for the unified ToolStore.
pub struct CompletionModule {
    gate: GateStore,
}

impl CompletionModule {
    pub fn new(gate: GateStore) -> Self {
        Self { gate }
    }

    /// Read a sub-path from the underlying GateStore.
    ///
    /// `sub` is a `/`-separated path string (e.g. `"defaults/account"`).
    pub fn read_gate(&mut self, sub: &str) -> Option<Value> {
        let path = Path::parse(sub).ok()?;
        let record = self.gate.read(&path).ok()??;
        record.as_value().cloned()
    }

    /// Write a sub-path to the underlying GateStore.
    ///
    /// `sub` is a `/`-separated path string (e.g. `"defaults/model"`).
    pub fn write_gate(&mut self, sub: &str, value: Value) -> Result<(), String> {
        let path = Path::parse(sub).map_err(|e| e.to_string())?;
        self.gate
            .write(&path, Record::Parsed(value))
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Tool schemas for completion accounts with API keys.
    ///
    /// For now returns an empty vec — will be refined when context-based
    /// synthesis lands.
    pub fn schemas(&self) -> Vec<ToolSchemaEntry> {
        vec![]
    }

    /// Mutable access to the inner GateStore.
    pub fn gate_mut(&mut self) -> &mut GateStore {
        &mut self.gate
    }

    /// Shared access to the inner GateStore.
    pub fn gate(&self) -> &GateStore {
        &self.gate
    }
}

/// StructFS Reader delegation to GateStore.
impl Reader for CompletionModule {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        self.gate.read(from)
    }
}

/// StructFS Writer delegation to GateStore.
impl Writer for CompletionModule {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        self.gate.write(to, data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schemas_returns_empty_without_keys() {
        let gate = GateStore::new();
        let module = CompletionModule::new(gate);
        assert!(module.schemas().is_empty());
    }

    #[test]
    fn gate_store_accessible_by_path() {
        let gate = GateStore::new();
        let mut module = CompletionModule::new(gate);
        let result = module.read_gate("defaults/account");
        assert!(result.is_some());
    }

    #[test]
    fn reader_delegates_to_gate() {
        let gate = GateStore::new();
        let mut module = CompletionModule::new(gate);
        // GateStore defaults include "anthropic" as default account
        let result = module.read(&structfs_core_store::path!("defaults/account"));
        assert!(result.is_ok());
    }
}
