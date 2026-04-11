# Unified ToolStore Architecture Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the separate `Tool` trait / `ToolRegistry` / `HostEffects` / `Transport` abstractions with a single `ToolStore` that models all effects — file I/O, shell execution, and LLM completions — as StructFS reads and writes through one unified store.

**Architecture:** The `ToolStore` is a StructFS routing layer that maps paths to tool implementations. It owns three built-in modules — `FsModule` (read/write/edit, sandboxed), `OsModule` (shell, sandboxed), and `CompletionModule` (wrapping `GateStore`, in-process) — plus supports registering arbitrary native tools (in-process closures, JS callbacks) at any path. Sandboxing is an attribute of specific modules (fs, os use `SandboxPolicy`/Clash for OS-level enforcement), not a property of the ToolStore itself. Completions are in-process HTTP calls. JS tools in the browser are in-process callbacks. All are mounted in the same ToolStore, addressed by the same StructFS paths. A `PolicyStore` wrapper enforces clash policy on writes. Each agent gets its own namespace with this shape; sub-agents get their own kernel loop.

**Tech Stack:** Rust (edition 2024), StructFS (`structfs-core-store`, `structfs-serde-store`), ox-path (`oxpath!` macro), existing ox-gate codec layer, existing ox-broker async routing, clash for managing and enforcing sandboxes.

---

## Scope Check

This plan covers three subsystems that must land in order:

1. **ToolStore crate** — the new `ox-tools` crate with `FsModule`, `OsModule`, `CompletionModule`, `TurnStore`, `PolicyStore`, and unified schema aggregation.
2. **Kernel refactor** — strip `Tool`/`ToolRegistry`/`run_turn` from ox-kernel, replace with pure `step()` loop over `tools/turn/`.
3. **Integration** — rewire ox-runtime `HostStore`, ox-cli `agents.rs`, ox-web `OxAgent`, and ox-core `Agent` to use the new ToolStore.

Each subsystem produces working, testable software on its own. Subsystem 1 is standalone. Subsystem 2 depends on 1. Subsystem 3 depends on 2.

---

## File Structure

### New files (ox-tools crate)

| File | Responsibility |
|------|---------------|
| `crates/ox-tools/Cargo.toml` | Crate manifest — depends on ox-kernel, ox-gate, ox-path, structfs-core-store, structfs-serde-store, serde, serde_json |
| `crates/ox-tools/src/lib.rs` | `ToolStore` struct (Reader + Writer), internal module routing, schema aggregation |
| `crates/ox-tools/src/sandbox.rs` | `SandboxedExec` — spawns ephemeral subprocess per operation, delegates sandbox enforcement to Clash via `SandboxPolicy` trait |
| `crates/ox-tools/src/fs.rs` | `FsModule` — read, write, edit operations, each executed in a fresh sandboxed subprocess via `SandboxedExec` |
| `crates/ox-tools/src/os.rs` | `OsModule` — shell execution in a fresh sandboxed subprocess via `SandboxedExec` |
| `crates/ox-tools/src/completion.rs` | `CompletionModule` — wraps `GateStore`, accepts context specs, synthesizes prompts via codecs |
| `crates/ox-tools/src/turn.rs` | `TurnStore` — batches pending effects, dispatches execution, collects results |
| `crates/ox-tools/src/policy_store.rs` | `PolicyStore<S>` — policy wrapper implementing Reader + Writer, delegates to inner store after clash check |
| `crates/ox-tools/src/name_map.rs` | Bidirectional mapping between StructFS paths (`fs/read`) and model-legal wire names (`read_file`) |

### Modified files

| File | Change |
|------|--------|
| `Cargo.toml` (workspace) | Add `crates/ox-tools` to members |
| `crates/ox-kernel/src/lib.rs` | Remove `Tool` trait, `FnTool`, `ToolRegistry`, `run_turn`. Add `step()` method returning effects. Keep `CompletionRequest`, `StreamEvent`, `ContentBlock`, `ToolCall`, `ToolResult`, `ToolSchema`, `AgentEvent`, `Kernel` state machine. |
| `crates/ox-core/src/lib.rs` | Rewrite `Agent` to use `ToolStore` instead of `ToolRegistry` + `SendFn` |
| `crates/ox-runtime/src/host_store.rs` | Replace `gate/complete` + `tools/execute` intercepts with single `tools/` prefix routing to ToolStore |
| `crates/ox-cli/src/tools.rs` | Remove — functionality moves to `ox-tools/src/fs.rs` and `ox-tools/src/os.rs` |
| `crates/ox-cli/src/agents.rs` | Replace `execute_tool()` + `ToolRegistry` with ToolStore mounted in broker |
| `crates/ox-web/src/lib.rs` | Replace dual Rust/JS tool dispatch with ToolStore |
| `crates/ox-gate/src/tools.rs` | Remove `completion_tool` factory and `SendFn` — completions become store writes |
| `crates/ox-gate/src/lib.rs` | `GateStore` keeps provider/account/config/codec infrastructure, removes `create_completion_tools()` |
| `crates/ox-context/src/lib.rs` | Remove `ToolsProvider` — schemas now served by `ToolStore` at `tools/schemas`. Keep `Namespace`, `SystemProvider`. Simplify `synthesize_prompt` to not read `tools/schemas` (that becomes the ToolStore's job when synthesizing context into prompts). |

---

## Subsystem 1: ox-tools Crate

### Task 1: Create ox-tools crate skeleton with SandboxedExec and FsModule

**Files:**
- Create: `crates/ox-tools/Cargo.toml`
- Create: `crates/ox-tools/src/lib.rs`
- Create: `crates/ox-tools/src/sandbox.rs`
- Create: `crates/ox-tools/src/fs.rs`
- Modify: `Cargo.toml` (workspace root, add member)

**Key design principle:** Every filesystem operation (read, write, edit) spawns a fresh subprocess with an ephemeral, per-call sandbox profile. The subprocess does exactly one operation and exits. No process reuse, no shared state between calls. The sandbox profile is generated from the operation type + target path:
- `fs/read` of `src/lib.rs` → subprocess can read `src/lib.rs`, nothing else
- `fs/write` of `tests/new.rs` → subprocess can write `tests/new.rs`, nothing else
- `fs/edit` of `src/main.rs` → subprocess can read+write `src/main.rs`, nothing else

`SandboxedExec` is the shared abstraction. It takes an operation descriptor (what to do, what path constraints to apply), generates a platform-appropriate sandbox profile, spawns the subprocess, collects the result, and returns. On macOS this uses `sandbox-exec` profiles. On Linux, `landlock` or `seccomp-bpf`. The implementer should start with macOS `sandbox-exec` since that's the development platform (Darwin 24.6.0), with a fallback to unsandboxed execution on platforms where sandboxing isn't available yet (with a tracing warning).

The subprocess itself is a thin helper that reads a JSON command from stdin, performs the single fs operation via `std::fs`, writes a JSON result to stdout, and exits. This helper can be the ox binary itself invoked with a `--tool-exec` flag, or a separate minimal binary — the implementer should choose based on what's simplest.

- [ ] **Step 1: Write failing test for FsModule read operation**

In `crates/ox-tools/src/fs.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::sandbox::PermissivePolicy;
    use std::io::Write as IoWrite;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tempfile::TempDir;

    fn executor_bin() -> PathBuf {
        PathBuf::from(env!("CARGO_BIN_EXE_ox-tool-exec"))
    }

    #[test]
    fn read_returns_file_content() {
        let dir = TempDir::new().unwrap();
        let file_path = dir.path().join("hello.txt");
        std::fs::write(&file_path, "hello world").unwrap();

        let mut module = FsModule::new(dir.path().to_path_buf(), executor_bin(), Arc::new(PermissivePolicy));
        let input = Value::Map(BTreeMap::from([(
            "path".into(),
            Value::String("hello.txt".into()),
        )]));
        let result = module.execute("read", &input).unwrap();

        let content = match &result {
            Value::Map(m) => m.get("content").and_then(|v| match v {
                Value::String(s) => Some(s.as_str()),
                _ => None,
            }),
            _ => None,
        };
        assert_eq!(content, Some("hello world"));
    }

    #[test]
    fn read_rejects_path_escape() {
        let dir = TempDir::new().unwrap();
        let mut module = FsModule::new(dir.path().to_path_buf(), executor_bin(), Arc::new(PermissivePolicy));
        let input = Value::Map(BTreeMap::from([(
            "path".into(),
            Value::String("../../etc/passwd".into()),
        )]));
        let result = module.execute("read", &input);
        assert!(result.is_err());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ox-tools -- fs::tests::read_returns_file_content`
Expected: compilation error — `ox-tools` crate doesn't exist yet.

- [ ] **Step 3: Create crate manifest and workspace entry**

`crates/ox-tools/Cargo.toml`:
```toml
[package]
name = "ox-tools"
version = "0.1.0"
edition.workspace = true
rust-version.workspace = true
license.workspace = true

[dependencies]
ox-kernel = { path = "../ox-kernel" }
ox-gate = { path = "../ox-gate" }
ox-path = { path = "../ox-path" }
structfs-core-store.workspace = true
structfs-serde-store.workspace = true
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tracing.workspace = true

[dev-dependencies]
tempfile = "3"
```

Add to workspace `Cargo.toml` members:
```toml
"crates/ox-tools",
```

- [ ] **Step 4: Write SandboxedExec**

`crates/ox-tools/src/sandbox.rs`:
```rust
//! Ephemeral sandboxed subprocess execution.
//!
//! Each tool operation spawns a fresh subprocess. Sandbox enforcement
//! is delegated to Clash via the `SandboxPolicy` trait — ox-tools
//! declares what access is needed, Clash decides how to enforce it
//! at the OS level (sandbox-exec on macOS, landlock on Linux, etc.).
//!
//! ox-tools never generates sandbox profiles directly.

use std::path::{Path, PathBuf};
use std::process::Command;
use structfs_core_store::Value;

/// Describes what access a single operation needs.
///
/// This is the contract between ox-tools and Clash. ox-tools declares
/// intent ("I need to read this file"), Clash translates that into
/// OS-level enforcement.
#[derive(Debug, Clone)]
pub enum AccessIntent {
    /// Read a single file.
    ReadFile(PathBuf),
    /// Write a single file (may create parent dirs).
    WriteFile(PathBuf),
    /// Read and write a single file (for edit).
    ReadWriteFile(PathBuf),
    /// Shell command — needs broader access to workspace.
    ShellInWorkspace(PathBuf),
}

/// Clash's interface for sandboxing a subprocess.
///
/// Clash implements this trait. It decides how to enforce the access
/// intent at the OS level — which sandbox mechanism, which profile,
/// which restrictions. ox-tools doesn't know or care about the
/// enforcement mechanism.
pub trait SandboxPolicy: Send + Sync {
    /// Wrap a `Command` with appropriate sandbox enforcement for the
    /// given access intent. Returns the modified command ready to spawn.
    ///
    /// Clash may:
    /// - Prepend `sandbox-exec -p <profile>` on macOS
    /// - Apply landlock restrictions on Linux
    /// - Do nothing (permissive mode / unsupported platform)
    /// - Deny entirely (return Err)
    fn apply(&self, intent: &AccessIntent, cmd: Command) -> Result<Command, String>;
}

/// A permissive policy that applies no sandboxing.
/// Used in tests and when `--no-policy` is set.
pub struct PermissivePolicy;

impl SandboxPolicy for PermissivePolicy {
    fn apply(&self, _intent: &AccessIntent, cmd: Command) -> Result<Command, String> {
        Ok(cmd)
    }
}

/// A JSON command sent to the tool executor subprocess.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExecCommand {
    pub op: String,
    pub args: serde_json::Value,
}

/// A JSON result returned from the tool executor subprocess.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExecResult {
    pub ok: bool,
    pub value: serde_json::Value,
}

/// Execute a single operation in a fresh sandboxed subprocess.
///
/// Builds the base command, asks Clash (via `SandboxPolicy`) to apply
/// enforcement, spawns the subprocess, sends JSON on stdin, reads
/// JSON result from stdout. The subprocess exits after the single op.
pub fn sandboxed_exec(
    intent: &AccessIntent,
    command: &ExecCommand,
    executor_bin: &Path,
    policy: &dyn SandboxPolicy,
) -> Result<Value, String> {
    let base_cmd = {
        let mut c = Command::new(executor_bin);
        c.arg("--tool-exec");
        c
    };

    // Clash decides how to sandbox this command
    let mut cmd = policy.apply(intent, base_cmd)?;

    let input_json = serde_json::to_string(command).map_err(|e| e.to_string())?;

    let output = cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            if let Some(ref mut stdin) = child.stdin {
                stdin.write_all(input_json.as_bytes())?;
            }
            drop(child.stdin.take());
            child.wait_with_output()
        })
        .map_err(|e| format!("subprocess spawn failed: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("subprocess exited {}: {stderr}", output.status));
    }

    let result: ExecResult = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("failed to parse subprocess output: {e}"))?;

    if result.ok {
        Ok(structfs_serde_store::json_to_value(result.value))
    } else {
        Err(result.value.as_str().unwrap_or("unknown error").to_string())
    }
}
```

ox-tools declares `AccessIntent` (what it needs) and `SandboxPolicy` (the trait Clash implements). ox-tools never generates sandbox profiles, never references `sandbox-exec` or `landlock`, never knows what platform it's on. Clash owns all of that.

- [ ] **Step 5: Write FsModule implementation using SandboxedExec**

`crates/ox-tools/src/fs.rs`:
```rust
//! Filesystem tool module — read, write, edit as a single unit.
//!
//! Each operation spawns a fresh sandboxed subprocess via SandboxedExec.
//! The subprocess does exactly one fs operation and exits. No process
//! reuse between calls. Clash enforces per-call, per-path sandboxing.

use crate::sandbox::{AccessIntent, ExecCommand, SandboxPolicy, sandboxed_exec};
use crate::ToolSchemaEntry;
use std::path::{Path as FsPath, PathBuf};
use std::sync::Arc;
use structfs_core_store::Value;

/// Filesystem operations module.
///
/// Mounted inside the ToolStore at `fs/`. Handles `read`, `write`, `edit`.
/// Each operation is executed in a fresh sandboxed subprocess. Sandbox
/// enforcement is delegated to Clash via `SandboxPolicy`.
pub struct FsModule {
    workspace: PathBuf,
    executor_bin: PathBuf,
    policy: Arc<dyn SandboxPolicy>,
}

impl FsModule {
    pub fn new(workspace: PathBuf, executor_bin: PathBuf, policy: Arc<dyn SandboxPolicy>) -> Self {
        Self { workspace, executor_bin, policy }
    }

    /// Execute a filesystem operation by sub-path name.
    pub fn execute(&mut self, op: &str, input: &Value) -> Result<Value, String> {
        let file_path = self.resolve_input_path(input)?;
        let intent = match op {
            "read" => AccessIntent::ReadFile(file_path.clone()),
            "write" => AccessIntent::WriteFile(file_path.clone()),
            "edit" => AccessIntent::ReadWriteFile(file_path.clone()),
            _ => return Err(format!("unknown fs operation: {op}")),
        };

        let input_json = structfs_serde_store::value_to_json(input.clone());
        let command = ExecCommand {
            op: format!("fs/{op}"),
            args: input_json,
        };

        sandboxed_exec(&intent, &command, &self.executor_bin, &*self.policy)
    }

    /// Tool schemas for model consumption.
    pub fn schemas(&self) -> Vec<ToolSchemaEntry> {
        vec![
            ToolSchemaEntry {
                wire_name: "read_file".into(),
                internal_path: "fs/read".into(),
                description: "Read the contents of a file".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative path to the file" }
                    },
                    "required": ["path"]
                }),
            },
            ToolSchemaEntry {
                wire_name: "write_file".into(),
                internal_path: "fs/write".into(),
                description: "Write content to a file".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative path to the file" },
                        "content": { "type": "string", "description": "Content to write" }
                    },
                    "required": ["path", "content"]
                }),
            },
            ToolSchemaEntry {
                wire_name: "edit_file".into(),
                internal_path: "fs/edit".into(),
                description: "Replace a string in a file".into(),
                input_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string", "description": "Relative path to the file" },
                        "old_string": { "type": "string", "description": "Text to find" },
                        "new_string": { "type": "string", "description": "Replacement text" },
                        "line_start": { "type": "integer", "description": "Optional line hint for disambiguation" }
                    },
                    "required": ["path", "old_string", "new_string"]
                }),
            },
        ]
    }

    // -- internals --

    fn resolve_input_path(&self, input: &Value) -> Result<PathBuf, String> {
        let relative = match input {
            Value::Map(m) => m
                .get("path")
                .and_then(|v| match v {
                    Value::String(s) => Some(s.as_str()),
                    _ => None,
                })
                .ok_or("missing 'path' in input")?,
            _ => return Err("expected map input with 'path' field".into()),
        };
        resolve_path(&self.workspace, relative)
    }
}

/// Resolve a relative path against the workspace, rejecting escapes.
pub fn resolve_path(workspace: &FsPath, relative: &str) -> Result<PathBuf, String> {
    let candidate = workspace.join(relative);
    let resolved = candidate
        .canonicalize()
        .or_else(|_| {
            // File may not exist yet (write/edit create). Canonicalize parent.
            if let Some(parent) = candidate.parent() {
                let canon_parent = parent.canonicalize().map_err(|e| e.to_string())?;
                Ok(canon_parent.join(candidate.file_name().unwrap_or_default()))
            } else {
                Err(format!("cannot resolve path: {relative}"))
            }
        })
        .map_err(|e: String| e)?;

    let workspace_canon = workspace
        .canonicalize()
        .map_err(|e| format!("workspace canonicalize: {e}"))?;
    if !resolved.starts_with(&workspace_canon) {
        return Err(format!("path escapes workspace: {relative}"));
    }
    Ok(resolved)
}
```

`crates/ox-tools/src/lib.rs`:
```rust
//! Unified tool store for the ox framework.
//!
//! All agent effects — filesystem I/O, shell execution, LLM completions —
//! are modeled as StructFS reads and writes through one `ToolStore`.
//!
//! Filesystem and shell operations execute in fresh sandboxed subprocesses.
//! Each call gets an ephemeral sandbox profile scoped to exactly the
//! resources that single operation needs. No process reuse between calls.

pub mod fs;
pub mod sandbox;
```

Note: The tool executor subprocess (invoked with `--tool-exec`) reads a JSON `ExecCommand` from stdin and performs the actual fs operation. This is a separate binary concern — it can be a `--tool-exec` subcommand of ox-cli or a standalone binary. The implementer should create a minimal executor that handles `fs/read`, `fs/write`, `fs/edit` operations. The executor itself uses `std::fs` directly — it's the _only_ thing that touches the filesystem, and it runs inside the sandbox.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p ox-tools`
Expected: All `fs::tests::*` pass.

- [ ] **Step 6: Write additional FsModule tests**

Add to `crates/ox-tools/src/fs.rs` `tests` module:

```rust
    #[test]
    fn write_creates_file() {
        let dir = TempDir::new().unwrap();
        let mut module = FsModule::new(dir.path().to_path_buf(), executor_bin(), Arc::new(PermissivePolicy));
        let input = Value::Map(BTreeMap::from([
            ("path".into(), Value::String("new.txt".into())),
            ("content".into(), Value::String("new content".into())),
        ]));
        let result = module.execute("write", &input).unwrap();
        assert!(matches!(result, Value::Map(_)));

        let on_disk = std::fs::read_to_string(dir.path().join("new.txt")).unwrap();
        assert_eq!(on_disk, "new content");
    }

    #[test]
    fn write_creates_parent_dirs() {
        let dir = TempDir::new().unwrap();
        let mut module = FsModule::new(dir.path().to_path_buf(), executor_bin(), Arc::new(PermissivePolicy));
        let input = Value::Map(BTreeMap::from([
            ("path".into(), Value::String("a/b/c.txt".into())),
            ("content".into(), Value::String("nested".into())),
        ]));
        module.execute("write", &input).unwrap();
        let on_disk = std::fs::read_to_string(dir.path().join("a/b/c.txt")).unwrap();
        assert_eq!(on_disk, "nested");
    }

    #[test]
    fn edit_replaces_unique_string() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("edit.txt");
        std::fs::write(&file, "hello world").unwrap();

        let mut module = FsModule::new(dir.path().to_path_buf(), executor_bin(), Arc::new(PermissivePolicy));
        let input = Value::Map(BTreeMap::from([
            ("path".into(), Value::String("edit.txt".into())),
            ("old_string".into(), Value::String("world".into())),
            ("new_string".into(), Value::String("rust".into())),
        ]));
        module.execute("edit", &input).unwrap();

        let on_disk = std::fs::read_to_string(&file).unwrap();
        assert_eq!(on_disk, "hello rust");
    }

    #[test]
    fn edit_rejects_ambiguous_without_line_hint() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("dup.txt");
        std::fs::write(&file, "aaa\naaa\naaa").unwrap();

        let mut module = FsModule::new(dir.path().to_path_buf(), executor_bin(), Arc::new(PermissivePolicy));
        let input = Value::Map(BTreeMap::from([
            ("path".into(), Value::String("dup.txt".into())),
            ("old_string".into(), Value::String("aaa".into())),
            ("new_string".into(), Value::String("bbb".into())),
        ]));
        let result = module.execute("edit", &input);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("3 times"));
    }

    #[test]
    fn edit_uses_line_start_hint() {
        let dir = TempDir::new().unwrap();
        let file = dir.path().join("hint.txt");
        std::fs::write(&file, "aaa\naaa\naaa").unwrap();

        let mut module = FsModule::new(dir.path().to_path_buf(), executor_bin(), Arc::new(PermissivePolicy));
        let input = Value::Map(BTreeMap::from([
            ("path".into(), Value::String("hint.txt".into())),
            ("old_string".into(), Value::String("aaa".into())),
            ("new_string".into(), Value::String("bbb".into())),
            ("line_start".into(), Value::Integer(2)),
        ]));
        module.execute("edit", &input).unwrap();

        let on_disk = std::fs::read_to_string(&file).unwrap();
        assert_eq!(on_disk, "aaa\nbbb\naaa");
    }

    #[test]
    fn schemas_returns_three_entries() {
        let module = FsModule::new(PathBuf::from("/tmp"), executor_bin(), Arc::new(PermissivePolicy));
        let schemas = module.schemas();
        assert_eq!(schemas.len(), 3);
        let names: Vec<&str> = schemas.iter().map(|s| s.wire_name.as_str()).collect();
        assert!(names.contains(&"read_file"));
        assert!(names.contains(&"write_file"));
        assert!(names.contains(&"edit_file"));
    }
```

- [ ] **Step 7: Run all tests**

Run: `cargo test -p ox-tools`
Expected: All pass.

- [ ] **Step 8: Commit**

```bash
git add crates/ox-tools/ Cargo.toml
git commit -m "$(cat <<'EOF'
feat(ox-tools): add FsModule with unified read/write/edit

Single module for all filesystem tool operations with shared
path sandboxing. Structured Value in/out instead of String.
EOF
)"
```

---

### Task 2: Add OsModule (shell execution via SandboxedExec)

**Files:**
- Create: `crates/ox-tools/src/os.rs`
- Modify: `crates/ox-tools/src/lib.rs` (add `pub mod os`)

Shell commands also run in sandboxed subprocesses via `SandboxedExec`. The sandbox profile for shell is broader than for fs ops — it grants read/write access to the workspace, process spawning, and potentially network access. But it's still ephemeral and per-call.

- [ ] **Step 1: Write failing test for OsModule**

In `crates/ox-tools/src/os.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn executor_bin() -> PathBuf {
        // Use the test binary or a mock executor for unit tests.
        // Integration tests should use the real ox binary.
        PathBuf::from(env!("CARGO_BIN_EXE_ox-tool-exec"))
    }

    #[test]
    fn shell_returns_structured_output() {
        let dir = TempDir::new().unwrap();
        let mut module = OsModule::new(dir.path().to_path_buf(), executor_bin(), Arc::new(PermissivePolicy));
        let input = Value::Map(BTreeMap::from([(
            "command".into(),
            Value::String("echo hello".into()),
        )]));
        let result = module.execute("shell", &input).unwrap();

        let map = match &result {
            Value::Map(m) => m,
            _ => panic!("expected map"),
        };
        assert_eq!(
            map.get("stdout"),
            Some(&Value::String("hello\n".into()))
        );
        assert_eq!(map.get("exit_code"), Some(&Value::Integer(0)));
    }

    #[test]
    fn shell_captures_stderr() {
        let dir = TempDir::new().unwrap();
        let mut module = OsModule::new(dir.path().to_path_buf(), executor_bin(), Arc::new(PermissivePolicy));
        let input = Value::Map(BTreeMap::from([(
            "command".into(),
            Value::String("echo err >&2".into()),
        )]));
        let result = module.execute("shell", &input).unwrap();
        let map = match &result {
            Value::Map(m) => m,
            _ => panic!("expected map"),
        };
        assert_eq!(
            map.get("stderr"),
            Some(&Value::String("err\n".into()))
        );
    }

    #[test]
    fn shell_returns_nonzero_exit_code() {
        let dir = TempDir::new().unwrap();
        let mut module = OsModule::new(dir.path().to_path_buf(), executor_bin(), Arc::new(PermissivePolicy));
        let input = Value::Map(BTreeMap::from([(
            "command".into(),
            Value::String("exit 42".into()),
        )]));
        let result = module.execute("shell", &input).unwrap();
        let map = match &result {
            Value::Map(m) => m,
            _ => panic!("expected map"),
        };
        assert_eq!(map.get("exit_code"), Some(&Value::Integer(42)));
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ox-tools -- os::tests`
Expected: compilation error.

- [ ] **Step 3: Write OsModule implementation using SandboxedExec**

`crates/ox-tools/src/os.rs`:
```rust
//! OS interaction module — shell execution in sandboxed subprocesses.

use crate::sandbox::{AccessIntent, ExecCommand, SandboxPolicy, sandboxed_exec};
use crate::ToolSchemaEntry;
use std::path::PathBuf;
use std::sync::Arc;
use structfs_core_store::Value;

/// OS operations module.
///
/// Mounted inside the ToolStore at `os/`. Currently handles `shell`.
/// Each command runs in a fresh sandboxed subprocess. Sandbox
/// enforcement is delegated to Clash via `SandboxPolicy`.
pub struct OsModule {
    workspace: PathBuf,
    executor_bin: PathBuf,
    policy: Arc<dyn SandboxPolicy>,
}

impl OsModule {
    pub fn new(workspace: PathBuf, executor_bin: PathBuf, policy: Arc<dyn SandboxPolicy>) -> Self {
        Self { workspace, executor_bin, policy }
    }

    pub fn execute(&mut self, op: &str, input: &Value) -> Result<Value, String> {
        match op {
            "shell" => self.run_shell(input),
            _ => Err(format!("unknown os operation: {op}")),
        }
    }

    pub fn schemas(&self) -> Vec<ToolSchemaEntry> {
        vec![ToolSchemaEntry {
            wire_name: "shell".into(),
            internal_path: "os/shell".into(),
            description: "Run a shell command in the workspace directory".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Shell command to execute" }
                },
                "required": ["command"]
            }),
        }]
    }

    fn run_shell(&self, input: &Value) -> Result<Value, String> {
        let intent = AccessIntent::ShellInWorkspace(self.workspace.clone());
        let input_json = structfs_serde_store::value_to_json(input.clone());
        let command = ExecCommand {
            op: "os/shell".into(),
            args: input_json,
        };
        sandboxed_exec(&intent, &command, &self.executor_bin, &*self.policy)
    }
}
```

Update `crates/ox-tools/src/lib.rs`:
```rust
pub mod fs;
pub mod os;
pub mod sandbox;
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p ox-tools`
Expected: All pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-tools/src/os.rs crates/ox-tools/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(ox-tools): add OsModule with sandboxed shell execution

Shell commands run in fresh sandboxed subprocesses via SandboxedExec.
Returns { stdout, stderr, exit_code } as structured Value.
EOF
)"
```

---

### Task 3: Add name_map and ToolSchemaEntry to shared types

**Files:**
- Create: `crates/ox-tools/src/name_map.rs`
- Modify: `crates/ox-tools/src/lib.rs`
- Modify: `crates/ox-tools/src/fs.rs` (move `ToolSchemaEntry` to lib.rs)

- [ ] **Step 1: Write failing test for NameMap**

In `crates/ox-tools/src/name_map.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_wire_to_internal() {
        let mut map = NameMap::new();
        map.register("read_file", "fs/read");
        map.register("shell", "os/shell");

        assert_eq!(map.to_internal("read_file"), Some("fs/read"));
        assert_eq!(map.to_wire("fs/read"), Some("read_file"));
        assert_eq!(map.to_internal("unknown"), None);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ox-tools -- name_map::tests`
Expected: compilation error.

- [ ] **Step 3: Implement NameMap**

`crates/ox-tools/src/name_map.rs`:
```rust
//! Bidirectional mapping between model wire names and internal StructFS paths.

use std::collections::HashMap;

/// Maps between flat wire names (for models) and hierarchical internal paths.
pub struct NameMap {
    wire_to_internal: HashMap<String, String>,
    internal_to_wire: HashMap<String, String>,
}

impl NameMap {
    pub fn new() -> Self {
        Self {
            wire_to_internal: HashMap::new(),
            internal_to_wire: HashMap::new(),
        }
    }

    pub fn register(&mut self, wire_name: &str, internal_path: &str) {
        self.wire_to_internal
            .insert(wire_name.to_string(), internal_path.to_string());
        self.internal_to_wire
            .insert(internal_path.to_string(), wire_name.to_string());
    }

    pub fn to_internal(&self, wire_name: &str) -> Option<&str> {
        self.wire_to_internal.get(wire_name).map(|s| s.as_str())
    }

    pub fn to_wire(&self, internal_path: &str) -> Option<&str> {
        self.internal_to_wire.get(internal_path).map(|s| s.as_str())
    }
}
```

Move `ToolSchemaEntry` from `fs.rs` to `lib.rs` so all modules can use it:

In `crates/ox-tools/src/lib.rs`:
```rust
//! Unified tool store for the ox framework.

pub mod fs;
pub mod name_map;
pub mod os;

/// Schema entry for a single tool operation.
///
/// Maps a model-facing wire name to an internal StructFS path,
/// with the JSON Schema the model sees.
pub struct ToolSchemaEntry {
    pub wire_name: String,
    pub internal_path: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}
```

Update `fs.rs` and `os.rs` to use `crate::ToolSchemaEntry` instead of the local one.

- [ ] **Step 4: Run tests**

Run: `cargo test -p ox-tools`
Expected: All pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-tools/src/name_map.rs crates/ox-tools/src/lib.rs crates/ox-tools/src/fs.rs crates/ox-tools/src/os.rs
git commit -m "$(cat <<'EOF'
feat(ox-tools): add NameMap for wire name ↔ internal path mapping

Shared ToolSchemaEntry type moved to lib.rs for cross-module use.
EOF
)"
```

---

### Task 4: Add CompletionModule wrapping GateStore

**Files:**
- Create: `crates/ox-tools/src/completion.rs`
- Modify: `crates/ox-tools/src/lib.rs`

The CompletionModule wraps `GateStore` and accepts context specs (sets of StructFS paths to include) rather than pre-built prompts. It resolves context by reading from a namespace reference, then synthesizes a dialect-specific prompt via ox-gate codecs.

- [ ] **Step 1: Write failing test for CompletionModule context resolution**

In `crates/ox-tools/src/completion.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ox_gate::GateStore;

    #[test]
    fn schemas_returns_entry_per_keyed_account() {
        let mut gate = GateStore::new();
        // No keys set — no completion schemas
        let module = CompletionModule::new(gate);
        let schemas = module.schemas();
        assert!(schemas.is_empty());
    }

    #[test]
    fn gate_store_accessible_by_path() {
        let gate = GateStore::new();
        let mut module = CompletionModule::new(gate);
        // Read defaults through the module
        let result = module.read_gate("defaults/account");
        assert!(result.is_some());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ox-tools -- completion::tests`
Expected: compilation error.

- [ ] **Step 3: Implement CompletionModule**

`crates/ox-tools/src/completion.rs`:
```rust
//! Completion module — wraps GateStore for context-based prompt synthesis.
//!
//! Accepts structured context specs, resolves them against the agent's
//! namespace, and synthesizes dialect-specific prompts via ox-gate codecs.

use crate::ToolSchemaEntry;
use ox_gate::GateStore;
use structfs_core_store::{Path, Reader, Record, Value, Writer};

/// Completion module wrapping GateStore.
///
/// Mounted inside the ToolStore at `completions/`. Preserves all
/// GateStore infrastructure: providers, accounts, codecs, config handle,
/// catalogs, snapshots, usage tracking.
pub struct CompletionModule {
    gate: GateStore,
}

impl CompletionModule {
    pub fn new(gate: GateStore) -> Self {
        Self { gate }
    }

    /// Read a sub-path from the underlying GateStore.
    pub fn read_gate(&mut self, sub: &str) -> Option<Value> {
        let path = ox_path::oxpath_from_str(sub).ok()?;
        self.gate
            .read(&path)
            .ok()
            .flatten()
            .and_then(|r| r.as_value().cloned())
    }

    /// Write a sub-path to the underlying GateStore.
    pub fn write_gate(&mut self, sub: &str, value: Value) -> Result<(), String> {
        let path = ox_path::oxpath_from_str(sub).map_err(|e| e.to_string())?;
        self.gate
            .write(&path, Record::parsed(value))
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Tool schemas for completion accounts that have API keys configured.
    pub fn schemas(&self) -> Vec<ToolSchemaEntry> {
        // Delegate to GateStore's existing completion_tool_schemas()
        // and convert to ToolSchemaEntry format
        let mut gate_clone = self.gate_ref_for_schemas();
        gate_clone
            .into_iter()
            .map(|ts| ToolSchemaEntry {
                wire_name: ts.name.clone(),
                internal_path: format!("completions/complete/{}", ts.name.strip_prefix("complete_").unwrap_or(&ts.name)),
                description: ts.description.clone(),
                input_schema: ts.input_schema.clone(),
            })
            .collect()
    }

    fn gate_ref_for_schemas(&self) -> Vec<ox_kernel::ToolSchema> {
        // GateStore::completion_tool_schemas requires &mut self due to
        // config handle reads. We need to work around this.
        // For now, return empty — this will be refined when we integrate
        // context-based synthesis in Task 8.
        vec![]
    }

    /// Get a mutable reference to the inner GateStore.
    pub fn gate_mut(&mut self) -> &mut GateStore {
        &mut self.gate
    }

    /// Get a reference to the inner GateStore.
    pub fn gate(&self) -> &GateStore {
        &self.gate
    }
}

/// StructFS Reader delegation to GateStore.
impl Reader for CompletionModule {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, structfs_core_store::Error> {
        self.gate.read(from)
    }
}

/// StructFS Writer delegation to GateStore.
impl Writer for CompletionModule {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, structfs_core_store::Error> {
        self.gate.write(to, data)
    }
}
```

Note: `ox_path::oxpath_from_str` may need to be checked — if it doesn't exist, use `Path::parse` or equivalent. The implementer should check the ox-path crate's public API.

Update `crates/ox-tools/src/lib.rs` to add `pub mod completion;`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p ox-tools`
Expected: All pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-tools/src/completion.rs crates/ox-tools/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(ox-tools): add CompletionModule wrapping GateStore

Preserves all ox-gate infrastructure (providers, accounts, codecs,
config, catalogs, snapshots). Reader/Writer delegates to GateStore.
EOF
)"
```

---

### Task 5: Build ToolStore with Reader + Writer routing

**Files:**
- Modify: `crates/ox-tools/src/lib.rs`

This is the central struct that routes StructFS reads/writes to the appropriate module.

- [ ] **Step 1: Write failing test for ToolStore routing**

Add to `crates/ox-tools/src/lib.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn executor_bin() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_BIN_EXE_ox-tool-exec"))
    }

    fn make_tool_store(dir: &std::path::Path) -> ToolStore {
        let exec = executor_bin();
        let policy: Arc<dyn sandbox::SandboxPolicy> = Arc::new(sandbox::PermissivePolicy);
        let fs = fs::FsModule::new(dir.to_path_buf(), exec.clone(), policy.clone());
        let os = os::OsModule::new(dir.to_path_buf(), exec, policy);
        let gate = ox_gate::GateStore::new();
        let completion = completion::CompletionModule::new(gate);
        ToolStore::new(fs, os, completion)
    }

    #[test]
    fn routes_fs_read() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("test.txt"), "content").unwrap();

        let mut store = make_tool_store(dir.path());
        let input = Value::Map(std::collections::BTreeMap::from([
            ("path".into(), Value::String("test.txt".into())),
        ]));
        store
            .write(
                &ox_path::oxpath!("fs", "read"),
                Record::parsed(input),
            )
            .unwrap();
        let result = store
            .read(&ox_path::oxpath!("fs", "read", "result"))
            .unwrap();
        assert!(result.is_some());
    }

    #[test]
    fn routes_completions_defaults() {
        let dir = TempDir::new().unwrap();
        let mut store = make_tool_store(dir.path());
        let result = store
            .read(&ox_path::oxpath!("completions", "defaults", "account"))
            .unwrap();
        assert!(result.is_some());
    }

    #[test]
    fn schemas_aggregates_all_modules() {
        let dir = TempDir::new().unwrap();
        let store = make_tool_store(dir.path());
        let schemas = store.all_schemas();
        // fs: read_file, write_file, edit_file + os: shell = 4 minimum
        assert!(schemas.len() >= 4);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ox-tools -- tests`
Expected: compilation error.

- [ ] **Step 3: Implement ToolStore**

In `crates/ox-tools/src/lib.rs`, add the `ToolStore` struct:

```rust
use std::collections::BTreeMap;
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};

pub mod completion;
pub mod fs;
pub mod name_map;
pub mod os;

/// Schema entry for a single tool operation.
pub struct ToolSchemaEntry {
    pub wire_name: String,
    pub internal_path: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// Unified tool store — all agent effects through one StructFS interface.
///
/// Routes reads/writes by first path component to the appropriate module:
/// - `fs/` → FsModule (read, write, edit)
/// - `os/` → OsModule (shell)
/// - `completions/` → CompletionModule (GateStore wrapper)
/// - `schemas` → aggregated tool schemas for model consumption
pub struct ToolStore {
    fs: fs::FsModule,
    os: os::OsModule,
    completions: completion::CompletionModule,
    name_map: name_map::NameMap,
    /// Last tool execution result, keyed by operation.
    /// Cleared on each new write to an execution path.
    last_result: BTreeMap<String, Value>,
}

impl ToolStore {
    pub fn new(
        fs: fs::FsModule,
        os: os::OsModule,
        completions: completion::CompletionModule,
    ) -> Self {
        let mut name_map = name_map::NameMap::new();
        // Register fs tool name mappings
        for schema in fs.schemas() {
            name_map.register(&schema.wire_name, &schema.internal_path);
        }
        // Register os tool name mappings
        for schema in os.schemas() {
            name_map.register(&schema.wire_name, &schema.internal_path);
        }
        // Completion schemas are registered when accounts have keys

        Self {
            fs,
            os,
            completions,
            name_map,
            last_result: BTreeMap::new(),
        }
    }

    /// All tool schemas from all modules.
    pub fn all_schemas(&self) -> Vec<ToolSchemaEntry> {
        let mut schemas = Vec::new();
        schemas.extend(self.fs.schemas());
        schemas.extend(self.os.schemas());
        schemas.extend(self.completions.schemas());
        schemas
    }

    /// Convert tool schemas to ox-kernel ToolSchema format for model consumption.
    pub fn tool_schemas_for_model(&self) -> Vec<ox_kernel::ToolSchema> {
        self.all_schemas()
            .into_iter()
            .map(|entry| ox_kernel::ToolSchema {
                name: entry.wire_name,
                description: entry.description,
                input_schema: entry.input_schema,
            })
            .collect()
    }

    /// Get the name map for wire name ↔ internal path resolution.
    pub fn name_map(&self) -> &name_map::NameMap {
        &self.name_map
    }
}

impl Reader for ToolStore {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        if from.is_empty() {
            return Ok(None);
        }

        let first = from.components[0].as_str();
        let tail = Path::from_components(from.components[1..].to_vec());

        match first {
            "fs" => {
                // Check for result reads: fs/read/result, fs/write/result, etc.
                if tail.components.len() >= 2 && tail.components.last().map(|s| s.as_str()) == Some("result") {
                    let op = tail.components[0].as_str();
                    let key = format!("fs/{op}");
                    return Ok(self.last_result.get(&key).map(|v| Record::parsed(v.clone())));
                }
                Ok(None)
            }
            "os" => {
                if tail.components.len() >= 2 && tail.components.last().map(|s| s.as_str()) == Some("result") {
                    let op = tail.components[0].as_str();
                    let key = format!("os/{op}");
                    return Ok(self.last_result.get(&key).map(|v| Record::parsed(v.clone())));
                }
                Ok(None)
            }
            "completions" => self.completions.read(&tail),
            "schemas" => {
                let schemas = self.tool_schemas_for_model();
                let json_array: Vec<serde_json::Value> = schemas
                    .iter()
                    .map(|s| {
                        serde_json::json!({
                            "name": s.name,
                            "description": s.description,
                            "input_schema": s.input_schema,
                        })
                    })
                    .collect();
                let value = structfs_serde_store::json_to_value(
                    serde_json::Value::Array(json_array),
                );
                Ok(Some(Record::parsed(value)))
            }
            _ => {
                // Try wire name resolution
                if let Some(internal) = self.name_map.to_internal(first) {
                    let parts: Vec<&str> = internal.split('/').collect();
                    if parts.len() == 2 {
                        let key = internal.to_string();
                        return Ok(self.last_result.get(&key).map(|v| Record::parsed(v.clone())));
                    }
                }
                Ok(None)
            }
        }
    }
}

impl Writer for ToolStore {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        if to.is_empty() {
            return Err(StoreError::store("tools", "write", "empty path"));
        }

        let first = to.components[0].as_str();
        let tail = Path::from_components(to.components[1..].to_vec());

        match first {
            "fs" => {
                let op = if tail.is_empty() {
                    return Err(StoreError::store("tools", "fs", "missing operation"));
                } else {
                    tail.components[0].as_str()
                };
                let input = data
                    .as_value()
                    .cloned()
                    .ok_or_else(|| StoreError::store("tools", "fs", "expected parsed record"))?;
                let result = self.fs.execute(op, &input).map_err(|e| {
                    StoreError::store("tools", &format!("fs/{op}"), e)
                })?;
                let key = format!("fs/{op}");
                self.last_result.insert(key, result);
                Ok(to.clone())
            }
            "os" => {
                let op = if tail.is_empty() {
                    return Err(StoreError::store("tools", "os", "missing operation"));
                } else {
                    tail.components[0].as_str()
                };
                let input = data
                    .as_value()
                    .cloned()
                    .ok_or_else(|| StoreError::store("tools", "os", "expected parsed record"))?;
                let result = self.os.execute(op, &input).map_err(|e| {
                    StoreError::store("tools", &format!("os/{op}"), e)
                })?;
                let key = format!("os/{op}");
                self.last_result.insert(key, result);
                Ok(to.clone())
            }
            "completions" => self.completions.write(&tail, data),
            _ => {
                // Try wire name resolution for tool execution
                if let Some(internal) = self.name_map.to_internal(first) {
                    let parts: Vec<&str> = internal.split('/').collect();
                    if parts.len() == 2 {
                        let input = data
                            .as_value()
                            .cloned()
                            .ok_or_else(|| StoreError::store("tools", first, "expected parsed record"))?;
                        let result = match parts[0] {
                            "fs" => self.fs.execute(parts[1], &input),
                            "os" => self.os.execute(parts[1], &input),
                            _ => Err(format!("unknown module: {}", parts[0])),
                        }
                        .map_err(|e| StoreError::store("tools", first, e))?;
                        self.last_result.insert(internal.to_string(), result);
                        return Ok(to.clone());
                    }
                }
                Err(StoreError::store("tools", "write", format!("unknown path: {to}")))
            }
        }
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p ox-tools`
Expected: All pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-tools/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(ox-tools): add ToolStore with Reader/Writer routing

Routes fs/, os/, completions/ to internal modules. Supports
wire name resolution for model-returned tool calls. Aggregates
schemas from all modules.
EOF
)"
```

---

### Task 6: Add PolicyStore policy wrapper

**Files:**
- Create: `crates/ox-tools/src/policy_store.rs`
- Modify: `crates/ox-tools/src/lib.rs`

- [ ] **Step 1: Write failing test for PolicyStore**

In `crates/ox-tools/src/policy_store.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    struct MockStore {
        writes: Vec<String>,
    }

    impl MockStore {
        fn new() -> Self {
            Self { writes: vec![] }
        }
    }

    impl Reader for MockStore {
        fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
            Ok(Some(Record::parsed(Value::String(
                format!("read:{from}"),
            ))))
        }
    }

    impl Writer for MockStore {
        fn write(&mut self, to: &Path, _data: Record) -> Result<Path, StoreError> {
            self.writes.push(to.to_string());
            Ok(to.clone())
        }
    }

    #[test]
    fn allow_policy_passes_through() {
        let store = MockStore::new();
        let policy = |_path: &Path, _data: &Record| -> PolicyDecision {
            PolicyDecision::Allow
        };
        let mut gated = PolicyStore::new(store, policy);

        let result = gated.write(
            &ox_path::oxpath!("fs", "read"),
            Record::parsed(Value::String("test".into())),
        );
        assert!(result.is_ok());
        assert_eq!(gated.inner.writes.len(), 1);
    }

    #[test]
    fn deny_policy_blocks_write() {
        let store = MockStore::new();
        let policy = |_path: &Path, _data: &Record| -> PolicyDecision {
            PolicyDecision::Deny("not allowed".into())
        };
        let mut gated = PolicyStore::new(store, policy);

        let result = gated.write(
            &ox_path::oxpath!("os", "shell"),
            Record::parsed(Value::String("rm -rf /".into())),
        );
        assert!(result.is_err());
        assert_eq!(gated.inner.writes.len(), 0);
    }

    #[test]
    fn reads_pass_through_ungated() {
        let store = MockStore::new();
        let policy = |_path: &Path, _data: &Record| -> PolicyDecision {
            PolicyDecision::Deny("blocked".into())
        };
        let mut gated = PolicyStore::new(store, policy);

        // Reads should always pass through regardless of policy
        let result = gated.read(&ox_path::oxpath!("fs", "read", "result"));
        assert!(result.is_ok());
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ox-tools -- gate::tests`
Expected: compilation error.

- [ ] **Step 3: Implement PolicyStore**

`crates/ox-tools/src/policy_store.rs`:
```rust
//! Policy store wrapper — intercepts writes for clash enforcement.

use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};

/// Policy decision for a write operation.
pub enum PolicyDecision {
    /// Allow the write to proceed.
    Allow,
    /// Deny the write with a reason.
    Deny(String),
    // Ask variant will be added when integrating with the TUI approval flow.
}

/// A store wrapper that enforces policy on writes.
///
/// Reads always pass through. Writes are checked against the policy
/// function before being forwarded to the inner store.
pub struct PolicyStore<S, F>
where
    S: Reader + Writer,
    F: FnMut(&Path, &Record) -> PolicyDecision,
{
    pub inner: S,
    policy: F,
}

impl<S, F> PolicyStore<S, F>
where
    S: Reader + Writer,
    F: FnMut(&Path, &Record) -> PolicyDecision,
{
    pub fn new(inner: S, policy: F) -> Self {
        Self { inner, policy }
    }
}

impl<S, F> Reader for PolicyStore<S, F>
where
    S: Reader + Writer,
    F: FnMut(&Path, &Record) -> PolicyDecision,
{
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        self.inner.read(from)
    }
}

impl<S, F> Writer for PolicyStore<S, F>
where
    S: Reader + Writer,
    F: FnMut(&Path, &Record) -> PolicyDecision,
{
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        match (self.policy)(to, &data) {
            PolicyDecision::Allow => self.inner.write(to, data),
            PolicyDecision::Deny(reason) => {
                Err(StoreError::store("gate", "policy", reason))
            }
        }
    }
}
```

Update `crates/ox-tools/src/lib.rs` to add `pub mod policy_store;`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p ox-tools`
Expected: All pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-tools/src/policy_store.rs crates/ox-tools/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(ox-tools): add PolicyStore policy wrapper

Intercepts writes for clash enforcement. Reads pass through.
PolicyDecision::Allow/Deny for now; Ask variant comes with TUI
integration.
EOF
)"
```

---

### Task 7: Add TurnStore for batched effect execution

**Files:**
- Create: `crates/ox-tools/src/turn.rs`
- Modify: `crates/ox-tools/src/lib.rs`

The TurnStore orchestrates batched effect execution. It reads pending effects (tool calls and completion requests extracted from model responses), dispatches them to the ToolStore, and collects results. The kernel drives this with `turn/pending` reads and `turn/execute` writes.

- [ ] **Step 1: Write failing test for TurnStore**

In `crates/ox-tools/src/turn.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_pending_when_empty() {
        let store = TurnStore::new();
        let pending = store.pending();
        assert!(pending.is_none());
    }

    #[test]
    fn enqueue_and_read_pending() {
        let mut store = TurnStore::new();
        store.enqueue_tool_call("call_1", "read_file", serde_json::json!({"path": "a.rs"}));
        store.enqueue_tool_call("call_2", "shell", serde_json::json!({"command": "ls"}));

        let pending = store.pending().unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].call_id, "call_1");
        assert_eq!(pending[1].call_id, "call_2");
    }

    #[test]
    fn submit_results_clears_pending() {
        let mut store = TurnStore::new();
        store.enqueue_tool_call("call_1", "read_file", serde_json::json!({"path": "a.rs"}));

        let results = vec![EffectOutcome {
            call_id: "call_1".into(),
            result: Ok(Value::String("file content".into())),
        }];
        store.submit_results(results);

        assert!(store.pending().is_none());
        let outcomes = store.take_results();
        assert_eq!(outcomes.len(), 1);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ox-tools -- turn::tests`
Expected: compilation error.

- [ ] **Step 3: Implement TurnStore**

`crates/ox-tools/src/turn.rs`:
```rust
//! Turn store — batched effect orchestration.
//!
//! Manages the pending/execute/results lifecycle for a single turn.
//! The kernel reads `turn/pending`, writes `turn/execute`, reads `turn/results`.

use structfs_core_store::Value;

/// A pending effect — either a tool call or a completion request.
#[derive(Debug, Clone)]
pub struct PendingEffect {
    pub call_id: String,
    pub wire_name: String,
    pub input: serde_json::Value,
}

/// The outcome of executing an effect.
#[derive(Debug, Clone)]
pub struct EffectOutcome {
    pub call_id: String,
    pub result: Result<Value, Value>,
}

/// Manages batched effect lifecycle for a single turn.
pub struct TurnStore {
    pending: Vec<PendingEffect>,
    results: Vec<EffectOutcome>,
}

impl TurnStore {
    pub fn new() -> Self {
        Self {
            pending: Vec::new(),
            results: Vec::new(),
        }
    }

    /// Enqueue a tool call for execution.
    pub fn enqueue_tool_call(&mut self, call_id: &str, wire_name: &str, input: serde_json::Value) {
        self.pending.push(PendingEffect {
            call_id: call_id.into(),
            wire_name: wire_name.into(),
            input,
        });
    }

    /// Read pending effects. Returns None if nothing is pending.
    pub fn pending(&self) -> Option<&[PendingEffect]> {
        if self.pending.is_empty() {
            None
        } else {
            Some(&self.pending)
        }
    }

    /// Submit execution results. Clears the pending queue.
    pub fn submit_results(&mut self, results: Vec<EffectOutcome>) {
        self.pending.clear();
        self.results = results;
    }

    /// Take collected results (drains the results buffer).
    pub fn take_results(&mut self) -> Vec<EffectOutcome> {
        std::mem::take(&mut self.results)
    }

    /// Clear all state for a new turn.
    pub fn clear(&mut self) {
        self.pending.clear();
        self.results.clear();
    }
}

impl Default for TurnStore {
    fn default() -> Self {
        Self::new()
    }
}
```

Update `crates/ox-tools/src/lib.rs` to add `pub mod turn;`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p ox-tools`
Expected: All pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-tools/src/turn.rs crates/ox-tools/src/lib.rs
git commit -m "$(cat <<'EOF'
feat(ox-tools): add TurnStore for batched effect orchestration

Manages pending/execute/results lifecycle. Supports multiple
effects per turn (tool calls and completions in parallel).
EOF
)"
```

---

## Subsystem 2: Kernel Refactor

### Task 8: Remove Tool/ToolRegistry/run_turn from ox-kernel

**Files:**
- Modify: `crates/ox-kernel/src/lib.rs`

This task removes the `Tool` trait, `FnTool`, `ToolRegistry`, and `run_turn` from ox-kernel. The kernel keeps its three-phase state machine (`initiate_completion`, `consume_events`, `complete_turn`) but gains a new `step()` method that returns tool calls as data instead of executing them.

**Important:** This is a breaking change. ox-core, ox-cli, ox-web, ox-runtime, and ox-gate all depend on these types. We deprecate first, then remove in a follow-up once consumers are migrated.

- [ ] **Step 1: Write test for new kernel step() method**

Add to the existing `#[cfg(test)]` module in `crates/ox-kernel/src/lib.rs`:

```rust
    #[test]
    fn complete_turn_returns_tool_calls_as_data() {
        let mut kernel = Kernel::new("test-model".into());
        // complete_turn already returns Vec<ToolCall> — verify it's pure data
        let content = vec![
            ContentBlock::Text { text: "thinking...".into() },
            ContentBlock::ToolUse(ToolCall {
                id: "call_1".into(),
                name: "read_file".into(),
                input: serde_json::json!({"path": "test.rs"}),
            }),
        ];
        // We need a store for complete_turn — use a minimal one
        let mut ns = TestNamespace::new();
        let tool_calls = kernel.complete_turn(&mut ns, &content).unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].name, "read_file");
        // Kernel did NOT execute the tool — it just returned the data
    }
```

Note: The implementer needs to check what test infrastructure already exists in ox-kernel's tests and adapt `TestNamespace` accordingly. The key point is that `complete_turn` returns `Vec<ToolCall>` as pure data.

- [ ] **Step 2: Verify the test passes with current code**

Run: `cargo test -p ox-kernel -- complete_turn_returns_tool_calls_as_data`
Expected: PASS — `complete_turn` already returns data without executing. This confirms the existing API is correct for the new architecture.

- [ ] **Step 3: Deprecate Tool, FnTool, ToolRegistry, run_turn**

In `crates/ox-kernel/src/lib.rs`, add deprecation attributes:

```rust
#[deprecated(since = "0.2.0", note = "Use ox-tools ToolStore instead")]
pub trait Tool: Send + Sync { ... }

#[deprecated(since = "0.2.0", note = "Use ox-tools ToolStore instead")]
pub struct FnTool { ... }

#[deprecated(since = "0.2.0", note = "Use ox-tools ToolStore instead")]
pub struct ToolRegistry { ... }
```

On `Kernel::run_turn`:
```rust
    #[deprecated(since = "0.2.0", note = "Use the three-phase methods with ox-tools TurnStore instead")]
    pub fn run_turn(...) { ... }
```

- [ ] **Step 4: Run full workspace check**

Run: `cargo check --workspace 2>&1 | head -50`
Expected: Deprecation warnings in ox-core, ox-cli, ox-web, ox-runtime, ox-gate. No errors.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-kernel/src/lib.rs
git commit -m "$(cat <<'EOF'
refactor(ox-kernel): deprecate Tool/ToolRegistry/run_turn

These types are superseded by the ox-tools ToolStore which models
all effects as StructFS reads and writes. Consumers should migrate
to the three-phase kernel methods with TurnStore orchestration.
EOF
)"
```

---

### Task 9: Update ToolResult to use structured Value

**Files:**
- Modify: `crates/ox-kernel/src/lib.rs`

Currently `ToolResult.content` is `String`. Change to `serde_json::Value` for structured outcomes.

- [ ] **Step 1: Write failing test**

```rust
    #[test]
    fn tool_result_accepts_structured_value() {
        let result = ToolResult {
            tool_use_id: "call_1".into(),
            content: serde_json::json!({
                "stdout": "hello\n",
                "stderr": "",
                "exit_code": 0,
            }),
        };
        assert!(result.content.is_object());
    }
```

- [ ] **Step 2: Run test — fails because content is String**

Run: `cargo test -p ox-kernel -- tool_result_accepts_structured_value`
Expected: FAIL — type mismatch.

- [ ] **Step 3: Change ToolResult.content from String to serde_json::Value**

In `crates/ox-kernel/src/lib.rs`, change:
```rust
pub struct ToolResult {
    pub tool_use_id: String,
    pub content: serde_json::Value,
}
```

Update `serialize_tool_results` to handle `Value` content:
```rust
pub fn serialize_tool_results(results: &[ToolResult]) -> serde_json::Value {
    let blocks: Vec<serde_json::Value> = results
        .iter()
        .map(|r| {
            let content_str = match &r.content {
                serde_json::Value::String(s) => s.clone(),
                other => serde_json::to_string(other).unwrap_or_default(),
            };
            serde_json::json!({
                "type": "tool_result",
                "tool_use_id": r.tool_use_id,
                "content": content_str,
            })
        })
        .collect();
    serde_json::json!({
        "role": "user",
        "content": blocks,
    })
}
```

Note: The wire format for Anthropic still expects tool results as strings. The serialization flattens structured values to JSON strings for the wire. This is a codec concern — eventually the completion module will handle this translation, but for now we maintain backward compatibility.

- [ ] **Step 4: Fix all compilation errors across workspace**

The implementer must update every site that creates a `ToolResult` to use `serde_json::Value` instead of `String`. Key locations:
- `crates/ox-kernel/src/lib.rs` (run_turn, line ~521)
- `crates/ox-cli/src/agents.rs` (execute_tool)
- `crates/ox-web/src/lib.rs` (run_agentic_loop)
- `crates/ox-runtime/src/host_store.rs` (write_tools_execute)

Each site currently does `content: result_str` — change to `content: serde_json::Value::String(result_str)` for backward compatibility, or `content: serde_json::json!({"output": result_str})` for structured output.

- [ ] **Step 5: Run full workspace tests**

Run: `cargo test --workspace`
Expected: All pass.

- [ ] **Step 6: Commit**

```bash
git add -u
git commit -m "$(cat <<'EOF'
refactor(ox-kernel): change ToolResult.content to serde_json::Value

Enables structured tool outcomes (stdout/stderr/exit_code, file
metadata, etc.) instead of flat strings. Wire serialization still
flattens to string for Anthropic API compatibility.
EOF
)"
```

---

## Subsystem 3: Integration

### Task 10: Wire ToolStore into ox-core Agent

**Files:**
- Modify: `crates/ox-core/Cargo.toml` (add ox-tools dependency)
- Modify: `crates/ox-core/src/lib.rs`

Replace `ToolRegistry` + `SendFn` with `ToolStore`. The `Agent` now takes a `ToolStore` and uses its StructFS interface for tool execution.

- [ ] **Step 1: Write test for new Agent construction**

In `crates/ox-core/src/lib.rs` tests:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_new_with_tool_store() {
        let workspace = std::env::temp_dir();
        let executor = std::path::PathBuf::from(env!("CARGO_BIN_EXE_ox-tool-exec"));
        let policy: std::sync::Arc<dyn ox_tools::sandbox::SandboxPolicy> =
            std::sync::Arc::new(ox_tools::sandbox::PermissivePolicy);
        let fs = ox_tools::fs::FsModule::new(workspace.clone(), executor.clone(), policy.clone());
        let os = ox_tools::os::OsModule::new(workspace, executor, policy);
        let gate = ox_gate::GateStore::new();
        let completion = ox_tools::completion::CompletionModule::new(gate);
        let tool_store = ox_tools::ToolStore::new(fs, os, completion);

        let agent = Agent::with_tool_store(
            "You are helpful.".into(),
            tool_store,
        );
        // Agent should have been created successfully
        // (We can't easily test prompt() without a real LLM)
    }
}
```

- [ ] **Step 2: Run test — fails**

Run: `cargo test -p ox-core`
Expected: FAIL — `Agent::with_tool_store` doesn't exist.

- [ ] **Step 3: Implement Agent::with_tool_store**

Add to `crates/ox-core/src/lib.rs`:

```rust
impl Agent {
    /// Create an agent using the unified ToolStore.
    ///
    /// The ToolStore provides all tool execution (fs, os, completions)
    /// through a single StructFS interface. The agent reads model/account
    /// defaults from the ToolStore's completion module.
    pub fn with_tool_store(
        system_prompt: String,
        tool_store: ox_tools::ToolStore,
    ) -> Self {
        let schemas = tool_store.tool_schemas_for_model();

        let mut context = Namespace::new();
        context.mount("system", Box::new(SystemProvider::new(system_prompt)));
        context.mount("history", Box::new(HistoryProvider::new()));
        context.mount("tools", Box::new(tool_store));

        Self {
            kernel: Kernel::new("default".into()),
            context,
            tools: ToolRegistry::new(), // deprecated, kept for backward compat
            send: Arc::new(|_| Err("use ToolStore completions".into())),
            subscribers: Vec::new(),
        }
    }
}
```

Note: This is a transitional API. The `tools` and `send` fields are deprecated and will be removed when `run_turn` is removed. The new `prompt()` method should use the three-phase kernel methods with the ToolStore for execution.

Add `ox-tools` to `crates/ox-core/Cargo.toml`:
```toml
ox-tools = { path = "../ox-tools" }
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p ox-core`
Expected: All pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-core/
git commit -m "$(cat <<'EOF'
feat(ox-core): add Agent::with_tool_store constructor

Transitional API that takes a unified ToolStore instead of
ToolRegistry + SendFn. ToolStore mounted at 'tools/' in namespace.
EOF
)"
```

---

### Task 11: Wire ToolStore into ox-runtime HostStore

**Files:**
- Modify: `crates/ox-runtime/Cargo.toml` (add ox-tools dependency)
- Modify: `crates/ox-runtime/src/host_store.rs`

Replace the `gate/complete` + `tools/execute` intercept paths with unified routing through `tools/` to the ToolStore. The `HostEffects` trait narrows to just `emit_event` — completion and tool execution are now store operations.

- [ ] **Step 1: Write test for new HostStore routing**

Add to `crates/ox-runtime/src/host_store.rs` tests:

```rust
    #[test]
    fn write_to_tools_fs_read_executes_via_store() {
        let ns = make_namespace();
        // Create a HostStore that routes tools/* to ToolStore
        let mut store = HostStore::new(ns, MockEffects::new());

        // Write a file so we can read it
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("test.txt"), "hello").unwrap();

        // This test validates that the new routing works.
        // The actual implementation will depend on how ToolStore
        // is integrated into the HostStore's backend namespace.
    }
```

Note: This task requires careful integration with the existing `HostStore` intercept mechanism. The implementer should:

1. Replace `HostEffects::complete()` with a write to `tools/completions/complete/{account}`
2. Replace `HostEffects::execute_tool()` with a write to the appropriate `tools/{path}`
3. Keep `HostEffects::emit_event()` as-is (events are not tool calls)
4. Update the Wasm agent ABI documentation

- [ ] **Step 2: Refactor HostEffects trait**

```rust
/// Callback trait for host-side effects.
///
/// With the ToolStore handling completions and tools, this narrows
/// to event emission only. Completions and tool execution are
/// store writes routed through the ToolStore.
pub trait HostEffects: Send {
    /// Emit an agent event to the TUI or other observer.
    fn emit_event(&mut self, event: AgentEvent);
}
```

- [ ] **Step 3: Update HostStore intercept paths**

Replace `write_gate_complete` and `write_tools_execute` with ToolStore delegation. The `tools/` prefix routes to the ToolStore mounted in the backend namespace. Remove `SimpleStore` for tool results — the ToolStore manages its own results.

- [ ] **Step 4: Update existing HostStore tests**

All tests that use `MockEffects::complete()` or `MockEffects::execute_tool()` need to be updated. They should now verify that writes to `tools/completions/complete/{account}` and `tools/{wire_name}` route through the ToolStore.

- [ ] **Step 5: Run tests**

Run: `cargo test -p ox-runtime`
Expected: All pass.

- [ ] **Step 6: Commit**

```bash
git add crates/ox-runtime/
git commit -m "$(cat <<'EOF'
refactor(ox-runtime): route tools/* through ToolStore

HostEffects narrows to emit_event only. Completions and tool
execution are now StructFS writes through the unified ToolStore.
EOF
)"
```

---

### Task 12: Wire ToolStore into ox-cli

**Files:**
- Modify: `crates/ox-cli/Cargo.toml`
- Modify: `crates/ox-cli/src/agents.rs`
- Delete: `crates/ox-cli/src/tools.rs`

Replace `standard_tools()` + `ToolRegistry` + `execute_tool()` with ToolStore mounted in the broker. The PolicyGuard wraps the ToolStore via PolicyStore.

- [ ] **Step 1: Update agents.rs to construct ToolStore**

Replace the tool registration block (currently lines ~170-183) with:

```rust
let executor = std::env::current_exe().expect("resolve own binary for tool executor");
// In production, this is Clash's SandboxPolicy implementation.
// With --no-policy, use PermissivePolicy.
let policy: Arc<dyn ox_tools::sandbox::SandboxPolicy> = if no_policy {
    Arc::new(ox_tools::sandbox::PermissivePolicy)
} else {
    // Clash provides the real implementation
    Arc::new(clash_sandbox_policy(&workspace))
};
let fs = ox_tools::fs::FsModule::new(workspace.clone(), executor.clone(), policy.clone());
let os = ox_tools::os::OsModule::new(workspace.clone(), executor, policy);
let gate = GateStore::new();
let completion = ox_tools::completion::CompletionModule::new(gate);
let tool_store = ox_tools::ToolStore::new(fs, os, completion);

let policy = if no_policy {
    crate::policy::PolicyGuard::permissive()
} else {
    crate::policy::PolicyGuard::load(&workspace)
};
```

- [ ] **Step 2: Replace execute_tool with ToolStore write**

The current `execute_tool()` method (lines 442-522) does policy check → execute → return result. Replace with:

1. Policy check uses the PolicyStore wrapper
2. Tool execution is a write to the ToolStore
3. Result is a read from the ToolStore

The approval flow (broker-based deferred write) integrates by making the PolicyStore an async store mounted in the broker. This fixes the timeout bug structurally — the deferred lives in the store, not in a client with a mismatched timeout.

- [ ] **Step 3: Delete tools.rs**

`crates/ox-cli/src/tools.rs` is fully replaced by `ox-tools/src/fs.rs` and `ox-tools/src/os.rs`. Remove the file and all references to it.

- [ ] **Step 4: Fix compilation**

Run: `cargo check -p ox-cli`
Fix all remaining references to `standard_tools`, `ToolRegistry`, etc.

- [ ] **Step 5: Run ox-cli tests**

Run: `cargo test -p ox-cli`
Expected: All pass.

- [ ] **Step 6: Commit**

```bash
git add -u crates/ox-cli/
git commit -m "$(cat <<'EOF'
refactor(ox-cli): replace ToolRegistry with ToolStore

standard_tools() removed. FsModule/OsModule/CompletionModule handle
all tool execution through unified StructFS interface. PolicyGuard
wraps via PolicyStore. Fixes approval timeout bug structurally.
EOF
)"
```

---

### Task 13: Wire ToolStore into ox-web

**Files:**
- Modify: `crates/ox-web/Cargo.toml`
- Modify: `crates/ox-web/src/lib.rs`

Replace dual Rust/JS tool dispatch with ToolStore. JS tools are registered into the ToolStore via a new JS module wrapper.

- [ ] **Step 1: Replace OxAgent tool fields**

Change `rust_tools: Rc<RefCell<ToolRegistry>>` and `js_tools: Rc<RefCell<HashMap<String, JsTool>>>` to a single `tool_store: Rc<RefCell<ToolStore>>`.

- [ ] **Step 2: Update execute_tool function**

Replace the current `execute_tool` (line ~687) which tries Rust then JS tools with a single ToolStore write:

```rust
fn execute_tool(
    tool_store: &Rc<RefCell<ToolStore>>,
    name: &str,
    input: &serde_json::Value,
) -> serde_json::Value {
    let input_value = structfs_serde_store::json_to_value(input.clone());
    let path = ox_path::oxpath!("tools", name);

    let mut store = tool_store.borrow_mut();
    match store.write(&path, Record::parsed(input_value)) {
        Ok(result_path) => {
            // Read the result
            match store.read(&result_path) {
                Ok(Some(record)) => {
                    structfs_serde_store::value_to_json(
                        record.as_value().cloned().unwrap_or(Value::Null)
                    )
                }
                _ => serde_json::json!({"error": "no result"}),
            }
        }
        Err(e) => serde_json::json!({"error": e.to_string()}),
    }
}
```

- [ ] **Step 3: Update register_tool for JS tools**

JS tool registration needs to wrap the JS callback in a way that integrates with the ToolStore. Options:
1. Add a `JsModule` to ToolStore that holds JS tools
2. Register JS tools as FsModule-style operations

The implementer should choose based on what's cleanest. The key constraint: JS tools must appear in `tools/schemas` and be executable via the same `write` path as Rust tools.

- [ ] **Step 4: Run wasm check and TS tests**

Run: `cargo check --target wasm32-unknown-unknown -p ox-web`
Run: `cd crates/ox-web/ui && bun test`
Expected: All pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-web/
git commit -m "$(cat <<'EOF'
refactor(ox-web): replace dual tool dispatch with ToolStore

Single ToolStore handles all tool execution. JS tools integrate
via store write/read instead of separate HashMap lookup.
EOF
)"
```

---

### Task 14: Remove deprecated types from ox-kernel

**Files:**
- Modify: `crates/ox-kernel/src/lib.rs`
- Modify: `crates/ox-gate/src/lib.rs` (remove `create_completion_tools`)
- Modify: `crates/ox-gate/src/tools.rs` (remove `completion_tool`, `SendFn`)
- Modify: `crates/ox-core/src/lib.rs` (remove old `Agent::new`, re-exports)
- Modify: `crates/ox-context/src/lib.rs` (remove `ToolsProvider`)

- [ ] **Step 1: Remove Tool, FnTool, ToolRegistry from ox-kernel**

Delete the `Tool` trait (lines 228-237), `FnTool` struct (lines 243-283), and `ToolRegistry` (lines 289-328) from `crates/ox-kernel/src/lib.rs`.

Remove `run_turn` method from `Kernel`.

- [ ] **Step 2: Remove completion_tool factory from ox-gate**

In `crates/ox-gate/src/tools.rs`, remove `SendFn`, `completion_tool`, `complete_via_gate`, `completion_tool_schema`. This file may become empty or be deleted entirely.

In `crates/ox-gate/src/lib.rs`, remove `create_completion_tools()`.

- [ ] **Step 3: Remove ToolsProvider from ox-context**

In `crates/ox-context/src/lib.rs`, remove the `ToolsProvider` struct. Tool schemas are now served by the ToolStore at `tools/schemas`.

- [ ] **Step 4: Clean up ox-core re-exports**

Remove re-exports of `Tool`, `FnTool`, `ToolRegistry`, `completion_tool` from `crates/ox-core/src/lib.rs`. Remove old `Agent::new` constructor. Keep `Agent::with_tool_store` as the sole constructor.

- [ ] **Step 5: Run full workspace**

Run: `cargo test --workspace`
Run: `cargo check --target wasm32-unknown-unknown -p ox-web`
Expected: All pass, no deprecation warnings.

- [ ] **Step 6: Commit**

```bash
git add -u
git commit -m "$(cat <<'EOF'
refactor: remove deprecated Tool/ToolRegistry/FnTool/SendFn

All tool execution now goes through the unified ToolStore.
Removes ToolsProvider from ox-context (schemas served by ToolStore).
Removes completion_tool factory from ox-gate (completions are store writes).
EOF
)"
```

---

### Task 15: Run quality gates

**Files:** None (verification only)

- [ ] **Step 1: Run formatting**

Run: `./scripts/fmt.sh`
Expected: Clean formatting.

- [ ] **Step 2: Run full quality gates**

Run: `./scripts/quality_gates.sh`
Expected: All 14+ gates pass.

- [ ] **Step 3: Run coverage**

Run: `./scripts/coverage.sh`
Expected: Coverage meets or exceeds current thresholds. If ox-tools is below threshold, add more tests.

- [ ] **Step 4: Commit any formatting fixes**

```bash
git add -u
git commit -m "chore: formatting after unified tool store refactor"
```

---

## Subsystem 4: Native Tool Registration and Full Migration

### Design Correction

The ToolStore is a **routing layer**, not just a sandboxed execution engine. It maps StructFS paths to tool implementations. Those implementations can be:

- **Sandboxed subprocess** (fs/read, fs/write, fs/edit, os/shell) — OS-enforced via Clash `SandboxPolicy`
- **In-process native** (completions via HTTP, JS callbacks in browser, Rust closures) — no sandbox, just function calls
- **Anything mountable** — as long as it takes `Value` in and returns `Value` out

Sandboxing is an attribute of specific modules (fs, os), not a property of the ToolStore. Completions are HTTP calls — no sandbox. JS tools in the browser are callbacks — no sandbox (the browser IS the sandbox).

### Task 16b: Add native tool registration to ToolStore

**Files:**
- Create: `crates/ox-tools/src/native.rs`
- Modify: `crates/ox-tools/src/lib.rs`

Add a `NativeTool` trait and registration mechanism. Native tools execute in-process — no subprocess, no sandbox. They're the equivalent of the old `FnTool` but integrated into StructFS routing.

```rust
// native.rs

/// A tool that executes in-process. No sandbox, no subprocess.
/// Used for: JS callbacks (browser), Rust closures, completion-like tools.
pub trait NativeTool: Send + Sync {
    fn execute(&self, input: serde_json::Value) -> Result<serde_json::Value, String>;
    fn schema(&self) -> crate::ToolSchemaEntry;
}

/// Closure-backed native tool (equivalent of old FnTool).
pub struct FnTool {
    wire_name: String,
    internal_path: String,
    description: String,
    input_schema: serde_json::Value,
    run: Box<dyn Fn(serde_json::Value) -> Result<serde_json::Value, String> + Send + Sync>,
}
```

ToolStore gains:
```rust
impl ToolStore {
    /// Register a native (in-process) tool at a wire name.
    pub fn register_native(&mut self, tool: Box<dyn NativeTool>) { ... }
}
```

The Writer impl checks native tools after the built-in modules. The Reader impl serves results from native tools the same way as module results.

### Task 17b: Migrate ox-web to ToolStore with native JS tools

JS tools become native tools registered into the ToolStore. The JsTool wrapper implements NativeTool (on wasm32 only). The dual dispatch (ToolRegistry + js_tools HashMap) is replaced by a single ToolStore that holds both sandboxed modules and native JS tools.

### Task 18b: Migrate ox-runtime HostStore fully

Remove `gate/complete` and `tools/execute` intercepts. Narrow HostEffects to `emit_event` only. All completions and tool execution route through ToolStore.

### Task 19b: Migrate ox-core Agent fully

Remove old `Agent::new`. Remove `tools: ToolRegistry` and `send: Arc<SendFn>` fields. `Agent::with_tool_store` is the only constructor.

### Task 20b: Remove deprecated types

Delete Tool trait, FnTool, ToolRegistry, run_turn from ox-kernel. Delete completion_tool/SendFn from ox-gate. Delete ToolsProvider from ox-context. Remove all `#[allow(deprecated)]`.

---

## Post-Plan Notes

### What This Plan Does NOT Cover

1. **Context-based prompt synthesis** — CompletionModule currently delegates reads/writes to GateStore. The context spec → prompt synthesis pipeline (where the agent specifies store paths to include rather than pre-built prompts) is a follow-up. The infrastructure is in place but the codec changes are substantial.

2. **Async TurnStore in broker** — TurnStore is synchronous in this plan. Making it async (for parallel effect execution) requires mounting it in the broker as an AsyncWriter, which is a natural follow-up using the `mount_async` pattern from ox-broker.

3. **Clash `SandboxPolicy` implementation** — This plan defines the `SandboxPolicy` trait that Clash implements. The actual Clash implementation (generating macOS sandbox-exec profiles, Linux landlock rules, etc. from `AccessIntent`) is Clash's responsibility. ox-tools ships with `PermissivePolicy` for tests and `--no-policy` mode.

4. **Multi-completion turns** — TurnStore supports batching multiple effects, but the kernel loop doesn't yet drive multiple completions per turn. This requires the kernel's `step()` to be aware of completion effects alongside tool effects, which depends on context-based synthesis being done first.

5. **Clash policy migration** — PolicyGuard currently works with flat tool names (`read_file`). PolicyStore uses StructFS paths (`fs/read`). The policy format needs migration, which should be coordinated with clash upstream.

### Dependency Order

```
Task 1-3 (FsModule, OsModule, NameMap) — independent, can parallelize
Task 4 (CompletionModule) — depends on lib.rs from Task 3
Task 5 (ToolStore) — depends on Tasks 1-4
Task 6 (PolicyStore) — depends on Task 5
Task 7 (TurnStore) — independent of Tasks 1-6
Task 8-9 (Kernel refactor) — depends on Task 5
Tasks 10-13 (Integration) — depends on Tasks 8-9
Task 14 (Cleanup) — depends on Tasks 10-13
Task 15 (Quality gates) — depends on Task 14
```

Tasks 1, 2, 3, and 7 can be implemented in parallel by separate agents.
