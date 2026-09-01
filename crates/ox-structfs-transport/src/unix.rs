use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::Path as FsPath;

use ox_broker::async_store::{AsyncReader as BrokerAsyncReader, AsyncWriter as BrokerAsyncWriter};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::task::JoinHandle;

use crate::{ExportRoot, RemoteStore, RemoteStoreConfig, ServerConfig, serve_stream};

/// Accept-loop handle owned by a long-lived service. Dropping a client or a
/// stdio bridge never touches this handle.
pub struct UnixServer {
    task: Option<JoinHandle<io::Result<()>>>,
}

impl UnixServer {
    pub fn is_finished(&self) -> bool {
        self.task.as_ref().is_none_or(JoinHandle::is_finished)
    }

    pub async fn shutdown(mut self) -> io::Result<()> {
        let Some(task) = self.task.take() else {
            return Ok(());
        };
        task.abort();
        match task.await {
            Ok(result) => result,
            Err(error) if error.is_cancelled() => Ok(()),
            Err(error) => Err(io::Error::other(error.to_string())),
        }
    }
}

impl Drop for UnixServer {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

pub fn spawn_unix_server<P, S>(
    socket_path: P,
    root: ExportRoot<S>,
    config: ServerConfig,
) -> io::Result<UnixServer>
where
    P: AsRef<FsPath>,
    S: BrokerAsyncReader + BrokerAsyncWriter + Send + 'static,
{
    let socket_path = socket_path.as_ref();
    let listener = UnixListener::bind(socket_path)?;
    std::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600))?;
    let task = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await?;
            let root = root.clone();
            let config = config.clone();
            tokio::spawn(async move {
                let _ = serve_stream(stream, root, config).await;
            });
        }
    });
    Ok(UnixServer { task: Some(task) })
}

pub async fn connect_unix<P: AsRef<FsPath>>(
    socket_path: P,
    config: RemoteStoreConfig,
) -> io::Result<RemoteStore> {
    let stream = UnixStream::connect(socket_path).await?;
    Ok(RemoteStore::connect(stream, config))
}

/// Stateless byte bridge used by `ox-worker structfs-stdio`.
///
/// EOF in either direction returns and drops only this socket connection. The
/// Unix service and any Store operations it already admitted remain alive.
pub async fn bridge_streams_to_unix<R, W, P>(
    mut input: R,
    mut output: W,
    socket_path: P,
) -> io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
    P: AsRef<FsPath>,
{
    let socket = UnixStream::connect(socket_path).await?;
    let (mut socket_reader, mut socket_writer) = socket.into_split();
    let to_socket = async {
        tokio::io::copy(&mut input, &mut socket_writer).await?;
        socket_writer.shutdown().await
    };
    let from_socket = async {
        tokio::io::copy(&mut socket_reader, &mut output).await?;
        output.shutdown().await
    };
    tokio::pin!(to_socket);
    tokio::pin!(from_socket);
    tokio::select! {
        result = &mut to_socket => {
            result?;
            // Input EOF is a half-close: the server must be allowed to finish
            // admitted requests and flush their responses before detaching.
            from_socket.await
        },
        result = &mut from_socket => result,
    }
}

pub async fn bridge_stdio_to_unix<P: AsRef<FsPath>>(socket_path: P) -> io::Result<()> {
    bridge_streams_to_unix(tokio::io::stdin(), tokio::io::stdout(), socket_path).await
}
