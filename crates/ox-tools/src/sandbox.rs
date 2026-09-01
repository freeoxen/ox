use serde::{Deserialize, Serialize};
use std::path::PathBuf;
#[cfg(not(target_arch = "wasm32"))]
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

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

/// Cooperative cancellation shared by a caller and one or more tool calls.
///
/// Cancellation is sticky for a turn. Callers may either create a fresh token
/// per turn or reset it only after the prior turn and subprocess have joined,
/// as the executor's sequential per-thread worker does.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug, Default)]
pub struct ToolCancellation {
    cancelled: Arc<AtomicBool>,
}

#[cfg(not(target_arch = "wasm32"))]
impl ToolCancellation {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    /// Prepare a sequential worker's token for its next turn. The caller must
    /// have joined the previous turn and its subprocess before resetting.
    pub fn reset(&self) {
        self.cancelled.store(false, Ordering::Release);
    }
}

/// Resource bounds for one `ox-tool-exec` subprocess.
#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone, Debug)]
pub struct SandboxedExecOptions {
    pub timeout: Duration,
    pub max_stdin_bytes: usize,
    pub max_stdout_bytes: usize,
    pub max_stderr_bytes: usize,
    pub cancellation: ToolCancellation,
}

#[cfg(not(target_arch = "wasm32"))]
impl SandboxedExecOptions {
    /// Bounded defaults used by the interactive CLI.
    pub fn local_compatible() -> Self {
        Self {
            timeout: Duration::from_secs(10 * 60),
            max_stdin_bytes: 16 * 1024 * 1024,
            max_stdout_bytes: 16 * 1024 * 1024,
            max_stderr_bytes: 1024 * 1024,
            cancellation: ToolCancellation::default(),
        }
    }

    /// Tighter defaults for an unattended remote worker.
    pub fn remote() -> Self {
        Self {
            timeout: Duration::from_secs(2 * 60),
            max_stdin_bytes: 4 * 1024 * 1024,
            max_stdout_bytes: 4 * 1024 * 1024,
            max_stderr_bytes: 256 * 1024,
            cancellation: ToolCancellation::default(),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Default for SandboxedExecOptions {
    fn default() -> Self {
        Self::local_compatible()
    }
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
    sandboxed_exec_with_options(
        intent,
        exec_cmd,
        executor_bin,
        policy,
        &SandboxedExecOptions::default(),
    )
}

/// Execute a tool with explicit cancellation, time, and output bounds.
#[cfg(not(target_arch = "wasm32"))]
pub fn sandboxed_exec_with_options(
    intent: &AccessIntent,
    exec_cmd: &ExecCommand,
    executor_bin: &std::path::Path,
    policy: &dyn SandboxPolicy,
    options: &SandboxedExecOptions,
) -> Result<serde_json::Value, String> {
    use std::process::Command;
    use std::time::Instant;

    if options.max_stdin_bytes == 0
        || options.max_stdout_bytes == 0
        || options.max_stderr_bytes == 0
    {
        return Err("tool I/O limits must be non-zero".to_string());
    }
    if options.timeout.is_zero() {
        return Err("tool execution timeout must be non-zero".to_string());
    }
    if options.cancellation.is_cancelled() {
        return Err("tool execution cancelled before spawn".to_string());
    }

    let base = Command::new(executor_bin);
    let mut cmd = policy.apply(intent, base)?;

    let mut input_value =
        serde_json::to_value(exec_cmd).map_err(|e| format!("failed to serialize command: {e}"))?;
    if let Some(object) = input_value.as_object_mut() {
        // The executor applies this before running an inner shell/read. Reserve
        // JSON escaping overhead so its final envelope also fits stdout.
        object.insert(
            "_ox_max_output_bytes".to_string(),
            serde_json::json!((options.max_stdout_bytes / 8).max(1)),
        );
        object.insert(
            "_ox_max_working_bytes".to_string(),
            serde_json::json!(options.max_stdin_bytes),
        );
    }
    let input_json = serde_json::to_vec(&input_value)
        .map_err(|e| format!("failed to serialize command: {e}"))?;
    if input_json.len() > options.max_stdin_bytes {
        return Err(format!(
            "executor input exceeded {} bytes",
            options.max_stdin_bytes
        ));
    }

    cmd.arg("--tool-exec")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    // Every tool invocation owns a process group so timeout/cancellation also
    // kills grandchildren created by a shell command.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("failed to spawn executor: {e}"))?;

    // Supervise stdin on its own thread: pipe backpressure is subject to the
    // same deadline/cancellation as the child rather than blocking the caller.
    let mut stdin = child.stdin.take().ok_or("failed to open stdin")?;
    let stdin_writer = std::thread::spawn(move || {
        use std::io::Write;
        stdin
            .write_all(&input_json)
            .map_err(|e| format!("failed to write to executor stdin: {e}"))
    });

    let stdout = child.stdout.take().ok_or("failed to capture stdout")?;
    let stderr = child.stderr.take().ok_or("failed to capture stderr")?;
    let stdout_limit = options.max_stdout_bytes;
    let stderr_limit = options.max_stderr_bytes;
    let stdout_reader = std::thread::spawn(move || read_bounded(stdout, stdout_limit));
    let stderr_reader = std::thread::spawn(move || read_bounded(stderr, stderr_limit));

    let started = Instant::now();
    let (status, stop_reason) = loop {
        if options.cancellation.is_cancelled() {
            kill_process_group(&mut child);
            let status = child
                .wait()
                .map_err(|e| format!("failed to reap cancelled executor: {e}"))?;
            break (status, Some("cancelled"));
        }
        if started.elapsed() >= options.timeout {
            kill_process_group(&mut child);
            let status = child
                .wait()
                .map_err(|e| format!("failed to reap timed-out executor: {e}"))?;
            break (status, Some("timed out"));
        }
        match child
            .try_wait()
            .map_err(|e| format!("failed to poll executor: {e}"))?
        {
            Some(status) => {
                // The launcher may have exited after spawning descendants that
                // still hold our pipe FDs. Kill the recorded group before any
                // reader/writer join so those joins remain bounded.
                kill_group_id(child.id());
                break (status, None);
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    };

    let stdin_result = stdin_writer
        .join()
        .map_err(|_| "executor stdin writer panicked".to_string())?;

    let stdout = stdout_reader
        .join()
        .map_err(|_| "executor stdout reader panicked".to_string())??;
    let stderr = stderr_reader
        .join()
        .map_err(|_| "executor stderr reader panicked".to_string())??;

    if let Some(reason) = stop_reason {
        return Err(format!("tool execution {reason}"));
    }
    if stdout.truncated {
        return Err(format!(
            "executor stdout exceeded {} bytes",
            options.max_stdout_bytes
        ));
    }

    if !status.success() {
        let stderr_text = String::from_utf8_lossy(&stderr.bytes);
        let suffix = if stderr.truncated { " [truncated]" } else { "" };
        return Err(format!(
            "executor exited with {status}: {stderr_text}{suffix}"
        ));
    }
    stdin_result?;

    let result: ExecResult = serde_json::from_slice(&stdout.bytes)
        .map_err(|e| format!("failed to parse executor output: {e}"))?;

    if result.ok {
        Ok(result.value)
    } else {
        Err(result.value.as_str().unwrap_or("unknown error").to_string())
    }
}

#[cfg(not(target_arch = "wasm32"))]
struct BoundedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

#[cfg(not(target_arch = "wasm32"))]
fn read_bounded(mut reader: impl std::io::Read, limit: usize) -> Result<BoundedOutput, String> {
    let mut bytes = Vec::with_capacity(limit.min(64 * 1024));
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|e| format!("failed to read executor output: {e}"))?;
        if read == 0 {
            break;
        }
        let remaining = limit.saturating_sub(bytes.len());
        let keep = remaining.min(read);
        bytes.extend_from_slice(&buffer[..keep]);
        truncated |= keep < read;
    }
    Ok(BoundedOutput { bytes, truncated })
}

#[cfg(all(not(target_arch = "wasm32"), unix))]
fn kill_process_group(child: &mut std::process::Child) {
    kill_group_id(child.id());
    let _ = child.kill();
}

#[cfg(all(not(target_arch = "wasm32"), unix))]
fn kill_group_id(id: u32) {
    let process_group = -(id as i32);
    // SAFETY: the child was spawned as leader of a fresh process group. A
    // negative pid targets exactly that group; SIGKILL cannot be caught and
    // guarantees descendants do not outlive a cancelled tool call.
    let result = unsafe { libc::kill(process_group, libc::SIGKILL) };
    let _ = result;
}

#[cfg(all(not(target_arch = "wasm32"), not(unix)))]
fn kill_process_group(child: &mut std::process::Child) {
    let _ = child.kill();
}

#[cfg(all(not(target_arch = "wasm32"), not(unix)))]
fn kill_group_id(_id: u32) {}

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
        assert!(format!("{:?}", wrapped).contains("echo"));
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
