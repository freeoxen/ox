use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Declares the kind of access a tool operation needs.
///
/// ox-tools declares intent; an external policy (e.g. Clash) decides
/// how to enforce it at the OS level.
#[derive(Debug, Clone)]
pub enum AccessIntent {
    ReadFile(PathBuf),
    WriteFile(PathBuf),
    ReadWriteFile(PathBuf),
    ShellInWorkspace(PathBuf),
}

/// The contract between ox-tools and a permission enforcement system.
///
/// Implementations receive an `AccessIntent` plus a pre-built `Command`
/// and may wrap, modify, or reject it.
///
/// On wasm32 targets this trait exists for type-checking purposes only;
/// `sandboxed_exec` is not available and subprocess execution is unsupported.
#[cfg(not(target_arch = "wasm32"))]
pub trait SandboxPolicy: Send + Sync {
    fn apply(
        &self,
        intent: &AccessIntent,
        cmd: std::process::Command,
    ) -> Result<std::process::Command, String>;
}

/// Wasm stub: SandboxPolicy with no `apply` — subprocess execution unavailable.
#[cfg(target_arch = "wasm32")]
pub trait SandboxPolicy: Send + Sync {}

/// A no-op policy that passes every command through unchanged.
/// Useful for tests and trusted environments.
pub struct PermissivePolicy;

#[cfg(not(target_arch = "wasm32"))]
impl SandboxPolicy for PermissivePolicy {
    fn apply(
        &self,
        _intent: &AccessIntent,
        cmd: std::process::Command,
    ) -> Result<std::process::Command, String> {
        Ok(cmd)
    }
}

#[cfg(target_arch = "wasm32")]
impl SandboxPolicy for PermissivePolicy {}

/// JSON-serializable command sent to the executor binary via stdin.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExecCommand {
    pub op: String,
    pub args: serde_json::Value,
}

/// JSON-serializable result received from the executor binary via stdout.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExecResult {
    pub ok: bool,
    pub value: serde_json::Value,
}

/// Build a `Command` targeting the executor binary, apply the sandbox policy,
/// pipe `ExecCommand` as JSON on stdin, and parse `ExecResult` from stdout.
///
/// Not available on wasm32 targets — subprocess execution requires a native OS.
#[cfg(not(target_arch = "wasm32"))]
pub fn sandboxed_exec(
    intent: &AccessIntent,
    exec_cmd: &ExecCommand,
    executor_bin: &std::path::Path,
    policy: &dyn SandboxPolicy,
) -> Result<serde_json::Value, String> {
    use std::process::Command;

    let base = Command::new(executor_bin);
    let mut cmd = policy.apply(intent, base)?;

    let input_json =
        serde_json::to_string(exec_cmd).map_err(|e| format!("failed to serialize command: {e}"))?;

    cmd.arg("--tool-exec")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn executor: {e}"))?;

    // Write JSON to stdin
    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().ok_or("failed to open stdin")?;
        stdin
            .write_all(input_json.as_bytes())
            .map_err(|e| format!("failed to write to stdin: {e}"))?;
    }

    let output = child
        .wait_with_output()
        .map_err(|e| format!("failed to wait on executor: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "executor exited with {}: {}",
            output.status, stderr
        ));
    }

    let result: ExecResult = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("failed to parse executor output: {e}"))?;

    if result.ok {
        Ok(result.value)
    } else {
        Err(result.value.as_str().unwrap_or("unknown error").to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn permissive_policy_passes_command_through() {
        use std::process::Command;

        let policy = PermissivePolicy;
        let intent = AccessIntent::ReadFile(PathBuf::from("/tmp/test.txt"));
        let cmd = Command::new("echo");
        let result = policy.apply(&intent, cmd);
        assert!(result.is_ok());
        // The command should still target "echo"
        let wrapped = result.unwrap();
        assert_eq!(format!("{:?}", wrapped).contains("echo"), true);
    }

    #[test]
    fn exec_command_serializes_to_json() {
        let cmd = ExecCommand {
            op: "fs/read".to_string(),
            args: serde_json::json!({"path": "/tmp/test.txt"}),
        };

        let json = serde_json::to_string(&cmd).unwrap();
        let roundtripped: ExecCommand = serde_json::from_str(&json).unwrap();
        assert_eq!(cmd, roundtripped);
    }
}
