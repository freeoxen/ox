#![cfg(unix)]

use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _};

use ox_broker::async_store::AsyncReader as _;
use ox_worker::service::test_support;
use ox_worker::{WorkerBuildIdentity, WorkerConfig, WorkerLimits, WorkerService};

#[tokio::test]
async fn private_unix_carrier_is_live_and_identity_safe() {
    let temp = tempfile::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = temp.path().join("runtime/worker.sock");
    let service = match WorkerService::start_bound_with_test_hooks(
        WorkerConfig {
            inbox_root: temp.path().join("inbox"),
            socket_path: socket.clone(),
            node_id: "socket-node".into(),
            attempt_id: "socket-attempt".into(),
            command_capacity: 8,
            limits: WorkerLimits::default(),
            transport: ox_structfs_transport::ServerConfig::default(),
        },
        WorkerBuildIdentity {
            executable_digest: "1".repeat(64),
            image_digest: format!("ox-worker@sha256:{}", "2".repeat(64)),
            sandbox_preflight: "test-verified".into(),
        },
        None,
        None,
    )
    .await
    {
        Ok(service) => service,
        Err(error) if error.contains("Operation not permitted") => return,
        Err(error) => panic!("bound worker failed: {error}"),
    };

    let parent = std::fs::metadata(socket.parent().unwrap()).unwrap();
    assert_eq!(parent.permissions().mode() & 0o777, 0o700);
    let metadata = std::fs::symlink_metadata(&socket).unwrap();
    assert!(metadata.file_type().is_socket());
    assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
    assert_eq!(metadata.permissions().mode() & 0o777, 0o600);

    let mut remote = ox_structfs_transport::connect_unix(
        &socket,
        ox_structfs_transport::RemoteStoreConfig::default(),
    )
    .await
    .unwrap();
    let health = remote
        .read(&structfs_core_store::path!("health"))
        .await
        .unwrap()
        .unwrap();
    let health = structfs_serde_store::value_to_json(health.as_value().unwrap().clone());
    assert_eq!(health["sandbox_enforcement"]["preflight"], "test-verified");

    service.shutdown().await.unwrap();
    assert!(!socket.exists());

    let direct_socket = temp.path().join("direct.sock");
    let listener = std::os::unix::net::UnixListener::bind(&direct_socket).unwrap();
    assert!(
        test_support::prepare_socket(&direct_socket)
            .await
            .unwrap_err()
            .contains("already live")
    );
    drop(listener);
    test_support::prepare_socket(&direct_socket).await.unwrap();
    assert!(!direct_socket.exists());

    let owned = temp.path().join("owned.sock");
    let listener = std::os::unix::net::UnixListener::bind(&owned).unwrap();
    let metadata = std::fs::symlink_metadata(&owned).unwrap();
    let identity = (metadata.dev(), metadata.ino());
    drop(listener);
    test_support::remove_owned_socket(&owned, identity).unwrap();
    assert!(!owned.exists());
}
