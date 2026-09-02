use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use ox_broker::async_store::{AsyncReader, AsyncWriter};
use ox_inbox::InboxStore;
use ox_remote::{
    ApprovalRequest, CancelRequest, CrashInjector, CrashPoint, CreateNodeRequest,
    DeleteNodeManagerRequest, MessageRequest, NodeProvisionSpec, PlacementPolicy,
    RemoteManagerConfig, RemoteManagerError, RemoteManagerStore, StartConversationRequest,
    StorePort, SyncStorePort, VmSpec, VmStatus, WorkerStoreConnector,
};
use structfs_core_store::{Error as StoreError, Path, Record, Value, path};

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
                ox_remote::vm_path(&spec.name)
                    .map_err(|error| StoreError::store("FakeProvider", "path", error.to_string()))
            }
            ["vms", component, "delete"] => {
                let name = ox_remote::decode_vm_component(component).map_err(|error| {
                    StoreError::store("FakeProvider", "decode", error.to_string())
                })?;
                if state.vm.take().is_some() {
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
        lease_seconds: 1,
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

    tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
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

    tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
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

    tokio::time::sleep(std::time::Duration::from_millis(1_100)).await;
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
