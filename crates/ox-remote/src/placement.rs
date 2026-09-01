use std::sync::Arc;

use ox_inbox::remote_state::RemoteNodeRecord;
use serde::Deserialize;
use structfs_core_store::{Path, Value, path};

use crate::{PlacementPolicy, RemoteManagerError, StorePort, WorkerHealth, WorkerStoreConnector};

#[derive(Deserialize)]
struct WorkerCapacity {
    active_turns: usize,
    total_threads: usize,
    limits: WorkerCapacityLimits,
}

#[derive(Deserialize)]
struct WorkerCapacityLimits {
    active_turns: usize,
    total_threads: usize,
}

pub(crate) async fn select_existing(
    policy: &PlacementPolicy,
    local: &Arc<dyn StorePort>,
    connector: &Arc<dyn WorkerStoreConnector>,
) -> Result<Option<(RemoteNodeRecord, Arc<dyn StorePort>)>, RemoteManagerError> {
    if matches!(policy, PlacementPolicy::FreshNode) {
        return Ok(None);
    }
    let records = local
        .read(&path!("remote/nodes"))
        .await
        .map_err(|error| RemoteManagerError::store("list nodes", error))?
        .and_then(|record| record.as_value().cloned())
        .ok_or_else(|| RemoteManagerError::Unavailable("local node state is missing".into()))?;
    let Value::Array(nodes) = records else {
        return Err(RemoteManagerError::Invalid(
            "local node listing is not an array".into(),
        ));
    };
    let required = match policy {
        PlacementPolicy::RequireNode { node_id } => Some(node_id.as_str()),
        _ => None,
    };
    let mut candidates = nodes
        .into_iter()
        .map(|value| {
            structfs_serde_store::from_value::<RemoteNodeRecord>(value)
                .map_err(|error| RemoteManagerError::store("decode node", error))
        })
        .collect::<Result<Vec<_>, _>>()?;
    candidates.sort_by(|a, b| a.node_id.cmp(&b.node_id));
    if let Some(required) = required {
        candidates.retain(|node| node.node_id == required);
        if candidates.is_empty() {
            return Err(RemoteManagerError::RequiredNodeUnavailable(required.into()));
        }
    }
    for node in candidates {
        if node.desired_state != "active"
            || node.observed_state != "ready"
            || node.ssh_host.is_none()
            || node.ssh_dest.is_none()
        {
            continue;
        }
        let worker = match connector.connect(&node).await {
            Ok(worker) => worker,
            Err(_) => continue,
        };
        if verify_worker(&worker, &node).await.is_err() {
            continue;
        }
        let Some(record) = worker
            .read(&path!("capacity"))
            .await
            .map_err(|error| RemoteManagerError::store("worker capacity", error))?
        else {
            continue;
        };
        let capacity: WorkerCapacity = decode_record(record, "worker capacity")?;
        if capacity.active_turns < capacity.limits.active_turns
            && capacity.total_threads < capacity.limits.total_threads
        {
            return Ok(Some((node, worker)));
        }
    }
    if let Some(required) = required {
        Err(RemoteManagerError::RequiredNodeUnavailable(required.into()))
    } else {
        Ok(None)
    }
}

pub(crate) async fn verify_worker(
    worker: &Arc<dyn StorePort>,
    node: &RemoteNodeRecord,
) -> Result<WorkerHealth, RemoteManagerError> {
    let record = worker
        .read(&path!("health"))
        .await
        .map_err(|error| RemoteManagerError::store("worker health", error))?
        .ok_or_else(|| RemoteManagerError::Unavailable("worker health is missing".into()))?;
    let health: WorkerHealth = decode_record(record, "worker health")?;
    if health.status != "ready"
        || health.node_id != node.node_id
        || health.attempt_id != node.node_attempt_id
        || health.sandbox_enforcement.mode != "required"
        || health.sandbox_enforcement.preflight != "passed"
        || node.image_digest.as_deref() != Some(health.image_digest.as_str())
    {
        return Err(RemoteManagerError::IdentityMismatch(format!(
            "expected {}/{}, got {}/{} ({})",
            node.node_id, node.node_attempt_id, health.node_id, health.attempt_id, health.status
        )));
    }
    Ok(health)
}

pub(crate) fn decode_record<T: serde::de::DeserializeOwned>(
    record: structfs_core_store::Record,
    operation: &'static str,
) -> Result<T, RemoteManagerError> {
    let value = record
        .as_value()
        .cloned()
        .ok_or_else(|| RemoteManagerError::Invalid(format!("{operation} was not parsed")))?;
    structfs_serde_store::from_value(value)
        .map_err(|error| RemoteManagerError::store(operation, error))
}

pub(crate) fn encoded_item(kind: &str, id: &str) -> Result<Path, RemoteManagerError> {
    ox_inbox::remote_state::remote_item_path(kind, id)
        .map_err(|error| RemoteManagerError::store("remote item path", error))
}

pub(crate) fn child(path: &Path, suffix: &str) -> Result<Path, RemoteManagerError> {
    Path::parse(&format!("{path}/{suffix}")).map_err(Into::into)
}
