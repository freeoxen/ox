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

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use structfs_core_store::{Path, Reader};

use crate::command::{CommandCtx, CommandId};
use crate::key::KeyChord;
use crate::path_serde;
use crate::write::Write;

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
            BindingScope::Exact(p) => p == cursor,
            BindingScope::Prefix(p) => cursor.has_prefix(p),
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
///
/// `priority` (0–255, lower = more important) drives status-bar
/// curation: when the page footer has limited horizontal room, the
/// renderer projects key hints sorted by priority ascending and keeps
/// the top N that fit. Unset bindings default to [`DEFAULT_BINDING_PRIORITY`]
/// — high enough that explicitly-curated bindings always win a slot
/// before unflagged ones.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingEntry {
    pub scope: BindingScope,
    pub key: KeyChord,
    pub phase: Phase,
    pub command_id: CommandId,
    #[serde(default = "default_binding_priority")]
    pub priority: u8,
}

/// Catch-all priority for unflagged bindings. Chosen so a curated
/// binding at any commonly-used priority (10/20/30/etc.) wins ahead
/// of every unflagged sibling.
pub const DEFAULT_BINDING_PRIORITY: u8 = 200;

fn default_binding_priority() -> u8 {
    DEFAULT_BINDING_PRIORITY
}

/// Stable identifier for a registered handler. Used as the path
/// component under `<handlers_prefix>/<handler-id>`.
#[derive(Hash, Eq, PartialEq, Clone, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct HandlerId(pub String);

/// Opaque event consumer. The dispatcher asks the handler whether
/// it claims the key; the handler inspects the chord and returns
/// `Some(writes)` to claim (with the writes that should be applied)
/// or `None` to pass.
///
/// `Some(vec![])` is a legitimate claim: "consumed, no writes." Distinct
/// from `None` (didn't claim), so a handler can swallow a key without
/// state change.
pub trait KeyHandler: Send + Sync {
    fn handle(
        &self,
        snapshot: &mut dyn Reader,
        key: &KeyChord,
        ctx: &CommandCtx<'_>,
    ) -> Option<Vec<Write>>;
}

/// One row in the handler tier: under (scope, phase), the opaque
/// `handler` may claim a keystroke. Registration-order, first match
/// wins within (scope, phase); discrete bindings always beat handlers
/// at the same (scope, phase).
pub struct HandlerEntry {
    pub scope: BindingScope,
    pub phase: Phase,
    pub handler: Arc<dyn KeyHandler>,
}

/// The data-half of a handler registration. The path-stored metadata
/// lives at `<handlers_prefix>/<handler-id>` so authors can introspect
/// which handlers are installed and which class of input each claims.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HandlerMetadata {
    pub scope: BindingScope,
    pub phase: Phase,
    /// Free-form label naming what the handler claims, for introspection.
    /// Not interpreted by the framework. Examples: "printable_ascii",
    /// "arrow_navigation", "function_keys".
    pub class: String,
}

/// Indexes bindings for `(cursor, key, phase)` → `CommandId`, plus an
/// opaque `KeyHandler` tier asked after every discrete-tier miss.
pub struct BindingRegistry {
    /// Entries in *resolution order*: the first matching entry wins.
    /// `register` keeps this list ordered by specificity-then-registration
    /// via a stable sort.
    entries: Vec<BindingEntry>,
    /// Handlers in *registration order*. The dispatcher asks handlers
    /// at each (scope, phase) only after the discrete tier misses at
    /// that same (scope, phase).
    handlers: Vec<HandlerEntry>,
}

impl BindingRegistry {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            handlers: Vec::new(),
        }
    }

    /// Register a binding. The list is re-sorted by specificity after
    /// every insertion; the sort is stable, so registration order is
    /// preserved within a specificity class.
    pub fn register(&mut self, entry: BindingEntry) {
        self.entries.push(entry);
        self.entries.sort_by_key(specificity_class);
    }

    /// Register a handler. Handlers are queried in registration order;
    /// the first whose scope admits the cursor and whose phase matches
    /// gets asked to claim the chord.
    pub fn register_handler(&mut self, entry: HandlerEntry) {
        self.handlers.push(entry);
    }

    /// All registered entries in resolution order (most-specific first;
    /// registration order preserved within a specificity class). Exposed
    /// for property tests that exercise every binding via `lookup`.
    pub fn entries(&self) -> &[BindingEntry] {
        &self.entries
    }

    /// All registered handlers in registration order. Exposed for
    /// introspection (help-hint projection, debug surfaces).
    pub fn handlers(&self) -> &[HandlerEntry] {
        &self.handlers
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

    /// First handler whose scope admits the cursor and whose phase matches.
    /// Registration order, first scope/phase match wins. Returns the handler
    /// reference; the caller invokes `.handle(snapshot, key, ctx)` to ask
    /// if it claims this specific key.
    ///
    /// The `_key` parameter is reserved for future use (e.g. cheap
    /// pre-filter by key class). Today the handler itself inspects the
    /// key.
    pub fn lookup_handler(
        &self,
        cursor: &Path,
        _key: &KeyChord,
        phase: Phase,
    ) -> Option<&dyn KeyHandler> {
        for entry in &self.handlers {
            if entry.phase != phase {
                continue;
            }
            if !entry.scope.matches(cursor) {
                continue;
            }
            return Some(&*entry.handler);
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
        BindingScope::Prefix(p) => (1, -(p.len() as i32)),
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
            priority: 200,
        });
        reg.register(BindingEntry {
            scope: BindingScope::Exact(p.clone()),
            key: key_char('a'),
            command_id: cmd("cursor_specific"),
            phase: Phase::Target,
            priority: 200,
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
            priority: 200,
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
            priority: 200,
        });
        reg.register(BindingEntry {
            scope: BindingScope::Anywhere,
            key: key_char('x'),
            command_id: cmd("second"),
            phase: Phase::Target,
            priority: 200,
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
            priority: 200,
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
            priority: 200,
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
            priority: 200,
        });
        reg.register(BindingEntry {
            scope: BindingScope::Exact(p.clone()),
            key: key_char('x'),
            command_id: cmd("more_specific"),
            phase: Phase::Capture,
            priority: 200,
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
            priority: 200,
        });
        reg.register(BindingEntry {
            scope: BindingScope::Exact(p.clone()),
            key: key_char('x'),
            command_id: cmd("on_target"),
            phase: Phase::Target,
            priority: 200,
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

    #[test]
    fn lookup_handler_finds_handler_at_matching_scope_and_phase() {
        use std::sync::Arc;
        use structfs_core_store::{Path, Reader};

        use crate::write::Write;

        struct AcceptAny;
        impl super::KeyHandler for AcceptAny {
            fn handle(
                &self,
                _: &mut dyn Reader,
                _: &crate::key::KeyChord,
                _: &crate::command::CommandCtx<'_>,
            ) -> Option<Vec<Write>> {
                Some(vec![])
            }
        }

        let mut reg = BindingRegistry::new();
        reg.register_handler(super::HandlerEntry {
            scope: BindingScope::Exact(Path::parse("a/b").unwrap()),
            phase: Phase::Target,
            handler: Arc::new(AcceptAny),
        });

        let cursor = Path::parse("a/b").unwrap();
        let chord = crate::key::KeyChord {
            modifiers: Default::default(),
            code: crate::key::KeyCodeRepr::Char('x'),
        };
        assert!(reg.lookup_handler(&cursor, &chord, Phase::Target).is_some());
    }

    #[test]
    fn lookup_handler_misses_when_phase_differs() {
        use std::sync::Arc;
        use structfs_core_store::{Path, Reader};

        use crate::write::Write;

        struct NoOp;
        impl super::KeyHandler for NoOp {
            fn handle(
                &self,
                _: &mut dyn Reader,
                _: &crate::key::KeyChord,
                _: &crate::command::CommandCtx<'_>,
            ) -> Option<Vec<Write>> {
                Some(vec![])
            }
        }

        let mut reg = BindingRegistry::new();
        reg.register_handler(super::HandlerEntry {
            scope: BindingScope::Exact(Path::parse("a").unwrap()),
            phase: Phase::Capture,
            handler: Arc::new(NoOp),
        });

        let cursor = Path::parse("a").unwrap();
        let chord = crate::key::KeyChord {
            modifiers: Default::default(),
            code: crate::key::KeyCodeRepr::Esc,
        };
        assert!(reg.lookup_handler(&cursor, &chord, Phase::Bubble).is_none());
    }

    #[test]
    fn lookup_handler_misses_when_scope_does_not_match() {
        use std::sync::Arc;
        use structfs_core_store::{Path, Reader};

        use crate::write::Write;

        struct NoOp;
        impl super::KeyHandler for NoOp {
            fn handle(
                &self,
                _: &mut dyn Reader,
                _: &crate::key::KeyChord,
                _: &crate::command::CommandCtx<'_>,
            ) -> Option<Vec<Write>> {
                Some(vec![])
            }
        }

        let mut reg = BindingRegistry::new();
        reg.register_handler(super::HandlerEntry {
            scope: BindingScope::Exact(Path::parse("a/b").unwrap()),
            phase: Phase::Target,
            handler: Arc::new(NoOp),
        });

        let cursor = Path::parse("c").unwrap();
        let chord = crate::key::KeyChord {
            modifiers: Default::default(),
            code: crate::key::KeyCodeRepr::Char('x'),
        };
        assert!(reg.lookup_handler(&cursor, &chord, Phase::Target).is_none());
    }
}
