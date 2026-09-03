#![cfg(unix)]

use std::os::unix::fs::PermissionsExt as _;
use std::process::Command;

fn private_tempdir() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    temp
}

fn serve_command(temp: &tempfile::TempDir) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ox-worker"));
    command.args([
        "serve",
        "--root",
        temp.path().join("inbox").to_str().unwrap(),
        "--socket",
        temp.path().join("runtime/worker.sock").to_str().unwrap(),
        "--node-id",
        "cli-node",
        "--attempt-id",
        "cli-attempt",
        "--max-active-turns",
        "3",
        "--max-queued-inputs-per-thread",
        "5",
        "--max-total-threads",
        "7",
        "--max-parked-cursors",
        "11",
        "--max-ledger-batch-entries",
        "13",
        "--max-ledger-batch-bytes",
        "4096",
        "--max-ledger-line-bytes",
        "2048",
    ]);
    command
}

#[test]
fn serve_cli_maps_limits_and_fails_closed_without_a_release_image() {
    let temp = private_tempdir();
    let output = serve_command(&temp)
        .env_remove("OX_WORKER_IMAGE_DIGEST")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("OX_WORKER_IMAGE_DIGEST must be set"));
    assert_eq!(
        std::fs::metadata(temp.path().join("runtime"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );

    let temp = private_tempdir();
    let output = serve_command(&temp)
        .env("OX_WORKER_IMAGE_DIGEST", "ox-worker:latest")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("must be pinned"));

    let temp = private_tempdir();
    let output = serve_command(&temp)
        .env(
            "OX_WORKER_IMAGE_DIGEST",
            "ox-worker@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("ox-tool-exec") || stderr.contains("sandbox"),
        "unexpected pinned-image startup error: {stderr}"
    );
}

#[test]
fn cli_rejects_invalid_capacity_and_unreachable_stdio_carrier() {
    let temp = private_tempdir();
    let output = serve_command(&temp)
        .args(["--command-capacity", "0"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("command_capacity"));

    let output = Command::new(env!("CARGO_BIN_EXE_ox-worker"))
        .args([
            "structfs-stdio",
            "--socket",
            temp.path().join("missing.sock").to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
}
