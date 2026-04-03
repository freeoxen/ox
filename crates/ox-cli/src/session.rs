use std::path::{Path, PathBuf};

/// Resolve the session directory (~/.ox/sessions/).
fn sessions_dir() -> Result<PathBuf, String> {
    let home = std::env::var("HOME").map_err(|_| "HOME not set".to_string())?;
    let dir = PathBuf::from(home).join(".ox").join("sessions");
    std::fs::create_dir_all(&dir).map_err(|e| format!("failed to create sessions dir: {e}"))?;
    Ok(dir)
}

/// Resolve the file path for a named session.
pub fn session_path(name: &str) -> Result<PathBuf, String> {
    let dir = sessions_dir()?;
    Ok(dir.join(format!("{name}.json")))
}

/// Find the most recently modified session file.
pub fn last_session() -> Result<Option<PathBuf>, String> {
    let dir = sessions_dir()?;
    let mut best: Option<(PathBuf, std::time::SystemTime)> = None;
    let entries = std::fs::read_dir(&dir).map_err(|e| e.to_string())?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "json") {
            if let Ok(meta) = path.metadata() {
                if let Ok(modified) = meta.modified() {
                    if best.as_ref().is_none_or(|(_, t)| modified > *t) {
                        best = Some((path, modified));
                    }
                }
            }
        }
    }
    Ok(best.map(|(p, _)| p))
}

/// Load session history from a JSON file.
/// Returns wire-format messages (Vec<serde_json::Value>).
pub fn load(path: &Path) -> Result<Vec<serde_json::Value>, String> {
    let content =
        std::fs::read_to_string(path).map_err(|e| format!("failed to read session: {e}"))?;
    let messages: Vec<serde_json::Value> =
        serde_json::from_str(&content).map_err(|e| format!("invalid session JSON: {e}"))?;
    Ok(messages)
}

/// Save session history to a JSON file.
/// Takes wire-format messages (Vec<serde_json::Value>).
pub fn save(path: &Path, messages: &[serde_json::Value]) -> Result<(), String> {
    let json =
        serde_json::to_string_pretty(messages).map_err(|e| format!("serialize error: {e}"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
    }
    std::fs::write(path, json).map_err(|e| format!("failed to write session: {e}"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.json");

        let messages = vec![
            serde_json::json!({"role": "user", "content": "hello"}),
            serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": "hi"}]}),
        ];

        save(&path, &messages).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0]["role"], "user");
        assert_eq!(loaded[1]["role"], "assistant");
    }

    #[test]
    fn load_missing_file() {
        let result = load(Path::new("/nonexistent/session.json"));
        assert!(result.is_err());
    }
}
