use std::path::{Path, PathBuf};
use std::sync::Arc;

use ox_gate::GateStore;
use ox_path::oxpath;
use ox_tools::ToolStore;
use ox_tools::completion::CompletionModule;
use ox_tools::fs::FsModule;
use ox_tools::os::OsModule;
use ox_tools::sandbox::PermissivePolicy;
use structfs_core_store::{Reader, Record, Value, Writer};

fn make_tool_store(dir: &Path) -> ToolStore {
    let exec = PathBuf::from(env!("CARGO_BIN_EXE_ox-tool-exec"));
    let policy: Arc<dyn ox_tools::sandbox::SandboxPolicy> = Arc::new(PermissivePolicy);
    let fs = FsModule::new(dir.to_path_buf(), exec.clone(), policy.clone());
    let os = OsModule::new(dir.to_path_buf(), exec, policy);
    let gate = GateStore::new();
    let completion = CompletionModule::new(gate);
    ToolStore::new(fs, os, completion)
}

#[test]
fn routes_fs_read() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("hello.txt");
    std::fs::write(&file, "hello world").unwrap();

    let mut store = make_tool_store(tmp.path());

    // Write (execute) a read operation via the StructFS path
    let input = structfs_serde_store::json_to_value(serde_json::json!({"path": "hello.txt"}));
    store
        .write(&oxpath!("fs", "read"), Record::parsed(input))
        .unwrap();

    // Read the result back
    let result = store.read(&oxpath!("fs", "read", "result")).unwrap();
    assert!(result.is_some(), "expected Some result for fs/read/result");
    let value = result.unwrap().as_value().unwrap().clone();
    // The result should be the file content as a string
    match value {
        Value::String(s) => assert_eq!(s, "hello world"),
        _ => panic!("expected String value, got: {value:?}"),
    }
}

#[test]
fn routes_completions_defaults() {
    let tmp = tempfile::tempdir().unwrap();
    let mut store = make_tool_store(tmp.path());

    // GateStore should have a default account
    let result = store
        .read(&oxpath!("completions", "defaults", "account"))
        .unwrap();
    assert!(
        result.is_some(),
        "expected Some for completions/defaults/account"
    );
}

#[test]
fn schemas_aggregates_all_modules() {
    let tmp = tempfile::tempdir().unwrap();
    let store = make_tool_store(tmp.path());

    let schemas = store.all_schemas();
    // fs has 3 (read, write, edit), os has 1 (shell) = at least 4
    assert!(
        schemas.len() >= 4,
        "expected >= 4 schemas, got {}",
        schemas.len()
    );

    let names: Vec<&str> = schemas.iter().map(|s| s.wire_name.as_str()).collect();
    assert!(names.contains(&"fs_read"), "missing fs_read");
    assert!(names.contains(&"fs_write"), "missing fs_write");
    assert!(names.contains(&"fs_edit"), "missing fs_edit");
    assert!(names.contains(&"shell"), "missing shell");
}

#[test]
fn routes_os_shell() {
    let tmp = tempfile::tempdir().unwrap();
    let mut store = make_tool_store(tmp.path());

    let input = structfs_serde_store::json_to_value(serde_json::json!({"command": "echo hi"}));
    store
        .write(&oxpath!("os", "shell"), Record::parsed(input))
        .unwrap();

    let result = store.read(&oxpath!("os", "shell", "result")).unwrap();
    assert!(result.is_some(), "expected Some result for os/shell/result");
    let value = result.unwrap().as_value().unwrap().clone();
    // The shell result should contain the output "hi"
    let json = structfs_serde_store::value_to_json(value);
    let output_str = format!("{json}");
    assert!(
        output_str.contains("hi"),
        "expected 'hi' in shell output, got: {output_str}"
    );
}

#[test]
fn wire_name_resolution() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("wire.txt");
    std::fs::write(&file, "wire content").unwrap();

    let mut store = make_tool_store(tmp.path());

    // "fs_read" is the wire name for "fs/read"
    // Writing to just the wire name should route through to fs/read
    let input = structfs_serde_store::json_to_value(serde_json::json!({"path": "wire.txt"}));
    store
        .write(&oxpath!("fs_read"), Record::parsed(input))
        .unwrap();

    // Read via internal path should have the result
    let result = store.read(&oxpath!("fs", "read", "result")).unwrap();
    assert!(result.is_some(), "expected result after wire-name write");
    let value = result.unwrap().as_value().unwrap().clone();
    match value {
        Value::String(s) => assert_eq!(s, "wire content"),
        _ => panic!("expected String value, got: {value:?}"),
    }
}

#[test]
fn schemas_read_returns_json_array() {
    let tmp = tempfile::tempdir().unwrap();
    let mut store = make_tool_store(tmp.path());

    let result = store.read(&oxpath!("schemas")).unwrap();
    assert!(result.is_some(), "expected Some for schemas path");
    let value = result.unwrap().as_value().unwrap().clone();
    match value {
        Value::Array(arr) => {
            assert!(
                arr.len() >= 4,
                "expected >= 4 schema entries, got {}",
                arr.len()
            );
        }
        _ => panic!("expected Array value for schemas, got: {value:?}"),
    }
}

#[test]
fn tool_schemas_for_model_converts() {
    let tmp = tempfile::tempdir().unwrap();
    let store = make_tool_store(tmp.path());

    let schemas = store.tool_schemas_for_model();
    assert!(schemas.len() >= 4);
    // Check that each has the right kernel format
    for schema in &schemas {
        assert!(!schema.name.is_empty());
        assert!(!schema.description.is_empty());
    }
}

#[test]
fn name_map_has_all_registrations() {
    let tmp = tempfile::tempdir().unwrap();
    let store = make_tool_store(tmp.path());

    let nm = store.name_map();
    assert_eq!(nm.to_internal("fs_read"), Some("fs/read"));
    assert_eq!(nm.to_internal("fs_write"), Some("fs/write"));
    assert_eq!(nm.to_internal("fs_edit"), Some("fs/edit"));
    assert_eq!(nm.to_internal("shell"), Some("os/shell"));
    assert_eq!(nm.to_wire("fs/read"), Some("fs_read"));
    assert_eq!(nm.to_wire("os/shell"), Some("shell"));
}
