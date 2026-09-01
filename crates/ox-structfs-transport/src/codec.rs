//! Public wire-domain types. Encoding lives in [`crate::frame`].

use structfs_core_store::{Path, Record};

/// One generic StructFS request.
#[derive(Clone, Debug)]
pub struct Request {
    pub request_id: u64,
    pub operation: RequestOperation,
    pub path: Path,
    /// Absolute Unix-epoch deadline in milliseconds. Absence means the caller
    /// supplied no wire deadline; carriers may still apply local limits.
    pub deadline_unix_ms: Option<u64>,
}

/// The two operations in the StructFS `Reader`/`Writer` contract.
#[derive(Clone, Debug)]
pub enum RequestOperation {
    Read,
    Write(Record),
}

/// A response correlated by request ID.
#[derive(Clone, Debug)]
pub struct Response {
    pub request_id: u64,
    pub result: Result<ResponseBody, WireError>,
}

/// Successful operation result.
#[derive(Clone, Debug)]
pub enum ResponseBody {
    /// `None` means the path is absent. `Some(Record::Parsed(Value::Null))` is
    /// a present parsed-null record and remains distinct on the wire.
    Read(Option<Record>),
    /// StructFS writes may return a path different from the requested path.
    Write(Path),
}

/// A typed, carrier-safe store error. The message is diagnostic rather than a
/// dispatch discriminator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WireError {
    pub code: WireErrorCode,
    pub message: String,
}

/// Stable wire error categories. Store implementations map their richer local
/// errors into these transport-level categories.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum WireErrorCode {
    InvalidRequest = 0,
    NotFound = 1,
    PermissionDenied = 2,
    DeadlineExceeded = 3,
    Overloaded = 4,
    Conflict = 5,
    Store = 6,
    Disconnected = 7,
    Internal = 8,
    ResourceLimit = 9,
    Unsupported = 10,
}

/// A complete v1 payload. Framing carries either direction symmetrically.
#[derive(Clone, Debug)]
pub enum WireMessage {
    Request(Request),
    Response(Response),
}
