//! Command and binding registry data shapes.
//!
//! These records are the typed surface of the command/binding system.
//! Crucially, this module ships **only** the data shapes — no `Command`
//! trait, no `CommandEffect` enum, no `PathTemplate`, no `PayloadSource`.
//! The trait `Command` lives in `ox-cli` (Phase H); the older
//! `PathTemplate`/`PayloadSource`/`CommandEffect` types are deliberately
//! deleted (spec §5.9: "Implicit in v0... → Replaced by `trait Command`").
//!
//! Per spec §5.6 of the settings-screen redesign.

use serde::{Deserialize, Serialize};
use structfs_core_store::Path;

use crate::key_chord::KeyChord;
use crate::path_serde;
use crate::ui::{Mode, Screen};

/// Stable identifier for a registered command. Newtype around String so
/// command IDs can't accidentally swap with arbitrary text fields. Wire
/// shape is the bare string (`#[serde(transparent)]`), matching the
/// project's other ID newtypes (e.g. `ox-gate::ApiKey`).
#[derive(Hash, Eq, PartialEq, Clone, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CommandId(pub String);

/// Human-facing presentation of a command (palette / help / shortcut hints).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandDisplay {
    pub name: String,
    pub description: String,
}

/// The (screen, cursor) the user must be on for a command to fire.
/// `cursor_path = None` means screen-wide.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandScope {
    pub screen: Screen,
    #[serde(with = "path_serde::option", default)]
    pub cursor_path: Option<Path>,
}

/// Where a binding fires within a screen. Three shapes:
///
/// - `Anywhere`: the binding fires regardless of cursor — the
///   whole-screen scope. Same as the legacy `cursor_path: None`.
/// - `Exact(p)`: only when the cursor matches `p` component-for-component.
///   Same as the legacy `cursor_path: Some(p)`.
/// - `Prefix(p)`: when the cursor starts with `p` (component-level
///   prefix match, never byte-level). Used by per-row bindings that
///   apply across an entire subtree — e.g. `t` on any focused account
///   row at `settings/accounts/{name}`, regardless of which account.
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

/// One row in the binding registry: under (screen, scope, mode), the
/// keystroke `key` invokes `command_id`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingEntry {
    pub screen: Screen,
    pub scope: BindingScope,
    pub mode: Option<Mode>,
    pub key: KeyChord,
    pub command_id: CommandId,
}

#[cfg(test)]
mod tests {
    use ox_path::oxpath;

    use super::*;
    use crate::key_chord::{KeyCodeRepr, KeyModifierSet};

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
    fn command_id_roundtrip() {
        json_roundtrip(CommandId("settings.save".to_string()));
    }

    #[test]
    fn command_id_wire_shape_is_bare_string() {
        let id = CommandId("settings.save".to_string());
        let json = serde_json::to_string(&id).expect("serialize");
        assert_eq!(json, "\"settings.save\"");
    }

    #[test]
    fn command_display_roundtrip() {
        json_roundtrip(CommandDisplay {
            name: "Save".to_string(),
            description: "Persist the current edits".to_string(),
        });
    }

    #[test]
    fn command_scope_screen_wide_roundtrip() {
        json_roundtrip(CommandScope {
            screen: Screen::Settings,
            cursor_path: None,
        });
    }

    #[test]
    fn command_scope_with_cursor_roundtrip() {
        json_roundtrip(CommandScope {
            screen: Screen::Settings,
            cursor_path: Some(oxpath!("settings", "accounts")),
        });
    }

    #[test]
    fn binding_entry_with_exact_scope_roundtrip() {
        json_roundtrip(BindingEntry {
            screen: Screen::Settings,
            scope: BindingScope::Exact(oxpath!("settings", "accounts")),
            mode: Some(Mode::Normal),
            key: KeyChord {
                modifiers: KeyModifierSet::default(),
                code: KeyCodeRepr::Char('a'),
            },
            command_id: CommandId("settings.account.add".to_string()),
        });
    }

    #[test]
    fn binding_entry_with_prefix_scope_roundtrip() {
        json_roundtrip(BindingEntry {
            screen: Screen::Settings,
            scope: BindingScope::Prefix(oxpath!("settings", "accounts")),
            mode: None,
            key: KeyChord {
                modifiers: KeyModifierSet::default(),
                code: KeyCodeRepr::Char('t'),
            },
            command_id: CommandId("account.test".to_string()),
        });
    }

    #[test]
    fn binding_entry_anywhere_roundtrip() {
        json_roundtrip(BindingEntry {
            screen: Screen::Inbox,
            scope: BindingScope::Anywhere,
            mode: None,
            key: KeyChord {
                modifiers: KeyModifierSet {
                    ctrl: true,
                    ..Default::default()
                },
                code: KeyCodeRepr::Char('s'),
            },
            command_id: CommandId("inbox.send".to_string()),
        });
    }

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
