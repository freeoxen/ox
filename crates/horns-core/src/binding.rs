//! Binding data shapes and registry.
//!
//! `BindingRegistry` maps `(cursor, key, phase)` to a `CommandId`.
//! Lookup is a linear scan ordered by scope specificity:
//!
//! 1. `Exact(p)`
//! 2. `Prefix(p)` (deeper p wins within this class)
//! 3. `Anywhere`
//!
//! Within a class, registration order breaks ties — the first registered
//! wins. The registry maintains entries in resolution order (specificity
//! descending, registration order preserved within a class) by stable-
//! sorting after each insertion.

use serde::{Deserialize, Serialize};
use structfs_core_store::Path;

use crate::command::CommandId;
use crate::key::KeyChord;
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

/// One row in the binding registry: under (scope, phase),
/// the keystroke `key` invokes `command_id`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingEntry {
    pub scope: BindingScope,
    pub key: KeyChord,
    pub phase: Phase,
    pub command_id: CommandId,
}

/// Indexes bindings for `(cursor, key, phase)` → `CommandId`.
pub struct BindingRegistry {
    /// Entries in *resolution order*: the first matching entry wins.
    /// `register` keeps this list ordered by specificity-then-registration
    /// via a stable sort.
    entries: Vec<BindingEntry>,
}

impl BindingRegistry {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Register a binding. The list is re-sorted by specificity after
    /// every insertion; the sort is stable, so registration order is
    /// preserved within a specificity class.
    pub fn register(&mut self, entry: BindingEntry) {
        self.entries.push(entry);
        self.entries.sort_by_key(specificity_class);
    }

    /// All registered entries in resolution order (most-specific first;
    /// registration order preserved within a specificity class). Exposed
    /// for property tests that exercise every binding via `lookup`.
    pub fn entries(&self) -> &[BindingEntry] {
        &self.entries
    }

    /// Find the binding matching `(cursor, key, phase)`, in scope-
    /// specificity order. Returns the `CommandId` of the first match,
    /// or `None`.
    pub fn lookup(&self, cursor: &Path, key: &KeyChord, phase: Phase) -> Option<&CommandId> {
        for e in &self.entries {
            if &e.key != key {
                continue;
            }
            if !e.scope.matches(cursor) {
                continue;
            }
            if e.phase != phase {
                continue;
            }
            return Some(&e.command_id);
        }
        None
    }
}

impl Default for BindingRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Specificity class (lower number = more specific = scanned first).
///
/// `Exact(p) > Prefix(p, longer) > Prefix(p, shorter) > Anywhere`.
/// We push `Prefix` deeper-first by letting longer prefixes win —
/// represented inline in the sort key by negating the component count.
fn specificity_class(e: &BindingEntry) -> i32 {
    // Major axis: scope class (smaller wins).
    //   0 = Exact
    //   1 = Prefix (with longer prefix winning within this class)
    //   2 = Anywhere
    let (scope_major, scope_depth_penalty) = match &e.scope {
        BindingScope::Exact(_) => (0, 0),
        // Within Prefix entries, a longer (more specific) prefix should
        // outrank a shorter one. Encode as negative depth so deeper
        // sorts earlier.
        BindingScope::Prefix(p) => (1, -(p.components.len() as i32)),
        BindingScope::Anywhere => (2, 0),
    };
    scope_major * 1000 + scope_depth_penalty * 10
}

#[cfg(test)]
mod tests {
    use ox_path::oxpath;

    use super::*;
    use crate::key::{KeyCodeRepr, KeyModifierSet};

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

    fn key_char(c: char) -> KeyChord {
        KeyChord {
            modifiers: KeyModifierSet::default(),
            code: KeyCodeRepr::Char(c),
        }
    }

    fn cmd(s: &str) -> CommandId {
        CommandId(s.to_string())
    }

    #[test]
    fn cursor_specific_beats_whole_screen() {
        let mut reg = BindingRegistry::new();
        let p = oxpath!("settings", "accounts");

        // Register the *less* specific one first to prove ordering is
        // not just "registration order."
        reg.register(BindingEntry {
            scope: BindingScope::Anywhere,
            key: key_char('a'),
            command_id: cmd("screen_wide"),
            phase: Phase::Target,
        });
        reg.register(BindingEntry {
            scope: BindingScope::Exact(p.clone()),
            key: key_char('a'),
            command_id: cmd("cursor_specific"),
            phase: Phase::Target,
        });

        let hit = reg
            .lookup(&p, &key_char('a'), Phase::Target)
            .expect("should match");
        assert_eq!(hit, &cmd("cursor_specific"));
    }

    #[test]
    fn falls_through_to_whole_screen() {
        let mut reg = BindingRegistry::new();
        reg.register(BindingEntry {
            scope: BindingScope::Anywhere,
            key: key_char('q'),
            command_id: cmd("quit"),
            phase: Phase::Target,
        });

        // Any cursor — there's no cursor-specific entry, so the
        // whole-screen one wins.
        let hit = reg
            .lookup(
                &oxpath!("settings", "anywhere"),
                &key_char('q'),
                Phase::Target,
            )
            .expect("should match");
        assert_eq!(hit, &cmd("quit"));

        let hit = reg
            .lookup(&oxpath!(), &key_char('q'), Phase::Target)
            .expect("should match");
        assert_eq!(hit, &cmd("quit"));
    }

    #[test]
    fn registration_order_breaks_ties() {
        let mut reg = BindingRegistry::new();

        // Two entries with identical specificity.
        reg.register(BindingEntry {
            scope: BindingScope::Anywhere,
            key: key_char('x'),
            command_id: cmd("first"),
            phase: Phase::Target,
        });
        reg.register(BindingEntry {
            scope: BindingScope::Anywhere,
            key: key_char('x'),
            command_id: cmd("second"),
            phase: Phase::Target,
        });

        let hit = reg
            .lookup(&oxpath!("settings"), &key_char('x'), Phase::Target)
            .expect("should match");
        assert_eq!(hit, &cmd("first"));
    }

    #[test]
    fn mismatched_key_returns_none() {
        let mut reg = BindingRegistry::new();
        reg.register(BindingEntry {
            scope: BindingScope::Anywhere,
            key: key_char('j'),
            command_id: cmd("down"),
            phase: Phase::Target,
        });

        let hit = reg.lookup(&oxpath!(), &key_char('k'), Phase::Target);
        assert!(hit.is_none());
    }

    #[test]
    fn empty_registry_returns_none() {
        let reg = BindingRegistry::new();
        let hit = reg.lookup(&oxpath!(), &key_char('a'), Phase::Target);
        assert!(hit.is_none());
    }

    #[test]
    fn phase_filters_lookup() {
        // An entry registered as Capture must not match a Target lookup,
        // and vice versa.
        let mut reg = BindingRegistry::new();
        reg.register(BindingEntry {
            scope: BindingScope::Anywhere,
            key: key_char('a'),
            command_id: cmd("capture_only"),
            phase: Phase::Capture,
        });

        // Target lookup misses the Capture entry.
        let hit = reg.lookup(&oxpath!(), &key_char('a'), Phase::Target);
        assert!(hit.is_none());

        // Capture lookup finds it.
        let hit = reg
            .lookup(&oxpath!(), &key_char('a'), Phase::Capture)
            .expect("should match");
        assert_eq!(hit, &cmd("capture_only"));
    }

    #[test]
    fn specificity_tie_breaking_within_phase() {
        // Specificity ordering still wins WITHIN a phase: two Capture
        // entries differing only by scope specificity must resolve to
        // the more-specific one.
        let mut reg = BindingRegistry::new();
        let p = oxpath!("settings", "accounts");

        // Less-specific (Anywhere) registered first to prove it's not
        // just registration order.
        reg.register(BindingEntry {
            scope: BindingScope::Anywhere,
            key: key_char('x'),
            command_id: cmd("less_specific"),
            phase: Phase::Capture,
        });
        reg.register(BindingEntry {
            scope: BindingScope::Exact(p.clone()),
            key: key_char('x'),
            command_id: cmd("more_specific"),
            phase: Phase::Capture,
        });

        let hit = reg
            .lookup(&p, &key_char('x'), Phase::Capture)
            .expect("should match");
        assert_eq!(hit, &cmd("more_specific"));
    }

    #[test]
    fn same_key_different_phases_route_independently() {
        // Same (scope, key) under two different phases must coexist and
        // route independently per the lookup phase.
        let mut reg = BindingRegistry::new();
        let p = oxpath!("settings", "accounts");

        reg.register(BindingEntry {
            scope: BindingScope::Exact(p.clone()),
            key: key_char('x'),
            command_id: cmd("on_capture"),
            phase: Phase::Capture,
        });
        reg.register(BindingEntry {
            scope: BindingScope::Exact(p.clone()),
            key: key_char('x'),
            command_id: cmd("on_target"),
            phase: Phase::Target,
        });

        let capture_hit = reg
            .lookup(&p, &key_char('x'), Phase::Capture)
            .expect("capture should match");
        assert_eq!(capture_hit, &cmd("on_capture"));

        let target_hit = reg
            .lookup(&p, &key_char('x'), Phase::Target)
            .expect("target should match");
        assert_eq!(target_hit, &cmd("on_target"));
    }
}
