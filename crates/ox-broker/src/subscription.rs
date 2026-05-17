//! Subscription protocol — re-exports the primitives that now live in
//! `horns-core`.
//!
//! The protocol traits (`Subscription`, `SubCtx`, `SpawnHandle`,
//! `AsyncWriter`, `SubscriptionRegistry`) and data shapes (`PathChange`,
//! `PathPattern`, `SubscriptionId`, `Write`) used to live here, but were
//! moved into `horns-core::subscription` to break a circular dependency
//! between the broker and the horns install API. The shapes are unchanged.

pub use horns_core::subscription::{
    AsyncWriter, BoxFuture, PathChange, PathPattern, SpawnHandle, SubCtx, Subscription,
    SubscriptionId, SubscriptionRegistry,
};
pub use horns_core::write::Write;
