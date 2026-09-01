//! Reusable execution core shared by interactive and headless ox hosts.
//!
//! This crate owns the existing Wasm agent pool and its tool, policy,
//! approval, token-accounting, and snapshot behavior. Surfaces provide the
//! broker namespace and presentation; they do not implement a second runtime.

mod agents;
mod broker_mounts;
mod clash_sandbox;
mod commit_drain;
mod ingress;
mod policy;
mod policy_check;
pub mod test_support;
mod thread_registry;

pub use agents::{
    AgentPool, ExecutionCommandError, ExecutionCore, ExecutionHandle, ExecutionStats,
    ExecutorConfig, IngressBoundary, IngressFailpoints, PolicyProfile, SYSTEM_PROMPT,
    ThreadExecutionConfig, write_save_result_to_inbox,
};
pub use ingress::derive_unresolved_approval_id;

/// Digest of the exact embedded agent module used by local and headless hosts.
pub fn agent_wasm_sha256() -> String {
    use sha2::{Digest as _, Sha256};
    format!("{:x}", Sha256::digest(agents::AGENT_WASM))
}

/// Prove that the remote tool launcher can enter the required OS sandbox.
///
/// The probe asks the existing `ox-tool-exec` binary to write inside the
/// workspace, denies reads and writes to sibling paths, and denies a connection
/// to a live local listener. Launcher/serialization failures and any successful
/// escape fail readiness.
#[cfg(not(target_arch = "wasm32"))]
pub fn remote_sandbox_preflight(
    workspace: &std::path::Path,
    executor_bin: &std::path::Path,
) -> Result<(), String> {
    use ox_tools::sandbox::{AccessIntent, ExecCommand, SandboxedExecOptions};
    use std::time::Duration;

    let metadata = std::fs::metadata(executor_bin)
        .map_err(|error| format!("ox-tool-exec is unavailable: {error}"))?;
    if !metadata.is_file() {
        return Err("ox-tool-exec is not a regular file".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err("ox-tool-exec is not executable".into());
        }
    }
    std::fs::create_dir_all(workspace)
        .map_err(|error| format!("sandbox preflight workspace failed: {error}"))?;
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .map_err(|error| format!("sandbox preflight listener failed: {error}"))?;
    let address = listener
        .local_addr()
        .map_err(|error| format!("sandbox preflight address failed: {error}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| format!("sandbox preflight listener failed: {error}"))?;
    let command = ExecCommand {
        op: "diagnostics/network_probe".into(),
        args: serde_json::json!({"address": address.to_string()}),
    };
    let options = SandboxedExecOptions {
        timeout: Duration::from_secs(10),
        max_stdin_bytes: 64 * 1024,
        max_stdout_bytes: 64 * 1024,
        max_stderr_bytes: 64 * 1024,
        ..SandboxedExecOptions::remote()
    };
    let policy = clash_sandbox::ClashSandboxPolicy::required(workspace.to_owned());
    let allowed_path = workspace.join("allowed-probe");
    let allowed_write = ExecCommand {
        op: "fs/write".into(),
        args: serde_json::json!({"path": allowed_path, "content": "sandbox-preflight"}),
    };
    ox_tools::sandbox::sandboxed_exec_with_options(
        &AccessIntent::ShellInWorkspace(workspace.to_owned()),
        &allowed_write,
        executor_bin,
        &policy,
        &options,
    )
    .map_err(|error| format!("sandbox allowed-write preflight failed: {error}"))?;
    if !matches!(
        std::fs::read_to_string(&allowed_path).as_deref(),
        Ok("sandbox-preflight")
    ) {
        return Err("sandbox allowed-write preflight produced wrong content".into());
    }

    let outside = workspace
        .parent()
        .ok_or("sandbox preflight workspace has no parent")?;
    let escape_read_path = outside.join(format!("sandbox-read-{}", std::process::id()));
    let escape_write_path = outside.join(format!("sandbox-write-{}", std::process::id()));
    if escape_read_path.exists() || escape_write_path.exists() {
        return Err("sandbox preflight escape path already exists".into());
    }
    std::fs::write(&escape_read_path, "sandbox-secret")
        .map_err(|error| format!("sandbox forbidden-read fixture failed: {error}"))?;
    let forbidden_read = ExecCommand {
        op: "fs/read".into(),
        args: serde_json::json!({"path": escape_read_path}),
    };
    let read_result = ox_tools::sandbox::sandboxed_exec_with_options(
        &AccessIntent::ShellInWorkspace(workspace.to_owned()),
        &forbidden_read,
        executor_bin,
        &policy,
        &options,
    );
    std::fs::remove_file(&escape_read_path)
        .map_err(|error| format!("sandbox forbidden-read cleanup failed: {error}"))?;
    let read_error = match read_result {
        Ok(value) => {
            return Err(format!(
                "sandbox forbidden read unexpectedly succeeded: {value}"
            ));
        }
        Err(error) => error.to_ascii_lowercase(),
    };
    if !(read_error.contains("operation not permitted") || read_error.contains("permission denied"))
    {
        return Err(format!(
            "sandbox read confinement preflight failed: {read_error}"
        ));
    }

    let forbidden_write = ExecCommand {
        op: "fs/write".into(),
        args: serde_json::json!({"path": escape_write_path, "content": "escaped"}),
    };
    let filesystem_error = match ox_tools::sandbox::sandboxed_exec_with_options(
        &AccessIntent::ShellInWorkspace(workspace.to_owned()),
        &forbidden_write,
        executor_bin,
        &policy,
        &options,
    ) {
        Ok(value) => {
            return Err(format!(
                "sandbox forbidden write unexpectedly succeeded: {value}"
            ));
        }
        Err(error) => error,
    };
    let filesystem_error = filesystem_error.to_ascii_lowercase();
    if escape_write_path.exists()
        || !(filesystem_error.contains("operation not permitted")
            || filesystem_error.contains("permission denied"))
    {
        return Err(format!(
            "sandbox filesystem confinement preflight failed: {filesystem_error}"
        ));
    }

    let error = match ox_tools::sandbox::sandboxed_exec_with_options(
        &AccessIntent::ShellInWorkspace(workspace.to_owned()),
        &command,
        executor_bin,
        &policy,
        &options,
    ) {
        Ok(value) => {
            return Err(format!(
                "sandbox network probe unexpectedly succeeded: {value}"
            ));
        }
        Err(error) => error,
    };
    let lower = error.to_ascii_lowercase();
    if !lower.starts_with("connect error:")
        || !(lower.contains("operation not permitted") || lower.contains("permission denied"))
    {
        return Err(format!("sandbox enforcement preflight failed: {error}"));
    }
    if listener.accept().is_ok() {
        return Err("sandbox network probe reached the listener".into());
    }
    Ok(())
}
pub use broker_mounts::mount_execution_stores;
pub use policy::{CheckResult, PolicyGuard, PolicyLoadError, PolicyStats};
pub use thread_registry::{
    LEDGER_HEALTH_DEGRADED, LEDGER_HEALTH_MISSING, LEDGER_HEALTH_OK, LEDGER_HEALTH_REPAIR_FAILED,
    ThreadNamespace, ThreadRegistry,
};

/// Existing shell contract used by the crash-recovery Skip decision.
pub const POST_CRASH_SKIP_CONTENT: &str = "[ox-cli: skipped by user after crash recovery. \
    The tool was not re-executed. Do not retry this tool in this turn.]";
