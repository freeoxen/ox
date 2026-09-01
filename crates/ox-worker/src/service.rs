use std::fs::Metadata;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ox_executor::{ExecutionCore, ExecutorConfig};
use ox_structfs_transport::{
    ExportRoot, ServerConfig, UnixServer, WireError, WireErrorCode, spawn_unix_server,
};
use sha2::{Digest as _, Sha256};

use crate::{PublicStore, WorkerBuildIdentity, WorkerLimits};

#[derive(Clone, Debug)]
pub struct WorkerConfig {
    pub inbox_root: PathBuf,
    pub socket_path: PathBuf,
    pub node_id: String,
    pub attempt_id: String,
    pub command_capacity: usize,
    pub limits: WorkerLimits,
    pub transport: ServerConfig,
}

pub struct WorkerService {
    pub public_store: PublicStore,
    server: Option<UnixServer>,
    socket_path: PathBuf,
    socket_identity: Option<(u64, u64)>,
    _mounts: Vec<tokio::task::JoinHandle<()>>,
    _execution: Arc<ox_executor::ExecutionHandle>,
}

impl WorkerService {
    pub async fn start(config: WorkerConfig) -> Result<Self, String> {
        Self::start_inner(config, true, None, None).await
    }

    /// Build the identical core/public Store without binding a carrier. Useful
    /// for embedding and semantic parity tests; production service mode uses
    /// [`Self::start`].
    pub async fn start_in_process(config: WorkerConfig) -> Result<Self, String> {
        Self::start_inner(config, false, None, None).await
    }

    /// Semantic-test constructor using the executor's existing scripted
    /// completion/tool hooks; it still constructs exactly one ExecutionCore.
    pub async fn start_in_process_with_test_hooks(
        config: WorkerConfig,
        transport_factory: Option<ox_executor::test_support::TransportFactory>,
        tool_injector: Option<ox_executor::test_support::ToolInjector>,
    ) -> Result<Self, String> {
        Self::start_inner(config, false, transport_factory, tool_injector).await
    }

    async fn start_inner(
        config: WorkerConfig,
        bind_socket: bool,
        transport_factory: Option<ox_executor::test_support::TransportFactory>,
        tool_injector: Option<ox_executor::test_support::ToolInjector>,
    ) -> Result<Self, String> {
        config.limits.validate()?;
        if config.command_capacity == 0 {
            return Err("command_capacity must be non-zero".into());
        }
        if config.node_id.is_empty()
            || config.attempt_id.is_empty()
            || config.node_id.len() > 128
            || config.attempt_id.len() > 128
        {
            return Err("node_id and attempt_id must contain 1..=128 bytes".into());
        }
        if bind_socket {
            prepare_socket(&config.socket_path).await?;
        }
        std::fs::create_dir_all(&config.inbox_root).map_err(|error| error.to_string())?;

        let (image_digest, sandbox_preflight) = if bind_socket {
            let image = std::env::var("OX_WORKER_IMAGE_DIGEST")
                .map_err(|_| "OX_WORKER_IMAGE_DIGEST must be set to a pinned image reference")?;
            validate_pinned_image(&image)?;
            let tool_executor = std::env::current_exe()
                .map_err(|error| error.to_string())?
                .parent()
                .ok_or("worker executable has no parent directory")?
                .join("ox-tool-exec");
            ox_executor::remote_sandbox_preflight(
                &config.inbox_root.join("sandbox-preflight"),
                &tool_executor,
            )?;
            (image, "passed".to_string())
        } else {
            // In-process semantic tests do not bind a remotely reachable
            // carrier. Their health shape remains complete without claiming an
            // image artifact was verified.
            ("test-in-process@sha256:0000000000000000000000000000000000000000000000000000000000000000".into(), "passed".into())
        };

        let broker = ox_broker::BrokerStore::default();
        // As in the interactive CLI, both handles are the established
        // InboxStore implementation over the same WAL database: one is mounted
        // for Store ingress and one is owned by the sole ExecutionCore.
        let mounted_inbox =
            ox_inbox::InboxStore::open(&config.inbox_root).map_err(|e| e.to_string())?;
        let core_inbox =
            ox_inbox::InboxStore::open(&config.inbox_root).map_err(|e| e.to_string())?;
        let mounts = ox_executor::mount_execution_stores(
            &broker,
            mounted_inbox,
            config.inbox_root.clone(),
            ox_store_util::LocalConfig::new(),
            ox_store_util::LocalConfig::new(),
        )
        .await;
        let executor_config = ExecutorConfig::remote(config.limits.max_active_turns)?;
        let core = ExecutionCore::new_with_config_and_test_hooks(
            config.inbox_root.join("workspaces"),
            false,
            core_inbox,
            config.inbox_root.clone(),
            broker.clone(),
            tokio::runtime::Handle::current(),
            executor_config,
            transport_factory,
            tool_injector,
        )?;
        let execution = Arc::new(core.into_handle(config.command_capacity));
        let public_store = PublicStore::new(
            broker.client(),
            execution.clone(),
            config.inbox_root,
            config.node_id,
            config.attempt_id,
            config.limits,
            WorkerBuildIdentity {
                executable_digest: executable_digest()?,
                image_digest,
                sandbox_preflight,
            },
        )?;
        let (server, socket_identity) = if bind_socket {
            let server = spawn_unix_server(
                &config.socket_path,
                ExportRoot::new(
                    public_store.clone(),
                    structfs_core_store::Path::parse("").expect("empty StructFS root is valid"),
                ),
                config.transport.with_error_mapper(Arc::new(worker_error)),
            )
            .map_err(|error| error.to_string())?;
            std::fs::set_permissions(&config.socket_path, std::fs::Permissions::from_mode(0o600))
                .map_err(|error| error.to_string())?;
            let metadata = std::fs::symlink_metadata(&config.socket_path)
                .map_err(|error| error.to_string())?;
            if !metadata.file_type().is_socket()
                || metadata.uid() != unsafe { libc::geteuid() }
                || metadata.permissions().mode() & 0o777 != 0o600
            {
                return Err("worker socket ownership or mode verification failed".into());
            }
            (Some(server), Some((metadata.dev(), metadata.ino())))
        } else {
            (None, None)
        };
        Ok(Self {
            public_store,
            server,
            socket_path: config.socket_path,
            socket_identity,
            _mounts: mounts,
            _execution: execution,
        })
    }

    pub async fn shutdown(self) -> Result<(), String> {
        if let Some(server) = self.server {
            server.shutdown().await.map_err(|error| error.to_string())?;
        }
        if let Some(identity) = self.socket_identity {
            remove_owned_socket(&self.socket_path, identity)
        } else {
            Ok(())
        }
    }
}

fn worker_error(error: &structfs_core_store::Error) -> WireError {
    let message = error.to_string();
    let code = if message.contains("overloaded:") || message.contains("limit reached") {
        WireErrorCode::Overloaded
    } else if message.contains("conflict:") || message.contains("stale") {
        WireErrorCode::Conflict
    } else {
        match error {
            structfs_core_store::Error::Path(_) | structfs_core_store::Error::Codec { .. } => {
                WireErrorCode::InvalidRequest
            }
            structfs_core_store::Error::NoRoute { .. } => WireErrorCode::NotFound,
            structfs_core_store::Error::UnsupportedFormat(_) => WireErrorCode::Unsupported,
            _ => WireErrorCode::Store,
        }
    };
    WireError { code, message }
}

async fn prepare_socket(path: &Path) -> Result<(), String> {
    let parent = path.parent().ok_or("socket path has no parent")?;
    match std::fs::symlink_metadata(parent) {
        Ok(metadata) => {
            if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
                return Err("worker socket parent must be a real directory".into());
            }
            if metadata.uid() != unsafe { libc::geteuid() }
                || metadata.permissions().mode() & 0o077 != 0
            {
                return Err(
                    "worker socket parent must be owned by the current uid and mode 0700".into(),
                );
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir(parent).map_err(|error| error.to_string())?;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))
                .map_err(|error| error.to_string())?;
        }
        Err(error) => return Err(error.to_string()),
    }
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    if !metadata.file_type().is_socket() {
        return Err("refusing to unlink non-socket service path".into());
    }
    if metadata.uid() != unsafe { libc::geteuid() } {
        return Err("refusing to unlink socket owned by another uid".into());
    }
    match tokio::net::UnixStream::connect(path).await {
        Ok(_) => Err("worker socket is already live".into()),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
            ) =>
        {
            // Re-read immediately before unlink so a replacement cannot be
            // confused with the stale socket we proved safe above.
            let current = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
            if !same_file(&metadata, &current) {
                return Err("worker socket changed during stale-socket check".into());
            }
            std::fs::remove_file(path).map_err(|error| error.to_string())
        }
        Err(error) => Err(format!("could not prove worker socket stale: {error}")),
    }
}

fn remove_owned_socket(path: &Path, identity: (u64, u64)) -> Result<(), String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    if metadata.file_type().is_socket()
        && metadata.uid() == unsafe { libc::geteuid() }
        && (metadata.dev(), metadata.ino()) == identity
    {
        std::fs::remove_file(path).map_err(|error| error.to_string())
    } else {
        Err("refusing to remove service path whose identity changed".into())
    }
}

fn same_file(left: &Metadata, right: &Metadata) -> bool {
    left.dev() == right.dev() && left.ino() == right.ino() && right.file_type().is_socket()
}

fn executable_digest() -> Result<String, String> {
    let path = std::env::current_exe().map_err(|error| error.to_string())?;
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn validate_pinned_image(value: &str) -> Result<(), String> {
    let digest = value
        .rsplit_once("@sha256:")
        .map(|(_, digest)| digest)
        .or_else(|| value.strip_prefix("sha256:"))
        .ok_or("worker image must be pinned with @sha256:<64 hex>")?;
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("worker image must be pinned with @sha256:<64 hex>".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn private_tempdir() -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        temp
    }

    #[test]
    fn worker_errors_keep_overload_and_conflict_categories() {
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
        assert_eq!(worker_error(&overload).code, WireErrorCode::Overloaded);
        assert_eq!(worker_error(&conflict).code, WireErrorCode::Conflict);
    }

    #[tokio::test]
    async fn socket_parent_and_stale_cleanup_are_fail_closed() {
        let temp = private_tempdir();
        let insecure = temp.path().join("insecure");
        std::fs::create_dir(&insecure).unwrap();
        std::fs::set_permissions(&insecure, std::fs::Permissions::from_mode(0o755)).unwrap();
        let socket = insecure.join("worker.sock");
        assert!(
            prepare_socket(&socket)
                .await
                .unwrap_err()
                .contains("mode 0700")
        );
        assert_eq!(
            std::fs::metadata(&insecure).unwrap().permissions().mode() & 0o777,
            0o755
        );

        let runtime = temp.path().join("runtime");
        let socket = runtime.join("worker.sock");
        prepare_socket(&socket).await.unwrap();
        assert_eq!(
            std::fs::metadata(&runtime).unwrap().permissions().mode() & 0o777,
            0o700
        );
        std::fs::write(&socket, b"not a socket").unwrap();
        assert!(
            prepare_socket(&socket)
                .await
                .unwrap_err()
                .contains("non-socket")
        );
        std::fs::remove_file(&socket).unwrap();

        let listener = match tokio::net::UnixListener::bind(&socket) {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("bind failed: {error}"),
        };
        assert!(
            prepare_socket(&socket)
                .await
                .unwrap_err()
                .contains("already live")
        );
        drop(listener);
        prepare_socket(&socket).await.unwrap();
        assert!(!socket.exists(), "owned stale socket was removed");
    }

    #[test]
    fn shutdown_cleanup_refuses_identity_swap() {
        let temp = private_tempdir();
        let socket = temp.path().join("worker.sock");
        let listener = match std::os::unix::net::UnixListener::bind(&socket) {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("bind failed: {error}"),
        };
        let metadata = std::fs::symlink_metadata(&socket).unwrap();
        let identity = (metadata.dev(), metadata.ino());
        drop(listener);
        std::fs::remove_file(&socket).unwrap();
        std::fs::write(&socket, b"replacement").unwrap();
        assert!(
            remove_owned_socket(&socket, identity)
                .unwrap_err()
                .contains("identity changed")
        );
        assert!(socket.exists());
    }

    #[test]
    fn production_image_reference_must_be_digest_pinned() {
        assert!(validate_pinned_image(
            "ghcr.io/freeoxen/ox-worker@sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        )
        .is_ok());
        assert!(validate_pinned_image("ghcr.io/freeoxen/ox-worker:latest").is_err());
        assert!(validate_pinned_image("sha256:short").is_err());
    }
}
