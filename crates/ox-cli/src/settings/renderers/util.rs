//! Shared helpers for the settings renderers.
//!
//! Renderers must be total over their input Reader: a missing or malformed
//! value at any path is a "skip" and a `tracing::warn!` event, never a panic.
//! These helpers wrap the read + as_value + from_value chain so each renderer
//! can stay focused on view construction.

use serde::de::DeserializeOwned;
use structfs_core_store::{Path, Reader, Value};

/// Read a typed value at `path`. Returns `None` (with a `tracing::warn!`) for
/// any failure: missing path, store error, non-value record, or
/// deserialization mismatch. Renderers must remain total over Reader state,
/// so callers handle `None` with the appropriate empty-state View.
pub(crate) fn read_typed<T: DeserializeOwned>(data: &mut dyn Reader, path: &Path) -> Option<T> {
    let record = match data.read(path) {
        Ok(Some(rec)) => rec,
        Ok(None) => return None,
        Err(e) => {
            tracing::warn!(path = %path, error = %e, "renderer: read failed");
            return None;
        }
    };
    let value = record.as_value()?.clone();
    match structfs_serde_store::from_value::<T>(value) {
        Ok(v) => Some(v),
        Err(e) => {
            tracing::warn!(path = %path, error = %e, "renderer: typed decode failed");
            None
        }
    }
}

/// Enumerate immediate children at `prefix` by reading the convention's
/// children-Map directly. Returns names in lexicographic order (BTreeMap
/// iteration). Returns `Vec::new()` for any non-Map read — missing path,
/// leaf value, error.
pub(crate) fn child_names_under(data: &mut dyn Reader, prefix_str: &str) -> Vec<String> {
    let path = match Path::parse(prefix_str) {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };
    let record = match data.read(&path) {
        Ok(Some(rec)) => rec,
        _ => return Vec::new(),
    };
    let map = match record.as_value() {
        Some(Value::Map(m)) => m,
        _ => return Vec::new(),
    };
    map.keys().cloned().collect()
}

/// Count immediate children under `prefix`. Convenience over
/// `child_names_under`.
pub(crate) fn subtree_count(data: &mut dyn Reader, prefix_str: &str) -> usize {
    child_names_under(data, prefix_str).len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::snapshot::SettingsSnapshot;
    use ox_path::oxpath;

    #[test]
    fn child_names_under_finds_direct_children() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("config", "gate", "accounts", "alpha", "provider"),
            Value::String("anthropic".into()),
        );
        snap.insert(
            &oxpath!("config", "gate", "accounts", "beta", "provider"),
            Value::String("openai".into()),
        );
        snap.insert(
            &oxpath!("config", "gate", "providers", "anthropic", "endpoint"),
            Value::String("https://api.anthropic.com".into()),
        );
        let names = child_names_under(&mut snap, "config/gate/accounts");
        assert_eq!(names, vec!["alpha".to_string(), "beta".to_string()]);
    }

    #[test]
    fn subtree_count_zero_when_empty() {
        let mut snap = SettingsSnapshot::empty();
        assert_eq!(subtree_count(&mut snap, "config/gate/accounts"), 0);
    }

    #[test]
    fn read_typed_returns_none_for_missing() {
        let mut snap = SettingsSnapshot::empty();
        let v: Option<String> = read_typed(&mut snap, &oxpath!("missing"));
        assert!(v.is_none());
    }

    #[test]
    fn read_typed_decodes_simple_string() {
        let mut snap = SettingsSnapshot::empty();
        snap.insert(
            &oxpath!("ui", "global", "mode"),
            Value::String("normal".into()),
        );
        let v: Option<String> = read_typed(&mut snap, &oxpath!("ui", "global", "mode"));
        assert_eq!(v.as_deref(), Some("normal"));
    }
}
