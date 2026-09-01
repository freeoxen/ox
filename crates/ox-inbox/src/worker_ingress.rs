//! Durable idempotency metadata for the headless worker ingress.
//!
//! Rows here are accepted intents, not another conversation model. The
//! existing thread row, thread directory, ledger, and Stores remain the
//! authoritative execution state.

use crate::InboxStore;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};
use structfs_core_store::{Error as StoreError, Path, Record, Value};

pub const ACCEPTED: &str = "accepted";
pub const APPLIED: &str = "applied";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CreateEnvelope {
    pub create_id: String,
    pub title: String,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PromptEnvelope {
    pub message_id: String,
    pub content: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DecisionEnvelope {
    pub approval_id: String,
    pub decision: ox_types::Decision,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CancelEnvelope {
    pub cancel_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum IntentKind {
    Create,
    Message,
    Decision,
    Cancel,
}

impl IntentKind {
    fn table(self) -> &'static str {
        match self {
            Self::Create => "worker_creates",
            Self::Message => "worker_inputs",
            Self::Decision => "worker_decisions",
            Self::Cancel => "worker_cancels",
        }
    }

    fn id_column(self) -> &'static str {
        match self {
            Self::Create => "create_id",
            Self::Message => "message_id",
            Self::Decision => "approval_id",
            Self::Cancel => "cancel_id",
        }
    }

    pub fn path_component(self) -> &'static str {
        match self {
            Self::Create => "creates",
            Self::Message => "messages",
            Self::Decision => "decisions",
            Self::Cancel => "cancels",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptedIntent {
    pub kind: IntentKind,
    pub semantic_id: String,
    pub thread_id: Option<String>,
    pub request_hash: String,
    pub record_json: Vec<u8>,
    pub state: String,
    pub result_path: Option<String>,
    pub accepted_seq: i64,
}

impl AcceptedIntent {
    pub fn decode<T: DeserializeOwned>(&self) -> Result<T, StoreError> {
        serde_json::from_slice(&self.record_json).map_err(|error| err("worker_decode", error))
    }

    pub fn receipt_path(&self) -> Result<Path, StoreError> {
        receipt_path(self.kind, &self.semantic_id)
    }

    fn to_value(&self) -> Value {
        let mut value = serde_json::json!({
            "kind": self.kind.path_component(),
            "semantic_id": self.semantic_id,
            "request_hash": self.request_hash,
            "state": self.state,
        });
        if let Some(thread_id) = &self.thread_id {
            value["thread_id"] = serde_json::Value::String(thread_id.clone());
        }
        if let Some(result_path) = &self.result_path {
            value["result_path"] = serde_json::Value::String(result_path.clone());
        }
        structfs_serde_store::json_to_value(value)
    }
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
    if id.is_empty() {
        return Err(err(operation, "semantic id must not be empty"));
    }
    Ok(())
}

/// Canonical v1 request hashing: domain and semantic fields are encoded as
/// an option tag plus an unsigned big-endian byte length and UTF-8 bytes.
fn canonical_hash(domain: &str, fields: &[Option<&str>]) -> String {
    let mut hasher = Sha256::new();
    hash_field(&mut hasher, Some(domain));
    for field in fields {
        hash_field(&mut hasher, *field);
    }
    format!("{:x}", hasher.finalize())
}

fn hash_field(hasher: &mut Sha256, field: Option<&str>) {
    match field {
        None => hasher.update([0]),
        Some(value) => {
            hasher.update([1]);
            hasher.update((value.len() as u64).to_be_bytes());
            hasher.update(value.as_bytes());
        }
    }
}

fn encoded<T: Serialize>(value: &T) -> Result<Vec<u8>, StoreError> {
    serde_json::to_vec(value).map_err(|error| err("worker_encode", error))
}

fn receipt_path(kind: IntentKind, id: &str) -> Result<Path, StoreError> {
    validate_id("worker_path", id)?;
    Path::parse(&format!(
        "worker/{}/{}",
        kind.path_component(),
        encode_id(id)
    ))
    .map_err(StoreError::from)
}

fn encode_id(id: &str) -> String {
    let mut encoded = String::with_capacity(1 + id.len() * 2);
    encoded.push('i');
    for byte in id.as_bytes() {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

fn decode_id(encoded: &str) -> Result<String, StoreError> {
    let hex = encoded
        .strip_prefix('i')
        .ok_or_else(|| err("worker_path", "encoded semantic id must start with 'i'"))?;
    if hex.len() % 2 != 0 {
        return Err(err("worker_path", "encoded semantic id has odd length"));
    }
    let bytes = (0..hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&hex[index..index + 2], 16))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| err("worker_path", error))?;
    String::from_utf8(bytes).map_err(|error| err("worker_path", error))
}

fn conflict(kind: IntentKind, id: &str) -> StoreError {
    err(
        "worker_accept",
        format!(
            "conflict: {} semantic id '{id}' was already accepted with a different request",
            kind.path_component()
        ),
    )
}

fn load_intent(
    conn: &Connection,
    kind: IntentKind,
    id: &str,
) -> Result<Option<AcceptedIntent>, StoreError> {
    let sql = format!(
        "SELECT {id}, thread_id, request_hash, record_json, state, result_path, accepted_seq \
         FROM {table} WHERE {id} = ?1",
        id = kind.id_column(),
        table = kind.table()
    );
    conn.query_row(&sql, [id], |row| {
        Ok(AcceptedIntent {
            kind,
            semantic_id: row.get(0)?,
            thread_id: row.get(1)?,
            request_hash: row.get(2)?,
            record_json: row.get(3)?,
            state: row.get(4)?,
            result_path: row.get(5)?,
            accepted_seq: row.get(6)?,
        })
    })
    .optional()
    .map_err(|error| err("worker_read", error))
}

fn accept(
    conn: &mut Connection,
    kind: IntentKind,
    id: &str,
    thread_id: Option<&str>,
    request_hash: &str,
    record_json: &[u8],
) -> Result<AcceptedIntent, StoreError> {
    validate_id("worker_accept", id)?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(|error| err("worker_accept", error))?;
    if let Some(existing) = load_intent(&tx, kind, id)? {
        return if existing.request_hash == request_hash {
            Ok(existing)
        } else {
            Err(conflict(kind, id))
        };
    }
    if let Some(thread_id) = thread_id {
        let exists = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM threads WHERE id = ?1)",
                [thread_id],
                |row| row.get::<_, bool>(0),
            )
            .map_err(|error| err("worker_accept", error))?;
        if !exists {
            return Err(err(
                "worker_accept",
                format!("unknown worker thread '{thread_id}'"),
            ));
        }
    }
    let accepted_seq = next_accepted_seq(&tx)?;
    let sql = format!(
        "INSERT INTO {table} \
         ({id}, thread_id, request_hash, record_json, state, accepted_seq, accepted_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        table = kind.table(),
        id = kind.id_column()
    );
    tx.execute(
        &sql,
        params![
            id,
            thread_id,
            request_hash,
            record_json,
            ACCEPTED,
            accepted_seq,
            now_epoch()
        ],
    )
    .map_err(|error| err("worker_accept", error))?;
    tx.commit().map_err(|error| err("worker_accept", error))?;
    load_intent(conn, kind, id)?.ok_or_else(|| err("worker_accept", "accepted row disappeared"))
}

fn next_accepted_seq(conn: &Connection) -> Result<i64, StoreError> {
    conn.query_row(
        "SELECT COALESCE(MAX(accepted_seq), 0) + 1 FROM (\
         SELECT accepted_seq FROM worker_creates UNION ALL \
         SELECT accepted_seq FROM worker_inputs UNION ALL \
         SELECT accepted_seq FROM worker_decisions UNION ALL \
         SELECT accepted_seq FROM worker_cancels)",
        [],
        |row| row.get(0),
    )
    .map_err(|error| err("worker_accept", error))
}

impl InboxStore {
    pub fn accept_worker_create(
        &self,
        envelope: &CreateEnvelope,
    ) -> Result<AcceptedIntent, StoreError> {
        validate_id("worker_create", &envelope.create_id)?;
        let request_hash = canonical_hash(
            "create-v1",
            &[
                Some(&envelope.title),
                Some(&envelope.prompt),
                envelope.parent_id.as_deref(),
            ],
        );
        let record_json = encoded(envelope)?;
        let mut conn = self
            .db
            .lock()
            .map_err(|error| err("worker_create", error))?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| err("worker_create", error))?;
        if let Some(existing) = load_intent(&tx, IntentKind::Create, &envelope.create_id)? {
            return if existing.request_hash == request_hash {
                Ok(existing)
            } else {
                Err(conflict(IntentKind::Create, &envelope.create_id))
            };
        }
        let thread_id = format!("t_{}", uuid::Uuid::new_v4().as_simple());
        let accepted_seq = next_accepted_seq(&tx)?;
        tx.execute(
            "INSERT INTO worker_creates \
             (create_id, request_hash, thread_id, record_json, state, accepted_seq, accepted_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                envelope.create_id,
                request_hash,
                thread_id,
                record_json,
                ACCEPTED,
                accepted_seq,
                now_epoch()
            ],
        )
        .map_err(|error| err("worker_create", error))?;
        tx.commit().map_err(|error| err("worker_create", error))?;
        load_intent(&conn, IntentKind::Create, &envelope.create_id)?
            .ok_or_else(|| err("worker_create", "accepted row disappeared"))
    }

    pub fn accept_worker_message(
        &self,
        thread_id: &str,
        envelope: &PromptEnvelope,
    ) -> Result<AcceptedIntent, StoreError> {
        let request_hash =
            canonical_hash("message-v1", &[Some(thread_id), Some(&envelope.content)]);
        let record_json = encoded(envelope)?;
        let mut conn = self
            .db
            .lock()
            .map_err(|error| err("worker_message", error))?;
        accept(
            &mut conn,
            IntentKind::Message,
            &envelope.message_id,
            Some(thread_id),
            &request_hash,
            &record_json,
        )
    }

    pub fn accept_worker_decision(
        &self,
        thread_id: &str,
        envelope: &DecisionEnvelope,
    ) -> Result<AcceptedIntent, StoreError> {
        let request_hash = canonical_hash(
            "decision-v1",
            &[Some(thread_id), Some(envelope.decision.as_str())],
        );
        let record_json = encoded(envelope)?;
        let mut conn = self
            .db
            .lock()
            .map_err(|error| err("worker_decision", error))?;
        accept(
            &mut conn,
            IntentKind::Decision,
            &envelope.approval_id,
            Some(thread_id),
            &request_hash,
            &record_json,
        )
    }

    pub fn accept_worker_cancel(
        &self,
        thread_id: &str,
        envelope: &CancelEnvelope,
    ) -> Result<AcceptedIntent, StoreError> {
        let request_hash =
            canonical_hash("cancel-v1", &[Some(thread_id), envelope.reason.as_deref()]);
        let record_json = encoded(envelope)?;
        let mut conn = self
            .db
            .lock()
            .map_err(|error| err("worker_cancel", error))?;
        accept(
            &mut conn,
            IntentKind::Cancel,
            &envelope.cancel_id,
            Some(thread_id),
            &request_hash,
            &record_json,
        )
    }

    pub fn worker_intent(
        &self,
        kind: IntentKind,
        id: &str,
    ) -> Result<Option<AcceptedIntent>, StoreError> {
        let conn = self.db.lock().map_err(|error| err("worker_read", error))?;
        load_intent(&conn, kind, id)
    }

    pub fn pending_worker_intents(&self) -> Result<Vec<AcceptedIntent>, StoreError> {
        let conn = self
            .db
            .lock()
            .map_err(|error| err("worker_pending", error))?;
        let mut intents = Vec::new();
        for kind in [
            IntentKind::Create,
            IntentKind::Message,
            IntentKind::Decision,
            IntentKind::Cancel,
        ] {
            let sql = format!(
                "SELECT {id}, thread_id, request_hash, record_json, state, result_path, accepted_seq \
                 FROM {table} WHERE state = ?1 ORDER BY accepted_seq",
                id = kind.id_column(),
                table = kind.table()
            );
            let mut statement = conn
                .prepare(&sql)
                .map_err(|error| err("worker_pending", error))?;
            let rows = statement
                .query_map([ACCEPTED], |row| {
                    Ok(AcceptedIntent {
                        kind,
                        semantic_id: row.get(0)?,
                        thread_id: row.get(1)?,
                        request_hash: row.get(2)?,
                        record_json: row.get(3)?,
                        state: row.get(4)?,
                        result_path: row.get(5)?,
                        accepted_seq: row.get(6)?,
                    })
                })
                .map_err(|error| err("worker_pending", error))?;
            intents.extend(
                rows.collect::<Result<Vec<_>, _>>()
                    .map_err(|error| err("worker_pending", error))?,
            );
        }
        intents.sort_by_key(|intent| intent.accepted_seq);
        Ok(intents)
    }

    pub fn pending_worker_message_count(&self, thread_id: &str) -> Result<usize, StoreError> {
        let conn = self
            .db
            .lock()
            .map_err(|error| err("worker_pending_count", error))?;
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM worker_inputs WHERE thread_id = ?1 AND state = ?2",
                params![thread_id, ACCEPTED],
                |row| row.get(0),
            )
            .map_err(|error| err("worker_pending_count", error))?;
        usize::try_from(count).map_err(|error| err("worker_pending_count", error))
    }

    pub fn reserved_worker_thread_count(&self) -> Result<usize, StoreError> {
        let conn = self
            .db
            .lock()
            .map_err(|error| err("worker_reserved_count", error))?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM worker_creates c LEFT JOIN threads t ON t.id = c.thread_id WHERE t.id IS NULL",
            [], |row| row.get(0),
        ).map_err(|error| err("worker_reserved_count", error))?;
        usize::try_from(count).map_err(|error| err("worker_reserved_count", error))
    }

    pub fn mark_worker_intent_applied(
        &self,
        kind: IntentKind,
        id: &str,
        result_path: &str,
    ) -> Result<AcceptedIntent, StoreError> {
        let conn = self
            .db
            .lock()
            .map_err(|error| err("worker_mark_applied", error))?;
        let sql = format!(
            "UPDATE {table} SET state = ?1, result_path = ?2, applied_at = ?3 \
             WHERE {id} = ?4",
            table = kind.table(),
            id = kind.id_column()
        );
        let changed = conn
            .execute(&sql, params![APPLIED, result_path, now_epoch(), id])
            .map_err(|error| err("worker_mark_applied", error))?;
        if changed == 0 {
            return Err(err(
                "worker_mark_applied",
                format!("unknown semantic id '{id}'"),
            ));
        }
        load_intent(&conn, kind, id)?
            .ok_or_else(|| err("worker_mark_applied", "applied row disappeared"))
    }

    /// Materialize the thread row and directory reserved during acceptance.
    pub fn apply_worker_create(&self, create_id: &str) -> Result<String, StoreError> {
        let intent = self
            .worker_intent(IntentKind::Create, create_id)?
            .ok_or_else(|| err("worker_apply_create", "unknown create id"))?;
        let envelope: CreateEnvelope = intent.decode()?;
        let thread_id = intent
            .thread_id
            .ok_or_else(|| err("worker_apply_create", "create has no reserved thread id"))?;
        {
            let conn = self
                .db
                .lock()
                .map_err(|error| err("worker_apply_create", error))?;
            let now = now_epoch();
            conn.execute(
                "INSERT OR IGNORE INTO threads \
                 (id, title, parent_id, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![thread_id, envelope.title, envelope.parent_id, now, now],
            )
            .map_err(|error| err("worker_apply_create", error))?;
            let existing: (String, Option<String>) = conn
                .query_row(
                    "SELECT title, parent_id FROM threads WHERE id = ?1",
                    [&thread_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(|error| err("worker_apply_create", error))?;
            if existing != (envelope.title.clone(), envelope.parent_id.clone()) {
                return Err(err(
                    "worker_apply_create",
                    format!(
                        "conflict: reserved thread '{thread_id}' already exists with different metadata"
                    ),
                ));
            }
        }
        std::fs::create_dir_all(self.threads_dir.join(&thread_id))
            .map_err(|error| err("worker_apply_create", error))?;
        Ok(thread_id)
    }

    pub(crate) fn worker_read_path(&self, from: &Path) -> Result<Option<Record>, StoreError> {
        let segments: Vec<&String> = from.iter().collect();
        match segments.as_slice() {
            [worker, pending] if worker.as_str() == "worker" && pending.as_str() == "pending" => {
                Ok(Some(Record::parsed(Value::Array(
                    self.pending_worker_intents()?
                        .into_iter()
                        .map(|intent| intent.to_value())
                        .collect(),
                ))))
            }
            [worker, pending, messages, thread_id]
                if worker.as_str() == "worker"
                    && pending.as_str() == "pending"
                    && messages.as_str() == "messages" =>
            {
                Ok(Some(Record::parsed(Value::Integer(
                    self.pending_worker_message_count(thread_id)? as i64,
                ))))
            }
            [worker, reserved, threads]
                if worker.as_str() == "worker"
                    && reserved.as_str() == "reserved"
                    && threads.as_str() == "threads" =>
            {
                Ok(Some(Record::parsed(Value::Integer(
                    self.reserved_worker_thread_count()? as i64,
                ))))
            }
            [worker, kind, id] if worker.as_str() == "worker" => Ok(self
                .worker_intent(parse_kind(kind)?, &decode_id(id)?)?
                .map(|intent| Record::parsed(intent.to_value()))),
            _ => Ok(None),
        }
    }

    pub(crate) fn worker_write_path(
        &self,
        to: &Path,
        data: &Record,
    ) -> Result<Option<Path>, StoreError> {
        let segments: Vec<&String> = to.iter().collect();
        let value = data
            .as_value()
            .cloned()
            .ok_or_else(|| err("worker_write", "expected parsed record"))?;
        let accepted = match segments.as_slice() {
            [worker, creates] if worker.as_str() == "worker" && creates.as_str() == "creates" => {
                let envelope: CreateEnvelope = structfs_serde_store::from_value(value)
                    .map_err(|error| err("worker_create", error))?;
                self.accept_worker_create(&envelope)?
            }
            [worker, messages, thread_id]
                if worker.as_str() == "worker" && messages.as_str() == "messages" =>
            {
                let envelope: PromptEnvelope = structfs_serde_store::from_value(value)
                    .map_err(|error| err("worker_message", error))?;
                self.accept_worker_message(thread_id, &envelope)?
            }
            [worker, decisions, thread_id]
                if worker.as_str() == "worker" && decisions.as_str() == "decisions" =>
            {
                let envelope: DecisionEnvelope = structfs_serde_store::from_value(value)
                    .map_err(|error| err("worker_decision", error))?;
                self.accept_worker_decision(thread_id, &envelope)?
            }
            [worker, cancels, thread_id]
                if worker.as_str() == "worker" && cancels.as_str() == "cancels" =>
            {
                let envelope: CancelEnvelope = structfs_serde_store::from_value(value)
                    .map_err(|error| err("worker_cancel", error))?;
                self.accept_worker_cancel(thread_id, &envelope)?
            }
            [worker, kind, id, applied]
                if worker.as_str() == "worker" && applied.as_str() == "applied" =>
            {
                let result_path = match data.as_value() {
                    Some(Value::String(path)) => path,
                    _ => return Err(err("worker_mark_applied", "expected result path string")),
                };
                let kind = parse_kind(kind)?;
                let semantic_id = decode_id(id)?;
                self.mark_worker_intent_applied(kind, &semantic_id, result_path)?;
                return Ok(Some(receipt_path(kind, &semantic_id)?));
            }
            _ => return Ok(None),
        };
        Ok(Some(accepted.receipt_path()?))
    }
}

fn parse_kind(value: &str) -> Result<IntentKind, StoreError> {
    match value {
        "creates" => Ok(IntentKind::Create),
        "messages" => Ok(IntentKind::Message),
        "decisions" => Ok(IntentKind::Decision),
        "cancels" => Ok(IntentKind::Cancel),
        _ => Err(err(
            "worker_path",
            format!("unknown worker intent kind '{value}'"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::{Reader, Writer, path};

    fn insert_thread(inbox: &InboxStore, thread_id: &str) {
        inbox
            .db
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO threads (id, title, created_at, updated_at) \
                 VALUES (?1, 'test', 0, 0)",
                [thread_id],
            )
            .unwrap();
    }

    #[test]
    fn identical_retry_is_stable_and_conflicting_retry_fails() {
        let root = tempfile::tempdir().unwrap();
        let mut inbox = InboxStore::open(root.path()).unwrap();
        let envelope = CreateEnvelope {
            create_id: "create-1".into(),
            title: "one".into(),
            prompt: "do one".into(),
            parent_id: None,
        };
        let record = Record::parsed(structfs_serde_store::to_value(&envelope).unwrap());
        let first = inbox
            .write(&path!("worker/creates"), record.clone())
            .unwrap();
        assert_eq!(
            first,
            inbox.write(&path!("worker/creates"), record).unwrap()
        );
        assert!(inbox.read(&first).unwrap().is_some());

        let conflict = CreateEnvelope {
            prompt: "different".into(),
            ..envelope
        };
        let error = inbox
            .write(
                &path!("worker/creates"),
                Record::parsed(structfs_serde_store::to_value(&conflict).unwrap()),
            )
            .unwrap_err();
        assert!(error.to_string().contains("conflict:"));
    }

    #[test]
    fn accepted_and_applied_state_survive_reopen() {
        let root = tempfile::tempdir().unwrap();
        let envelope = PromptEnvelope {
            message_id: "message-1".into(),
            content: "hello".into(),
        };
        let inbox = InboxStore::open(root.path()).unwrap();
        insert_thread(&inbox, "t_existing");
        inbox
            .accept_worker_message("t_existing", &envelope)
            .unwrap();
        let inbox = InboxStore::open(root.path()).unwrap();
        assert_eq!(inbox.pending_worker_intents().unwrap().len(), 1);
        inbox
            .mark_worker_intent_applied(
                IntentKind::Message,
                "message-1",
                "conversations/t_existing/ledger/from/0",
            )
            .unwrap();
        drop(inbox);
        let inbox = InboxStore::open(root.path()).unwrap();
        assert!(inbox.pending_worker_intents().unwrap().is_empty());
        assert_eq!(
            inbox
                .worker_intent(IntentKind::Message, "message-1")
                .unwrap()
                .unwrap()
                .state,
            APPLIED
        );
    }

    #[test]
    fn create_reserves_one_thread_across_apply_mark_crash() {
        let root = tempfile::tempdir().unwrap();
        let envelope = CreateEnvelope {
            create_id: "create-crash".into(),
            title: "durable".into(),
            prompt: "survive".into(),
            parent_id: None,
        };
        let reserved = InboxStore::open(root.path())
            .unwrap()
            .accept_worker_create(&envelope)
            .unwrap()
            .thread_id
            .unwrap();
        assert_eq!(
            InboxStore::open(root.path())
                .unwrap()
                .apply_worker_create("create-crash")
                .unwrap(),
            reserved
        );
        // Reopen after simulated crash between existing action and applied mark.
        assert_eq!(
            InboxStore::open(root.path())
                .unwrap()
                .apply_worker_create("create-crash")
                .unwrap(),
            reserved
        );
    }

    #[test]
    fn create_rejects_mismatched_existing_reserved_thread() {
        let root = tempfile::tempdir().unwrap();
        let inbox = InboxStore::open(root.path()).unwrap();
        let envelope = CreateEnvelope {
            create_id: "create-conflict".into(),
            title: "expected".into(),
            prompt: "survive".into(),
            parent_id: None,
        };
        let reserved = inbox
            .accept_worker_create(&envelope)
            .unwrap()
            .thread_id
            .unwrap();
        inbox
            .db
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO threads (id, title, parent_id, created_at, updated_at) \
                 VALUES (?1, 'different', NULL, 0, 0)",
                [&reserved],
            )
            .unwrap();

        let error = inbox.apply_worker_create("create-conflict").unwrap_err();
        assert!(error.to_string().contains("conflict:"));
    }

    #[test]
    fn pending_intents_preserve_global_cross_kind_accept_order() {
        let root = tempfile::tempdir().unwrap();
        let inbox = InboxStore::open(root.path()).unwrap();
        insert_thread(&inbox, "t_order");
        inbox
            .accept_worker_cancel(
                "t_order",
                &CancelEnvelope {
                    cancel_id: "cancel-first".into(),
                    reason: None,
                },
            )
            .unwrap();
        inbox
            .accept_worker_message(
                "t_order",
                &PromptEnvelope {
                    message_id: "message-second".into(),
                    content: "later".into(),
                },
            )
            .unwrap();
        assert_eq!(inbox.pending_worker_message_count("t_order").unwrap(), 1);
        assert_eq!(inbox.pending_worker_message_count("t_other").unwrap(), 0);
        inbox
            .accept_worker_decision(
                "t_order",
                &DecisionEnvelope {
                    approval_id: "decision-third".into(),
                    decision: ox_types::Decision::DenyOnce,
                },
            )
            .unwrap();
        let pending = inbox.pending_worker_intents().unwrap();
        assert_eq!(
            pending
                .iter()
                .map(|intent| intent.semantic_id.as_str())
                .collect::<Vec<_>>(),
            ["cancel-first", "message-second", "decision-third"]
        );
        assert!(
            pending
                .windows(2)
                .all(|pair| pair[0].accepted_seq < pair[1].accepted_seq)
        );
    }

    #[test]
    fn concurrent_identical_acceptance_returns_one_stable_receipt() {
        let root = tempfile::tempdir().unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let root = root.path().to_path_buf();
            let barrier = barrier.clone();
            workers.push(std::thread::spawn(move || {
                let inbox = InboxStore::open(&root).unwrap();
                barrier.wait();
                inbox
                    .accept_worker_create(&CreateEnvelope {
                        create_id: "concurrent-create".into(),
                        title: "same".into(),
                        prompt: "same prompt".into(),
                        parent_id: None,
                    })
                    .unwrap()
            }));
        }
        barrier.wait();
        let first = workers.remove(0).join().unwrap();
        let second = workers.remove(0).join().unwrap();
        assert_eq!(first.thread_id, second.thread_id);
        assert_eq!(
            first.receipt_path().unwrap(),
            second.receipt_path().unwrap()
        );
        assert_eq!(first.accepted_seq, second.accepted_seq);
        let inbox = InboxStore::open(root.path()).unwrap();
        assert_eq!(inbox.reserved_worker_thread_count().unwrap(), 1);
        inbox.apply_worker_create("concurrent-create").unwrap();
        assert_eq!(inbox.reserved_worker_thread_count().unwrap(), 0);
    }

    #[test]
    fn new_thread_scoped_intent_rejects_an_unknown_thread() {
        let root = tempfile::tempdir().unwrap();
        let inbox = InboxStore::open(root.path()).unwrap();
        let error = inbox
            .accept_worker_message(
                "t_missing",
                &PromptEnvelope {
                    message_id: "message-missing".into(),
                    content: "must not create an implicit thread".into(),
                },
            )
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("unknown worker thread 't_missing'")
        );
        assert!(inbox.pending_worker_intents().unwrap().is_empty());
    }
}
