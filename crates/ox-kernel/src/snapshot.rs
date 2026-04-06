//! Snapshot lens helpers — hash computation and record building.

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use structfs_core_store::Value;
use structfs_serde_store::value_to_json;

/// Compute the snapshot hash: SHA-256 of the JSON-serialized state, truncated to 16 hex chars.
///
/// StructFS `Value::Map` uses `BTreeMap` (sorted keys), so output is deterministic.
pub fn snapshot_hash(state: &Value) -> String {
    let json = value_to_json(state.clone());
    let json_bytes = serde_json::to_vec(&json).expect("Value always serializes to JSON");
    let digest = Sha256::digest(&json_bytes);
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// Build a snapshot Value: `{"hash": "<16 hex>", "state": <value>}`.
pub fn snapshot_record(state: Value) -> Value {
    let hash = snapshot_hash(&state);
    let mut map = BTreeMap::new();
    map.insert("hash".to_string(), Value::String(hash));
    map.insert("state".to_string(), state);
    Value::Map(map)
}

/// Extract the restorable state from a written snapshot value.
///
/// Accepts either `{"state": <value>, ...}` (full snapshot map) or a bare value
/// (treated as the state directly). The `hash` field, if present, is ignored —
/// the store recomputes it.
pub fn extract_snapshot_state(value: Value) -> Result<Value, String> {
    match value {
        Value::Map(ref m) if m.contains_key("state") => Ok(m.get("state").unwrap().clone()),
        other => Ok(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_of_string_value() {
        let state = Value::String("hello".to_string());
        let hash = snapshot_hash(&state);
        assert_eq!(hash.len(), 16);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn hash_is_deterministic() {
        let state = Value::String("test prompt".to_string());
        let h1 = snapshot_hash(&state);
        let h2 = snapshot_hash(&state);
        assert_eq!(h1, h2);
    }

    #[test]
    fn hash_of_map_is_sorted_key_order() {
        let mut map = BTreeMap::new();
        map.insert("z".to_string(), Value::Integer(1));
        map.insert("a".to_string(), Value::Integer(2));
        let state = Value::Map(map);
        let h1 = snapshot_hash(&state);
        let h2 = snapshot_hash(&state);
        assert_eq!(h1, h2);
    }

    #[test]
    fn snapshot_record_contains_hash_and_state() {
        let state = Value::String("prompt".to_string());
        let record = snapshot_record(state.clone());
        match &record {
            Value::Map(m) => {
                assert!(m.contains_key("hash"));
                assert!(m.contains_key("state"));
                assert_eq!(m.get("state").unwrap(), &state);
                match m.get("hash").unwrap() {
                    Value::String(h) => assert_eq!(h.len(), 16),
                    _ => panic!("hash should be a string"),
                }
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn extract_state_from_full_snapshot() {
        let state = Value::String("data".to_string());
        let snap = snapshot_record(state.clone());
        let extracted = extract_snapshot_state(snap).unwrap();
        assert_eq!(extracted, state);
    }

    #[test]
    fn extract_state_from_state_only() {
        let state = Value::String("data".to_string());
        let extracted = extract_snapshot_state(state.clone()).unwrap();
        assert_eq!(extracted, state);
    }
}
