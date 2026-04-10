use std::path::{Path, PathBuf};
use std::sync::Arc;

use ox_tools::fs::FsModule;
use ox_tools::sandbox::PermissivePolicy;

fn make_module(dir: &Path) -> FsModule {
    let executor = PathBuf::from(env!("CARGO_BIN_EXE_ox-tool-exec"));
    FsModule::new(dir.to_path_buf(), executor, Arc::new(PermissivePolicy))
}

#[test]
fn read_returns_file_content() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("hello.txt");
    std::fs::write(&file, "hello world").unwrap();

    let m = make_module(tmp.path());
    let result = m
        .execute("read", &serde_json::json!({"path": "hello.txt"}))
        .unwrap();

    assert_eq!(result.as_str().unwrap(), "hello world");
}

#[test]
fn read_rejects_path_escape() {
    let tmp = tempfile::tempdir().unwrap();
    let m = make_module(tmp.path());
    let result = m.execute("read", &serde_json::json!({"path": "../../etc/passwd"}));
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("escapes workspace"),
        "expected escape error, got: {err}"
    );
}

#[test]
fn write_creates_file() {
    let tmp = tempfile::tempdir().unwrap();
    let m = make_module(tmp.path());

    m.execute(
        "write",
        &serde_json::json!({"path": "out.txt", "content": "written"}),
    )
    .unwrap();

    let content = std::fs::read_to_string(tmp.path().join("out.txt")).unwrap();
    assert_eq!(content, "written");
}

#[test]
fn write_creates_parent_dirs() {
    let tmp = tempfile::tempdir().unwrap();
    let m = make_module(tmp.path());

    m.execute(
        "write",
        &serde_json::json!({"path": "a/b/c.txt", "content": "nested"}),
    )
    .unwrap();

    let content = std::fs::read_to_string(tmp.path().join("a/b/c.txt")).unwrap();
    assert_eq!(content, "nested");
}

#[test]
fn edit_replaces_unique_string() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("greet.txt");
    std::fs::write(&file, "hello world").unwrap();

    let m = make_module(tmp.path());
    m.execute(
        "edit",
        &serde_json::json!({
            "path": "greet.txt",
            "old_string": "world",
            "new_string": "rust"
        }),
    )
    .unwrap();

    let content = std::fs::read_to_string(&file).unwrap();
    assert_eq!(content, "hello rust");
}

#[test]
fn edit_rejects_ambiguous_without_line_hint() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("repeat.txt");
    std::fs::write(&file, "aaa\naaa\naaa").unwrap();

    let m = make_module(tmp.path());
    let result = m.execute(
        "edit",
        &serde_json::json!({
            "path": "repeat.txt",
            "old_string": "aaa",
            "new_string": "bbb"
        }),
    );

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.contains("3 times"),
        "expected '3 times' in error, got: {err}"
    );
}

#[test]
fn edit_uses_line_start_hint() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("repeat.txt");
    std::fs::write(&file, "aaa\naaa\naaa").unwrap();

    let m = make_module(tmp.path());
    m.execute(
        "edit",
        &serde_json::json!({
            "path": "repeat.txt",
            "old_string": "aaa",
            "new_string": "bbb",
            "line_start": 2
        }),
    )
    .unwrap();

    let content = std::fs::read_to_string(&file).unwrap();
    assert_eq!(content, "aaa\nbbb\naaa");
}

#[test]
fn schemas_returns_three_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let m = make_module(tmp.path());
    let schemas = m.schemas();
    assert_eq!(schemas.len(), 3);

    let names: Vec<&str> = schemas.iter().map(|s| s.wire_name.as_str()).collect();
    assert!(names.contains(&"fs_read"));
    assert!(names.contains(&"fs_write"));
    assert!(names.contains(&"fs_edit"));
}
