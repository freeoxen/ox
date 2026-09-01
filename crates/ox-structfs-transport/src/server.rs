use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use ox_broker::async_store::{AsyncReader as BrokerAsyncReader, AsyncWriter as BrokerAsyncWriter};
use structfs_core_store::{Error as StoreError, Path};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Semaphore, mpsc};

use crate::io::{read_message, write_frame};
use crate::{
    ExportRoot, RemoteStore, RemoteStoreConfig, Request, RequestOperation, Response, ResponseBody,
    WireCodec, WireError, WireErrorCode, WireMessage,
};

pub type ErrorMapper = Arc<dyn Fn(&StoreError) -> WireError + Send + Sync>;

#[derive(Clone)]
pub struct ServerConfig {
    pub codec: WireCodec,
    pub response_queue_capacity: usize,
    pub max_inflight_requests: usize,
    error_mapper: ErrorMapper,
}

impl std::fmt::Debug for ServerConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServerConfig")
            .field("codec", &self.codec)
            .field("response_queue_capacity", &self.response_queue_capacity)
            .field("max_inflight_requests", &self.max_inflight_requests)
            .finish_non_exhaustive()
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            codec: WireCodec::default(),
            response_queue_capacity: 64,
            max_inflight_requests: 256,
            error_mapper: Arc::new(default_error_mapper),
        }
    }
}

impl ServerConfig {
    pub fn with_codec(mut self, codec: WireCodec) -> Self {
        self.codec = codec;
        self
    }

    /// Supply domain-aware mapping for Store implementations whose typed
    /// conflict/overload categories are represented inside `Error::Store`.
    pub fn with_error_mapper(mut self, mapper: ErrorMapper) -> Self {
        self.error_mapper = mapper;
        self
    }

    pub fn error_mapper(&self) -> &ErrorMapper {
        &self.error_mapper
    }
}

pub async fn serve_stream<T, S>(
    stream: T,
    root: ExportRoot<S>,
    config: ServerConfig,
) -> Result<(), String>
where
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    S: BrokerAsyncReader + BrokerAsyncWriter + Send + 'static,
{
    assert!(
        config.response_queue_capacity > 0,
        "response queue must be non-zero"
    );
    assert!(
        config.max_inflight_requests > 0,
        "inflight limit must be non-zero"
    );

    let (mut reader, mut writer) = tokio::io::split(stream);
    let (responses, mut response_receiver) =
        mpsc::channel::<Response>(config.response_queue_capacity);
    let writer_codec = config.codec.clone();
    tokio::spawn(async move {
        while let Some(response) = response_receiver.recv().await {
            let request_id = response.request_id;
            let frame = writer_codec
                .encode(&WireMessage::Response(response))
                .or_else(|_| {
                    writer_codec.encode(&WireMessage::Response(error_response(
                        request_id,
                        WireErrorCode::ResourceLimit,
                        "response exceeds configured wire limits",
                    )))
                });
            let Ok(frame) = frame else {
                return;
            };
            if write_frame(&mut writer, &frame).await.is_err() {
                return;
            }
        }
    });

    let admission = Arc::new(Semaphore::new(config.max_inflight_requests));
    loop {
        let message = read_message(&mut reader, &config.codec)
            .await
            .map_err(|error| format!("wire read failed: {error}"))?;
        let Some(message) = message else {
            // Connection lifetime owns only response delivery. Any operation
            // task already spawned below remains detached and keeps its root.
            return Ok(());
        };
        let WireMessage::Request(request) = message else {
            return Err("server received a response frame".into());
        };

        if request_is_expired(&request) {
            let _ = responses
                .send(error_response(
                    request.request_id,
                    WireErrorCode::DeadlineExceeded,
                    "request deadline had elapsed before Store invocation",
                ))
                .await;
            continue;
        }

        let Ok(permit) = admission.clone().try_acquire_owned() else {
            let _ = responses
                .send(error_response(
                    request.request_id,
                    WireErrorCode::Overloaded,
                    "server request admission is saturated",
                ))
                .await;
            continue;
        };

        let root = root.clone();
        let responses = responses.clone();
        let error_mapper = config.error_mapper.clone();
        tokio::spawn(async move {
            // There is deliberately no timeout wrapper around this future.
            // Once admitted, a deadline or connection loss may discard the
            // reply but must not cancel a possibly-effective Store write.
            let result = execute(root, request.operation, request.path).await;
            let response = Response {
                request_id: request.request_id,
                result: result.map_err(|error| error_mapper(&error)),
            };
            let _ = responses.send(response).await;
            drop(permit);
        });
    }
}

/// Convenience loopback carrier. The direct [`ExportRoot`] API remains the
/// in-process, no-wire baseline; this helper exercises the full wire over a
/// bounded Tokio duplex stream.
pub fn connect_in_process<S>(
    root: ExportRoot<S>,
    client_config: RemoteStoreConfig,
    server_config: ServerConfig,
    stream_capacity: usize,
) -> RemoteStore
where
    S: BrokerAsyncReader + BrokerAsyncWriter + Send + 'static,
{
    let (client, server) = tokio::io::duplex(stream_capacity);
    tokio::spawn(async move {
        let _ = serve_stream(server, root, server_config).await;
    });
    RemoteStore::connect(client, client_config)
}

async fn execute<S>(
    root: ExportRoot<S>,
    operation: RequestOperation,
    path: Path,
) -> Result<ResponseBody, StoreError>
where
    S: BrokerAsyncReader + BrokerAsyncWriter + Send + 'static,
{
    match operation {
        RequestOperation::Read => root.read(path).await.map(ResponseBody::Read),
        RequestOperation::Write(record) => root.write(path, record).await.map(ResponseBody::Write),
    }
}

fn request_is_expired(request: &Request) -> bool {
    request
        .deadline_unix_ms
        .is_some_and(|deadline| unix_millis_now() > deadline)
}

fn unix_millis_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn error_response(request_id: u64, code: WireErrorCode, message: &str) -> Response {
    Response {
        request_id,
        result: Err(WireError {
            code,
            message: message.into(),
        }),
    }
}

fn default_error_mapper(error: &StoreError) -> WireError {
    let code = match error {
        StoreError::Path(_) => WireErrorCode::InvalidRequest,
        StoreError::NoRoute { .. } => WireErrorCode::NotFound,
        StoreError::UnsupportedFormat(_) => WireErrorCode::Unsupported,
        StoreError::Codec { .. } => WireErrorCode::InvalidRequest,
        StoreError::Ll(_) | StoreError::Io(_) | StoreError::Store { .. } => WireErrorCode::Store,
    };
    WireError {
        code,
        message: error.to_string(),
    }
}
