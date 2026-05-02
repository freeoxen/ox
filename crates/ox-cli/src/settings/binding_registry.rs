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

use ox_types::{BindingEntry, CommandId, KeyChord, Mode, Screen};

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

    /// Find the binding matching all four selectors, in specificity order.
    /// Returns the `CommandId` of the first match, or `None`.
    pub fn lookup(
        &self,
        screen: Screen,
        cursor: &Path,
        mode: Option<Mode>,
        key: &KeyChord,
    ) -> Option<&CommandId> {
        for e in &self.entries {
            if e.screen != screen {
                continue;
            }
            if &e.key != key {
                continue;
            }
            // Cursor scope: a `Some(p)` entry must match the current cursor;
            // a `None` entry matches any cursor.
            if let Some(p) = &e.cursor_path {
                if p != cursor {
                    continue;
                }
            }
            // Mode scope: a `Some(m)` entry must match the current mode;
            // a `None` entry matches any mode (including no-mode).
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

/// Specificity class (lower is more specific). Used as the stable-sort
/// key so resolution scans the most specific entries first.
fn specificity_class(e: &BindingEntry) -> u8 {
    match (e.cursor_path.is_some(), e.mode.is_some()) {
        (true, true) => 0,
        (true, false) => 1,
        (false, true) => 2,
        (false, false) => 3,
    }
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
            cursor_path: None,
            mode: None,
            key: key_char('a'),
            command_id: cmd("screen_wide"),
        });
        reg.register(BindingEntry {
            screen: Screen::Settings,
            cursor_path: Some(p.clone()),
            mode: None,
            key: key_char('a'),
            command_id: cmd("cursor_specific"),
        });

        let hit = reg
            .lookup(Screen::Settings, &p, None, &key_char('a'))
            .expect("should match");
        assert_eq!(hit, &cmd("cursor_specific"));
    }

    #[test]
    fn mode_specific_beats_unspecified_when_same_cursor() {
        let mut reg = BindingRegistry::new();
        let p = oxpath!("settings", "accounts");

        reg.register(BindingEntry {
            screen: Screen::Settings,
            cursor_path: Some(p.clone()),
            mode: None,
            key: key_char('a'),
            command_id: cmd("mode_any"),
        });
        reg.register(BindingEntry {
            screen: Screen::Settings,
            cursor_path: Some(p.clone()),
            mode: Some(Mode::Insert),
            key: key_char('a'),
            command_id: cmd("mode_insert"),
        });

        let hit = reg
            .lookup(Screen::Settings, &p, Some(Mode::Insert), &key_char('a'))
            .expect("should match");
        assert_eq!(hit, &cmd("mode_insert"));
    }

    #[test]
    fn falls_through_to_whole_screen() {
        let mut reg = BindingRegistry::new();
        reg.register(BindingEntry {
            screen: Screen::Settings,
            cursor_path: None,
            mode: None,
            key: key_char('q'),
            command_id: cmd("quit"),
        });

        // Any cursor — there's no cursor-specific entry, so the
        // whole-screen one wins.
        let hit = reg
            .lookup(
                Screen::Settings,
                &oxpath!("settings", "anywhere"),
                None,
                &key_char('q'),
            )
            .expect("should match");
        assert_eq!(hit, &cmd("quit"));

        let hit = reg
            .lookup(Screen::Settings, &oxpath!(), None, &key_char('q'))
            .expect("should match");
        assert_eq!(hit, &cmd("quit"));
    }

    #[test]
    fn registration_order_breaks_ties() {
        let mut reg = BindingRegistry::new();

        // Two entries with identical specificity (whole-screen, no mode).
        reg.register(BindingEntry {
            screen: Screen::Settings,
            cursor_path: None,
            mode: None,
            key: key_char('x'),
            command_id: cmd("first"),
        });
        reg.register(BindingEntry {
            screen: Screen::Settings,
            cursor_path: None,
            mode: None,
            key: key_char('x'),
            command_id: cmd("second"),
        });

        let hit = reg
            .lookup(Screen::Settings, &oxpath!("settings"), None, &key_char('x'))
            .expect("should match");
        assert_eq!(hit, &cmd("first"));
    }

    #[test]
    fn mismatched_key_returns_none() {
        let mut reg = BindingRegistry::new();
        reg.register(BindingEntry {
            screen: Screen::Settings,
            cursor_path: None,
            mode: None,
            key: key_char('j'),
            command_id: cmd("down"),
        });

        let hit = reg.lookup(Screen::Settings, &oxpath!(), None, &key_char('k'));
        assert!(hit.is_none());
    }

    #[test]
    fn mismatched_screen_returns_none() {
        let mut reg = BindingRegistry::new();
        reg.register(BindingEntry {
            screen: Screen::Settings,
            cursor_path: None,
            mode: None,
            key: key_char('j'),
            command_id: cmd("down"),
        });

        let hit = reg.lookup(Screen::Inbox, &oxpath!(), None, &key_char('j'));
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
            cursor_path: None,
            mode: Some(Mode::Insert),
            key: key_char('a'),
            command_id: cmd("insert_only"),
        });

        let hit = reg.lookup(
            Screen::Settings,
            &oxpath!(),
            Some(Mode::Normal),
            &key_char('a'),
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
            cursor_path: None,
            mode: Some(Mode::Insert),
            key: key_char('a'),
            command_id: cmd("insert_only"),
        });

        let hit = reg.lookup(Screen::Settings, &oxpath!(), None, &key_char('a'));
        assert!(hit.is_none());
    }

    #[test]
    fn empty_registry_returns_none() {
        let reg = BindingRegistry::new();
        let hit = reg.lookup(Screen::Settings, &oxpath!(), None, &key_char('a'));
        assert!(hit.is_none());
    }
}
