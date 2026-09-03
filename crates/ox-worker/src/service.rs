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
        Self::start_inner(config, true, None, None, None).await
    }

    /// Build the identical core/public Store without binding a carrier. Useful
    /// for embedding and semantic parity tests; production service mode uses
    /// [`Self::start`].
    pub async fn start_in_process(config: WorkerConfig) -> Result<Self, String> {
        Self::start_inner(config, false, None, None, None).await
    }

    /// Semantic-test constructor using the executor's existing scripted
    /// completion/tool hooks; it still constructs exactly one ExecutionCore.
    pub async fn start_in_process_with_test_hooks(
        config: WorkerConfig,
        transport_factory: Option<ox_executor::test_support::TransportFactory>,
        tool_injector: Option<ox_executor::test_support::ToolInjector>,
    ) -> Result<Self, String> {
        Self::start_inner(config, false, None, transport_factory, tool_injector).await
    }

    /// Bind the real Unix carrier while using an already-verified build
    /// identity. This keeps transport lifecycle tests independent of the host's
    /// sandbox implementation; production callers must use [`Self::start`].
    #[doc(hidden)]
    pub async fn start_bound_with_test_hooks(
        config: WorkerConfig,
        build_identity: WorkerBuildIdentity,
        transport_factory: Option<ox_executor::test_support::TransportFactory>,
        tool_injector: Option<ox_executor::test_support::ToolInjector>,
    ) -> Result<Self, String> {
        Self::start_inner(
            config,
            true,
            Some(build_identity),
            transport_factory,
            tool_injector,
        )
        .await
    }

    async fn start_inner(
        config: WorkerConfig,
        bind_socket: bool,
        build_identity: Option<WorkerBuildIdentity>,
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
        std::fs::create_dir_all(&config.inbox_root)
            .map_err(|error| format!("create worker inbox root: {error}"))?;

        let build_identity = if let Some(build_identity) = build_identity {
            build_identity
        } else if bind_socket {
            let image = std::env::var("OX_WORKER_IMAGE_DIGEST")
                .map_err(|_| "OX_WORKER_IMAGE_DIGEST must be set to a pinned image reference")?;
            validate_pinned_image(&image)?;
            let executable = std::env::current_exe().map_err(|error| error.to_string())?;
            let tool_executor = tool_executor_path(&executable)?;
            ox_executor::remote_sandbox_preflight(
                &config.inbox_root.join("sandbox-preflight"),
                &tool_executor,
            )?;
            verified_build_identity(image, executable_digest()?)
        } else {
            // In-process semantic tests do not bind a remotely reachable
            // carrier. Their health shape remains complete without claiming an
            // image artifact was verified.
            WorkerBuildIdentity {
                executable_digest: executable_digest()?,
                image_digest: "test-in-process@sha256:0000000000000000000000000000000000000000000000000000000000000000".into(),
                sandbox_preflight: "passed".into(),
            }
        };

        let broker = ox_broker::BrokerStore::default();
        // As in the interactive CLI, both handles are the established
        // InboxStore implementation over the same WAL database: one is mounted
        // for Store ingress and one is owned by the sole ExecutionCore.
        let mounted_inbox = ox_inbox::InboxStore::open(&config.inbox_root)
            .map_err(|error| format!("open mounted worker inbox: {error}"))?;
        let core_inbox = ox_inbox::InboxStore::open(&config.inbox_root)
            .map_err(|error| format!("open executor worker inbox: {error}"))?;
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
        )
        .map_err(|error| format!("initialize worker execution core: {error}"))?;
        let execution = Arc::new(core.into_handle(config.command_capacity));
        let public_store = PublicStore::new(
            broker.client(),
            execution.clone(),
            config.inbox_root,
            config.node_id,
            config.attempt_id,
            config.limits,
            build_identity,
        )
        .map_err(|error| format!("initialize worker public store: {error}"))?;
        let (server, socket_identity) = if bind_socket {
            let server = spawn_unix_server(
                &config.socket_path,
                ExportRoot::new(
                    public_store.clone(),
                    structfs_core_store::Path::parse("").expect("empty StructFS root is valid"),
                ),
                config.transport.with_error_mapper(Arc::new(worker_error)),
            )
            .map_err(|error| format!("bind worker Unix carrier: {error}"))?;
            let identity = finalize_bound_socket(&config.socket_path)?;
            (Some(server), Some(identity))
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
            validate_socket_parent(
                metadata.file_type().is_dir(),
                metadata.file_type().is_symlink(),
                metadata.uid(),
                unsafe { libc::geteuid() },
                metadata.permissions().mode(),
            )?;
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
    validate_unlink_candidate(metadata.file_type().is_socket(), metadata.uid(), unsafe {
        libc::geteuid()
    })?;
    match tokio::net::UnixStream::connect(path).await {
        Ok(_) => Err("worker socket is already live".into()),
        Err(error) => {
            validate_connect_error(error)?;
            // Re-read immediately before unlink so a replacement cannot be
            // confused with the stale socket we proved safe above.
            let current = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
            if !same_file(&metadata, &current) {
                return Err("worker socket changed during stale-socket check".into());
            }
            std::fs::remove_file(path).map_err(|error| error.to_string())
        }
    }
}

fn remove_owned_socket(path: &Path, identity: (u64, u64)) -> Result<(), String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    if owns_socket(
        metadata.file_type().is_socket(),
        metadata.uid(),
        unsafe { libc::geteuid() },
        (metadata.dev(), metadata.ino()),
        identity,
    ) {
        std::fs::remove_file(path).map_err(|error| error.to_string())
    } else {
        Err("refusing to remove service path whose identity changed".into())
    }
}

fn same_file(left: &Metadata, right: &Metadata) -> bool {
    same_socket_identity(
        (left.dev(), left.ino()),
        (right.dev(), right.ino()),
        right.file_type().is_socket(),
    )
}

fn validate_socket_metadata(metadata: &Metadata) -> Result<(), String> {
    validate_bound_socket(
        metadata.file_type().is_socket(),
        metadata.uid(),
        unsafe { libc::geteuid() },
        metadata.permissions().mode(),
    )
}

fn finalize_bound_socket(path: &Path) -> Result<(u64, u64), String> {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|error| error.to_string())?;
    let metadata = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    validate_socket_metadata(&metadata)?;
    Ok((metadata.dev(), metadata.ino()))
}

fn validate_socket_parent(
    is_dir: bool,
    is_symlink: bool,
    uid: u32,
    current_uid: u32,
    mode: u32,
) -> Result<(), String> {
    if !is_dir || is_symlink {
        return Err("worker socket parent must be a real directory".into());
    }
    if uid != current_uid || mode & 0o077 != 0 {
        return Err("worker socket parent must be owned by the current uid and mode 0700".into());
    }
    Ok(())
}

fn validate_unlink_candidate(is_socket: bool, uid: u32, current_uid: u32) -> Result<(), String> {
    if !is_socket {
        return Err("refusing to unlink non-socket service path".into());
    }
    if uid != current_uid {
        return Err("refusing to unlink socket owned by another uid".into());
    }
    Ok(())
}

fn validate_connect_error(error: std::io::Error) -> Result<(), String> {
    if matches!(
        error.kind(),
        std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
    ) {
        Ok(())
    } else {
        Err(format!("could not prove worker socket stale: {error}"))
    }
}

fn owns_socket(
    is_socket: bool,
    uid: u32,
    current_uid: u32,
    actual: (u64, u64),
    expected: (u64, u64),
) -> bool {
    is_socket && uid == current_uid && actual == expected
}

fn same_socket_identity(left: (u64, u64), right: (u64, u64), right_is_socket: bool) -> bool {
    left == right && right_is_socket
}

fn validate_bound_socket(
    is_socket: bool,
    uid: u32,
    current_uid: u32,
    mode: u32,
) -> Result<(), String> {
    if !is_socket || uid != current_uid || mode & 0o777 != 0o600 {
        return Err("worker socket ownership or mode verification failed".into());
    }
    Ok(())
}

fn executable_digest() -> Result<String, String> {
    let path = std::env::current_exe().map_err(|error| error.to_string())?;
    let bytes = std::fs::read(path).map_err(|error| error.to_string())?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn tool_executor_path(worker_executable: &Path) -> Result<PathBuf, String> {
    Ok(worker_executable
        .parent()
        .ok_or("worker executable has no parent directory")?
        .join("ox-tool-exec"))
}

fn verified_build_identity(image_digest: String, executable_digest: String) -> WorkerBuildIdentity {
    WorkerBuildIdentity {
        executable_digest,
        image_digest,
        sandbox_preflight: "passed".into(),
    }
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

/// Narrow hooks for exercising fail-closed boundary behavior without counting
/// test harness code as worker production coverage.
#[doc(hidden)]
pub mod test_support {
    use super::*;

    pub fn map_error(error: &structfs_core_store::Error) -> WireError {
        worker_error(error)
    }

    pub async fn prepare_socket(path: &Path) -> Result<(), String> {
        super::prepare_socket(path).await
    }

    pub fn remove_owned_socket(path: &Path, identity: (u64, u64)) -> Result<(), String> {
        super::remove_owned_socket(path, identity)
    }

    pub fn same_file(left: &Metadata, right: &Metadata) -> bool {
        super::same_file(left, right)
    }

    pub fn validate_socket_metadata(metadata: &Metadata) -> Result<(), String> {
        super::validate_socket_metadata(metadata)
    }

    pub fn finalize_bound_socket(path: &Path) -> Result<(u64, u64), String> {
        super::finalize_bound_socket(path)
    }

    pub fn validate_socket_parent(
        is_dir: bool,
        is_symlink: bool,
        uid: u32,
        current_uid: u32,
        mode: u32,
    ) -> Result<(), String> {
        super::validate_socket_parent(is_dir, is_symlink, uid, current_uid, mode)
    }

    pub fn validate_unlink_candidate(
        is_socket: bool,
        uid: u32,
        current_uid: u32,
    ) -> Result<(), String> {
        super::validate_unlink_candidate(is_socket, uid, current_uid)
    }

    pub fn validate_connect_error(kind: std::io::ErrorKind) -> Result<(), String> {
        super::validate_connect_error(std::io::Error::from(kind))
    }

    pub fn owns_socket(
        is_socket: bool,
        uid: u32,
        current_uid: u32,
        actual: (u64, u64),
        expected: (u64, u64),
    ) -> bool {
        super::owns_socket(is_socket, uid, current_uid, actual, expected)
    }

    pub fn same_socket_identity(
        left: (u64, u64),
        right: (u64, u64),
        right_is_socket: bool,
    ) -> bool {
        super::same_socket_identity(left, right, right_is_socket)
    }

    pub fn validate_bound_socket(
        is_socket: bool,
        uid: u32,
        current_uid: u32,
        mode: u32,
    ) -> Result<(), String> {
        super::validate_bound_socket(is_socket, uid, current_uid, mode)
    }

    pub fn executable_digest() -> Result<String, String> {
        super::executable_digest()
    }

    pub fn validate_pinned_image(value: &str) -> Result<(), String> {
        super::validate_pinned_image(value)
    }

    pub fn tool_executor_path(worker_executable: &Path) -> Result<PathBuf, String> {
        super::tool_executor_path(worker_executable)
    }

    pub fn verified_build_identity(
        image_digest: String,
        executable_digest: String,
    ) -> WorkerBuildIdentity {
        super::verified_build_identity(image_digest, executable_digest)
    }

    pub fn with_socket_identity(
        mut service: WorkerService,
        socket_path: PathBuf,
        identity: (u64, u64),
    ) -> WorkerService {
        service.socket_path = socket_path;
        service.socket_identity = Some(identity);
        service
    }
}
