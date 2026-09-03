use std::sync::{Arc, Mutex};
use std::time::Duration;

use ox_broker::async_store::{AsyncReader, AsyncWriter, BoxFuture};
use ox_remote::{
    AsyncStorePort, SshWorkerConnector, SshWorkerIdentityVerifier, StorePort, SyncStorePort,
    VmStatus, WorkerIdentityVerifier, WorkerStoreConnector,
};
use ox_structfs_transport::{HostKeyEnrollment, RemoteStoreConfig};
use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer, path};

#[derive(Clone, Default)]
struct MemoryAsyncStore {
    writes: Arc<Mutex<Vec<(Path, Record)>>>,
}

impl AsyncReader for MemoryAsyncStore {
    fn read(&mut self, from: &Path) -> BoxFuture<Result<Option<Record>, StoreError>> {
        let found = self
            .writes
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|(path, _)| path == from)
            .map(|(_, record)| record.clone());
        Box::pin(async move { Ok(found) })
    }
}

impl AsyncWriter for MemoryAsyncStore {
    fn write(&mut self, to: &Path, data: Record) -> BoxFuture<Result<Path, StoreError>> {
        self.writes.lock().unwrap().push((to.clone(), data));
        let to = to.clone();
        Box::pin(async move { Ok(to) })
    }
}

struct PoisonOnce(bool);

impl Reader for PoisonOnce {
    fn read(&mut self, _from: &Path) -> Result<Option<Record>, StoreError> {
        if self.0 {
            self.0 = false;
            panic!("poison synchronous store lock");
        }
        Ok(Some(Record::parsed(Value::String("recovered".into()))))
    }
}

impl Writer for PoisonOnce {
    fn write(&mut self, to: &Path, _data: Record) -> Result<Path, StoreError> {
        Ok(to.clone())
    }
}

fn node_without_route() -> ox_inbox::remote_state::RemoteNodeRecord {
    ox_inbox::remote_state::RemoteNodeRecord {
        node_id: "node".into(),
        node_attempt_id: "attempt".into(),
        provider: "exe.dev".into(),
        vm_name: "ox-node".into(),
        ssh_host: None,
        ssh_port: 22,
        ssh_user: None,
        ssh_dest: None,
        identity_path: "/tmp/id".into(),
        known_hosts_path: "/tmp/known-hosts".into(),
        worker_socket_path: "/run/ox/worker.sock".into(),
        desired_state: "active".into(),
        observed_state: "ready".into(),
        cleanup_state: "none".into(),
        image_digest: Some("worker@sha256:abc".into()),
    }
}

#[tokio::test]
async fn async_store_port_preserves_structfs_records_and_paths() {
    let store = MemoryAsyncStore::default();
    let port = AsyncStorePort::new(store);
    let target = path!("typed/value");
    let record = Record::parsed(Value::String("value".into()));
    assert_eq!(port.write(&target, record.clone()).await.unwrap(), target);
    assert_eq!(
        port.read(&target).await.unwrap().unwrap().as_value(),
        record.as_value()
    );
    assert!(port.read(&path!("missing")).await.unwrap().is_none());
}

#[tokio::test]
async fn sync_store_port_recovers_a_poisoned_lock_for_reads_and_writes() {
    let port = Arc::new(SyncStorePort::new(PoisonOnce(true)));
    let panicking = port.clone();
    assert!(
        tokio::spawn(async move { panicking.read(&path!("value")).await })
            .await
            .is_err()
    );
    assert_eq!(
        port.read(&path!("value"))
            .await
            .unwrap()
            .unwrap()
            .as_value(),
        Some(&Value::String("recovered".into()))
    );
    let target = path!("written");
    assert_eq!(
        port.write(&target, Record::parsed(Value::Null))
            .await
            .unwrap(),
        target
    );
}

#[tokio::test]
async fn production_ssh_adapters_reject_incomplete_typed_routes_before_network_io() {
    let connector = SshWorkerConnector::default();
    assert!(connector.connect(&node_without_route()).await.is_err());
    let mut invalid_port = node_without_route();
    invalid_port.ssh_host = Some("127.0.0.1".into());
    invalid_port.ssh_user = Some("worker".into());
    invalid_port.ssh_port = 0;
    assert!(connector.connect(&invalid_port).await.is_err());

    let mut missing_key = node_without_route();
    missing_key.ssh_host = Some("127.0.0.1".into());
    missing_key.ssh_user = Some("worker".into());
    missing_key.identity_path = "/definitely/missing/ox-worker-key".into();
    assert!(connector.connect(&missing_key).await.is_err());

    let verifier = SshWorkerIdentityVerifier {
        identity_path: "/tmp/id".into(),
        known_hosts_path: "/tmp/known-hosts".into(),
        worker_socket_path: "/run/ox/worker.sock".into(),
        ssh_port: 22,
        enrollment: HostKeyEnrollment::RefuseUnknown,
        inactivity_timeout: Duration::from_secs(1),
        remote: RemoteStoreConfig::default(),
    };
    let vm = VmStatus {
        schema_version: 1,
        vm_name: "ox-node".into(),
        status: "running".into(),
        ssh_dest: String::new(),
        ssh_host: String::new(),
        ssh_user: None,
    };
    assert!(verifier.verify(&vm, "node", "attempt").await.is_err());
    let routed_vm = VmStatus {
        ssh_dest: "worker@127.0.0.1".into(),
        ssh_host: "127.0.0.1".into(),
        ssh_user: Some("worker".into()),
        ..vm
    };
    assert!(
        verifier
            .verify(&routed_vm, "node", "attempt")
            .await
            .is_err()
    );
}
