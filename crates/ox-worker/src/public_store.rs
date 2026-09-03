use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use ox_broker::async_store::{AsyncReader, AsyncWriter, BoxFuture};
use ox_executor::{ExecutionHandle, derive_unresolved_approval_id};
use ox_inbox::worker_ingress::{CancelEnvelope, CreateEnvelope, DecisionEnvelope, PromptEnvelope};
use sha2::{Digest as _, Sha256};
use structfs_core_store::{Error as StoreError, Path, Record, Value, path};
use tokio::sync::{Mutex, Semaphore};

use crate::ledger_cursor::{LedgerCursorLimits, read_batch, read_tail};

#[derive(Clone, Debug)]
pub struct WorkerLimits {
    pub max_active_turns: usize,
    pub max_queued_inputs_per_thread: usize,
    pub max_total_threads: usize,
    pub max_parked_cursors: usize,
    pub max_ledger_batch_entries: usize,
    pub max_ledger_batch_bytes: usize,
    pub max_ledger_line_bytes: usize,
}

#[derive(Clone, Debug)]
pub struct WorkerBuildIdentity {
    pub executable_digest: String,
    pub image_digest: String,
    pub sandbox_preflight: String,
}

impl Default for WorkerLimits {
    fn default() -> Self {
        Self {
            max_active_turns: 8,
            max_queued_inputs_per_thread: 16,
            max_total_threads: 256,
            max_parked_cursors: 64,
            max_ledger_batch_entries: 256,
            max_ledger_batch_bytes: 1024 * 1024,
            max_ledger_line_bytes: 256 * 1024,
        }
    }
}

impl WorkerLimits {
    pub fn validate(&self) -> Result<(), String> {
        if [
            self.max_active_turns,
            self.max_queued_inputs_per_thread,
            self.max_total_threads,
            self.max_parked_cursors,
            self.max_ledger_batch_entries,
            self.max_ledger_batch_bytes,
            self.max_ledger_line_bytes,
        ]
        .contains(&0)
        {
            return Err("all worker limits must be non-zero".into());
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct PublicStore {
    client: ox_broker::ClientHandle,
    execution: Arc<ExecutionHandle>,
    inbox_root: PathBuf,
    node_id: Arc<str>,
    attempt_id: Arc<str>,
    limits: WorkerLimits,
    create_admission: Arc<Mutex<()>>,
    message_admissions: Arc<std::sync::Mutex<HashMap<String, std::sync::Weak<Mutex<()>>>>>,
    cursor_admission: Arc<Semaphore>,
    executable_digest: Arc<str>,
    image_digest: Arc<str>,
    sandbox_preflight: Arc<str>,
}

impl PublicStore {
    pub fn new(
        client: ox_broker::ClientHandle,
        execution: Arc<ExecutionHandle>,
        inbox_root: PathBuf,
        node_id: String,
        attempt_id: String,
        limits: WorkerLimits,
        build_identity: WorkerBuildIdentity,
    ) -> Result<Self, String> {
        limits.validate()?;
        Ok(Self {
            client,
            execution,
            inbox_root,
            node_id: node_id.into(),
            attempt_id: attempt_id.into(),
            cursor_admission: Arc::new(Semaphore::new(limits.max_parked_cursors)),
            limits,
            create_admission: Arc::new(Mutex::new(())),
            message_admissions: Arc::new(std::sync::Mutex::new(HashMap::new())),
            executable_digest: build_identity.executable_digest.into(),
            image_digest: build_identity.image_digest.into(),
            sandbox_preflight: build_identity.sandbox_preflight.into(),
        })
    }

    fn error(operation: &'static str, message: impl std::fmt::Display) -> StoreError {
        StoreError::store("WorkerPublicStore", operation, message.to_string())
    }

    async fn read_value(&self, path: &Path) -> Result<Option<Value>, StoreError> {
        Ok(self
            .client
            .read(path)
            .await?
            .and_then(|record| record.as_value().cloned()))
    }

    async fn thread_exists(&self, thread_id: &str) -> Result<bool, StoreError> {
        let target = Path::parse(&format!("inbox/threads/{thread_id}"))?;
        Ok(self.client.read(&target).await?.is_some())
    }

    async fn dispatch(&self) -> Result<(), StoreError> {
        let execution = self.execution.clone();
        tokio::task::spawn_blocking(move || execution.dispatch_worker_ingress())
            .await
            .map_err(|error| Self::error("dispatch", error))?
            .map(|_| ())
            .map_err(|error| Self::error("dispatch", error))
    }

    async fn ingress_receipt_exists(&self, kind: &str, id: &str) -> Result<bool, StoreError> {
        let encoded = encode_id(id);
        let target = Path::parse(&format!("inbox/worker/{kind}/{encoded}"))?;
        Ok(self.client.read(&target).await?.is_some())
    }

    async fn pending_inputs(&self, thread_id: &str) -> Result<usize, StoreError> {
        required_count(
            self.read_value(&Path::parse(&format!(
                "inbox/worker/pending/messages/{thread_id}"
            ))?)
            .await?,
            "pending message count was missing",
        )
    }

    async fn thread_count(&self) -> Result<usize, StoreError> {
        thread_count_value(self.read_value(&path!("inbox/threads")).await?)
    }

    async fn reserved_thread_count(&self) -> Result<usize, StoreError> {
        required_count(
            self.read_value(&path!("inbox/worker/reserved/threads"))
                .await?,
            "reserved thread count was missing",
        )
    }

    fn message_admission(&self, thread_id: &str) -> Arc<Mutex<()>> {
        let mut locks = self
            .message_admissions
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(thread_id).and_then(std::sync::Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(Mutex::new(()));
        locks.insert(thread_id.to_string(), Arc::downgrade(&lock));
        lock
    }

    async fn unresolved_approval(
        &self,
        thread_id: &str,
    ) -> Result<Option<(String, Value)>, StoreError> {
        if !self.thread_exists(thread_id).await? {
            return Ok(None);
        }
        let scoped = self.client.scoped(&format!("threads/{thread_id}"));
        let pending = scoped.read(&path!("approval/pending")).await?;
        let Some(pending_value) =
            pending_value(pending.and_then(|record| record.as_value().cloned()))
        else {
            return Ok(None);
        };
        let entries = scoped
            .read_typed::<Vec<ox_kernel::log::LogEntry>>(&path!("log/entries"))
            .await?
            .unwrap_or_default();
        Ok(derive_unresolved_approval_id(thread_id, &entries).map(|id| (id, pending_value)))
    }

    async fn read_impl(self, from: Path) -> Result<Option<Record>, StoreError> {
        let parts: Vec<&str> = from.iter().map(String::as_str).collect();
        match parts.as_slice() {
            ["health"] => Ok(Some(parsed(object([
                ("status", "ready".into()),
                ("node_id", self.node_id.to_string().into()),
                ("attempt_id", self.attempt_id.to_string().into()),
                ("worker_version", env!("CARGO_PKG_VERSION").into()),
                ("wire_version", ox_structfs_transport::WIRE_VERSION.into()),
                ("image_digest", self.image_digest.to_string().into()),
                ("agent_wasm_sha256", ox_executor::agent_wasm_sha256().into()),
                (
                    "executable_sha256",
                    self.executable_digest.to_string().into(),
                ),
                ("policy_profile", "clash_remote_enforced".into()),
                (
                    "policy_contract_sha256",
                    format!("{:x}", Sha256::digest(b"clash_remote_enforced_v1")).into(),
                ),
                (
                    "sandbox_enforcement",
                    object([
                        ("mode", "required".into()),
                        ("preflight", self.sandbox_preflight.to_string().into()),
                    ]),
                ),
            ])))),
            ["capabilities"] => Ok(Some(parsed(object([
                ("protocol", "ox-worker-v1".into()),
                ("wire_version", ox_structfs_transport::WIRE_VERSION.into()),
                (
                    "operations",
                    serde_json::Value::Array(
                        ["create", "message", "ledger", "approval", "cancel"]
                            .map(|operation| operation.into())
                            .into(),
                    ),
                ),
                ("multiple_conversations", true.into()),
            ])))),
            ["capacity"] => {
                let stats = self.execution.stats();
                let threads = self.thread_count().await?;
                Ok(Some(parsed(object([
                    ("active_turns", stats.active_turns.into()),
                    ("active_turns_include_approval_parked_wasm", true.into()),
                    ("resident_threads", stats.resident_threads.into()),
                    ("total_threads", threads.into()),
                    (
                        "limits",
                        object([
                            ("active_turns", self.limits.max_active_turns.into()),
                            (
                                "queued_inputs_per_thread",
                                self.limits.max_queued_inputs_per_thread.into(),
                            ),
                            ("total_threads", self.limits.max_total_threads.into()),
                            ("parked_cursors", self.limits.max_parked_cursors.into()),
                        ]),
                    ),
                ]))))
            }
            ["conversations"] => self.client.read(&path!("inbox/threads")).await,
            ["conversations", thread_id] => {
                self.client
                    .read(&Path::parse(&format!("inbox/threads/{thread_id}"))?)
                    .await
            }
            ["conversations", thread_id, "ledger", "from", seq] => {
                if !self.thread_exists(thread_id).await? {
                    return Ok(None);
                }
                let from_seq = seq
                    .parse::<u64>()
                    .map_err(|_| Self::error("ledger", "invalid sequence"))?;
                let permit = self
                    .cursor_admission
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| Self::error("ledger", "overloaded: cursor admission is full"))?;
                let root = self.inbox_root.clone();
                let thread_id = (*thread_id).to_string();
                let limits = LedgerCursorLimits {
                    max_entries: self.limits.max_ledger_batch_entries,
                    max_batch_bytes: self.limits.max_ledger_batch_bytes,
                    max_line_bytes: self.limits.max_ledger_line_bytes,
                };
                let batch = tokio::task::spawn_blocking(move || {
                    read_batch(&root, &thread_id, from_seq, limits)
                })
                .await
                .map_err(|error| Self::error("ledger", error))?
                .map_err(|error| Self::error("ledger", error))?;
                drop(permit);
                Ok(Some(parsed(
                    serde_json::json!({"entries": batch.entries, "next_seq": batch.next_seq, "has_more": batch.has_more}),
                )))
            }
            ["conversations", thread_id, "result"] => {
                let permit = self
                    .cursor_admission
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| Self::error("result", "overloaded: cursor admission is full"))?;
                let metadata = self
                    .client
                    .read(&Path::parse(&format!("inbox/threads/{thread_id}"))?)
                    .await?;
                let Some(metadata) = metadata else {
                    return Ok(None);
                };
                let root = self.inbox_root.clone();
                let id = (*thread_id).to_string();
                let limits = LedgerCursorLimits {
                    max_entries: self.limits.max_ledger_batch_entries,
                    max_batch_bytes: self.limits.max_ledger_batch_bytes,
                    max_line_bytes: self.limits.max_ledger_line_bytes,
                };
                let batch = tokio::task::spawn_blocking(move || read_tail(&root, &id, limits))
                    .await
                    .map_err(|error| Self::error("result", error))?
                    .map_err(|error| Self::error("result", error))?;
                drop(permit);
                Ok(Some(parsed(serde_json::json!({
                    "thread": metadata.as_value().cloned().map(structfs_serde_store::value_to_json),
                    "ledger_tail": batch.entries,
                    "next_seq": batch.next_seq,
                    "projection": "durable_ledger_tail"
                }))))
            }
            ["conversations", thread_id, "approvals", "pending"] => {
                match self.unresolved_approval(thread_id).await? {
                    Some((id, request)) => Ok(Some(parsed(serde_json::json!({
                        "approval_id": id,
                        "request": structfs_serde_store::value_to_json(request)
                    })))),
                    None => Ok(Some(Record::parsed(Value::Null))),
                }
            }
            _ => Ok(None),
        }
    }

    async fn write_impl(self, to: Path, data: Record) -> Result<Path, StoreError> {
        let parts: Vec<&str> = to.iter().map(String::as_str).collect();
        match parts.as_slice() {
            ["conversations"] => {
                let _admission = self.create_admission.lock().await;
                let envelope: CreateEnvelope = decode(&data, "create")?;
                if !self
                    .ingress_receipt_exists("creates", &envelope.create_id)
                    .await?
                    && self
                        .thread_count()
                        .await?
                        .saturating_add(self.reserved_thread_count().await?)
                        >= self.limits.max_total_threads
                {
                    return Err(Self::error(
                        "create",
                        "overloaded: total thread limit reached",
                    ));
                }
                let value = structfs_serde_store::to_value(&envelope)
                    .map_err(|error| Self::error("create", error))?;
                self.client
                    .write(&path!("inbox/worker/creates"), Record::parsed(value))
                    .await?;
                self.dispatch().await?;
                let receipt = self
                    .client
                    .read(&Path::parse(&format!(
                        "inbox/worker/creates/{}",
                        encode_id(&envelope.create_id)
                    ))?)
                    .await?
                    .and_then(|record| record.as_value().cloned())
                    .ok_or_else(|| Self::error("create", "missing receipt"))?;
                let thread_id = receipt_thread_id(receipt)?;
                Path::parse(&format!("conversations/{thread_id}")).map_err(Into::into)
            }
            ["conversations", thread_id, "messages"] => {
                let admission = self.message_admission(thread_id);
                let _admission = admission.lock().await;
                let envelope: PromptEnvelope = decode(&data, "message")?;
                if !self.thread_exists(thread_id).await? {
                    return Err(Self::error("message", "unknown conversation"));
                }
                if !self
                    .ingress_receipt_exists("messages", &envelope.message_id)
                    .await?
                    && self.pending_inputs(thread_id).await?
                        >= self.limits.max_queued_inputs_per_thread
                {
                    return Err(Self::error(
                        "message",
                        "overloaded: queued input limit reached",
                    ));
                }
                let value = structfs_serde_store::to_value(&envelope)
                    .map_err(|error| Self::error("message", error))?;
                self.client
                    .write(
                        &Path::parse(&format!("inbox/worker/messages/{thread_id}"))?,
                        Record::parsed(value),
                    )
                    .await?;
                self.dispatch().await?;
                Path::parse(&format!(
                    "conversations/{thread_id}/messages/{}",
                    encode_id(&envelope.message_id)
                ))
                .map_err(Into::into)
            }
            ["conversations", thread_id, "approvals", approval_id] => {
                let response: ox_types::ApprovalResponse = decode(&data, "approval")?;
                let envelope = DecisionEnvelope {
                    approval_id: (*approval_id).to_string(),
                    decision: response.decision,
                };
                if !self
                    .ingress_receipt_exists("decisions", approval_id)
                    .await?
                {
                    let current = self
                        .unresolved_approval(thread_id)
                        .await?
                        .map(|value| value.0);
                    if current.as_deref() != Some(*approval_id) {
                        return Err(Self::error(
                            "approval",
                            "stale or missing approval identity",
                        ));
                    }
                }
                let value = structfs_serde_store::to_value(&envelope)
                    .map_err(|error| Self::error("approval", error))?;
                self.client
                    .write(
                        &Path::parse(&format!("inbox/worker/decisions/{thread_id}"))?,
                        Record::parsed(value),
                    )
                    .await?;
                self.dispatch().await?;
                Path::parse(&format!(
                    "conversations/{thread_id}/approvals/{approval_id}"
                ))
                .map_err(Into::into)
            }
            ["conversations", thread_id, "control", "cancel"] => {
                let envelope: CancelEnvelope = decode(&data, "cancel")?;
                if !self.thread_exists(thread_id).await? {
                    return Err(Self::error("cancel", "unknown conversation"));
                }
                let value = structfs_serde_store::to_value(&envelope)
                    .map_err(|error| Self::error("cancel", error))?;
                self.client
                    .write(
                        &Path::parse(&format!("inbox/worker/cancels/{thread_id}"))?,
                        Record::parsed(value),
                    )
                    .await?;
                self.dispatch().await?;
                Path::parse(&format!(
                    "conversations/{thread_id}/control/cancel/{}",
                    encode_id(&envelope.cancel_id)
                ))
                .map_err(Into::into)
            }
            _ => Err(StoreError::NoRoute { path: to }),
        }
    }
}

impl AsyncReader for PublicStore {
    fn read(&mut self, from: &Path) -> BoxFuture<Result<Option<Record>, StoreError>> {
        let store = self.clone();
        let from = from.clone();
        Box::pin(async move { store.read_impl(from).await })
    }
}

impl AsyncWriter for PublicStore {
    fn write(&mut self, to: &Path, data: Record) -> BoxFuture<Result<Path, StoreError>> {
        let store = self.clone();
        let to = to.clone();
        Box::pin(async move { store.write_impl(to, data).await })
    }
}

fn parsed(value: serde_json::Value) -> Record {
    Record::parsed(structfs_serde_store::json_to_value(value))
}

fn object<const N: usize>(entries: [(&str, serde_json::Value); N]) -> serde_json::Value {
    serde_json::Value::Object(
        entries
            .into_iter()
            .map(|(key, value)| (key.to_string(), value))
            .collect(),
    )
}

fn decode<T: serde::de::DeserializeOwned>(
    record: &Record,
    operation: &'static str,
) -> Result<T, StoreError> {
    let value = record
        .as_value()
        .cloned()
        .ok_or_else(|| PublicStore::error(operation, "expected parsed record"))?;
    structfs_serde_store::from_value(value).map_err(|error| PublicStore::error(operation, error))
}

fn required_count(value: Option<Value>, missing: &'static str) -> Result<usize, StoreError> {
    match value {
        Some(Value::Integer(count)) => {
            usize::try_from(count).map_err(|error| PublicStore::error("capacity", error))
        }
        _ => Err(PublicStore::error("capacity", missing)),
    }
}

fn thread_count_value(value: Option<Value>) -> Result<usize, StoreError> {
    match value {
        Some(Value::Array(threads)) => Ok(threads.len()),
        None => Ok(0),
        _ => Err(PublicStore::error(
            "capacity",
            "inbox thread listing was not an array",
        )),
    }
}

fn pending_value(value: Option<Value>) -> Option<Value> {
    match value {
        None | Some(Value::Null) => None,
        value => value,
    }
}

fn receipt_thread_id(receipt: Value) -> Result<String, StoreError> {
    match receipt {
        Value::Map(map) => match map.get("thread_id") {
            Some(Value::String(id)) => Ok(id.clone()),
            _ => Err(PublicStore::error("create", "receipt has no thread id")),
        },
        _ => Err(PublicStore::error("create", "invalid receipt")),
    }
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

/// Pure adapters used to prove corruption handling without mutating the live
/// inbox implementation behind the public Store.
#[doc(hidden)]
pub mod test_support {
    use super::*;

    pub fn required_count(value: Option<Value>) -> Result<usize, StoreError> {
        super::required_count(value, "missing test count")
    }

    pub fn thread_count(value: Option<Value>) -> Result<usize, StoreError> {
        super::thread_count_value(value)
    }

    pub fn pending_value(value: Option<Value>) -> Option<Value> {
        super::pending_value(value)
    }

    pub fn receipt_thread_id(receipt: Value) -> Result<String, StoreError> {
        super::receipt_thread_id(receipt)
    }

    pub async fn hold_cursor(store: &PublicStore) -> tokio::sync::OwnedSemaphorePermit {
        store
            .cursor_admission
            .clone()
            .acquire_owned()
            .await
            .unwrap()
    }

    pub fn message_admission_is_reused(store: &PublicStore, thread_id: &str) -> bool {
        let first = store.message_admission(thread_id);
        let second = store.message_admission(thread_id);
        Arc::ptr_eq(&first, &second)
    }

    pub fn message_admission_recovers_from_poison(store: &PublicStore) -> bool {
        let locks = store.message_admissions.clone();
        let _ = std::thread::spawn(move || {
            let _guard = locks.lock().unwrap();
            panic!("intentional admission-lock poison");
        })
        .join();
        Arc::strong_count(&store.message_admission("after-poison")) == 1
    }
}
