#![cfg(unix)]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use ox_broker::async_store::{AsyncReader, AsyncWriter, BoxFuture};
use ox_structfs_transport::{
    CodecLimits, ExportRoot, RemoteError, RemoteStore, RemoteStoreConfig, Request,
    RequestOperation, ResponseBody, ServerConfig, WireCodec, WireError, WireErrorCode, WireMessage,
    bridge_streams_to_unix, connect_unix, serve_stream, spawn_unix_server,
};
use structfs_core_store::{Error as StoreError, Path, Record, Value};
use tempfile::TempDir;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::sync::{Mutex, Notify, Semaphore};

#[derive(Clone)]
struct TestStore {
    state: Arc<TestState>,
}

struct TestState {
    records: Mutex<HashMap<String, Record>>,
    long_started: AtomicBool,
    long_started_notify: Notify,
    long_gate: Semaphore,
    long_completed: AtomicBool,
    long_invocations: AtomicUsize,
}

impl TestState {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            records: Mutex::new(HashMap::from([
                (
                    "public/fast".into(),
                    Record::parsed(Value::String("fast".into())),
                ),
                (
                    "public/slow".into(),
                    Record::parsed(Value::String("slow".into())),
                ),
                (
                    "private/value".into(),
                    Record::parsed(Value::String("secret".into())),
                ),
            ])),
            long_started: AtomicBool::new(false),
            long_started_notify: Notify::new(),
            long_gate: Semaphore::new(0),
            long_completed: AtomicBool::new(false),
            long_invocations: AtomicUsize::new(0),
        })
    }

    async fn wait_for_long_start(&self) {
        loop {
            let notified = self.long_started_notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.long_started.load(Ordering::Acquire) {
                return;
            }
            notified.await;
        }
    }

    fn release_long(&self) {
        self.long_gate.add_permits(1);
    }
}

impl AsyncReader for TestStore {
    fn read(&mut self, from: &Path) -> BoxFuture<Result<Option<Record>, StoreError>> {
        let state = self.state.clone();
        let path = from.to_string();
        Box::pin(async move {
            if path.ends_with("/conflict") {
                return Err(StoreError::store("test", "read", "revision mismatch"));
            }
            if path.ends_with("/slow") {
                tokio::time::sleep(Duration::from_millis(140)).await;
            }
            Ok(state.records.lock().await.get(&path).cloned())
        })
    }
}

impl AsyncWriter for TestStore {
    fn write(&mut self, to: &Path, data: Record) -> BoxFuture<Result<Path, StoreError>> {
        let state = self.state.clone();
        let path = to.clone();
        Box::pin(async move {
            if path.to_string().ends_with("/long") {
                state.long_invocations.fetch_add(1, Ordering::AcqRel);
                state.long_started.store(true, Ordering::Release);
                state.long_started_notify.notify_waiters();
                let permit =
                    state.long_gate.acquire().await.map_err(|_| {
                        StoreError::store("test", "write", "long-write gate closed")
                    })?;
                permit.forget();
                state.long_completed.store(true, Ordering::Release);
            }
            state.records.lock().await.insert(path.to_string(), data);

            if path.to_string().ends_with("/escape") {
                return Ok(Path::parse("private/value").unwrap());
            }
            if path.to_string().ends_with("/collision") {
                return Ok(Path::parse("publicity/value").unwrap());
            }
            if path.to_string().ends_with("/exact") {
                return Ok(Path::parse("public").unwrap());
            }
            Ok(path)
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    InProcess,
    Duplex,
    Unix,
    Stdio,
}

#[derive(Clone)]
enum Client {
    Direct(ExportRoot<TestStore>),
    Remote(RemoteStore),
}

#[derive(Debug)]
enum ClientError {
    Direct,
    Remote(RemoteError),
}

impl Client {
    async fn read(&self, path: &str) -> Result<Option<Record>, ClientError> {
        let path = Path::parse(path).unwrap();
        match self {
            Self::Direct(root) => root.read(path).await.map_err(|_| ClientError::Direct),
            Self::Remote(remote) => remote.read_remote(&path).await.map_err(ClientError::Remote),
        }
    }

    async fn write(&self, path: &str, value: &str) -> Result<Path, ClientError> {
        let path = Path::parse(path).unwrap();
        let record = Record::parsed(Value::String(value.into()));
        match self {
            Self::Direct(root) => root
                .write(path, record)
                .await
                .map_err(|_| ClientError::Direct),
            Self::Remote(remote) => remote
                .write_remote(&path, record)
                .await
                .map_err(ClientError::Remote),
        }
    }
}

struct Harness {
    client: Client,
    state: Arc<TestState>,
    _temp: Option<TempDir>,
    _server: Option<ox_structfs_transport::UnixServer>,
}

impl Harness {
    async fn start(mode: Mode, client_config: RemoteStoreConfig) -> Self {
        let state = TestState::new();
        let root = ExportRoot::new(
            TestStore {
                state: state.clone(),
            },
            Path::parse("public").unwrap(),
        );
        let server_config = ServerConfig::default();
        match mode {
            Mode::InProcess => Self {
                client: Client::Direct(root),
                state,
                _temp: None,
                _server: None,
            },
            Mode::Duplex => {
                let (client_stream, server_stream) = tokio::io::duplex(64 * 1024);
                tokio::spawn(async move {
                    serve_stream(server_stream, root, server_config)
                        .await
                        .unwrap();
                });
                Self {
                    client: Client::Remote(RemoteStore::connect(client_stream, client_config)),
                    state,
                    _temp: None,
                    _server: None,
                }
            }
            Mode::Unix | Mode::Stdio => {
                let temp = tempfile::tempdir().unwrap();
                let socket_path = temp.path().join("store.sock");
                let server = spawn_unix_server(&socket_path, root, server_config).unwrap();
                let remote = if mode == Mode::Unix {
                    connect_unix(&socket_path, client_config).await.unwrap()
                } else {
                    let (client_stream, bridge_stream) = tokio::io::duplex(64 * 1024);
                    let (bridge_input, bridge_output) = tokio::io::split(bridge_stream);
                    let bridge_socket = socket_path.clone();
                    tokio::spawn(async move {
                        bridge_streams_to_unix(bridge_input, bridge_output, bridge_socket)
                            .await
                            .unwrap();
                    });
                    RemoteStore::connect(client_stream, client_config)
                };
                Self {
                    client: Client::Remote(remote),
                    state,
                    _temp: Some(temp),
                    _server: Some(server),
                }
            }
        }
    }
}

fn value(record: Option<Record>) -> Option<Value> {
    record.and_then(|record| record.as_value().cloned())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_conformance_suite_covers_all_local_carriers() {
    for mode in [Mode::InProcess, Mode::Duplex, Mode::Unix, Mode::Stdio] {
        let harness = Harness::start(mode, RemoteStoreConfig::default()).await;

        // The direct Store and every byte carrier preserve ordinary semantics.
        assert_eq!(
            value(harness.client.read("fast").await.unwrap()),
            Some(Value::String("fast".into())),
            "{mode:?}"
        );
        let written = harness.client.write("nested/value", "ok").await.unwrap();
        assert_eq!(written, Path::parse("nested/value").unwrap(), "{mode:?}");

        // A slow response cannot head-of-line block a later request.
        let slow_client = harness.client.clone();
        let slow = tokio::spawn(async move { slow_client.read("slow").await });
        tokio::time::sleep(Duration::from_millis(15)).await;
        let fast = tokio::time::timeout(Duration::from_millis(70), harness.client.read("fast"))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(value(fast), Some(Value::String("fast".into())), "{mode:?}");
        assert_eq!(
            value(slow.await.unwrap().unwrap()),
            Some(Value::String("slow".into())),
            "{mode:?}"
        );

        // The supplied root is the complete authority. A sibling secret is
        // not reachable, and returned paths must remain under the root.
        assert!(
            harness
                .client
                .read("private/value")
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            harness.client.write("exact", "ok").await.unwrap(),
            Path::parse("").unwrap(),
            "root itself is a valid returned write path for {mode:?}"
        );
        assert!(
            harness.client.write("escape", "no").await.is_err(),
            "{mode:?}"
        );
        assert!(
            harness.client.write("collision", "no").await.is_err(),
            "prefix collisions must not pass confinement for {mode:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn timeout_discards_late_response_for_every_byte_carrier() {
    for mode in [Mode::Duplex, Mode::Unix, Mode::Stdio] {
        let config = RemoteStoreConfig {
            request_timeout: Duration::from_millis(45),
            ..RemoteStoreConfig::default()
        };
        let harness = Harness::start(mode, config).await;
        assert!(matches!(
            harness.client.read("slow").await,
            Err(ClientError::Remote(RemoteError::DeadlineExceeded))
        ));
        tokio::time::sleep(Duration::from_millis(130)).await;
        assert_eq!(
            value(harness.client.read("fast").await.unwrap()),
            Some(Value::String("fast".into())),
            "a late response corrupted correlation for {mode:?}"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bounded_admission_reports_overload_for_every_byte_carrier() {
    for mode in [Mode::Duplex, Mode::Unix, Mode::Stdio] {
        let config = RemoteStoreConfig {
            max_inflight_requests: 1,
            request_timeout: Duration::from_secs(2),
            ..RemoteStoreConfig::default()
        };
        let harness = Harness::start(mode, config).await;
        let long_client = harness.client.clone();
        let long = tokio::spawn(async move { long_client.write("long", "eventual").await });
        harness.state.wait_for_long_start().await;
        assert!(matches!(
            harness.client.read("fast").await,
            Err(ClientError::Remote(RemoteError::Overloaded))
        ));
        harness.state.release_long();
        long.await.unwrap().unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn disconnect_detaches_but_does_not_cancel_admitted_writes() {
    for mode in [Mode::Duplex, Mode::Unix, Mode::Stdio] {
        let harness = Harness::start(mode, RemoteStoreConfig::default()).await;
        let long_client = harness.client.clone();
        let long = tokio::spawn(async move { long_client.write("long", "eventual").await });
        harness.state.wait_for_long_start().await;

        // Drop all user-side handles and the pending request future. This
        // closes the channel/bridge but not the already-admitted Store future.
        drop(harness.client);
        long.abort();
        harness.state.release_long();
        tokio::time::timeout(Duration::from_secs(1), async {
            while !harness.state.long_completed.load(Ordering::Acquire) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("admitted write was cancelled by {mode:?} disconnect"));
        assert_eq!(
            harness.state.long_invocations.load(Ordering::Acquire),
            1,
            "transport retried an ambiguous write for {mode:?}"
        );
    }
}

#[tokio::test]
async fn disconnect_failure_is_stable_and_writes_are_not_retried() {
    let (client_stream, server_stream) = tokio::io::duplex(1024);
    drop(server_stream);
    let remote = RemoteStore::connect(client_stream, RemoteStoreConfig::default());
    tokio::task::yield_now().await;
    let first = remote
        .read_remote(&Path::parse("a").unwrap())
        .await
        .unwrap_err();
    let second = remote
        .read_remote(&Path::parse("b").unwrap())
        .await
        .unwrap_err();
    assert_eq!(first, second);
    assert!(matches!(first, RemoteError::Disconnected(_)));
}

#[tokio::test]
async fn cancelling_request_future_removes_its_correlation_entry() {
    let (client_stream, _server_stream) = tokio::io::duplex(4096);
    let remote = RemoteStore::connect(
        client_stream,
        RemoteStoreConfig {
            request_timeout: Duration::from_secs(10),
            ..RemoteStoreConfig::default()
        },
    );
    let pending_remote = remote.clone();
    let pending = tokio::spawn(async move {
        pending_remote
            .read_remote(&Path::parse("never").unwrap())
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        while remote.inflight_requests() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    pending.abort();
    let _ = pending.await;
    assert_eq!(remote.inflight_requests(), 0);
}

struct WriteBlockedStream {
    write_polled: Arc<AtomicBool>,
    write_notify: Arc<Notify>,
}

impl AsyncRead for WriteBlockedStream {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        _context: &mut Context<'_>,
        _buffer: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Poll::Pending
    }
}

impl AsyncWrite for WriteBlockedStream {
    fn poll_write(
        self: std::pin::Pin<&mut Self>,
        _context: &mut Context<'_>,
        _buffer: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        self.write_polled.store(true, Ordering::Release);
        self.write_notify.notify_waiters();
        Poll::Pending
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _context: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _context: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn bounded_send_queue_rejects_without_waiting_or_growing_pending() {
    let write_polled = Arc::new(AtomicBool::new(false));
    let write_notify = Arc::new(Notify::new());
    let remote = RemoteStore::connect(
        WriteBlockedStream {
            write_polled: write_polled.clone(),
            write_notify: write_notify.clone(),
        },
        RemoteStoreConfig {
            send_queue_capacity: 1,
            max_inflight_requests: 10,
            request_timeout: Duration::from_secs(10),
            ..RemoteStoreConfig::default()
        },
    );
    let first_remote = remote.clone();
    let first = tokio::spawn(async move {
        first_remote
            .read_remote(&Path::parse("first").unwrap())
            .await
    });
    loop {
        let notified = write_notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if write_polled.load(Ordering::Acquire) {
            break;
        }
        notified.await;
    }

    let second_remote = remote.clone();
    let second = tokio::spawn(async move {
        second_remote
            .read_remote(&Path::parse("second").unwrap())
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        while remote.inflight_requests() != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        remote
            .read_remote(&Path::parse("third").unwrap())
            .await
            .unwrap_err(),
        RemoteError::Overloaded
    );
    assert_eq!(remote.inflight_requests(), 2);
    first.abort();
    second.abort();
    let _ = first.await;
    let _ = second.await;
    assert_eq!(remote.inflight_requests(), 0);
}

#[tokio::test]
async fn server_error_mapper_preserves_domain_categories() {
    let state = TestState::new();
    let root = ExportRoot::new(TestStore { state }, Path::parse("public").unwrap());
    let server_config = ServerConfig::default().with_error_mapper(Arc::new(|error| WireError {
        code: if error.to_string().contains("revision mismatch") {
            WireErrorCode::Conflict
        } else {
            WireErrorCode::Store
        },
        message: error.to_string(),
    }));
    let (client_stream, server_stream) = tokio::io::duplex(4096);
    tokio::spawn(async move {
        serve_stream(server_stream, root, server_config)
            .await
            .unwrap();
    });
    let remote = RemoteStore::connect(client_stream, RemoteStoreConfig::default());
    assert!(matches!(
        remote.read_remote(&Path::parse("conflict").unwrap()).await,
        Err(RemoteError::Wire {
            code: WireErrorCode::Conflict,
            ..
        })
    ));
}

#[tokio::test]
async fn local_or_remote_oversize_is_per_request_not_connection_fatal() {
    let state = TestState::new();
    state.records.lock().await.insert(
        "public/huge".into(),
        Record::parsed(Value::String("x".repeat(1024))),
    );
    let root = ExportRoot::new(TestStore { state }, Path::parse("public").unwrap());
    let codec = WireCodec::new(CodecLimits {
        max_frame_bytes: 128,
        ..CodecLimits::default()
    });
    let (client_stream, server_stream) = tokio::io::duplex(4096);
    let server_config = ServerConfig::default().with_codec(codec.clone());
    tokio::spawn(async move {
        serve_stream(server_stream, root, server_config)
            .await
            .unwrap();
    });
    let remote = RemoteStore::connect(
        client_stream,
        RemoteStoreConfig {
            codec,
            ..RemoteStoreConfig::default()
        },
    );

    assert!(matches!(
        remote
            .write_remote(
                &Path::parse("oversize").unwrap(),
                Record::parsed(Value::String("x".repeat(1024))),
            )
            .await,
        Err(RemoteError::InvalidRequest(_))
    ));
    assert!(matches!(
        remote.read_remote(&Path::parse("huge").unwrap()).await,
        Err(RemoteError::Wire {
            code: WireErrorCode::ResourceLimit,
            ..
        })
    ));
    assert_eq!(
        value(
            remote
                .read_remote(&Path::parse("fast").unwrap())
                .await
                .unwrap()
        ),
        Some(Value::String("fast".into()))
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stdio_input_half_close_still_drains_the_response() {
    let state = TestState::new();
    let root = ExportRoot::new(TestStore { state }, Path::parse("public").unwrap());
    let temp = tempfile::tempdir().unwrap();
    let socket_path = temp.path().join("store.sock");
    let _server = spawn_unix_server(&socket_path, root, ServerConfig::default()).unwrap();
    let (client, bridge) = tokio::io::duplex(4096);
    let (bridge_input, bridge_output) = tokio::io::split(bridge);
    let bridge_socket = socket_path.clone();
    tokio::spawn(async move {
        bridge_streams_to_unix(bridge_input, bridge_output, bridge_socket)
            .await
            .unwrap();
    });

    let codec = WireCodec::default();
    let request = WireMessage::Request(Request {
        request_id: 91,
        operation: RequestOperation::Read,
        path: Path::parse("fast").unwrap(),
        deadline_unix_ms: None,
    });
    let (mut reader, mut writer) = tokio::io::split(client);
    writer
        .write_all(&codec.encode(&request).unwrap())
        .await
        .unwrap();
    writer.shutdown().await.unwrap();
    let mut response = Vec::new();
    tokio::time::timeout(Duration::from_secs(1), reader.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    let WireMessage::Response(response) = codec.decode(&response).unwrap() else {
        panic!("expected response")
    };
    assert_eq!(response.request_id, 91);
    let ResponseBody::Read(record) = response.result.unwrap() else {
        panic!("expected read response")
    };
    assert_eq!(value(record), Some(Value::String("fast".into())));
}
