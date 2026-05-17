//! Canonical `KeyChord` set for property-style round-trip tests.
//!
//! [`canonical_chords`] returns a hand-curated set of `KeyChord` values
//! that the encoder/parser pipeline (`key_encode::encode_key` →
//! `parse_key_str`) and the binding-registry lookup are expected to
//! handle. The set covers every `KeyCodeRepr` variant under
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

use ox_types::key_chord::KeyChord;
use ox_types::key_chord::KeyModifierSet;
// `KeyCodeRepr` is referenced both by the always-compiled
// `encode_keychord_to_str` (variant matching) and by the test-only
// chord constructors.
use ox_types::key_chord::KeyCodeRepr;

/// Build the canonical chord set. ~150-200 entries.
#[cfg(test)]
pub fn canonical_chords() -> Vec<KeyChord> {
    let mut out = Vec::new();

    // ---- Plain letters: lowercase a-z, no modifiers and ctrl ----
    for c in 'a'..='z' {
        out.push(plain(KeyCodeRepr::Char(c)));
        out.push(with_mods(KeyCodeRepr::Char(c), ctrl()));
    }

    // ---- Capital letters A-Z: shift implied, plus ctrl+shift ----
    // Bindings register capital letters with `shift: true` (mirrors what
    // the encoder/parser produces — see `parse_key_str`).
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

#[cfg(test)]
fn plain(code: KeyCodeRepr) -> KeyChord {
    KeyChord {
        modifiers: KeyModifierSet::default(),
        code,
    }
}

#[cfg(test)]
fn with_mods(code: KeyCodeRepr, modifiers: KeyModifierSet) -> KeyChord {
    KeyChord { modifiers, code }
}

#[cfg(test)]
fn ctrl() -> KeyModifierSet {
    KeyModifierSet {
        ctrl: true,
        ..KeyModifierSet::default()
    }
}

#[cfg(test)]
fn shift() -> KeyModifierSet {
    KeyModifierSet {
        shift: true,
        ..KeyModifierSet::default()
    }
}

#[cfg(test)]
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

/// Parse a wire-form key string back into a `KeyChord`.
///
/// Inverse of `crate::key_encode::encode_key`. The encoder is the
/// source of truth for the wire shape; this parser tracks it.
/// Returns `None` for any string the encoder would never produce
/// (e.g. function keys F1..F12, which the encoder drops to `None`).
///
/// Conventions handled:
/// - `"j"`, `"q"`, `"P"`, `"/"` etc. → bare `Char(c)` (uppercase
///   ASCII letters also imply `shift: true`, mirroring the encoder).
/// - `"Esc"`, `"Enter"`, `"Backspace"`, `"Tab"`, `"Up"`, `"Down"`,
///   `"Left"`, `"Right"`, `"Delete"`, `"PageUp"`, `"PageDown"`,
///   `"Home"`, `"End"`, `"Insert"`.
/// - `"Shift+Tab"` → `BackTab` with `shift: true` (mirrors the encoder).
/// - `"Ctrl+x"` → `Char('x')` with `ctrl: true`.
/// - `"Ctrl+Enter"` → `Enter` with `ctrl: true`.
pub fn parse_key_str(s: &str) -> Option<KeyChord> {
    // Encoder convention: KeyCode::BackTab → "Shift+Tab" wire string.
    // Bindings register KeyChord { shift: true, code: BackTab }, so we
    // must produce that exact chord rather than Tab+shift.
    if s == "Shift+Tab" {
        return Some(KeyChord {
            modifiers: KeyModifierSet {
                shift: true,
                ..KeyModifierSet::default()
            },
            code: KeyCodeRepr::BackTab,
        });
    }
    if let Some(rest) = s.strip_prefix("Ctrl+") {
        let mut chord = parse_key_str(rest)?;
        chord.modifiers.ctrl = true;
        return Some(chord);
    }
    if let Some(rest) = s.strip_prefix("Shift+") {
        let mut chord = parse_key_str(rest)?;
        chord.modifiers.shift = true;
        return Some(chord);
    }
    if let Some(rest) = s.strip_prefix("Alt+") {
        let mut chord = parse_key_str(rest)?;
        chord.modifiers.alt = true;
        return Some(chord);
    }

    let code = match s {
        "Esc" => KeyCodeRepr::Esc,
        "Enter" => KeyCodeRepr::Enter,
        "Backspace" => KeyCodeRepr::Backspace,
        "Tab" => KeyCodeRepr::Tab,
        "Up" => KeyCodeRepr::Up,
        "Down" => KeyCodeRepr::Down,
        "Left" => KeyCodeRepr::Left,
        "Right" => KeyCodeRepr::Right,
        "Delete" => KeyCodeRepr::Delete,
        "PageUp" => KeyCodeRepr::PageUp,
        "PageDown" => KeyCodeRepr::PageDown,
        "Home" => KeyCodeRepr::Home,
        "End" => KeyCodeRepr::End,
        "Insert" => KeyCodeRepr::Insert,
        _ => {
            let mut chars = s.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                // Multi-char token we don't recognize.
                return None;
            }
            // Uppercase ASCII letter on the wire reflects a Shift+letter
            // chord (the encoder writes "P" for crossterm
            // `Shift+KeyCode::Char('P')`). The settings bindings table
            // registers capital letters with `shift: true`, so set the
            // flag here.
            let mut modifiers = KeyModifierSet::default();
            if c.is_ascii_uppercase() {
                modifiers.shift = true;
            }
            return Some(KeyChord {
                modifiers,
                code: KeyCodeRepr::Char(c),
            });
        }
    };
    Some(KeyChord {
        modifiers: KeyModifierSet::default(),
        code,
    })
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

    // -------- parse_key_str unit tests ------------------------------------

    #[test]
    fn parse_bare_lowercase() {
        let chord = parse_key_str("j").expect("parsed");
        assert_eq!(chord.modifiers, KeyModifierSet::default());
        assert!(matches!(chord.code, KeyCodeRepr::Char('j')));
    }

    #[test]
    fn parse_bare_uppercase_implies_shift() {
        let chord = parse_key_str("P").expect("parsed");
        assert!(chord.modifiers.shift);
        assert!(!chord.modifiers.ctrl);
        assert!(matches!(chord.code, KeyCodeRepr::Char('P')));
    }

    #[test]
    fn parse_esc() {
        let chord = parse_key_str("Esc").expect("parsed");
        assert!(matches!(chord.code, KeyCodeRepr::Esc));
    }

    #[test]
    fn parse_ctrl_char() {
        let chord = parse_key_str("Ctrl+s").expect("parsed");
        assert!(chord.modifiers.ctrl);
        assert!(matches!(chord.code, KeyCodeRepr::Char('s')));
    }

    #[test]
    fn parse_unknown_returns_none() {
        assert!(parse_key_str("F1").is_none());
        assert!(parse_key_str("absolutelyNotAKey").is_none());
    }

    #[test]
    fn parse_shift_tab_yields_back_tab() {
        let chord = parse_key_str("Shift+Tab").expect("parsed");
        assert!(chord.modifiers.shift);
        assert!(!chord.modifiers.ctrl);
        assert!(!chord.modifiers.alt);
        assert!(matches!(chord.code, KeyCodeRepr::BackTab));
    }

    /// Property test: every `KeyChord` in the canonical set must round-trip
    /// through `encode_keychord_to_str` → `parse_key_str`.
    #[test]
    fn keychord_encode_parse_roundtrip() {
        let mut failures: Vec<String> = Vec::new();
        let mut roundtripped = 0usize;
        let mut encoder_skipped = 0usize;
        for chord in canonical_chords() {
            let Some(wire) = encode_keychord_to_str(&chord) else {
                encoder_skipped += 1;
                continue;
            };
            match parse_key_str(&wire) {
                Some(parsed) if parsed == chord => roundtripped += 1,
                Some(parsed) => failures.push(format!(
                    "{chord:?} encoded to {wire:?}, parsed back as {parsed:?} (mismatch)"
                )),
                None => failures.push(format!(
                    "{chord:?} encoded to {wire:?}, parser returned None"
                )),
            }
        }
        assert!(
            failures.is_empty(),
            "round-trip failures ({} encoder gaps tolerated):\n{}",
            encoder_skipped,
            failures.join("\n"),
        );
        assert!(
            roundtripped >= 100,
            "expected ≥100 round-trip-clean chords; got {roundtripped} (encoder skipped {encoder_skipped})"
        );
    }
}
