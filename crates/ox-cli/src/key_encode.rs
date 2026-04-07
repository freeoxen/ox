//! Encode crossterm key events as string names for InputStore dispatch.

use crossterm::event::{KeyCode, KeyModifiers};

/// Encode a crossterm key event as a string key name.
///
/// Convention:
/// - Letters: `"j"`, `"k"`, `"q"`, `"i"`
/// - Special: `"Enter"`, `"Esc"`, `"Backspace"`, `"Up"`, `"Down"`, `"Left"`, `"Right"`
/// - With Ctrl: `"Ctrl+c"`, `"Ctrl+s"`, `"Ctrl+Enter"`
/// - Digits: `"1"` through `"9"`
/// - Punctuation: `"/"`, `"d"`, etc.
pub fn encode_key(modifiers: KeyModifiers, code: KeyCode) -> Option<String> {
    let ctrl = modifiers.contains(KeyModifiers::CONTROL);

    match code {
        KeyCode::Char(c) if ctrl => Some(format!("Ctrl+{}", c)),
        KeyCode::Enter if ctrl => Some("Ctrl+Enter".to_string()),
        KeyCode::Char(c) => Some(c.to_string()),
        KeyCode::Enter => Some("Enter".to_string()),
        KeyCode::Esc => Some("Esc".to_string()),
        KeyCode::Backspace => Some("Backspace".to_string()),
        KeyCode::Up => Some("Up".to_string()),
        KeyCode::Down => Some("Down".to_string()),
        KeyCode::Left => Some("Left".to_string()),
        KeyCode::Right => Some("Right".to_string()),
        KeyCode::Tab => Some("Tab".to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_char() {
        assert_eq!(
            encode_key(KeyModifiers::NONE, KeyCode::Char('j')),
            Some("j".to_string())
        );
    }

    #[test]
    fn ctrl_char() {
        assert_eq!(
            encode_key(KeyModifiers::CONTROL, KeyCode::Char('c')),
            Some("Ctrl+c".to_string())
        );
    }

    #[test]
    fn special_keys() {
        assert_eq!(
            encode_key(KeyModifiers::NONE, KeyCode::Enter),
            Some("Enter".to_string())
        );
        assert_eq!(
            encode_key(KeyModifiers::NONE, KeyCode::Esc),
            Some("Esc".to_string())
        );
        assert_eq!(
            encode_key(KeyModifiers::NONE, KeyCode::Up),
            Some("Up".to_string())
        );
    }

    #[test]
    fn ctrl_enter() {
        assert_eq!(
            encode_key(KeyModifiers::CONTROL, KeyCode::Enter),
            Some("Ctrl+Enter".to_string())
        );
    }

    #[test]
    fn unknown_returns_none() {
        assert_eq!(encode_key(KeyModifiers::NONE, KeyCode::F(1)), None);
    }
}
