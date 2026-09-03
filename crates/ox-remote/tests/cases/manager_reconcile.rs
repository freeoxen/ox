use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use ox_broker::async_store::{AsyncReader, AsyncWriter};
use ox_inbox::InboxStore;
use ox_inbox::remote_state::{
    RemoteAction, RemoteCleanupState, RemoteNodeDesiredState, RemoteNodeIntent,
    RemoteNodeObservedState, RemoteNodeUpdate, RemoteOperationIntent, RemoteOperationRecord,
    RemoteOperationResult, RemoteOperationState,
};
use ox_remote::{
    ApprovalRequest, CancelRequest, CrashInjector, CrashPoint, CreateNodeRequest,
    DeleteNodeManagerRequest, MessageRequest, NodeProvisionSpec, PlacementPolicy,
    RemoteManagerConfig, RemoteManagerError, RemoteManagerStore, StartConversationRequest,
    StorePort, SyncStorePort, VmSpec, VmStatus, WorkerStoreConnector,
};
use sha2::Digest;
use structfs_core_store::{Error as StoreError, Format, Path, Record, Value, path};

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
    suppress_create: bool,
    suppress_delete: bool,
    read_errors: HashSet<String>,
    write_errors: HashSet<String>,
    read_sequences: HashMap<String, VecDeque<ReadOverride>>,
}

#[derive(Default)]
struct FakeProvider(Mutex<ProviderState>);

#[async_trait]
impl StorePort for FakeProvider {
    async fn read(&self, path: &Path) -> Result<Option<Record>, StoreError> {
        let parts: Vec<&str> = path.iter().map(String::as_str).collect();
        let mut state = self.0.lock().unwrap();
        if let Some(behavior) = state
            .read_sequences
            .get_mut(&path.to_string())
            .and_then(VecDeque::pop_front)
        {
            return match behavior {
                ReadOverride::Missing => Ok(None),
                ReadOverride::Value(value) => Ok(Some(Record::parsed(value))),
                ReadOverride::Raw(bytes) => Ok(Some(Record::raw(bytes, Format::OCTET_STREAM))),
                ReadOverride::Error => Err(StoreError::store(
                    "FakeProvider",
                    "read",
                    "injected sequenced failure",
                )),
            };
        }
        if state.read_errors.contains(&path.to_string()) {
            return Err(StoreError::store(
                "FakeProvider",
                "read",
                "injected failure",
            ));
        }
        match parts.as_slice() {
            ["identity"] => Ok(Some(parsed(&serde_json::json!({
                "schema_version": 1,
                "authenticated": true
            })))),
            ["vms"] => Ok(Some(parsed(&state.vm.iter().cloned().collect::<Vec<_>>()))),
            ["vms", component] => {
                let name = ox_remote::decode_vm_component(component).map_err(|error| {
                    StoreError::store("FakeProvider", "decode", error.to_string())
                })?;
                Ok(state
                    .vm
                    .as_ref()
                    .filter(|vm| vm.vm_name == name)
                    .map(parsed))
            }
            _ => Ok(None),
        }
    }

    async fn write(&self, path: &Path, record: Record) -> Result<Path, StoreError> {
        let parts: Vec<&str> = path.iter().map(String::as_str).collect();
        let mut state = self.0.lock().unwrap();
        if state.write_errors.contains(&path.to_string()) {
            return Err(StoreError::store(
                "FakeProvider",
                "write",
                "injected failure",
            ));
        }
        match parts.as_slice() {
            ["vms"] => {
                let spec: VmSpec = decode(record);
                if state.vm.is_none() && !state.suppress_create {
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
                ox_remote::vm_path(&spec.name)
                    .map_err(|error| StoreError::store("FakeProvider", "path", error.to_string()))
            }
            ["vms", component, "delete"] => {
                let name = ox_remote::decode_vm_component(component).map_err(|error| {
                    StoreError::store("FakeProvider", "decode", error.to_string())
                })?;
                if !state.suppress_delete && state.vm.take().is_some() {
                    state.actual_deletes += 1;
                }
                let item = ox_remote::vm_path(&name).map_err(|error| {
                    StoreError::store("FakeProvider", "path", error.to_string())
                })?;
                Path::parse(&format!("{item}/deleted")).map_err(StoreError::from)
            }
            _ => Err(StoreError::NoRoute { path: path.clone() }),
        }
    }
}

struct WorkerState {
    node_id: String,
    attempt_id: String,
    image_digest: String,
    creates: HashMap<String, String>,
    create_effects: usize,
    ledger: Vec<ox_inbox::ledger::LedgerEntry>,
    thread_states: HashMap<String, String>,
    cancel_ids: std::collections::HashSet<String>,
    cancel_effects: usize,
    read_overrides: HashMap<String, ReadOverride>,
    read_sequences: HashMap<String, VecDeque<ReadOverride>>,
    write_errors: HashSet<String>,
    ledger_batch_size: usize,
}

#[derive(Clone)]
enum ReadOverride {
    Missing,
    Value(Value),
    Raw(Vec<u8>),
    Error,
}

struct FaultStore {
    inner: Arc<dyn StorePort>,
    reads: Mutex<HashMap<String, ReadOverride>>,
    read_sequences: Mutex<HashMap<String, VecDeque<ReadOverride>>>,
    write_errors: Mutex<HashSet<String>>,
}

impl FaultStore {
    fn new(inner: Arc<dyn StorePort>) -> Self {
        Self {
            inner,
            reads: Mutex::new(HashMap::new()),
            read_sequences: Mutex::new(HashMap::new()),
            write_errors: Mutex::new(HashSet::new()),
        }
    }
}

#[async_trait]
impl StorePort for FaultStore {
    async fn read(&self, path: &Path) -> Result<Option<Record>, StoreError> {
        if let Some(behavior) = self
            .read_sequences
            .lock()
            .unwrap()
            .get_mut(&path.to_string())
            .and_then(VecDeque::pop_front)
        {
            return match behavior {
                ReadOverride::Missing => Ok(None),
                ReadOverride::Value(value) => Ok(Some(Record::parsed(value))),
                ReadOverride::Raw(bytes) => Ok(Some(Record::raw(bytes, Format::OCTET_STREAM))),
                ReadOverride::Error => {
                    Err(StoreError::store("FaultStore", "read", "injected failure"))
                }
            };
        }
        if let Some(behavior) = self.reads.lock().unwrap().get(&path.to_string()) {
            return match behavior {
                ReadOverride::Missing => Ok(None),
                ReadOverride::Value(value) => Ok(Some(Record::parsed(value.clone()))),
                ReadOverride::Raw(bytes) => {
                    Ok(Some(Record::raw(bytes.clone(), Format::OCTET_STREAM)))
                }
                ReadOverride::Error => {
                    Err(StoreError::store("FaultStore", "read", "injected failure"))
                }
            };
        }
        self.inner.read(path).await
    }

    async fn write(&self, path: &Path, record: Record) -> Result<Path, StoreError> {
        if self
            .write_errors
            .lock()
            .unwrap()
            .contains(&path.to_string())
        {
            return Err(StoreError::store("FaultStore", "write", "injected failure"));
        }
        self.inner.write(path, record).await
    }
}

struct FakeWorker(Mutex<WorkerState>);

impl FakeWorker {
    fn new(node_id: String, attempt_id: String, image_digest: String) -> Self {
        let message = serde_json::json!({"type":"user","content":"durable"});
        Self(Mutex::new(WorkerState {
            node_id,
            attempt_id,
            image_digest,
            creates: HashMap::new(),
            create_effects: 0,
            ledger: vec![ox_inbox::ledger::LedgerEntry {
                seq: 0,
                hash: ox_inbox::ledger::entry_hash(&message),
                parent: None,
                msg: message,
            }],
            thread_states: HashMap::new(),
            cancel_ids: std::collections::HashSet::new(),
            cancel_effects: 0,
            read_overrides: HashMap::new(),
            read_sequences: HashMap::new(),
            write_errors: HashSet::new(),
            ledger_batch_size: usize::MAX,
        }))
    }
}

#[async_trait]
impl StorePort for FakeWorker {
    async fn read(&self, path: &Path) -> Result<Option<Record>, StoreError> {
        let parts: Vec<&str> = path.iter().map(String::as_str).collect();
        let mut state = self.0.lock().unwrap();
        if let Some(behavior) = state
            .read_sequences
            .get_mut(&path.to_string())
            .and_then(VecDeque::pop_front)
        {
            return match behavior {
                ReadOverride::Missing => Ok(None),
                ReadOverride::Value(value) => Ok(Some(Record::parsed(value))),
                ReadOverride::Raw(bytes) => Ok(Some(Record::raw(bytes, Format::OCTET_STREAM))),
                ReadOverride::Error => Err(StoreError::store(
                    "FakeWorker",
                    "read",
                    "injected sequenced failure",
                )),
            };
        }
        if let Some(behavior) = state.read_overrides.get(&path.to_string()) {
            return match behavior {
                ReadOverride::Missing => Ok(None),
                ReadOverride::Value(value) => Ok(Some(Record::parsed(value.clone()))),
                ReadOverride::Raw(bytes) => {
                    Ok(Some(Record::raw(bytes.clone(), Format::OCTET_STREAM)))
                }
                ReadOverride::Error => {
                    Err(StoreError::store("FakeWorker", "read", "injected failure"))
                }
            };
        }
        match parts.as_slice() {
            ["health"] => Ok(Some(parsed(&serde_json::json!({
                "status":"ready", "node_id":state.node_id, "attempt_id":state.attempt_id,
                "worker_version":"0.1.0", "wire_version":1,
                "image_digest":state.image_digest,
                "agent_wasm_sha256":"agent", "executable_sha256":"executable",
                "policy_profile":"clash_remote_enforced", "policy_contract_sha256":"policy",
                "sandbox_enforcement":{"mode":"required","preflight":"passed"}
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
                        map.insert(
                            "thread_state".into(),
                            Value::String(
                                state
                                    .thread_states
                                    .get(id)
                                    .cloned()
                                    .unwrap_or_else(|| "running".into()),
                            ),
                        );
                        Value::Map(map)
                    })
                    .collect();
                Ok(Some(Record::parsed(Value::Array(values))))
            }
            ["conversations", thread, "ledger", "from", seq]
                if state.creates.values().any(|id| id == thread) =>
            {
                let seq = seq.parse::<u64>().unwrap();
                let matching: Vec<_> = state
                    .ledger
                    .iter()
                    .filter(|entry| entry.seq >= seq)
                    .cloned()
                    .collect();
                let has_more = matching.len() > state.ledger_batch_size;
                let entries: Vec<_> = matching.into_iter().take(state.ledger_batch_size).collect();
                Ok(Some(parsed(&serde_json::json!({
                    "entries": entries,
                    "next_seq": state.ledger.len(),
                    "has_more": has_more
                }))))
            }
            ["conversations", thread] if state.creates.values().any(|id| id == thread) => {
                Ok(Some(parsed(&serde_json::json!({
                    "id":thread,
                    "thread_state":state.thread_states.get(*thread).cloned().unwrap_or_else(|| "running".into())
                }))))
            }
            _ => Ok(None),
        }
    }

    async fn write(&self, path: &Path, record: Record) -> Result<Path, StoreError> {
        let parts: Vec<&str> = path.iter().map(String::as_str).collect();
        let mut state = self.0.lock().unwrap();
        if state.write_errors.contains(&path.to_string()) {
            return Err(StoreError::store("FakeWorker", "write", "injected failure"));
        }
        match parts.as_slice() {
            ["conversations"] => {
                let envelope: ox_inbox::worker_ingress::CreateEnvelope = decode(record);
                let thread = if let Some(thread) = state.creates.get(&envelope.create_id) {
                    thread.clone()
                } else {
                    state.create_effects += 1;
                    let thread = format!("t_{}", state.create_effects);
                    state.creates.insert(envelope.create_id, thread.clone());
                    state.thread_states.insert(thread.clone(), "running".into());
                    thread
                };
                Path::parse(&format!("conversations/{thread}")).map_err(StoreError::from)
            }
            ["conversations", thread, "control", "cancel"]
                if state.creates.values().any(|id| id == thread) =>
            {
                let envelope: ox_inbox::worker_ingress::CancelEnvelope = decode(record);
                if state.cancel_ids.insert(envelope.cancel_id.clone()) {
                    state.cancel_effects += 1;
                    state
                        .thread_states
                        .insert((*thread).to_owned(), "interrupted".into());
                }
                Path::parse(&format!("conversations/{thread}/cancellations/cancel_1"))
                    .map_err(StoreError::from)
            }
            ["conversations", thread, "messages"]
                if state.creates.values().any(|id| id == thread) =>
            {
                let envelope: ox_inbox::worker_ingress::PromptEnvelope = decode(record);
                Path::parse(&format!(
                    "conversations/{thread}/messages/{}",
                    envelope.message_id
                ))
                .map_err(StoreError::from)
            }
            ["conversations", thread, "approvals", approval_id]
                if state.creates.values().any(|id| id == thread) =>
            {
                let _: ox_types::ApprovalResponse = decode(record);
                Path::parse(&format!("conversations/{thread}/approvals/{approval_id}"))
                    .map_err(StoreError::from)
            }
            _ => Err(StoreError::NoRoute { path: path.clone() }),
        }
    }
}

struct FakeConnector(
    Mutex<Option<Arc<FakeWorker>>>,
    AtomicBool,
    AtomicUsize,
    AtomicUsize,
);

impl FakeConnector {
    fn new() -> Self {
        Self(
            Mutex::new(None),
            AtomicBool::new(false),
            AtomicUsize::new(0),
            AtomicUsize::new(0),
        )
    }
}

#[async_trait]
impl WorkerStoreConnector for FakeConnector {
    async fn connect(
        &self,
        node: &ox_inbox::remote_state::RemoteNodeRecord,
    ) -> Result<Arc<dyn StorePort>, StoreError> {
        let connect_number = self.2.fetch_add(1, Ordering::SeqCst) + 1;
        if self.1.load(Ordering::SeqCst) || self.3.load(Ordering::SeqCst) == connect_number {
            return Err(StoreError::store(
                "FakeConnector",
                "connect",
                "injected failure",
            ));
        }
        let mut worker = self.0.lock().unwrap();
        let worker = worker
            .get_or_insert_with(|| {
                Arc::new(FakeWorker::new(
                    node.node_id.clone(),
                    node.node_attempt_id.clone(),
                    node.image_digest.clone().unwrap_or_default(),
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
                CrashPoint::ProjectionCommitted => "projection commit",
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
        lease_seconds: 5,
        provider: "exe.dev".into(),
        ssh_port: 22,
        identity_path: "/tmp/test-identity".into(),
        known_hosts_path: "/tmp/test-known-hosts".into(),
        worker_socket_path: "/tmp/test-worker.sock".into(),
    }
}

#[tokio::test]
async fn manager_structfs_routes_cover_node_conversation_control_and_observation() {
    let root = tempfile::tempdir().unwrap();
    let local: Arc<dyn StorePort> =
        Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
    let provider = Arc::new(FakeProvider::default());
    let connector = Arc::new(FakeConnector::new());
    let mut manager =
        RemoteManagerStore::new(local, provider, connector, config("store-routes")).unwrap();

    let node_request = CreateNodeRequest {
        schema_version: 1,
        request_id: "store_node_request".into(),
        node: request().node,
    };
    let node_path = manager
        .write(&path!("nodes"), parsed(&node_request))
        .await
        .unwrap();
    let node_id = node_path.iter().nth(1).unwrap().clone();
    assert!(manager.read(&path!("nodes")).await.unwrap().is_some());
    let node_storage = ox_inbox::remote_state::remote_item_path("nodes", &node_id).unwrap();
    assert!(manager.read(&node_storage).await.unwrap().is_some());

    let doctor = Path::parse(&format!("nodes/{node_id}/doctor")).unwrap();
    assert!(manager.read(&doctor).await.unwrap().is_some());
    assert!(
        manager
            .read(&path!("doctor/provider"))
            .await
            .unwrap()
            .is_some()
    );

    let mut start = request();
    start.request_id = "store_start_request".into();
    start.placement = PlacementPolicy::RequireNode {
        node_id: node_id.clone(),
    };
    let conversation_path = manager
        .write(&path!("conversations"), parsed(&start))
        .await
        .unwrap();
    let conversation_id = conversation_path.iter().nth(1).unwrap().clone();
    assert!(
        manager
            .read(&path!("conversations"))
            .await
            .unwrap()
            .is_some()
    );
    let conversation_storage =
        ox_inbox::remote_state::remote_item_path("conversations", &conversation_id).unwrap();
    assert!(manager.read(&conversation_storage).await.unwrap().is_some());

    let message_path = Path::parse(&format!("conversations/{conversation_id}/messages")).unwrap();
    let message_receipt = manager
        .write(
            &message_path,
            parsed(&MessageRequest {
                request_id: "store_message_request".into(),
                message_id: "message_route".into(),
                content: "continue".into(),
            }),
        )
        .await
        .unwrap();
    assert!(message_receipt.to_string().contains("message_route"));

    let approval_path = Path::parse(&format!("conversations/{conversation_id}/approvals")).unwrap();
    let approval_receipt = manager
        .write(
            &approval_path,
            parsed(&ApprovalRequest {
                request_id: "store_approval_request".into(),
                approval_id: "approval_route".into(),
                decision: ox_types::Decision::DenyOnce,
            }),
        )
        .await
        .unwrap();
    assert!(approval_receipt.to_string().contains("approval_route"));

    let ledger_path = Path::parse(&format!("conversations/{conversation_id}/reconcile")).unwrap();
    assert_eq!(
        manager
            .write(
                &ledger_path,
                parsed(&serde_json::json!({"request_id":"store_ledger_request"})),
            )
            .await
            .unwrap(),
        Path::parse(&format!("conversations/{conversation_id}/ledger")).unwrap()
    );
    assert!(
        manager
            .write(&ledger_path, parsed(&serde_json::json!({})))
            .await
            .is_err()
    );

    let refresh = Path::parse(&format!("conversations/{conversation_id}/refresh")).unwrap();
    assert!(manager.read(&refresh).await.unwrap().is_some());
    let cancel_path = Path::parse(&format!("conversations/{conversation_id}/cancel")).unwrap();
    manager
        .write(
            &cancel_path,
            parsed(&CancelRequest {
                request_id: "store_cancel_request".into(),
                cancel_id: "cancel_route".into(),
                reason: None,
            }),
        )
        .await
        .unwrap();
    assert!(manager.read(&refresh).await.unwrap().is_some());

    let drain_path = Path::parse(&format!("nodes/{node_id}/drain")).unwrap();
    assert_eq!(
        manager
            .write(&drain_path, Record::parsed(Value::Null))
            .await
            .unwrap(),
        node_path
    );
    assert_eq!(
        manager
            .write(&path!("reconcile"), Record::parsed(Value::Null))
            .await
            .unwrap(),
        path!("reconcile")
    );
    let delete_path = Path::parse(&format!("nodes/{node_id}/delete")).unwrap();
    assert_eq!(
        manager
            .write(
                &delete_path,
                parsed(&DeleteNodeManagerRequest {
                    request_id: "store_delete_request".into(),
                    delete_id: "store_delete".into(),
                    force: false,
                }),
            )
            .await
            .unwrap(),
        node_path
    );
    assert!(
        manager
            .write(&path!("unsupported"), Record::parsed(Value::Null))
            .await
            .is_err()
    );
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

    tokio::time::sleep(std::time::Duration::from_millis(5_100)).await;
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
    tokio::time::sleep(std::time::Duration::from_millis(5_100)).await;
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
    tokio::time::sleep(std::time::Duration::from_millis(5_100)).await;

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
async fn non_force_delete_refuses_active_local_and_worker_references() {
    let root = tempfile::tempdir().unwrap();
    let local: Arc<dyn StorePort> =
        Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
    let provider = Arc::new(FakeProvider::default());
    let connector = Arc::new(FakeConnector::new());
    let manager =
        RemoteManagerStore::new(local, provider.clone(), connector, config("active-delete"))
            .unwrap();
    let started = manager.start_conversation(request()).await.unwrap();
    let delete = DeleteNodeManagerRequest {
        request_id: "delete-active-request".into(),
        delete_id: "delete-active".into(),
        force: false,
    };

    let error = manager
        .delete_node(&started.node_id, delete.clone())
        .await
        .unwrap_err();
    let RemoteManagerError::ActiveReferences(references) = error else {
        panic!("expected active references refusal, got {error:?}");
    };
    assert!(references.contains(&started.conversation_id));
    assert!(
        references
            .iter()
            .any(|value| value == &format!("worker:{}", started.worker_thread_id))
    );
    assert_eq!(provider.0.lock().unwrap().actual_deletes, 0);

    assert!(matches!(
        manager.delete_node(&started.node_id, delete).await,
        Err(RemoteManagerError::ActiveReferences(_))
    ));
    assert_eq!(provider.0.lock().unwrap().actual_deletes, 0);
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

#[tokio::test]
async fn repeated_ids_reject_changed_node_and_conversation_intents() {
    let root = tempfile::tempdir().unwrap();
    let local: Arc<dyn StorePort> =
        Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
    let provider = Arc::new(FakeProvider::default());
    let connector = Arc::new(FakeConnector::new());
    let manager = RemoteManagerStore::new(local, provider, connector, config("conflicts")).unwrap();

    let node = CreateNodeRequest {
        schema_version: 1,
        request_id: "stable-node".into(),
        node: request().node,
    };
    manager.create_node(node.clone()).await.unwrap();
    let mut changed_node = node;
    changed_node.node.cpu += 1;
    assert!(
        manager
            .create_node(changed_node)
            .await
            .unwrap_err()
            .to_string()
            .contains("conflict")
    );

    manager.start_conversation(request()).await.unwrap();
    let mut changed_conversation = request();
    changed_conversation.prompt = "different work".into();
    assert!(
        manager
            .start_conversation(changed_conversation)
            .await
            .unwrap_err()
            .to_string()
            .contains("conflict")
    );
}

#[tokio::test]
async fn ready_node_retry_closes_pending_provision_receipt() {
    let root = tempfile::tempdir().unwrap();
    let local: Arc<dyn StorePort> =
        Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
    let provider = Arc::new(FakeProvider::default());
    let connector = Arc::new(FakeConnector::new());
    let request = CreateNodeRequest {
        schema_version: 1,
        request_id: "ready-pending-node".into(),
        node: request().node,
    };
    let manager = RemoteManagerStore::with_crash_injector(
        local.clone(),
        provider.clone(),
        connector.clone(),
        config("ready-node-crash"),
        Arc::new(CrashOnce {
            point: CrashPoint::ProjectionCommitted,
            occurrence: 1,
            seen: Mutex::new(0),
        }),
    )
    .unwrap();
    assert!(manager.create_node(request.clone()).await.is_err());
    let node = manager.list_nodes().await.unwrap().pop().unwrap();
    assert_eq!(node.observed_state, "ready");

    tokio::time::sleep(std::time::Duration::from_millis(5_100)).await;
    let manager = RemoteManagerStore::new(
        local.clone(),
        provider.clone(),
        connector,
        config("ready-node-retry"),
    )
    .unwrap();
    manager.create_node(request).await.unwrap();
    let pending = local
        .read(&Path::parse("remote/operations/pending").unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pending.as_value(), Some(&Value::Array(vec![])));
    assert_eq!(provider.0.lock().unwrap().actual_creates, 1);
}

#[tokio::test]
async fn bound_conversation_retry_closes_pending_create_receipt() {
    let root = tempfile::tempdir().unwrap();
    let local: Arc<dyn StorePort> =
        Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
    let provider = Arc::new(FakeProvider::default());
    let connector = Arc::new(FakeConnector::new());
    let manager = RemoteManagerStore::new(
        local.clone(),
        provider.clone(),
        connector.clone(),
        config("bound-node"),
    )
    .unwrap();
    let node = manager
        .create_node(CreateNodeRequest {
            schema_version: 1,
            request_id: "bound-node".into(),
            node: request().node,
        })
        .await
        .unwrap();
    let mut conversation_request = request();
    conversation_request.request_id = "bound-pending-conversation".into();
    conversation_request.placement = PlacementPolicy::RequireNode {
        node_id: node.node_id,
    };
    let manager = RemoteManagerStore::with_crash_injector(
        local.clone(),
        provider,
        connector.clone(),
        config("bound-conversation-crash"),
        Arc::new(CrashOnce {
            point: CrashPoint::ProjectionCommitted,
            occurrence: 1,
            seen: Mutex::new(0),
        }),
    )
    .unwrap();
    assert!(
        manager
            .start_conversation(conversation_request.clone())
            .await
            .is_err()
    );
    let bound = manager.list_conversations().await.unwrap().pop().unwrap();
    assert_eq!(bound.observed_state, "running");
    assert!(bound.worker_thread_id.is_some());

    tokio::time::sleep(std::time::Duration::from_millis(5_100)).await;
    let manager = RemoteManagerStore::new(
        local.clone(),
        Arc::new(FakeProvider::default()),
        connector.clone(),
        config("bound-conversation-retry"),
    )
    .unwrap();
    manager
        .start_conversation(conversation_request)
        .await
        .unwrap();
    let pending = local
        .read(&Path::parse("remote/operations/pending").unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pending.as_value(), Some(&Value::Array(vec![])));
    assert_eq!(
        connector
            .0
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .0
            .lock()
            .unwrap()
            .create_effects,
        1
    );
}

#[tokio::test]
async fn idle_ledger_poll_reuses_one_pending_cursor_operation() {
    let root = tempfile::tempdir().unwrap();
    let local: Arc<dyn StorePort> =
        Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
    let manager = RemoteManagerStore::new(
        local.clone(),
        Arc::new(FakeProvider::default()),
        Arc::new(FakeConnector::new()),
        config("idle-ledger"),
    )
    .unwrap();
    let started = manager.start_conversation(request()).await.unwrap();
    manager
        .reconcile_ledger(&started.conversation_id, "ignored-one")
        .await
        .unwrap();
    for _ in 0..3 {
        manager
            .reconcile_ledger(&started.conversation_id, "ignored-retry")
            .await
            .unwrap();
    }
    let pending = local
        .read(&Path::parse("remote/operations/pending").unwrap())
        .await
        .unwrap()
        .unwrap();
    let Value::Array(pending) = pending.as_value().unwrap() else {
        panic!("pending operations was not an array")
    };
    assert_eq!(
        pending.len(),
        1,
        "idle polling must reuse the cursor intent"
    );
}

#[tokio::test]
async fn cancel_intent_survives_effect_crash_and_refreshes_interrupted_as_canceled() {
    let root = tempfile::tempdir().unwrap();
    let local: Arc<dyn StorePort> =
        Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
    let provider = Arc::new(FakeProvider::default());
    let connector = Arc::new(FakeConnector::new());
    let manager = RemoteManagerStore::new(
        local.clone(),
        provider.clone(),
        connector.clone(),
        config("cancel-start"),
    )
    .unwrap();
    let started = manager.start_conversation(request()).await.unwrap();
    let cancel = CancelRequest {
        request_id: "stable-cancel-request".into(),
        cancel_id: "stable-cancel".into(),
        reason: Some("test".into()),
    };
    let crash = Arc::new(CrashOnce {
        point: CrashPoint::ExternalEffectReturned,
        occurrence: 1,
        seen: Mutex::new(0),
    });
    let manager = RemoteManagerStore::with_crash_injector(
        local.clone(),
        provider.clone(),
        connector.clone(),
        config("cancel-crash"),
        crash,
    )
    .unwrap();
    assert!(
        manager
            .cancel(&started.conversation_id, cancel.clone())
            .await
            .is_err()
    );
    assert_eq!(
        manager
            .get_conversation(&started.conversation_id)
            .await
            .unwrap()
            .unwrap()
            .desired_state,
        "canceled"
    );

    tokio::time::sleep(std::time::Duration::from_millis(5_100)).await;
    let manager =
        RemoteManagerStore::new(local, provider, connector.clone(), config("cancel-retry"))
            .unwrap();
    manager
        .cancel(&started.conversation_id, cancel)
        .await
        .unwrap();
    let refreshed = manager
        .refresh_conversation(&started.conversation_id)
        .await
        .unwrap();
    assert_eq!(refreshed.observed_state, "canceled");
    assert_eq!(
        connector
            .0
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .0
            .lock()
            .unwrap()
            .cancel_effects,
        1
    );
}

fn node_request(id: &str) -> CreateNodeRequest {
    CreateNodeRequest {
        schema_version: 1,
        request_id: id.into(),
        node: request().node,
    }
}

async fn started_fixture(
    owner: &str,
) -> (
    tempfile::TempDir,
    Arc<dyn StorePort>,
    Arc<FakeProvider>,
    Arc<FakeConnector>,
    RemoteManagerStore,
    ox_remote::StartConversationResult,
) {
    let root = tempfile::tempdir().unwrap();
    let local: Arc<dyn StorePort> =
        Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
    let provider = Arc::new(FakeProvider::default());
    let connector = Arc::new(FakeConnector::new());
    let mut manager_config = config(owner);
    manager_config.lease_seconds = 10;
    let manager = RemoteManagerStore::new(
        local.clone(),
        provider.clone(),
        connector.clone(),
        manager_config,
    )
    .unwrap();
    let mut start = request();
    start.request_id = format!("{owner}-start");
    let started = manager.start_conversation(start).await.unwrap();
    (root, local, provider, connector, manager, started)
}

fn worker(connector: &FakeConnector) -> Arc<FakeWorker> {
    connector.0.lock().unwrap().as_ref().unwrap().clone()
}

fn read_override(value: serde_json::Value) -> ReadOverride {
    ReadOverride::Value(structfs_serde_store::json_to_value(value))
}

fn deterministic_id(prefix: &str, value: &str) -> String {
    let digest = format!("{:x}", sha2::Sha256::digest(value.as_bytes()));
    format!("{prefix}_{}", &digest[..32])
}

fn deterministic_vm(value: &str) -> String {
    let digest = format!("{:x}", sha2::Sha256::digest(value.as_bytes()));
    format!("ox-{}", &digest[..20])
}

fn pending_operation(id: &str, action: RemoteAction) -> RemoteOperationRecord {
    RemoteOperationRecord {
        operation_id: id.into(),
        operation_kind: "test".into(),
        node_id: None,
        node_attempt_id: None,
        conversation_id: None,
        request_hash: "test".into(),
        intent: RemoteOperationIntent {
            semantic_key: format!("semantic-{id}"),
            node_id: None,
            node_attempt_id: None,
            conversation_id: None,
            action,
        },
        state: RemoteOperationState::Pending,
        result: None,
        lease_owner: None,
        lease_until: None,
        lease_epoch: 0,
    }
}

fn node_intent(request_id: &str, observed_state: RemoteNodeObservedState) -> RemoteNodeIntent {
    RemoteNodeIntent {
        node_id: deterministic_id("n", request_id),
        node_attempt_id: deterministic_id("a", request_id),
        provider: "exe.dev".into(),
        vm_name: deterministic_vm(request_id),
        ssh_host: None,
        ssh_port: 22,
        ssh_user: None,
        ssh_dest: None,
        identity_path: "/tmp/test-identity".into(),
        known_hosts_path: "/tmp/test-known-hosts".into(),
        worker_socket_path: "/tmp/test-worker.sock".into(),
        desired_state: RemoteNodeDesiredState::Active,
        observed_state,
        cleanup_state: RemoteCleanupState::None,
        image_digest: Some("worker@sha256:abc".into()),
    }
}

#[tokio::test]
async fn manager_rejects_invalid_configuration_and_request_boundaries() {
    let root = tempfile::tempdir().unwrap();
    let local: Arc<dyn StorePort> =
        Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
    for invalid in [
        RemoteManagerConfig {
            reconciler_id: String::new(),
            ..config("valid")
        },
        RemoteManagerConfig {
            lease_seconds: 0,
            ..config("valid")
        },
        RemoteManagerConfig {
            lease_seconds: 61,
            ..config("valid")
        },
        RemoteManagerConfig {
            ssh_port: 0,
            ..config("valid")
        },
        RemoteManagerConfig {
            ssh_port: 65_536,
            ..config("valid")
        },
    ] {
        assert!(
            RemoteManagerStore::new(
                local.clone(),
                Arc::new(FakeProvider::default()),
                Arc::new(FakeConnector::new()),
                invalid,
            )
            .is_err()
        );
    }

    let manager = RemoteManagerStore::new(
        local,
        Arc::new(FakeProvider::default()),
        Arc::new(FakeConnector::new()),
        config("request-validation"),
    )
    .unwrap();
    for mutate in [
        |value: &mut StartConversationRequest| value.schema_version = 2,
        |value: &mut StartConversationRequest| value.request_id.clear(),
        |value: &mut StartConversationRequest| value.title.clear(),
        |value: &mut StartConversationRequest| value.node.cpu = 0,
        |value: &mut StartConversationRequest| value.node.memory_mib = 512,
        |value: &mut StartConversationRequest| value.node.memory_mib = 1025,
        |value: &mut StartConversationRequest| value.node.disk_gib = 0,
    ] {
        let mut invalid = request();
        mutate(&mut invalid);
        assert!(matches!(
            manager.start_conversation(invalid).await,
            Err(RemoteManagerError::Invalid(_))
        ));
    }
    for mutate in [
        |value: &mut CreateNodeRequest| value.schema_version = 2,
        |value: &mut CreateNodeRequest| value.request_id.clear(),
        |value: &mut CreateNodeRequest| value.node.cpu = 0,
        |value: &mut CreateNodeRequest| value.node.memory_mib = 512,
        |value: &mut CreateNodeRequest| value.node.memory_mib = 1025,
        |value: &mut CreateNodeRequest| value.node.disk_gib = 0,
    ] {
        let mut invalid = node_request("invalid-node");
        mutate(&mut invalid);
        assert!(matches!(
            manager.create_node(invalid).await,
            Err(RemoteManagerError::Invalid(_))
        ));
    }
    assert!(matches!(
        manager.drain_node("missing").await,
        Err(RemoteManagerError::Invalid(_))
    ));
    assert!(matches!(
        manager.doctor_node("missing").await,
        Err(RemoteManagerError::Invalid(_))
    ));
    assert!(matches!(
        manager
            .send_message(
                "missing",
                MessageRequest {
                    request_id: "request".into(),
                    message_id: "message".into(),
                    content: "content".into(),
                },
            )
            .await,
        Err(RemoteManagerError::Invalid(_))
    ));
    assert!(matches!(
        manager.refresh_conversation("missing").await,
        Err(RemoteManagerError::Invalid(_))
    ));
    assert!(matches!(
        manager.reconcile_ledger("missing", "request").await,
        Err(RemoteManagerError::Invalid(_))
    ));
    assert!(matches!(
        manager
            .delete_node(
                "missing",
                DeleteNodeManagerRequest {
                    request_id: "request".into(),
                    delete_id: "delete".into(),
                    force: false,
                },
            )
            .await,
        Err(RemoteManagerError::Invalid(_))
    ));
}

#[tokio::test]
async fn refresh_projects_every_worker_lifecycle_and_rejects_bad_snapshots() {
    for (thread_state, expected) in [
        ("running", "running"),
        ("waiting_for_input", "waiting_for_input"),
        ("blocked_on_approval", "blocked_on_approval"),
        ("completed", "completed"),
        ("errored", "errored"),
        ("interrupted", "errored"),
    ] {
        let owner = format!("refresh-{thread_state}");
        let (_root, _local, _provider, connector, manager, started) = started_fixture(&owner).await;
        worker(&connector)
            .0
            .lock()
            .unwrap()
            .thread_states
            .insert(started.worker_thread_id.clone(), thread_state.into());
        let refreshed = manager
            .refresh_conversation(&started.conversation_id)
            .await
            .unwrap();
        assert_eq!(refreshed.observed_state, expected);
    }

    let (_root, _local, _provider, connector, manager, started) =
        started_fixture("refresh-bad-snapshots").await;
    let worker = worker(&connector);
    let thread_path = format!("conversations/{}", started.worker_thread_id);
    worker.0.lock().unwrap().read_overrides.insert(
        thread_path.clone(),
        read_override(serde_json::json!({
            "id":"wrong-thread",
            "thread_state":"running"
        })),
    );
    assert!(matches!(
        manager.refresh_conversation(&started.conversation_id).await,
        Err(RemoteManagerError::IdentityMismatch(_))
    ));
    worker
        .0
        .lock()
        .unwrap()
        .read_overrides
        .insert(thread_path.clone(), ReadOverride::Missing);
    assert!(matches!(
        manager.refresh_conversation(&started.conversation_id).await,
        Err(RemoteManagerError::Unavailable(_))
    ));
    worker
        .0
        .lock()
        .unwrap()
        .read_overrides
        .insert(thread_path.clone(), ReadOverride::Error);
    assert!(matches!(
        manager.refresh_conversation(&started.conversation_id).await,
        Err(RemoteManagerError::Store { .. })
    ));
    worker
        .0
        .lock()
        .unwrap()
        .read_overrides
        .insert(thread_path, ReadOverride::Value(Value::Null));
    assert!(
        manager
            .refresh_conversation(&started.conversation_id)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn required_placement_obeys_health_capacity_and_exact_node_selection() {
    let (_root, _local, provider, connector, manager, _) =
        started_fixture("required-placement-seed").await;
    let node = manager.list_nodes().await.unwrap().pop().unwrap();
    let creates_before = provider.0.lock().unwrap().actual_creates;
    let mut required = request();
    required.request_id = "required-placement-success".into();
    required.placement = PlacementPolicy::RequireNode {
        node_id: node.node_id.clone(),
    };
    let result = manager.start_conversation(required).await.unwrap();
    assert_eq!(result.node_id, node.node_id);
    assert_eq!(provider.0.lock().unwrap().actual_creates, creates_before);

    let mut missing = request();
    missing.request_id = "required-placement-missing".into();
    missing.placement = PlacementPolicy::RequireNode {
        node_id: "n_missing".into(),
    };
    assert!(matches!(
        manager.start_conversation(missing).await,
        Err(RemoteManagerError::RequiredNodeUnavailable(_))
    ));

    worker(&connector).0.lock().unwrap().read_overrides.insert(
        "capacity".into(),
        read_override(serde_json::json!({
            "active_turns":8,
            "total_threads":256,
            "limits":{"active_turns":8,"total_threads":256}
        })),
    );
    let mut full = request();
    full.request_id = "required-placement-full".into();
    full.placement = PlacementPolicy::RequireNode {
        node_id: node.node_id,
    };
    assert!(matches!(
        manager.start_conversation(full).await,
        Err(RemoteManagerError::RequiredNodeUnavailable(_))
    ));
}

#[tokio::test]
async fn placement_skips_draining_disconnected_unhealthy_and_capacityless_nodes() {
    for mode in [
        "draining",
        "connect",
        "health",
        "capacity-missing",
        "capacity-error",
    ] {
        let (_root, local, _provider, connector, manager, _) =
            started_fixture(&format!("placement-{mode}")).await;
        let node = manager.list_nodes().await.unwrap().pop().unwrap();
        match mode {
            "draining" => {
                let item =
                    ox_inbox::remote_state::remote_item_path("nodes", &node.node_id).unwrap();
                local
                    .write(
                        &Path::parse(&format!("{item}/state")).unwrap(),
                        parsed(&RemoteNodeUpdate {
                            node_attempt_id: node.node_attempt_id.clone(),
                            desired_state: Some(
                                ox_inbox::remote_state::RemoteNodeDesiredState::Draining,
                            ),
                            observed_state: None,
                            cleanup_state: None,
                        }),
                    )
                    .await
                    .unwrap();
            }
            "connect" => connector.1.store(true, Ordering::SeqCst),
            "health" => {
                worker(&connector).0.lock().unwrap().read_overrides.insert(
                    "health".into(),
                    read_override(serde_json::json!({
                        "status":"starting", "node_id":node.node_id,
                        "attempt_id":node.node_attempt_id, "worker_version":"0.1.0",
                        "wire_version":1, "image_digest":"worker@sha256:abc",
                        "agent_wasm_sha256":"agent", "executable_sha256":"executable",
                        "policy_profile":"clash_remote_enforced",
                        "policy_contract_sha256":"policy",
                        "sandbox_enforcement":{"mode":"required","preflight":"passed"}
                    })),
                );
            }
            "capacity-missing" => {
                worker(&connector)
                    .0
                    .lock()
                    .unwrap()
                    .read_overrides
                    .insert("capacity".into(), ReadOverride::Missing);
            }
            "capacity-error" => {
                worker(&connector)
                    .0
                    .lock()
                    .unwrap()
                    .read_overrides
                    .insert("capacity".into(), ReadOverride::Error);
            }
            _ => unreachable!(),
        }
        let mut required = request();
        required.request_id = format!("placement-{mode}-request");
        required.placement = PlacementPolicy::RequireNode {
            node_id: node.node_id,
        };
        let result = manager.start_conversation(required).await;
        if mode == "capacity-error" {
            assert!(matches!(result, Err(RemoteManagerError::Store { .. })));
        } else {
            assert!(matches!(
                result,
                Err(RemoteManagerError::RequiredNodeUnavailable(_))
            ));
        }
    }

    let root = tempfile::tempdir().unwrap();
    let local: Arc<dyn StorePort> =
        Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
    let manager = RemoteManagerStore::new(
        local,
        Arc::new(FakeProvider::default()),
        Arc::new(FakeConnector::new()),
        config("fresh-policy"),
    )
    .unwrap();
    let mut fresh = request();
    fresh.request_id = "fresh-policy-request".into();
    fresh.placement = PlacementPolicy::FreshNode;
    manager.start_conversation(fresh).await.unwrap();
}

#[tokio::test]
async fn provisioning_failure_paths_persist_absence_unavailability_and_identity_errors() {
    for mode in ["absent", "provider-read", "connect", "identity"] {
        let root = tempfile::tempdir().unwrap();
        let local: Arc<dyn StorePort> =
            Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
        let provider = Arc::new(FakeProvider::default());
        let connector = Arc::new(FakeConnector::new());
        match mode {
            "absent" => provider.0.lock().unwrap().suppress_create = true,
            "provider-read" => {
                provider.0.lock().unwrap().write_errors.insert("vms".into());
                let expected_name = format!(
                    "ox-{}",
                    &format!("{:x}", sha2::Sha256::digest(b"provision-provider-read"))[..20]
                );
                provider
                    .0
                    .lock()
                    .unwrap()
                    .read_errors
                    .insert(ox_remote::vm_path(&expected_name).unwrap().to_string());
            }
            "connect" => connector.1.store(true, Ordering::SeqCst),
            "identity" => {
                connector
                    .0
                    .lock()
                    .unwrap()
                    .replace(Arc::new(FakeWorker::new(
                        "wrong-node".into(),
                        "wrong-attempt".into(),
                        "worker@sha256:abc".into(),
                    )));
            }
            _ => unreachable!(),
        }
        let manager = RemoteManagerStore::new(
            local,
            provider,
            connector,
            config(&format!("provision-{mode}")),
        )
        .unwrap();
        let error = manager
            .create_node(node_request(&format!("provision-{mode}")))
            .await
            .unwrap_err();
        match mode {
            "identity" => assert!(matches!(error, RemoteManagerError::IdentityMismatch(_))),
            _ => assert!(matches!(
                error,
                RemoteManagerError::Unavailable(_) | RemoteManagerError::Store { .. }
            )),
        }
    }
}

#[tokio::test]
async fn provisioning_reconciles_provider_identity_and_recovers_an_unavailable_observation() {
    let request_id = "wrong-provider-vm";
    let expected_path = ox_remote::vm_path(&deterministic_vm(request_id))
        .unwrap()
        .to_string();
    let root = tempfile::tempdir().unwrap();
    let local: Arc<dyn StorePort> =
        Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
    let provider = Arc::new(FakeProvider::default());
    provider.0.lock().unwrap().read_sequences.insert(
        expected_path,
        VecDeque::from([read_override(serde_json::json!({
            "schema_version":1, "vm_name":"ox-wrong", "status":"running",
            "ssh_dest":"route@wrong", "ssh_host":"203.0.113.50", "ssh_user":"route"
        }))]),
    );
    let manager = RemoteManagerStore::new(
        local,
        provider,
        Arc::new(FakeConnector::new()),
        config("wrong-provider-vm"),
    )
    .unwrap();
    assert!(matches!(
        manager.create_node(node_request(request_id)).await,
        Err(RemoteManagerError::IdentityMismatch(_))
    ));

    let request_id = "recover-unavailable-node";
    let root = tempfile::tempdir().unwrap();
    let local: Arc<dyn StorePort> =
        Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
    let provider = Arc::new(FakeProvider::default());
    provider.0.lock().unwrap().read_sequences.insert(
        ox_remote::vm_path(&deterministic_vm(request_id))
            .unwrap()
            .to_string(),
        VecDeque::from([ReadOverride::Error]),
    );
    let connector = Arc::new(FakeConnector::new());
    let manager = RemoteManagerStore::new(
        local.clone(),
        provider.clone(),
        connector.clone(),
        config("unavailable-first"),
    )
    .unwrap();
    let request = node_request(request_id);
    assert!(manager.create_node(request.clone()).await.is_err());
    assert_eq!(
        manager.list_nodes().await.unwrap()[0].observed_state,
        "unavailable"
    );
    tokio::time::sleep(std::time::Duration::from_millis(5_100)).await;
    let retry =
        RemoteManagerStore::new(local, provider, connector, config("unavailable-retry")).unwrap();
    retry.create_node(request).await.unwrap();
    assert_eq!(retry.list_nodes().await.unwrap()[0].observed_state, "ready");

    let request_id = "missing-worker-health";
    let root = tempfile::tempdir().unwrap();
    let local: Arc<dyn StorePort> =
        Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
    let connector = Arc::new(FakeConnector::new());
    let worker = Arc::new(FakeWorker::new(
        deterministic_id("n", request_id),
        deterministic_id("a", request_id),
        "worker@sha256:abc".into(),
    ));
    worker
        .0
        .lock()
        .unwrap()
        .read_overrides
        .insert("health".into(), ReadOverride::Missing);
    connector.0.lock().unwrap().replace(worker);
    let manager = RemoteManagerStore::new(
        local,
        Arc::new(FakeProvider::default()),
        connector,
        config("missing-worker-health"),
    )
    .unwrap();
    assert!(matches!(
        manager.create_node(node_request(request_id)).await,
        Err(RemoteManagerError::Unavailable(_))
    ));
}

#[tokio::test]
async fn crash_recovery_replays_every_persistable_node_observation_state() {
    for (label, observed) in [
        ("pending", RemoteNodeObservedState::Pending),
        ("provisioning", RemoteNodeObservedState::Provisioning),
        ("unavailable", RemoteNodeObservedState::Unavailable),
        ("absent", RemoteNodeObservedState::Absent),
        ("errored", RemoteNodeObservedState::Errored),
    ] {
        let request_id = format!("recover-node-state-{label}");
        let root = tempfile::tempdir().unwrap();
        let local: Arc<dyn StorePort> =
            Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
        local
            .write(
                &path!("remote/nodes"),
                parsed(&node_intent(&request_id, observed)),
            )
            .await
            .unwrap();
        let manager = RemoteManagerStore::new(
            local,
            Arc::new(FakeProvider::default()),
            Arc::new(FakeConnector::new()),
            config(&request_id),
        )
        .unwrap();
        let mut start = request();
        start.request_id = request_id;
        let result = manager.start_conversation(start).await;
        match result {
            Ok(started) => assert!(!started.worker_thread_id.is_empty()),
            Err(RemoteManagerError::Store { message, .. }) => {
                assert!(
                    message.contains("illegal state transition"),
                    "{label}: {message}"
                );
            }
            Err(error) => panic!("{label}: unexpected replay result {error:?}"),
        }
    }
}

#[tokio::test]
async fn conversation_creation_releases_its_lease_when_write_or_receipt_verification_fails() {
    for mode in ["write", "missing-receipt"] {
        let root = tempfile::tempdir().unwrap();
        let local: Arc<dyn StorePort> =
            Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
        let provider = Arc::new(FakeProvider::default());
        let connector = Arc::new(FakeConnector::new());
        let manager = RemoteManagerStore::new(
            local,
            provider,
            connector.clone(),
            config(&format!("create-failure-{mode}")),
        )
        .unwrap();
        let node = manager
            .create_node(node_request(&format!("create-failure-node-{mode}")))
            .await
            .unwrap();
        match mode {
            "write" => {
                worker(&connector)
                    .0
                    .lock()
                    .unwrap()
                    .write_errors
                    .insert("conversations".into());
            }
            "missing-receipt" => {
                worker(&connector)
                    .0
                    .lock()
                    .unwrap()
                    .read_overrides
                    .insert("conversations/t_1".into(), ReadOverride::Missing);
            }
            _ => unreachable!(),
        }
        let mut start = request();
        start.request_id = format!("create-failure-conversation-{mode}");
        start.placement = PlacementPolicy::RequireNode {
            node_id: node.node_id,
        };
        assert!(manager.start_conversation(start).await.is_err());
    }
}

#[tokio::test]
async fn worker_connection_and_health_failures_mark_active_actions_unavailable() {
    for mode in ["connect", "health"] {
        let (_root, _local, _provider, connector, manager, started) =
            started_fixture(&format!("action-{mode}")).await;
        match mode {
            "connect" => connector.1.store(true, Ordering::SeqCst),
            "health" => {
                worker(&connector)
                    .0
                    .lock()
                    .unwrap()
                    .read_overrides
                    .insert("health".into(), ReadOverride::Missing);
            }
            _ => unreachable!(),
        }
        assert!(
            manager
                .send_message(
                    &started.conversation_id,
                    MessageRequest {
                        request_id: format!("action-{mode}-request"),
                        message_id: format!("action_{mode}"),
                        content: "continue".into(),
                    },
                )
                .await
                .is_err()
        );
        assert_eq!(
            manager
                .get_conversation(&started.conversation_id)
                .await
                .unwrap()
                .unwrap()
                .observed_state,
            "unavailable"
        );
    }
}

#[tokio::test]
async fn conversation_actions_are_idempotent_and_release_failed_worker_mutations() {
    let (_root, _local, _provider, connector, manager, started) =
        started_fixture("action-failures").await;
    let message = MessageRequest {
        request_id: "action-message".into(),
        message_id: "message_1".into(),
        content: "continue".into(),
    };
    let first = manager
        .send_message(&started.conversation_id, message.clone())
        .await
        .unwrap();
    let retry = manager
        .send_message(&started.conversation_id, message)
        .await
        .unwrap();
    assert_eq!(first, retry);

    let message_path = format!("conversations/{}/messages", started.worker_thread_id);
    worker(&connector)
        .0
        .lock()
        .unwrap()
        .write_errors
        .insert(message_path);
    assert!(matches!(
        manager
            .send_message(
                &started.conversation_id,
                MessageRequest {
                    request_id: "action-message-failure".into(),
                    message_id: "message_2".into(),
                    content: "fail".into(),
                },
            )
            .await,
        Err(RemoteManagerError::Store { .. })
    ));

    manager
        .cancel(
            &started.conversation_id,
            CancelRequest {
                request_id: "action-cancel".into(),
                cancel_id: "cancel-action".into(),
                reason: None,
            },
        )
        .await
        .unwrap();
    assert!(matches!(
        manager
            .respond_approval(
                &started.conversation_id,
                ApprovalRequest {
                    request_id: "late-approval".into(),
                    approval_id: "approval".into(),
                    decision: ox_types::Decision::AllowOnce,
                },
            )
            .await,
        Err(RemoteManagerError::Invalid(_))
    ));
}

#[tokio::test]
async fn ledger_reconciliation_handles_multiple_batches_and_rejects_stalled_or_bad_workers() {
    let (_root, _local, _provider, connector, manager, started) =
        started_fixture("ledger-batches").await;
    let worker_store = worker(&connector);
    {
        let mut state = worker_store.0.lock().unwrap();
        let parent = state.ledger[0].hash.clone();
        let message = serde_json::json!({"type":"assistant","content":"second"});
        state.ledger.push(ox_inbox::ledger::LedgerEntry {
            seq: 1,
            hash: ox_inbox::ledger::entry_hash(&message),
            parent: Some(parent),
            msg: message,
        });
        state.ledger_batch_size = 1;
    }
    manager
        .reconcile_ledger(&started.conversation_id, "multi-batch")
        .await
        .unwrap();

    let (_root, _local, _provider, connector, manager, started) =
        started_fixture("ledger-stalled").await;
    worker(&connector).0.lock().unwrap().read_overrides.insert(
        format!("conversations/{}/ledger/from/0", started.worker_thread_id),
        read_override(serde_json::json!({"entries":[],"has_more":true})),
    );
    assert!(matches!(
        manager
            .reconcile_ledger(&started.conversation_id, "stalled")
            .await,
        Err(RemoteManagerError::Invalid(_))
    ));

    for (owner, behavior) in [
        ("ledger-missing", ReadOverride::Missing),
        ("ledger-read-error", ReadOverride::Error),
        ("ledger-malformed", ReadOverride::Value(Value::Null)),
    ] {
        let (_root, _local, _provider, connector, manager, started) = started_fixture(owner).await;
        worker(&connector).0.lock().unwrap().read_overrides.insert(
            format!("conversations/{}/ledger/from/0", started.worker_thread_id),
            behavior,
        );
        assert!(
            manager
                .reconcile_ledger(&started.conversation_id, owner)
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn pending_replay_applies_each_worker_action_through_its_public_contract() {
    let (_root, local, provider, connector, _manager, started) =
        started_fixture("action-replay").await;
    let crash_manager = |owner: &str| {
        RemoteManagerStore::with_crash_injector(
            local.clone(),
            provider.clone(),
            connector.clone(),
            config(owner),
            Arc::new(CrashOnce {
                point: CrashPoint::OperationIntentPersisted,
                occurrence: 1,
                seen: Mutex::new(0),
            }),
        )
        .unwrap()
    };
    assert!(
        crash_manager("message-crash")
            .send_message(
                &started.conversation_id,
                MessageRequest {
                    request_id: "replay-message".into(),
                    message_id: "replay_message".into(),
                    content: "continue".into(),
                },
            )
            .await
            .is_err()
    );
    assert!(
        crash_manager("approval-crash")
            .respond_approval(
                &started.conversation_id,
                ApprovalRequest {
                    request_id: "replay-approval".into(),
                    approval_id: "replay_approval".into(),
                    decision: ox_types::Decision::DenyOnce,
                },
            )
            .await
            .is_err()
    );
    assert!(
        crash_manager("cancel-crash")
            .cancel(
                &started.conversation_id,
                CancelRequest {
                    request_id: "replay-cancel".into(),
                    cancel_id: "replay_cancel".into(),
                    reason: Some("stop".into()),
                },
            )
            .await
            .is_err()
    );

    let manager =
        RemoteManagerStore::new(local, provider, connector, config("action-reconciler")).unwrap();
    let results = manager.reconcile_pending().await.unwrap();
    assert_eq!(results.len(), 3);
    assert!(results.iter().all(|result| result.applied));
}

#[tokio::test]
async fn pending_replay_preserves_fresh_required_and_ledger_action_semantics() {
    for placement in [PlacementPolicy::FreshNode, PlacementPolicy::PreferExisting] {
        let root = tempfile::tempdir().unwrap();
        let local: Arc<dyn StorePort> =
            Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
        let provider = Arc::new(FakeProvider::default());
        let connector = Arc::new(FakeConnector::new());
        let owner = match placement {
            PlacementPolicy::FreshNode => "replay-fresh",
            _ => "replay-prefer",
        };
        let manager = RemoteManagerStore::with_crash_injector(
            local.clone(),
            provider.clone(),
            connector.clone(),
            config(owner),
            Arc::new(CrashOnce {
                point: CrashPoint::OperationIntentPersisted,
                occurrence: 1,
                seen: Mutex::new(0),
            }),
        )
        .unwrap();
        let mut start = request();
        start.request_id = format!("{owner}-request");
        start.placement = placement;
        assert!(manager.start_conversation(start).await.is_err());
        let reconciler = RemoteManagerStore::new(
            local,
            provider,
            connector,
            config(&format!("{owner}-reconciler")),
        )
        .unwrap();
        let results = reconciler.reconcile_pending().await.unwrap();
        assert!(results.iter().all(|result| result.applied));
    }

    let (_root, local, provider, connector, manager, _) =
        started_fixture("replay-required-seed").await;
    let node = manager.list_nodes().await.unwrap().pop().unwrap();
    let crashing = RemoteManagerStore::with_crash_injector(
        local.clone(),
        provider.clone(),
        connector.clone(),
        config("replay-required"),
        Arc::new(CrashOnce {
            point: CrashPoint::OperationIntentPersisted,
            occurrence: 1,
            seen: Mutex::new(0),
        }),
    )
    .unwrap();
    let mut required = request();
    required.request_id = "replay-required-request".into();
    required.placement = PlacementPolicy::RequireNode {
        node_id: node.node_id,
    };
    assert!(crashing.start_conversation(required).await.is_err());
    let reconciler = RemoteManagerStore::new(
        local,
        provider,
        connector,
        config("replay-required-reconciler"),
    )
    .unwrap();
    assert!(
        reconciler
            .reconcile_pending()
            .await
            .unwrap()
            .iter()
            .all(|result| result.applied)
    );

    let (_root, _local, _provider, _connector, manager, started) =
        started_fixture("replay-ledger").await;
    manager
        .reconcile_ledger(&started.conversation_id, "ledger-first")
        .await
        .unwrap();
    manager
        .reconcile_ledger(&started.conversation_id, "ledger-idle")
        .await
        .unwrap();
    let results = manager.reconcile_pending().await.unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].applied);
}

#[tokio::test]
async fn local_structfs_faults_are_reported_without_inventing_remote_state() {
    let root = tempfile::tempdir().unwrap();
    let inner: Arc<dyn StorePort> =
        Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
    let local = Arc::new(FaultStore::new(inner));
    let manager = RemoteManagerStore::new(
        local.clone(),
        Arc::new(FakeProvider::default()),
        Arc::new(FakeConnector::new()),
        config("local-faults"),
    )
    .unwrap();

    for behavior in [
        ReadOverride::Missing,
        ReadOverride::Raw(vec![1]),
        ReadOverride::Value(Value::Null),
        ReadOverride::Error,
    ] {
        local
            .reads
            .lock()
            .unwrap()
            .insert("remote/nodes".into(), behavior);
        assert!(manager.list_nodes().await.is_err());
    }
    local.reads.lock().unwrap().clear();
    for behavior in [
        ReadOverride::Missing,
        ReadOverride::Raw(vec![1]),
        ReadOverride::Value(Value::Null),
        ReadOverride::Value(Value::Array(vec![Value::Null])),
        ReadOverride::Error,
    ] {
        local
            .reads
            .lock()
            .unwrap()
            .insert("remote/operations/pending".into(), behavior);
        assert!(manager.reconcile_pending().await.is_err());
    }

    local.reads.lock().unwrap().clear();
    for behavior in [
        ReadOverride::Missing,
        ReadOverride::Raw(vec![1]),
        ReadOverride::Value(Value::Null),
        ReadOverride::Value(Value::Array(vec![Value::Null])),
        ReadOverride::Error,
    ] {
        local
            .reads
            .lock()
            .unwrap()
            .insert("remote/conversations".into(), behavior);
        assert!(manager.list_conversations().await.is_err());
    }
}

#[tokio::test]
async fn retries_fail_closed_when_durable_node_or_create_receipts_are_corrupt() {
    let root = tempfile::tempdir().unwrap();
    let inner: Arc<dyn StorePort> =
        Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
    let local = Arc::new(FaultStore::new(inner));
    let manager = RemoteManagerStore::new(
        local.clone(),
        Arc::new(FakeProvider::default()),
        Arc::new(FakeConnector::new()),
        config("corrupt-node-attempt"),
    )
    .unwrap();
    let request_id = "corrupt-node-attempt";
    let intent = node_intent(request_id, RemoteNodeObservedState::Pending);
    local
        .inner
        .write(&path!("remote/nodes"), parsed(&intent))
        .await
        .unwrap();
    let mut bad_node: ox_inbox::remote_state::RemoteNodeRecord = local
        .inner
        .read(&ox_inbox::remote_state::remote_item_path("nodes", &intent.node_id).unwrap())
        .await
        .unwrap()
        .map(decode)
        .unwrap();
    bad_node.node_attempt_id = "different-attempt".into();
    local.reads.lock().unwrap().insert(
        ox_inbox::remote_state::remote_item_path("nodes", &intent.node_id)
            .unwrap()
            .to_string(),
        ReadOverride::Value(structfs_serde_store::to_value(&bad_node).unwrap()),
    );
    assert!(matches!(
        manager.create_node(node_request(request_id)).await,
        Err(RemoteManagerError::IdentityMismatch(_))
    ));

    let root = tempfile::tempdir().unwrap();
    let inner: Arc<dyn StorePort> =
        Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
    let local = Arc::new(FaultStore::new(inner));
    let provider = Arc::new(FakeProvider::default());
    let connector = Arc::new(FakeConnector::new());
    let manager = RemoteManagerStore::new(
        local.clone(),
        provider,
        connector,
        config("corrupt-create-receipt"),
    )
    .unwrap();
    let mut start = request();
    start.request_id = "corrupt-create-receipt".into();
    let started = manager.start_conversation(start.clone()).await.unwrap();
    let conversation = manager
        .get_conversation(&started.conversation_id)
        .await
        .unwrap()
        .unwrap();
    let create_intent = RemoteOperationIntent {
        semantic_key: format!("{}:create", start.request_id),
        node_id: Some(conversation.node_id.clone()),
        node_attempt_id: Some(conversation.node_attempt_id.clone()),
        conversation_id: Some(conversation.conversation_id.clone()),
        action: RemoteAction::CreateConversation {
            create_id: conversation.create_id.clone(),
            title: conversation.title.clone(),
            prompt: conversation.initial_prompt.clone(),
            parent_thread_id: conversation.parent_thread_id.clone(),
        },
    };
    let operation_path =
        ox_inbox::remote_state::remote_operation_item_path(&create_intent).unwrap();
    let mut operation: RemoteOperationRecord = local
        .inner
        .read(&operation_path)
        .await
        .unwrap()
        .map(decode)
        .unwrap();
    operation.result = Some(RemoteOperationResult {
        result_path: Some("conversations/wrong_thread".into()),
        error_code: None,
        error_message: None,
    });
    local.reads.lock().unwrap().insert(
        operation_path.to_string(),
        ReadOverride::Value(structfs_serde_store::to_value(&operation).unwrap()),
    );
    assert!(matches!(
        manager.start_conversation(start).await,
        Err(RemoteManagerError::IdentityMismatch(_))
    ));
}

#[tokio::test]
async fn missing_persisted_nodes_and_unbound_threads_fail_before_worker_mutation() {
    let root = tempfile::tempdir().unwrap();
    let inner: Arc<dyn StorePort> =
        Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
    let local = Arc::new(FaultStore::new(inner));
    let manager = RemoteManagerStore::new(
        local.clone(),
        Arc::new(FakeProvider::default()),
        Arc::new(FakeConnector::new()),
        config("missing-persisted-node"),
    )
    .unwrap();
    let mut start = request();
    start.request_id = "missing-persisted-node".into();
    let started = manager.start_conversation(start.clone()).await.unwrap();
    let node_path = ox_inbox::remote_state::remote_item_path("nodes", &started.node_id)
        .unwrap()
        .to_string();
    local
        .reads
        .lock()
        .unwrap()
        .insert(node_path, ReadOverride::Missing);
    assert!(matches!(
        manager.start_conversation(start).await,
        Err(RemoteManagerError::Unavailable(_))
    ));
    assert!(matches!(
        manager
            .send_message(
                &started.conversation_id,
                MessageRequest {
                    request_id: "missing-node-message".into(),
                    message_id: "missing_node_message".into(),
                    content: "continue".into(),
                },
            )
            .await,
        Err(RemoteManagerError::Unavailable(_))
    ));

    let root = tempfile::tempdir().unwrap();
    let local: Arc<dyn StorePort> =
        Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
    let provider = Arc::new(FakeProvider::default());
    let connector = Arc::new(FakeConnector::new());
    let manager = RemoteManagerStore::with_crash_injector(
        local.clone(),
        provider.clone(),
        connector.clone(),
        config("unbound-action-start"),
        Arc::new(CrashOnce {
            point: CrashPoint::OperationIntentPersisted,
            occurrence: 1,
            seen: Mutex::new(0),
        }),
    )
    .unwrap();
    let mut start = request();
    start.request_id = "unbound-action-start".into();
    assert!(manager.start_conversation(start).await.is_err());
    let conversation = manager.list_conversations().await.unwrap().pop().unwrap();
    let manager =
        RemoteManagerStore::new(local, provider, connector, config("unbound-action")).unwrap();
    assert!(matches!(
        manager
            .send_message(
                &conversation.conversation_id,
                MessageRequest {
                    request_id: "unbound-message".into(),
                    message_id: "unbound_message".into(),
                    content: "continue".into(),
                },
            )
            .await,
        Err(RemoteManagerError::Invalid(_))
    ));
}

#[tokio::test]
async fn standalone_ready_node_is_reused_by_same_request_and_post_provision_connect_is_checked() {
    let root = tempfile::tempdir().unwrap();
    let local: Arc<dyn StorePort> =
        Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
    let provider = Arc::new(FakeProvider::default());
    let connector = Arc::new(FakeConnector::new());
    let manager = RemoteManagerStore::new(
        local,
        provider.clone(),
        connector,
        config("same-request-node"),
    )
    .unwrap();
    manager
        .create_node(node_request("same-request-node"))
        .await
        .unwrap();
    manager
        .create_node(node_request("same-request-node"))
        .await
        .unwrap();
    let mut start = request();
    start.request_id = "same-request-node".into();
    manager.start_conversation(start.clone()).await.unwrap();
    manager.start_conversation(start).await.unwrap();
    assert_eq!(provider.0.lock().unwrap().actual_creates, 1);

    let root = tempfile::tempdir().unwrap();
    let local: Arc<dyn StorePort> =
        Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
    let connector = Arc::new(FakeConnector::new());
    connector.3.store(2, Ordering::SeqCst);
    let manager = RemoteManagerStore::new(
        local,
        Arc::new(FakeProvider::default()),
        connector,
        config("post-provision-connect"),
    )
    .unwrap();
    let mut start = request();
    start.request_id = "post-provision-connect".into();
    assert!(matches!(
        manager.start_conversation(start).await,
        Err(RemoteManagerError::Store { .. })
    ));
}

#[tokio::test]
async fn a_live_fenced_lease_blocks_a_second_reconciler_until_expiry() {
    let (_root, local, provider, connector, _manager, started) =
        started_fixture("lease-held").await;
    let message = MessageRequest {
        request_id: "leased-message".into(),
        message_id: "leased_message".into(),
        content: "continue".into(),
    };
    let first = RemoteManagerStore::with_crash_injector(
        local.clone(),
        provider.clone(),
        connector.clone(),
        config("lease-owner"),
        Arc::new(CrashOnce {
            point: CrashPoint::ExternalEffectReturned,
            occurrence: 1,
            seen: Mutex::new(0),
        }),
    )
    .unwrap();
    assert!(
        first
            .send_message(&started.conversation_id, message.clone())
            .await
            .is_err()
    );
    let second =
        RemoteManagerStore::new(local, provider, connector, config("other-owner")).unwrap();
    assert!(matches!(
        second.send_message(&started.conversation_id, message).await,
        Err(RemoteManagerError::LeaseHeld)
    ));
}

#[tokio::test]
async fn local_and_worker_reference_listings_fail_closed_on_missing_or_malformed_records() {
    for (owner, behavior) in [
        ("refs-missing", ReadOverride::Missing),
        ("refs-raw", ReadOverride::Raw(vec![1])),
        ("refs-null", ReadOverride::Value(Value::Null)),
        (
            "refs-invalid-item",
            ReadOverride::Value(Value::Array(vec![Value::Null])),
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let inner: Arc<dyn StorePort> =
            Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
        let local = Arc::new(FaultStore::new(inner));
        let manager = RemoteManagerStore::new(
            local.clone(),
            Arc::new(FakeProvider::default()),
            Arc::new(FakeConnector::new()),
            config(owner),
        )
        .unwrap();
        let mut start = request();
        start.request_id = format!("{owner}-start");
        let started = manager.start_conversation(start).await.unwrap();
        local
            .reads
            .lock()
            .unwrap()
            .insert("remote/conversations".into(), behavior);
        assert!(
            manager
                .delete_node(
                    &started.node_id,
                    DeleteNodeManagerRequest {
                        request_id: format!("{owner}-delete-request"),
                        delete_id: format!("{owner}-delete"),
                        force: true,
                    },
                )
                .await
                .is_err()
        );
    }

    for (owner, listing) in [
        ("worker-refs-null", Value::Null),
        ("worker-refs-non-map", Value::Array(vec![Value::Null])),
    ] {
        let (_root, _local, _provider, connector, manager, started) = started_fixture(owner).await;
        worker(&connector)
            .0
            .lock()
            .unwrap()
            .read_overrides
            .insert("conversations".into(), ReadOverride::Value(listing));
        manager
            .delete_node(
                &started.node_id,
                DeleteNodeManagerRequest {
                    request_id: format!("{owner}-delete-request"),
                    delete_id: format!("{owner}-delete"),
                    force: true,
                },
            )
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn non_lease_store_errors_are_not_misreported_as_lease_contention() {
    let root = tempfile::tempdir().unwrap();
    let inner: Arc<dyn StorePort> =
        Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
    let local = Arc::new(FaultStore::new(inner));
    let manager = RemoteManagerStore::new(
        local.clone(),
        Arc::new(FakeProvider::default()),
        Arc::new(FakeConnector::new()),
        config("lease-write-error"),
    )
    .unwrap();
    let mut start = request();
    start.request_id = "lease-write-error-start".into();
    let started = manager.start_conversation(start).await.unwrap();
    let message = MessageRequest {
        request_id: "lease-write-error-message".into(),
        message_id: "lease_write_error_message".into(),
        content: "continue".into(),
    };
    let operation = RemoteOperationIntent {
        semantic_key: message.request_id.clone(),
        node_id: Some(started.node_id.clone()),
        node_attempt_id: Some(started.node_attempt_id.clone()),
        conversation_id: Some(started.conversation_id.clone()),
        action: RemoteAction::SendMessage {
            message_id: message.message_id.clone(),
            content: message.content.clone(),
        },
    };
    let operation_path = ox_inbox::remote_state::remote_operation_item_path(&operation).unwrap();
    local
        .write_errors
        .lock()
        .unwrap()
        .insert(format!("{operation_path}/lease"));
    assert!(matches!(
        manager
            .send_message(&started.conversation_id, message)
            .await,
        Err(RemoteManagerError::Store { .. })
    ));

    let mapped = RemoteManagerError::from(StoreError::store("test", "test", "failure"));
    assert!(matches!(mapped, RemoteManagerError::Store { .. }));
    let path_error = Path::parse("bad-component").unwrap_err();
    let mapped = RemoteManagerError::from(path_error);
    assert!(matches!(mapped, RemoteManagerError::Store { .. }));
}

#[tokio::test]
async fn cached_ledger_cursor_validation_rejects_missing_unparsed_and_invalid_shapes() {
    for (owner, behavior) in [
        ("cursor-missing", ReadOverride::Missing),
        ("cursor-raw", ReadOverride::Raw(vec![1])),
        ("cursor-null", ReadOverride::Value(Value::Null)),
        (
            "cursor-last-seq",
            ReadOverride::Value(Value::Map(BTreeMap::new())),
        ),
        (
            "cursor-last-hash",
            ReadOverride::Value(Value::Map(BTreeMap::from([
                ("last_seq".into(), Value::Integer(-1)),
                ("last_hash".into(), Value::Integer(3)),
            ]))),
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let inner: Arc<dyn StorePort> =
            Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
        let local = Arc::new(FaultStore::new(inner));
        let manager = RemoteManagerStore::new(
            local.clone(),
            Arc::new(FakeProvider::default()),
            Arc::new(FakeConnector::new()),
            config(owner),
        )
        .unwrap();
        let mut start = request();
        start.request_id = format!("{owner}-start");
        let started = manager.start_conversation(start).await.unwrap();
        let item =
            ox_inbox::remote_state::remote_item_path("conversations", &started.conversation_id)
                .unwrap();
        local
            .reads
            .lock()
            .unwrap()
            .insert(format!("{item}/ledger/cursor"), behavior);
        assert!(
            manager
                .reconcile_ledger(&started.conversation_id, owner)
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn ledger_sync_marks_connection_and_health_failures_and_rejects_unbound_threads() {
    for mode in ["connect", "health"] {
        let (_root, _local, _provider, connector, manager, started) =
            started_fixture(&format!("ledger-{mode}")).await;
        if mode == "connect" {
            connector.1.store(true, Ordering::SeqCst);
        } else {
            worker(&connector)
                .0
                .lock()
                .unwrap()
                .read_overrides
                .insert("health".into(), ReadOverride::Missing);
        }
        assert!(
            manager
                .reconcile_ledger(&started.conversation_id, mode)
                .await
                .is_err()
        );
        assert_eq!(
            manager
                .get_conversation(&started.conversation_id)
                .await
                .unwrap()
                .unwrap()
                .observed_state,
            "unavailable"
        );
    }

    let root = tempfile::tempdir().unwrap();
    let local: Arc<dyn StorePort> =
        Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
    let provider = Arc::new(FakeProvider::default());
    let connector = Arc::new(FakeConnector::new());
    let manager = RemoteManagerStore::with_crash_injector(
        local.clone(),
        provider.clone(),
        connector.clone(),
        config("unbound-start"),
        Arc::new(CrashOnce {
            point: CrashPoint::OperationIntentPersisted,
            occurrence: 1,
            seen: Mutex::new(0),
        }),
    )
    .unwrap();
    let mut start = request();
    start.request_id = "unbound-start-request".into();
    assert!(manager.start_conversation(start).await.is_err());
    let conversation = manager.list_conversations().await.unwrap().pop().unwrap();
    let manager =
        RemoteManagerStore::new(local, provider, connector, config("unbound-ledger")).unwrap();
    assert!(matches!(
        manager
            .reconcile_ledger(&conversation.conversation_id, "unbound")
            .await,
        Err(RemoteManagerError::Invalid(_))
    ));
    assert!(matches!(
        manager
            .refresh_conversation(&conversation.conversation_id)
            .await,
        Err(RemoteManagerError::Unavailable(_))
    ));
}

#[tokio::test]
async fn deletion_releases_leases_on_provider_worker_and_delete_failures() {
    for mode in [
        "preflight",
        "connect",
        "worker-list",
        "delete-write",
        "still-present",
    ] {
        let (_root, _local, provider, connector, manager, started) =
            started_fixture(&format!("delete-{mode}")).await;
        let vm_path = ox_remote::vm_path(
            &manager
                .get_node(&started.node_id)
                .await
                .unwrap()
                .unwrap()
                .vm_name,
        )
        .unwrap();
        match mode {
            "preflight" => {
                provider
                    .0
                    .lock()
                    .unwrap()
                    .read_errors
                    .insert(vm_path.to_string());
            }
            "connect" => connector.1.store(true, Ordering::SeqCst),
            "worker-list" => {
                worker(&connector)
                    .0
                    .lock()
                    .unwrap()
                    .read_overrides
                    .insert("conversations".into(), ReadOverride::Missing);
            }
            "delete-write" => {
                let node = manager.get_node(&started.node_id).await.unwrap().unwrap();
                let path = ox_remote::vm_delete_path(&node.vm_name)
                    .unwrap()
                    .to_string();
                provider.0.lock().unwrap().write_errors.insert(path);
            }
            "still-present" => provider.0.lock().unwrap().suppress_delete = true,
            _ => unreachable!(),
        }
        assert!(
            manager
                .delete_node(
                    &started.node_id,
                    DeleteNodeManagerRequest {
                        request_id: format!("delete-{mode}-request"),
                        delete_id: format!("delete-{mode}"),
                        force: true,
                    },
                )
                .await
                .is_err()
        );
    }
}

fn provider_vm_value(vm: &VmStatus) -> ReadOverride {
    ReadOverride::Value(structfs_serde_store::to_value(vm).unwrap())
}

fn worker_health_value(node: &ox_inbox::remote_state::RemoteNodeRecord) -> ReadOverride {
    read_override(serde_json::json!({
        "status":"ready", "node_id":node.node_id, "attempt_id":node.node_attempt_id,
        "worker_version":"0.1.0", "wire_version":1,
        "image_digest":node.image_digest.clone().unwrap(),
        "agent_wasm_sha256":"agent", "executable_sha256":"executable",
        "policy_profile":"clash_remote_enforced", "policy_contract_sha256":"policy",
        "sandbox_enforcement":{"mode":"required","preflight":"passed"}
    }))
}

#[tokio::test]
async fn deletion_distinguishes_failures_before_and_after_its_fenced_claim() {
    for mode in [
        "second-provider-read",
        "second-connect",
        "second-health",
        "second-list",
    ] {
        let (_root, _local, provider, connector, manager, started) =
            started_fixture(&format!("delete-fenced-{mode}")).await;
        let node = manager.get_node(&started.node_id).await.unwrap().unwrap();
        let vm_path = ox_remote::vm_path(&node.vm_name).unwrap().to_string();
        let vm = provider.0.lock().unwrap().vm.clone().unwrap();
        match mode {
            "second-provider-read" => {
                provider.0.lock().unwrap().read_sequences.insert(
                    vm_path,
                    VecDeque::from([provider_vm_value(&vm), ReadOverride::Error]),
                );
            }
            "second-connect" => {
                let next = connector.2.load(Ordering::SeqCst) + 2;
                connector.3.store(next, Ordering::SeqCst);
            }
            "second-health" => {
                worker(&connector).0.lock().unwrap().read_sequences.insert(
                    "health".into(),
                    VecDeque::from([worker_health_value(&node), ReadOverride::Error]),
                );
            }
            "second-list" => {
                worker(&connector).0.lock().unwrap().read_sequences.insert(
                    "conversations".into(),
                    VecDeque::from([
                        read_override(serde_json::json!([{
                            "id":started.worker_thread_id,
                            "thread_state":"running"
                        }])),
                        ReadOverride::Error,
                    ]),
                );
            }
            _ => unreachable!(),
        }
        assert!(
            manager
                .delete_node(
                    &started.node_id,
                    DeleteNodeManagerRequest {
                        request_id: format!("delete-fenced-{mode}-request"),
                        delete_id: format!("delete-fenced-{mode}"),
                        force: true,
                    },
                )
                .await
                .is_err()
        );
    }

    let (_root, _local, provider, _connector, manager, started) =
        started_fixture("delete-provider-absent").await;
    provider.0.lock().unwrap().vm = None;
    let request = DeleteNodeManagerRequest {
        request_id: "delete-provider-absent-request".into(),
        delete_id: "delete-provider-absent".into(),
        force: true,
    };
    let first = manager
        .delete_node(&started.node_id, request.clone())
        .await
        .unwrap();
    assert!(first.affected_references.contains(&started.conversation_id));
    let retry = manager
        .delete_node(&started.node_id, request)
        .await
        .unwrap();
    assert_eq!(retry.node_id, started.node_id);

    let (_root, _local, provider, connector, manager, started) =
        started_fixture("delete-current-reference").await;
    manager
        .cancel(
            &started.conversation_id,
            CancelRequest {
                request_id: "delete-current-cancel".into(),
                cancel_id: "delete_current_cancel".into(),
                reason: None,
            },
        )
        .await
        .unwrap();
    manager
        .refresh_conversation(&started.conversation_id)
        .await
        .unwrap();
    worker(&connector).0.lock().unwrap().read_sequences.insert(
        "conversations".into(),
        VecDeque::from([
            read_override(serde_json::json!([{
                "id":started.worker_thread_id,
                "thread_state":"interrupted"
            }])),
            read_override(serde_json::json!([{
                "id":started.worker_thread_id,
                "thread_state":"running"
            }])),
        ]),
    );
    assert!(matches!(
        manager
            .delete_node(
                &started.node_id,
                DeleteNodeManagerRequest {
                    request_id: "delete-current-reference-request".into(),
                    delete_id: "delete-current-reference".into(),
                    force: false,
                },
            )
            .await,
        Err(RemoteManagerError::ActiveReferences(_))
    ));
    assert!(provider.0.lock().unwrap().vm.is_some());
}

#[tokio::test]
async fn pending_replay_reports_each_incomplete_durable_action_without_blocking_the_batch() {
    let root = tempfile::tempdir().unwrap();
    let inner: Arc<dyn StorePort> =
        Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
    let local = Arc::new(FaultStore::new(inner));
    let manager = RemoteManagerStore::new(
        local.clone(),
        Arc::new(FakeProvider::default()),
        Arc::new(FakeConnector::new()),
        config("malformed-pending"),
    )
    .unwrap();
    let mut missing_provision_node = pending_operation(
        "unavailable-provision-node",
        RemoteAction::ProvisionNode {
            cpu: 2,
            memory_gb: 4,
            disk_gb: 20,
            image: "worker@sha256:abc".into(),
        },
    );
    missing_provision_node.node_id = Some("n_missing".into());
    let mut missing_create_record = pending_operation(
        "unavailable-create-conversation",
        RemoteAction::CreateConversation {
            create_id: "create_unavailable".into(),
            title: "missing".into(),
            prompt: "missing".into(),
            parent_thread_id: None,
        },
    );
    missing_create_record.conversation_id = Some("c_missing".into());
    let cases = vec![
        pending_operation(
            "missing-provision-node",
            RemoteAction::ProvisionNode {
                cpu: 2,
                memory_gb: 4,
                disk_gb: 20,
                image: "worker@sha256:abc".into(),
            },
        ),
        missing_provision_node,
        pending_operation(
            "missing-create-conversation",
            RemoteAction::CreateConversation {
                create_id: "create_missing".into(),
                title: "missing".into(),
                prompt: "missing".into(),
                parent_thread_id: None,
            },
        ),
        missing_create_record,
        pending_operation(
            "missing-message-conversation",
            RemoteAction::SendMessage {
                message_id: "message_missing".into(),
                content: "missing".into(),
            },
        ),
        pending_operation(
            "missing-approval-conversation",
            RemoteAction::RespondApproval {
                approval_id: "approval_missing".into(),
                decision: ox_types::Decision::DenyOnce,
            },
        ),
        pending_operation(
            "missing-cancel-conversation",
            RemoteAction::CancelConversation {
                cancel_id: "cancel_missing".into(),
                reason: None,
            },
        ),
        pending_operation(
            "missing-ledger-conversation",
            RemoteAction::ReconcileLedger {
                from_seq: 0,
                parent_hash: None,
            },
        ),
        pending_operation(
            "missing-delete-node",
            RemoteAction::DeleteNode {
                delete_id: "delete_missing".into(),
                force: true,
                affected_references: Vec::new(),
            },
        ),
    ];
    local.reads.lock().unwrap().insert(
        "remote/operations/pending".into(),
        ReadOverride::Value(Value::Array(
            cases
                .iter()
                .map(|operation| structfs_serde_store::to_value(operation).unwrap())
                .collect(),
        )),
    );

    let results = manager.reconcile_pending().await.unwrap();
    assert_eq!(results.len(), cases.len());
    assert!(results.iter().all(|result| !result.applied));
    assert!(results.iter().all(|result| result.error.is_some()));
}

#[tokio::test]
async fn pending_replay_validates_persisted_resources_and_can_finish_a_delete() {
    let (_root, inner, provider, connector, original, started) =
        started_fixture("replay-validation").await;
    let conversation = original
        .get_conversation(&started.conversation_id)
        .await
        .unwrap()
        .unwrap();
    let local = Arc::new(FaultStore::new(inner));
    let manager = RemoteManagerStore::new(
        local.clone(),
        provider.clone(),
        connector,
        config("replay-validation-next"),
    )
    .unwrap();

    let mut overflow = pending_operation(
        "overflow-provision",
        RemoteAction::ProvisionNode {
            cpu: u32::from(u16::MAX) + 1,
            memory_gb: 4,
            disk_gb: 20,
            image: "worker@sha256:abc".into(),
        },
    );
    overflow.node_id = Some(started.node_id.clone());

    let mut create = pending_operation(
        "existing-create",
        RemoteAction::CreateConversation {
            create_id: conversation.create_id.clone(),
            title: conversation.title.clone(),
            prompt: conversation.initial_prompt.clone(),
            parent_thread_id: conversation.parent_thread_id.clone(),
        },
    );
    create.conversation_id = Some(conversation.conversation_id.clone());

    let mut delete = pending_operation(
        "valid-delete",
        RemoteAction::DeleteNode {
            delete_id: "replayed_delete".into(),
            force: true,
            affected_references: vec![conversation.conversation_id.clone()],
        },
    );
    delete.node_id = Some(started.node_id.clone());

    let operations = [overflow, create, delete];
    local.reads.lock().unwrap().insert(
        "remote/operations/pending".into(),
        ReadOverride::Value(Value::Array(
            operations
                .iter()
                .map(|operation| structfs_serde_store::to_value(operation).unwrap())
                .collect(),
        )),
    );

    let results = manager.reconcile_pending().await.unwrap();
    assert_eq!(results.len(), 3);
    assert!(results.iter().any(|result| {
        result.operation_id == "overflow-provision"
            && !result.applied
            && result
                .error
                .as_deref()
                .is_some_and(|error| error.contains("CPU"))
    }));
    assert!(
        results
            .iter()
            .any(|result| result.operation_id == "valid-delete" && result.applied)
    );
    assert!(provider.0.lock().unwrap().vm.is_none());
}

#[tokio::test]
async fn corrupted_replay_projections_and_delete_kind_collisions_fail_closed() {
    let (_root, inner, provider, connector, original, started) =
        started_fixture("corrupt-replay").await;
    let mut node = original.get_node(&started.node_id).await.unwrap().unwrap();
    let conversation = original
        .get_conversation(&started.conversation_id)
        .await
        .unwrap()
        .unwrap();
    let local = Arc::new(FaultStore::new(inner));
    let manager = RemoteManagerStore::new(
        local.clone(),
        provider,
        connector,
        config("corrupt-replay-next"),
    )
    .unwrap();
    let node_path = ox_inbox::remote_state::remote_item_path("nodes", &node.node_id)
        .unwrap()
        .to_string();

    let mut provision = pending_operation(
        "unknown-node-state",
        RemoteAction::ProvisionNode {
            cpu: 2,
            memory_gb: 4,
            disk_gb: 20,
            image: "worker@sha256:abc".into(),
        },
    );
    provision.node_id = Some(node.node_id.clone());
    node.observed_state = "future_state".into();
    local.reads.lock().unwrap().insert(
        node_path.clone(),
        ReadOverride::Value(structfs_serde_store::to_value(&node).unwrap()),
    );
    local.reads.lock().unwrap().insert(
        "remote/operations/pending".into(),
        ReadOverride::Value(Value::Array(vec![
            structfs_serde_store::to_value(&provision).unwrap(),
        ])),
    );
    let result = manager.reconcile_pending().await.unwrap();
    assert!(
        result[0]
            .error
            .as_deref()
            .is_some_and(|error| error.contains("unknown node state"))
    );

    node.observed_state = "pending".into();
    local.reads.lock().unwrap().insert(
        node_path.clone(),
        ReadOverride::Value(structfs_serde_store::to_value(&node).unwrap()),
    );
    let mut create = pending_operation(
        "create-on-nonready",
        RemoteAction::CreateConversation {
            create_id: conversation.create_id,
            title: conversation.title,
            prompt: conversation.initial_prompt,
            parent_thread_id: conversation.parent_thread_id,
        },
    );
    create.conversation_id = Some(conversation.conversation_id);
    local.reads.lock().unwrap().insert(
        "remote/operations/pending".into(),
        ReadOverride::Value(Value::Array(vec![
            structfs_serde_store::to_value(&create).unwrap(),
        ])),
    );
    let result = manager.reconcile_pending().await.unwrap();
    assert!(
        result[0]
            .error
            .as_deref()
            .is_some_and(|error| error.contains("not ready"))
    );

    local.reads.lock().unwrap().remove(&node_path);
    let delete_request = DeleteNodeManagerRequest {
        request_id: "delete-kind-collision".into(),
        delete_id: "delete_kind_collision".into(),
        force: true,
    };
    let delete_intent = RemoteOperationIntent {
        semantic_key: delete_request.request_id.clone(),
        node_id: Some(node.node_id.clone()),
        node_attempt_id: Some(node.node_attempt_id.clone()),
        conversation_id: None,
        action: RemoteAction::DeleteNode {
            delete_id: delete_request.delete_id.clone(),
            force: true,
            affected_references: Vec::new(),
        },
    };
    let operation_path =
        ox_inbox::remote_state::remote_operation_item_path(&delete_intent).unwrap();
    let collision = pending_operation(
        "collision",
        RemoteAction::SendMessage {
            message_id: "collision".into(),
            content: "collision".into(),
        },
    );
    local.reads.lock().unwrap().insert(
        operation_path.to_string(),
        ReadOverride::Value(structfs_serde_store::to_value(&collision).unwrap()),
    );
    assert!(matches!(
        manager.delete_node(&node.node_id, delete_request).await,
        Err(RemoteManagerError::Invalid(message)) if message.contains("kind collision")
    ));
}

#[tokio::test]
async fn applied_provision_and_replayed_create_detect_disappearing_node_records() {
    let root = tempfile::tempdir().unwrap();
    let inner: Arc<dyn StorePort> =
        Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
    let provider = Arc::new(FakeProvider::default());
    let connector = Arc::new(FakeConnector::new());
    let request = node_request("applied-node-disappears");
    let original = RemoteManagerStore::new(
        inner.clone(),
        provider.clone(),
        connector.clone(),
        config("applied-node-original"),
    )
    .unwrap();
    let created = original.create_node(request.clone()).await.unwrap();
    let node = original.get_node(&created.node_id).await.unwrap().unwrap();
    let node_path = ox_inbox::remote_state::remote_item_path("nodes", &created.node_id)
        .unwrap()
        .to_string();
    let local = Arc::new(FaultStore::new(inner));
    local.read_sequences.lock().unwrap().insert(
        node_path,
        VecDeque::from([
            ReadOverride::Value(structfs_serde_store::to_value(&node).unwrap()),
            ReadOverride::Missing,
        ]),
    );
    let retry =
        RemoteManagerStore::new(local, provider, connector, config("applied-node-retry")).unwrap();
    assert!(matches!(
        retry.create_node(request).await,
        Err(RemoteManagerError::Unavailable(message)) if message.contains("applied node record missing")
    ));

    let (_root, inner, provider, connector, original, started) =
        started_fixture("replayed-create-node-disappears").await;
    let node = original.get_node(&started.node_id).await.unwrap().unwrap();
    let conversation = original
        .get_conversation(&started.conversation_id)
        .await
        .unwrap()
        .unwrap();
    let local = Arc::new(FaultStore::new(inner));
    local.read_sequences.lock().unwrap().insert(
        ox_inbox::remote_state::remote_item_path("nodes", &node.node_id)
            .unwrap()
            .to_string(),
        VecDeque::from([
            ReadOverride::Value(structfs_serde_store::to_value(&node).unwrap()),
            ReadOverride::Missing,
        ]),
    );
    let mut operation = pending_operation(
        "replayed-create-node-disappears",
        RemoteAction::CreateConversation {
            create_id: conversation.create_id.clone(),
            title: conversation.title.clone(),
            prompt: conversation.initial_prompt.clone(),
            parent_thread_id: conversation.parent_thread_id.clone(),
        },
    );
    operation.conversation_id = Some(conversation.conversation_id);
    local.reads.lock().unwrap().insert(
        ox_inbox::remote_state::remote_item_path("operations", &operation.operation_id)
            .unwrap()
            .to_string(),
        ReadOverride::Value(structfs_serde_store::to_value(&operation).unwrap()),
    );
    local.reads.lock().unwrap().insert(
        "remote/operations/pending".into(),
        ReadOverride::Value(Value::Array(vec![
            structfs_serde_store::to_value(&operation).unwrap(),
        ])),
    );
    let manager = RemoteManagerStore::new(
        local,
        provider,
        connector,
        config("replayed-create-node-disappears-next"),
    )
    .unwrap();
    let result = manager.reconcile_pending().await.unwrap();
    assert!(
        result[0]
            .error
            .as_deref()
            .is_some_and(|error| error.contains("conversation node is missing")),
        "{result:?}"
    );
}

#[tokio::test]
async fn placement_and_ledger_batch_reject_structfs_shape_changes_at_their_boundaries() {
    let root = tempfile::tempdir().unwrap();
    let inner: Arc<dyn StorePort> =
        Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
    let local = Arc::new(FaultStore::new(inner));
    local.reads.lock().unwrap().insert(
        "remote/nodes".into(),
        ReadOverride::Value(Value::String("not-an-array".into())),
    );
    let manager = RemoteManagerStore::new(
        local,
        Arc::new(FakeProvider::default()),
        Arc::new(FakeConnector::new()),
        config("bad-placement-list"),
    )
    .unwrap();
    assert!(matches!(
        manager.start_conversation(request()).await,
        Err(RemoteManagerError::Invalid(message)) if message.contains("not an array")
    ));

    let (_root, inner, provider, connector, original, started) =
        started_fixture("changing-ledger-cursor").await;
    let conversation_path =
        ox_inbox::remote_state::remote_item_path("conversations", &started.conversation_id)
            .unwrap();
    let cursor_path = Path::parse(&format!("{conversation_path}/ledger/cursor")).unwrap();
    let cursor = inner
        .read(&cursor_path)
        .await
        .unwrap()
        .unwrap()
        .as_value()
        .unwrap()
        .clone();
    let local = Arc::new(FaultStore::new(inner));
    local.read_sequences.lock().unwrap().insert(
        cursor_path.to_string(),
        VecDeque::from([
            ReadOverride::Value(cursor),
            ReadOverride::Value(Value::String("not-a-map".into())),
        ]),
    );
    let manager = RemoteManagerStore::new(
        local,
        provider,
        connector,
        config("changing-ledger-cursor-next"),
    )
    .unwrap();
    assert!(matches!(
        manager
            .reconcile_ledger(&started.conversation_id, "shape-change")
            .await,
        Err(RemoteManagerError::Invalid(message)) if message.contains("not a map")
    ));
    assert!(
        original
            .get_conversation(&started.conversation_id)
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn provisioning_reports_node_disappearance_at_each_durable_projection_boundary() {
    for mode in ["before-observation", "after-observation", "after-receipt"] {
        let root = tempfile::tempdir().unwrap();
        let inner: Arc<dyn StorePort> =
            Arc::new(SyncStorePort::new(InboxStore::open(root.path()).unwrap()));
        let local = Arc::new(FaultStore::new(inner));
        let request_id = format!("disappearing-node-{mode}");
        let intent = node_intent(&request_id, RemoteNodeObservedState::Pending);
        let pending = ox_inbox::remote_state::RemoteNodeRecord {
            node_id: intent.node_id.clone(),
            node_attempt_id: intent.node_attempt_id.clone(),
            provider: intent.provider.clone(),
            vm_name: intent.vm_name.clone(),
            ssh_host: None,
            ssh_port: intent.ssh_port,
            ssh_user: None,
            ssh_dest: None,
            identity_path: intent.identity_path.clone(),
            known_hosts_path: intent.known_hosts_path.clone(),
            worker_socket_path: intent.worker_socket_path.clone(),
            desired_state: "active".into(),
            observed_state: "pending".into(),
            cleanup_state: "none".into(),
            image_digest: Some("worker@sha256:abc".into()),
        };
        let mut observed = pending.clone();
        observed.ssh_host = Some("203.0.113.10".into());
        observed.ssh_user = Some("route".into());
        observed.ssh_dest = Some(format!("route@{}", observed.vm_name));
        observed.observed_state = "provisioning".into();
        let values = match mode {
            "before-observation" => vec![
                ReadOverride::Value(structfs_serde_store::to_value(&pending).unwrap()),
                ReadOverride::Missing,
            ],
            "after-observation" => vec![
                ReadOverride::Value(structfs_serde_store::to_value(&pending).unwrap()),
                ReadOverride::Value(structfs_serde_store::to_value(&pending).unwrap()),
                ReadOverride::Missing,
            ],
            "after-receipt" => vec![
                ReadOverride::Value(structfs_serde_store::to_value(&pending).unwrap()),
                ReadOverride::Value(structfs_serde_store::to_value(&pending).unwrap()),
                ReadOverride::Value(structfs_serde_store::to_value(&observed).unwrap()),
                ReadOverride::Missing,
            ],
            _ => unreachable!(),
        };
        local.read_sequences.lock().unwrap().insert(
            ox_inbox::remote_state::remote_item_path("nodes", &intent.node_id)
                .unwrap()
                .to_string(),
            VecDeque::from(values),
        );
        let manager = RemoteManagerStore::new(
            local,
            Arc::new(FakeProvider::default()),
            Arc::new(FakeConnector::new()),
            config(&request_id),
        )
        .unwrap();
        let error = manager
            .create_node(node_request(&request_id))
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("disappeared") || error.contains("missing"),
            "{error}"
        );
    }
}
