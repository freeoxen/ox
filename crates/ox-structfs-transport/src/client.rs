use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use ox_broker::async_store::{
    AsyncReader as BrokerAsyncReader, AsyncWriter as BrokerAsyncWriter, BoxFuture,
};
use structfs_core_store::{AsyncReader, AsyncWriter, Error as StoreError, Path, Record};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Semaphore, mpsc, oneshot};

use crate::io::{StreamError, read_message, write_frame};
use crate::{
    Request, RequestOperation, ResponseBody, WireCodec, WireError, WireErrorCode, WireMessage,
};

#[derive(Clone, Debug)]
pub struct RemoteStoreConfig {
    pub codec: WireCodec,
    pub send_queue_capacity: usize,
    pub max_inflight_requests: usize,
    pub request_timeout: Duration,
}

impl Default for RemoteStoreConfig {
    fn default() -> Self {
        Self {
            codec: WireCodec::default(),
            send_queue_capacity: 64,
            max_inflight_requests: 256,
            request_timeout: Duration::from_secs(30),
        }
    }
}

/// Typed failures available to callers that use [`RemoteStore::read_remote`]
/// and [`RemoteStore::write_remote`] directly.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum RemoteError {
    #[error("request queue is saturated")]
    Overloaded,
    #[error("request deadline exceeded")]
    DeadlineExceeded,
    #[error("transport disconnected: {0}")]
    Disconnected(String),
    #[error("transport protocol failure: {0}")]
    Protocol(String),
    #[error("request cannot be encoded: {0}")]
    InvalidRequest(String),
    #[error("remote Store error ({code:?}): {message}")]
    Wire {
        code: WireErrorCode,
        message: String,
    },
}

impl From<WireError> for RemoteError {
    fn from(error: WireError) -> Self {
        Self::Wire {
            code: error.code,
            message: error.message,
        }
    }
}

impl RemoteError {
    fn into_store_error(self, operation: &'static str) -> StoreError {
        StoreError::store("remote_store", operation, self.to_string())
    }
}

struct Outbound {
    frame: Vec<u8>,
}

struct ClientState {
    disconnected: Option<RemoteError>,
    pending: HashMap<u64, oneshot::Sender<Result<ResponseBody, RemoteError>>>,
}

struct ClientInner {
    state: Mutex<ClientState>,
    outbound: mpsc::Sender<Outbound>,
    next_request_id: AtomicU64,
    admission: Arc<Semaphore>,
    request_timeout: Duration,
    codec: WireCodec,
}

struct PendingGuard {
    inner: Arc<ClientInner>,
    request_id: u64,
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        lock_state(&self.inner).pending.remove(&self.request_id);
    }
}

/// A multiplexed StructFS Store client over one bidirectional async stream.
///
/// Clones share the stream and request-ID space. Requests are never retried;
/// after an ambiguous write timeout or disconnect, the caller must reconcile
/// using its semantic idempotency key.
#[derive(Clone)]
pub struct RemoteStore {
    inner: Arc<ClientInner>,
}

impl RemoteStore {
    pub fn connect<T>(stream: T, config: RemoteStoreConfig) -> Self
    where
        T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        assert!(
            config.send_queue_capacity > 0,
            "send queue must be non-zero"
        );
        assert!(
            config.max_inflight_requests > 0,
            "inflight limit must be non-zero"
        );

        let (outbound, receiver) = mpsc::channel(config.send_queue_capacity);
        let inner = Arc::new(ClientInner {
            state: Mutex::new(ClientState {
                disconnected: None,
                pending: HashMap::new(),
            }),
            outbound,
            next_request_id: AtomicU64::new(1),
            admission: Arc::new(Semaphore::new(config.max_inflight_requests)),
            request_timeout: config.request_timeout,
            codec: config.codec.clone(),
        });
        let (reader, writer) = tokio::io::split(stream);
        tokio::spawn(writer_loop(writer, receiver, Arc::downgrade(&inner)));
        tokio::spawn(reader_loop(reader, config.codec, Arc::downgrade(&inner)));
        Self { inner }
    }

    pub async fn read_remote(&self, path: &Path) -> Result<Option<Record>, RemoteError> {
        let body = self.request(path.clone(), RequestOperation::Read).await?;
        match body {
            ResponseBody::Read(record) => Ok(record),
            ResponseBody::Write(_) => Err(RemoteError::Protocol(
                "write response received for read request".into(),
            )),
        }
    }

    pub async fn write_remote(&self, path: &Path, data: Record) -> Result<Path, RemoteError> {
        let body = self
            .request(path.clone(), RequestOperation::Write(data))
            .await?;
        match body {
            ResponseBody::Write(path) => Ok(path),
            ResponseBody::Read(_) => Err(RemoteError::Protocol(
                "read response received for write request".into(),
            )),
        }
    }

    /// Current correlation-table size, useful for capacity telemetry.
    pub fn inflight_requests(&self) -> usize {
        lock_state(&self.inner).pending.len()
    }

    async fn request(
        &self,
        path: Path,
        operation: RequestOperation,
    ) -> Result<ResponseBody, RemoteError> {
        let permit = self
            .inner
            .admission
            .clone()
            .try_acquire_owned()
            .map_err(|_| RemoteError::Overloaded)?;
        let request_id = self.inner.next_request_id.fetch_add(1, Ordering::Relaxed);
        let deadline_unix_ms = unix_millis_after(self.inner.request_timeout);
        let (reply, receiver) = oneshot::channel();
        let request = Request {
            request_id,
            operation,
            path,
            deadline_unix_ms: Some(deadline_unix_ms),
        };
        let frame = self
            .inner
            .codec
            .encode(&WireMessage::Request(request))
            .map_err(|error| RemoteError::InvalidRequest(error.to_string()))?;

        {
            let mut state = lock_state(&self.inner);
            if let Some(error) = &state.disconnected {
                return Err(error.clone());
            }
            match state.pending.entry(request_id) {
                Entry::Vacant(entry) => {
                    entry.insert(reply);
                }
                Entry::Occupied(_) => {
                    return Err(RemoteError::Protocol("request ID collision".into()));
                }
            }
        }
        let _pending_guard = PendingGuard {
            inner: self.inner.clone(),
            request_id,
        };

        let outbound = Outbound { frame };
        if let Err(error) = self.inner.outbound.try_send(outbound) {
            return Err(match error {
                mpsc::error::TrySendError::Full(_) => RemoteError::Overloaded,
                mpsc::error::TrySendError::Closed(_) => {
                    RemoteError::Disconnected("request sender is closed".into())
                }
            });
        }

        let result = match tokio::time::timeout(self.inner.request_timeout, receiver).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Err(RemoteError::Disconnected(
                "response dispatcher stopped".into(),
            )),
            Err(_) => {
                // Removing correlation state is what makes every later reply
                // for this request a harmless late response.
                Err(RemoteError::DeadlineExceeded)
            }
        };
        drop(permit);
        result
    }
}

#[async_trait]
impl AsyncReader for RemoteStore {
    async fn read_async(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        self.read_remote(from)
            .await
            .map_err(|error| error.into_store_error("read"))
    }
}

#[async_trait]
impl AsyncWriter for RemoteStore {
    async fn write_async(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        self.write_remote(to, data)
            .await
            .map_err(|error| error.into_store_error("write"))
    }
}

impl BrokerAsyncReader for RemoteStore {
    fn read(&mut self, from: &Path) -> BoxFuture<Result<Option<Record>, StoreError>> {
        let store = self.clone();
        let path = from.clone();
        Box::pin(async move {
            store
                .read_remote(&path)
                .await
                .map_err(|error| error.into_store_error("read"))
        })
    }
}

impl BrokerAsyncWriter for RemoteStore {
    fn write(&mut self, to: &Path, data: Record) -> BoxFuture<Result<Path, StoreError>> {
        let store = self.clone();
        let path = to.clone();
        Box::pin(async move {
            store
                .write_remote(&path, data)
                .await
                .map_err(|error| error.into_store_error("write"))
        })
    }
}

async fn writer_loop<W: AsyncWrite + Unpin>(
    mut writer: W,
    mut receiver: mpsc::Receiver<Outbound>,
    inner: Weak<ClientInner>,
) {
    while let Some(outbound) = receiver.recv().await {
        if let Err(error) = write_frame(&mut writer, &outbound.frame).await {
            if let Some(inner) = inner.upgrade() {
                fail_all(
                    &inner,
                    RemoteError::Disconnected(format!("stream write failed: {error}")),
                );
            }
            return;
        }
    }
    let _ = tokio::io::AsyncWriteExt::shutdown(&mut writer).await;
}

async fn reader_loop<R: AsyncRead + Unpin>(
    mut reader: R,
    codec: WireCodec,
    inner: Weak<ClientInner>,
) {
    loop {
        match read_message(&mut reader, &codec).await {
            Ok(Some(WireMessage::Response(response))) => {
                let Some(inner) = inner.upgrade() else {
                    return;
                };
                let sender = lock_state(&inner).pending.remove(&response.request_id);
                if let Some(sender) = sender {
                    let _ = sender.send(response.result.map_err(RemoteError::from));
                }
                // Missing IDs are timed-out/cancelled requests. Discard them.
            }
            Ok(Some(WireMessage::Request(_))) => {
                if let Some(inner) = inner.upgrade() {
                    fail_all(
                        &inner,
                        RemoteError::Protocol("client received a request frame".into()),
                    );
                }
                return;
            }
            Ok(None) => {
                if let Some(inner) = inner.upgrade() {
                    fail_all(
                        &inner,
                        RemoteError::Disconnected("peer closed the stream".into()),
                    );
                }
                return;
            }
            Err(StreamError::Io(error)) => {
                if let Some(inner) = inner.upgrade() {
                    fail_all(
                        &inner,
                        RemoteError::Disconnected(format!("stream read failed: {error}")),
                    );
                }
                return;
            }
            Err(StreamError::Codec(error)) => {
                if let Some(inner) = inner.upgrade() {
                    fail_all(&inner, RemoteError::Protocol(error.to_string()));
                }
                return;
            }
        }
    }
}

fn fail_all(inner: &ClientInner, error: RemoteError) {
    let pending = {
        let mut state = lock_state(inner);
        let stable = state
            .disconnected
            .get_or_insert_with(|| error.clone())
            .clone();
        let pending = std::mem::take(&mut state.pending);
        (stable, pending)
    };
    for sender in pending.1.into_values() {
        let _ = sender.send(Err(pending.0.clone()));
    }
}

fn lock_state(inner: &ClientInner) -> MutexGuard<'_, ClientState> {
    inner
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn unix_millis_after(duration: Duration) -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .saturating_add(duration)
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}
