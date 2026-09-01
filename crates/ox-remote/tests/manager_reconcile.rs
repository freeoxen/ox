use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use ox_inbox::InboxStore;
use ox_remote::{
    CrashInjector, CrashPoint, DeleteNodeManagerRequest, NodeProvisionSpec, PlacementPolicy,
    RemoteManagerConfig, RemoteManagerError, RemoteManagerStore, StartConversationRequest,
    StorePort, SyncStorePort, VmSpec, VmStatus, WorkerStoreConnector,
};
use structfs_core_store::{Error as StoreError, Path, Record, Value};

fn parsed<T: serde::Serialize>(value: &T) -> Record {
    Record::parsed(structfs_serde_store::to_value(value).unwrap())
}

fn decode<T: serde::de::DeserializeOwned>(record: Record) -> T {
    structfs_serde_store::from_value(record.as_value().unwrap().clone()).unwrap()
}

#[derive(Default)]
struct ProviderState {
    vm: Option<VmStatus>,
    actual_creates: usize,
    actual_deletes: usize,
}

#[derive(Default)]
struct FakeProvider(Mutex<ProviderState>);

#[async_trait]
impl StorePort for FakeProvider {
    async fn read(&self, path: &Path) -> Result<Option<Record>, StoreError> {
        let parts: Vec<&str> = path.iter().map(String::as_str).collect();
        let state = self.0.lock().unwrap();
        match parts.as_slice() {
            ["vms", name] => Ok(state
                .vm
                .as_ref()
                .filter(|vm| vm.vm_name == *name)
                .map(parsed)),
            _ => Ok(None),
        }
    }

    async fn write(&self, path: &Path, record: Record) -> Result<Path, StoreError> {
        let parts: Vec<&str> = path.iter().map(String::as_str).collect();
        let mut state = self.0.lock().unwrap();
        match parts.as_slice() {
            ["vms"] => {
                let spec: VmSpec = decode(record);
                if state.vm.is_none() {
                    state.actual_creates += 1;
                    state.vm = Some(VmStatus {
                        schema_version: 1,
                        vm_name: spec.name.clone(),
                        status: "running".into(),
                        ssh_dest: format!("route@{}", spec.name),
                        ssh_host: "203.0.113.10".into(),
                        ssh_user: Some("route".into()),
                    });
                }
                Path::parse(&format!("vms/{}", spec.name)).map_err(StoreError::from)
            }
            ["vms", name, "delete"] => {
                if state.vm.take().is_some() {
                    state.actual_deletes += 1;
                }
                Path::parse(&format!("vms/{name}/deleted")).map_err(StoreError::from)
            }
            _ => Err(StoreError::NoRoute { path: path.clone() }),
        }
    }
}

struct WorkerState {
    node_id: String,
    attempt_id: String,
    creates: HashMap<String, String>,
    create_effects: usize,
    ledger: Vec<ox_inbox::ledger::LedgerEntry>,
}

struct FakeWorker(Mutex<WorkerState>);

impl FakeWorker {
    fn new(node_id: String, attempt_id: String) -> Self {
        let message = serde_json::json!({"type":"user","content":"durable"});
        Self(Mutex::new(WorkerState {
            node_id,
            attempt_id,
            creates: HashMap::new(),
            create_effects: 0,
            ledger: vec![ox_inbox::ledger::LedgerEntry {
                seq: 0,
                hash: ox_inbox::ledger::entry_hash(&message),
                parent: None,
                msg: message,
            }],
        }))
    }
}

#[async_trait]
impl StorePort for FakeWorker {
    async fn read(&self, path: &Path) -> Result<Option<Record>, StoreError> {
        let parts: Vec<&str> = path.iter().map(String::as_str).collect();
        let state = self.0.lock().unwrap();
        match parts.as_slice() {
            ["health"] => Ok(Some(parsed(&serde_json::json!({
                "status":"ready", "node_id":state.node_id, "attempt_id":state.attempt_id
            })))),
            ["capacity"] => Ok(Some(parsed(&serde_json::json!({
                "active_turns":0, "total_threads":state.creates.len(),
                "limits":{"active_turns":8,"total_threads":256}
            })))),
            ["conversations"] => {
                let values = state
                    .creates
                    .values()
                    .map(|id| {
                        let mut map = BTreeMap::new();
                        map.insert("id".into(), Value::String(id.clone()));
                        map.insert("thread_state".into(), Value::String("running".into()));
                        Value::Map(map)
                    })
                    .collect();
                Ok(Some(Record::parsed(Value::Array(values))))
            }
            ["conversations", thread, "ledger", "from", seq]
                if state.creates.values().any(|id| id == thread) =>
            {
                let seq = seq.parse::<u64>().unwrap();
                let entries: Vec<_> = state
                    .ledger
                    .iter()
                    .filter(|entry| entry.seq >= seq)
                    .cloned()
                    .collect();
                Ok(Some(parsed(&serde_json::json!({
                    "entries": entries,
                    "next_seq": state.ledger.len(),
                    "has_more": false
                }))))
            }
            ["conversations", thread] if state.creates.values().any(|id| id == thread) => Ok(Some(
                parsed(&serde_json::json!({"id":thread,"thread_state":"running"})),
            )),
            _ => Ok(None),
        }
    }

    async fn write(&self, path: &Path, record: Record) -> Result<Path, StoreError> {
        let parts: Vec<&str> = path.iter().map(String::as_str).collect();
        let mut state = self.0.lock().unwrap();
        match parts.as_slice() {
            ["conversations"] => {
                let envelope: ox_inbox::worker_ingress::CreateEnvelope = decode(record);
                let thread = if let Some(thread) = state.creates.get(&envelope.create_id) {
                    thread.clone()
                } else {
                    state.create_effects += 1;
                    let thread = format!("t_{}", state.create_effects);
                    state.creates.insert(envelope.create_id, thread.clone());
                    thread
                };
                Path::parse(&format!("conversations/{thread}")).map_err(StoreError::from)
            }
            _ => Err(StoreError::NoRoute { path: path.clone() }),
        }
    }
}

struct FakeConnector(Mutex<Option<Arc<FakeWorker>>>);

impl FakeConnector {
    fn new() -> Self {
        Self(Mutex::new(None))
    }
}

#[async_trait]
impl WorkerStoreConnector for FakeConnector {
    async fn connect(
        &self,
        node: &ox_inbox::remote_state::RemoteNodeRecord,
    ) -> Result<Arc<dyn StorePort>, StoreError> {
        let mut worker = self.0.lock().unwrap();
        let worker = worker
            .get_or_insert_with(|| {
                Arc::new(FakeWorker::new(
                    node.node_id.clone(),
                    node.node_attempt_id.clone(),
                ))
            })
            .clone();
        Ok(worker)
    }
}

struct CrashOnce {
    point: CrashPoint,
    occurrence: usize,
    seen: Mutex<usize>,
}

impl CrashInjector for CrashOnce {
    fn hit(&self, point: CrashPoint) -> Result<(), RemoteManagerError> {
        let mut seen = self.seen.lock().unwrap();
        if point == self.point {
            *seen += 1;
        }
        if point == self.point && *seen == self.occurrence {
            return Err(RemoteManagerError::InjectedCrash(match point {
                CrashPoint::NodeIntentPersisted => "node intent",
                CrashPoint::OperationIntentPersisted => "operation intent",
                CrashPoint::ExternalEffectReturned => "external effect",
                CrashPoint::ResultCommitted => "result commit",
            }));
        }
        Ok(())
    }
}

fn request() -> StartConversationRequest {
    StartConversationRequest {
        schema_version: 1,
        request_id: "stable-start-request".into(),
        title: "remote".into(),
        prompt: "do work".into(),
        parent_thread_id: None,
        placement: PlacementPolicy::PreferExisting,
        node: NodeProvisionSpec {
            image: "worker@sha256:abc".into(),
            cpu: 2,
            memory_mib: 4096,
            disk_gib: 20,
        },
    }
}

fn config(owner: &str) -> RemoteManagerConfig {
    RemoteManagerConfig {
        reconciler_id: owner.into(),
        lease_seconds: 1,
        provider: "exe.dev".into(),
        ssh_port: 22,
        identity_path: "/tmp/test-identity".into(),
        known_hosts_path: "/tmp/test-known-hosts".into(),
        worker_socket_path: "/tmp/test-worker.sock".into(),
    }
}

#[tokio::test]
async fn crash_after_node_intent_resumes_exact_placement_without_duplicate_effects() {
    let root = tempfile::tempdir().unwrap();
    let local: Arc<dyn StorePort> =
        Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
    let provider = Arc::new(FakeProvider::default());
    let connector = Arc::new(FakeConnector::new());
    let crash = Arc::new(CrashOnce {
        point: CrashPoint::NodeIntentPersisted,
        occurrence: 1,
        seen: Mutex::new(0),
    });
    let manager = RemoteManagerStore::with_crash_injector(
        local.clone(),
        provider.clone(),
        connector.clone(),
        config("process-a"),
        crash,
    )
    .unwrap();
    assert!(matches!(
        manager.start_conversation(request()).await,
        Err(RemoteManagerError::InjectedCrash(_))
    ));
    assert_eq!(provider.0.lock().unwrap().actual_creates, 0);

    let manager = RemoteManagerStore::new(
        local,
        provider.clone(),
        connector.clone(),
        config("process-b"),
    )
    .unwrap();
    let first = manager.start_conversation(request()).await.unwrap();
    let second = manager.start_conversation(request()).await.unwrap();
    assert_eq!(first, second);
    assert_eq!(provider.0.lock().unwrap().actual_creates, 1);
    let worker = connector.0.lock().unwrap().clone().unwrap();
    assert_eq!(worker.0.lock().unwrap().create_effects, 1);
}

#[tokio::test]
async fn provider_effect_crash_replays_against_exact_name_without_duplicate_vm() {
    let root = tempfile::tempdir().unwrap();
    let local: Arc<dyn StorePort> =
        Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
    let provider = Arc::new(FakeProvider::default());
    let connector = Arc::new(FakeConnector::new());
    let crash = Arc::new(CrashOnce {
        point: CrashPoint::ExternalEffectReturned,
        occurrence: 1,
        seen: Mutex::new(0),
    });
    let manager = RemoteManagerStore::with_crash_injector(
        local.clone(),
        provider.clone(),
        connector.clone(),
        config("process-a"),
        crash,
    )
    .unwrap();
    assert!(manager.start_conversation(request()).await.is_err());
    assert_eq!(provider.0.lock().unwrap().actual_creates, 1);

    tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
    let manager = RemoteManagerStore::new(
        local,
        provider.clone(),
        connector.clone(),
        config("process-b"),
    )
    .unwrap();
    manager.start_conversation(request()).await.unwrap();
    assert_eq!(provider.0.lock().unwrap().actual_creates, 1);
}

#[tokio::test]
async fn pending_reconcile_runs_provision_before_create() {
    let root = tempfile::tempdir().unwrap();
    let local: Arc<dyn StorePort> =
        Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
    let provider = Arc::new(FakeProvider::default());
    let connector = Arc::new(FakeConnector::new());
    let crash = Arc::new(CrashOnce {
        point: CrashPoint::OperationIntentPersisted,
        occurrence: 1,
        seen: Mutex::new(0),
    });
    let manager = RemoteManagerStore::with_crash_injector(
        local.clone(),
        provider.clone(),
        connector.clone(),
        config("process-a"),
        crash,
    )
    .unwrap();
    assert!(manager.start_conversation(request()).await.is_err());
    assert_eq!(provider.0.lock().unwrap().actual_creates, 0);

    let manager = RemoteManagerStore::new(
        local,
        provider.clone(),
        connector.clone(),
        config("process-b"),
    )
    .unwrap();
    let report = manager.reconcile_pending().await.unwrap();
    assert_eq!(report.iter().filter(|item| item.applied).count(), 2);
    assert_eq!(provider.0.lock().unwrap().actual_creates, 1);
    assert_eq!(
        connector
            .0
            .lock()
            .unwrap()
            .clone()
            .unwrap()
            .0
            .lock()
            .unwrap()
            .create_effects,
        1
    );
}

#[tokio::test]
async fn worker_create_effect_crash_replays_stable_create_id_once() {
    let root = tempfile::tempdir().unwrap();
    let local: Arc<dyn StorePort> =
        Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
    let provider = Arc::new(FakeProvider::default());
    let connector = Arc::new(FakeConnector::new());
    let provision_commit_crash = Arc::new(CrashOnce {
        point: CrashPoint::ResultCommitted,
        occurrence: 1,
        seen: Mutex::new(0),
    });
    let manager = RemoteManagerStore::with_crash_injector(
        local.clone(),
        provider.clone(),
        connector.clone(),
        config("process-a"),
        provision_commit_crash,
    )
    .unwrap();
    assert!(manager.start_conversation(request()).await.is_err());

    let worker_effect_crash = Arc::new(CrashOnce {
        point: CrashPoint::ExternalEffectReturned,
        occurrence: 1,
        seen: Mutex::new(0),
    });
    let manager = RemoteManagerStore::with_crash_injector(
        local.clone(),
        provider.clone(),
        connector.clone(),
        config("process-b"),
        worker_effect_crash,
    )
    .unwrap();
    assert!(manager.start_conversation(request()).await.is_err());
    assert_eq!(
        connector
            .0
            .lock()
            .unwrap()
            .clone()
            .unwrap()
            .0
            .lock()
            .unwrap()
            .create_effects,
        1
    );
    tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
    let manager =
        RemoteManagerStore::new(local, provider, connector.clone(), config("process-c")).unwrap();
    manager.start_conversation(request()).await.unwrap();
    assert_eq!(
        connector
            .0
            .lock()
            .unwrap()
            .clone()
            .unwrap()
            .0
            .lock()
            .unwrap()
            .create_effects,
        1
    );
}

#[tokio::test]
async fn delete_effect_crash_finishes_absent_vm_from_persisted_affected_refs() {
    let root = tempfile::tempdir().unwrap();
    let local: Arc<dyn StorePort> =
        Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
    let provider = Arc::new(FakeProvider::default());
    let connector = Arc::new(FakeConnector::new());
    let manager = RemoteManagerStore::new(
        local.clone(),
        provider.clone(),
        connector.clone(),
        config("process-a"),
    )
    .unwrap();
    let started = manager.start_conversation(request()).await.unwrap();

    let crash = Arc::new(CrashOnce {
        point: CrashPoint::ExternalEffectReturned,
        occurrence: 1,
        seen: Mutex::new(0),
    });
    let manager = RemoteManagerStore::with_crash_injector(
        local.clone(),
        provider.clone(),
        connector.clone(),
        config("process-b"),
        crash,
    )
    .unwrap();
    let delete = DeleteNodeManagerRequest {
        request_id: "delete-stable".into(),
        delete_id: "delete-1".into(),
        force: true,
    };
    assert!(
        manager
            .delete_node(&started.node_id, delete.clone())
            .await
            .is_err()
    );
    assert_eq!(provider.0.lock().unwrap().actual_deletes, 1);
    tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;

    let manager =
        RemoteManagerStore::new(local, provider.clone(), connector, config("process-c")).unwrap();
    let result = manager.delete_node(&started.node_id, delete).await.unwrap();
    assert!(
        result
            .affected_references
            .contains(&started.conversation_id)
    );
    assert!(
        result
            .affected_references
            .iter()
            .any(|value| value == &format!("worker:{}", started.worker_thread_id))
    );
    assert_eq!(provider.0.lock().unwrap().actual_deletes, 1);
}

#[tokio::test]
async fn concurrent_same_semantic_start_converges_to_one_vm_and_thread() {
    let root = tempfile::tempdir().unwrap();
    let local: Arc<dyn StorePort> =
        Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
    let provider = Arc::new(FakeProvider::default());
    let connector = Arc::new(FakeConnector::new());
    let first = Arc::new(
        RemoteManagerStore::new(
            local.clone(),
            provider.clone(),
            connector.clone(),
            config("process-a"),
        )
        .unwrap(),
    );
    let second = Arc::new(
        RemoteManagerStore::new(
            local.clone(),
            provider.clone(),
            connector.clone(),
            config("process-b"),
        )
        .unwrap(),
    );
    let (left, right) = tokio::join!(
        first.start_conversation(request()),
        second.start_conversation(request())
    );
    assert!(left.is_ok() || right.is_ok());
    let manager = RemoteManagerStore::new(
        local,
        provider.clone(),
        connector.clone(),
        config("process-c"),
    )
    .unwrap();
    manager.start_conversation(request()).await.unwrap();
    assert_eq!(provider.0.lock().unwrap().actual_creates, 1);
    assert_eq!(
        connector
            .0
            .lock()
            .unwrap()
            .clone()
            .unwrap()
            .0
            .lock()
            .unwrap()
            .create_effects,
        1
    );
}

#[tokio::test]
async fn ledger_reconciliation_advances_only_the_committed_validated_cursor() {
    let root = tempfile::tempdir().unwrap();
    let local: Arc<dyn StorePort> =
        Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
    let provider = Arc::new(FakeProvider::default());
    let connector = Arc::new(FakeConnector::new());
    let manager =
        RemoteManagerStore::new(local.clone(), provider, connector, config("process-a")).unwrap();
    let started = manager.start_conversation(request()).await.unwrap();
    manager
        .reconcile_ledger(&started.conversation_id, "ledger-sync-1")
        .await
        .unwrap();
    let conversation_path =
        ox_inbox::remote_state::remote_item_path("conversations", &started.conversation_id)
            .unwrap();
    let cursor = local
        .read(&Path::parse(&format!("{conversation_path}/ledger/cursor")).unwrap())
        .await
        .unwrap()
        .unwrap();
    let Value::Map(map) = cursor.as_value().unwrap() else {
        panic!()
    };
    assert_eq!(map.get("last_seq"), Some(&Value::Integer(0)));
    assert_eq!(
        map.get("last_hash"),
        Some(&Value::String(ox_inbox::ledger::entry_hash(
            &serde_json::json!({"type":"user","content":"durable"})
        )))
    );
}
