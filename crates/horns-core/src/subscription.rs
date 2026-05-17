//! Subscription protocol — data shapes and traits for path-pattern dispatch.
//!
//! StructFS itself only does reads and writes. The subscription protocol
//! is a runtime layered above a broker that intercepts writes, computes
//! the resulting `PathChange`, looks up matching handlers, and invokes
//! them. Handlers can return more writes (queued FIFO and re-dispatched,
//! cascade-bounded) and/or spawn long-running tasks that write back via
//! the back-channel `AsyncWriter`.
//!
//! These primitives live in `horns-core` (rather than ox-broker / ox-types)
//! because the install API needs to materialize subscriptions that the
//! host then registers with its broker — keeping the trait shapes here
//! means horns-core stays broker-agnostic while `ox-broker` depends on
//! horns-core for the trait definitions it dispatches against.
//!
//! `PathChange` and `Write` carry a `Record`, which has no serde impl.
//! These two records are intentionally **in-process only** — they are
//! never round-tripped through a wire format — so they only derive
//! `Clone, Debug`. `SubscriptionId` and `PathPattern` are persistable
//! and derive serde.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use structfs_core_store::{Error as StoreError, Path, Reader, Record};

use crate::path_serde;

/// A boxed, Send, 'static future. Mirrors `ox_broker::async_store::BoxFuture`
/// so the subscription protocol can be defined without depending on the
/// broker crate.
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Stable identifier for a registered subscription. Newtype around String
/// so subscription IDs can't be confused with arbitrary text fields.
/// Wire shape is the bare string (`#[serde(transparent)]`).
#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SubscriptionId(pub String);

/// A path-matching predicate used by the subscription registry to decide
/// which handlers fire on a given write.
///
/// Matching is **component-level**, never byte-level: a `Prefix` matching
/// `config/gate/accounts` does not match `config/gate/accounts_other` —
/// the boundary lives between path components, not between characters.
//
// Externally-tagged on purpose: `#[serde(with = "path_serde")]` on inline
// `Path` fields doesn't compose with `#[serde(tag = "kind")]` because
// internal tagging can't see through field-level adapters.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathPattern {
    /// Matches exactly one path.
    Exact(#[serde(with = "path_serde")] Path),
    /// Matches any path whose components start with the given path's
    /// components. A path is a prefix of itself.
    Prefix(#[serde(with = "path_serde")] Path),
    /// Matches any path that starts with `prefix`'s components AND ends
    /// with `suffix`'s components, with **at least one** component
    /// between them. So `{ prefix: a/b, suffix: x }` matches
    /// `a/b/<one-or-more>/x` but not `a/b/x` (no instance segment).
    PrefixSuffix {
        #[serde(with = "path_serde")]
        prefix: Path,
        #[serde(with = "path_serde")]
        suffix: Path,
    },
}

impl PathPattern {
    /// Returns `true` if `path` matches this pattern. See the variant
    /// docs for the exact semantics.
    pub fn matches(&self, path: &Path) -> bool {
        match self {
            PathPattern::Exact(p) => path.components == p.components,
            PathPattern::Prefix(p) => is_component_prefix(&p.components, &path.components),
            PathPattern::PrefixSuffix { prefix, suffix } => {
                let plen = prefix.components.len();
                let slen = suffix.components.len();
                let total = path.components.len();
                // Must have at least one component between prefix and suffix.
                if plen + slen >= total {
                    return false;
                }
                if !is_component_prefix(&prefix.components, &path.components) {
                    return false;
                }
                // Compare suffix tail.
                let tail_start = total - slen;
                path.components[tail_start..] == suffix.components[..]
            }
        }
    }
}

/// `prefix` is a component-wise prefix of `whole` (a path is a prefix
/// of itself).
fn is_component_prefix(prefix: &[String], whole: &[String]) -> bool {
    if prefix.len() > whole.len() {
        return false;
    }
    whole[..prefix.len()] == *prefix
}

/// An observed change at a path. `before == None` means the path was
/// previously unset (creation); `after == None` means deletion. Carries
/// `Record`, which has no serde impl — `PathChange` is **in-process only**.
#[derive(Clone, Debug)]
pub struct PathChange {
    pub path: Path,
    pub before: Option<Record>,
    pub after: Option<Record>,
}

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
    fn handle(&self, ctx: SubCtx<'_>) -> Vec<crate::write::Write>;
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
/// scope of the spawned task.
///
/// Distinct from the `AsyncWriter` in `ox_broker::async_store`, which is
/// the *server-side* trait used by `mount_async` with `&mut self` and a
/// `'static` future. This one is a *shareable handle* trait: `&self`,
/// `Send + Sync`, designed to be held as `Arc<dyn AsyncWriter>` by
/// spawned tasks. The two cannot be unified — different self-types,
/// different lifetimes, different intended usage.
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

    /// Remove every entry whose subscription has the given id.
    /// Returns the number of `(pattern, sub)` rows removed.
    ///
    /// Used by hosts to take a subscription back out of the registry
    /// when its lifecycle ends — e.g. the ratatui
    /// `ViewRenderSubscription` only lives for the duration of a
    /// horns settings session and unregisters on exit so the terminal
    /// it holds becomes uniquely owned again.
    pub fn unregister(&mut self, id: &SubscriptionId) -> usize {
        let before = self.entries.len();
        self.entries.retain(|(_, sub)| sub.id() != id);
        before - self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use ox_path::oxpath;

    use super::*;
    use crate::write::Write;

    fn json_roundtrip<
        T: serde::Serialize + serde::de::DeserializeOwned + PartialEq + std::fmt::Debug,
    >(
        value: T,
    ) {
        let json = serde_json::to_string(&value).expect("serialize");
        let back: T = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, value);
    }

    // ----- SubscriptionId -----

    #[test]
    fn subscription_id_roundtrip() {
        json_roundtrip(SubscriptionId("settings.accounts.test".to_string()));
    }

    #[test]
    fn subscription_id_wire_shape_is_bare_string() {
        let id = SubscriptionId("settings.accounts.test".to_string());
        let json = serde_json::to_string(&id).expect("serialize");
        assert_eq!(json, "\"settings.accounts.test\"");
    }

    // ----- Exact -----

    #[test]
    fn exact_matches_identical_path() {
        let pat = PathPattern::Exact(oxpath!("config", "gate", "accounts"));
        assert!(pat.matches(&oxpath!("config", "gate", "accounts")));
    }

    #[test]
    fn exact_does_not_match_different_path() {
        let pat = PathPattern::Exact(oxpath!("config", "gate", "accounts"));
        assert!(!pat.matches(&oxpath!("config", "gate", "defaults")));
    }

    #[test]
    fn exact_does_not_match_longer_with_same_prefix() {
        let pat = PathPattern::Exact(oxpath!("config", "gate", "accounts"));
        assert!(!pat.matches(&oxpath!("config", "gate", "accounts", "foo")));
    }

    // ----- Prefix -----

    #[test]
    fn prefix_matches_descendant() {
        let pat = PathPattern::Prefix(oxpath!("config", "gate", "accounts"));
        assert!(pat.matches(&oxpath!("config", "gate", "accounts", "foo")));
    }

    #[test]
    fn prefix_matches_self() {
        let pat = PathPattern::Prefix(oxpath!("config", "gate", "accounts"));
        assert!(pat.matches(&oxpath!("config", "gate", "accounts")));
    }

    #[test]
    fn prefix_component_boundary_not_byte() {
        // `accounts_other` shares a byte prefix with `accounts` but is a
        // distinct component — must not match.
        let pat = PathPattern::Prefix(oxpath!("config", "gate", "accounts"));
        assert!(!pat.matches(&oxpath!("config", "gate", "accounts_other", "foo")));
    }

    #[test]
    fn prefix_does_not_match_shorter_path() {
        let pat = PathPattern::Prefix(oxpath!("config", "gate", "accounts"));
        assert!(!pat.matches(&oxpath!("config", "gate")));
    }

    // ----- PrefixSuffix -----

    #[test]
    fn prefix_suffix_matches_single_segment_instance() {
        let pat = PathPattern::PrefixSuffix {
            prefix: oxpath!("config", "gate", "accounts"),
            suffix: oxpath!("test_now"),
        };
        assert!(pat.matches(&oxpath!("config", "gate", "accounts", "foo", "test_now")));
    }

    #[test]
    fn prefix_suffix_matches_named_account_instance() {
        let pat = PathPattern::PrefixSuffix {
            prefix: oxpath!("config", "gate", "accounts"),
            suffix: oxpath!("test_now"),
        };
        assert!(pat.matches(&oxpath!(
            "config",
            "gate",
            "accounts",
            "anthropic_personal",
            "test_now"
        )));
    }

    #[test]
    fn prefix_suffix_matches_multi_segment_instance() {
        let pat = PathPattern::PrefixSuffix {
            prefix: oxpath!("config", "gate", "accounts"),
            suffix: oxpath!("test_now"),
        };
        // gap is `foo/bar` (2 segments) — also valid; the spec says
        // "at least one component between," so >= 1 not == 1.
        assert!(pat.matches(&oxpath!(
            "config", "gate", "accounts", "foo", "bar", "test_now"
        )));
    }

    #[test]
    fn prefix_suffix_does_not_match_with_no_instance_segment() {
        let pat = PathPattern::PrefixSuffix {
            prefix: oxpath!("config", "gate", "accounts"),
            suffix: oxpath!("test_now"),
        };
        // No segment between `accounts` and `test_now` — must not match.
        assert!(!pat.matches(&oxpath!("config", "gate", "accounts", "test_now")));
    }

    #[test]
    fn prefix_suffix_does_not_match_wrong_suffix() {
        let pat = PathPattern::PrefixSuffix {
            prefix: oxpath!("config", "gate", "accounts"),
            suffix: oxpath!("test_now"),
        };
        assert!(!pat.matches(&oxpath!("config", "gate", "accounts", "foo", "refresh_now")));
    }

    #[test]
    fn prefix_suffix_does_not_match_missing_suffix() {
        let pat = PathPattern::PrefixSuffix {
            prefix: oxpath!("config", "gate", "accounts"),
            suffix: oxpath!("test_now"),
        };
        assert!(!pat.matches(&oxpath!("config", "gate", "accounts", "foo")));
    }

    // ----- Empty-path edge cases -----

    #[test]
    fn exact_empty_matches_empty() {
        let pat = PathPattern::Exact(oxpath!());
        assert!(pat.matches(&oxpath!()));
    }

    #[test]
    fn exact_empty_does_not_match_nonempty() {
        let pat = PathPattern::Exact(oxpath!());
        assert!(!pat.matches(&oxpath!("foo")));
    }

    #[test]
    fn prefix_empty_matches_every_path() {
        let pat = PathPattern::Prefix(oxpath!());
        assert!(pat.matches(&oxpath!()));
        assert!(pat.matches(&oxpath!("foo")));
        assert!(pat.matches(&oxpath!("foo", "bar", "baz")));
    }

    #[test]
    fn prefix_suffix_empty_prefix_requires_at_least_one_leading_segment() {
        // With prefix=empty (len 0) and suffix=x (len 1), the rule
        // "plen + slen < total" reduces to "1 < total", i.e. at least
        // 2 components. So `x` alone does NOT match, but `<anything>/x`
        // does. This is the spec-specified surprising-but-consistent
        // behavior.
        let pat = PathPattern::PrefixSuffix {
            prefix: oxpath!(),
            suffix: oxpath!("x"),
        };
        assert!(!pat.matches(&oxpath!("x")));
        assert!(pat.matches(&oxpath!("foo", "x")));
        assert!(pat.matches(&oxpath!("foo", "bar", "x")));
    }

    // ----- Serde round-trip per variant -----

    #[test]
    fn path_pattern_exact_roundtrip() {
        json_roundtrip(PathPattern::Exact(oxpath!("config", "gate", "accounts")));
    }

    #[test]
    fn path_pattern_prefix_roundtrip() {
        json_roundtrip(PathPattern::Prefix(oxpath!("config", "gate", "accounts")));
    }

    #[test]
    fn path_pattern_prefix_suffix_roundtrip() {
        json_roundtrip(PathPattern::PrefixSuffix {
            prefix: oxpath!("config", "gate", "accounts"),
            suffix: oxpath!("test_now"),
        });
    }

    // ----- Registry / Subscription trait shape -----

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
        // once per matching pattern. Dispatcher dedups by id.
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
