//! Bridge serialization between StructFS types and JSON strings.
//!
//! These functions handle the conversion at the Wasm boundary where all
//! data crosses as JSON strings. Records become JSON strings for transport,
//! and JSON strings become Records for store operations.

use structfs_core_store::{Path, Record};

/// Serialize a Record to a JSON string.
///
/// Only works with `Record::Parsed` — raw records cannot be serialized
/// without a codec.
pub fn record_to_json(record: &Record) -> Result<String, String> {
    let value = record
        .as_value()
        .ok_or_else(|| "cannot serialize raw record".to_string())?;
    let json = structfs_serde_store::value_to_json(value.clone());
    serde_json::to_string(&json).map_err(|e| e.to_string())
}

/// Deserialize a JSON string into a parsed Record.
pub fn json_to_record(json: &str) -> Result<Record, String> {
    let json_value: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let value = structfs_serde_store::json_to_value(json_value);
    Ok(Record::parsed(value))
}

/// Convert a Path to its string representation.
pub fn path_to_string(path: &Path) -> String {
    path.to_string()
}

/// Parse a string into a validated Path.
pub fn string_to_path(s: &str) -> Result<Path, String> {
    Path::parse(s).map_err(|e| e.to_string())
}

/// Convert an optional Record (from a read result) to an optional JSON string.
pub fn read_result_to_json(result: Option<Record>) -> Result<Option<String>, String> {
    match result {
        Some(record) => Ok(Some(record_to_json(&record)?)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use structfs_core_store::Value;

    #[test]
    fn record_round_trip() {
        let mut map = BTreeMap::new();
        map.insert("title".to_string(), Value::String("hello".to_string()));
        map.insert("count".to_string(), Value::Integer(42));
        let record = Record::parsed(Value::Map(map));
        let json = record_to_json(&record).unwrap();
        let restored = json_to_record(&json).unwrap();
        assert_eq!(record.as_value(), restored.as_value());
    }

    #[test]
    fn path_round_trip() {
        let path = Path::parse("threads/t_abc123/messages").unwrap();
        let s = path_to_string(&path);
        assert_eq!(s, "threads/t_abc123/messages");
        let restored = string_to_path(&s).unwrap();
        assert_eq!(path, restored);
    }

    #[test]
    fn read_result_none() {
        let result = read_result_to_json(None).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn read_result_some() {
        let record = Record::parsed(Value::String("hello".to_string()));
        let result = read_result_to_json(Some(record)).unwrap();
        assert!(result.is_some());
        let restored = json_to_record(&result.unwrap()).unwrap();
        assert_eq!(
            restored.as_value(),
            Some(&Value::String("hello".to_string()))
        );
    }
}
