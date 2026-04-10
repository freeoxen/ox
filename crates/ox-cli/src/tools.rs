#![allow(deprecated)] // FnTool/Tool pending migration to ToolStore

use std::path::{Component, Path, PathBuf};

use ox_kernel::{FnTool, Tool};

/// Resolve a user-supplied path against the workspace root.
/// Returns an error if the resolved path escapes the workspace.
pub fn resolve_path(workspace: &Path, relative: &str) -> Result<PathBuf, String> {
    let path = if Path::new(relative).is_absolute() {
        PathBuf::from(relative)
    } else {
        workspace.join(relative)
    };
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            other => normalized.push(other),
        }
    }
    if !normalized.starts_with(workspace) {
        return Err(format!("path '{}' escapes workspace root", relative));
    }
    Ok(normalized)
}

pub fn read_file(workspace: PathBuf) -> FnTool {
    FnTool::new(
        "read_file",
        "Read the contents of a file in the workspace",
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path relative to the workspace root" }
            },
            "required": ["path"]
        }),
        move |input| {
            let p = input
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or("missing 'path'")?;
            let resolved = resolve_path(&workspace, p)?;
            std::fs::read_to_string(&resolved)
                .map_err(|e| format!("failed to read '{}': {e}", resolved.display()))
        },
    )
}

pub fn write_file(workspace: PathBuf) -> FnTool {
    FnTool::new(
        "write_file",
        "Write content to a file in the workspace (creates parent directories as needed)",
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path relative to the workspace root" },
                "content": { "type": "string", "description": "Content to write" }
            },
            "required": ["path", "content"]
        }),
        move |input| {
            let p = input
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or("missing 'path'")?;
            let content = input
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or("missing 'content'")?;
            let resolved = resolve_path(&workspace, p)?;
            if let Some(parent) = resolved.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("mkdir: {e}"))?;
            }
            std::fs::write(&resolved, content)
                .map_err(|e| format!("failed to write '{}': {e}", resolved.display()))?;
            Ok(format!("wrote {} bytes to {p}", content.len()))
        },
    )
}

pub fn edit_file(workspace: PathBuf) -> FnTool {
    FnTool::new(
        "edit_file",
        "Replace a string in a file. old_string must be unique in the file, or provide line_start to disambiguate.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": { "type": "string", "description": "File path relative to the workspace root" },
                "old_string": { "type": "string", "description": "Exact string to find" },
                "new_string": { "type": "string", "description": "Replacement string" },
                "line_start": { "type": "integer", "description": "1-based line number to start searching from (narrows scope when old_string is not unique)" }
            },
            "required": ["path", "old_string", "new_string"]
        }),
        move |input| {
            let p = input
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or("missing 'path'")?;
            let old = input
                .get("old_string")
                .and_then(|v| v.as_str())
                .ok_or("missing 'old_string'")?;
            let new = input
                .get("new_string")
                .and_then(|v| v.as_str())
                .ok_or("missing 'new_string'")?;
            let line_start = input.get("line_start").and_then(|v| v.as_u64());
            let resolved = resolve_path(&workspace, p)?;
            let content = std::fs::read_to_string(&resolved)
                .map_err(|e| format!("failed to read '{}': {e}", resolved.display()))?;

            let updated = if let Some(start_line) = line_start {
                // Narrow search to content from line_start onward
                let start_line = start_line.max(1) as usize - 1; // 0-based
                let lines: Vec<&str> = content.lines().collect();
                let prefix: String = lines
                    .iter()
                    .take(start_line)
                    .map(|l| format!("{l}\n"))
                    .collect();
                let suffix: String = lines
                    .iter()
                    .skip(start_line)
                    .map(|l| format!("{l}\n"))
                    .collect();
                // Trim trailing newline if original didn't end with one
                let suffix = if content.ends_with('\n') {
                    suffix
                } else {
                    suffix.strip_suffix('\n').unwrap_or(&suffix).to_string()
                };
                let count = suffix.matches(old).count();
                if count == 0 {
                    return Err(format!(
                        "old_string not found in {p} from line {}",
                        start_line + 1
                    ));
                }
                if count > 1 {
                    return Err(format!(
                        "old_string found {count} times in {p} from line {} (must be unique)",
                        start_line + 1
                    ));
                }
                format!("{prefix}{}", suffix.replacen(old, new, 1))
            } else {
                let count = content.matches(old).count();
                if count == 0 {
                    return Err(format!("old_string not found in {p}"));
                }
                if count > 1 {
                    // Report line numbers of matches to help the LLM retry with line_start
                    let match_lines: Vec<usize> = content
                        .lines()
                        .enumerate()
                        .filter(|(_, line)| line.contains(old))
                        .map(|(i, _)| i + 1)
                        .collect();
                    return Err(format!(
                        "old_string found {count} times in {p} at lines {match_lines:?} — use line_start to disambiguate"
                    ));
                }
                content.replacen(old, new, 1)
            };

            std::fs::write(&resolved, &updated)
                .map_err(|e| format!("failed to write '{}': {e}", resolved.display()))?;
            Ok(format!("edited {p}"))
        },
    )
}

pub fn shell(workspace: PathBuf) -> FnTool {
    FnTool::new(
        "shell",
        "Run a shell command in the workspace directory. Returns stdout, stderr, and exit code.",
        serde_json::json!({
            "type": "object",
            "properties": {
                "command": { "type": "string", "description": "Shell command to run" }
            },
            "required": ["command"]
        }),
        move |input| {
            let cmd = input
                .get("command")
                .and_then(|v| v.as_str())
                .ok_or("missing 'command'")?;
            let output = std::process::Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .current_dir(&workspace)
                .output()
                .map_err(|e| format!("failed to execute: {e}"))?;
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let mut result = String::new();
            if !stdout.is_empty() {
                result.push_str(&stdout);
            }
            if !stderr.is_empty() {
                if !result.is_empty() {
                    result.push('\n');
                }
                result.push_str("[stderr]\n");
                result.push_str(&stderr);
            }
            if !output.status.success() {
                result.push_str(&format!(
                    "\n[exit code: {}]",
                    output.status.code().unwrap_or(-1)
                ));
            }
            Ok(result)
        },
    )
}

/// Create all standard distribution tools for the given workspace.
pub fn standard_tools(workspace: PathBuf) -> Vec<Box<dyn Tool>> {
    vec![
        Box::new(read_file(workspace.clone())),
        Box::new(write_file(workspace.clone())),
        Box::new(edit_file(workspace.clone())),
        Box::new(shell(workspace)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_relative_path() {
        let ws = PathBuf::from("/project");
        let resolved = resolve_path(&ws, "src/main.rs").unwrap();
        assert_eq!(resolved, PathBuf::from("/project/src/main.rs"));
    }

    #[test]
    fn resolve_rejects_escape() {
        let ws = PathBuf::from("/project");
        assert!(resolve_path(&ws, "../../etc/passwd").is_err());
    }

    #[test]
    fn resolve_normalizes_dots() {
        let ws = PathBuf::from("/project");
        let resolved = resolve_path(&ws, "src/../src/main.rs").unwrap();
        assert_eq!(resolved, PathBuf::from("/project/src/main.rs"));
    }

    #[test]
    fn test_read_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("hello.txt"), "hello world").unwrap();
        let tool = read_file(dir.path().to_path_buf());
        assert_eq!(
            tool.execute(serde_json::json!({"path": "hello.txt"}))
                .unwrap(),
            "hello world"
        );
    }

    #[test]
    fn test_read_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let tool = read_file(dir.path().to_path_buf());
        assert!(
            tool.execute(serde_json::json!({"path": "nope.txt"}))
                .is_err()
        );
    }

    #[test]
    fn test_read_file_escapes() {
        let dir = tempfile::tempdir().unwrap();
        let tool = read_file(dir.path().to_path_buf());
        let err = tool
            .execute(serde_json::json!({"path": "../../etc/passwd"}))
            .unwrap_err();
        assert!(err.contains("escapes"));
    }

    #[test]
    fn test_write_file() {
        let dir = tempfile::tempdir().unwrap();
        let tool = write_file(dir.path().to_path_buf());
        tool.execute(serde_json::json!({"path": "out.txt", "content": "hi"}))
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("out.txt")).unwrap(),
            "hi"
        );
    }

    #[test]
    fn test_write_file_creates_parents() {
        let dir = tempfile::tempdir().unwrap();
        let tool = write_file(dir.path().to_path_buf());
        tool.execute(serde_json::json!({"path": "a/b/c.txt", "content": "deep"}))
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a/b/c.txt")).unwrap(),
            "deep"
        );
    }

    #[test]
    fn test_edit_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "hello world").unwrap();
        let tool = edit_file(dir.path().to_path_buf());
        tool.execute(serde_json::json!({
            "path": "f.txt",
            "old_string": "hello",
            "new_string": "goodbye"
        }))
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "goodbye world"
        );
    }

    #[test]
    fn test_edit_file_not_found() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "hello").unwrap();
        let tool = edit_file(dir.path().to_path_buf());
        let err = tool
            .execute(serde_json::json!({
                "path": "f.txt",
                "old_string": "nope",
                "new_string": "x"
            }))
            .unwrap_err();
        assert!(err.contains("not found"));
    }

    #[test]
    fn test_edit_file_not_unique_reports_lines() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "x = 1\ny = 2\nx = 3\n").unwrap();
        let tool = edit_file(dir.path().to_path_buf());
        let err = tool
            .execute(serde_json::json!({
                "path": "f.txt",
                "old_string": "x",
                "new_string": "z"
            }))
            .unwrap_err();
        assert!(err.contains("line_start"));
        assert!(err.contains("[1, 3]"));
    }

    #[test]
    fn test_edit_file_with_line_start() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "x = 1\ny = 2\nx = 3\n").unwrap();
        let tool = edit_file(dir.path().to_path_buf());
        // Disambiguate: edit only the x on line 3
        tool.execute(serde_json::json!({
            "path": "f.txt",
            "old_string": "x",
            "new_string": "z",
            "line_start": 3
        }))
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "x = 1\ny = 2\nz = 3\n"
        );
    }

    #[test]
    fn test_shell() {
        let dir = tempfile::tempdir().unwrap();
        let tool = shell(dir.path().to_path_buf());
        let result = tool
            .execute(serde_json::json!({"command": "echo hello"}))
            .unwrap();
        assert_eq!(result.trim(), "hello");
    }

    #[test]
    fn test_shell_exit_code() {
        let dir = tempfile::tempdir().unwrap();
        let tool = shell(dir.path().to_path_buf());
        let result = tool
            .execute(serde_json::json!({"command": "false"}))
            .unwrap();
        assert!(result.contains("[exit code:"));
    }
}
