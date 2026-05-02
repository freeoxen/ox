//! Canonical `KeyChord` set for property-style round-trip tests.
//!
//! [`canonical_chords`] returns a hand-curated set of `KeyChord` values
//! that the encoder/parser pipeline (`key_encode::encode_key` →
//! `dispatch::parse_key_str`) and the binding-registry lookup are
//! expected to handle. The set covers every `KeyCodeRepr` variant under
//! representative modifier subsets, plus a sample of common symbol/digit
//! chars for breadth.
//!
//! The set is hand-curated rather than exhaustively enumerated:
//! letter × modifier-subset × keycode is too many shapes to assert
//! individually, and most are degenerate (e.g. `ctrl+ctrl` is impossible
//! to represent). The selection here is "what we ship in bindings today
//! plus a representative sample of unused-but-valid chords for
//! future-proofing." When a new binding lands using a chord not in this
//! set, add it here so the round-trip tests cover it.

use ox_types::key_chord::{KeyChord, KeyCodeRepr, KeyModifierSet};

/// Build the canonical chord set. ~150-200 entries.
pub fn canonical_chords() -> Vec<KeyChord> {
    let mut out = Vec::new();

    // ---- Plain letters: lowercase a-z, no modifiers and ctrl ----
    for c in 'a'..='z' {
        out.push(plain(KeyCodeRepr::Char(c)));
        out.push(with_mods(KeyCodeRepr::Char(c), ctrl()));
    }

    // ---- Capital letters A-Z: shift implied, plus ctrl+shift ----
    // Bindings register capital letters with `shift: true` (mirrors what
    // the encoder/parser produces — see `dispatch::parse_key_str`).
    for c in 'A'..='Z' {
        out.push(with_mods(KeyCodeRepr::Char(c), shift()));
        out.push(with_mods(KeyCodeRepr::Char(c), ctrl_shift()));
    }

    // ---- Digits 0-9: plain and with ctrl ----
    for c in '0'..='9' {
        out.push(plain(KeyCodeRepr::Char(c)));
        out.push(with_mods(KeyCodeRepr::Char(c), ctrl()));
    }

    // ---- Common symbols (subset of what the encoder can round-trip) ----
    for &c in &['/', ',', '.', ';', '\'', '[', ']', '\\', '=', '-', '`', ' '] {
        out.push(plain(KeyCodeRepr::Char(c)));
    }

    // ---- Special keys: plain ----
    for code in [
        KeyCodeRepr::Enter,
        KeyCodeRepr::Esc,
        KeyCodeRepr::Tab,
        KeyCodeRepr::Backspace,
        KeyCodeRepr::Up,
        KeyCodeRepr::Down,
        KeyCodeRepr::Left,
        KeyCodeRepr::Right,
        KeyCodeRepr::Delete,
        KeyCodeRepr::PageUp,
        KeyCodeRepr::PageDown,
        KeyCodeRepr::Home,
        KeyCodeRepr::End,
        KeyCodeRepr::Insert,
    ] {
        out.push(plain(code));
    }

    // ---- Special keys: with ctrl (Enter is the one we ship today) ----
    out.push(with_mods(KeyCodeRepr::Enter, ctrl()));

    // ---- Shift+Tab is the canonical BackTab encoding ----
    out.push(with_mods(KeyCodeRepr::BackTab, shift()));

    // ---- Function keys F1..=F12 ----
    // Encoder gap today (`encode_key` returns None for function keys) — the
    // round-trip test silently skips chords the encoder cannot represent.
    // Included here so future-proofing intent is documented and so the
    // sanity test's `F1` anchor lands.
    for n in 1..=12u8 {
        out.push(plain(KeyCodeRepr::F(n)));
    }

    out
}

fn plain(code: KeyCodeRepr) -> KeyChord {
    KeyChord {
        modifiers: KeyModifierSet::default(),
        code,
    }
}

fn with_mods(code: KeyCodeRepr, modifiers: KeyModifierSet) -> KeyChord {
    KeyChord { modifiers, code }
}

fn ctrl() -> KeyModifierSet {
    KeyModifierSet {
        ctrl: true,
        ..KeyModifierSet::default()
    }
}

fn shift() -> KeyModifierSet {
    KeyModifierSet {
        shift: true,
        ..KeyModifierSet::default()
    }
}

fn ctrl_shift() -> KeyModifierSet {
    KeyModifierSet {
        ctrl: true,
        shift: true,
        ..KeyModifierSet::default()
    }
}

/// Bridge a `KeyChord` to the wire form an equivalent crossterm `KeyEvent`
/// would produce via `key_encode::encode_key`. Returns `None` when the
/// encoder cannot represent the chord (e.g. function keys today). The
/// round-trip property test relies on this to avoid duplicating the
/// encoder's logic; the encoder remains the source of truth for the wire
/// shape.
pub fn encode_keychord_to_str(chord: &KeyChord) -> Option<String> {
    use crossterm::event::{KeyCode, KeyModifiers};

    let mut mods = KeyModifiers::NONE;
    if chord.modifiers.ctrl {
        mods |= KeyModifiers::CONTROL;
    }
    if chord.modifiers.shift {
        mods |= KeyModifiers::SHIFT;
    }
    if chord.modifiers.alt {
        mods |= KeyModifiers::ALT;
    }
    if chord.modifiers.super_ {
        mods |= KeyModifiers::SUPER;
    }
    let code = match chord.code {
        KeyCodeRepr::Char(c) => KeyCode::Char(c),
        KeyCodeRepr::Enter => KeyCode::Enter,
        KeyCodeRepr::Esc => KeyCode::Esc,
        KeyCodeRepr::Tab => KeyCode::Tab,
        KeyCodeRepr::BackTab => KeyCode::BackTab,
        KeyCodeRepr::Backspace => KeyCode::Backspace,
        KeyCodeRepr::Delete => KeyCode::Delete,
        KeyCodeRepr::Up => KeyCode::Up,
        KeyCodeRepr::Down => KeyCode::Down,
        KeyCodeRepr::Left => KeyCode::Left,
        KeyCodeRepr::Right => KeyCode::Right,
        KeyCodeRepr::PageUp => KeyCode::PageUp,
        KeyCodeRepr::PageDown => KeyCode::PageDown,
        KeyCodeRepr::Home => KeyCode::Home,
        KeyCodeRepr::End => KeyCode::End,
        KeyCodeRepr::Insert => KeyCode::Insert,
        KeyCodeRepr::F(n) => KeyCode::F(n),
    };
    crate::key_encode::encode_key(mods, code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_set_has_at_least_100_entries() {
        assert!(
            canonical_chords().len() >= 100,
            "canonical chord set must cover ≥100 chords; got {}",
            canonical_chords().len()
        );
    }

    #[test]
    fn canonical_set_contains_expected_anchors() {
        let chords = canonical_chords();
        let anchors = [
            ("ctrl+s", with_mods(KeyCodeRepr::Char('s'), ctrl())),
            ("Shift+Tab", with_mods(KeyCodeRepr::BackTab, shift())),
            ("F1", plain(KeyCodeRepr::F(1))),
            ("plain Esc", plain(KeyCodeRepr::Esc)),
            ("plain j", plain(KeyCodeRepr::Char('j'))),
        ];
        for (name, chord) in anchors {
            assert!(
                chords.contains(&chord),
                "canonical set missing anchor {name} ({chord:?})"
            );
        }
    }
}
