//! Subscription protocol data shapes.
//!
//! These records are the typed surface of the broker's path-pattern
//! subscription protocol. Per spec §5.8, the **traits** (`Subscription`,
//! `SpawnHandle`, `AsyncWriter`, `SubscriptionRegistry`, `DispatchingStore`)
//! live in `ox-broker` because they reference `Reader` / `Store`. The
//! data shapes that cross subscription handlers — `SubscriptionId`,
//! `PathPattern`, `PathChange`, `Write` — live here in `ox-types`.
//!
//! `PathChange` and `Write` carry a `Record`, which has no serde impl.
//! These two records are intentionally **in-process only** — they are
//! never round-tripped through a wire format — so they only derive
//! `Clone, Debug`. `SubscriptionId` and `PathPattern` are persistable
//! and derive serde.
//!
//! Per spec §5.8 of the settings-screen redesign.

use serde::{Deserialize, Serialize};
use structfs_core_store::{Path, Record};

use crate::path_serde;

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

/// A single write to be dispatched. Carries `Record`, so this is
/// **in-process only**.
#[derive(Clone, Debug)]
pub struct Write {
    pub path: Path,
    pub record: Record,
}

#[cfg(test)]
mod tests {
    use ox_path::oxpath;

    use super::*;

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
        assert!(!pat.matches(&oxpath!(
            "config",
            "gate",
            "accounts",
            "foo",
            "refresh_now"
        )));
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
}
