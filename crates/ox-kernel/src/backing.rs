//! StoreBacking — platform-agnostic persistence abstraction.
//!
//! Stores that want durability accept an optional `Box<dyn StoreBacking>`.
//! On construction, `load()` populates initial state. On writes that change
//! state, `save()` flushes the current snapshot. Stores without a backing
//! are purely in-memory.

use structfs_core_store::{Error as StoreError, Value};

/// Persistence abstraction for StructFS stores.
///
/// Implementations handle the mechanics of durability (files, IndexedDB,
/// REST API, etc.). The store handles caching and the read/write protocol.
pub trait StoreBacking: Send + Sync {
    /// Load the full persisted state. Returns None if no prior state exists.
    fn load(&self) -> Result<Option<Value>, StoreError>;

    /// Persist the full state atomically (overwrite).
    fn save(&self, value: &Value) -> Result<(), StoreError>;
}
