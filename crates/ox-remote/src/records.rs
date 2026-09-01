use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "policy", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlacementPolicy {
    #[default]
    FreshNode,
    PreferExisting,
    RequireNode {
        node_id: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeProvisionSpec {
    pub image: String,
    pub cpu: u16,
    pub memory_mib: u32,
    pub disk_gib: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartConversationRequest {
    pub schema_version: u32,
    /// Stable semantic ID supplied by the caller. Repeating it repeats the
    /// same durable node/conversation/create operation.
    pub request_id: String,
    pub title: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_thread_id: Option<String>,
    #[serde(default)]
    pub placement: PlacementPolicy,
    pub node: NodeProvisionSpec,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateNodeRequest {
    pub schema_version: u32,
    pub request_id: String,
    pub node: NodeProvisionSpec,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateNodeResult {
    pub node_id: String,
    pub node_attempt_id: String,
    pub vm_name: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeDoctorResult {
    pub node: ox_inbox::remote_state::RemoteNodeRecord,
    pub health: WorkerHealth,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerSandboxHealth {
    pub mode: String,
    pub preflight: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkerHealth {
    pub status: String,
    pub node_id: String,
    pub attempt_id: String,
    pub worker_version: String,
    pub wire_version: u64,
    pub image_digest: String,
    pub agent_wasm_sha256: String,
    pub executable_sha256: String,
    pub policy_profile: String,
    pub policy_contract_sha256: String,
    pub sandbox_enforcement: WorkerSandboxHealth,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MessageRequest {
    pub request_id: String,
    pub message_id: String,
    pub content: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRequest {
    pub request_id: String,
    pub approval_id: String,
    pub decision: ox_types::Decision,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelRequest {
    pub request_id: String,
    pub cancel_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeleteNodeManagerRequest {
    pub request_id: String,
    pub delete_id: String,
    #[serde(default)]
    pub force: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartConversationResult {
    pub conversation_id: String,
    pub node_id: String,
    pub node_attempt_id: String,
    pub worker_thread_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeleteNodeResult {
    pub node_id: String,
    pub affected_references: Vec<String>,
    pub forced: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReconcileItem {
    pub operation_id: String,
    pub applied: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CrashPoint {
    NodeIntentPersisted,
    OperationIntentPersisted,
    ExternalEffectReturned,
    ProjectionCommitted,
    ResultCommitted,
}

pub trait CrashInjector: Send + Sync {
    fn hit(&self, point: CrashPoint) -> Result<(), crate::RemoteManagerError>;
}

#[derive(Default)]
pub struct NoCrash;

impl CrashInjector for NoCrash {
    fn hit(&self, _point: CrashPoint) -> Result<(), crate::RemoteManagerError> {
        Ok(())
    }
}
