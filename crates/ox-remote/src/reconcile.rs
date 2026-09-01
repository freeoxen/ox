use std::sync::Arc;

use ox_inbox::remote_state::{CachedLedgerBatch, CachedLedgerEntry, RemoteConversationRecord};
use serde::Deserialize;
use structfs_core_store::Value;

use crate::{RemoteManagerError, StorePort};

#[derive(Deserialize)]
struct WorkerLedgerBatch {
    entries: Vec<ox_inbox::ledger::LedgerEntry>,
    has_more: bool,
}

pub(crate) async fn reconcile_ledger_batches(
    local: &Arc<dyn StorePort>,
    worker: &Arc<dyn StorePort>,
    conversation: &RemoteConversationRecord,
) -> Result<bool, RemoteManagerError> {
    let mut advanced = false;
    let thread_id = conversation.worker_thread_id.as_deref().ok_or_else(|| {
        RemoteManagerError::Invalid("conversation has no worker thread id".into())
    })?;
    loop {
        let conversation_path =
            crate::placement::encoded_item("conversations", &conversation.conversation_id)?;
        let ledger_path = crate::placement::child(&conversation_path, "ledger")?;
        let cursor_path = crate::placement::child(&ledger_path, "cursor")?;
        let cursor = local
            .read(&cursor_path)
            .await
            .map_err(|error| RemoteManagerError::store("read ledger cursor", error))?
            .ok_or_else(|| RemoteManagerError::Unavailable("ledger cursor is missing".into()))?;
        let Value::Map(cursor) = cursor
            .as_value()
            .cloned()
            .ok_or_else(|| RemoteManagerError::Invalid("ledger cursor was not parsed".into()))?
        else {
            return Err(RemoteManagerError::Invalid(
                "ledger cursor is not a map".into(),
            ));
        };
        let last_seq = match cursor.get("last_seq") {
            Some(Value::Integer(value)) => *value,
            _ => return Err(RemoteManagerError::Invalid("missing last_seq".into())),
        };
        let last_hash = match cursor.get("last_hash") {
            Some(Value::String(value)) => Some(value.clone()),
            None => None,
            _ => return Err(RemoteManagerError::Invalid("invalid last_hash".into())),
        };
        let next = last_seq
            .checked_add(1)
            .ok_or_else(|| RemoteManagerError::Invalid("ledger cursor overflow".into()))?;
        let remote_path = structfs_core_store::Path::parse(&format!(
            "conversations/{thread_id}/ledger/from/{next}"
        ))?;
        let batch = worker
            .read(&remote_path)
            .await
            .map_err(|error| RemoteManagerError::store("worker ledger", error))?
            .ok_or_else(|| RemoteManagerError::Unavailable("worker ledger is missing".into()))?;
        let batch: WorkerLedgerBatch = crate::placement::decode_record(batch, "worker ledger")?;
        let entries = batch
            .entries
            .into_iter()
            .map(|entry| {
                let seq = i64::try_from(entry.seq)
                    .map_err(|_| RemoteManagerError::Invalid("ledger sequence overflow".into()))?;
                Ok(CachedLedgerEntry {
                    seq,
                    hash: entry.hash,
                    parent: entry.parent,
                    msg: entry.msg,
                })
            })
            .collect::<Result<Vec<_>, RemoteManagerError>>()?;
        if entries.is_empty() {
            if batch.has_more {
                return Err(RemoteManagerError::Invalid(
                    "worker ledger made no progress".into(),
                ));
            }
            return Ok(advanced);
        }
        let commit = CachedLedgerBatch {
            node_attempt_id: conversation.node_attempt_id.clone(),
            expected_last_seq: last_seq,
            expected_last_hash: last_hash,
            entries,
        };
        let value = structfs_serde_store::to_value(&commit)
            .map_err(|error| RemoteManagerError::store("encode ledger", error))?;
        let conversation_path =
            crate::placement::encoded_item("conversations", &conversation.conversation_id)?;
        let target = crate::placement::child(&conversation_path, "ledger")?;
        local
            .write(&target, structfs_core_store::Record::parsed(value))
            .await
            .map_err(|error| RemoteManagerError::store("commit ledger", error))?;
        advanced = true;
        if !batch.has_more {
            return Ok(advanced);
        }
    }
}
