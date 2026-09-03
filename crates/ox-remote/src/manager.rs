use std::sync::Arc;

use ox_broker::async_store::{AsyncReader, AsyncWriter, BoxFuture};
use ox_inbox::remote_state::{
    RemoteAction, RemoteCleanupState, RemoteConversationDesiredState, RemoteConversationIntent,
    RemoteConversationObservedState, RemoteConversationRecord, RemoteConversationUpdate,
    RemoteNodeDesiredState, RemoteNodeIntent, RemoteNodeObservation, RemoteNodeObservedState,
    RemoteNodeRecord, RemoteNodeUpdate, RemoteOperationIntent, RemoteOperationLeaseRelease,
    RemoteOperationLeaseRequest, RemoteOperationRecord, RemoteOperationResult,
    RemoteOperationState, RemoteOperationUpdate, RemotePlacement,
};
use ox_inbox::worker_ingress::{CancelEnvelope, CreateEnvelope, PromptEnvelope};
use serde::Serialize;
use sha2::{Digest, Sha256};
use structfs_core_store::{Error as StoreError, Path, Record, Value, path};

use crate::placement::{child, decode_record, encoded_item, select_existing, verify_worker};
use crate::{
    ApprovalRequest, CancelRequest, CrashInjector, CrashPoint, CreateNodeRequest, CreateNodeResult,
    DeleteNodeManagerRequest, DeleteNodeResult, DeleteVmRequest, MessageRequest, NoCrash,
    NodeDoctorResult, PlacementPolicy, ReconcileItem, RemoteManagerError, StartConversationRequest,
    StartConversationResult, StorePort, VmSpec, VmStatus, WorkerStoreConnector,
};

#[derive(Clone, Debug)]
pub struct RemoteManagerConfig {
    /// Invocation-unique lease owner. It must not be a shared installation or
    /// account ID; a new process uses a new value so fencing can distinguish it
    /// from a crashed predecessor.
    pub reconciler_id: String,
    pub lease_seconds: u32,
    pub provider: String,
    pub ssh_port: i64,
    pub identity_path: String,
    pub known_hosts_path: String,
    pub worker_socket_path: String,
}

pub struct RemoteManagerStore {
    local: Arc<dyn StorePort>,
    provider: Arc<dyn StorePort>,
    workers: Arc<dyn WorkerStoreConnector>,
    config: RemoteManagerConfig,
    crash: Arc<dyn CrashInjector>,
}

impl RemoteManagerStore {
    pub fn new(
        local: Arc<dyn StorePort>,
        provider: Arc<dyn StorePort>,
        workers: Arc<dyn WorkerStoreConnector>,
        config: RemoteManagerConfig,
    ) -> Result<Self, RemoteManagerError> {
        Self::with_crash_injector(local, provider, workers, config, Arc::new(NoCrash))
    }

    pub fn with_crash_injector(
        local: Arc<dyn StorePort>,
        provider: Arc<dyn StorePort>,
        workers: Arc<dyn WorkerStoreConnector>,
        config: RemoteManagerConfig,
        crash: Arc<dyn CrashInjector>,
    ) -> Result<Self, RemoteManagerError> {
        if config.reconciler_id.is_empty()
            || config.lease_seconds == 0
            || config.lease_seconds > 60
            || config.ssh_port <= 0
            || config.ssh_port > 65_535
        {
            return Err(RemoteManagerError::Invalid(
                "invalid reconciler id, lease, or SSH port".into(),
            ));
        }
        Ok(Self {
            local,
            provider,
            workers,
            config,
            crash,
        })
    }

    /// Provision a durable empty worker node. This uses the same node intent,
    /// fenced operation, provider Store, and identity verification as
    /// conversation-driven fresh placement.
    pub async fn create_node(
        &self,
        request: CreateNodeRequest,
    ) -> Result<CreateNodeResult, RemoteManagerError> {
        validate_node_request(&request)?;
        let node_id = stable_id("n", &request.request_id);
        let attempt_id = stable_id("a", &request.request_id);
        let vm_name = format!("ox-{}", digest_prefix(&request.request_id, 20));
        let intent = RemoteNodeIntent {
            node_id: node_id.clone(),
            node_attempt_id: attempt_id.clone(),
            provider: self.config.provider.clone(),
            vm_name: vm_name.clone(),
            ssh_host: None,
            ssh_port: self.config.ssh_port,
            ssh_user: None,
            ssh_dest: None,
            identity_path: self.config.identity_path.clone(),
            known_hosts_path: self.config.known_hosts_path.clone(),
            worker_socket_path: self.config.worker_socket_path.clone(),
            desired_state: RemoteNodeDesiredState::Active,
            observed_state: RemoteNodeObservedState::Pending,
            cleanup_state: RemoteCleanupState::None,
            image_digest: Some(request.node.image.clone()),
        };
        // Re-submit the deterministic intent even on retries. InboxStore keeps
        // the original request hash after provider observations, so this is an
        // idempotent no-op for the same request and a conflict for changed
        // image/resources under the same request ID.
        self.write_local(&path!("remote/nodes"), &intent, "persist node intent")
            .await?;
        self.crash.hit(CrashPoint::NodeIntentPersisted)?;
        let current = self
            .read_node(&node_id)
            .await?
            .ok_or_else(|| RemoteManagerError::Unavailable("persisted node disappeared".into()))?;
        if current.node_attempt_id != attempt_id {
            return Err(RemoteManagerError::IdentityMismatch(format!(
                "node {node_id} belongs to attempt {}",
                current.node_attempt_id
            )));
        }
        let operation = RemoteOperationIntent {
            semantic_key: format!("{}:provision", request.request_id),
            node_id: Some(node_id.clone()),
            node_attempt_id: Some(attempt_id.clone()),
            conversation_id: None,
            action: RemoteAction::ProvisionNode {
                cpu: u32::from(request.node.cpu),
                memory_gb: request.node.memory_mib / 1024,
                disk_gb: request.node.disk_gib,
                image: request.node.image.clone(),
            },
        };
        let operation_path = self.accept_operation(&operation).await?;
        self.crash.hit(CrashPoint::OperationIntentPersisted)?;
        // A ready projection does not prove the provision operation receipt
        // was committed: the process may have crashed between those writes.
        // Always drive the deterministic operation to a terminal state.
        self.provision_node(&operation_path, &intent, &request.node)
            .await?;
        Ok(CreateNodeResult {
            node_id,
            node_attempt_id: attempt_id,
            vm_name,
        })
    }

    pub async fn list_nodes(&self) -> Result<Vec<RemoteNodeRecord>, RemoteManagerError> {
        self.read_typed_list(&path!("remote/nodes"), "node listing")
            .await
    }

    pub async fn list_conversations(
        &self,
    ) -> Result<Vec<RemoteConversationRecord>, RemoteManagerError> {
        self.read_typed_list(&path!("remote/conversations"), "conversation listing")
            .await
    }

    pub async fn get_node(&self, id: &str) -> Result<Option<RemoteNodeRecord>, RemoteManagerError> {
        self.read_node(id).await
    }

    pub async fn get_conversation(
        &self,
        id: &str,
    ) -> Result<Option<RemoteConversationRecord>, RemoteManagerError> {
        self.read_conversation(id).await
    }

    pub async fn drain_node(&self, id: &str) -> Result<(), RemoteManagerError> {
        let node = self
            .read_node(id)
            .await?
            .ok_or_else(|| RemoteManagerError::Invalid("unknown node".into()))?;
        self.write_local(
            &child(&encoded_item("nodes", id)?, "state")?,
            &RemoteNodeUpdate {
                node_attempt_id: node.node_attempt_id,
                desired_state: Some(RemoteNodeDesiredState::Draining),
                observed_state: None,
                cleanup_state: None,
            },
            "drain node",
        )
        .await?;
        Ok(())
    }

    pub async fn doctor_node(&self, id: &str) -> Result<NodeDoctorResult, RemoteManagerError> {
        let node = self
            .read_node(id)
            .await?
            .ok_or_else(|| RemoteManagerError::Invalid("unknown node".into()))?;
        let worker = self
            .workers
            .connect(&node)
            .await
            .map_err(|error| RemoteManagerError::store("connect worker", error))?;
        let health = verify_worker(&worker, &node).await?;
        Ok(NodeDoctorResult { node, health })
    }

    pub async fn start_conversation(
        &self,
        request: StartConversationRequest,
    ) -> Result<StartConversationResult, RemoteManagerError> {
        validate_start(&request)?;
        let conversation_id = stable_id("c", &request.request_id);
        if let Some(existing) = self.read_conversation(&conversation_id).await? {
            // Placement was already durably closed before the first external
            // mutation. A retry must resume that exact node attempt.
            let mut node = self.read_node(&existing.node_id).await?.ok_or_else(|| {
                RemoteManagerError::Unavailable("persisted placement node is missing".into())
            })?;
            let conversation = conversation_intent_for_node(&request, &conversation_id, &node);
            self.write_local(
                &path!("remote/conversations"),
                &conversation,
                "verify conversation intent",
            )
            .await?;
            let create_path = self
                .accept_operation(&create_operation(&request, &conversation))
                .await?;
            if let Some(worker_thread_id) = existing.worker_thread_id {
                // The binding can be durable while the create operation is
                // still pending after a crash. Replay the stable create_id so
                // the worker and local operation receipt converge.
                let worker = self
                    .workers
                    .connect(&node)
                    .await
                    .map_err(|error| RemoteManagerError::store("resume worker", error))?;
                verify_worker(&worker, &node).await?;
                let replayed_thread_id = self
                    .create_worker_conversation(&create_path, &conversation, &worker)
                    .await?;
                if replayed_thread_id != worker_thread_id {
                    return Err(RemoteManagerError::IdentityMismatch(format!(
                        "conversation {} is bound to worker thread {worker_thread_id}, but create receipt returned {replayed_thread_id}",
                        existing.conversation_id
                    )));
                }
                return Ok(StartConversationResult {
                    conversation_id,
                    node_id: existing.node_id,
                    node_attempt_id: existing.node_attempt_id,
                    worker_thread_id: replayed_thread_id,
                });
            }
            if node.observed_state != "ready" {
                let provision = RemoteOperationIntent {
                    semantic_key: format!("{}:provision", request.request_id),
                    node_id: Some(node.node_id.clone()),
                    node_attempt_id: Some(node.node_attempt_id.clone()),
                    conversation_id: None,
                    action: RemoteAction::ProvisionNode {
                        cpu: u32::from(request.node.cpu),
                        memory_gb: request.node.memory_mib / 1024,
                        disk_gb: request.node.disk_gib,
                        image: request.node.image.clone(),
                    },
                };
                let provision_path = self.accept_operation(&provision).await?;
                node = self
                    .provision_node(
                        &provision_path,
                        &node_record_as_intent(&node)?,
                        &request.node,
                    )
                    .await?;
            }
            let worker = self
                .workers
                .connect(&node)
                .await
                .map_err(|error| RemoteManagerError::store("resume worker", error))?;
            verify_worker(&worker, &node).await?;
            let worker_thread_id = self
                .create_worker_conversation(&create_path, &conversation, &worker)
                .await?;
            return Ok(StartConversationResult {
                conversation_id,
                node_id: node.node_id,
                node_attempt_id: node.node_attempt_id,
                worker_thread_id,
            });
        }

        // A crash may occur immediately after the deterministic fresh-node
        // intent commit and before the conversation row. Recover that intent
        // before consulting live placement again.
        let deterministic_node_id = stable_id("n", &request.request_id);
        if let Some(mut node) = self.read_node(&deterministic_node_id).await? {
            let conversation = conversation_intent_for_node(&request, &conversation_id, &node);
            self.write_local(
                &path!("remote/conversations"),
                &conversation,
                "persist recovered conversation intent",
            )
            .await?;
            let provision = RemoteOperationIntent {
                semantic_key: format!("{}:provision", request.request_id),
                node_id: Some(node.node_id.clone()),
                node_attempt_id: Some(node.node_attempt_id.clone()),
                conversation_id: None,
                action: RemoteAction::ProvisionNode {
                    cpu: u32::from(request.node.cpu),
                    memory_gb: request.node.memory_mib / 1024,
                    disk_gb: request.node.disk_gib,
                    image: request.node.image.clone(),
                },
            };
            let provision_path = self.accept_operation(&provision).await?;
            let create_path = self
                .accept_operation(&create_operation(&request, &conversation))
                .await?;
            if node.observed_state != "ready" {
                node = self
                    .provision_node(
                        &provision_path,
                        &node_record_as_intent(&node)?,
                        &request.node,
                    )
                    .await?;
            }
            let worker = self
                .workers
                .connect(&node)
                .await
                .map_err(|error| RemoteManagerError::store("recover worker", error))?;
            verify_worker(&worker, &node).await?;
            let worker_thread_id = self
                .create_worker_conversation(&create_path, &conversation, &worker)
                .await?;
            return Ok(StartConversationResult {
                conversation_id,
                node_id: node.node_id,
                node_attempt_id: node.node_attempt_id,
                worker_thread_id,
            });
        }

        let selected = select_existing(&request.placement, &self.local, &self.workers).await?;
        let (node, worker) = if let Some(selected) = selected {
            selected
        } else {
            // `select_existing` returns `RequiredNodeUnavailable` itself for
            // every unsuccessful `RequireNode` placement, so this arm is only
            // reachable for policies that permit provisioning.
            let node_id = stable_id("n", &request.request_id);
            let attempt_id = stable_id("a", &request.request_id);
            let vm_name = format!("ox-{}", digest_prefix(&request.request_id, 20));
            let intent = RemoteNodeIntent {
                node_id: node_id.clone(),
                node_attempt_id: attempt_id.clone(),
                provider: self.config.provider.clone(),
                vm_name,
                ssh_host: None,
                ssh_port: self.config.ssh_port,
                ssh_user: None,
                ssh_dest: None,
                identity_path: self.config.identity_path.clone(),
                known_hosts_path: self.config.known_hosts_path.clone(),
                worker_socket_path: self.config.worker_socket_path.clone(),
                desired_state: RemoteNodeDesiredState::Active,
                observed_state: RemoteNodeObservedState::Pending,
                cleanup_state: RemoteCleanupState::None,
                image_digest: Some(request.node.image.clone()),
            };
            self.write_local(&path!("remote/nodes"), &intent, "persist node intent")
                .await?;
            self.crash.hit(CrashPoint::NodeIntentPersisted)?;
            let conversation = conversation_intent(&request, &conversation_id, &intent);
            self.write_local(
                &path!("remote/conversations"),
                &conversation,
                "persist conversation intent",
            )
            .await?;
            let provision = RemoteOperationIntent {
                semantic_key: format!("{}:provision", request.request_id),
                node_id: Some(node_id.clone()),
                node_attempt_id: Some(attempt_id.clone()),
                conversation_id: None,
                action: RemoteAction::ProvisionNode {
                    cpu: u32::from(request.node.cpu),
                    memory_gb: request.node.memory_mib / 1024,
                    disk_gb: request.node.disk_gib,
                    image: request.node.image.clone(),
                },
            };
            let provision_path = self.accept_operation(&provision).await?;
            let create = create_operation(&request, &conversation);
            self.accept_operation(&create).await?;
            self.crash.hit(CrashPoint::OperationIntentPersisted)?;
            let node = self
                .provision_node(&provision_path, &intent, &request.node)
                .await?;
            let worker = self.workers.connect(&node).await.map_err(|error| {
                RemoteManagerError::store("connect worker after provision", error)
            })?;
            verify_worker(&worker, &node).await?;
            (node, worker)
        };

        let conversation = conversation_intent_for_node(&request, &conversation_id, &node);
        self.write_local(
            &path!("remote/conversations"),
            &conversation,
            "persist conversation intent",
        )
        .await?;
        let create = create_operation(&request, &conversation);
        let create_path = self.accept_operation(&create).await?;
        self.crash.hit(CrashPoint::OperationIntentPersisted)?;
        let worker_thread_id = self
            .create_worker_conversation(&create_path, &conversation, &worker)
            .await?;
        Ok(StartConversationResult {
            conversation_id,
            node_id: node.node_id,
            node_attempt_id: node.node_attempt_id,
            worker_thread_id,
        })
    }

    async fn provision_node(
        &self,
        operation_path: &Path,
        intent: &RemoteNodeIntent,
        spec: &crate::NodeProvisionSpec,
    ) -> Result<RemoteNodeRecord, RemoteManagerError> {
        if let Some(applied) = self.applied_operation(operation_path).await? {
            let _ = applied;
            return self.read_node(&intent.node_id).await?.ok_or_else(|| {
                RemoteManagerError::Unavailable("applied node record missing".into())
            });
        }
        let epoch = self.claim(operation_path).await?;
        let vm_spec = VmSpec {
            schema_version: 1,
            name: intent.vm_name.clone(),
            node_id: intent.node_id.clone(),
            node_attempt_id: intent.node_attempt_id.clone(),
            image: spec.image.clone(),
            cpu: spec.cpu,
            memory_mib: spec.memory_mib,
            disk_gib: spec.disk_gib,
            comment: format!(
                "ox node {} attempt {}",
                intent.node_id, intent.node_attempt_id
            ),
            tags: vec!["ox-worker".into()],
            integrations: vec![],
        };
        let write = self
            .provider
            .write(&path!("vms"), typed_record(&vm_spec)?)
            .await;
        let vm_path = crate::vm_path(&intent.vm_name)
            .map_err(|error| RemoteManagerError::store("provider VM path", error))?;
        let vm = match write {
            Ok(_) => self.provider.read(&vm_path).await,
            Err(_) => self.provider.read(&vm_path).await,
        };
        let vm = match vm {
            Ok(Some(record)) => decode_record::<VmStatus>(record, "provider VM")?,
            Ok(None) => {
                self.update_node_state(
                    &intent.node_id,
                    &intent.node_attempt_id,
                    RemoteNodeObservedState::Absent,
                )
                .await?;
                self.fail_operation(
                    operation_path,
                    epoch,
                    "provider_absent",
                    "exact provider query proved VM absent",
                )
                .await?;
                return Err(RemoteManagerError::Unavailable(
                    "provider proved VM absent after create".into(),
                ));
            }
            Err(error) => {
                self.update_node_state(
                    &intent.node_id,
                    &intent.node_attempt_id,
                    RemoteNodeObservedState::Unavailable,
                )
                .await?;
                self.release(operation_path, epoch).await?;
                return Err(RemoteManagerError::store("provider reconcile", error));
            }
        };
        self.crash.hit(CrashPoint::ExternalEffectReturned)?;
        if vm.vm_name != intent.vm_name {
            self.fail_operation(
                operation_path,
                epoch,
                "identity_mismatch",
                "provider returned wrong VM name",
            )
            .await?;
            return Err(RemoteManagerError::IdentityMismatch(vm.vm_name));
        }
        let current = self.read_node(&intent.node_id).await?.ok_or_else(|| {
            RemoteManagerError::Unavailable("node disappeared before observation".into())
        })?;
        let observation = RemoteNodeObservation {
            node_attempt_id: intent.node_attempt_id.clone(),
            ssh_host: vm.ssh_host,
            ssh_user: vm.ssh_user,
            ssh_dest: vm.ssh_dest,
            observed_state: match current.observed_state.as_str() {
                "unavailable" => RemoteNodeObservedState::Unavailable,
                // A crash can leave the health-verified Ready projection
                // durable while the operation receipt remains pending.
                "ready" => RemoteNodeObservedState::Ready,
                _ => RemoteNodeObservedState::Provisioning,
            },
        };
        let path = child(&encoded_item("nodes", &intent.node_id)?, "observation")?;
        self.write_local(&path, &observation, "persist provider observation")
            .await?;
        let mut node = self.read_node(&intent.node_id).await?.ok_or_else(|| {
            RemoteManagerError::Unavailable("provisioned node record missing".into())
        })?;
        let worker = match self.workers.connect(&node).await {
            Ok(worker) => worker,
            Err(error) => {
                self.update_node_state(
                    &node.node_id,
                    &node.node_attempt_id,
                    RemoteNodeObservedState::Unavailable,
                )
                .await?;
                self.release(operation_path, epoch).await?;
                return Err(RemoteManagerError::store("connect worker", error));
            }
        };
        if let Err(error) = verify_worker(&worker, &node).await {
            match error {
                RemoteManagerError::IdentityMismatch(_) => {
                    self.fail_operation(
                        operation_path,
                        epoch,
                        "identity_mismatch",
                        &error.to_string(),
                    )
                    .await?;
                }
                _ => {
                    self.update_node_state(
                        &node.node_id,
                        &node.node_attempt_id,
                        RemoteNodeObservedState::Unavailable,
                    )
                    .await?;
                    self.release(operation_path, epoch).await?;
                }
            }
            return Err(error);
        }
        self.update_node_state(
            &node.node_id,
            &node.node_attempt_id,
            RemoteNodeObservedState::Ready,
        )
        .await?;
        self.crash.hit(CrashPoint::ProjectionCommitted)?;
        self.complete_operation(
            operation_path,
            epoch,
            RemoteOperationResult {
                result_path: Some(format!("vms/{}", node.vm_name)),
                error_code: None,
                error_message: None,
            },
        )
        .await?;
        self.crash.hit(CrashPoint::ResultCommitted)?;
        node = self.read_node(&intent.node_id).await?.ok_or_else(|| {
            RemoteManagerError::Unavailable("ready node record disappeared".into())
        })?;
        Ok(node)
    }

    async fn create_worker_conversation(
        &self,
        operation_path: &Path,
        conversation: &RemoteConversationIntent,
        worker: &Arc<dyn StorePort>,
    ) -> Result<String, RemoteManagerError> {
        if let Some(result) = self.applied_operation(operation_path).await? {
            if let Some(path) = result.result_path {
                return path
                    .split('/')
                    .next_back()
                    .map(str::to_owned)
                    .ok_or_else(|| RemoteManagerError::Invalid("empty create receipt".into()));
            }
        }
        let node = self
            .read_node(&conversation.node_id)
            .await?
            .ok_or_else(|| {
                RemoteManagerError::Unavailable("conversation node is missing".into())
            })?;
        verify_worker(worker, &node).await?;
        let epoch = self.claim(operation_path).await?;
        let current = self
            .read_conversation(&conversation.conversation_id)
            .await?
            .ok_or_else(|| RemoteManagerError::Unavailable("conversation disappeared".into()))?;
        if matches!(current.observed_state.as_str(), "pending" | "creating") {
            self.update_conversation(
                conversation,
                None,
                RemoteConversationObservedState::Creating,
            )
            .await?;
        }
        let envelope = CreateEnvelope {
            create_id: conversation.create_id.clone(),
            title: conversation.title.clone(),
            prompt: conversation.initial_prompt.clone(),
            parent_id: conversation.parent_thread_id.clone(),
        };
        let receipt = match worker
            .write(&path!("conversations"), typed_record(&envelope)?)
            .await
        {
            Ok(path) => path,
            Err(error) => {
                self.release(operation_path, epoch).await?;
                if let Some(current) = self
                    .read_conversation(&conversation.conversation_id)
                    .await?
                {
                    self.mark_unavailable_or_lost(&node, Some(&current)).await?;
                }
                return Err(RemoteManagerError::store("worker create", error));
            }
        };
        self.crash.hit(CrashPoint::ExternalEffectReturned)?;
        let thread_id = receipt
            .iter()
            .last()
            .cloned()
            .ok_or_else(|| RemoteManagerError::Invalid("empty worker create path".into()))?;
        let thread_path = Path::parse(&format!("conversations/{thread_id}"))?;
        if worker
            .read(&thread_path)
            .await
            .map_err(|error| RemoteManagerError::store("verify worker thread", error))?
            .is_none()
        {
            self.release(operation_path, epoch).await?;
            return Err(RemoteManagerError::Unavailable(
                "worker create receipt has no thread".into(),
            ));
        }
        self.update_conversation(
            conversation,
            Some(thread_id.clone()),
            RemoteConversationObservedState::Running,
        )
        .await?;
        self.crash.hit(CrashPoint::ProjectionCommitted)?;
        self.complete_operation(
            operation_path,
            epoch,
            RemoteOperationResult {
                result_path: Some(format!("conversations/{thread_id}")),
                error_code: None,
                error_message: None,
            },
        )
        .await?;
        self.crash.hit(CrashPoint::ResultCommitted)?;
        Ok(thread_id)
    }

    pub async fn send_message(
        &self,
        conversation_id: &str,
        request: MessageRequest,
    ) -> Result<Path, RemoteManagerError> {
        let action = RemoteAction::SendMessage {
            message_id: request.message_id.clone(),
            content: request.content.clone(),
        };
        self.apply_conversation_action(conversation_id, request.request_id, action)
            .await
    }

    pub async fn respond_approval(
        &self,
        conversation_id: &str,
        request: ApprovalRequest,
    ) -> Result<Path, RemoteManagerError> {
        let action = RemoteAction::RespondApproval {
            approval_id: request.approval_id.clone(),
            decision: request.decision,
        };
        self.apply_conversation_action(conversation_id, request.request_id, action)
            .await
    }

    pub async fn cancel(
        &self,
        conversation_id: &str,
        request: CancelRequest,
    ) -> Result<Path, RemoteManagerError> {
        let action = RemoteAction::CancelConversation {
            cancel_id: request.cancel_id.clone(),
            reason: request.reason.clone(),
        };
        self.apply_conversation_action(conversation_id, request.request_id, action)
            .await
    }

    async fn apply_conversation_action(
        &self,
        conversation_id: &str,
        semantic_key: String,
        action: RemoteAction,
    ) -> Result<Path, RemoteManagerError> {
        let is_cancel = matches!(action, RemoteAction::CancelConversation { .. });
        let conversation = self
            .read_conversation(conversation_id)
            .await?
            .ok_or_else(|| RemoteManagerError::Invalid("unknown remote conversation".into()))?;
        if !is_cancel && conversation.desired_state != "active" {
            return Err(RemoteManagerError::Invalid(format!(
                "conversation desired state is {}; messages and approvals require active",
                conversation.desired_state
            )));
        }
        let operation = RemoteOperationIntent {
            semantic_key,
            node_id: Some(conversation.node_id.clone()),
            node_attempt_id: Some(conversation.node_attempt_id.clone()),
            conversation_id: Some(conversation.conversation_id.clone()),
            action: action.clone(),
        };
        let operation_path = self.accept_operation(&operation).await?;
        self.crash.hit(CrashPoint::OperationIntentPersisted)?;
        if is_cancel {
            self.write_local(
                &child(&encoded_item("conversations", conversation_id)?, "state")?,
                &RemoteConversationUpdate {
                    node_attempt_id: conversation.node_attempt_id.clone(),
                    worker_thread_id: None,
                    desired_state: Some(RemoteConversationDesiredState::Canceled),
                    observed_state: None,
                    cleanup_state: None,
                },
                "mark conversation cancel requested",
            )
            .await?;
        }
        if let Some(result) = self.applied_operation(&operation_path).await? {
            return Path::parse(result.result_path.as_deref().unwrap_or("conversations"))
                .map_err(Into::into);
        }
        let node = self
            .read_node(&conversation.node_id)
            .await?
            .ok_or_else(|| {
                RemoteManagerError::Unavailable("conversation node is missing".into())
            })?;
        let worker = match self.workers.connect(&node).await {
            Ok(worker) => worker,
            Err(error) => {
                self.mark_unavailable_or_lost(&node, Some(&conversation))
                    .await?;
                return Err(RemoteManagerError::store("connect worker", error));
            }
        };
        if let Err(error) = verify_worker(&worker, &node).await {
            self.mark_unavailable_or_lost(&node, Some(&conversation))
                .await?;
            return Err(error);
        }
        let epoch = self.claim(&operation_path).await?;
        let thread = conversation.worker_thread_id.as_deref().ok_or_else(|| {
            RemoteManagerError::Invalid("conversation has no worker thread".into())
        })?;
        let (target, record) = match action {
            RemoteAction::SendMessage {
                message_id,
                content,
            } => (
                Path::parse(&format!("conversations/{thread}/messages"))?,
                typed_record(&PromptEnvelope {
                    message_id,
                    content,
                })?,
            ),
            RemoteAction::RespondApproval {
                approval_id,
                decision,
            } => (
                Path::parse(&format!("conversations/{thread}/approvals/{approval_id}"))?,
                typed_record(&ox_types::ApprovalResponse { decision })?,
            ),
            RemoteAction::CancelConversation { cancel_id, reason } => (
                Path::parse(&format!("conversations/{thread}/control/cancel"))?,
                typed_record(&CancelEnvelope { cancel_id, reason })?,
            ),
            _ => unreachable!(),
        };
        let receipt = match worker.write(&target, record).await {
            Ok(receipt) => receipt,
            Err(error) => {
                self.release(&operation_path, epoch).await?;
                self.mark_unavailable_or_lost(&node, Some(&conversation))
                    .await?;
                return Err(RemoteManagerError::store("worker mutation", error));
            }
        };
        self.crash.hit(CrashPoint::ExternalEffectReturned)?;
        self.complete_operation(
            &operation_path,
            epoch,
            RemoteOperationResult {
                result_path: Some(receipt.to_string()),
                error_code: None,
                error_message: None,
            },
        )
        .await?;
        Ok(receipt)
    }

    /// Refresh lifecycle state from the existing worker conversation record.
    /// The remote worker remains the authoritative executor; this only updates
    /// the local orchestration projection.
    pub async fn refresh_conversation(
        &self,
        conversation_id: &str,
    ) -> Result<RemoteConversationRecord, RemoteManagerError> {
        #[derive(serde::Deserialize)]
        struct WorkerConversationSnapshot {
            id: String,
            thread_state: ox_types::ThreadState,
        }

        let conversation = self
            .read_conversation(conversation_id)
            .await?
            .ok_or_else(|| RemoteManagerError::Invalid("unknown remote conversation".into()))?;
        let node = self
            .read_node(&conversation.node_id)
            .await?
            .ok_or_else(|| RemoteManagerError::Unavailable("conversation node missing".into()))?;
        let worker = self
            .workers
            .connect(&node)
            .await
            .map_err(|error| RemoteManagerError::store("connect worker", error))?;
        verify_worker(&worker, &node).await?;
        let thread_id = conversation.worker_thread_id.as_deref().ok_or_else(|| {
            RemoteManagerError::Unavailable("conversation has no worker thread".into())
        })?;
        let record = worker
            .read(&Path::parse(&format!("conversations/{thread_id}"))?)
            .await
            .map_err(|error| RemoteManagerError::store("worker conversation", error))?
            .ok_or_else(|| RemoteManagerError::Unavailable("worker conversation missing".into()))?;
        let snapshot: WorkerConversationSnapshot = decode_record(record, "worker conversation")?;
        if snapshot.id != thread_id {
            return Err(RemoteManagerError::IdentityMismatch(format!(
                "expected worker thread {thread_id}, got {}",
                snapshot.id
            )));
        }
        let observed = match snapshot.thread_state {
            ox_types::ThreadState::Running => RemoteConversationObservedState::Running,
            ox_types::ThreadState::WaitingForInput => {
                RemoteConversationObservedState::WaitingForInput
            }
            ox_types::ThreadState::BlockedOnApproval => {
                RemoteConversationObservedState::BlockedOnApproval
            }
            ox_types::ThreadState::Completed => RemoteConversationObservedState::Completed,
            ox_types::ThreadState::Errored => RemoteConversationObservedState::Errored,
            ox_types::ThreadState::Interrupted if conversation.desired_state == "canceled" => {
                RemoteConversationObservedState::Canceled
            }
            ox_types::ThreadState::Interrupted => RemoteConversationObservedState::Errored,
        };
        self.update_conversation_record(&conversation, None, observed)
            .await?;
        self.read_conversation(conversation_id)
            .await?
            .ok_or_else(|| RemoteManagerError::Unavailable("conversation disappeared".into()))
    }

    pub async fn reconcile_ledger(
        &self,
        conversation_id: &str,
        _request_id: &str,
    ) -> Result<(), RemoteManagerError> {
        let conversation = self
            .read_conversation(conversation_id)
            .await?
            .ok_or_else(|| RemoteManagerError::Invalid("unknown remote conversation".into()))?;
        let conversation_path = encoded_item("conversations", conversation_id)?;
        let cursor_path = child(&child(&conversation_path, "ledger")?, "cursor")?;
        let cursor = self
            .local
            .read(&cursor_path)
            .await
            .map_err(|error| RemoteManagerError::store("read local ledger cursor", error))?
            .ok_or_else(|| RemoteManagerError::Unavailable("ledger cursor missing".into()))?;
        let map = match cursor.as_value() {
            Some(Value::Map(map)) => map,
            _ => return Err(RemoteManagerError::Invalid("invalid ledger cursor".into())),
        };
        let last_seq = match map.get("last_seq") {
            Some(Value::Integer(seq)) => *seq,
            _ => -1,
        };
        let parent_hash = match map.get("last_hash") {
            Some(Value::String(hash)) => Some(hash.clone()),
            _ => None,
        };
        let operation = RemoteOperationIntent {
            // One durable operation per cursor position. Empty polls release
            // this row for reuse; advancing polls apply it and the next cursor
            // naturally derives a new semantic key.
            semantic_key: format!("ledger:{conversation_id}:{}", last_seq + 1),
            node_id: Some(conversation.node_id.clone()),
            node_attempt_id: Some(conversation.node_attempt_id.clone()),
            conversation_id: Some(conversation.conversation_id.clone()),
            action: RemoteAction::ReconcileLedger {
                from_seq: last_seq + 1,
                parent_hash,
            },
        };
        let operation_path = self.accept_operation(&operation).await?;
        if self.applied_operation(&operation_path).await?.is_some() {
            return Ok(());
        }
        let epoch = self.claim(&operation_path).await?;
        let node = self
            .read_node(&conversation.node_id)
            .await?
            .ok_or_else(|| RemoteManagerError::Unavailable("node missing".into()))?;
        let worker = match self.workers.connect(&node).await {
            Ok(worker) => worker,
            Err(error) => {
                self.release(&operation_path, epoch).await?;
                self.mark_unavailable_or_lost(&node, Some(&conversation))
                    .await?;
                return Err(RemoteManagerError::store("connect worker", error));
            }
        };
        if let Err(error) = verify_worker(&worker, &node).await {
            self.release(&operation_path, epoch).await?;
            self.mark_unavailable_or_lost(&node, Some(&conversation))
                .await?;
            return Err(error);
        }
        let advanced =
            match crate::reconcile::reconcile_ledger_batches(&self.local, &worker, &conversation)
                .await
            {
                Ok(advanced) => advanced,
                Err(error) => {
                    self.release(&operation_path, epoch).await?;
                    self.mark_unavailable_or_lost(&node, Some(&conversation))
                        .await?;
                    return Err(error);
                }
            };
        if !advanced {
            self.release(&operation_path, epoch).await?;
            return Ok(());
        }
        self.complete_operation(
            &operation_path,
            epoch,
            RemoteOperationResult {
                result_path: Some(cursor_path.to_string()),
                error_code: None,
                error_message: None,
            },
        )
        .await?;
        Ok(())
    }

    /// Replays every pending or expired-running durable intent. Each item is
    /// isolated by its own fenced lease, so one unavailable node does not
    /// block reconciliation of unrelated nodes or conversations.
    pub async fn reconcile_pending(&self) -> Result<Vec<ReconcileItem>, RemoteManagerError> {
        let record = self
            .local
            .read(&path!("remote/operations/pending"))
            .await
            .map_err(|error| RemoteManagerError::store("read pending operations", error))?
            .ok_or_else(|| RemoteManagerError::Unavailable("pending operations missing".into()))?;
        let Value::Array(values) = record.as_value().cloned().ok_or_else(|| {
            RemoteManagerError::Invalid("pending operations were not parsed".into())
        })?
        else {
            return Err(RemoteManagerError::Invalid(
                "pending operations are not an array".into(),
            ));
        };
        let mut operations = values
            .into_iter()
            .map(|value| {
                structfs_serde_store::from_value::<RemoteOperationRecord>(value)
                    .map_err(|error| RemoteManagerError::store("decode pending operation", error))
            })
            .collect::<Result<Vec<_>, _>>()?;
        operations.sort_by_key(|operation| match &operation.intent.action {
            RemoteAction::ProvisionNode { .. } => 0,
            RemoteAction::CreateConversation { .. } => 1,
            _ => 2,
        });
        let mut results = Vec::with_capacity(operations.len());
        for operation in operations {
            let operation_id = operation.operation_id.clone();
            let result = self.replay_operation(operation).await;
            results.push(ReconcileItem {
                operation_id,
                applied: result.is_ok(),
                error: result.err().map(|error| error.to_string()),
            });
        }
        Ok(results)
    }

    async fn replay_operation(
        &self,
        operation: RemoteOperationRecord,
    ) -> Result<(), RemoteManagerError> {
        match operation.intent.action.clone() {
            RemoteAction::ProvisionNode {
                cpu,
                memory_gb,
                disk_gb,
                image,
            } => {
                let node_id = operation
                    .node_id
                    .as_deref()
                    .ok_or_else(|| RemoteManagerError::Invalid("provision has no node".into()))?;
                let node = self.read_node(node_id).await?.ok_or_else(|| {
                    RemoteManagerError::Unavailable("provision node missing".into())
                })?;
                let path = encoded_item("operations", &operation.operation_id)?;
                self.provision_node(
                    &path,
                    &node_record_as_intent(&node)?,
                    &crate::NodeProvisionSpec {
                        image,
                        cpu: u16::try_from(cpu).map_err(|_| {
                            RemoteManagerError::Invalid("CPU value overflow".into())
                        })?,
                        memory_mib: memory_gb.saturating_mul(1024),
                        disk_gib: disk_gb,
                    },
                )
                .await?;
            }
            RemoteAction::CreateConversation { .. } => {
                let conversation_id = operation.conversation_id.as_deref().ok_or_else(|| {
                    RemoteManagerError::Invalid("create has no conversation".into())
                })?;
                let conversation =
                    self.read_conversation(conversation_id)
                        .await?
                        .ok_or_else(|| {
                            RemoteManagerError::Unavailable("conversation missing".into())
                        })?;
                let node = self
                    .read_node(&conversation.node_id)
                    .await?
                    .ok_or_else(|| {
                        RemoteManagerError::Unavailable("conversation node missing".into())
                    })?;
                if node.observed_state != "ready" {
                    return Err(RemoteManagerError::Unavailable(
                        "conversation node is not ready".into(),
                    ));
                }
                let worker = self
                    .workers
                    .connect(&node)
                    .await
                    .map_err(|error| RemoteManagerError::store("connect worker", error))?;
                verify_worker(&worker, &node).await?;
                let intent = RemoteConversationIntent {
                    conversation_id: conversation.conversation_id,
                    node_id: conversation.node_id,
                    node_attempt_id: conversation.node_attempt_id,
                    create_id: conversation.create_id,
                    title: conversation.title,
                    initial_prompt: conversation.initial_prompt,
                    parent_thread_id: conversation.parent_thread_id,
                    placement: match conversation.placement.as_str() {
                        "prefer_existing" => RemotePlacement::PreferExisting,
                        "require_node" => RemotePlacement::RequireNode,
                        _ => RemotePlacement::FreshNode,
                    },
                    desired_state: RemoteConversationDesiredState::Active,
                    observed_state: RemoteConversationObservedState::Pending,
                    cleanup_state: RemoteCleanupState::None,
                };
                self.create_worker_conversation(
                    &encoded_item("operations", &operation.operation_id)?,
                    &intent,
                    &worker,
                )
                .await?;
            }
            RemoteAction::SendMessage { .. }
            | RemoteAction::RespondApproval { .. }
            | RemoteAction::CancelConversation { .. } => {
                let conversation_id = operation.conversation_id.as_deref().ok_or_else(|| {
                    RemoteManagerError::Invalid("worker action has no conversation".into())
                })?;
                self.apply_conversation_action(
                    conversation_id,
                    operation.intent.semantic_key,
                    operation.intent.action,
                )
                .await?;
            }
            RemoteAction::ReconcileLedger { .. } => {
                let conversation_id = operation.conversation_id.as_deref().ok_or_else(|| {
                    RemoteManagerError::Invalid("ledger action has no conversation".into())
                })?;
                self.reconcile_ledger(conversation_id, &operation.intent.semantic_key)
                    .await?;
            }
            RemoteAction::DeleteNode {
                delete_id, force, ..
            } => {
                let node_id = operation.node_id.as_deref().ok_or_else(|| {
                    RemoteManagerError::Invalid("delete action has no node".into())
                })?;
                self.delete_node(
                    node_id,
                    DeleteNodeManagerRequest {
                        request_id: operation.intent.semantic_key,
                        delete_id,
                        force,
                    },
                )
                .await?;
            }
        }
        Ok(())
    }

    pub async fn delete_node(
        &self,
        node_id: &str,
        request: DeleteNodeManagerRequest,
    ) -> Result<DeleteNodeResult, RemoteManagerError> {
        let node = self
            .read_node(node_id)
            .await?
            .ok_or_else(|| RemoteManagerError::Invalid("unknown node".into()))?;
        if node.desired_state == "active" {
            self.write_local(
                &child(&encoded_item("nodes", node_id)?, "state")?,
                &RemoteNodeUpdate {
                    node_attempt_id: node.node_attempt_id.clone(),
                    desired_state: Some(RemoteNodeDesiredState::Draining),
                    observed_state: None,
                    cleanup_state: None,
                },
                "drain node before deletion",
            )
            .await?;
        }
        let probe = RemoteOperationIntent {
            semantic_key: request.request_id.clone(),
            node_id: Some(node.node_id.clone()),
            node_attempt_id: Some(node.node_attempt_id.clone()),
            conversation_id: None,
            action: RemoteAction::DeleteNode {
                delete_id: request.delete_id.clone(),
                force: request.force,
                affected_references: Vec::new(),
            },
        };
        let operation_path = ox_inbox::remote_state::remote_operation_item_path(&probe)
            .map_err(|error| RemoteManagerError::store("delete operation path", error))?;
        let mut affected = if let Some(existing) = self
            .local
            .read(&operation_path)
            .await
            .map_err(|error| RemoteManagerError::store("read delete operation", error))?
        {
            let existing: RemoteOperationRecord = decode_record(existing, "delete operation")?;
            match existing.intent.action {
                RemoteAction::DeleteNode {
                    affected_references,
                    ..
                } => affected_references,
                _ => {
                    return Err(RemoteManagerError::Invalid(
                        "operation kind collision".into(),
                    ));
                }
            }
        } else {
            let mut affected = self.local_active_references(node_id).await?;
            match self
                .provider
                .read(
                    &crate::vm_path(&node.vm_name)
                        .map_err(|error| RemoteManagerError::store("provider VM path", error))?,
                )
                .await
            {
                Ok(Some(_)) => {
                    let worker = self.workers.connect(&node).await.map_err(|error| {
                        RemoteManagerError::store("connect worker for deletion", error)
                    })?;
                    verify_worker(&worker, &node).await?;
                    affected.extend(self.worker_active_references(&worker).await?);
                }
                Ok(None) => {}
                Err(error) => {
                    return Err(RemoteManagerError::store(
                        "provider deletion preflight",
                        error,
                    ));
                }
            }
            affected.sort();
            affected.dedup();
            affected
        };
        let operation = RemoteOperationIntent {
            action: RemoteAction::DeleteNode {
                delete_id: request.delete_id.clone(),
                force: request.force,
                affected_references: affected.clone(),
            },
            ..probe
        };
        let operation_path = self.accept_operation(&operation).await?;
        let operation_record = self.read_operation(&operation_path).await?;
        if operation_record.state == RemoteOperationState::Applied {
            let result = operation_record.result.unwrap_or(RemoteOperationResult {
                result_path: None,
                error_code: None,
                error_message: None,
            });
            let affected_references = result
                .error_message
                .as_deref()
                .and_then(|value| serde_json::from_str(value).ok())
                .unwrap_or_default();
            return Ok(DeleteNodeResult {
                node_id: node_id.into(),
                affected_references,
                forced: request.force,
            });
        }
        if operation_record.state == RemoteOperationState::Failed {
            return Err(RemoteManagerError::ActiveReferences(affected));
        }
        let epoch = self.claim(&operation_path).await?;
        if !affected.is_empty() && !request.force {
            self.fail_operation(
                &operation_path,
                epoch,
                "active_references",
                &serde_json::to_string(&affected).unwrap_or_default(),
            )
            .await?;
            return Err(RemoteManagerError::ActiveReferences(affected));
        }
        // This exact-name query comes before worker access on replay: if the
        // previous process deleted the VM, absence is sufficient to finish the
        // local commit without trying to dial a machine that no longer exists.
        let provider_path = crate::vm_path(&node.vm_name)
            .map_err(|error| RemoteManagerError::store("provider VM path", error))?;
        match self.provider.read(&provider_path).await {
            Ok(None) => {
                self.finish_deleted_node(&node, &operation_path, epoch, &affected)
                    .await?;
                return Ok(DeleteNodeResult {
                    node_id: node_id.into(),
                    affected_references: affected,
                    forced: request.force,
                });
            }
            Ok(Some(_)) => {}
            Err(error) => {
                self.release(&operation_path, epoch).await?;
                return Err(RemoteManagerError::store(
                    "provider deletion reconcile",
                    error,
                ));
            }
        }
        let worker = match self.workers.connect(&node).await {
            Ok(worker) => worker,
            Err(error) => {
                self.release(&operation_path, epoch).await?;
                return Err(RemoteManagerError::store(
                    "connect worker for deletion",
                    error,
                ));
            }
        };
        if let Err(error) = verify_worker(&worker, &node).await {
            self.release(&operation_path, epoch).await?;
            return Err(error);
        }
        let current = match self.worker_active_references(&worker).await {
            Ok(current) => current,
            Err(error) => {
                self.release(&operation_path, epoch).await?;
                return Err(error);
            }
        };
        if !current.is_empty() && !request.force {
            self.release(&operation_path, epoch).await?;
            return Err(RemoteManagerError::ActiveReferences(current));
        }
        affected.extend(current);
        affected.sort();
        affected.dedup();
        let target = crate::vm_delete_path(&node.vm_name)
            .map_err(|error| RemoteManagerError::store("provider delete path", error))?;
        let delete = DeleteVmRequest {
            schema_version: 1,
            deletion_id: request.delete_id,
            node_id: node.node_id.clone(),
            node_attempt_id: node.node_attempt_id.clone(),
        };
        if let Err(error) = self.provider.write(&target, typed_record(&delete)?).await {
            self.release(&operation_path, epoch).await?;
            return Err(RemoteManagerError::store("provider delete", error));
        }
        self.crash.hit(CrashPoint::ExternalEffectReturned)?;
        let exact = self
            .provider
            .read(
                &crate::vm_path(&node.vm_name)
                    .map_err(|error| RemoteManagerError::store("provider VM path", error))?,
            )
            .await
            .map_err(|error| RemoteManagerError::store("provider delete reconcile", error))?;
        if exact.is_some() {
            self.release(&operation_path, epoch).await?;
            return Err(RemoteManagerError::Unavailable(
                "provider still reports node".into(),
            ));
        }
        self.finish_deleted_node(&node, &operation_path, epoch, &affected)
            .await?;
        Ok(DeleteNodeResult {
            node_id: node_id.into(),
            affected_references: affected,
            forced: request.force,
        })
    }

    async fn worker_active_references(
        &self,
        worker: &Arc<dyn StorePort>,
    ) -> Result<Vec<String>, RemoteManagerError> {
        let listing = worker
            .read(&path!("conversations"))
            .await
            .map_err(|error| RemoteManagerError::store("worker conversations", error))?
            .ok_or_else(|| {
                RemoteManagerError::Unavailable("worker conversations missing".into())
            })?;
        let mut affected = Vec::new();
        if let Some(Value::Array(items)) = listing.as_value() {
            for item in items {
                if let Value::Map(map) = item {
                    let terminal = matches!(map.get("thread_state"), Some(Value::String(state)) if matches!(state.as_str(), "completed" | "interrupted" | "errored"));
                    if !terminal {
                        if let Some(Value::String(id)) = map.get("id") {
                            affected.push(format!("worker:{id}"));
                        }
                    }
                }
            }
        }
        Ok(affected)
    }

    async fn finish_deleted_node(
        &self,
        node: &RemoteNodeRecord,
        operation_path: &Path,
        epoch: i64,
        affected: &[String],
    ) -> Result<(), RemoteManagerError> {
        for conversation_id in self.local_active_references(&node.node_id).await? {
            if let Some(conversation) = self.read_conversation(&conversation_id).await? {
                self.update_conversation_record(
                    &conversation,
                    None,
                    RemoteConversationObservedState::Lost,
                )
                .await?;
            }
        }
        let state_path = child(&encoded_item("nodes", &node.node_id)?, "state")?;
        self.write_local(
            &state_path,
            &RemoteNodeUpdate {
                node_attempt_id: node.node_attempt_id.clone(),
                desired_state: Some(RemoteNodeDesiredState::Deleted),
                observed_state: Some(RemoteNodeObservedState::Absent),
                cleanup_state: Some(RemoteCleanupState::Pending),
            },
            "mark node deletion pending",
        )
        .await?;
        self.write_local(
            &state_path,
            &RemoteNodeUpdate {
                node_attempt_id: node.node_attempt_id.clone(),
                desired_state: None,
                observed_state: None,
                cleanup_state: Some(RemoteCleanupState::Complete),
            },
            "mark node deleted",
        )
        .await?;
        self.complete_operation(
            operation_path,
            epoch,
            RemoteOperationResult {
                result_path: Some(format!("nodes/{}", node.node_id)),
                error_code: None,
                error_message: Some(serde_json::to_string(affected).unwrap_or_default()),
            },
        )
        .await?;
        self.crash.hit(CrashPoint::ResultCommitted)?;
        Ok(())
    }

    async fn local_active_references(
        &self,
        node_id: &str,
    ) -> Result<Vec<String>, RemoteManagerError> {
        let record = self
            .local
            .read(&path!("remote/conversations"))
            .await
            .map_err(|error| RemoteManagerError::store("list conversations", error))?
            .ok_or_else(|| {
                RemoteManagerError::Unavailable("conversation listing missing".into())
            })?;
        let Value::Array(items) = record
            .as_value()
            .cloned()
            .ok_or_else(|| RemoteManagerError::Invalid("conversation listing not parsed".into()))?
        else {
            return Err(RemoteManagerError::Invalid(
                "conversation listing not array".into(),
            ));
        };
        let mut refs = Vec::new();
        for value in items {
            let conversation: RemoteConversationRecord = structfs_serde_store::from_value(value)
                .map_err(|error| RemoteManagerError::store("decode conversation", error))?;
            if conversation.node_id == node_id
                && !matches!(
                    conversation.observed_state.as_str(),
                    "completed" | "canceled" | "errored" | "lost"
                )
            {
                refs.push(conversation.conversation_id);
            }
        }
        Ok(refs)
    }

    async fn mark_unavailable_or_lost(
        &self,
        node: &RemoteNodeRecord,
        conversation: Option<&RemoteConversationRecord>,
    ) -> Result<(), RemoteManagerError> {
        let provider_path = crate::vm_path(&node.vm_name)
            .map_err(|error| RemoteManagerError::store("provider VM path", error))?;
        let (node_state, conversation_state) = match self.provider.read(&provider_path).await {
            Ok(None) => (
                RemoteNodeObservedState::Absent,
                RemoteConversationObservedState::Lost,
            ),
            Ok(Some(_)) | Err(_) => (
                RemoteNodeObservedState::Unavailable,
                RemoteConversationObservedState::Unavailable,
            ),
        };
        self.update_node_state(&node.node_id, &node.node_attempt_id, node_state)
            .await?;
        if let Some(conversation) = conversation {
            self.update_conversation_record(conversation, None, conversation_state)
                .await?;
        }
        Ok(())
    }

    async fn accept_operation(
        &self,
        intent: &RemoteOperationIntent,
    ) -> Result<Path, RemoteManagerError> {
        self.write_local(
            &path!("remote/operations"),
            intent,
            "persist operation intent",
        )
        .await
    }

    async fn claim(&self, operation_path: &Path) -> Result<i64, RemoteManagerError> {
        let request = RemoteOperationLeaseRequest {
            owner_id: self.config.reconciler_id.clone(),
            lease_seconds: self.config.lease_seconds,
        };
        let path = child(operation_path, "lease")?;
        let receipt = self
            .write_local(&path, &request, "claim operation lease")
            .await
            .map_err(|error| match error {
                RemoteManagerError::Store { message, .. } if message.contains("lease is held") => {
                    RemoteManagerError::LeaseHeld
                }
                other => other,
            })?;
        receipt
            .iter()
            .last()
            .and_then(|part| part.parse().ok())
            .ok_or_else(|| RemoteManagerError::Invalid("lease receipt has no epoch".into()))
    }

    async fn release(&self, operation_path: &Path, epoch: i64) -> Result<(), RemoteManagerError> {
        let request = RemoteOperationLeaseRelease {
            owner_id: self.config.reconciler_id.clone(),
            lease_epoch: epoch,
        };
        let lease = child(operation_path, "lease")?;
        self.write_local(
            &child(&lease, "release")?,
            &request,
            "release operation lease",
        )
        .await?;
        Ok(())
    }

    async fn complete_operation(
        &self,
        operation_path: &Path,
        epoch: i64,
        result: RemoteOperationResult,
    ) -> Result<(), RemoteManagerError> {
        let operation = self.read_operation(operation_path).await?;
        let update = RemoteOperationUpdate {
            node_attempt_id: operation.node_attempt_id,
            expected_state: RemoteOperationState::Running,
            state: RemoteOperationState::Applied,
            lease_owner: Some(self.config.reconciler_id.clone()),
            lease_epoch: Some(epoch),
            result: Some(result),
        };
        self.write_local(
            &child(operation_path, "state")?,
            &update,
            "commit operation result",
        )
        .await?;
        Ok(())
    }

    async fn fail_operation(
        &self,
        operation_path: &Path,
        epoch: i64,
        code: &str,
        message: &str,
    ) -> Result<(), RemoteManagerError> {
        let operation = self.read_operation(operation_path).await?;
        let update = RemoteOperationUpdate {
            node_attempt_id: operation.node_attempt_id,
            expected_state: RemoteOperationState::Running,
            state: RemoteOperationState::Failed,
            lease_owner: Some(self.config.reconciler_id.clone()),
            lease_epoch: Some(epoch),
            result: Some(RemoteOperationResult {
                result_path: None,
                error_code: Some(code.into()),
                error_message: Some(message.into()),
            }),
        };
        self.write_local(
            &child(operation_path, "state")?,
            &update,
            "commit operation failure",
        )
        .await?;
        Ok(())
    }

    async fn applied_operation(
        &self,
        path: &Path,
    ) -> Result<Option<RemoteOperationResult>, RemoteManagerError> {
        let operation = self.read_operation(path).await?;
        Ok((operation.state == RemoteOperationState::Applied)
            .then_some(operation.result)
            .flatten())
    }

    async fn read_operation(
        &self,
        path: &Path,
    ) -> Result<RemoteOperationRecord, RemoteManagerError> {
        let record = self
            .local
            .read(path)
            .await
            .map_err(|error| RemoteManagerError::store("read operation", error))?
            .ok_or_else(|| RemoteManagerError::Unavailable("operation record missing".into()))?;
        decode_record(record, "operation record")
    }

    async fn read_node(&self, id: &str) -> Result<Option<RemoteNodeRecord>, RemoteManagerError> {
        self.read_typed_optional(&encoded_item("nodes", id)?, "node record")
            .await
    }

    async fn read_conversation(
        &self,
        id: &str,
    ) -> Result<Option<RemoteConversationRecord>, RemoteManagerError> {
        self.read_typed_optional(&encoded_item("conversations", id)?, "conversation record")
            .await
    }

    async fn read_typed_optional<T: serde::de::DeserializeOwned>(
        &self,
        path: &Path,
        operation: &'static str,
    ) -> Result<Option<T>, RemoteManagerError> {
        self.local
            .read(path)
            .await
            .map_err(|error| RemoteManagerError::store(operation, error))?
            .map(|record| decode_record(record, operation))
            .transpose()
    }

    async fn read_typed_list<T: serde::de::DeserializeOwned>(
        &self,
        path: &Path,
        operation: &'static str,
    ) -> Result<Vec<T>, RemoteManagerError> {
        let record = self
            .local
            .read(path)
            .await
            .map_err(|error| RemoteManagerError::store(operation, error))?
            .ok_or_else(|| RemoteManagerError::Unavailable(format!("{operation} missing")))?;
        let Value::Array(values) = record
            .as_value()
            .cloned()
            .ok_or_else(|| RemoteManagerError::Invalid(format!("{operation} was not parsed")))?
        else {
            return Err(RemoteManagerError::Invalid(format!(
                "{operation} was not an array"
            )));
        };
        values
            .into_iter()
            .map(|value| {
                structfs_serde_store::from_value(value)
                    .map_err(|error| RemoteManagerError::store(operation, error))
            })
            .collect()
    }

    async fn update_node_state(
        &self,
        id: &str,
        attempt: &str,
        state: RemoteNodeObservedState,
    ) -> Result<(), RemoteManagerError> {
        let update = RemoteNodeUpdate {
            node_attempt_id: attempt.into(),
            desired_state: None,
            observed_state: Some(state),
            cleanup_state: None,
        };
        self.write_local(
            &child(&encoded_item("nodes", id)?, "state")?,
            &update,
            "update node state",
        )
        .await?;
        Ok(())
    }

    async fn update_conversation(
        &self,
        conversation: &RemoteConversationIntent,
        worker_thread_id: Option<String>,
        state: RemoteConversationObservedState,
    ) -> Result<(), RemoteManagerError> {
        let record = RemoteConversationRecord {
            conversation_id: conversation.conversation_id.clone(),
            node_id: conversation.node_id.clone(),
            node_attempt_id: conversation.node_attempt_id.clone(),
            worker_thread_id: None,
            create_id: conversation.create_id.clone(),
            title: conversation.title.clone(),
            initial_prompt: conversation.initial_prompt.clone(),
            parent_thread_id: conversation.parent_thread_id.clone(),
            placement: placement_str(conversation.placement).into(),
            desired_state: "active".into(),
            observed_state: "pending".into(),
            cleanup_state: "none".into(),
        };
        self.update_conversation_record(&record, worker_thread_id, state)
            .await
    }

    async fn update_conversation_record(
        &self,
        conversation: &RemoteConversationRecord,
        worker_thread_id: Option<String>,
        state: RemoteConversationObservedState,
    ) -> Result<(), RemoteManagerError> {
        let update = RemoteConversationUpdate {
            node_attempt_id: conversation.node_attempt_id.clone(),
            worker_thread_id,
            desired_state: None,
            observed_state: Some(state),
            cleanup_state: None,
        };
        self.write_local(
            &child(
                &encoded_item("conversations", &conversation.conversation_id)?,
                "state",
            )?,
            &update,
            "update conversation",
        )
        .await?;
        Ok(())
    }

    async fn write_local<T: Serialize>(
        &self,
        path: &Path,
        value: &T,
        operation: &'static str,
    ) -> Result<Path, RemoteManagerError> {
        self.local
            .write(path, typed_record(value)?)
            .await
            .map_err(|error| RemoteManagerError::store(operation, error))
    }
}

impl AsyncReader for RemoteManagerStore {
    fn read(&mut self, from: &Path) -> BoxFuture<Result<Option<Record>, StoreError>> {
        let parts: Vec<&str> = from.iter().map(String::as_str).collect();
        if let ["nodes", id, "doctor"] = parts.as_slice() {
            let this = self.clone_for_future();
            let id = (*id).to_owned();
            return Box::pin(async move {
                let result = this.doctor_node(&id).await.map_err(manager_store_error)?;
                Ok(Some(typed_record(&result).map_err(manager_store_error)?))
            });
        }
        if let ["conversations", id, "refresh"] = parts.as_slice() {
            let this = self.clone_for_future();
            let id = (*id).to_owned();
            return Box::pin(async move {
                let result = this
                    .refresh_conversation(&id)
                    .await
                    .map_err(manager_store_error)?;
                Ok(Some(typed_record(&result).map_err(manager_store_error)?))
            });
        }
        if parts.as_slice() == ["doctor", "provider"] {
            let provider = self.provider.clone();
            return Box::pin(async move {
                let identity = provider.read(&path!("identity")).await?;
                let vms = provider.read(&path!("vms")).await?;
                let value = serde_json::json!({
                    "identity": identity.and_then(|record| record.as_value().cloned()).map(structfs_serde_store::value_to_json),
                    "vms": vms.and_then(|record| record.as_value().cloned()).map(structfs_serde_store::value_to_json),
                });
                Ok(Some(Record::parsed(structfs_serde_store::json_to_value(
                    value,
                ))))
            });
        }
        let local = self.local.clone();
        let target = if from.iter().next().is_some_and(|part| part == "remote") {
            from.clone()
        } else {
            Path::parse(&format!("remote/{from}")).unwrap_or_else(|_| from.clone())
        };
        Box::pin(async move { local.read(&target).await })
    }
}

impl AsyncWriter for RemoteManagerStore {
    fn write(&mut self, to: &Path, data: Record) -> BoxFuture<Result<Path, StoreError>> {
        let this = self.clone_for_future();
        let to = to.clone();
        Box::pin(async move {
            let parts: Vec<&str> = to.iter().map(String::as_str).collect();
            let result = match parts.as_slice() {
                ["nodes"] => {
                    let request: CreateNodeRequest =
                        decode_record(data, "create node").map_err(manager_store_error)?;
                    let result = this
                        .create_node(request)
                        .await
                        .map_err(manager_store_error)?;
                    Path::parse(&format!("nodes/{}", result.node_id)).map_err(StoreError::from)
                }
                ["nodes", id, "drain"] => {
                    this.drain_node(id).await.map_err(manager_store_error)?;
                    Path::parse(&format!("nodes/{id}")).map_err(StoreError::from)
                }
                ["conversations"] => {
                    let request: StartConversationRequest =
                        decode_record(data, "start conversation").map_err(manager_store_error)?;
                    let result = this
                        .start_conversation(request)
                        .await
                        .map_err(manager_store_error)?;
                    Path::parse(&format!("conversations/{}", result.conversation_id))
                        .map_err(StoreError::from)
                }
                ["conversations", id, "messages"] => {
                    let request = decode_record(data, "message").map_err(manager_store_error)?;
                    this.send_message(id, request)
                        .await
                        .map_err(manager_store_error)
                }
                ["conversations", id, "approvals"] => {
                    let request = decode_record(data, "approval").map_err(manager_store_error)?;
                    this.respond_approval(id, request)
                        .await
                        .map_err(manager_store_error)
                }
                ["conversations", id, "cancel"] => {
                    let request = decode_record(data, "cancel").map_err(manager_store_error)?;
                    this.cancel(id, request).await.map_err(manager_store_error)
                }
                ["conversations", id, "reconcile"] => {
                    let request: serde_json::Value = decode_record(data, "reconcile conversation")
                        .map_err(manager_store_error)?;
                    let request_id = request
                        .get("request_id")
                        .and_then(serde_json::Value::as_str)
                        .ok_or_else(|| {
                            manager_store_error(RemoteManagerError::Invalid(
                                "reconcile request_id is required".into(),
                            ))
                        })?;
                    this.reconcile_ledger(id, request_id)
                        .await
                        .map_err(manager_store_error)?;
                    Path::parse(&format!("conversations/{id}/ledger")).map_err(StoreError::from)
                }
                ["nodes", id, "delete"] => {
                    let request =
                        decode_record(data, "delete node").map_err(manager_store_error)?;
                    this.delete_node(id, request)
                        .await
                        .map_err(manager_store_error)?;
                    Path::parse(&format!("nodes/{id}")).map_err(StoreError::from)
                }
                ["reconcile"] => {
                    this.reconcile_pending()
                        .await
                        .map_err(manager_store_error)?;
                    Path::parse("reconcile").map_err(StoreError::from)
                }
                _ => Err(StoreError::NoRoute { path: to }),
            }?;
            Ok(result)
        })
    }
}

impl RemoteManagerStore {
    fn clone_for_future(&self) -> Self {
        Self {
            local: self.local.clone(),
            provider: self.provider.clone(),
            workers: self.workers.clone(),
            config: self.config.clone(),
            crash: self.crash.clone(),
        }
    }
}

fn manager_store_error(error: RemoteManagerError) -> StoreError {
    StoreError::store("RemoteManagerStore", "operation", error.to_string())
}

fn typed_record<T: Serialize>(value: &T) -> Result<Record, RemoteManagerError> {
    structfs_serde_store::to_value(value)
        .map(Record::parsed)
        .map_err(|error| RemoteManagerError::store("encode record", error))
}

fn digest_prefix(value: &str, bytes: usize) -> String {
    let digest = format!("{:x}", Sha256::digest(value.as_bytes()));
    digest[..bytes.min(digest.len())].into()
}

fn stable_id(prefix: &str, value: &str) -> String {
    format!("{prefix}_{}", digest_prefix(value, 32))
}

fn validate_start(request: &StartConversationRequest) -> Result<(), RemoteManagerError> {
    if request.schema_version != 1
        || request.request_id.is_empty()
        || request.title.is_empty()
        || request.node.cpu == 0
        || request.node.memory_mib < 1024
        || request.node.memory_mib % 1024 != 0
        || request.node.disk_gib == 0
    {
        return Err(RemoteManagerError::Invalid(
            "invalid start conversation request".into(),
        ));
    }
    Ok(())
}

fn validate_node_request(request: &CreateNodeRequest) -> Result<(), RemoteManagerError> {
    if request.schema_version != 1
        || request.request_id.is_empty()
        || request.node.cpu == 0
        || request.node.memory_mib < 1024
        || request.node.memory_mib % 1024 != 0
        || request.node.disk_gib == 0
    {
        return Err(RemoteManagerError::Invalid(
            "invalid create node request".into(),
        ));
    }
    Ok(())
}

fn conversation_intent(
    request: &StartConversationRequest,
    conversation_id: &str,
    node: &RemoteNodeIntent,
) -> RemoteConversationIntent {
    RemoteConversationIntent {
        conversation_id: conversation_id.into(),
        node_id: node.node_id.clone(),
        node_attempt_id: node.node_attempt_id.clone(),
        create_id: stable_id("create", &request.request_id),
        title: request.title.clone(),
        initial_prompt: request.prompt.clone(),
        parent_thread_id: request.parent_thread_id.clone(),
        placement: map_placement(&request.placement),
        desired_state: RemoteConversationDesiredState::Active,
        observed_state: RemoteConversationObservedState::Pending,
        cleanup_state: RemoteCleanupState::None,
    }
}

fn conversation_intent_for_node(
    request: &StartConversationRequest,
    conversation_id: &str,
    node: &RemoteNodeRecord,
) -> RemoteConversationIntent {
    RemoteConversationIntent {
        conversation_id: conversation_id.into(),
        node_id: node.node_id.clone(),
        node_attempt_id: node.node_attempt_id.clone(),
        create_id: stable_id("create", &request.request_id),
        title: request.title.clone(),
        initial_prompt: request.prompt.clone(),
        parent_thread_id: request.parent_thread_id.clone(),
        placement: map_placement(&request.placement),
        desired_state: RemoteConversationDesiredState::Active,
        observed_state: RemoteConversationObservedState::Pending,
        cleanup_state: RemoteCleanupState::None,
    }
}

fn node_record_as_intent(node: &RemoteNodeRecord) -> Result<RemoteNodeIntent, RemoteManagerError> {
    let observed_state = match node.observed_state.as_str() {
        "pending" => RemoteNodeObservedState::Pending,
        "provisioning" => RemoteNodeObservedState::Provisioning,
        "ready" => RemoteNodeObservedState::Ready,
        "unavailable" => RemoteNodeObservedState::Unavailable,
        "absent" => RemoteNodeObservedState::Absent,
        "errored" => RemoteNodeObservedState::Errored,
        other => {
            return Err(RemoteManagerError::Invalid(format!(
                "unknown node state {other}"
            )));
        }
    };
    Ok(RemoteNodeIntent {
        node_id: node.node_id.clone(),
        node_attempt_id: node.node_attempt_id.clone(),
        provider: node.provider.clone(),
        vm_name: node.vm_name.clone(),
        ssh_host: node.ssh_host.clone(),
        ssh_port: node.ssh_port,
        ssh_user: node.ssh_user.clone(),
        ssh_dest: node.ssh_dest.clone(),
        identity_path: node.identity_path.clone(),
        known_hosts_path: node.known_hosts_path.clone(),
        worker_socket_path: node.worker_socket_path.clone(),
        desired_state: RemoteNodeDesiredState::Active,
        observed_state,
        cleanup_state: RemoteCleanupState::None,
        image_digest: node.image_digest.clone(),
    })
}

fn create_operation(
    request: &StartConversationRequest,
    conversation: &RemoteConversationIntent,
) -> RemoteOperationIntent {
    RemoteOperationIntent {
        semantic_key: format!("{}:create", request.request_id),
        node_id: Some(conversation.node_id.clone()),
        node_attempt_id: Some(conversation.node_attempt_id.clone()),
        conversation_id: Some(conversation.conversation_id.clone()),
        action: RemoteAction::CreateConversation {
            create_id: conversation.create_id.clone(),
            title: conversation.title.clone(),
            prompt: conversation.initial_prompt.clone(),
            parent_thread_id: conversation.parent_thread_id.clone(),
        },
    }
}

fn map_placement(value: &PlacementPolicy) -> RemotePlacement {
    match value {
        PlacementPolicy::FreshNode => RemotePlacement::FreshNode,
        PlacementPolicy::PreferExisting => RemotePlacement::PreferExisting,
        PlacementPolicy::RequireNode { .. } => RemotePlacement::RequireNode,
    }
}
fn placement_str(value: RemotePlacement) -> &'static str {
    match value {
        RemotePlacement::FreshNode => "fresh_node",
        RemotePlacement::PreferExisting => "prefer_existing",
        RemotePlacement::RequireNode => "require_node",
    }
}

impl From<structfs_core_store::Error> for RemoteManagerError {
    fn from(value: structfs_core_store::Error) -> Self {
        Self::store("path", value)
    }
}
