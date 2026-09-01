//! Generic, carrier-independent StructFS wire protocol.
//!
//! Wire v1 is a four-byte big-endian length prefix followed by deterministic
//! CBOR. This crate deliberately contains no ox conversation, worker, or
//! remote-node concepts; stream multiplexing and concrete carriers build on
//! this sans-I/O codec.
//!
//! The normative byte layout is documented in `WIRE.md` at the crate root.

mod client;
mod codec;
mod error;
mod frame;
mod io;
mod root;
mod server;
#[cfg(unix)]
mod ssh;

#[cfg(unix)]
mod unix;

pub use client::{RemoteError, RemoteStore, RemoteStoreConfig};
pub use codec::{
    Request, RequestOperation, Response, ResponseBody, WireError, WireErrorCode, WireMessage,
};
pub use error::CodecError;
pub use frame::{CodecLimits, WIRE_VERSION, WireCodec};
pub use root::ExportRoot;
pub use server::{ErrorMapper, ServerConfig, connect_in_process, serve_stream};
#[cfg(unix)]
pub use ssh::{
    HostKeyEnrollment, IdentityFileError, KnownHosts, KnownHostsError, SshConnectError,
    WorkerSshConfig, connect_worker_ssh, load_private_identity,
};

#[cfg(unix)]
pub use unix::{
    UnixServer, bridge_stdio_to_unix, bridge_streams_to_unix, connect_unix, spawn_unix_server,
};
