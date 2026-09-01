//! Codec and framing failures.

use thiserror::Error;

/// A fail-closed wire decoding or encoding error.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CodecError {
    #[error("frame is truncated: declared {declared} payload bytes, received {available}")]
    TruncatedFrame { declared: usize, available: usize },

    #[error("frame has {trailing} trailing bytes")]
    TrailingFrameBytes { trailing: usize },

    #[error("frame size {actual} exceeds limit {limit}")]
    FrameTooLarge { actual: usize, limit: usize },

    #[error("decoded allocation would exceed limit {limit}")]
    AllocationLimit { limit: usize },

    #[error("CBOR nesting depth exceeds limit {limit}")]
    NestingLimit { limit: usize },

    #[error("{kind} length {actual} exceeds limit {limit}")]
    CollectionLimit {
        kind: &'static str,
        actual: usize,
        limit: usize,
    },

    #[error("path has {actual} components, limit is {limit}")]
    PathLength { actual: usize, limit: usize },

    #[error("path component has {actual} bytes, limit is {limit}")]
    PathComponentLength { actual: usize, limit: usize },

    #[error("invalid StructFS path: {0}")]
    InvalidPath(String),

    #[error("invalid CBOR: {0}")]
    InvalidCbor(String),

    #[error("duplicate map key")]
    DuplicateKey,

    #[error("CBOR payload is not deterministic canonical encoding")]
    NonCanonical,

    #[error("unsupported wire version {0}")]
    UnsupportedVersion(u64),

    #[error("unsupported {kind} discriminant {value}")]
    UnsupportedVariant { kind: &'static str, value: u64 },

    #[error("unknown or misplaced field {0}")]
    UnknownField(u64),

    #[error("missing required field {0}")]
    MissingField(u64),

    #[error("field {field} has the wrong CBOR type; expected {expected}")]
    WrongType {
        field: &'static str,
        expected: &'static str,
    },

    #[error("read requests must not carry a record")]
    RecordOnRead,

    #[error("write requests must carry a record")]
    MissingWriteRecord,

    #[error("StructFS added a Value or Record variant unsupported by wire v1")]
    UnsupportedStructFsVariant,

    #[error("frame payload length does not fit the four-byte prefix")]
    LengthOverflow,
}
