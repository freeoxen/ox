use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::Value;

use crate::sandbox::{AccessIntent, ExecCommand, SandboxPolicy};
#[cfg(not(target_arch = "wasm32"))]
use crate::sandbox::sandboxed_exec;
use crate::ToolSchemaEntry;

/// File-system tool module: read, write, and edit files within a workspace.
///
/// All operations are delegated to an external executor binary through
/// `sandboxed_exec`, allowing a `SandboxPolicy` to wrap every invocation.
pub struct FsModule {
    workspace: PathBuf,
    executor_bin: PathBuf,
    policy: Arc<dyn SandboxPolicy>,
}

impl FsModule {
    pub fn new(
        workspace: PathBuf,
        executor_bin: PathBuf,
        policy: Arc<dyn SandboxPolicy>,
    ) -> Self {
        Self {
            workspace,
            executor_bin,
            policy,
        }
    }

    /// Resolve a relative path against the workspace root, rejecting escapes.
    fn resolve_path(&self, rel: &str) -> Result<PathBuf, String> {
        // Normalize by joining and canonicalizing what we can
        let candidate = self.workspace.join(rel);

        // We need to check that the resolved path is within the workspace.
        // Since the file might not exist yet (for writes), we canonicalize
        // the longest existing prefix and check containment.
        let resolved = resolve_within(&self.workspace, &candidate)?;
        Ok(resolved)
    }

    /// Execute an fs operation by name.
    ///
    /// On wasm32 targets this always returns an error — subprocess execution
    /// is not available in the browser.
    pub fn execute(&self, op: &str, input: &Value) -> Result<Value, String> {
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (op, input);
            return Err("fs operations are not available on wasm32 targets".to_string());
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            let path_str = input
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "missing 'path' field".to_string())?;

            let resolved = self.resolve_path(path_str)?;

            let (intent, full_op) = match op {
                "read" => (AccessIntent::ReadFile(resolved.clone()), "fs/read"),
                "write" => (AccessIntent::WriteFile(resolved.clone()), "fs/write"),
                "edit" => (AccessIntent::ReadWriteFile(resolved.clone()), "fs/edit"),
                _ => return Err(format!("unknown fs operation: {op}")),
            };

            // Build args with the resolved absolute path
            let mut args = input.clone();
            if let Some(obj) = args.as_object_mut() {
                obj.insert(
                    "path".to_string(),
                    Value::String(resolved.to_string_lossy().into()),
                );
            }

            let exec_cmd = ExecCommand {
                op: full_op.to_string(),
                args,
            };

            sandboxed_exec(&intent, &exec_cmd, &self.executor_bin, self.policy.as_ref())
        }
    }

    /// Return tool schemas for the three fs operations.
    pub fn schemas(&self) -> Vec<ToolSchemaEntry> {
        vec![
            ToolSchemaEntry {
                wire_name: "fs_read".to_string(),
                internal_path: "fs/read".to_string(),
                description: "Read the contents of a file".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Relative path within the workspace"
                        }
                    },
                    "required": ["path"]
                }),
            },
            ToolSchemaEntry {
                wire_name: "fs_write".to_string(),
                internal_path: "fs/write".to_string(),
                description: "Write content to a file, creating parent directories as needed"
                    .to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Relative path within the workspace"
                        },
                        "content": {
                            "type": "string",
                            "description": "Content to write"
                        }
                    },
                    "required": ["path", "content"]
                }),
            },
            ToolSchemaEntry {
                wire_name: "fs_edit".to_string(),
                internal_path: "fs/edit".to_string(),
                description: "Edit a file by replacing a string".to_string(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": {
                            "type": "string",
                            "description": "Relative path within the workspace"
                        },
                        "old_string": {
                            "type": "string",
                            "description": "The exact string to find and replace"
                        },
                        "new_string": {
                            "type": "string",
                            "description": "The replacement string"
                        },
                        "line_start": {
                            "type": "integer",
                            "description": "1-based line number hint to disambiguate multiple matches"
                        }
                    },
                    "required": ["path", "old_string", "new_string"]
                }),
            },
        ]
    }
}

/// Resolve `candidate` ensuring it stays within `workspace`.
/// Works even if the file doesn't exist yet by canonicalizing the
/// longest existing ancestor.
fn resolve_within(workspace: &Path, candidate: &Path) -> Result<PathBuf, String> {
    // Canonicalize the workspace (must exist)
    let canon_ws = workspace
        .canonicalize()
        .map_err(|e| format!("cannot canonicalize workspace: {e}"))?;

    // Walk up until we find an existing ancestor, then append the remainder
    let mut existing = candidate.to_path_buf();
    let mut suffix_parts: Vec<std::ffi::OsString> = Vec::new();

    while !existing.exists() {
        if let Some(name) = existing.file_name() {
            suffix_parts.push(name.to_os_string());
        } else {
            return Err("path resolves to nothing".to_string());
        }
        existing = existing
            .parent()
            .ok_or_else(|| "path escapes filesystem root".to_string())?
            .to_path_buf();
    }

    let mut canon = existing
        .canonicalize()
        .map_err(|e| format!("cannot canonicalize path: {e}"))?;

    // Re-append the non-existing tail
    for part in suffix_parts.into_iter().rev() {
        canon.push(part);
    }

    if !canon.starts_with(&canon_ws) {
        return Err(format!(
            "path escapes workspace: {} is not under {}",
            canon.display(),
            canon_ws.display()
        ));
    }

    Ok(canon)
}
