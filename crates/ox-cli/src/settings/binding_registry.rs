//! Binding registry — maps `(screen, cursor, mode, key)` to a `CommandId`.
//!
//! Lookup is a linear scan ordered by specificity (per spec §4.5):
//!
//! 1. cursor-Some + mode-Some  (most specific)
//! 2. cursor-Some + mode-None
//! 3. cursor-None + mode-Some
//! 4. cursor-None + mode-None  (least specific)
//!
//! Within a class, registration order breaks ties — the first registered
//! wins. The registry maintains entries in resolution order (specificity
//! descending, registration order preserved within a class) by stable-
//! sorting after each insertion.

use structfs_core_store::Path;

use ox_types::{BindingEntry, BindingScope, CommandId, KeyChord, Mode, Phase, Screen};

/// Indexes bindings for `(screen, cursor, mode, key)` → `CommandId`.
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

    /// Find the binding matching all five selectors, in specificity order.
    /// Returns the `CommandId` of the first match, or `None`. The `phase`
    /// argument filters by hierarchical-dispatch phase — `Target` is the
    /// single-phase default for callers that don't yet split capture /
    /// target / bubble.
    pub fn lookup(
        &self,
        screen: Screen,
        cursor: &Path,
        mode: Option<Mode>,
        key: &KeyChord,
        phase: Phase,
    ) -> Option<&CommandId> {
        for e in &self.entries {
            if e.screen != screen {
                continue;
            }
            if &e.key != key {
                continue;
            }
            if !e.scope.matches(cursor) {
                continue;
            }
            if e.phase != phase {
                continue;
            }
            if let Some(m) = e.mode {
                if Some(m) != mode {
                    continue;
                }
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
/// `Exact(p) > Prefix(p, longer) > Prefix(p, shorter) > Anywhere`,
/// crossed with mode-Some > mode-None. The five-bit code packs both
/// dimensions into one sort key so a stable sort produces the right
/// resolution order. We push `Prefix` deeper-first by letting longer
/// prefixes win — represented inline in the sort key by negating the
/// component count.
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
    // Minor axis: mode-Some wins over mode-None within a scope class.
    let mode_offset = if e.mode.is_some() { 0 } else { 1 };
    // Compose: scope_major dominates by a wide stride; depth penalty
    // distinguishes Prefix entries; mode offset breaks remaining ties.
    scope_major * 1000 + scope_depth_penalty * 10 + mode_offset
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use ox_path::oxpath;
    use ox_types::key_chord::{KeyCodeRepr, KeyModifierSet};

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
            screen: Screen::Settings,
            scope: BindingScope::Anywhere,
            mode: None,
            key: key_char('a'),
            command_id: cmd("screen_wide"),
            phase: Phase::Target,
        });
        reg.register(BindingEntry {
            screen: Screen::Settings,
            scope: BindingScope::Exact(p.clone()),
            mode: None,
            key: key_char('a'),
            command_id: cmd("cursor_specific"),
            phase: Phase::Target,
        });

        let hit = reg
            .lookup(Screen::Settings, &p, None, &key_char('a'), Phase::Target)
            .expect("should match");
        assert_eq!(hit, &cmd("cursor_specific"));
    }

    #[test]
    fn mode_specific_beats_unspecified_when_same_cursor() {
        let mut reg = BindingRegistry::new();
        let p = oxpath!("settings", "accounts");

        reg.register(BindingEntry {
            screen: Screen::Settings,
            scope: BindingScope::Exact(p.clone()),
            mode: None,
            key: key_char('a'),
            command_id: cmd("mode_any"),
            phase: Phase::Target,
        });
        reg.register(BindingEntry {
            screen: Screen::Settings,
            scope: BindingScope::Exact(p.clone()),
            mode: Some(Mode::Insert),
            key: key_char('a'),
            command_id: cmd("mode_insert"),
            phase: Phase::Target,
        });

        let hit = reg
            .lookup(
                Screen::Settings,
                &p,
                Some(Mode::Insert),
                &key_char('a'),
                Phase::Target,
            )
            .expect("should match");
        assert_eq!(hit, &cmd("mode_insert"));
    }

    #[test]
    fn falls_through_to_whole_screen() {
        let mut reg = BindingRegistry::new();
        reg.register(BindingEntry {
            screen: Screen::Settings,
            scope: BindingScope::Anywhere,
            mode: None,
            key: key_char('q'),
            command_id: cmd("quit"),
            phase: Phase::Target,
        });

        // Any cursor — there's no cursor-specific entry, so the
        // whole-screen one wins.
        let hit = reg
            .lookup(
                Screen::Settings,
                &oxpath!("settings", "anywhere"),
                None,
                &key_char('q'),
                Phase::Target,
            )
            .expect("should match");
        assert_eq!(hit, &cmd("quit"));

        let hit = reg
            .lookup(
                Screen::Settings,
                &oxpath!(),
                None,
                &key_char('q'),
                Phase::Target,
            )
            .expect("should match");
        assert_eq!(hit, &cmd("quit"));
    }

    #[test]
    fn registration_order_breaks_ties() {
        let mut reg = BindingRegistry::new();

        // Two entries with identical specificity (whole-screen, no mode).
        reg.register(BindingEntry {
            screen: Screen::Settings,
            scope: BindingScope::Anywhere,
            mode: None,
            key: key_char('x'),
            command_id: cmd("first"),
            phase: Phase::Target,
        });
        reg.register(BindingEntry {
            screen: Screen::Settings,
            scope: BindingScope::Anywhere,
            mode: None,
            key: key_char('x'),
            command_id: cmd("second"),
            phase: Phase::Target,
        });

        let hit = reg
            .lookup(
                Screen::Settings,
                &oxpath!("settings"),
                None,
                &key_char('x'),
                Phase::Target,
            )
            .expect("should match");
        assert_eq!(hit, &cmd("first"));
    }

    #[test]
    fn mismatched_key_returns_none() {
        let mut reg = BindingRegistry::new();
        reg.register(BindingEntry {
            screen: Screen::Settings,
            scope: BindingScope::Anywhere,
            mode: None,
            key: key_char('j'),
            command_id: cmd("down"),
            phase: Phase::Target,
        });

        let hit = reg.lookup(
            Screen::Settings,
            &oxpath!(),
            None,
            &key_char('k'),
            Phase::Target,
        );
        assert!(hit.is_none());
    }

    #[test]
    fn mismatched_screen_returns_none() {
        let mut reg = BindingRegistry::new();
        reg.register(BindingEntry {
            screen: Screen::Settings,
            scope: BindingScope::Anywhere,
            mode: None,
            key: key_char('j'),
            command_id: cmd("down"),
            phase: Phase::Target,
        });

        let hit = reg.lookup(
            Screen::Inbox,
            &oxpath!(),
            None,
            &key_char('j'),
            Phase::Target,
        );
        assert!(hit.is_none());
    }

    #[test]
    fn mode_specific_does_not_fire_when_mode_differs() {
        // Defensive: a `mode: Some(Insert)` entry must NOT match a
        // lookup with `mode: Some(Normal)` even if the entry is the
        // most specific one registered.
        let mut reg = BindingRegistry::new();
        reg.register(BindingEntry {
            screen: Screen::Settings,
            scope: BindingScope::Anywhere,
            mode: Some(Mode::Insert),
            key: key_char('a'),
            command_id: cmd("insert_only"),
            phase: Phase::Target,
        });

        let hit = reg.lookup(
            Screen::Settings,
            &oxpath!(),
            Some(Mode::Normal),
            &key_char('a'),
            Phase::Target,
        );
        assert!(hit.is_none());
    }

    #[test]
    fn mode_specific_does_not_fire_when_mode_is_none() {
        // Defensive: a `mode: Some(Insert)` entry must NOT match a
        // lookup with `mode: None`.
        let mut reg = BindingRegistry::new();
        reg.register(BindingEntry {
            screen: Screen::Settings,
            scope: BindingScope::Anywhere,
            mode: Some(Mode::Insert),
            key: key_char('a'),
            command_id: cmd("insert_only"),
            phase: Phase::Target,
        });

        let hit = reg.lookup(
            Screen::Settings,
            &oxpath!(),
            None,
            &key_char('a'),
            Phase::Target,
        );
        assert!(hit.is_none());
    }

    #[test]
    fn empty_registry_returns_none() {
        let reg = BindingRegistry::new();
        let hit = reg.lookup(
            Screen::Settings,
            &oxpath!(),
            None,
            &key_char('a'),
            Phase::Target,
        );
        assert!(hit.is_none());
    }

    #[test]
    fn phase_filters_lookup() {
        // An entry registered as Capture must not match a Target lookup,
        // and vice versa.
        let mut reg = BindingRegistry::new();
        reg.register(BindingEntry {
            screen: Screen::Settings,
            scope: BindingScope::Anywhere,
            mode: None,
            key: key_char('a'),
            command_id: cmd("capture_only"),
            phase: Phase::Capture,
        });

        // Target lookup misses the Capture entry.
        let hit = reg.lookup(
            Screen::Settings,
            &oxpath!(),
            None,
            &key_char('a'),
            Phase::Target,
        );
        assert!(hit.is_none());

        // Capture lookup finds it.
        let hit = reg
            .lookup(
                Screen::Settings,
                &oxpath!(),
                None,
                &key_char('a'),
                Phase::Capture,
            )
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
            screen: Screen::Settings,
            scope: BindingScope::Anywhere,
            mode: None,
            key: key_char('x'),
            command_id: cmd("less_specific"),
            phase: Phase::Capture,
        });
        reg.register(BindingEntry {
            screen: Screen::Settings,
            scope: BindingScope::Exact(p.clone()),
            mode: None,
            key: key_char('x'),
            command_id: cmd("more_specific"),
            phase: Phase::Capture,
        });

        let hit = reg
            .lookup(Screen::Settings, &p, None, &key_char('x'), Phase::Capture)
            .expect("should match");
        assert_eq!(hit, &cmd("more_specific"));
    }

    #[test]
    fn same_key_different_phases_route_independently() {
        // Same (screen, scope, mode, key) under two different phases must
        // coexist and route independently per the lookup phase.
        let mut reg = BindingRegistry::new();
        let p = oxpath!("settings", "accounts");

        reg.register(BindingEntry {
            screen: Screen::Settings,
            scope: BindingScope::Exact(p.clone()),
            mode: None,
            key: key_char('x'),
            command_id: cmd("on_capture"),
            phase: Phase::Capture,
        });
        reg.register(BindingEntry {
            screen: Screen::Settings,
            scope: BindingScope::Exact(p.clone()),
            mode: None,
            key: key_char('x'),
            command_id: cmd("on_target"),
            phase: Phase::Target,
        });

        let capture_hit = reg
            .lookup(Screen::Settings, &p, None, &key_char('x'), Phase::Capture)
            .expect("capture should match");
        assert_eq!(capture_hit, &cmd("on_capture"));

        let target_hit = reg
            .lookup(Screen::Settings, &p, None, &key_char('x'), Phase::Target)
            .expect("target should match");
        assert_eq!(target_hit, &cmd("on_target"));
    }
}
