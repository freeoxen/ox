#![cfg(unix)]

use std::os::fd::OwnedFd;
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

use ox_structfs_transport::WireErrorCode;
use ox_worker::service::test_support;
use ox_worker::{WorkerBuildIdentity, WorkerConfig, WorkerLimits, WorkerService};

fn private_tempdir() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    temp
}

fn config(temp: &tempfile::TempDir) -> WorkerConfig {
    WorkerConfig {
        inbox_root: temp.path().join("inbox"),
        socket_path: temp.path().join("runtime/worker.sock"),
        node_id: "service-node".into(),
        attempt_id: "service-attempt".into(),
        command_capacity: 8,
        limits: WorkerLimits::default(),
        transport: ox_structfs_transport::ServerConfig::default(),
    }
}

fn verified_test_identity() -> WorkerBuildIdentity {
    WorkerBuildIdentity {
        executable_digest: "1".repeat(64),
        image_digest: format!("ox-worker@sha256:{}", "2".repeat(64)),
        sandbox_preflight: "test-verified".into(),
    }
}

#[tokio::test]
async fn bound_service_refuses_unsafe_or_occupied_socket_paths() {
    let temp = private_tempdir();

    let insecure = temp.path().join("insecure");
    std::fs::create_dir(&insecure).unwrap();
    std::fs::set_permissions(&insecure, std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut candidate = config(&temp);
    candidate.socket_path = insecure.join("worker.sock");
    assert!(
        WorkerService::start_bound_with_test_hooks(
            candidate,
            verified_test_identity(),
            None,
            None,
        )
        .await
        .err()
        .unwrap()
        .contains("mode 0700")
    );

    let parent_file = temp.path().join("parent-file");
    std::fs::write(&parent_file, b"not a directory").unwrap();
    let mut candidate = config(&temp);
    candidate.socket_path = parent_file.join("worker.sock");
    assert!(
        WorkerService::start_bound_with_test_hooks(
            candidate,
            verified_test_identity(),
            None,
            None,
        )
        .await
        .err()
        .unwrap()
        .contains("real directory")
    );

    let real_parent = temp.path().join("real-parent");
    std::fs::create_dir(&real_parent).unwrap();
    std::fs::set_permissions(&real_parent, std::fs::Permissions::from_mode(0o700)).unwrap();
    let linked_parent = temp.path().join("linked-parent");
    std::os::unix::fs::symlink(&real_parent, &linked_parent).unwrap();
    let mut candidate = config(&temp);
    candidate.socket_path = linked_parent.join("worker.sock");
    assert!(
        WorkerService::start_bound_with_test_hooks(
            candidate,
            verified_test_identity(),
            None,
            None,
        )
        .await
        .err()
        .unwrap()
        .contains("real directory")
    );

    let runtime = temp.path().join("occupied");
    std::fs::create_dir(&runtime).unwrap();
    std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o700)).unwrap();
    let occupied = runtime.join("worker.sock");
    std::fs::write(&occupied, b"not a socket").unwrap();
    let mut candidate = config(&temp);
    candidate.socket_path = occupied;
    assert!(
        WorkerService::start_bound_with_test_hooks(
            candidate,
            verified_test_identity(),
            None,
            None,
        )
        .await
        .err()
        .unwrap()
        .contains("non-socket")
    );
}

#[test]
fn service_error_mapping_and_image_identity_are_typed_and_fail_closed() {
    let overload = structfs_core_store::Error::store(
        "WorkerPublicStore",
        "message",
        "overloaded: queued input limit reached",
    );
    let conflict = structfs_core_store::Error::store(
        "WorkerPublicStore",
        "approval",
        "stale approval identity",
    );
    assert_eq!(
        test_support::map_error(&overload).code,
        WireErrorCode::Overloaded
    );
    assert_eq!(
        test_support::map_error(&conflict).code,
        WireErrorCode::Conflict
    );

    let invalid_path: structfs_core_store::Error =
        structfs_core_store::Path::parse("bad/path with space")
            .unwrap_err()
            .into();
    assert_eq!(
        test_support::map_error(&invalid_path).code,
        WireErrorCode::InvalidRequest
    );
    let codec = structfs_core_store::Error::decode(structfs_core_store::Format::JSON, "malformed");
    assert_eq!(
        test_support::map_error(&codec).code,
        WireErrorCode::InvalidRequest
    );
    let missing = structfs_core_store::Error::NoRoute {
        path: structfs_core_store::path!("missing"),
    };
    assert_eq!(
        test_support::map_error(&missing).code,
        WireErrorCode::NotFound
    );
    let unsupported =
        structfs_core_store::Error::UnsupportedFormat(structfs_core_store::Format::JSON);
    assert_eq!(
        test_support::map_error(&unsupported).code,
        WireErrorCode::Unsupported
    );
    let store = structfs_core_store::Error::store("test", "read", "failed");
    assert_eq!(test_support::map_error(&store).code, WireErrorCode::Store);

    assert!(test_support::validate_pinned_image(
        "ghcr.io/freeoxen/ox-worker@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
    )
    .is_ok());
    assert!(test_support::validate_pinned_image("ghcr.io/freeoxen/ox-worker:latest").is_err());
    assert!(test_support::validate_pinned_image("sha256:short").is_err());
    assert!(
        test_support::validate_pinned_image(
            "sha256:ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789"
        )
        .is_ok()
    );
    assert_eq!(test_support::executable_digest().unwrap().len(), 64);
    assert_eq!(
        test_support::tool_executor_path(std::path::Path::new("/opt/ox/ox-worker")).unwrap(),
        std::path::Path::new("/opt/ox/ox-tool-exec")
    );
    assert!(test_support::tool_executor_path(std::path::Path::new("/")).is_err());
    let identity = test_support::verified_build_identity("image@sha256:test".into(), "abc".into());
    assert_eq!(identity.image_digest, "image@sha256:test");
    assert_eq!(identity.executable_digest, "abc");
    assert_eq!(identity.sandbox_preflight, "passed");

    assert!(test_support::validate_socket_parent(true, false, 7, 7, 0o700).is_ok());
    assert!(test_support::validate_socket_parent(false, false, 7, 7, 0o700).is_err());
    assert!(test_support::validate_socket_parent(true, true, 7, 7, 0o700).is_err());
    assert!(test_support::validate_socket_parent(true, false, 8, 7, 0o700).is_err());
    assert!(test_support::validate_socket_parent(true, false, 7, 7, 0o755).is_err());

    assert!(test_support::validate_unlink_candidate(true, 7, 7).is_ok());
    assert!(test_support::validate_unlink_candidate(false, 7, 7).is_err());
    assert!(test_support::validate_unlink_candidate(true, 8, 7).is_err());
    assert!(test_support::validate_connect_error(std::io::ErrorKind::ConnectionRefused).is_ok());
    assert!(test_support::validate_connect_error(std::io::ErrorKind::NotFound).is_ok());
    assert!(test_support::validate_connect_error(std::io::ErrorKind::PermissionDenied).is_err());

    assert!(test_support::owns_socket(true, 7, 7, (1, 2), (1, 2)));
    assert!(!test_support::owns_socket(false, 7, 7, (1, 2), (1, 2)));
    assert!(!test_support::owns_socket(true, 8, 7, (1, 2), (1, 2)));
    assert!(!test_support::owns_socket(true, 7, 7, (1, 3), (1, 2)));

    assert!(test_support::same_socket_identity((1, 2), (1, 2), true));
    assert!(!test_support::same_socket_identity((1, 2), (1, 3), true));
    assert!(!test_support::same_socket_identity((1, 2), (1, 2), false));

    assert!(test_support::validate_bound_socket(true, 7, 7, 0o600).is_ok());
    assert!(test_support::validate_bound_socket(false, 7, 7, 0o600).is_err());
    assert!(test_support::validate_bound_socket(true, 8, 7, 0o600).is_err());
    assert!(test_support::validate_bound_socket(true, 7, 7, 0o644).is_err());
}

#[tokio::test]
async fn socket_preparation_and_identity_cleanup_cover_safe_lifecycle_edges() {
    let temp = private_tempdir();
    assert!(
        test_support::prepare_socket(std::path::Path::new("worker.sock"))
            .await
            .is_err()
    );
    assert!(
        test_support::prepare_socket(std::path::Path::new("/"))
            .await
            .unwrap_err()
            .contains("no parent")
    );
    let oversized = "x".repeat(300);
    assert!(
        test_support::prepare_socket(&temp.path().join(&oversized).join("worker.sock"))
            .await
            .is_err()
    );

    let runtime = temp.path().join("runtime-direct");
    let socket = runtime.join("worker.sock");
    test_support::prepare_socket(&socket).await.unwrap();
    assert_eq!(
        std::fs::metadata(&runtime).unwrap().permissions().mode() & 0o777,
        0o700
    );
    std::fs::write(&socket, b"not a socket").unwrap();
    assert!(
        test_support::prepare_socket(&socket)
            .await
            .unwrap_err()
            .contains("non-socket")
    );
    std::fs::remove_file(&socket).unwrap();
    assert!(
        test_support::prepare_socket(&runtime.join(&oversized))
            .await
            .is_err()
    );

    assert!(test_support::remove_owned_socket(&socket, (0, 0)).is_ok());
    let replacement = temp.path().join("replacement-file");
    std::fs::write(&replacement, b"replacement").unwrap();
    let replacement_metadata = std::fs::symlink_metadata(&replacement).unwrap();
    assert!(test_support::validate_socket_metadata(&replacement_metadata).is_err());
    assert!(test_support::finalize_bound_socket(&replacement).is_err());
    assert!(!test_support::same_file(
        &replacement_metadata,
        &replacement_metadata
    ));
    assert!(
        test_support::remove_owned_socket(
            &replacement,
            (replacement_metadata.dev(), replacement_metadata.ino())
        )
        .unwrap_err()
        .contains("identity changed")
    );
    assert!(test_support::remove_owned_socket(&temp.path().join(&oversized), (0, 0)).is_err());
    assert!(test_support::finalize_bound_socket(&temp.path().join(&oversized)).is_err());

    let (socket_a, _socket_b) = std::os::unix::net::UnixStream::pair().unwrap();
    let socket_fd: OwnedFd = socket_a.into();
    let socket_file = std::fs::File::from(socket_fd);
    let socket_metadata = socket_file.metadata().unwrap();
    assert!(test_support::validate_socket_metadata(&socket_metadata).is_err());
    assert!(test_support::same_file(&socket_metadata, &socket_metadata));

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let listener = loop {
        match std::os::unix::net::UnixListener::bind(&socket) {
            Ok(listener) => break listener,
            Err(error)
                if error.kind() == std::io::ErrorKind::PermissionDenied
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("bind failed: {error}"),
        }
    };
    assert!(
        test_support::prepare_socket(&socket)
            .await
            .unwrap_err()
            .contains("already live")
    );
    let metadata = std::fs::symlink_metadata(&socket).unwrap();
    let identity = (metadata.dev(), metadata.ino());
    drop(listener);
    test_support::prepare_socket(&socket).await.unwrap();
    assert!(!socket.exists());

    assert!(test_support::remove_owned_socket(&socket, identity).is_ok());
    let owned = temp.path().join("owned.sock");
    let listener = std::os::unix::net::UnixListener::bind(&owned).unwrap();
    let metadata = std::fs::symlink_metadata(&owned).unwrap();
    let identity = (metadata.dev(), metadata.ino());
    drop(listener);
    test_support::remove_owned_socket(&owned, identity).unwrap();
    assert!(!owned.exists());

    let replacement = temp.path().join("replacement.sock");
    let listener = std::os::unix::net::UnixListener::bind(&replacement).unwrap();
    let metadata = std::fs::symlink_metadata(&replacement).unwrap();
    let identity = (metadata.dev(), metadata.ino());
    drop(listener);
    std::fs::remove_file(&replacement).unwrap();
    std::fs::write(&replacement, b"replacement").unwrap();
    assert!(
        test_support::remove_owned_socket(&replacement, identity)
            .unwrap_err()
            .contains("identity changed")
    );
}

#[tokio::test]
async fn in_process_configuration_rejects_each_invalid_boundary() {
    let temp = private_tempdir();

    let mut candidate = config(&temp);
    candidate.command_capacity = 0;
    assert_eq!(
        WorkerService::start_in_process(candidate)
            .await
            .err()
            .unwrap(),
        "command_capacity must be non-zero"
    );

    let mut candidate = config(&temp);
    candidate.node_id.clear();
    assert!(
        WorkerService::start_in_process(candidate)
            .await
            .err()
            .unwrap()
            .contains("node_id and attempt_id")
    );

    let mut candidate = config(&temp);
    candidate.attempt_id = "x".repeat(129);
    assert!(
        WorkerService::start_in_process(candidate)
            .await
            .err()
            .unwrap()
            .contains("1..=128")
    );

    for mutate in [
        |limits: &mut WorkerLimits| limits.max_active_turns = 0,
        |limits: &mut WorkerLimits| limits.max_queued_inputs_per_thread = 0,
        |limits: &mut WorkerLimits| limits.max_total_threads = 0,
        |limits: &mut WorkerLimits| limits.max_parked_cursors = 0,
        |limits: &mut WorkerLimits| limits.max_ledger_batch_entries = 0,
        |limits: &mut WorkerLimits| limits.max_ledger_batch_bytes = 0,
        |limits: &mut WorkerLimits| limits.max_ledger_line_bytes = 0,
    ] {
        let mut candidate = config(&temp);
        mutate(&mut candidate.limits);
        assert_eq!(
            WorkerService::start_in_process(candidate)
                .await
                .err()
                .unwrap(),
            "all worker limits must be non-zero"
        );
    }

    let inbox_file = temp.path().join("inbox-file");
    std::fs::write(&inbox_file, b"not a directory").unwrap();
    let mut candidate = config(&temp);
    candidate.inbox_root = inbox_file;
    assert!(WorkerService::start_in_process(candidate).await.is_err());

    let database_failure = private_tempdir();
    let candidate = config(&database_failure);
    std::fs::create_dir_all(candidate.inbox_root.join("ox.db")).unwrap();
    assert!(
        WorkerService::start_in_process(candidate)
            .await
            .err()
            .unwrap()
            .contains("open mounted worker inbox")
    );
}

#[tokio::test]
async fn shutdown_refuses_a_replaced_service_identity() {
    let temp = private_tempdir();
    let service = WorkerService::start_in_process(config(&temp))
        .await
        .unwrap();
    let replacement = temp.path().join("replacement-service-path");
    std::fs::write(&replacement, b"not a socket").unwrap();
    let metadata = std::fs::symlink_metadata(&replacement).unwrap();
    let service = test_support::with_socket_identity(
        service,
        replacement.clone(),
        (metadata.dev(), metadata.ino()),
    );
    assert!(
        service
            .shutdown()
            .await
            .unwrap_err()
            .contains("identity changed")
    );
    assert!(replacement.exists());
}
