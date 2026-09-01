#![cfg(not(target_arch = "wasm32"))]

use std::time::{Duration, Instant};

use ox_tools::sandbox::{
    AccessIntent, ExecCommand, PermissivePolicy, SandboxedExecOptions, ToolCancellation,
    sandboxed_exec_with_options,
};

fn executor() -> &'static std::path::Path {
    std::path::Path::new(env!("CARGO_BIN_EXE_ox-tool-exec"))
}

fn shell(command: &str) -> ExecCommand {
    ExecCommand {
        op: "os/shell".to_string(),
        args: serde_json::json!({"command": command, "workspace": "/tmp"}),
    }
}

fn options(timeout: Duration) -> SandboxedExecOptions {
    SandboxedExecOptions {
        timeout,
        max_stdin_bytes: 64 * 1024,
        max_stdout_bytes: 4096,
        max_stderr_bytes: 4096,
        cancellation: ToolCancellation::default(),
    }
}

#[test]
fn timeout_kills_shell_process_group_and_returns_promptly() {
    let started = Instant::now();
    let result = sandboxed_exec_with_options(
        &AccessIntent::ShellInWorkspace("/tmp".into()),
        &shell("sleep 30 & wait"),
        executor(),
        &PermissivePolicy,
        &options(Duration::from_millis(100)),
    );
    assert!(matches!(result, Err(ref error) if error.contains("timed out")));
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[cfg(unix)]
#[test]
fn normal_launcher_exit_kills_background_descendant_before_pipe_join() {
    let started = Instant::now();
    let value = sandboxed_exec_with_options(
        &AccessIntent::ShellInWorkspace("/tmp".into()),
        &shell("sleep 30 & echo $!"),
        executor(),
        &PermissivePolicy,
        &options(Duration::from_secs(5)),
    )
    .expect("background command should complete without waiting for descendant");
    assert!(started.elapsed() < Duration::from_secs(2));
    let pid: i32 = value["stdout"]
        .as_str()
        .expect("stdout string")
        .trim()
        .parse()
        .expect("background pid");
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        // SAFETY: signal 0 performs a liveness/permission check and does not
        // modify the process. The pid came from the just-launched shell.
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        if !alive {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "background descendant {pid} survived process-group cleanup"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn cancellation_kills_active_shell_through_public_token() {
    let exec_options = options(Duration::from_secs(30));
    let cancellation = exec_options.cancellation.clone();
    let worker = std::thread::spawn(move || {
        sandboxed_exec_with_options(
            &AccessIntent::ShellInWorkspace("/tmp".into()),
            &shell("sleep 30"),
            executor(),
            &PermissivePolicy,
            &exec_options,
        )
    });
    std::thread::sleep(Duration::from_millis(100));
    cancellation.cancel();
    let result = worker.join().expect("tool worker panicked");
    assert!(matches!(result, Err(ref error) if error.contains("cancelled")));
}

#[test]
fn inner_shell_capture_is_bounded_before_json_encoding() {
    let value = sandboxed_exec_with_options(
        &AccessIntent::ShellInWorkspace("/tmp".into()),
        &shell("yes x | head -c 1000000"),
        executor(),
        &PermissivePolicy,
        &options(Duration::from_secs(5)),
    )
    .expect("bounded shell should return a result");
    let stdout = value["stdout"].as_str().expect("stdout string");
    assert!(stdout.len() < 2048, "inner capture was not bounded");
    assert!(stdout.contains("truncated"));
}

#[test]
fn oversized_input_is_rejected_before_spawn() {
    let mut exec_options = options(Duration::from_secs(1));
    exec_options.max_stdin_bytes = 32;
    let result = sandboxed_exec_with_options(
        &AccessIntent::ShellInWorkspace("/tmp".into()),
        &shell("true"),
        executor(),
        &PermissivePolicy,
        &exec_options,
    );
    assert!(matches!(result, Err(ref error) if error.contains("input exceeded")));
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use clash::policy::sandbox_types::{
        Cap, NetworkPolicy, PathMatch, RuleEffect, SandboxPolicy as ClashPolicy, SandboxRule,
    };
    use ox_tools::sandbox::SandboxPolicy;

    struct RequiredClash {
        policy: ClashPolicy,
        cwd: std::path::PathBuf,
    }

    impl SandboxPolicy for RequiredClash {
        fn apply(
            &self,
            _intent: &AccessIntent,
            cmd: std::process::Command,
        ) -> Result<std::process::Command, String> {
            let mut wrapped = std::process::Command::new(cmd.get_program());
            wrapped
                .arg("--clash-sandbox")
                .arg(serde_json::to_string(&self.policy).unwrap())
                .arg(&self.cwd);
            Ok(wrapped)
        }
    }

    fn allow(path: &std::path::Path, caps: Cap) -> SandboxRule {
        SandboxRule {
            effect: RuleEffect::Allow,
            caps,
            path: path.to_string_lossy().into_owned(),
            path_match: PathMatch::Subpath,
            doc: None,
            follow_worktrees: false,
        }
    }

    fn policy(workspace: &std::path::Path) -> RequiredClash {
        let mut rules = vec![allow(
            workspace,
            Cap::READ | Cap::WRITE | Cap::CREATE | Cap::DELETE | Cap::EXECUTE,
        )];
        for path in [
            "/bin",
            "/usr/bin",
            "/usr/local/bin",
            "/lib",
            "/lib64",
            "/usr/lib",
        ] {
            let path = std::path::Path::new(path);
            if path.exists() {
                rules.push(allow(path, Cap::READ | Cap::EXECUTE));
            }
        }
        rules.push(allow(executor(), Cap::READ | Cap::EXECUTE));
        RequiredClash {
            policy: ClashPolicy {
                default: Cap::empty(),
                rules,
                network: NetworkPolicy::Deny,
                doc: None,
            },
            cwd: workspace.to_path_buf(),
        }
    }

    #[test]
    fn landlock_allows_workspace_but_blocks_host_read_and_write() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let allowed = workspace.join("allowed.txt");
        let secret = temp.path().join("secret.txt");
        std::fs::write(&allowed, "allowed").unwrap();
        std::fs::write(&secret, "secret").unwrap();
        let policy = policy(&workspace);
        let exec_options = options(Duration::from_secs(5));

        let read_allowed = ExecCommand {
            op: "fs/read".to_string(),
            args: serde_json::json!({"path": allowed}),
        };
        assert_eq!(
            sandboxed_exec_with_options(
                &AccessIntent::ReadFile(workspace.join("allowed.txt")),
                &read_allowed,
                executor(),
                &policy,
                &exec_options,
            )
            .unwrap(),
            serde_json::json!("allowed")
        );

        let read_secret = ExecCommand {
            op: "fs/read".to_string(),
            args: serde_json::json!({"path": secret}),
        };
        assert!(
            sandboxed_exec_with_options(
                &AccessIntent::ReadFile(temp.path().join("secret.txt")),
                &read_secret,
                executor(),
                &policy,
                &exec_options,
            )
            .is_err()
        );

        let write_escape = ExecCommand {
            op: "fs/write".to_string(),
            args: serde_json::json!({
                "path": temp.path().join("escape.txt"),
                "content": "escaped"
            }),
        };
        assert!(
            sandboxed_exec_with_options(
                &AccessIntent::WriteFile(temp.path().join("escape.txt")),
                &write_escape,
                executor(),
                &policy,
                &exec_options,
            )
            .is_err()
        );
        assert!(!temp.path().join("escape.txt").exists());
    }

    #[test]
    fn seccomp_blocks_network_escape() {
        let temp = tempfile::tempdir().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let probe = ExecCommand {
            op: "diagnostics/network_probe".to_string(),
            args: serde_json::json!({"address": address}),
        };
        let result = sandboxed_exec_with_options(
            &AccessIntent::ShellInWorkspace(workspace.clone()),
            &probe,
            executor(),
            &policy(&workspace),
            &options(Duration::from_secs(5)),
        );
        assert!(
            result.is_err(),
            "sandbox unexpectedly connected: {result:?}"
        );
        listener.set_nonblocking(true).unwrap();
        assert!(listener.accept().is_err());
    }
}
