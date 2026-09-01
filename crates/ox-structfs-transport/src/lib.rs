//! Generic, carrier-independent StructFS wire protocol.
//!
//! Wire v1 is a four-byte big-endian length prefix followed by deterministic
//! CBOR. This crate deliberately contains no ox conversation, worker, or
//! remote-node concepts; stream multiplexing and concrete carriers build on
//! this sans-I/O codec.
//!
//! The normative byte layout is documented in `WIRE.md` at the crate root.

mod codec;
mod error;
mod frame;

pub use codec::{
    Request, RequestOperation, Response, ResponseBody, WireError, WireErrorCode, WireMessage,
};
pub use error::CodecError;
pub use frame::{CodecLimits, WIRE_VERSION, WireCodec};
