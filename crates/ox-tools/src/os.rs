use std::path::PathBuf;
use std::sync::Arc;

use serde_json::Value;

use crate::ToolSchemaEntry;
use crate::sandbox::SandboxPolicy;
#[cfg(not(target_arch = "wasm32"))]
use crate::sandbox::{AccessIntent, ExecCommand, sandboxed_exec};

/// OS tool module: shell command execution within a workspace.
///
/// All commands are delegated to an external executor binary through
/// `sandboxed_exec`, allowing a `SandboxPolicy` to wrap every invocation.
#[allow(dead_code)] // Fields used on native, not on wasm32
pub struct OsModule {
    workspace: PathBuf,
    executor_bin: PathBuf,
    policy: Arc<dyn SandboxPolicy>,
}

impl OsModule {
    pub fn new(workspace: PathBuf, executor_bin: PathBuf, policy: Arc<dyn SandboxPolicy>) -> Self {
        Self {
            workspace,
            executor_bin,
            policy,
        }
    }

    /// Execute an os operation by name.
    ///
    /// On wasm32 targets this always returns an error — subprocess execution
    /// is not available in the browser.
    pub fn execute(&self, op: &str, input: &Value) -> Result<Value, String> {
        #[cfg(target_arch = "wasm32")]
        {
            let _ = (op, input);
            Err("os operations are not available on wasm32 targets".to_string())
        }

        #[cfg(not(target_arch = "wasm32"))]
        match op {
            "shell" => self.run_shell(input),
            _ => Err(format!("unknown os operation: {op}")),
        }
    }

    /// Return tool schemas for os operations.
    pub fn schemas(&self) -> Vec<ToolSchemaEntry> {
        vec![ToolSchemaEntry {
            wire_name: "shell".to_string(),
            internal_path: "os/shell".to_string(),
            description: "Run a shell command in the workspace directory".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": {
                        "type": "string",
                        "description": "Shell command to execute"
                    }
                },
                "required": ["command"]
            }),
        }]
    }

    #[cfg(not(target_arch = "wasm32"))]
    fn run_shell(&self, input: &Value) -> Result<Value, String> {
        input
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "missing 'command' field".to_string())?;

        let intent = AccessIntent::ShellInWorkspace(self.workspace.clone());

        // Include workspace in args so the executor knows where to run
        let mut args = input.clone();
        if let Some(obj) = args.as_object_mut() {
            obj.insert(
                "workspace".to_string(),
                Value::String(self.workspace.to_string_lossy().into()),
            );
        }

        let exec_cmd = ExecCommand {
            op: "os/shell".to_string(),
            args,
        };

        sandboxed_exec(&intent, &exec_cmd, &self.executor_bin, self.policy.as_ref())
    }
}
