//! Thread directory format — read/write context.json and view.json.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// The context.json file — snapshot of non-history stores + thread metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextFile {
    pub version: u32,
    pub thread_id: String,
    pub title: String,
    pub labels: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
    /// Store snapshots keyed by mount name (e.g. "system", "model", "gate").
    /// Values are the snapshot state for each store (serde_json::Value).
    #[serde(flatten)]
    pub stores: BTreeMap<String, serde_json::Value>,
}

/// A range of sequence numbers to include in the view.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncludeRange {
    pub start: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<u64>,
}

/// The view.json file — projection manifest defining what the agent sees.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ViewFile {
    pub parent: Option<String>,
    pub include: Vec<IncludeRange>,
    pub masks: Vec<u64>,
    pub replacements: BTreeMap<String, serde_json::Value>,
}

/// Write context.json to a thread directory.
pub fn write_context(dir: &Path, ctx: &ContextFile) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_string_pretty(ctx).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("context.json"), json).map_err(|e| e.to_string())
}

/// Read context.json from a thread directory. Returns None if file doesn't exist.
pub fn read_context(dir: &Path) -> Result<Option<ContextFile>, String> {
    let path = dir.join("context.json");
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let ctx: ContextFile = serde_json::from_str(&content).map_err(|e| e.to_string())?;
    Ok(Some(ctx))
}

/// Write a default view.json (include all, no masks, no replacements).
pub fn write_default_view(dir: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let view = ViewFile {
        parent: None,
        include: vec![IncludeRange {
            start: 0,
            end: None,
        }],
        masks: vec![],
        replacements: BTreeMap::new(),
    };
    let json = serde_json::to_string_pretty(&view).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("view.json"), json).map_err(|e| e.to_string())
}

/// Read view.json from a thread directory. Returns None if file doesn't exist.
pub fn read_view(dir: &Path) -> Result<Option<ViewFile>, String> {
    let path = dir.join("view.json");
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let view: ViewFile = serde_json::from_str(&content).map_err(|e| e.to_string())?;
    Ok(Some(view))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_context(thread_id: &str, title: &str) -> ContextFile {
        let mut stores = BTreeMap::new();
        stores.insert("system".to_string(), serde_json::json!("You are helpful."));
        stores.insert(
            "model".to_string(),
            serde_json::json!({"model": "claude-sonnet-4-20250514", "max_tokens": 4096}),
        );
        ContextFile {
            version: 1,
            thread_id: thread_id.to_string(),
            title: title.to_string(),
            labels: vec!["backend".to_string()],
            created_at: 1712345678,
            updated_at: 1712345900,
            stores,
        }
    }

    #[test]
    fn write_and_read_context() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = make_context("t_abc123", "Test thread");
        write_context(dir.path(), &ctx).unwrap();

        let read_back = read_context(dir.path()).unwrap().unwrap();
        assert_eq!(read_back.version, 1);
        assert_eq!(read_back.thread_id, "t_abc123");
        assert_eq!(read_back.title, "Test thread");
        assert_eq!(read_back.labels, vec!["backend"]);
        assert_eq!(read_back.stores.len(), 2);
        assert_eq!(
            read_back.stores["system"],
            serde_json::json!("You are helpful.")
        );
    }

    #[test]
    fn read_context_returns_none_for_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_context(dir.path()).unwrap().is_none());
    }

    #[test]
    fn write_and_read_default_view() {
        let dir = tempfile::tempdir().unwrap();
        write_default_view(dir.path()).unwrap();

        let view = read_view(dir.path()).unwrap().unwrap();
        assert!(view.parent.is_none());
        assert_eq!(view.include.len(), 1);
        assert_eq!(view.include[0].start, 0);
        assert!(view.include[0].end.is_none());
        assert!(view.masks.is_empty());
        assert!(view.replacements.is_empty());
    }

    #[test]
    fn read_view_returns_none_for_missing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_view(dir.path()).unwrap().is_none());
    }
}
