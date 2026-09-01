//! Durable local orchestration state for remote ox execution.
//!
//! This module is an additive projection in the existing `InboxStore` database.
//! It does not own remote execution state or replace a worker thread ledger.

use crate::{InboxStore, id_codec};
use rusqlite::{OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path as FsPath;
use std::time::{SystemTime, UNIX_EPOCH};
use structfs_core_store::{Error as StoreError, Path, Record, Value};

const MAX_ID_BYTES: usize = 512;
const MAX_LEDGER_BATCH: usize = 1_024;
const MAX_LEDGER_ENTRY_BYTES: usize = 8 * 1024 * 1024;
const MAX_LEDGER_BATCH_BYTES: usize = 16 * 1024 * 1024;
const MAX_OPERATION_TEXT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteNodeIntent {
    pub node_id: String,
    pub node_attempt_id: String,
    pub provider: String,
    pub vm_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_host: Option<String>,
    pub ssh_port: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_dest: Option<String>,
    /// Filesystem handle only. Private-key bytes are not accepted by this type.
    pub identity_path: String,
    /// Filesystem handle only. Known-host contents are not accepted by this type.
    pub known_hosts_path: String,
    pub worker_socket_path: String,
    pub desired_state: RemoteNodeDesiredState,
    pub observed_state: RemoteNodeObservedState,
    pub cleanup_state: RemoteCleanupState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_digest: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteNodeAttemptReplacement {
    pub expected_attempt_id: String,
    pub node: RemoteNodeIntent,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteNodeUpdate {
    pub node_attempt_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desired_state: Option<RemoteNodeDesiredState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_state: Option<RemoteNodeObservedState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup_state: Option<RemoteCleanupState>,
}

/// Provider-returned addressing observed after a provisioning effect. The
/// node attempt compare-and-swap prevents a late provider response from being
/// attached to a replacement attempt.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteNodeObservation {
    pub node_attempt_id: String,
    pub ssh_host: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_user: Option<String>,
    pub ssh_dest: String,
    pub observed_state: RemoteNodeObservedState,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteOperationLeaseRequest {
    pub owner_id: String,
    pub lease_seconds: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteOperationLeaseRelease {
    pub owner_id: String,
    pub lease_epoch: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteOperationLease {
    pub owner_id: String,
    pub lease_epoch: i64,
    pub lease_until: i64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteConversationIntent {
    pub conversation_id: String,
    pub node_id: String,
    pub node_attempt_id: String,
    pub create_id: String,
    pub title: String,
    pub initial_prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_thread_id: Option<String>,
    pub placement: RemotePlacement,
    pub desired_state: RemoteConversationDesiredState,
    pub observed_state: RemoteConversationObservedState,
    pub cleanup_state: RemoteCleanupState,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteConversationUpdate {
    pub node_attempt_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_thread_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desired_state: Option<RemoteConversationDesiredState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_state: Option<RemoteConversationObservedState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup_state: Option<RemoteCleanupState>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RemoteConversationRecord {
    pub conversation_id: String,
    pub node_id: String,
    pub node_attempt_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker_thread_id: Option<String>,
    pub create_id: String,
    pub title: String,
    pub initial_prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_thread_id: Option<String>,
    pub placement: String,
    pub desired_state: String,
    pub observed_state: String,
    pub cleanup_state: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RemoteNodeRecord {
    pub node_id: String,
    pub node_attempt_id: String,
    pub provider: String,
    pub vm_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_host: Option<String>,
    pub ssh_port: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ssh_user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh_dest: Option<String>,
    pub identity_path: String,
    pub known_hosts_path: String,
    pub worker_socket_path: String,
    pub desired_state: String,
    pub observed_state: String,
    pub cleanup_state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_digest: Option<String>,
}

/// Closed durable intent vocabulary. It intentionally has no credential-specific
/// or raw Store-snapshot field. Prompt/content strings remain user-controlled.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum RemoteAction {
    ProvisionNode {
        cpu: u32,
        memory_gb: u32,
        disk_gb: u32,
        image: String,
    },
    CreateConversation {
        create_id: String,
        title: String,
        prompt: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_thread_id: Option<String>,
    },
    SendMessage {
        message_id: String,
        content: String,
    },
    RespondApproval {
        approval_id: String,
        decision: ox_types::Decision,
    },
    CancelConversation {
        cancel_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
    DeleteNode {
        delete_id: String,
        #[serde(default)]
        force: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        affected_references: Vec<String>,
    },
    ReconcileLedger {
        from_seq: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_hash: Option<String>,
    },
}

impl RemoteAction {
    fn kind(&self) -> &'static str {
        match self {
            Self::ProvisionNode { .. } => "provision_node",
            Self::CreateConversation { .. } => "create_conversation",
            Self::SendMessage { .. } => "send_message",
            Self::RespondApproval { .. } => "respond_approval",
            Self::CancelConversation { .. } => "cancel_conversation",
            Self::DeleteNode { .. } => "delete_node",
            Self::ReconcileLedger { .. } => "reconcile_ledger",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteOperationIntent {
    /// Stable caller-selected semantic key, such as a local command/request ID.
    pub semantic_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_attempt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    pub action: RemoteAction,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteOperationResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteOperationUpdate {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_attempt_id: Option<String>,
    pub expected_state: RemoteOperationState,
    pub state: RemoteOperationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_epoch: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<RemoteOperationResult>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RemoteOperationRecord {
    pub operation_id: String,
    pub operation_kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_attempt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conversation_id: Option<String>,
    pub request_hash: String,
    pub intent: RemoteOperationIntent,
    pub state: RemoteOperationState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<RemoteOperationResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_until: Option<i64>,
    pub lease_epoch: i64,
}

macro_rules! string_enum {
    ($name:ident { $($variant:ident => $value:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }

        impl $name {
            fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $value),+ }
            }
        }
    };
}

string_enum!(RemoteNodeDesiredState {
    Active => "active",
    Draining => "draining",
    Deleted => "deleted",
});
string_enum!(RemoteNodeObservedState {
    Pending => "pending",
    Provisioning => "provisioning",
    Ready => "ready",
    Unavailable => "unavailable",
    Absent => "absent",
    Errored => "errored",
});
string_enum!(RemoteConversationDesiredState {
    Active => "active",
    Completed => "completed",
    Canceled => "canceled",
    Deleted => "deleted",
});
string_enum!(RemoteConversationObservedState {
    Pending => "pending",
    Creating => "creating",
    Running => "running",
    WaitingForInput => "waiting_for_input",
    BlockedOnApproval => "blocked_on_approval",
    Completed => "completed",
    Canceled => "canceled",
    Errored => "errored",
    Unavailable => "unavailable",
    Lost => "lost",
});
string_enum!(RemoteCleanupState {
    None => "none",
    Pending => "pending",
    Complete => "complete",
    Failed => "failed",
});
string_enum!(RemotePlacement {
    FreshNode => "fresh_node",
    PreferExisting => "prefer_existing",
    RequireNode => "require_node",
});
string_enum!(RemoteOperationState {
    Pending => "pending",
    Running => "running",
    Applied => "applied",
    Failed => "failed",
    Superseded => "superseded",
});

fn transition_allowed(current: &str, next: &str, edges: &[(&str, &[&str])]) -> bool {
    current == next
        || edges
            .iter()
            .find(|(state, _)| *state == current)
            .is_some_and(|(_, allowed)| allowed.contains(&next))
}

fn cleanup_transition_allowed(current: &str, next: &str) -> bool {
    transition_allowed(
        current,
        next,
        &[
            ("none", &["pending"]),
            ("pending", &["complete", "failed"]),
            ("failed", &["pending", "complete"]),
            ("complete", &[]),
        ],
    )
}

fn node_desired_transition_allowed(current: &str, next: &str) -> bool {
    transition_allowed(
        current,
        next,
        &[
            ("active", &["draining", "deleted"]),
            ("draining", &["deleted"]),
            ("deleted", &[]),
        ],
    )
}

fn node_observed_transition_allowed(current: &str, next: &str) -> bool {
    transition_allowed(
        current,
        next,
        &[
            (
                "pending",
                &["provisioning", "ready", "unavailable", "absent", "errored"],
            ),
            (
                "provisioning",
                &["ready", "unavailable", "absent", "errored"],
            ),
            ("ready", &["unavailable", "absent", "errored"]),
            ("unavailable", &["ready", "absent", "errored"]),
            ("errored", &["provisioning", "unavailable", "absent"]),
            ("absent", &[]),
        ],
    )
}

fn conversation_desired_transition_allowed(current: &str, next: &str) -> bool {
    transition_allowed(
        current,
        next,
        &[
            ("active", &["completed", "canceled", "deleted"]),
            ("completed", &["deleted"]),
            ("canceled", &["deleted"]),
            ("deleted", &[]),
        ],
    )
}

fn conversation_observed_transition_allowed(current: &str, next: &str) -> bool {
    transition_allowed(
        current,
        next,
        &[
            ("pending", &["creating", "unavailable", "lost", "errored"]),
            (
                "creating",
                &[
                    "running",
                    "waiting_for_input",
                    "blocked_on_approval",
                    "completed",
                    "canceled",
                    "unavailable",
                    "lost",
                    "errored",
                ],
            ),
            (
                "running",
                &[
                    "waiting_for_input",
                    "blocked_on_approval",
                    "completed",
                    "canceled",
                    "unavailable",
                    "lost",
                    "errored",
                ],
            ),
            (
                "waiting_for_input",
                &[
                    "running",
                    "completed",
                    "canceled",
                    "unavailable",
                    "lost",
                    "errored",
                ],
            ),
            (
                "blocked_on_approval",
                &["running", "canceled", "unavailable", "lost", "errored"],
            ),
            (
                "unavailable",
                &[
                    "running",
                    "waiting_for_input",
                    "blocked_on_approval",
                    "completed",
                    "canceled",
                    "lost",
                    "errored",
                ],
            ),
            ("completed", &[]),
            ("canceled", &[]),
            ("errored", &[]),
            ("lost", &[]),
        ],
    )
}

fn operation_transition_allowed(current: RemoteOperationState, next: RemoteOperationState) -> bool {
    current == next
        || matches!(
            (current, next),
            (
                RemoteOperationState::Pending,
                RemoteOperationState::Running
                    | RemoteOperationState::Applied
                    | RemoteOperationState::Failed
                    | RemoteOperationState::Superseded
            ) | (
                RemoteOperationState::Running,
                RemoteOperationState::Applied
                    | RemoteOperationState::Failed
                    | RemoteOperationState::Superseded
            )
        )
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CachedLedgerEntry {
    pub seq: i64,
    pub hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    pub msg: serde_json::Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CachedLedgerBatch {
    pub node_attempt_id: String,
    pub expected_last_seq: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_last_hash: Option<String>,
    pub entries: Vec<CachedLedgerEntry>,
}

fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

fn err(operation: &'static str, message: impl std::fmt::Display) -> StoreError {
    StoreError::store("InboxStore", operation, message.to_string())
}

fn validate_id(operation: &'static str, id: &str) -> Result<(), StoreError> {
    if id.is_empty() || id.len() > MAX_ID_BYTES || id.contains('\0') {
        return Err(err(
            operation,
            "id must be 1..=512 bytes and contain no NUL",
        ));
    }
    Ok(())
}

fn validate_nonempty(operation: &'static str, field: &str, value: &str) -> Result<(), StoreError> {
    if value.is_empty() || value.len() > 65_536 || value.contains('\0') {
        return Err(err(
            operation,
            format!("{field} must be 1..=65536 bytes and contain no NUL"),
        ));
    }
    Ok(())
}

fn validate_handle(operation: &'static str, field: &str, value: &str) -> Result<(), StoreError> {
    validate_nonempty(operation, field, value)?;
    if !FsPath::new(value).is_absolute() {
        return Err(err(
            operation,
            format!("{field} must be an absolute path handle"),
        ));
    }
    if value.len() > 4_096 {
        return Err(err(operation, format!("{field} exceeds 4096 bytes")));
    }
    Ok(())
}

fn validate_node(node: &RemoteNodeIntent) -> Result<(), StoreError> {
    validate_id("remote_node", &node.node_id)?;
    validate_id("remote_node", &node.node_attempt_id)?;
    for (field, value) in [
        ("provider", node.provider.as_str()),
        ("vm_name", node.vm_name.as_str()),
        ("desired_state", node.desired_state.as_str()),
        ("observed_state", node.observed_state.as_str()),
        ("cleanup_state", node.cleanup_state.as_str()),
    ] {
        validate_nonempty("remote_node", field, value)?;
        if value.len() > 4_096 {
            return Err(err("remote_node", format!("{field} exceeds 4096 bytes")));
        }
    }
    if let Some(host) = &node.ssh_host {
        validate_nonempty("remote_node", "ssh_host", host)?;
    }
    if let Some(dest) = &node.ssh_dest {
        validate_nonempty("remote_node", "ssh_dest", dest)?;
    }
    if let Some(user) = &node.ssh_user {
        validate_nonempty("remote_node", "ssh_user", user)?;
    }
    if !(1..=65_535).contains(&node.ssh_port) {
        return Err(err("remote_node", "ssh_port must be in 1..=65535"));
    }
    validate_handle("remote_node", "identity_path", &node.identity_path)?;
    validate_handle("remote_node", "known_hosts_path", &node.known_hosts_path)?;
    validate_handle(
        "remote_node",
        "worker_socket_path",
        &node.worker_socket_path,
    )
}

fn decode_id(encoded: &str) -> Result<String, StoreError> {
    let id = id_codec::decode_id("InboxStore", "remote_path", encoded)?;
    validate_id("remote_path", &id)?;
    Ok(id)
}

pub fn remote_item_path(kind: &str, id: &str) -> Result<Path, StoreError> {
    validate_id("remote_path", id)?;
    Path::parse(&format!("remote/{kind}/{}", id_codec::encode_id(id))).map_err(StoreError::from)
}

fn item_path(kind: &str, id: &str) -> Result<Path, StoreError> {
    remote_item_path(kind, id)
}

fn serialize<T: Serialize>(operation: &'static str, value: &T) -> Result<Vec<u8>, StoreError> {
    serde_json::to_vec(value).map_err(|error| err(operation, error))
}

fn operation_identity(intent: &RemoteOperationIntent) -> Result<(String, String), StoreError> {
    validate_id("remote_operation", &intent.semantic_key)?;
    if let Some(id) = &intent.node_id {
        validate_id("remote_operation", id)?;
    }
    if let Some(id) = &intent.node_attempt_id {
        validate_id("remote_operation", id)?;
    }
    if let Some(id) = &intent.conversation_id {
        validate_id("remote_operation", id)?;
    }
    validate_operation_shape(intent)?;
    let bytes = serialize("remote_operation", intent)?;
    if bytes.len() > MAX_OPERATION_TEXT_BYTES {
        return Err(err("remote_operation", "operation intent exceeds 8 MiB"));
    }
    let request_hash = format!("{:x}", Sha256::digest(&bytes));
    let mut identity = Sha256::new();
    identity.update(b"ox-remote-operation-v1\0");
    identity.update(intent.action.kind().as_bytes());
    identity.update([0]);
    identity.update(intent.semantic_key.as_bytes());
    identity.update([0]);
    identity.update(intent.node_id.as_deref().unwrap_or("").as_bytes());
    identity.update([0]);
    identity.update(intent.node_attempt_id.as_deref().unwrap_or("").as_bytes());
    identity.update([0]);
    identity.update(intent.conversation_id.as_deref().unwrap_or("").as_bytes());
    Ok((format!("rop_{:x}", identity.finalize()), request_hash))
}

pub fn remote_operation_item_path(intent: &RemoteOperationIntent) -> Result<Path, StoreError> {
    let (operation_id, _) = operation_identity(intent)?;
    remote_item_path("operations", &operation_id)
}

fn validate_operation_shape(intent: &RemoteOperationIntent) -> Result<(), StoreError> {
    let has_node = intent.node_id.is_some() && intent.node_attempt_id.is_some();
    let has_conversation = intent.conversation_id.is_some();
    let target_ok = match &intent.action {
        RemoteAction::ProvisionNode {
            cpu,
            memory_gb,
            disk_gb,
            image,
        } => {
            if !(1..=64).contains(cpu)
                || !(1..=512).contains(memory_gb)
                || !(1..=4096).contains(disk_gb)
            {
                return Err(err(
                    "remote_operation",
                    "node resources exceed supported bounds",
                ));
            }
            validate_nonempty("remote_operation", "image", image)?;
            if image.len() > 4_096 {
                return Err(err(
                    "remote_operation",
                    "image reference exceeds 4096 bytes",
                ));
            }
            has_node && !has_conversation
        }
        RemoteAction::DeleteNode { delete_id, .. } => {
            validate_id("remote_operation", delete_id)?;
            has_node && !has_conversation
        }
        RemoteAction::CreateConversation {
            create_id,
            title,
            prompt,
            parent_thread_id,
        } => {
            validate_id("remote_operation", create_id)?;
            if let Some(parent) = parent_thread_id {
                validate_id("remote_operation", parent)?;
            }
            validate_nonempty("remote_operation", "title", title)?;
            if title.len() > 65_536 || prompt.len() > MAX_OPERATION_TEXT_BYTES {
                return Err(err(
                    "remote_operation",
                    "conversation intent text exceeds limits",
                ));
            }
            has_node && has_conversation
        }
        RemoteAction::SendMessage {
            message_id,
            content,
        } => {
            validate_id("remote_operation", message_id)?;
            if content.len() > MAX_OPERATION_TEXT_BYTES {
                return Err(err("remote_operation", "message content exceeds 8 MiB"));
            }
            has_node && has_conversation
        }
        RemoteAction::RespondApproval { approval_id, .. } => {
            validate_id("remote_operation", approval_id)?;
            has_node && has_conversation
        }
        RemoteAction::CancelConversation { cancel_id, reason } => {
            validate_id("remote_operation", cancel_id)?;
            if reason.as_ref().is_some_and(|reason| reason.len() > 65_536) {
                return Err(err("remote_operation", "cancel reason exceeds 65536 bytes"));
            }
            has_node && has_conversation
        }
        RemoteAction::ReconcileLedger { from_seq, .. } => {
            if *from_seq < 0 {
                return Err(err(
                    "remote_operation",
                    "ledger cursor must be non-negative",
                ));
            }
            has_node && has_conversation
        }
    };
    if !target_ok {
        return Err(err(
            "remote_operation",
            "operation action has an invalid node/conversation target shape",
        ));
    }
    Ok(())
}

fn node_value(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let node = RemoteNodeRecord {
        node_id: row.get(0)?,
        node_attempt_id: row.get(1)?,
        provider: row.get(2)?,
        vm_name: row.get(3)?,
        ssh_host: row.get(4)?,
        ssh_port: row.get(5)?,
        ssh_user: row.get(6)?,
        ssh_dest: row.get(7)?,
        identity_path: row.get(8)?,
        known_hosts_path: row.get(9)?,
        worker_socket_path: row.get(10)?,
        desired_state: row.get(11)?,
        observed_state: row.get(12)?,
        cleanup_state: row.get(13)?,
        image_digest: row.get(14)?,
    };
    Ok(structfs_serde_store::to_value(&node).expect("serializable remote node"))
}

const NODE_SELECT: &str = "SELECT node_id, node_attempt_id, provider, vm_name, ssh_host, ssh_port, ssh_user, ssh_dest, identity_path, known_hosts_path, worker_socket_path, desired_state, observed_state, cleanup_state, image_digest FROM remote_nodes";

fn conversation_value(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let record = RemoteConversationRecord {
        conversation_id: row.get(0)?,
        node_id: row.get(1)?,
        node_attempt_id: row.get(2)?,
        worker_thread_id: row.get(3)?,
        create_id: row.get(4)?,
        title: row.get(5)?,
        initial_prompt: row.get(6)?,
        parent_thread_id: row.get(7)?,
        placement: row.get(8)?,
        desired_state: row.get(9)?,
        observed_state: row.get(10)?,
        cleanup_state: row.get(11)?,
    };
    Ok(structfs_serde_store::to_value(&record).expect("serializable remote conversation"))
}

const CONVERSATION_SELECT: &str = "SELECT conversation_id, node_id, node_attempt_id, worker_thread_id, create_id, title, initial_prompt, parent_thread_id, placement, desired_state, observed_state, cleanup_state FROM remote_conversations";

impl InboxStore {
    fn put_remote_node(&self, node: &RemoteNodeIntent) -> Result<Path, StoreError> {
        validate_node(node)?;
        let request_hash = format!("{:x}", Sha256::digest(serialize("remote_node", node)?));
        let conn = self.db.lock().map_err(|error| err("remote_node", error))?;
        let inserted = conn.execute(
            "INSERT OR IGNORE INTO remote_nodes (node_id, node_attempt_id, provider, vm_name, ssh_host, ssh_port, ssh_user, ssh_dest, identity_path, known_hosts_path, worker_socket_path, desired_state, observed_state, cleanup_state, image_digest, request_hash, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?17)",
            params![node.node_id, node.node_attempt_id, node.provider, node.vm_name, node.ssh_host, node.ssh_port, node.ssh_user, node.ssh_dest, node.identity_path, node.known_hosts_path, node.worker_socket_path, node.desired_state.as_str(), node.observed_state.as_str(), node.cleanup_state.as_str(), node.image_digest, request_hash, now_epoch()],
        ).map_err(|error| err("remote_node", error))?;
        if inserted == 0 {
            let existing: Option<String> = conn
                .query_row(
                    "SELECT request_hash FROM remote_nodes WHERE node_id = ?1",
                    [&node.node_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| err("remote_node", error))?;
            if existing.as_deref() != Some(request_hash.as_str()) {
                return Err(err(
                    "remote_node",
                    "conflict: node id or VM name already has different durable intent",
                ));
            }
        }
        item_path("nodes", &node.node_id)
    }

    fn replace_remote_node_attempt(
        &self,
        path_node_id: &str,
        replacement: &RemoteNodeAttemptReplacement,
    ) -> Result<Path, StoreError> {
        validate_node(&replacement.node)?;
        if replacement.node.node_id != path_node_id {
            return Err(err(
                "remote_node_attempt",
                "path node id does not match record",
            ));
        }
        let request_hash = format!(
            "{:x}",
            Sha256::digest(serialize("remote_node_attempt", &replacement.node)?)
        );
        let mut conn = self
            .db
            .lock()
            .map_err(|error| err("remote_node_attempt", error))?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| err("remote_node_attempt", error))?;
        let changed = tx.execute(
            "UPDATE remote_nodes SET node_attempt_id=?1, provider=?2, vm_name=?3, ssh_host=?4, ssh_port=?5, ssh_user=?6, ssh_dest=?7, identity_path=?8, known_hosts_path=?9, worker_socket_path=?10, desired_state=?11, observed_state=?12, cleanup_state=?13, image_digest=?14, request_hash=?15, updated_at=?16 WHERE node_id=?17 AND node_attempt_id=?18",
            params![replacement.node.node_attempt_id, replacement.node.provider, replacement.node.vm_name, replacement.node.ssh_host, replacement.node.ssh_port, replacement.node.ssh_user, replacement.node.ssh_dest, replacement.node.identity_path, replacement.node.known_hosts_path, replacement.node.worker_socket_path, replacement.node.desired_state.as_str(), replacement.node.observed_state.as_str(), replacement.node.cleanup_state.as_str(), replacement.node.image_digest, request_hash, now_epoch(), path_node_id, replacement.expected_attempt_id],
        ).map_err(|error| err("remote_node_attempt", error))?;
        if changed != 1 {
            let current: Option<(String, String)> = tx
                .query_row(
                    "SELECT node_attempt_id, request_hash FROM remote_nodes WHERE node_id=?1",
                    [path_node_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(|error| err("remote_node_attempt", error))?;
            if current.as_ref() != Some(&(replacement.node.node_attempt_id.clone(), request_hash)) {
                return Err(err("remote_node_attempt", "stale node attempt"));
            }
        } else {
            tx.execute(
                "UPDATE remote_conversations SET observed_state='lost', updated_at=?1 WHERE node_id=?2 AND node_attempt_id=?3 AND observed_state NOT IN ('completed', 'canceled', 'errored', 'lost')",
                params![now_epoch(), path_node_id, replacement.expected_attempt_id],
            ).map_err(|error| err("remote_node_attempt", error))?;
            tx.execute(
                "UPDATE remote_operations SET state='superseded', lease_owner=NULL, lease_until=NULL, updated_at=?1 WHERE node_id=?2 AND node_attempt_id=?3 AND state IN ('pending', 'running')",
                params![now_epoch(), path_node_id, replacement.expected_attempt_id],
            ).map_err(|error| err("remote_node_attempt", error))?;
        }
        tx.commit()
            .map_err(|error| err("remote_node_attempt", error))?;
        item_path("nodes", path_node_id)
    }

    fn update_remote_node(
        &self,
        node_id: &str,
        update: &RemoteNodeUpdate,
    ) -> Result<Path, StoreError> {
        validate_id("remote_node_update", node_id)?;
        validate_id("remote_node_update", &update.node_attempt_id)?;
        let mut conn = self
            .db
            .lock()
            .map_err(|error| err("remote_node_update", error))?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| err("remote_node_update", error))?;
        let current: Option<(String, String, String)> = tx.query_row(
            "SELECT desired_state, observed_state, cleanup_state FROM remote_nodes WHERE node_id=?1 AND node_attempt_id=?2",
            params![node_id, update.node_attempt_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
        ).optional().map_err(|error| err("remote_node_update", error))?;
        let Some((desired, observed, cleanup)) = current else {
            return Err(err("remote_node_update", "stale node attempt"));
        };
        if update
            .desired_state
            .is_some_and(|next| !node_desired_transition_allowed(&desired, next.as_str()))
            || update
                .observed_state
                .is_some_and(|next| !node_observed_transition_allowed(&observed, next.as_str()))
            || update
                .cleanup_state
                .is_some_and(|next| !cleanup_transition_allowed(&cleanup, next.as_str()))
        {
            return Err(err("remote_node_update", "illegal state transition"));
        }
        let changed = tx.execute(
            "UPDATE remote_nodes SET desired_state=COALESCE(?1, desired_state), observed_state=COALESCE(?2, observed_state), cleanup_state=COALESCE(?3, cleanup_state), updated_at=?4 WHERE node_id=?5 AND node_attempt_id=?6 AND desired_state=?7 AND observed_state=?8 AND cleanup_state=?9",
            params![update.desired_state.map(|v| v.as_str()), update.observed_state.map(|v| v.as_str()), update.cleanup_state.map(|v| v.as_str()), now_epoch(), node_id, update.node_attempt_id, desired, observed, cleanup],
        ).map_err(|error| err("remote_node_update", error))?;
        if changed != 1 {
            return Err(err("remote_node_update", "stale node attempt"));
        }
        tx.commit()
            .map_err(|error| err("remote_node_update", error))?;
        item_path("nodes", node_id)
    }

    fn observe_remote_node(
        &self,
        node_id: &str,
        observation: &RemoteNodeObservation,
    ) -> Result<Path, StoreError> {
        validate_id("remote_node_observation", node_id)?;
        validate_id("remote_node_observation", &observation.node_attempt_id)?;
        validate_nonempty("remote_node_observation", "ssh_host", &observation.ssh_host)?;
        validate_nonempty("remote_node_observation", "ssh_dest", &observation.ssh_dest)?;
        if let Some(user) = &observation.ssh_user {
            validate_nonempty("remote_node_observation", "ssh_user", user)?;
        }
        let mut conn = self
            .db
            .lock()
            .map_err(|error| err("remote_node_observation", error))?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| err("remote_node_observation", error))?;
        let current: Option<String> = tx
            .query_row(
                "SELECT observed_state FROM remote_nodes WHERE node_id=?1 AND node_attempt_id=?2",
                params![node_id, observation.node_attempt_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| err("remote_node_observation", error))?;
        let Some(current) = current else {
            return Err(err("remote_node_observation", "stale node attempt"));
        };
        if !node_observed_transition_allowed(&current, observation.observed_state.as_str()) {
            return Err(err("remote_node_observation", "illegal state transition"));
        }
        let changed = tx
            .execute(
                "UPDATE remote_nodes SET ssh_host=?1, ssh_user=?2, ssh_dest=?3, observed_state=?4, updated_at=?5 WHERE node_id=?6 AND node_attempt_id=?7 AND observed_state=?8",
                params![observation.ssh_host, observation.ssh_user, observation.ssh_dest, observation.observed_state.as_str(), now_epoch(), node_id, observation.node_attempt_id, current],
            )
            .map_err(|error| err("remote_node_observation", error))?;
        if changed != 1 {
            return Err(err("remote_node_observation", "stale node attempt"));
        }
        tx.commit()
            .map_err(|error| err("remote_node_observation", error))?;
        item_path("nodes", node_id)
    }

    fn claim_remote_operation(
        &self,
        operation_id: &str,
        request: &RemoteOperationLeaseRequest,
    ) -> Result<Path, StoreError> {
        validate_id("remote_operation_lease", operation_id)?;
        validate_id("remote_operation_lease", &request.owner_id)?;
        if !(1..=60).contains(&request.lease_seconds) {
            return Err(err(
                "remote_operation_lease",
                "lease_seconds must be in 1..=60",
            ));
        }
        let now = now_epoch();
        let until = now.saturating_add(i64::from(request.lease_seconds));
        let mut conn = self
            .db
            .lock()
            .map_err(|error| err("remote_operation_lease", error))?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| err("remote_operation_lease", error))?;
        let current: Option<(String, Option<String>, Option<i64>, i64)> = tx
            .query_row(
                "SELECT state, lease_owner, lease_until, lease_epoch FROM remote_operations WHERE operation_id=?1",
                [operation_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(|error| err("remote_operation_lease", error))?;
        let Some((state, owner, expiry, epoch)) = current else {
            return Err(err("remote_operation_lease", "unknown operation"));
        };
        if !matches!(state.as_str(), "pending" | "running") {
            return Err(err("remote_operation_lease", "operation is terminal"));
        }
        let renewal = owner.as_deref() == Some(request.owner_id.as_str())
            && expiry.is_some_and(|expiry| expiry > now);
        if !renewal && expiry.is_some_and(|expiry| expiry > now) {
            return Err(err("remote_operation_lease", "operation lease is held"));
        }
        let next_epoch = if renewal {
            epoch
        } else {
            epoch.saturating_add(1)
        };
        let changed = tx.execute(
            "UPDATE remote_operations SET state='running', lease_owner=?1, lease_until=?2, lease_epoch=?3, updated_at=?4 WHERE operation_id=?5 AND state=?6 AND lease_epoch=?7",
            params![request.owner_id, until, next_epoch, now, operation_id, state, epoch],
        )
        .map_err(|error| err("remote_operation_lease", error))?;
        if changed != 1 {
            return Err(err("remote_operation_lease", "operation lease raced"));
        }
        tx.commit()
            .map_err(|error| err("remote_operation_lease", error))?;
        Path::parse(&format!(
            "remote/operations/{}/lease/{next_epoch}",
            id_codec::encode_id(operation_id)
        ))
        .map_err(StoreError::from)
    }

    fn release_remote_operation(
        &self,
        operation_id: &str,
        request: &RemoteOperationLeaseRelease,
    ) -> Result<Path, StoreError> {
        validate_id("remote_operation_lease", operation_id)?;
        validate_id("remote_operation_lease", &request.owner_id)?;
        let conn = self
            .db
            .lock()
            .map_err(|error| err("remote_operation_lease", error))?;
        let changed = conn
            .execute(
                "UPDATE remote_operations SET state='pending', lease_owner=NULL, lease_until=NULL, updated_at=?1 WHERE operation_id=?2 AND state='running' AND lease_owner=?3 AND lease_epoch=?4 AND lease_until>?1",
                params![now_epoch(), operation_id, request.owner_id, request.lease_epoch],
            )
            .map_err(|error| err("remote_operation_lease", error))?;
        if changed != 1 {
            return Err(err("remote_operation_lease", "stale operation lease"));
        }
        item_path("operations", operation_id)
    }

    fn put_remote_conversation(
        &self,
        value: &RemoteConversationIntent,
    ) -> Result<Path, StoreError> {
        for id in [
            &value.conversation_id,
            &value.node_id,
            &value.node_attempt_id,
            &value.create_id,
        ] {
            validate_id("remote_conversation", id)?;
        }
        if let Some(parent) = &value.parent_thread_id {
            validate_id("remote_conversation", parent)?;
        }
        validate_nonempty("remote_conversation", "title", &value.title)?;
        if value.initial_prompt.len() > MAX_OPERATION_TEXT_BYTES {
            return Err(err("remote_conversation", "initial prompt exceeds 8 MiB"));
        }
        let request_hash = format!(
            "{:x}",
            Sha256::digest(serialize("remote_conversation", value)?)
        );
        let mut conn = self
            .db
            .lock()
            .map_err(|error| err("remote_conversation", error))?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| err("remote_conversation", error))?;
        if let Some(existing) = tx
            .query_row(
                "SELECT request_hash FROM remote_conversations WHERE conversation_id=?1",
                [&value.conversation_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| err("remote_conversation", error))?
        {
            if existing != request_hash {
                return Err(err(
                    "remote_conversation",
                    "conflict: conversation id already has different durable intent",
                ));
            }
            tx.commit()
                .map_err(|error| err("remote_conversation", error))?;
            return item_path("conversations", &value.conversation_id);
        }
        let current_attempt: Option<(String, String)> = tx
            .query_row(
                "SELECT node_attempt_id, desired_state FROM remote_nodes WHERE node_id=?1",
                [&value.node_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|error| err("remote_conversation", error))?;
        if current_attempt.as_ref().map(|value| value.0.as_str())
            != Some(value.node_attempt_id.as_str())
        {
            return Err(err("remote_conversation", "stale or unknown node attempt"));
        }
        if current_attempt.as_ref().map(|value| value.1.as_str()) != Some("active") {
            return Err(err(
                "remote_conversation",
                "node is draining and rejects new conversations",
            ));
        }
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO remote_conversations (conversation_id, node_id, node_attempt_id, create_id, title, initial_prompt, parent_thread_id, placement, desired_state, observed_state, cleanup_state, request_hash, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?13)",
            params![value.conversation_id, value.node_id, value.node_attempt_id, value.create_id, value.title, value.initial_prompt, value.parent_thread_id, value.placement.as_str(), value.desired_state.as_str(), value.observed_state.as_str(), value.cleanup_state.as_str(), request_hash, now_epoch()],
        ).map_err(|error| err("remote_conversation", error))?;
        if inserted == 0 {
            let existing: Option<String> = tx
                .query_row(
                    "SELECT request_hash FROM remote_conversations WHERE conversation_id=?1",
                    [&value.conversation_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| err("remote_conversation", error))?;
            if existing.as_deref() != Some(request_hash.as_str()) {
                return Err(err(
                    "remote_conversation",
                    "conflict: conversation or create id already has different durable intent",
                ));
            }
        } else {
            tx.execute(
                "INSERT INTO remote_ledger_cursors (conversation_id, last_seq, last_hash) VALUES (?1, -1, NULL)",
                [&value.conversation_id],
            ).map_err(|error| err("remote_conversation", error))?;
        }
        tx.commit()
            .map_err(|error| err("remote_conversation", error))?;
        item_path("conversations", &value.conversation_id)
    }

    fn update_remote_conversation(
        &self,
        id: &str,
        update: &RemoteConversationUpdate,
    ) -> Result<Path, StoreError> {
        validate_id("remote_conversation_update", id)?;
        validate_id("remote_conversation_update", &update.node_attempt_id)?;
        let mut conn = self
            .db
            .lock()
            .map_err(|error| err("remote_conversation_update", error))?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| err("remote_conversation_update", error))?;
        let current: Option<(Option<String>,String,String,String)> = tx.query_row(
            "SELECT worker_thread_id, desired_state, observed_state, cleanup_state FROM remote_conversations WHERE conversation_id=?1 AND node_attempt_id=?2 AND EXISTS(SELECT 1 FROM remote_nodes n WHERE n.node_id=remote_conversations.node_id AND n.node_attempt_id=?2)",
            params![id,update.node_attempt_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?)),
        ).optional().map_err(|error| err("remote_conversation_update", error))?;
        let Some((worker, desired, observed, cleanup)) = current else {
            return Err(err("remote_conversation_update", "stale node attempt"));
        };
        if update
            .worker_thread_id
            .as_ref()
            .is_some_and(|next| worker.as_ref().is_some_and(|current| current != next))
        {
            return Err(err(
                "remote_conversation_update",
                "worker thread id is already bound",
            ));
        }
        if update
            .desired_state
            .is_some_and(|next| !conversation_desired_transition_allowed(&desired, next.as_str()))
            || update.observed_state.is_some_and(|next| {
                !conversation_observed_transition_allowed(&observed, next.as_str())
            })
            || update
                .cleanup_state
                .is_some_and(|next| !cleanup_transition_allowed(&cleanup, next.as_str()))
        {
            return Err(err(
                "remote_conversation_update",
                "illegal state transition",
            ));
        }
        let changed = tx.execute(
            "UPDATE remote_conversations SET worker_thread_id=COALESCE(worker_thread_id, ?1), desired_state=COALESCE(?2, desired_state), observed_state=COALESCE(?3, observed_state), cleanup_state=COALESCE(?4, cleanup_state), updated_at=?5 WHERE conversation_id=?6 AND node_attempt_id=?7 AND desired_state=?8 AND observed_state=?9 AND cleanup_state=?10",
            params![update.worker_thread_id, update.desired_state.map(|v| v.as_str()), update.observed_state.map(|v| v.as_str()), update.cleanup_state.map(|v| v.as_str()), now_epoch(), id, update.node_attempt_id,desired,observed,cleanup],
        ).map_err(|error| err("remote_conversation_update", error))?;
        if changed != 1 {
            return Err(err("remote_conversation_update", "stale node attempt"));
        }
        tx.commit()
            .map_err(|error| err("remote_conversation_update", error))?;
        item_path("conversations", id)
    }

    fn accept_remote_operation(&self, intent: &RemoteOperationIntent) -> Result<Path, StoreError> {
        let (operation_id, request_hash) = operation_identity(intent)?;
        let bytes = serialize("remote_operation", intent)?;
        if intent.node_id.is_some() != intent.node_attempt_id.is_some() {
            return Err(err(
                "remote_operation",
                "node_id and node_attempt_id must be supplied together",
            ));
        }
        let mut conn = self
            .db
            .lock()
            .map_err(|error| err("remote_operation", error))?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| err("remote_operation", error))?;
        if let Some(existing) = tx
            .query_row(
                "SELECT request_hash FROM remote_operations WHERE operation_id=?1",
                [&operation_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|error| err("remote_operation", error))?
        {
            if existing != request_hash {
                return Err(err(
                    "remote_operation",
                    "conflict: semantic operation id has different intent",
                ));
            }
            tx.commit()
                .map_err(|error| err("remote_operation", error))?;
            return item_path("operations", &operation_id);
        }
        if let (Some(node_id), Some(attempt)) = (&intent.node_id, &intent.node_attempt_id) {
            let matches: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM remote_nodes WHERE node_id=?1 AND node_attempt_id=?2)",
                params![node_id, attempt], |row| row.get(0),
            ).map_err(|error| err("remote_operation", error))?;
            if !matches {
                return Err(err("remote_operation", "stale node attempt"));
            }
        }
        if let Some(conversation_id) = &intent.conversation_id {
            let create_id: Option<String> = tx
                .query_row(
                    "SELECT create_id FROM remote_conversations WHERE conversation_id=?1 AND node_id IS ?2 AND node_attempt_id IS ?3",
                    params![conversation_id, intent.node_id, intent.node_attempt_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| err("remote_operation", error))?;
            let Some(create_id) = create_id else {
                return Err(err(
                    "remote_operation",
                    "conversation target does not match current node attempt",
                ));
            };
            if let RemoteAction::CreateConversation {
                create_id: action_create_id,
                ..
            } = &intent.action
            {
                if action_create_id != &create_id {
                    return Err(err(
                        "remote_operation",
                        "create action id does not match durable conversation intent",
                    ));
                }
            }
        }
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO remote_operations (operation_id, operation_kind, node_id, node_attempt_id, conversation_id, request_hash, intent_json, state, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'pending', ?8, ?8)",
            params![operation_id, intent.action.kind(), intent.node_id, intent.node_attempt_id, intent.conversation_id, request_hash, bytes, now_epoch()],
        ).map_err(|error| err("remote_operation", error))?;
        if inserted == 0 {
            let existing: Option<String> = tx
                .query_row(
                    "SELECT request_hash FROM remote_operations WHERE operation_id=?1",
                    [&operation_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| err("remote_operation", error))?;
            if existing.as_deref() != Some(request_hash.as_str()) {
                return Err(err(
                    "remote_operation",
                    "conflict: semantic operation id has different intent",
                ));
            }
        }
        tx.commit()
            .map_err(|error| err("remote_operation", error))?;
        item_path("operations", &operation_id)
    }

    fn update_remote_operation(
        &self,
        id: &str,
        update: &RemoteOperationUpdate,
    ) -> Result<Path, StoreError> {
        validate_id("remote_operation_update", id)?;
        if !operation_transition_allowed(update.expected_state, update.state) {
            return Err(err("remote_operation_update", "illegal state transition"));
        }
        let result = update
            .result
            .as_ref()
            .map(|value| serialize("remote_operation_update", value))
            .transpose()?;
        if result
            .as_ref()
            .is_some_and(|bytes| bytes.len() > 1024 * 1024)
        {
            return Err(err(
                "remote_operation_update",
                "operation result exceeds 1 MiB",
            ));
        }
        let conn = self
            .db
            .lock()
            .map_err(|error| err("remote_operation_update", error))?;
        if update.lease_owner.is_some() != update.lease_epoch.is_some() {
            return Err(err(
                "remote_operation_update",
                "lease owner and epoch must be supplied together",
            ));
        }
        if update.expected_state == RemoteOperationState::Running && update.lease_owner.is_none() {
            return Err(err(
                "remote_operation_update",
                "running operation commit requires a fenced lease",
            ));
        }
        if let Some(owner) = &update.lease_owner {
            validate_id("remote_operation_update", owner)?;
        }
        let now = now_epoch();
        let changed = match (&update.node_attempt_id, &update.lease_owner, update.lease_epoch) {
            (Some(attempt), Some(owner), Some(epoch)) => conn.execute(
                "UPDATE remote_operations SET state=?1, result_json=?2, lease_owner=NULL, lease_until=NULL, updated_at=?3 WHERE operation_id=?4 AND state=?5 AND node_attempt_id=?6 AND lease_owner=?7 AND lease_epoch=?8 AND lease_until>?3 AND EXISTS(SELECT 1 FROM remote_nodes n WHERE n.node_id=remote_operations.node_id AND n.node_attempt_id=?6)",
                params![update.state.as_str(), result, now, id, update.expected_state.as_str(), attempt, owner, epoch],
            ),
            (None, Some(owner), Some(epoch)) => conn.execute(
                "UPDATE remote_operations SET state=?1, result_json=?2, lease_owner=NULL, lease_until=NULL, updated_at=?3 WHERE operation_id=?4 AND state=?5 AND node_attempt_id IS NULL AND lease_owner=?6 AND lease_epoch=?7 AND lease_until>?3",
                params![update.state.as_str(), result, now, id, update.expected_state.as_str(), owner, epoch],
            ),
            (Some(attempt), None, None) => conn.execute(
                "UPDATE remote_operations SET state=?1, result_json=?2, updated_at=?3 WHERE operation_id=?4 AND state=?5 AND node_attempt_id=?6 AND EXISTS(SELECT 1 FROM remote_nodes n WHERE n.node_id=remote_operations.node_id AND n.node_attempt_id=?6)",
                params![update.state.as_str(), result, now, id, update.expected_state.as_str(), attempt],
            ),
            (None, None, None) => conn.execute(
                "UPDATE remote_operations SET state=?1, result_json=?2, updated_at=?3 WHERE operation_id=?4 AND state=?5 AND node_attempt_id IS NULL",
                params![update.state.as_str(), result, now, id, update.expected_state.as_str()],
            ),
            _ => unreachable!(),
        }.map_err(|error| err("remote_operation_update", error))?;
        if changed != 1 {
            return Err(err(
                "remote_operation_update",
                "stale attempt or operation state",
            ));
        }
        item_path("operations", id)
    }

    fn commit_cached_ledger(
        &self,
        id: &str,
        batch: &CachedLedgerBatch,
    ) -> Result<Path, StoreError> {
        validate_id("remote_ledger", id)?;
        validate_id("remote_ledger", &batch.node_attempt_id)?;
        if batch.entries.len() > MAX_LEDGER_BATCH {
            return Err(err("remote_ledger", "ledger batch exceeds 1024 entries"));
        }
        let mut conn = self
            .db
            .lock()
            .map_err(|error| err("remote_ledger", error))?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| err("remote_ledger", error))?;
        let attempt_matches: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM remote_conversations c JOIN remote_nodes n ON n.node_id=c.node_id WHERE c.conversation_id=?1 AND c.node_attempt_id=?2 AND n.node_attempt_id=?2)",
            params![id, batch.node_attempt_id], |row| row.get(0),
        ).map_err(|error| err("remote_ledger", error))?;
        if !attempt_matches {
            return Err(err("remote_ledger", "stale node attempt"));
        }
        let cursor: (i64, Option<String>) = tx
            .query_row(
                "SELECT last_seq, last_hash FROM remote_ledger_cursors WHERE conversation_id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(|error| err("remote_ledger", error))?;
        if cursor != (batch.expected_last_seq, batch.expected_last_hash.clone()) {
            return Err(err("remote_ledger", "cached ledger cursor conflict"));
        }
        let mut expected_seq = cursor
            .0
            .checked_add(1)
            .ok_or_else(|| err("remote_ledger", "ledger sequence overflow"))?;
        let mut expected_parent = cursor.1;
        let mut batch_bytes = 0usize;
        for entry in &batch.entries {
            if entry.seq != expected_seq || entry.parent != expected_parent {
                return Err(err(
                    "remote_ledger",
                    "ledger batch is not contiguous with cursor",
                ));
            }
            validate_nonempty("remote_ledger", "hash", &entry.hash)?;
            if entry.hash != crate::ledger::entry_hash(&entry.msg) {
                return Err(err("remote_ledger", "ledger entry hash mismatch"));
            }
            let msg =
                serde_json::to_vec(&entry.msg).map_err(|error| err("remote_ledger", error))?;
            if msg.len() > MAX_LEDGER_ENTRY_BYTES {
                return Err(err("remote_ledger", "ledger message exceeds 8 MiB"));
            }
            batch_bytes = batch_bytes
                .checked_add(msg.len())
                .ok_or_else(|| err("remote_ledger", "ledger batch byte count overflow"))?;
            if batch_bytes > MAX_LEDGER_BATCH_BYTES {
                return Err(err("remote_ledger", "ledger batch exceeds 16 MiB"));
            }
            tx.execute(
                "INSERT INTO remote_cached_ledger_entries (conversation_id, seq, hash, parent_hash, message_json) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![id, entry.seq, entry.hash, entry.parent, msg],
            ).map_err(|error| err("remote_ledger", error))?;
            expected_seq = expected_seq
                .checked_add(1)
                .ok_or_else(|| err("remote_ledger", "ledger sequence overflow"))?;
            expected_parent = Some(entry.hash.clone());
        }
        if let Some(last) = batch.entries.last() {
            let changed = tx.execute(
                "UPDATE remote_ledger_cursors SET last_seq=?1, last_hash=?2 WHERE conversation_id=?3 AND last_seq=?4 AND last_hash IS ?5",
                params![last.seq, last.hash, id, batch.expected_last_seq, batch.expected_last_hash],
            ).map_err(|error| err("remote_ledger", error))?;
            if changed != 1 {
                return Err(err("remote_ledger", "cached ledger cursor conflict"));
            }
        }
        tx.commit().map_err(|error| err("remote_ledger", error))?;
        Path::parse(&format!(
            "remote/conversations/{}/ledger/cursor",
            id_codec::encode_id(id)
        ))
        .map_err(StoreError::from)
    }

    pub(crate) fn remote_read_path(&self, from: &Path) -> Result<Option<Record>, StoreError> {
        let segments: Vec<&String> = from.iter().collect();
        let conn = self.db.lock().map_err(|error| err("remote_read", error))?;
        let value = match segments.as_slice() {
            [remote, nodes] if remote.as_str()=="remote" && nodes.as_str()=="nodes" => {
                let mut statement = conn.prepare(&format!("{NODE_SELECT} ORDER BY created_at, node_id")).map_err(|error| err("remote_read", error))?;
                let rows = statement.query_map([], node_value).map_err(|error| err("remote_read", error))?;
                Some(Value::Array(rows.collect::<Result<Vec<_>, _>>().map_err(|error| err("remote_read", error))?))
            }
            [remote, nodes, id] if remote.as_str()=="remote" && nodes.as_str()=="nodes" => conn.query_row(
                &format!("{NODE_SELECT} WHERE node_id=?1"), [decode_id(id)?], node_value,
            ).optional().map_err(|error| err("remote_read", error))?,
            [remote, conversations] if remote.as_str()=="remote" && conversations.as_str()=="conversations" => {
                let mut statement = conn.prepare(&format!("{CONVERSATION_SELECT} ORDER BY created_at, conversation_id")).map_err(|error| err("remote_read", error))?;
                let rows = statement.query_map([], conversation_value).map_err(|error| err("remote_read", error))?;
                Some(Value::Array(rows.collect::<Result<Vec<_>, _>>().map_err(|error| err("remote_read", error))?))
            }
            [remote, conversations, id] if remote.as_str()=="remote" && conversations.as_str()=="conversations" => conn.query_row(
                &format!("{CONVERSATION_SELECT} WHERE conversation_id=?1"), [decode_id(id)?], conversation_value,
            ).optional().map_err(|error| err("remote_read", error))?,
            [remote, conversations, id, ledger, cursor] if remote.as_str()=="remote" && conversations.as_str()=="conversations" && ledger.as_str()=="ledger" && cursor.as_str()=="cursor" => {
                let id = decode_id(id)?;
                conn.query_row("SELECT last_seq, last_hash FROM remote_ledger_cursors WHERE conversation_id=?1", [id], |row| {
                    let mut map=std::collections::BTreeMap::new();
                    map.insert("last_seq".into(), Value::Integer(row.get(0)?));
                    if let Some(hash)=row.get::<_,Option<String>>(1)? { map.insert("last_hash".into(), Value::String(hash)); }
                    Ok(Value::Map(map))
                }).optional().map_err(|error| err("remote_read", error))?
            }
            [remote, conversations, id, ledger, from_part, seq] if remote.as_str()=="remote" && conversations.as_str()=="conversations" && ledger.as_str()=="ledger" && from_part.as_str()=="from" => {
                let id=decode_id(id)?;
                let seq: i64=seq.parse().map_err(|error| err("remote_read", error))?;
                if seq < 0 { return Err(err("remote_read", "ledger from cursor must be non-negative")); }
                let mut statement=conn.prepare("SELECT seq, hash, parent_hash, message_json FROM remote_cached_ledger_entries WHERE conversation_id=?1 AND seq>=?2 ORDER BY seq LIMIT 1024").map_err(|error| err("remote_read", error))?;
                let rows=statement.query_map(params![id,seq], |row| {
                    let bytes: Vec<u8>=row.get(3)?;
                    let msg: serde_json::Value=serde_json::from_slice(&bytes).map_err(|error| rusqlite::Error::FromSqlConversionFailure(bytes.len(), rusqlite::types::Type::Blob, Box::new(error)))?;
                    Ok(CachedLedgerEntry{seq:row.get(0)?,hash:row.get(1)?,parent:row.get(2)?,msg})
                }).map_err(|error| err("remote_read", error))?;
                let entries=rows.collect::<Result<Vec<_>,_>>().map_err(|error| err("remote_read", error))?;
                Some(structfs_serde_store::to_value(&entries).map_err(|error| err("remote_read", error))?)
            }
            [remote, operations, pending] if remote.as_str()=="remote" && operations.as_str()=="operations" && pending.as_str()=="pending" => {
                let mut statement=conn.prepare("SELECT operation_id, operation_kind, node_id, node_attempt_id, conversation_id, request_hash, intent_json, state, result_json, lease_owner, lease_until, lease_epoch FROM remote_operations WHERE state='pending' OR (state='running' AND (lease_until IS NULL OR lease_until<=?1)) ORDER BY created_at, operation_id").map_err(|error| err("remote_read", error))?;
                let rows=statement.query_map([now_epoch()], operation_value).map_err(|error| err("remote_read", error))?;
                Some(Value::Array(rows.collect::<Result<Vec<_>,_>>().map_err(|error| err("remote_read", error))?))
            }
            [remote, operations, id] if remote.as_str()=="remote" && operations.as_str()=="operations" => conn.query_row(
                "SELECT operation_id, operation_kind, node_id, node_attempt_id, conversation_id, request_hash, intent_json, state, result_json, lease_owner, lease_until, lease_epoch FROM remote_operations WHERE operation_id=?1",
                [decode_id(id)?], operation_value,
            ).optional().map_err(|error| err("remote_read", error))?,
            _ => None,
        };
        Ok(value.map(Record::parsed))
    }

    pub(crate) fn remote_write_path(
        &self,
        to: &Path,
        data: &Record,
    ) -> Result<Option<Path>, StoreError> {
        let segments: Vec<&String> = to.iter().collect();
        let value = data
            .as_value()
            .cloned()
            .ok_or_else(|| err("remote_write", "expected parsed record"))?;
        let path = match segments.as_slice() {
            [remote, nodes] if remote.as_str() == "remote" && nodes.as_str() == "nodes" => {
                self.put_remote_node(&decode(value, "remote_node")?)?
            }
            [remote, nodes, id, attempt]
                if remote.as_str() == "remote"
                    && nodes.as_str() == "nodes"
                    && attempt.as_str() == "attempt" =>
            {
                self.replace_remote_node_attempt(
                    &decode_id(id)?,
                    &decode(value, "remote_node_attempt")?,
                )?
            }
            [remote, nodes, id, state]
                if remote.as_str() == "remote"
                    && nodes.as_str() == "nodes"
                    && state.as_str() == "state" =>
            {
                self.update_remote_node(&decode_id(id)?, &decode(value, "remote_node_update")?)?
            }
            [remote, nodes, id, observation]
                if remote.as_str() == "remote"
                    && nodes.as_str() == "nodes"
                    && observation.as_str() == "observation" =>
            {
                self.observe_remote_node(
                    &decode_id(id)?,
                    &decode(value, "remote_node_observation")?,
                )?
            }
            [remote, conversations]
                if remote.as_str() == "remote" && conversations.as_str() == "conversations" =>
            {
                self.put_remote_conversation(&decode(value, "remote_conversation")?)?
            }
            [remote, conversations, id, state]
                if remote.as_str() == "remote"
                    && conversations.as_str() == "conversations"
                    && state.as_str() == "state" =>
            {
                self.update_remote_conversation(
                    &decode_id(id)?,
                    &decode(value, "remote_conversation_update")?,
                )?
            }
            [remote, conversations, id, ledger]
                if remote.as_str() == "remote"
                    && conversations.as_str() == "conversations"
                    && ledger.as_str() == "ledger" =>
            {
                self.commit_cached_ledger(&decode_id(id)?, &decode(value, "remote_ledger")?)?
            }
            [remote, operations]
                if remote.as_str() == "remote" && operations.as_str() == "operations" =>
            {
                self.accept_remote_operation(&decode(value, "remote_operation")?)?
            }
            [remote, operations, id, state]
                if remote.as_str() == "remote"
                    && operations.as_str() == "operations"
                    && state.as_str() == "state" =>
            {
                self.update_remote_operation(
                    &decode_id(id)?,
                    &decode(value, "remote_operation_update")?,
                )?
            }
            [remote, operations, id, lease]
                if remote.as_str() == "remote"
                    && operations.as_str() == "operations"
                    && lease.as_str() == "lease" =>
            {
                self.claim_remote_operation(
                    &decode_id(id)?,
                    &decode(value, "remote_operation_lease")?,
                )?
            }
            [remote, operations, id, lease, release]
                if remote.as_str() == "remote"
                    && operations.as_str() == "operations"
                    && lease.as_str() == "lease"
                    && release.as_str() == "release" =>
            {
                self.release_remote_operation(
                    &decode_id(id)?,
                    &decode(value, "remote_operation_lease_release")?,
                )?
            }
            _ => return Ok(None),
        };
        Ok(Some(path))
    }
}

fn decode<T: for<'de> Deserialize<'de>>(
    value: Value,
    operation: &'static str,
) -> Result<T, StoreError> {
    structfs_serde_store::from_value(value).map_err(|error| err(operation, error))
}

fn operation_value(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let intent_bytes: Vec<u8> = row.get(6)?;
    let intent: serde_json::Value = serde_json::from_slice(&intent_bytes).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            intent_bytes.len(),
            rusqlite::types::Type::Blob,
            Box::new(error),
        )
    })?;
    let mut map = std::collections::BTreeMap::new();
    for (index, name) in [
        (0, "operation_id"),
        (1, "operation_kind"),
        (5, "request_hash"),
        (7, "state"),
    ] {
        map.insert(name.into(), Value::String(row.get(index)?));
    }
    for (index, name) in [
        (2, "node_id"),
        (3, "node_attempt_id"),
        (4, "conversation_id"),
    ] {
        if let Some(value) = row.get::<_, Option<String>>(index)? {
            map.insert(name.into(), Value::String(value));
        }
    }
    map.insert("intent".into(), structfs_serde_store::json_to_value(intent));
    if let Some(bytes) = row.get::<_, Option<Vec<u8>>>(8)? {
        let result: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                bytes.len(),
                rusqlite::types::Type::Blob,
                Box::new(error),
            )
        })?;
        map.insert("result".into(), structfs_serde_store::json_to_value(result));
    }
    if let Some(owner) = row.get::<_, Option<String>>(9)? {
        map.insert("lease_owner".into(), Value::String(owner));
    }
    if let Some(until) = row.get::<_, Option<i64>>(10)? {
        map.insert("lease_until".into(), Value::Integer(until));
    }
    map.insert("lease_epoch".into(), Value::Integer(row.get(11)?));
    Ok(Value::Map(map))
}

#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::{Reader, Writer, path};

    fn record<T: Serialize>(value: &T) -> Record {
        Record::parsed(structfs_serde_store::to_value(value).unwrap())
    }

    fn node(node_id: &str, attempt: &str) -> RemoteNodeIntent {
        RemoteNodeIntent {
            node_id: node_id.into(),
            node_attempt_id: attempt.into(),
            provider: "exe.dev".into(),
            vm_name: format!("ox-{node_id}"),
            ssh_host: Some("203.0.113.7".into()),
            ssh_port: 22,
            ssh_user: None,
            ssh_dest: Some("route@203.0.113.7".into()),
            identity_path: "/home/test/.ssh/id_ed25519".into(),
            known_hosts_path: "/home/test/.ox/known_hosts".into(),
            worker_socket_path: "/run/user/1000/ox-worker.sock".into(),
            desired_state: RemoteNodeDesiredState::Active,
            observed_state: RemoteNodeObservedState::Ready,
            cleanup_state: RemoteCleanupState::None,
            image_digest: Some("sha256:image".into()),
        }
    }

    fn conversation(id: &str, node_id: &str, attempt: &str) -> RemoteConversationIntent {
        RemoteConversationIntent {
            conversation_id: id.into(),
            node_id: node_id.into(),
            node_attempt_id: attempt.into(),
            create_id: format!("create-{id}"),
            title: "remote title".into(),
            initial_prompt: "do the work".into(),
            parent_thread_id: None,
            placement: RemotePlacement::FreshNode,
            desired_state: RemoteConversationDesiredState::Active,
            observed_state: RemoteConversationObservedState::Pending,
            cleanup_state: RemoteCleanupState::None,
        }
    }

    fn write_node(store: &mut InboxStore, value: &RemoteNodeIntent) -> Path {
        store.write(&path!("remote/nodes"), record(value)).unwrap()
    }

    fn write_conversation(store: &mut InboxStore, value: &RemoteConversationIntent) -> Path {
        store
            .write(&path!("remote/conversations"), record(value))
            .unwrap()
    }

    fn value_map(record: &Record) -> &std::collections::BTreeMap<String, Value> {
        let Value::Map(map) = record.as_value().unwrap() else {
            panic!("expected map")
        };
        map
    }

    #[test]
    fn structfs_paths_round_trip_optional_provider_fields_and_parent() {
        let root = tempfile::tempdir().unwrap();
        let mut store = InboxStore::open(root.path()).unwrap();
        let node_id = "node / 🦀";
        let node_path = write_node(&mut store, &node(node_id, "attempt-1"));
        assert_eq!(node_path, item_path("nodes", node_id).unwrap());
        let node_record = store.read(&node_path).unwrap().unwrap();
        assert!(!value_map(&node_record).contains_key("ssh_user"));

        let first = conversation("conversation / one", node_id, "attempt-1");
        let first_path = write_conversation(&mut store, &first);
        assert_eq!(
            first_path,
            item_path("conversations", &first.conversation_id).unwrap()
        );
        let first_read = store.read(&first_path).unwrap().unwrap();
        let first_map = value_map(&first_read);
        assert_eq!(
            first_map.get("placement"),
            Some(&Value::String("fresh_node".into()))
        );
        assert_eq!(
            first_map.get("cleanup_state"),
            Some(&Value::String("none".into()))
        );
        assert!(!first_map.contains_key("parent_thread_id"));

        let mut second = conversation("conversation-two", node_id, "attempt-1");
        second.parent_thread_id = Some("local-parent".into());
        let second_path = write_conversation(&mut store, &second);
        let second_read = store.read(&second_path).unwrap().unwrap();
        assert_eq!(
            value_map(&second_read).get("parent_thread_id"),
            Some(&Value::String("local-parent".into()))
        );

        let listed = store.read(&path!("remote/conversations")).unwrap().unwrap();
        let Value::Array(items) = listed.as_value().unwrap() else {
            panic!()
        };
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn semantic_operation_id_is_deterministic_unique_and_conflict_checked() {
        let root = tempfile::tempdir().unwrap();
        let mut store = InboxStore::open(root.path()).unwrap();
        write_node(&mut store, &node("node-1", "attempt-1"));
        let intent = RemoteOperationIntent {
            semantic_key: "command-42".into(),
            node_id: Some("node-1".into()),
            node_attempt_id: Some("attempt-1".into()),
            conversation_id: None,
            action: RemoteAction::ProvisionNode {
                cpu: 2,
                memory_gb: 4,
                disk_gb: 20,
                image: "image@sha256:one".into(),
            },
        };
        let first = store
            .write(&path!("remote/operations"), record(&intent))
            .unwrap();
        let second = store
            .write(&path!("remote/operations"), record(&intent))
            .unwrap();
        assert_eq!(first, second);
        assert!(store.read(&first).unwrap().is_some());

        let conflict = RemoteOperationIntent {
            action: RemoteAction::ProvisionNode {
                cpu: 8,
                memory_gb: 16,
                disk_gb: 100,
                image: "image@sha256:other".into(),
            },
            ..intent
        };
        assert!(
            store
                .write(&path!("remote/operations"), record(&conflict))
                .unwrap_err()
                .to_string()
                .contains("conflict:")
        );

        let impossible = RemoteOperationIntent {
            semantic_key: "message-without-conversation".into(),
            node_id: Some("node-1".into()),
            node_attempt_id: Some("attempt-1".into()),
            conversation_id: None,
            action: RemoteAction::SendMessage {
                message_id: "message-1".into(),
                content: "hello".into(),
            },
        };
        assert!(
            store
                .write(&path!("remote/operations"), record(&impossible))
                .unwrap_err()
                .to_string()
                .contains("invalid node/conversation target shape")
        );
    }

    #[test]
    fn cached_ledger_batch_and_cursor_are_atomic_and_hash_checked() {
        let root = tempfile::tempdir().unwrap();
        let mut store = InboxStore::open(root.path()).unwrap();
        write_node(&mut store, &node("node-1", "attempt-1"));
        let conversation = conversation("conversation-1", "node-1", "attempt-1");
        write_conversation(&mut store, &conversation);
        let first_msg = serde_json::json!({"type":"user","content":"one"});
        let first_hash = crate::ledger::entry_hash(&first_msg);
        let forged = CachedLedgerBatch {
            node_attempt_id: "attempt-1".into(),
            expected_last_seq: -1,
            expected_last_hash: None,
            entries: vec![
                CachedLedgerEntry {
                    seq: 0,
                    hash: first_hash.clone(),
                    parent: None,
                    msg: first_msg.clone(),
                },
                CachedLedgerEntry {
                    seq: 1,
                    hash: "forged".into(),
                    parent: Some(first_hash.clone()),
                    msg: serde_json::json!({"type":"assistant","content":"two"}),
                },
            ],
        };
        let ledger_path = Path::parse(&format!(
            "remote/conversations/{}/ledger",
            id_codec::encode_id(&conversation.conversation_id)
        ))
        .unwrap();
        assert!(
            store
                .write(&ledger_path, record(&forged))
                .unwrap_err()
                .to_string()
                .contains("hash mismatch")
        );
        let conn = store.db.lock().unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM remote_cached_ledger_entries",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let cursor: i64 = conn
            .query_row("SELECT last_seq FROM remote_ledger_cursors", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(
            (count, cursor),
            (0, -1),
            "the first insert must roll back with the forged second entry"
        );
        drop(conn);

        let valid = CachedLedgerBatch {
            entries: vec![CachedLedgerEntry {
                seq: 0,
                hash: first_hash.clone(),
                parent: None,
                msg: first_msg,
            }],
            ..forged
        };
        let cursor_path = store.write(&ledger_path, record(&valid)).unwrap();
        let cursor = store.read(&cursor_path).unwrap().unwrap();
        assert_eq!(value_map(&cursor).get("last_seq"), Some(&Value::Integer(0)));
        let from_path = Path::parse(&format!(
            "remote/conversations/{}/ledger/from/0",
            id_codec::encode_id(&conversation.conversation_id)
        ))
        .unwrap();
        let entries = store.read(&from_path).unwrap().unwrap();
        let Value::Array(entries) = entries.as_value().unwrap() else {
            panic!()
        };
        assert_eq!(entries.len(), 1);
        // StructFS rejects a negative component before it can reach the Store.
        assert!(
            Path::parse(&format!(
                "remote/conversations/{}/ledger/from/-1",
                id_codec::encode_id(&conversation.conversation_id)
            ))
            .is_err()
        );
    }

    #[test]
    fn attempt_replacement_rejects_stale_mutations_and_supersedes_old_operations() {
        let root = tempfile::tempdir().unwrap();
        let mut store = InboxStore::open(root.path()).unwrap();
        let original = node("node-1", "attempt-1");
        let node_path = write_node(&mut store, &original);
        let conversation = conversation("conversation-1", "node-1", "attempt-1");
        write_conversation(&mut store, &conversation);
        let operation = RemoteOperationIntent {
            semantic_key: "create-one".into(),
            node_id: Some("node-1".into()),
            node_attempt_id: Some("attempt-1".into()),
            conversation_id: Some("conversation-1".into()),
            action: RemoteAction::CreateConversation {
                create_id: conversation.create_id.clone(),
                title: conversation.title.clone(),
                prompt: conversation.initial_prompt.clone(),
                parent_thread_id: None,
            },
        };
        let operation_path = store
            .write(&path!("remote/operations"), record(&operation))
            .unwrap();

        let conversation_state = Path::parse(&format!(
            "{}/state",
            item_path("conversations", "conversation-1").unwrap()
        ))
        .unwrap();
        let bind = RemoteConversationUpdate {
            node_attempt_id: "attempt-1".into(),
            worker_thread_id: Some("t_remote".into()),
            desired_state: None,
            observed_state: None,
            cleanup_state: None,
        };
        store.write(&conversation_state, record(&bind)).unwrap();
        let conflicting_bind = RemoteConversationUpdate {
            worker_thread_id: Some("t_other".into()),
            ..bind
        };
        assert!(
            store
                .write(&conversation_state, record(&conflicting_bind))
                .unwrap_err()
                .to_string()
                .contains("already bound")
        );

        let replacement = RemoteNodeAttemptReplacement {
            expected_attempt_id: "attempt-1".into(),
            node: node("node-1", "attempt-2"),
        };
        let replace_path = Path::parse(&format!("{node_path}/attempt")).unwrap();
        store.write(&replace_path, record(&replacement)).unwrap();
        // An identical retry after an ambiguous local response is stable.
        store.write(&replace_path, record(&replacement)).unwrap();
        assert_eq!(
            store
                .write(&path!("remote/conversations"), record(&conversation))
                .unwrap(),
            item_path("conversations", "conversation-1").unwrap()
        );
        assert_eq!(
            store
                .write(&path!("remote/operations"), record(&operation))
                .unwrap(),
            operation_path
        );
        let operation = store.read(&operation_path).unwrap().unwrap();
        assert_eq!(
            value_map(&operation).get("state"),
            Some(&Value::String("superseded".into()))
        );
        let pending = store
            .read(&path!("remote/operations/pending"))
            .unwrap()
            .unwrap();
        assert_eq!(pending.as_value(), Some(&Value::Array(vec![])));
        let lost = store
            .read(&item_path("conversations", "conversation-1").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(
            value_map(&lost).get("observed_state"),
            Some(&Value::String("lost".into()))
        );

        let stale_node = RemoteNodeUpdate {
            node_attempt_id: "attempt-1".into(),
            desired_state: None,
            observed_state: Some(RemoteNodeObservedState::Absent),
            cleanup_state: None,
        };
        let node_state = Path::parse(&format!("{node_path}/state")).unwrap();
        assert!(
            store
                .write(&node_state, record(&stale_node))
                .unwrap_err()
                .to_string()
                .contains("stale")
        );
        let stale_conversation = RemoteConversationUpdate {
            node_attempt_id: "attempt-1".into(),
            worker_thread_id: Some("t_remote".into()),
            desired_state: None,
            observed_state: None,
            cleanup_state: None,
        };
        assert!(
            store
                .write(&conversation_state, record(&stale_conversation))
                .unwrap_err()
                .to_string()
                .contains("stale")
        );
        let stale_batch = CachedLedgerBatch {
            node_attempt_id: "attempt-1".into(),
            expected_last_seq: -1,
            expected_last_hash: None,
            entries: vec![],
        };
        let ledger_path = Path::parse(&format!(
            "{}/ledger",
            item_path("conversations", "conversation-1").unwrap()
        ))
        .unwrap();
        assert!(
            store
                .write(&ledger_path, record(&stale_batch))
                .unwrap_err()
                .to_string()
                .contains("stale")
        );
    }

    #[test]
    fn durable_state_survives_idempotent_reopen() {
        let root = tempfile::tempdir().unwrap();
        let (node_path, conversation_path) = {
            let mut store = InboxStore::open(root.path()).unwrap();
            let node_path = write_node(&mut store, &node("node-1", "attempt-1"));
            let conversation_path = write_conversation(
                &mut store,
                &conversation("conversation-1", "node-1", "attempt-1"),
            );
            (node_path, conversation_path)
        };
        let mut reopened = InboxStore::open(root.path()).unwrap();
        assert!(reopened.read(&node_path).unwrap().is_some());
        assert!(reopened.read(&conversation_path).unwrap().is_some());
        assert!(
            reopened
                .read(&Path::parse(&format!("{conversation_path}/ledger/cursor")).unwrap())
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn credentials_are_paths_only_and_unknown_secret_fields_are_not_persisted() {
        let root = tempfile::tempdir().unwrap();
        let mut store = InboxStore::open(root.path()).unwrap();
        let mut value = structfs_serde_store::to_value(&node("node-secret", "attempt-1")).unwrap();
        let Value::Map(map) = &mut value else {
            panic!()
        };
        let secret = "PRIVATE-KEY-BYTES-MUST-NOT-BE-IN-OX-DB";
        map.insert("private_key".into(), Value::String(secret.into()));
        assert!(
            store
                .write(&path!("remote/nodes"), Record::parsed(value))
                .is_err()
        );
        drop(store);
        for file in [root.path().join("ox.db"), root.path().join("ox.db-wal")] {
            if let Ok(bytes) = std::fs::read(file) {
                assert!(
                    !bytes
                        .windows(secret.len())
                        .any(|window| window == secret.as_bytes())
                );
            }
        }
    }

    #[test]
    fn invalid_closed_state_is_rejected_before_storage() {
        let root = tempfile::tempdir().unwrap();
        let mut store = InboxStore::open(root.path()).unwrap();
        let mut value = structfs_serde_store::to_value(&node("node-1", "attempt-1")).unwrap();
        let Value::Map(map) = &mut value else {
            panic!()
        };
        map.insert("observed_state".into(), Value::String("raedy".into()));
        assert!(
            store
                .write(&path!("remote/nodes"), Record::parsed(value))
                .is_err()
        );
        let count: i64 = store
            .db
            .lock()
            .unwrap()
            .query_row("SELECT COUNT(*) FROM remote_nodes", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);

        write_node(&mut store, &node("node-2", "attempt-1"));
        let node_path = item_path("nodes", "node-2").unwrap();
        let state_path = Path::parse(&format!("{node_path}/state")).unwrap();
        let regression = RemoteNodeUpdate {
            node_attempt_id: "attempt-1".into(),
            desired_state: None,
            observed_state: Some(RemoteNodeObservedState::Provisioning),
            cleanup_state: None,
        };
        assert!(
            store
                .write(&state_path, record(&regression))
                .unwrap_err()
                .to_string()
                .contains("illegal state transition")
        );
    }

    #[test]
    fn provider_observation_is_nullable_before_effect_and_attempt_fenced() {
        let root = tempfile::tempdir().unwrap();
        let mut store = InboxStore::open(root.path()).unwrap();
        let mut pending = node("node-pending", "attempt-1");
        pending.ssh_host = None;
        pending.ssh_dest = None;
        pending.observed_state = RemoteNodeObservedState::Pending;
        let node_path = write_node(&mut store, &pending);
        let persisted = store.read(&node_path).unwrap().unwrap();
        assert!(!value_map(&persisted).contains_key("ssh_host"));
        assert!(!value_map(&persisted).contains_key("ssh_dest"));

        let observation = RemoteNodeObservation {
            node_attempt_id: "attempt-1".into(),
            ssh_host: "203.0.113.9".into(),
            ssh_user: Some("route".into()),
            ssh_dest: "route@203.0.113.9".into(),
            observed_state: RemoteNodeObservedState::Provisioning,
        };
        store
            .write(
                &Path::parse(&format!("{node_path}/observation")).unwrap(),
                record(&observation),
            )
            .unwrap();
        let persisted = store.read(&node_path).unwrap().unwrap();
        assert_eq!(
            value_map(&persisted).get("ssh_host"),
            Some(&Value::String("203.0.113.9".into()))
        );
        let stale = RemoteNodeObservation {
            node_attempt_id: "attempt-old".into(),
            ..observation
        };
        assert!(
            store
                .write(
                    &Path::parse(&format!("{node_path}/observation")).unwrap(),
                    record(&stale),
                )
                .unwrap_err()
                .to_string()
                .contains("stale")
        );
    }

    #[test]
    fn expired_lease_takeover_fences_old_owner_terminal_commit_and_release() {
        let root = tempfile::tempdir().unwrap();
        let mut store = InboxStore::open(root.path()).unwrap();
        write_node(&mut store, &node("node-lease", "attempt-1"));
        let intent = RemoteOperationIntent {
            semantic_key: "lease-op".into(),
            node_id: Some("node-lease".into()),
            node_attempt_id: Some("attempt-1".into()),
            conversation_id: None,
            action: RemoteAction::ProvisionNode {
                cpu: 2,
                memory_gb: 4,
                disk_gb: 20,
                image: "image@sha256:one".into(),
            },
        };
        let operation_path = store
            .write(&path!("remote/operations"), record(&intent))
            .unwrap();
        let lease_path = Path::parse(&format!("{operation_path}/lease")).unwrap();
        let first = store
            .write(
                &lease_path,
                record(&RemoteOperationLeaseRequest {
                    owner_id: "owner-a".into(),
                    lease_seconds: 30,
                }),
            )
            .unwrap();
        assert_eq!(first.iter().last().map(String::as_str), Some("1"));
        store
            .db
            .lock()
            .unwrap()
            .execute(
                "UPDATE remote_operations SET lease_until=0 WHERE operation_id=?1",
                [decode_id(operation_path.iter().nth(2).unwrap()).unwrap()],
            )
            .unwrap();
        let second = store
            .write(
                &lease_path,
                record(&RemoteOperationLeaseRequest {
                    owner_id: "owner-b".into(),
                    lease_seconds: 30,
                }),
            )
            .unwrap();
        assert_eq!(second.iter().last().map(String::as_str), Some("2"));

        let release_path = Path::parse(&format!("{lease_path}/release")).unwrap();
        assert!(
            store
                .write(
                    &release_path,
                    record(&RemoteOperationLeaseRelease {
                        owner_id: "owner-a".into(),
                        lease_epoch: 1,
                    }),
                )
                .unwrap_err()
                .to_string()
                .contains("stale")
        );
        let state_path = Path::parse(&format!("{operation_path}/state")).unwrap();
        let old_commit = RemoteOperationUpdate {
            node_attempt_id: Some("attempt-1".into()),
            expected_state: RemoteOperationState::Running,
            state: RemoteOperationState::Applied,
            lease_owner: Some("owner-a".into()),
            lease_epoch: Some(1),
            result: None,
        };
        assert!(store.write(&state_path, record(&old_commit)).is_err());
        let current_commit = RemoteOperationUpdate {
            lease_owner: Some("owner-b".into()),
            lease_epoch: Some(2),
            ..old_commit
        };
        store.write(&state_path, record(&current_commit)).unwrap();
    }

    #[test]
    fn simultaneous_store_claimers_have_exactly_one_winner() {
        let root = tempfile::tempdir().unwrap();
        let operation_path = {
            let mut store = InboxStore::open(root.path()).unwrap();
            write_node(&mut store, &node("node-race", "attempt-1"));
            store
                .write(
                    &path!("remote/operations"),
                    record(&RemoteOperationIntent {
                        semantic_key: "race-op".into(),
                        node_id: Some("node-race".into()),
                        node_attempt_id: Some("attempt-1".into()),
                        conversation_id: None,
                        action: RemoteAction::ProvisionNode {
                            cpu: 2,
                            memory_gb: 4,
                            disk_gb: 20,
                            image: "image@sha256:one".into(),
                        },
                    }),
                )
                .unwrap()
        };
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let mut joins = Vec::new();
        for owner in ["owner-a", "owner-b"] {
            let root = root.path().to_path_buf();
            let barrier = barrier.clone();
            let lease_path = Path::parse(&format!("{operation_path}/lease")).unwrap();
            joins.push(std::thread::spawn(move || {
                let mut store = InboxStore::open(&root).unwrap();
                barrier.wait();
                store.write(
                    &lease_path,
                    record(&RemoteOperationLeaseRequest {
                        owner_id: owner.into(),
                        lease_seconds: 30,
                    }),
                )
            }));
        }
        barrier.wait();
        let results: Vec<_> = joins.into_iter().map(|join| join.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(results.iter().filter(|result| result.is_err()).count(), 1);
    }
}
