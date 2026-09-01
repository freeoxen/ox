use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ox_broker::async_store::{AsyncReader, AsyncWriter};
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Writer};
use tokio::sync::Mutex;

use ox_inbox::remote_state::RemoteNodeRecord;
use ox_structfs_transport::{
    HostKeyEnrollment, KnownHosts, RemoteStore, RemoteStoreConfig, WorkerSshConfig,
    connect_worker_ssh,
};

use crate::{ExeError, VmStatus, WorkerHealth, WorkerIdentityVerifier};

#[async_trait]
pub trait StorePort: Send + Sync {
    async fn read(&self, path: &Path) -> Result<Option<Record>, StoreError>;
    async fn write(&self, path: &Path, record: Record) -> Result<Path, StoreError>;
}

/// Adapts an existing repository-local async StructFS Store without exposing
/// any provider- or transport-specific method to the coordinator.
pub struct AsyncStorePort<S> {
    inner: Mutex<S>,
}

impl<S> AsyncStorePort<S> {
    pub fn new(store: S) -> Self {
        Self {
            inner: Mutex::new(store),
        }
    }
}

#[async_trait]
impl<S> StorePort for AsyncStorePort<S>
where
    S: AsyncReader + AsyncWriter + Send,
{
    async fn read(&self, path: &Path) -> Result<Option<Record>, StoreError> {
        let future = {
            let mut store = self.inner.lock().await;
            store.read(path)
        };
        future.await
    }

    async fn write(&self, path: &Path, record: Record) -> Result<Path, StoreError> {
        let future = {
            let mut store = self.inner.lock().await;
            store.write(path, record)
        };
        future.await
    }
}

/// Adapts the existing synchronous `InboxStore` Store boundary. SQLite stays
/// owned by `ox-inbox`; the manager never receives its connection.
pub struct SyncStorePort<S> {
    inner: std::sync::Mutex<S>,
}

impl<S> SyncStorePort<S> {
    pub fn new(store: S) -> Self {
        Self {
            inner: std::sync::Mutex::new(store),
        }
    }
}

#[async_trait]
impl<S> StorePort for SyncStorePort<S>
where
    S: Reader + Writer + Send,
{
    async fn read(&self, path: &Path) -> Result<Option<Record>, StoreError> {
        self.inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .read(path)
    }

    async fn write(&self, path: &Path, record: Record) -> Result<Path, StoreError> {
        self.inner
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .write(path, record)
    }
}

#[async_trait]
pub trait WorkerStoreConnector: Send + Sync {
    async fn connect(&self, node: &RemoteNodeRecord) -> Result<Arc<dyn StorePort>, StoreError>;
}

/// Production connector for the worker's deliberately narrow StructFS-over-SSH
/// endpoint. It can only issue the fixed `ox-worker structfs-stdio` command
/// enforced by `ox-structfs-transport`.
#[derive(Clone, Debug)]
pub struct SshWorkerConnector {
    pub enrollment: HostKeyEnrollment,
    pub inactivity_timeout: Duration,
    pub remote: RemoteStoreConfig,
}

impl Default for SshWorkerConnector {
    fn default() -> Self {
        Self {
            enrollment: HostKeyEnrollment::RefuseUnknown,
            inactivity_timeout: Duration::from_secs(30),
            remote: RemoteStoreConfig::default(),
        }
    }
}

#[async_trait]
impl WorkerStoreConnector for SshWorkerConnector {
    async fn connect(&self, node: &RemoteNodeRecord) -> Result<Arc<dyn StorePort>, StoreError> {
        let ssh = worker_ssh_config(
            node.ssh_host.as_deref(),
            node.ssh_port,
            node.ssh_user.as_deref(),
            node.ssh_dest.as_deref(),
            &node.identity_path,
            &node.known_hosts_path,
            &node.worker_socket_path,
            self.enrollment,
            self.inactivity_timeout,
        )
        .map_err(|error| StoreError::store("SshWorkerConnector", "config", error))?;
        let store = tokio::time::timeout(
            self.inactivity_timeout,
            connect_worker_ssh(ssh, self.remote.clone()),
        )
        .await
        .map_err(|_| {
            StoreError::store(
                "SshWorkerConnector",
                "connect",
                "worker SSH connection timeout",
            )
        })?
        .map_err(|error| StoreError::store("SshWorkerConnector", "connect", error.to_string()))?;
        Ok(Arc::new(RemoteStorePort(store)))
    }
}

/// Provider-side identity verifier used by `ExeControlStore` before a VM is
/// adopted or deleted. It validates node/attempt identity through the same
/// public worker Store used by the coordinator.
#[derive(Clone, Debug)]
pub struct SshWorkerIdentityVerifier {
    pub identity_path: std::path::PathBuf,
    pub known_hosts_path: std::path::PathBuf,
    pub worker_socket_path: std::path::PathBuf,
    pub ssh_port: u16,
    pub enrollment: HostKeyEnrollment,
    pub inactivity_timeout: Duration,
    pub remote: RemoteStoreConfig,
}

#[async_trait]
impl WorkerIdentityVerifier for SshWorkerIdentityVerifier {
    async fn verify(
        &self,
        vm: &VmStatus,
        node_id: &str,
        node_attempt_id: &str,
    ) -> Result<bool, ExeError> {
        let ssh = worker_ssh_config(
            Some(&vm.ssh_host),
            i64::from(self.ssh_port),
            vm.ssh_user.as_deref(),
            Some(&vm.ssh_dest),
            self.identity_path.to_string_lossy().as_ref(),
            self.known_hosts_path.to_string_lossy().as_ref(),
            self.worker_socket_path.to_string_lossy().as_ref(),
            self.enrollment,
            self.inactivity_timeout,
        )
        .map_err(ExeError::Invalid)?;
        let store = tokio::time::timeout(
            self.inactivity_timeout,
            connect_worker_ssh(ssh, self.remote.clone()),
        )
        .await
        .map_err(|_| ExeError::Unavailable("worker SSH connection timeout".into()))?
        .map_err(|error| ExeError::Unavailable(error.to_string()))?;
        let Some(record) = store
            .read_remote(&structfs_core_store::path!("health"))
            .await
            .map_err(|error| ExeError::Unavailable(error.to_string()))?
        else {
            return Ok(false);
        };
        let value = record
            .as_value()
            .cloned()
            .ok_or_else(|| ExeError::Malformed("worker health must be a map".into()))?;
        let health: WorkerHealth = structfs_serde_store::from_value(value)
            .map_err(|error| ExeError::Malformed(error.to_string()))?;
        Ok(health.status == "ready"
            && health.node_id == node_id
            && health.attempt_id == node_attempt_id
            && health.sandbox_enforcement.mode == "required"
            && health.sandbox_enforcement.preflight == "passed")
    }
}

#[derive(Clone)]
struct RemoteStorePort(RemoteStore);

#[async_trait]
impl StorePort for RemoteStorePort {
    async fn read(&self, path: &Path) -> Result<Option<Record>, StoreError> {
        self.0
            .read_remote(path)
            .await
            .map_err(|error| StoreError::store("RemoteStorePort", "read", error.to_string()))
    }

    async fn write(&self, path: &Path, record: Record) -> Result<Path, StoreError> {
        self.0
            .write_remote(path, record)
            .await
            .map_err(|error| StoreError::store("RemoteStorePort", "write", error.to_string()))
    }
}

#[allow(clippy::too_many_arguments)]
fn worker_ssh_config(
    host: Option<&str>,
    port: i64,
    explicit_user: Option<&str>,
    ssh_dest: Option<&str>,
    identity_path: &str,
    known_hosts_path: &str,
    worker_socket_path: &str,
    enrollment: HostKeyEnrollment,
    inactivity_timeout: Duration,
) -> Result<WorkerSshConfig, String> {
    let host = host
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "worker SSH host is unavailable".to_string())?;
    let port = u16::try_from(port).map_err(|_| "worker SSH port is invalid".to_string())?;
    if port == 0 {
        return Err("worker SSH port is invalid".into());
    }
    let user = explicit_user
        .map(str::to_owned)
        .or_else(|| {
            ssh_dest
                .and_then(|destination| destination.split_once('@'))
                .map(|(user, _)| user.to_owned())
        })
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "worker SSH user is unavailable".to_string())?;
    let config = WorkerSshConfig {
        host: host.to_owned(),
        port,
        user,
        identity_file: identity_path.into(),
        known_hosts: KnownHosts::new(known_hosts_path, enrollment),
        socket_path: worker_socket_path.into(),
        inactivity_timeout,
    };
    config.validate().map_err(|error| error.to_string())?;
    Ok(config)
}

#[cfg(test)]
mod ssh_tests {
    use super::*;

    #[test]
    fn worker_user_is_derived_only_from_typed_destination() {
        let config = worker_ssh_config(
            Some("vm.example"),
            22,
            None,
            Some("route+vm@vm.example"),
            "/tmp/id",
            "/tmp/known_hosts",
            "/run/ox/worker.sock",
            HostKeyEnrollment::RefuseUnknown,
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(config.user, "route+vm");
    }

    #[test]
    fn connector_rejects_missing_address_or_user_before_network() {
        assert!(
            worker_ssh_config(
                None,
                22,
                Some("root"),
                None,
                "/tmp/id",
                "/tmp/known_hosts",
                "/run/ox/worker.sock",
                HostKeyEnrollment::RefuseUnknown,
                Duration::from_secs(5),
            )
            .is_err()
        );
        assert!(
            worker_ssh_config(
                Some("vm.example"),
                22,
                None,
                Some("vm.example"),
                "/tmp/id",
                "/tmp/known_hosts",
                "/run/ox/worker.sock",
                HostKeyEnrollment::RefuseUnknown,
                Duration::from_secs(5),
            )
            .is_err()
        );
    }
}
