//! `JsonFileBacking` — persists a flat path-keyed `BTreeMap<String, Value>`
//! as a JSON object with `chmod 0600` on Unix.
//!
//! Used for the secrets file (`keys.json`). JSON over TOML because the
//! representation is simpler — flat key→value, no nested-table reshuffle —
//! and because there is no human-editing story for keys.json the way there
//! is for config.toml.

use std::path::PathBuf;
use structfs_core_store::{Error as StoreError, Value};

/// File-based JSON persistence for a flat `BTreeMap<String, Value>`. On
/// Unix the file is written with `0600` permissions so the on-disk
/// secrets are readable only by the owner.
pub struct JsonFileBacking {
    path: PathBuf,
}

impl JsonFileBacking {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

impl ox_store_util::StoreBacking for JsonFileBacking {
    fn load(&self) -> Result<Option<Value>, StoreError> {
        let content = match std::fs::read_to_string(&self.path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(StoreError::store("json_backing", "load", e.to_string())),
        };
        let json: serde_json::Value = serde_json::from_str(&content)
            .map_err(|e| StoreError::store("json_backing", "load", e.to_string()))?;
        let value = structfs_serde_store::json_to_value(json);
        Ok(Some(value))
    }

    fn save(&self, value: &Value) -> Result<(), StoreError> {
        let Value::Map(_) = value else {
            return Err(StoreError::store(
                "json_backing",
                "save",
                "expected Value::Map",
            ));
        };
        let json = structfs_serde_store::value_to_json(value.clone());
        let content = serde_json::to_string_pretty(&json)
            .map_err(|e| StoreError::store("json_backing", "save", e.to_string()))?;

        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| StoreError::store("json_backing", "save", e.to_string()))?;
        }

        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, content)
            .map_err(|e| StoreError::store("json_backing", "save", e.to_string()))?;
        // Apply 0600 to the temp before rename so the final file never
        // appears with broader perms even briefly.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| StoreError::store("json_backing", "save", e.to_string()))?;
        }
        std::fs::rename(&tmp, &self.path)
            .map_err(|e| StoreError::store("json_backing", "save", e.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ox_store_util::StoreBacking;
    use std::collections::BTreeMap;

    #[test]
    fn roundtrip_save_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.json");
        let backing = JsonFileBacking::new(path.clone());
        assert!(backing.load().unwrap().is_none());

        let mut map = BTreeMap::new();
        map.insert(
            "keys/anthropic".to_string(),
            Value::String("sk-anthropic".into()),
        );
        map.insert("keys/openai".to_string(), Value::String("sk-openai".into()));
        backing.save(&Value::Map(map.clone())).unwrap();
        assert!(path.exists());

        let loaded = backing.load().unwrap().unwrap();
        match loaded {
            Value::Map(m) => {
                assert_eq!(
                    m.get("keys/anthropic").unwrap(),
                    &Value::String("sk-anthropic".into())
                );
                assert_eq!(
                    m.get("keys/openai").unwrap(),
                    &Value::String("sk-openai".into())
                );
            }
            other => panic!("expected map, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn save_applies_0600_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("keys.json");
        let backing = JsonFileBacking::new(path.clone());

        let mut map = BTreeMap::new();
        map.insert("keys/x".to_string(), Value::String("sk-x".into()));
        backing.save(&Value::Map(map)).unwrap();

        let perms = std::fs::metadata(&path).unwrap().permissions();
        assert_eq!(
            perms.mode() & 0o777,
            0o600,
            "keys.json must land with 0600 perms"
        );
    }
}
