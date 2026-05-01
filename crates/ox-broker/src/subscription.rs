//! Subscription protocol — traits and registry for path-pattern dispatch.
//!
//! StructFS itself only does reads and writes. The subscription protocol
//! is a runtime layered above the broker that intercepts writes, computes
//! the resulting `PathChange`, looks up matching handlers, and invokes
//! them. Handlers can return more writes (queued FIFO and re-dispatched,
//! cascade-bounded) and/or spawn long-running tasks that write back via
//! the back-channel `AsyncWriter`.
//!
//! Per spec §3.3 / §5.8 of the settings-screen redesign.
//!
//! ## Naming note: `AsyncWriter`
//!
//! The crate already exports a different `AsyncWriter` from
//! [`crate::async_store`] — that one is the *server-side* trait used by
//! `mount_async`, with `&mut self` and a `'static` future. The subscription
//! protocol's `AsyncWriter` (defined here) is a *shareable handle* trait:
//! `&self`, `Send + Sync`, designed to be held as `Arc<dyn AsyncWriter>`
//! by spawned tasks. The two cannot be unified — different self-types,
//! different lifetimes, different intended usage. They live in separate
//! modules and `use`-site disambiguation handles the name clash.

use std::sync::Arc;

use structfs_core_store::{Error as StoreError, Path, Reader, Record};

pub use ox_types::subscription::{PathChange, PathPattern, SubscriptionId, Write};

use crate::async_store::BoxFuture;

/// A handler that fires when a write hits one of its watched patterns.
///
/// `handle` runs synchronously in the dispatcher's call-stack after the
/// triggering write commits. Returned writes are queued and applied
/// through the same dispatcher (cascade-bounded). For long-running work,
/// `ctx.spawn` a future that writes back through `ctx.writer`.
///
/// Panics or errors raised here are caught at the dispatcher boundary —
/// siblings still run, the original write returns Ok.
pub trait Subscription: Send + Sync {
    /// Stable identifier — used for logging and supersession bookkeeping.
    fn id(&self) -> &SubscriptionId;

    /// Patterns this subscription watches. May be multiple; the registry
    /// indexes one entry per pattern, but a write that matches multiple
    /// of the same subscription's patterns invokes the handler once.
    fn watches(&self) -> &[PathPattern];

    /// Called once after the watched write commits. Returns additional
    /// writes for the dispatcher to apply (cascade-bounded). Errors and
    /// panics are caught at the dispatcher boundary.
    fn handle(&self, ctx: SubCtx<'_>) -> Vec<Write>;
}

/// Context passed to `Subscription::handle`. The snapshot is pinned at
/// the post-write state; the change carries `before`/`after`. `spawn`
/// hands a future to the runtime and returns an `AbortHandle` so the
/// subscription can supersede prior tasks; `writer` is the back-channel
/// the spawned future writes through (this is the dispatcher itself, so
/// spawned writes re-enter the protocol as new logical events).
///
/// The spec literal is `snapshot: &'a dyn Reader` but `Reader::read`
/// takes `&mut self`, so we use `&mut dyn Reader` here. The handler can
/// still only read (no write surface on the trait).
pub struct SubCtx<'a> {
    pub snapshot: &'a mut dyn Reader,
    pub change: &'a PathChange,
    pub spawn: &'a dyn SpawnHandle,
    pub writer: Arc<dyn AsyncWriter>,
}

/// Hands a `'static` future to the runtime and returns an `AbortHandle`.
/// Production: `TokioSpawnHandle` calls `tokio::spawn`. Tests: a mock that
/// records the spawned tasks for later inspection.
pub trait SpawnHandle: Send + Sync {
    fn spawn(&self, task: BoxFuture<()>) -> tokio::task::AbortHandle;
}

/// A shareable async writer — the back-channel for spawned subscription
/// tasks. Held as `Arc<dyn AsyncWriter>` by the spawned future.
///
/// The future returned is `'static` — implementations clone whatever
/// they need for the call. This avoids a borrow on `&self` outliving the
/// scope of the spawned task. (See module-level docs on the name clash
/// with `crate::async_store::AsyncWriter`.)
pub trait AsyncWriter: Send + Sync {
    fn write(&self, path: Path, record: Record) -> BoxFuture<Result<Path, StoreError>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trivial subscription that watches nothing and returns no writes.
    /// Sanity-checks that the trait shape compiles and is object-safe.
    struct NoOpSubscription {
        id: SubscriptionId,
        watches: Vec<PathPattern>,
    }

    impl Subscription for NoOpSubscription {
        fn id(&self) -> &SubscriptionId {
            &self.id
        }
        fn watches(&self) -> &[PathPattern] {
            &self.watches
        }
        fn handle(&self, _ctx: SubCtx<'_>) -> Vec<Write> {
            Vec::new()
        }
    }

    #[test]
    fn noop_subscription_is_object_safe() {
        let sub: Arc<dyn Subscription> = Arc::new(NoOpSubscription {
            id: SubscriptionId("test".to_string()),
            watches: Vec::new(),
        });
        assert_eq!(sub.id().0, "test");
        assert!(sub.watches().is_empty());
    }

    #[test]
    fn subscription_id_roundtrip() {
        let id = SubscriptionId(String::from("test"));
        let cloned = id.clone();
        assert_eq!(id, cloned);
        assert_eq!(id.0, "test");
    }
}
