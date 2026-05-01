//! Typed key chord representation.
//!
//! `KeyChord` is the broker-boundary input record that the dispatch loop
//! reasons over. It is intentionally backend-agnostic: the CLI translates
//! `crossterm::event::KeyEvent` into a `KeyChord`; the web frontend will do
//! the same for browser key events. The dispatch table only sees `KeyChord`,
//! never the backend-specific event types.
//!
//! # Deferred: `KeyChord::from_crossterm`
//!
//! The plan (§C2 step 3) suggests a `from_crossterm(KeyEvent) -> KeyChord`
//! helper. `crossterm` is **not** currently a dependency of `ox-types` (this
//! crate stays free of TUI-backend deps), so the helper is deferred to Phase
//! P (dispatch wiring) where the conversion either lives in `ox-cli`
//! alongside the existing crossterm event loop, or `ox-types` grows a
//! `crossterm` cargo feature. Adding the dep here just to host one helper
//! would broaden ox-types' surface for no immediate caller.
//!
//! Per spec §5.6 of the settings-screen redesign.

use serde::{Deserialize, Serialize};

/// A modifier-key bitset. Use `Default::default()` for the no-modifier case.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct KeyModifierSet {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub super_: bool,
}

/// Backend-agnostic key code. Mirrors the curated subset of crossterm's
/// `KeyCode` we route through dispatch.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum KeyCodeRepr {
    Char(char),
    Enter,
    Esc,
    Tab,
    BackTab,
    Backspace,
    Delete,
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    Insert,
    F(u8),
}

/// A modifier-set + code pair representing a single keystroke.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct KeyChord {
    pub modifiers: KeyModifierSet,
    pub code: KeyCodeRepr,
}

#[cfg(test)]
mod tests {
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

    #[test]
    fn key_modifier_set_default_is_all_false() {
        let m = KeyModifierSet::default();
        assert!(!m.ctrl);
        assert!(!m.alt);
        assert!(!m.shift);
        assert!(!m.super_);
    }

    #[test]
    fn bare_char_chord_roundtrip() {
        json_roundtrip(KeyChord {
            modifiers: KeyModifierSet::default(),
            code: KeyCodeRepr::Char('a'),
        });
    }

    #[test]
    fn ctrl_char_chord_roundtrip() {
        json_roundtrip(KeyChord {
            modifiers: KeyModifierSet {
                ctrl: true,
                ..Default::default()
            },
            code: KeyCodeRepr::Char('s'),
        });
    }

    #[test]
    fn esc_chord_roundtrip() {
        json_roundtrip(KeyChord {
            modifiers: KeyModifierSet::default(),
            code: KeyCodeRepr::Esc,
        });
    }

    #[test]
    fn function_key_chord_roundtrip() {
        json_roundtrip(KeyChord {
            modifiers: KeyModifierSet::default(),
            code: KeyCodeRepr::F(5),
        });
    }

    #[test]
    fn up_arrow_chord_roundtrip() {
        json_roundtrip(KeyChord {
            modifiers: KeyModifierSet::default(),
            code: KeyCodeRepr::Up,
        });
    }

    #[test]
    fn back_tab_chord_roundtrip() {
        json_roundtrip(KeyChord {
            modifiers: KeyModifierSet {
                shift: true,
                ..Default::default()
            },
            code: KeyCodeRepr::BackTab,
        });
    }

    #[test]
    fn all_modifiers_chord_roundtrip() {
        json_roundtrip(KeyChord {
            modifiers: KeyModifierSet {
                ctrl: true,
                alt: true,
                shift: true,
                super_: true,
            },
            code: KeyCodeRepr::Char('q'),
        });
    }
}
