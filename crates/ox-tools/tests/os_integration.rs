use std::path::{Path, PathBuf};
use std::sync::Arc;

use ox_tools::os::OsModule;
use ox_tools::sandbox::PermissivePolicy;

fn make_module(dir: &Path) -> OsModule {
    let executor = PathBuf::from(env!("CARGO_BIN_EXE_ox-tool-exec"));
    OsModule::new(dir.to_path_buf(), executor, Arc::new(PermissivePolicy))
}

#[test]
fn shell_returns_structured_output() {
    let tmp = tempfile::tempdir().unwrap();
    let m = make_module(tmp.path());
    let result = m
        .execute("shell", &serde_json::json!({"command": "echo hello"}))
        .unwrap();

    assert_eq!(result.get("stdout").and_then(|v| v.as_str()), Some("hello\n"));
    assert_eq!(result.get("exit_code").and_then(|v| v.as_i64()), Some(0));
}

#[test]
fn shell_captures_stderr() {
    let tmp = tempfile::tempdir().unwrap();
    let m = make_module(tmp.path());
    let result = m
        .execute("shell", &serde_json::json!({"command": "echo err >&2"}))
        .unwrap();

    assert_eq!(result.get("stderr").and_then(|v| v.as_str()), Some("err\n"));
}

#[test]
fn shell_returns_nonzero_exit_code() {
    let tmp = tempfile::tempdir().unwrap();
    let m = make_module(tmp.path());
    let result = m
        .execute("shell", &serde_json::json!({"command": "exit 42"}))
        .unwrap();

    assert_eq!(result.get("exit_code").and_then(|v| v.as_i64()), Some(42));
}

#[test]
fn shell_runs_in_workspace_directory() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("marker.txt"), "found").unwrap();

    let m = make_module(tmp.path());
    let result = m
        .execute("shell", &serde_json::json!({"command": "cat marker.txt"}))
        .unwrap();

    assert_eq!(result.get("stdout").and_then(|v| v.as_str()), Some("found"));
    assert_eq!(result.get("exit_code").and_then(|v| v.as_i64()), Some(0));
}

#[test]
fn shell_rejects_missing_command() {
    let tmp = tempfile::tempdir().unwrap();
    let m = make_module(tmp.path());
    let result = m.execute("shell", &serde_json::json!({}));
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("missing 'command'"));
}

#[test]
fn schemas_returns_one_entry() {
    let tmp = tempfile::tempdir().unwrap();
    let m = make_module(tmp.path());
    let schemas = m.schemas();
    assert_eq!(schemas.len(), 1);
    assert_eq!(schemas[0].wire_name, "shell");
    assert_eq!(schemas[0].internal_path, "os/shell");
}
