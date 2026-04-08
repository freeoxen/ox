//! JsonFileBacking — file-based StoreBacking implementation.
//!
//! Persists a StructFS `Value` as JSON to a single file on disk.
//! Writes are atomic: the new state is written to a temp file in the same
//! directory and then renamed into place so no partial writes are visible.

use ox_kernel::StoreBacking;
use std::path::PathBuf;
use structfs_core_store::{Error as StoreError, Value};
use structfs_serde_store::{json_to_value, value_to_json};

/// File-based persistence using JSON serialization.
///
/// `load()` returns `None` if the file does not yet exist.
/// `save()` writes atomically via a temp file + rename.
pub struct JsonFileBacking {
    path: PathBuf,
}

impl JsonFileBacking {
    /// Create a new `JsonFileBacking` targeting `path`.
    ///
    /// The file (and its parent directories) need not exist yet — they are
    /// created on the first `save()`.
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl StoreBacking for JsonFileBacking {
    fn load(&self) -> Result<Option<Value>, StoreError> {
        if !self.path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(&self.path)
            .map_err(|e| StoreError::store("JsonFileBacking", "load", e.to_string()))?;
        let json: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|e| StoreError::store("JsonFileBacking", "load", e.to_string()))?;
        let value = json_to_value(json);
        Ok(Some(value))
    }

    fn save(&self, value: &Value) -> Result<(), StoreError> {
        let json = value_to_json(value.clone());
        let bytes = serde_json::to_vec_pretty(&json)
            .map_err(|e| StoreError::store("JsonFileBacking", "save", e.to_string()))?;

        // Ensure parent directories exist.
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| StoreError::store("JsonFileBacking", "save", e.to_string()))?;
        }

        // Write to a sibling temp file then rename for atomicity.
        let tmp_path = self.path.with_extension("tmp");
        std::fs::write(&tmp_path, &bytes)
            .map_err(|e| StoreError::store("JsonFileBacking", "save", e.to_string()))?;
        std::fs::rename(&tmp_path, &self.path)
            .map_err(|e| StoreError::store("JsonFileBacking", "save", e.to_string()))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn make_test_value() -> Value {
        let mut map = BTreeMap::new();
        map.insert("key".to_string(), Value::String("value".to_string()));
        map.insert("num".to_string(), Value::Integer(42));
        Value::Map(map)
    }

    #[test]
    fn load_nonexistent_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let backing = JsonFileBacking::new(dir.path().join("nonexistent.json"));
        let result = backing.load().unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn save_then_load_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let backing = JsonFileBacking::new(dir.path().join("state.json"));

        let original = make_test_value();
        backing.save(&original).unwrap();

        let loaded = backing.load().unwrap().expect("should have loaded a value");
        assert_eq!(loaded, original);
    }

    #[test]
    fn save_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let deep_path = dir.path().join("a").join("b").join("c").join("state.json");
        let backing = JsonFileBacking::new(deep_path.clone());

        let value = Value::String("hello".to_string());
        backing.save(&value).unwrap();

        assert!(deep_path.exists(), "file should exist after save");
        let loaded = backing.load().unwrap().unwrap();
        assert_eq!(loaded, value);
    }

    #[test]
    fn save_is_atomic_no_partial_writes() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("state.json");
        let backing = JsonFileBacking::new(file_path.clone());

        // First save establishes a known good state.
        let original = make_test_value();
        backing.save(&original).unwrap();

        // Second save with a different value should fully replace the file.
        let updated = Value::String("updated".to_string());
        backing.save(&updated).unwrap();

        // The temp file must not linger after a successful save.
        let tmp_path = file_path.with_extension("tmp");
        assert!(
            !tmp_path.exists(),
            "temp file should be cleaned up after rename"
        );

        // The final file must contain the latest value (not a mix).
        let loaded = backing.load().unwrap().unwrap();
        assert_eq!(loaded, updated);
    }
}
