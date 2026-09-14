//! The CLI package must build its own sibling tool executor for cargo install.

use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn cli_helper_executes_bounded_reads_and_rejects_invalid_invocation() {
    let helper = env!("CARGO_BIN_EXE_ox-tool-exec");
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), "abcdefgh").unwrap();
    let mut child = Command::new(helper)
        .arg("--tool-exec")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let request = serde_json::json!({
        "op": "fs/read",
        "args": {"path": file.path()},
        "_ox_max_output_bytes": 4
    });
    child
        .stdin
        .take()
        .unwrap()
        .write_all(request.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["ok"], true);
    assert_eq!(
        response["value"],
        "abcd\n[... file truncated at byte limit]"
    );

    let output = Command::new(helper).output().unwrap();
    assert_eq!(output.status.code(), Some(1));
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["ok"], false);
    assert_eq!(response["value"], "expected --tool-exec flag");
}
