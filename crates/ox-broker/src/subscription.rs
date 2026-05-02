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
    /// indexes one entry per pattern. If a single write matches more than
    /// one of this subscription's patterns, the dispatcher invokes `handle`
    /// *exactly once per write* (dedup-by-id). Authors can think of
    /// `watches()` as a union of paths the subscription cares about;
    /// overlap is safe.
    fn watches(&self) -> &[PathPattern];

    /// Invoked after the watched write commits, exactly once per
    /// triggering write regardless of how many of this subscription's
    /// patterns matched. Returns additional writes for the dispatcher to
    /// apply (cascade-bounded). Errors and panics are caught at the
    /// dispatcher boundary.
    fn handle(&self, ctx: SubCtx<'_>) -> Vec<Write>;
}

/// Context passed to `Subscription::handle`. The `change` carries the
/// pre-/post-write `Record`s; `spawn` hands a future to the runtime and
/// returns an `AbortHandle` so the subscription can supersede prior tasks;
/// `writer` is the back-channel the spawned future writes through (this is
/// the dispatcher itself, so spawned writes re-enter the protocol as new
/// logical events).
///
/// **`snapshot` is a *live broker reader*, not a pinned point-in-time
/// snapshot.** Successive `read` calls may observe writes that landed after
/// the triggering one. This deviates from the spec's "snapshot pinned at
/// post-write state" wording — the broker has no global version to pin
/// against, so we expose the live reader and let handlers reason about
/// concurrent visibility. Most handlers read a single path and don't care;
/// handlers that read multiple paths and reason about cross-path
/// consistency must do their own coordination.
///
/// The spec literal is `snapshot: &'a dyn Reader` but `Reader::read` takes
/// `&mut self`, so we use `&mut dyn Reader` here. Handlers can still only
/// read (no write surface on the trait).
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

/// Linear list of `(pattern, subscription)` entries. Registration appends
/// one entry per pattern in `sub.watches()`, cloning the `Arc`. `matching`
/// is a linear scan that returns subscriptions in registration order;
/// fine for tens of subscriptions, which is the expected scale.
///
/// A subscription whose multiple patterns all match the same path is
/// returned multiple times (once per matching pattern). Dispatch
/// dedup-by-id is the dispatcher's responsibility, not the registry's.
#[derive(Default)]
pub struct SubscriptionRegistry {
    entries: Vec<(PathPattern, Arc<dyn Subscription>)>,
}

impl SubscriptionRegistry {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Append one entry per pattern in `sub.watches()`, cloning the Arc.
    pub fn register(&mut self, sub: Arc<dyn Subscription>) {
        for pattern in sub.watches() {
            self.entries.push((pattern.clone(), sub.clone()));
        }
    }

    /// Subscriptions whose pattern matches `path`. Order matches
    /// registration order (FIFO across the entire registry).
    pub fn matching(&self, path: &Path) -> Vec<Arc<dyn Subscription>> {
        self.entries
            .iter()
            .filter(|(pat, _)| pat.matches(path))
            .map(|(_, sub)| sub.clone())
            .collect()
    }

    /// Number of `(pattern, sub)` entries — for tests and diagnostics.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ox_path::oxpath;

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

    fn sub(id: &str, watches: Vec<PathPattern>) -> Arc<dyn Subscription> {
        Arc::new(NoOpSubscription {
            id: SubscriptionId(id.to_string()),
            watches,
        })
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

    // ----- SubscriptionRegistry -----

    #[test]
    fn register_indexes_each_pattern() {
        let mut reg = SubscriptionRegistry::new();
        let s = sub(
            "two-pattern",
            vec![
                PathPattern::Exact(oxpath!("a")),
                PathPattern::Prefix(oxpath!("b")),
            ],
        );
        reg.register(s);
        assert_eq!(reg.len(), 2);
    }

    #[test]
    fn matching_returns_subs_whose_pattern_matches() {
        let mut reg = SubscriptionRegistry::new();
        let a = sub("A", vec![PathPattern::Exact(oxpath!("p"))]);
        let b = sub("B", vec![PathPattern::Prefix(oxpath!("q"))]);
        reg.register(a);
        reg.register(b);

        let m = reg.matching(&oxpath!("p"));
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].id().0, "A");

        let m = reg.matching(&oxpath!("q", "x"));
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].id().0, "B");

        let m = reg.matching(&oxpath!("unrelated"));
        assert!(m.is_empty());
    }

    #[test]
    fn registration_order_is_stable() {
        let mut reg = SubscriptionRegistry::new();
        // Both subs match `p/x` via Prefix(p).
        let a = sub("A", vec![PathPattern::Prefix(oxpath!("p"))]);
        let b = sub("B", vec![PathPattern::Prefix(oxpath!("p"))]);
        reg.register(a);
        reg.register(b);

        let m = reg.matching(&oxpath!("p", "x"));
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].id().0, "A");
        assert_eq!(m[1].id().0, "B");
    }

    #[test]
    fn unique_subscription_returned_when_pattern_overlaps() {
        // A single subscription registered with two patterns that both
        // match the same path. Registry behavior: returns the subscription
        // once per matching pattern. The *dispatcher* then dedups by id
        // (Arc::ptr_eq) so handlers fire exactly once per write — see
        // `dispatching_store::tests::overlapping_patterns_invoke_handler_once`.
        // This test pins the registry-level behavior; dedup is layered above.
        let mut reg = SubscriptionRegistry::new();
        let s = sub(
            "multi",
            vec![
                PathPattern::Prefix(oxpath!("p")),
                PathPattern::PrefixSuffix {
                    prefix: oxpath!("p"),
                    suffix: oxpath!("suffix"),
                },
            ],
        );
        reg.register(s);

        // Path matches both Prefix(p) AND PrefixSuffix{p, suffix}.
        let m = reg.matching(&oxpath!("p", "x", "suffix"));
        assert_eq!(m.len(), 2, "two patterns match → two entries");
        assert_eq!(m[0].id().0, "multi");
        assert_eq!(m[1].id().0, "multi");
    }
}
