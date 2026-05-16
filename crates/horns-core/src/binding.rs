//! Binding data shapes. The full registry impl lives in this file
//! after Task 4 moves it from ox-cli/src/settings/binding_registry.rs.

use serde::{Deserialize, Serialize};
use structfs_core_store::Path;

use crate::path_serde;

/// Where a binding fires. Three shapes:
///
/// - `Anywhere`: the binding fires regardless of cursor — the
///   whole-screen scope.
/// - `Exact(p)`: only when the cursor matches `p` component-for-component.
/// - `Prefix(p)`: when the cursor starts with `p` (component-level
///   prefix match, never byte-level). Used by per-row bindings that
///   apply across an entire subtree.
///
/// Specificity for resolution order: `Exact` > `Prefix(deeper)` >
/// `Prefix(shallower)` > `Anywhere`. Within a class registration order
/// breaks ties.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BindingScope {
    Anywhere,
    Exact(#[serde(with = "path_serde")] Path),
    Prefix(#[serde(with = "path_serde")] Path),
}

impl BindingScope {
    /// True when the scope admits the cursor at `cursor`. Prefix
    /// matches are component-level — `Prefix(a/b)` admits `a/b/c` but
    /// never `a/bx` — so a deeper rename can't accidentally start
    /// firing under an unrelated parent.
    pub fn matches(&self, cursor: &Path) -> bool {
        match self {
            BindingScope::Anywhere => true,
            BindingScope::Exact(p) => p.components == cursor.components,
            BindingScope::Prefix(p) => {
                p.components.len() <= cursor.components.len()
                    && cursor.components[..p.components.len()] == p.components[..]
            }
        }
    }

    /// The path the scope keys on (for Exact / Prefix), or `None` for
    /// `Anywhere`. Used by the help-hint projector to ask "which
    /// scope owns this row?" without re-implementing the match.
    pub fn keyed_path(&self) -> Option<&Path> {
        match self {
            BindingScope::Anywhere => None,
            BindingScope::Exact(p) | BindingScope::Prefix(p) => Some(p),
        }
    }
}

/// Hierarchical-dispatch phase a binding fires in. Models the DOM
/// event-flow shape:
///
/// - `Capture`: container-owned lifecycle keys that fire before the
///   focused leaf sees them.
/// - `Target`: keys the focused leaf claims (the bulk of bindings).
/// - `Bubble`: container fallbacks that fire only when the leaf
///   didn't consume the key.
///
/// No `Default` impl: every in-process `BindingEntry` registration must
/// declare its phase explicitly. Phase is a load-bearing routing
/// decision, not a default to fall back into.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Capture,
    Target,
    Bubble,
}

/// Stable identifier for a registered binding. Used as the path
/// component under `<bindings_prefix>/<binding-id>`.
#[derive(Hash, Eq, PartialEq, Clone, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BindingId(pub String);

#[cfg(test)]
mod tests {
    use ox_path::oxpath;

    use super::*;

    #[test]
    fn scope_anywhere_admits_any_cursor() {
        let s = BindingScope::Anywhere;
        assert!(s.matches(&oxpath!()));
        assert!(s.matches(&oxpath!("settings", "accounts", "alpha")));
    }

    #[test]
    fn scope_exact_matches_only_identical_cursor() {
        let s = BindingScope::Exact(oxpath!("settings", "accounts"));
        assert!(s.matches(&oxpath!("settings", "accounts")));
        assert!(!s.matches(&oxpath!("settings", "accounts", "alpha")));
        assert!(!s.matches(&oxpath!("settings")));
    }

    #[test]
    fn scope_prefix_matches_descendants_at_component_boundary() {
        let s = BindingScope::Prefix(oxpath!("settings", "accounts"));
        // Same path is its own prefix.
        assert!(s.matches(&oxpath!("settings", "accounts")));
        // Deeper component matches.
        assert!(s.matches(&oxpath!("settings", "accounts", "alpha")));
        assert!(s.matches(&oxpath!("settings", "accounts", "alpha", "key")));
        // Sibling does NOT match (component boundary, not byte).
        assert!(!s.matches(&oxpath!("settings", "models")));
        // Shallower path is not a descendant.
        assert!(!s.matches(&oxpath!("settings")));
    }

    #[test]
    fn keyed_path_returns_inner_for_exact_and_prefix() {
        let p = oxpath!("settings", "accounts");
        assert_eq!(BindingScope::Exact(p.clone()).keyed_path(), Some(&p));
        assert_eq!(BindingScope::Prefix(p.clone()).keyed_path(), Some(&p));
        assert_eq!(BindingScope::Anywhere.keyed_path(), None);
    }
}
