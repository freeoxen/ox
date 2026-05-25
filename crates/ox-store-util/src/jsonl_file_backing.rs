//! Append-only JSON Lines file backing for `StoreBacking`.
//!
//! Each `append` call writes one JSON line. `load` reads the file into a
//! `Value::Array`. A partially-written trailing line is silently skipped
//! so a process crash mid-write doesn't poison subsequent loads.
//!
//! `StoreBacking::save` returns an error: this backing is append-only by
//! design. Callers append via the concrete `JsonlFileBacking::append`.

use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use structfs_core_store::{Error as StoreError, Value};
use structfs_serde_store::{json_to_value, value_to_json};

use crate::StoreBacking;

pub struct JsonlFileBacking {
    path: PathBuf,
}

impl JsonlFileBacking {
    pub fn new(path: impl Into<PathBuf>) -> std::io::Result<Self> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(Self { path })
    }

    /// Append one record. The caller passes a `Value` (typically produced
    /// by `structfs_serde_store::to_value` from a typed record).
    pub fn append(&self, value: &Value) -> Result<(), StoreError> {
        let json = value_to_json(value.clone());
        let mut line = serde_json::to_string(&json)
            .map_err(|e| StoreError::store("jsonl", "append", e.to_string()))?;
        line.push('\n');
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .map_err(|e| StoreError::store("jsonl", "open", e.to_string()))?;
        f.write_all(line.as_bytes())
            .map_err(|e| StoreError::store("jsonl", "write", e.to_string()))?;
        Ok(())
    }
}

impl StoreBacking for JsonlFileBacking {
    fn load(&self) -> Result<Option<Value>, StoreError> {
        let f = match std::fs::File::open(&self.path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Some(Value::Array(vec![])));
            }
            Err(e) => return Err(StoreError::store("jsonl", "open", e.to_string())),
        };
        let reader = BufReader::new(f);
        let mut arr = Vec::new();
        for line in reader.lines() {
            let line = line.map_err(|e| StoreError::store("jsonl", "read", e.to_string()))?;
            if line.trim().is_empty() {
                continue;
            }
            let json: serde_json::Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue, // skip partial/corrupt trailing lines
            };
            arr.push(json_to_value(json));
        }
        Ok(Some(Value::Array(arr)))
    }

    fn save(&self, _value: &Value) -> Result<(), StoreError> {
        Err(StoreError::store(
            "jsonl",
            "save",
            "JsonlFileBacking is append-only; use .append() directly",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::Value;

    #[test]
    fn append_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.jsonl");
        let backing = JsonlFileBacking::new(&path).unwrap();

        backing.append(&Value::String("first".into())).unwrap();
        backing.append(&Value::String("second".into())).unwrap();

        let loaded = backing.load().unwrap().unwrap();
        let arr = match loaded {
            Value::Array(a) => a,
            _ => panic!("expected array"),
        };
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0], Value::String("first".into()));
        assert_eq!(arr[1], Value::String("second".into()));
    }

    #[test]
    fn load_missing_file_returns_empty_array() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nonexistent.jsonl");
        let backing = JsonlFileBacking::new(&path).unwrap();
        let loaded = backing.load().unwrap().unwrap();
        match loaded {
            Value::Array(a) => assert!(a.is_empty()),
            _ => panic!("expected empty array"),
        }
    }

    #[test]
    fn load_skips_corrupt_trailing_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("partial.jsonl");
        {
            let mut f = std::fs::File::create(&path).unwrap();
            writeln!(f, "{}", serde_json::json!({"ok": true})).unwrap();
            write!(f, "{{not-json-and-no-newline").unwrap();
        }
        let backing = JsonlFileBacking::new(&path).unwrap();
        let loaded = backing.load().unwrap().unwrap();
        let arr = match loaded {
            Value::Array(a) => a,
            _ => panic!("expected array"),
        };
        assert_eq!(arr.len(), 1);
    }

    #[test]
    fn save_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("readonly.jsonl");
        let backing = JsonlFileBacking::new(&path).unwrap();
        assert!(backing.save(&Value::Null).is_err());
    }
}
