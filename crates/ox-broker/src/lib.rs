//! Async BrokerStore for StructFS — routes reads/writes between stores
//! by path prefix.

pub mod types;

pub use types::{Request, RequestKind, Response};
