//! Subscription protocol shim — re-exports the data shapes that now
//! live in `horns-core::subscription`.
//!
//! Per the horns-core extraction, the subscription protocol primitives
//! (`PathChange`, `PathPattern`, `SubscriptionId`, `Write`, plus the
//! traits `Subscription` / `SubCtx` / `SpawnHandle` / `AsyncWriter` /
//! `SubscriptionRegistry`) moved into `horns-core` so the broker can
//! depend on horns-core for its dispatcher trait surfaces without
//! pulling in ox-types. This module preserves the existing import path
//! `ox_types::subscription::{...}` for callers that have not yet moved
//! to importing directly from horns-core.

pub use horns_core::subscription::{PathChange, PathPattern, SubscriptionId};
pub use horns_core::write::Write;
