//! Thin JSON-in/JSON-out executor for fs operations.
//!
//! Reads an `ExecCommand` from stdin, performs the operation, and writes
//! an `ExecResult` to stdout.

use serde::{Deserialize, Serialize};
use std::io::Read;

#[derive(Deserialize)]
struct ExecCommand {
    op: String,
    args: serde_json::Value,
    #[serde(default = "default_max_output_bytes")]
    _ox_max_output_bytes: usize,
    #[serde(default = "default_max_working_bytes")]
    _ox_max_working_bytes: usize,
}

fn default_max_output_bytes() -> usize {
    2 * 1024 * 1024
}

fn default_max_working_bytes() -> usize {
    16 * 1024 * 1024
}

#[derive(Serialize)]
struct ExecResult {
    ok: bool,
    value: serde_json::Value,
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--clash-sandbox") {
        enter_clash_sandbox();
    }

    // We expect --tool-exec as first arg (for future dispatch)
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 || args[1] != "--tool-exec" {
        let result = ExecResult {
            ok: false,
            value: serde_json::Value::String("expected --tool-exec flag".into()),
        };
        serde_json::to_writer(std::io::stdout(), &result).unwrap();
        std::process::exit(1);
    }

    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input).unwrap();

    let cmd: ExecCommand = match serde_json::from_str(&input) {
        Ok(c) => c,
        Err(e) => {
            let result = ExecResult {
                ok: false,
                value: serde_json::Value::String(format!("invalid input: {e}")),
            };
            serde_json::to_writer(std::io::stdout(), &result).unwrap();
            std::process::exit(1);
        }
    };

    let result = match cmd.op.as_str() {
        "fs/read" => op_read(&cmd.args, cmd._ox_max_output_bytes),
        "fs/write" => op_write(&cmd.args),
        "fs/edit" => op_edit(&cmd.args, cmd._ox_max_working_bytes),
        "os/shell" => op_shell(&cmd.args, cmd._ox_max_output_bytes),
        // Not registered as an agent tool. Worker-image readiness tests use
        // this operation to prove the sandbox blocks socket creation.
        "diagnostics/network_probe" => op_network_probe(&cmd.args),
        other => ExecResult {
            ok: false,
            value: serde_json::Value::String(format!("unknown op: {other}")),
        },
    };

    serde_json::to_writer(std::io::stdout(), &result).unwrap();
}

fn op_network_probe(args: &serde_json::Value) -> ExecResult {
    let address = match args.get("address").and_then(|value| value.as_str()) {
        Some(address) => address,
        None => {
            return ExecResult {
                ok: false,
                value: serde_json::Value::String("missing 'address'".into()),
            };
        }
    };
    match std::net::TcpStream::connect(address) {
        Ok(_) => ExecResult {
            ok: true,
            value: serde_json::json!("connected"),
        },
        Err(error) => ExecResult {
            ok: false,
            value: serde_json::Value::String(format!("connect error: {error}")),
        },
    }
}

/// Re-exec this binary under Clash's platform backend. On Linux this is the
/// only supported route into Landlock/seccomp; profile compilation is a macOS
/// implementation detail and must never be treated as Linux enforcement.
fn enter_clash_sandbox() -> ! {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        eprintln!("invalid --clash-sandbox invocation");
        std::process::exit(125);
    }
    let policy: clash::policy::sandbox_types::SandboxPolicy = serde_json::from_str(&args[2])
        .unwrap_or_else(|error| {
            eprintln!("invalid Clash sandbox policy: {error}");
            std::process::exit(125);
        });
    let cwd = std::path::Path::new(&args[3]);
    let current_exe = std::env::current_exe().unwrap_or_else(|error| {
        eprintln!("cannot resolve ox-tool-exec: {error}");
        std::process::exit(125);
    });
    let mut command = vec![current_exe.to_string_lossy().into_owned()];
    command.extend(args[4..].iter().cloned());
    match clash::sandbox::exec_sandboxed(&policy, cwd, &command, None) {
        Ok(never) => match never {},
        Err(error) => {
            eprintln!("failed to enforce Clash sandbox: {error}");
            std::process::exit(125);
        }
    }
}

fn op_read(args: &serde_json::Value, max_output_bytes: usize) -> ExecResult {
    let path = match args.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => {
            return ExecResult {
                ok: false,
                value: serde_json::Value::String("missing 'path'".into()),
            };
        }
    };

    match read_file_bounded(path, max_output_bytes) {
        Ok(content) => ExecResult {
            ok: true,
            value: serde_json::Value::String(content),
        },
        Err(e) => ExecResult {
            ok: false,
            value: serde_json::Value::String(format!("read error: {e}")),
        },
    }
}

fn op_write(args: &serde_json::Value) -> ExecResult {
    let path = match args.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => {
            return ExecResult {
                ok: false,
                value: serde_json::Value::String("missing 'path'".into()),
            };
        }
    };
    let content = match args.get("content").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => {
            return ExecResult {
                ok: false,
                value: serde_json::Value::String("missing 'content'".into()),
            };
        }
    };

    // Create parent directories if needed
    if let Some(parent) = std::path::Path::new(path).parent() {
        if !parent.exists() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return ExecResult {
                    ok: false,
                    value: serde_json::Value::String(format!("mkdir error: {e}")),
                };
            }
        }
    }

    match std::fs::write(path, content) {
        Ok(()) => ExecResult {
            ok: true,
            value: serde_json::Value::String("ok".into()),
        },
        Err(e) => ExecResult {
            ok: false,
            value: serde_json::Value::String(format!("write error: {e}")),
        },
    }
}

fn op_edit(args: &serde_json::Value, max_working_bytes: usize) -> ExecResult {
    let path = match args.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => {
            return ExecResult {
                ok: false,
                value: serde_json::Value::String("missing 'path'".into()),
            };
        }
    };
    let old_string = match args.get("old_string").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            return ExecResult {
                ok: false,
                value: serde_json::Value::String("missing 'old_string'".into()),
            };
        }
    };
    let new_string = match args.get("new_string").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => {
            return ExecResult {
                ok: false,
                value: serde_json::Value::String("missing 'new_string'".into()),
            };
        }
    };
    let line_start = args.get("line_start").and_then(|v| v.as_u64());

    // Read the file
    let content = match read_file_strict(path, max_working_bytes) {
        Ok(c) => c,
        Err(e) => {
            return ExecResult {
                ok: false,
                value: serde_json::Value::String(format!("read error: {e}")),
            };
        }
    };

    // Find all occurrences
    let matches: Vec<usize> = content
        .match_indices(old_string)
        .map(|(idx, _)| idx)
        .collect();

    if matches.is_empty() {
        return ExecResult {
            ok: false,
            value: serde_json::Value::String(format!("'old_string' not found in {}", path)),
        };
    }

    let replacement_idx = if matches.len() == 1 {
        matches[0]
    } else if let Some(hint) = line_start {
        // Use line_start hint to disambiguate: find the match whose
        // 1-based line number equals the hint.
        let hint = hint as usize;
        let mut found = None;
        for &idx in &matches {
            let line_num = content[..idx].chars().filter(|&c| c == '\n').count() + 1;
            if line_num == hint {
                found = Some(idx);
                break;
            }
        }
        match found {
            Some(idx) => idx,
            None => {
                return ExecResult {
                    ok: false,
                    value: serde_json::Value::String(format!(
                        "'old_string' found {} times but none at line {}",
                        matches.len(),
                        hint
                    )),
                };
            }
        }
    } else {
        return ExecResult {
            ok: false,
            value: serde_json::Value::String(format!(
                "'old_string' found {} times — provide line_start to disambiguate",
                matches.len()
            )),
        };
    };

    // Perform the replacement
    let mut result = String::with_capacity(content.len());
    result.push_str(&content[..replacement_idx]);
    result.push_str(new_string);
    result.push_str(&content[replacement_idx + old_string.len()..]);

    match std::fs::write(path, &result) {
        Ok(()) => ExecResult {
            ok: true,
            value: serde_json::Value::String("ok".into()),
        },
        Err(e) => ExecResult {
            ok: false,
            value: serde_json::Value::String(format!("write error: {e}")),
        },
    }
}

fn read_file_strict(path: &str, limit: usize) -> std::io::Result<String> {
    let metadata = std::fs::metadata(path)?;
    if metadata.len() > limit as u64 {
        return Err(std::io::Error::other(format!(
            "file exceeds {limit}-byte edit limit"
        )));
    }
    std::fs::read_to_string(path)
}

fn op_shell(args: &serde_json::Value, max_output_bytes: usize) -> ExecResult {
    let command = match args.get("command").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => {
            return ExecResult {
                ok: false,
                value: serde_json::Value::String("missing 'command'".into()),
            };
        }
    };

    let workspace = args.get("workspace").and_then(|v| v.as_str());
    let max_lines = args
        .get("max_lines")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);

    let mut cmd = std::process::Command::new("sh");
    cmd.arg("-c").arg(command);

    if let Some(ws) = workspace {
        cmd.current_dir(ws);
    }

    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            return ExecResult {
                ok: false,
                value: serde_json::Value::String(format!("spawn error: {e}")),
            };
        }
    };
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    let child_done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stdout_done = child_done.clone();
    let stderr_done = child_done.clone();
    let stdout_reader = std::thread::spawn(move || {
        read_bounded_until_child_exit(stdout, max_output_bytes, stdout_done)
    });
    let stderr_reader = std::thread::spawn(move || {
        read_bounded_until_child_exit(stderr, max_output_bytes, stderr_done)
    });
    let status = match child.wait() {
        Ok(status) => status,
        Err(error) => {
            return ExecResult {
                ok: false,
                value: serde_json::Value::String(format!("wait error: {error}")),
            };
        }
    };
    child_done.store(true, std::sync::atomic::Ordering::Release);
    let (stdout, stdout_truncated) = stdout_reader.join().expect("stdout reader panicked");
    let (stderr, stderr_truncated) = stderr_reader.join().expect("stderr reader panicked");
    let mut stdout = String::from_utf8_lossy(&stdout).into_owned();
    let mut stderr = String::from_utf8_lossy(&stderr).into_owned();
    if stdout_truncated {
        stdout.push_str("\n[... output truncated at byte limit]");
    }
    if stderr_truncated {
        stderr.push_str("\n[... output truncated at byte limit]");
    }
    let exit_code = status.code().unwrap_or(-1);

    let stdout = if let Some(max) = max_lines {
        let lines: Vec<&str> = stdout.lines().collect();
        if lines.len() > max {
            let truncated = lines[..max].join("\n");
            format!(
                "{truncated}\n[... truncated at {max} lines, {} total]",
                lines.len()
            )
        } else {
            stdout
        }
    } else {
        stdout
    };

    ExecResult {
        ok: true,
        value: serde_json::json!({
            "stdout": stdout,
            "stderr": stderr,
            "exit_code": exit_code,
        }),
    }
}

fn read_file_bounded(path: &str, limit: usize) -> std::io::Result<String> {
    let file = std::fs::File::open(path)?;
    let (bytes, truncated) = read_bounded(file, limit);
    let mut output = String::from_utf8_lossy(&bytes).into_owned();
    if truncated {
        output.push_str("\n[... file truncated at byte limit]");
    }
    Ok(output)
}

fn read_bounded(mut reader: impl std::io::Read, limit: usize) -> (Vec<u8>, bool) {
    let mut output = Vec::with_capacity(limit.min(64 * 1024));
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;
    loop {
        let read = match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        let keep = limit.saturating_sub(output.len()).min(read);
        output.extend_from_slice(&buffer[..keep]);
        truncated |= keep < read;
    }
    (output, truncated)
}

#[cfg(unix)]
fn read_bounded_until_child_exit(
    mut reader: impl std::io::Read + std::os::fd::AsRawFd,
    limit: usize,
    child_done: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> (Vec<u8>, bool) {
    let descriptor = reader.as_raw_fd();
    // SAFETY: `descriptor` is owned by `reader` for this function's duration.
    // O_NONBLOCK only changes how this dedicated pipe reader observes an idle
    // descendant after the direct shell child has exited.
    unsafe {
        let flags = libc::fcntl(descriptor, libc::F_GETFL);
        if flags >= 0 {
            libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
    let mut output = Vec::with_capacity(limit.min(64 * 1024));
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                let keep = limit.saturating_sub(output.len()).min(read);
                output.extend_from_slice(&buffer[..keep]);
                truncated |= keep < read;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if child_done.load(std::sync::atomic::Ordering::Acquire) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
            Err(_) => break,
        }
    }
    (output, truncated)
}

#[cfg(not(unix))]
fn read_bounded_until_child_exit(
    reader: impl std::io::Read,
    limit: usize,
    _child_done: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> (Vec<u8>, bool) {
    read_bounded(reader, limit)
}
