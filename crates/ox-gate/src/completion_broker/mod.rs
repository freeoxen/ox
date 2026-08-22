//! `CompletionBrokerStore` — substrate-mediated LLM completion dispatch,
//! modeled on structfs_http::HttpBrokerStore but generalized for streaming.
//!
//! Path layout (mirrors HttpBrokerStore's `outstanding/{N}` convention):
//!   write /                                CompletionRequest → outstanding/{N}
//!   read  outstanding/{N}                  CompletionStatus
//!   read  outstanding/{N}/request          original CompletionRequest
//!   read  outstanding/{N}/events/from/{S}  Vec<StreamEvent> — BLOCKING
//!   read  outstanding/{N}/events/count     usize — non-blocking buffer length
//!   read  outstanding/{N}/usage            UsageInfo (None until Complete)
//!   write outstanding/{N} null             GC

mod dispatch;
mod inflight;

#[cfg(any(test, feature = "test-utils"))]
pub mod mock;

pub use inflight::CompletionStatus;
#[allow(unused_imports)]
pub(crate) use inflight::{Inflight, InflightState};

use std::collections::HashMap;
use std::sync::Arc;

use ox_broker::ClientHandle;
use ox_broker::async_store::{AsyncReader, AsyncWriter, BoxFuture};
use structfs_core_store::{Error as StoreError, Path, Record, Value};
use tokio::runtime::Handle as TokioHandle;


// Used in the AsyncWriter impl for deserializing the inbound record.
#[allow(unused_imports)]
use structfs_serde_store;

pub type RequestId = u64;

/// Streaming completion broker.
pub struct CompletionBrokerStore {
    /// Broker client handle used by per-request dispatch tasks to resolve
    /// gate/* and secret/* paths. Cloned per spawn.
    #[allow(dead_code)]
    pub(crate) substrate: ClientHandle,

    /// Handle scoped to the upstream mount (`UpstreamStore`). Dispatch
    /// writes the outbound request there and drains events with blocking
    /// reads — the broker owns no sockets, only paths.
    #[allow(dead_code)]
    pub(crate) upstream: ClientHandle,

    /// In-memory in-flight tracker. Per-request state has its own Notify.
    /// No outer Mutex needed — AsyncReader/AsyncWriter give us &mut self.
    #[allow(dead_code)]
    pub(crate) handles: HashMap<RequestId, Arc<Inflight>>,

    #[allow(dead_code)]
    pub(crate) next_request_id: RequestId,

    /// Broker client scoped to gateway/usage for appending UsageRecords on
    /// Complete (Task 3.4 uses this).
    #[allow(dead_code)]
    pub(crate) usage_writer: ClientHandle,

    /// Tokio handle for spawning per-request dispatch tasks.
    #[allow(dead_code)]
    pub(crate) runtime: TokioHandle,

    /// Optional traffic-log writer (scoped to gateway/traffic). When set,
    /// each request's full lifecycle — decoded request, upstream body,
    /// every stream event, terminal status, usage — is appended as one
    /// record at terminal. Opt-in because the record contains complete
    /// prompt and completion text.
    pub(crate) traffic_writer: Option<ClientHandle>,
}

impl CompletionBrokerStore {
    pub fn new(
        substrate: ClientHandle,
        upstream: ClientHandle,
        usage_writer: ClientHandle,
        runtime: TokioHandle,
    ) -> Self {
        Self {
            substrate,
            upstream,
            handles: HashMap::new(),
            next_request_id: 0,
            usage_writer,
            runtime,
            traffic_writer: None,
        }
    }

    /// Enable full traffic logging: one record per request appended via
    /// `handle` (expected to be scoped to a traffic-log mount).
    pub fn with_traffic_writer(mut self, handle: ClientHandle) -> Self {
        self.traffic_writer = Some(handle);
        self
    }

    /// Parse `outstanding/{id}[/sub/...]` path. Returns the request id
    /// and any sub-path as a `/`-joined string.
    pub(crate) fn parse_handle_path(path: &Path) -> Option<(RequestId, Option<String>)> {
        if path.is_empty() || path[0].as_str() != "outstanding" {
            return None;
        }
        if path.len() == 1 {
            return None;
        }
        let id: RequestId = path[1].as_str().parse().ok()?;
        let sub = if path.len() > 2 {
            Some(
                (2..path.len())
                    .map(|i| path[i].as_str())
                    .collect::<Vec<_>>()
                    .join("/"),
            )
        } else {
            None
        };
        Some((id, sub))
    }
}

impl AsyncReader for CompletionBrokerStore {
    fn read(&mut self, from: &Path) -> BoxFuture<Result<Option<Record>, StoreError>> {
        // Root descriptor map.
        if from.is_empty() {
            let mut map = std::collections::BTreeMap::new();
            map.insert("outstanding".to_string(), Value::String("outstanding".into()));
            map.insert("docs".to_string(), Value::String("docs".into()));
            return Box::pin(async move { Ok(Some(Record::parsed(Value::Map(map)))) });
        }

        // /docs
        if from.len() == 1 && from[0].as_str() == "docs" {
            return Box::pin(async move { Ok(Some(Record::parsed(docs_value()))) });
        }

        // /outstanding listing
        if from.len() == 1 && from[0].as_str() == "outstanding" {
            let items: Vec<Value> = self
                .handles
                .keys()
                .map(|id| Value::String(format!("outstanding/{id}")))
                .collect();
            let mut map = std::collections::BTreeMap::new();
            map.insert("items".to_string(), Value::Array(items));
            return Box::pin(async move { Ok(Some(Record::parsed(Value::Map(map)))) });
        }

        // /outstanding/{N}[/sub]
        let (id, sub) = match Self::parse_handle_path(from) {
            Some(t) => t,
            None => return Box::pin(async move { Ok(None) }),
        };

        // Clone the Arc out of the map; the borrow on self ends when this
        // function returns, freeing the actor lock for other reads/writes.
        let inflight = match self.handles.get(&id) {
            Some(arc) => arc.clone(),
            None => return Box::pin(async move { Ok(None) }),
        };

        Box::pin(async move {
            match sub.as_deref() {
                // outstanding/{N} — current status (non-blocking)
                None => {
                    let state = inflight.state.lock().await;
                    let value = structfs_serde_store::to_value(&state.status).map_err(|e| {
                        StoreError::store("completion_broker", "read", e.to_string())
                    })?;
                    Ok(Some(Record::parsed(value)))
                }
                // outstanding/{N}/request — original CompletionRequest
                Some("request") => {
                    let state = inflight.state.lock().await;
                    let value = structfs_serde_store::to_value(&state.request).map_err(|e| {
                        StoreError::store("completion_broker", "read", e.to_string())
                    })?;
                    Ok(Some(Record::parsed(value)))
                }
                // outstanding/{N}/usage — UsageInfo (None until Complete)
                Some("usage") => {
                    let state = inflight.state.lock().await;
                    match &state.usage {
                        Some(u) => {
                            let value = structfs_serde_store::to_value(u).map_err(|e| {
                                StoreError::store("completion_broker", "read", e.to_string())
                            })?;
                            Ok(Some(Record::parsed(value)))
                        }
                        None => Ok(None),
                    }
                }
                // outstanding/{N}/events/count — buffer length (non-blocking)
                Some("events/count") => {
                    let state = inflight.state.lock().await;
                    Ok(Some(Record::parsed(Value::Integer(
                        state.events.len() as i64,
                    ))))
                }
                // outstanding/{N}/events/from/{S} — blocking drain from index S.
                //
                // The Notified future is created and enabled BEFORE the state
                // check. dispatch.rs signals with notify_waiters(), which
                // stores no permit — a plain check-then-notified().await form
                // loses any notification that lands between the lock drop and
                // the await's first poll, and with no later notification the
                // read hangs forever.
                Some(s) if s.starts_with("events/from/") => {
                    let seq: usize = s
                        .trim_start_matches("events/from/")
                        .parse()
                        .map_err(|e: std::num::ParseIntError| {
                            StoreError::store("completion_broker", "read", e.to_string())
                        })?;
                    loop {
                        let notified = inflight.notify.notified();
                        tokio::pin!(notified);
                        notified.as_mut().enable();
                        {
                            let state = inflight.state.lock().await;
                            if state.events.len() > seq || state.status.is_terminal() {
                                // Terminal short-circuits the length check, so
                                // clamp: a client-supplied seq past the end must
                                // read as empty, not panic the actor.
                                let start = seq.min(state.events.len());
                                let tail = state.events[start..].to_vec();
                                let value = structfs_serde_store::to_value(&tail).map_err(|e| {
                                    StoreError::store("completion_broker", "read", e.to_string())
                                })?;
                                return Ok(Some(Record::parsed(value)));
                            }
                        }
                        notified.await;
                    }
                }
                _ => Ok(None),
            }
        })
    }
}

fn docs_value() -> Value {
    let json = serde_json::json!({
        "title": "CompletionBrokerStore",
        "paths": {
            "write /": "Queue CompletionRequest → outstanding/{N}",
            "read outstanding/{N}": "CompletionStatus",
            "read outstanding/{N}/request": "Original CompletionRequest",
            "read outstanding/{N}/events/from/{S}": "Vec<StreamEvent> from index S (BLOCKING)",
            "read outstanding/{N}/events/count": "usize current buffer length",
            "read outstanding/{N}/usage": "UsageInfo (None until Complete)",
            "write outstanding/{N} null": "Delete handle"
        }
    });
    structfs_serde_store::json_to_value(json)
}

/// `AsyncWriter` for `CompletionBrokerStore`.
///
/// Two legal write shapes:
///
/// 1. Root (`/`) with a `CompletionRequest` record — inserts an `Inflight`
///    handle, spawns the per-request dispatch task, returns `outstanding/{N}`.
///
/// 2. `outstanding/{N}` with `Value::Null` — GC: removes the handle and
///    returns the same path.
///
/// All other paths and non-null writes to existing handles are errors.
impl AsyncWriter for CompletionBrokerStore {
    fn write(&mut self, to: &Path, data: Record) -> BoxFuture<Result<Path, StoreError>> {
        let to = to.clone();

        // GC: write null to outstanding/{N}
        if let Some((id, None)) = Self::parse_handle_path(&to) {
            let value_is_null = matches!(data.as_value(), Some(Value::Null));
            if value_is_null {
                self.handles.remove(&id);
                return Box::pin(async move { Ok(to) });
            }
            return Box::pin(async move {
                Err(StoreError::store(
                    "completion_broker",
                    "write",
                    "cannot overwrite an outstanding handle; write null to delete",
                ))
            });
        }

        // Queue: write CompletionRequest to root
        if to.is_empty() {
            let value = match data.as_value() {
                Some(v) => v.clone(),
                None => {
                    return Box::pin(async move {
                        Err(StoreError::store(
                            "completion_broker",
                            "write",
                            "expected parsed record",
                        ))
                    });
                }
            };
            let request: ox_kernel::CompletionRequest =
                match structfs_serde_store::from_value(value) {
                    Ok(r) => r,
                    Err(e) => {
                        return Box::pin(async move {
                            Err(StoreError::store(
                                "completion_broker",
                                "write",
                                format!("invalid CompletionRequest: {e}"),
                            ))
                        });
                    }
                };

            let id = self.next_request_id;
            self.next_request_id += 1;

            let inflight = Inflight::new(request);
            self.handles.insert(id, inflight.clone());

            let substrate = self.substrate.clone();
            let upstream = self.upstream.clone();
            let usage_writer = self.usage_writer.clone();
            let traffic_writer = self.traffic_writer.clone();
            self.runtime.spawn(async move {
                dispatch::per_request_task(inflight, substrate, upstream, usage_writer, traffic_writer)
                    .await;
            });

            let path = Path::try_from_components(vec![
                "outstanding".to_string(),
                id.to_string(),
            ])
            .map_err(|e| StoreError::store("completion_broker", "write", e.to_string()));
            return Box::pin(async move { path });
        }

        Box::pin(async move {
            Err(StoreError::store(
                "completion_broker",
                "write",
                format!("unexpected write path: {to}"),
            ))
        })
    }
}

#[cfg(test)]
mod dispatch_tests {
    use super::*;
    use crate::completion_broker::mock::MockSseExecutor;
    use ox_broker::BrokerStore;
    use ox_kernel::CompletionRequest;
    use ox_path::oxpath;
    use ox_store_util::StoreBacking;
    use ox_types::StreamEvent;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use structfs_core_store::{Error as StoreError, Record, Value, path};
    use structfs_serde_store::to_value;

    /// Minimal in-memory `StoreBacking` for tests. Holds an append-only
    /// Vec of items (or a single snapshot value for save/load).
    struct MemoryBacking {
        items: Mutex<Vec<Value>>,
    }

    impl MemoryBacking {
        fn new() -> Self {
            Self {
                items: Mutex::new(Vec::new()),
            }
        }
    }

    impl StoreBacking for MemoryBacking {
        fn load(&self) -> Result<Option<Value>, StoreError> {
            let items = self.items.lock().unwrap();
            if items.is_empty() {
                Ok(None)
            } else {
                Ok(Some(Value::Array(items.clone())))
            }
        }

        fn save(&self, value: &Value) -> Result<(), StoreError> {
            let mut items = self.items.lock().unwrap();
            *items = match value {
                Value::Array(a) => a.clone(),
                other => vec![other.clone()],
            };
            Ok(())
        }

        fn append(&self, item: &Value) -> Result<(), StoreError> {
            self.items.lock().unwrap().push(item.clone());
            Ok(())
        }
    }

    /// Stand up an in-memory broker seeded with gate + secret mounts that the
    /// dispatch task resolves.
    async fn build_substrate() -> BrokerStore {
        use crate::{AccountConfig, ApiKey, ProviderConfig};
        use ox_store_util::LocalConfig;
        use ox_types::CompletionRole;

        let broker = BrokerStore::new(Duration::from_secs(2));

        let mut gate_config = LocalConfig::new();
        let role = CompletionRole {
            account: "anthropic".into(),
            model_id: "claude-sonnet-4-20250514".into(),
        };
        gate_config.set("gate/completions/fast", to_value(&role).unwrap());
        gate_config.set(
            "gate/accounts/anthropic",
            to_value(&AccountConfig {
                provider: "anthropic".into(),
                ..Default::default()
            })
            .unwrap(),
        );
        gate_config.set(
            "gate/providers/anthropic",
            to_value(&ProviderConfig::anthropic()).unwrap(),
        );
        // Mount at root so paths like "gate/accounts/anthropic" resolve directly.
        broker.mount(path!(""), gate_config).await;

        let mut secret = LocalConfig::new();
        secret.set("keys/anthropic", to_value(&ApiKey::new("sk-test")).unwrap());
        broker.mount(oxpath!("secret"), secret).await;

        broker
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn write_returns_handle_and_dispatch_completes() {
        let broker = build_substrate().await;

        // Stand up an in-memory UsageStore at gateway/usage.
        let usage_backing = Box::new(MemoryBacking::new());
        let usage_store = crate::UsageStore::new(usage_backing);
        broker
            .mount(oxpath!("gateway", "usage"), usage_store)
            .await;

        let executor = Arc::new(MockSseExecutor::new());
        executor.push_immediate(StreamEvent::TextDelta { text: "hi".into() });
        executor.push_immediate(StreamEvent::OutputUsage { output_tokens: 1 });
        executor.push_immediate(StreamEvent::MessageStop);

        let client = broker.client();
        let substrate = client.clone();
        let usage_writer = client.scoped("gateway/usage");

        let upstream_store =
            crate::upstream_store::UpstreamStore::new(executor, tokio::runtime::Handle::current());
        broker.mount_async(oxpath!("upstream"), upstream_store).await;
        let store = CompletionBrokerStore::new(
            substrate,
            client.scoped("upstream"),
            usage_writer,
            tokio::runtime::Handle::current(),
        );
        broker
            .mount_async(oxpath!("gateway", "completions"), store)
            .await;

        let request = CompletionRequest {
            model: "anthropic/claude-sonnet-4-20250514".into(),
            max_tokens: 100,
            system: String::new(),
            messages: vec![serde_json::json!({"role": "user", "content": "hi"})],
            tools: vec![],
            stream: true,
            extra: Default::default(),
        };

        let handle_path = client
            .write_typed(&path!("gateway/completions"), &request)
            .await
            .unwrap();

        // The returned path must be outstanding/0.
        assert!(
            handle_path.to_string().contains("outstanding"),
            "expected outstanding/N path, got: {handle_path}"
        );

        // Wait briefly for the dispatch task to complete.
        tokio::time::sleep(Duration::from_millis(200)).await;

        // A usage record should have been appended — the usage store returns
        // the full ledger as an array at its root.
        let usage_records: Vec<crate::UsageRecord> = client
            .read_typed(&path!("gateway/usage"))
            .await
            .unwrap()
            .unwrap_or_default();
        assert_eq!(
            usage_records.len(),
            1,
            "expected 1 usage record after dispatch, got: {usage_records:?}",
        );
        assert_eq!(usage_records[0].output_tokens, 1);
        assert_eq!(usage_records[0].account, "anthropic");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn write_null_to_outstanding_gc_removes_handle() {
        // Directly exercise the GC path on the AsyncWriter without going
        // through the broker: construct the store and call write() directly.
        let broker = build_substrate().await;
        let client = broker.client();
        let usage_writer = client.scoped("gateway/usage");
        let executor = Arc::new(MockSseExecutor::new());
        let upstream_store =
            crate::upstream_store::UpstreamStore::new(executor, tokio::runtime::Handle::current());
        broker.mount_async(oxpath!("upstream"), upstream_store).await;

        let mut store = CompletionBrokerStore::new(
            client.clone(),
            client.scoped("upstream"),
            usage_writer,
            tokio::runtime::Handle::current(),
        );

        // Insert a dummy handle by writing a request.
        let request = CompletionRequest {
            model: "anthropic/claude-sonnet-4-20250514".into(),
            max_tokens: 1,
            system: String::new(),
            messages: vec![],
            tools: vec![],
            stream: true,
            extra: Default::default(),
        };
        let value = to_value(&request).unwrap();
        let root = path!("");
        let handle_path = store.write(&root, Record::parsed(value)).await.unwrap();
        assert_eq!(store.handles.len(), 1, "handle should be inserted");

        // GC: write null to the handle path.
        let gc_result = store
            .write(&handle_path, Record::parsed(Value::Null))
            .await
            .unwrap();
        assert_eq!(gc_result, handle_path);
        assert_eq!(store.handles.len(), 0, "handle should be removed after GC");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use structfs_core_store::path;

    #[test]
    fn parse_handle_path_basic() {
        assert_eq!(
            CompletionBrokerStore::parse_handle_path(
                &path!("outstanding/42")
            ),
            Some((42, None))
        );
    }

    #[test]
    fn parse_handle_path_with_subpath() {
        assert_eq!(
            CompletionBrokerStore::parse_handle_path(
                &path!("outstanding/7/events/from/3")
            ),
            Some((7, Some("events/from/3".into())))
        );
    }

    #[test]
    fn parse_handle_path_root_returns_none() {
        assert_eq!(
            CompletionBrokerStore::parse_handle_path(
                &path!("")
            ),
            None
        );
    }

    #[test]
    fn parse_handle_path_outstanding_only_returns_none() {
        assert_eq!(
            CompletionBrokerStore::parse_handle_path(
                &path!("outstanding")
            ),
            None
        );
    }

    #[test]
    fn parse_handle_path_nonnumeric_id_returns_none() {
        assert_eq!(
            CompletionBrokerStore::parse_handle_path(
                &path!("outstanding/abc")
            ),
            None
        );
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use crate::completion_broker::mock::MockSseExecutor;
    use ox_broker::BrokerStore;
    use ox_kernel::CompletionRequest;
    use ox_path::oxpath;
    use ox_store_util::StoreBacking;
    use ox_types::StreamEvent;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use structfs_core_store::{Error as StoreError, Value, path};
    use structfs_serde_store::to_value;

    /// Minimal in-memory `StoreBacking` for the UsageStore in tests.
    struct MemoryBacking {
        items: Mutex<Vec<Value>>,
    }

    impl MemoryBacking {
        fn new() -> Self {
            Self {
                items: Mutex::new(Vec::new()),
            }
        }
    }

    impl StoreBacking for MemoryBacking {
        fn load(&self) -> Result<Option<Value>, StoreError> {
            let items = self.items.lock().unwrap();
            if items.is_empty() {
                Ok(None)
            } else {
                Ok(Some(Value::Array(items.clone())))
            }
        }

        fn save(&self, value: &Value) -> Result<(), StoreError> {
            let mut items = self.items.lock().unwrap();
            *items = match value {
                Value::Array(a) => a.clone(),
                other => vec![other.clone()],
            };
            Ok(())
        }

        fn append(&self, item: &Value) -> Result<(), StoreError> {
            self.items.lock().unwrap().push(item.clone());
            Ok(())
        }
    }

    /// Stand up a broker seeded with gate + secret mounts for the named role.
    async fn build_substrate_with_role(role_name: &str) -> BrokerStore {
        use crate::{AccountConfig, ApiKey, ProviderConfig};
        use ox_store_util::LocalConfig;
        use ox_types::CompletionRole;

        let broker = BrokerStore::new(Duration::from_secs(5));

        let mut gate_config = LocalConfig::new();
        gate_config.set(
            &format!("gate/completions/{role_name}"),
            to_value(&CompletionRole {
                account: "anthropic".into(),
                model_id: "claude-sonnet-4-20250514".into(),
            })
            .unwrap(),
        );
        gate_config.set(
            "gate/accounts/anthropic",
            to_value(&AccountConfig {
                provider: "anthropic".into(),
                ..Default::default()
            })
            .unwrap(),
        );
        gate_config.set(
            "gate/providers/anthropic",
            to_value(&ProviderConfig::anthropic()).unwrap(),
        );
        broker.mount(path!(""), gate_config).await;

        let mut secret = LocalConfig::new();
        secret.set("keys/anthropic", to_value(&ApiKey::new("sk-test")).unwrap());
        broker.mount(oxpath!("secret"), secret).await;

        broker
    }

    /// Mount UsageStore and CompletionBrokerStore on the broker, returning the
    /// client ready for test use.
    async fn mount_completion_store(
        broker: &BrokerStore,
        executor: Arc<MockSseExecutor>,
    ) -> ox_broker::ClientHandle {
        let usage_backing = Box::new(MemoryBacking::new());
        let usage_store = crate::UsageStore::new(usage_backing);
        broker.mount(oxpath!("gateway", "usage"), usage_store).await;

        let client = broker.client();
        let substrate = client.clone();
        let usage_writer = client.scoped("gateway/usage");

        let upstream_store =
            crate::upstream_store::UpstreamStore::new(executor, tokio::runtime::Handle::current());
        broker.mount_async(oxpath!("upstream"), upstream_store).await;
        let store = CompletionBrokerStore::new(
            substrate,
            client.scoped("upstream"),
            usage_writer,
            tokio::runtime::Handle::current(),
        );
        broker
            .mount_async(oxpath!("gateway", "completions"), store)
            .await;

        client
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn keyless_account_with_auth_none_completes() {
        use crate::{AccountConfig, AuthScheme, ProviderConfig};
        use ox_store_util::LocalConfig;
        use ox_types::CompletionRole;

        // LM Studio-shaped config: openai dialect, auth explicitly none,
        // and NO entry in the secrets mount.
        let broker = BrokerStore::new(Duration::from_secs(5));
        let mut gate_config = LocalConfig::new();
        gate_config.set(
            "gate/completions/primary",
            to_value(&CompletionRole {
                account: "lmstudio".into(),
                model_id: "qwen".into(),
            })
            .unwrap(),
        );
        gate_config.set(
            "gate/accounts/lmstudio",
            to_value(&AccountConfig {
                provider: "lmstudio".into(),
                ..Default::default()
            })
            .unwrap(),
        );
        gate_config.set(
            "gate/providers/lmstudio",
            to_value(&ProviderConfig {
                dialect: "openai".into(),
                endpoint: "http://127.0.0.1:1234".into(),
                version: String::new(),
                auth: Some(AuthScheme::None),
            })
            .unwrap(),
        );
        broker.mount(path!(""), gate_config).await;
        broker.mount(oxpath!("secret"), LocalConfig::new()).await;

        let executor = Arc::new(MockSseExecutor::new());
        executor.push_immediate(StreamEvent::TextDelta { text: "hi".into() });
        executor.push_immediate(StreamEvent::MessageStop);
        let client = mount_completion_store(&broker, executor).await;

        let req = CompletionRequest {
            model: "primary".into(),
            max_tokens: 100,
            system: String::new(),
            messages: vec![serde_json::json!({"role": "user", "content": "hi"})],
            tools: vec![],
            stream: true,
            extra: Default::default(),
        };
        let handle = client
            .write_typed(&path!("gateway/completions"), &req)
            .await
            .expect("write completion request");
        let handle_path = path!("gateway/completions").join(&handle);

        let mut status = None;
        for _ in 0..50 {
            let s: Option<CompletionStatus> =
                client.read_typed(&handle_path).await.expect("status read");
            if s.as_ref().is_some_and(|s| s.is_terminal()) {
                status = s;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        match status {
            Some(CompletionStatus::Complete { .. }) => {}
            other => panic!("keyless account should complete, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn drains_all_events_to_terminal_via_blocking_read() {
        let broker = build_substrate_with_role("primary").await;

        let executor = Arc::new(MockSseExecutor::new());
        executor.push_immediate(StreamEvent::InputUsage {
            input_tokens: 10,
            cache_creation: 0,
            cache_read: 0,
        });
        executor.push_immediate(StreamEvent::TextDelta { text: "Hello".into() });
        executor.push_immediate(StreamEvent::OutputUsage { output_tokens: 1 });
        executor.push_immediate(StreamEvent::MessageStop);

        let client = mount_completion_store(&broker, executor).await;

        let request = CompletionRequest {
            model: "anthropic/claude-sonnet-4-20250514".into(),
            max_tokens: 100,
            system: String::new(),
            messages: vec![serde_json::json!({"role": "user", "content": "hi"})],
            tools: vec![],
            stream: true,
            extra: Default::default(),
        };

        // write_typed returns the store-relative path (e.g. "outstanding/0").
        // Prefix with the mount point to get the broker-absolute path for reads.
        let handle_rel = client
            .write_typed(&path!("gateway/completions"), &request)
            .await
            .unwrap();
        let mount = path!("gateway/completions");
        let handle_path = mount.join(&handle_rel);

        // Drain events using the blocking events/from/{S} read, polling
        // until the handle reports a terminal status.
        let mut next = 0usize;
        let mut all_events: Vec<StreamEvent> = Vec::new();
        loop {
            let events_sub = Path::parse(&format!("events/from/{next}")).unwrap();
            let events_path = handle_path.join(&events_sub);
            let batch: Vec<StreamEvent> = client
                .read_typed(&events_path)
                .await
                .unwrap()
                .unwrap_or_default();
            next += batch.len();
            all_events.extend(batch);

            let status: CompletionStatus = client
                .read_typed(&handle_path)
                .await
                .unwrap()
                .unwrap();
            if status.is_terminal() {
                break;
            }
        }

        assert_eq!(all_events.len(), 4, "expected 4 events, got: {all_events:?}");
        assert!(
            matches!(all_events.last().unwrap(), StreamEvent::MessageStop),
            "last event should be MessageStop"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn named_role_resolves_model_id_at_executor() {
        let broker = build_substrate_with_role("fast").await;

        let executor = Arc::new(MockSseExecutor::new());
        executor.push_immediate(StreamEvent::TextDelta { text: "ok".into() });
        executor.push_immediate(StreamEvent::MessageStop);

        let client = mount_completion_store(&broker, executor.clone()).await;

        let request = CompletionRequest {
            model: "fast".into(),
            max_tokens: 50,
            system: String::new(),
            messages: vec![],
            tools: vec![],
            stream: true,
            extra: Default::default(),
        };

        // write_typed returns the store-relative path; prefix with mount for reads.
        let handle_rel = client
            .write_typed(&path!("gateway/completions"), &request)
            .await
            .unwrap();
        let mount = path!("gateway/completions");
        let handle_path = mount.join(&handle_rel);

        // Drain to terminal so the dispatch task finishes.
        let mut next = 0usize;
        loop {
            let events_sub = Path::parse(&format!("events/from/{next}")).unwrap();
            let events_path = handle_path.join(&events_sub);
            let batch: Vec<StreamEvent> = client
                .read_typed(&events_path)
                .await
                .unwrap()
                .unwrap_or_default();
            next += batch.len();

            let status: CompletionStatus = client
                .read_typed(&handle_path)
                .await
                .unwrap()
                .unwrap();
            if status.is_terminal() {
                break;
            }
        }

        // The request that reached the executor must carry the upstream model
        // id, not the role alias "fast".
        let seen = executor.requests_seen();
        assert_eq!(seen.len(), 1, "expected exactly one upstream request");
        if let Some(body) = &seen[0].body {
            assert_eq!(
                body["model"], "claude-sonnet-4-20250514",
                "model id should be rewritten to upstream id"
            );
        }
    }
}
