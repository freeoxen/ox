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

    /// Append one item to an array-shaped backing.
    ///
    /// The default implementation does a load-extend-save round-trip and
    /// works for any backing whose `save` succeeds. Append-optimised backings
    /// (e.g. `JsonlFileBacking`) override this to write a single line without
    /// rewriting the entire file.
    fn append(&self, item: &Value) -> Result<(), StoreError> {
        let mut arr = match self.load()? {
            Some(Value::Array(a)) => a,
            Some(other) => {
                return Err(StoreError::store(
                    "backing",
                    "append",
                    format!("expected array, got {:?}", other),
                ));
            }
            None => vec![],
        };
        arr.push(item.clone());
        self.save(&Value::Array(arr))
    }
}
