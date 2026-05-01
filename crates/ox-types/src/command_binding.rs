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

/// One row in the binding registry: under (screen, cursor, mode), the
/// keystroke `key` invokes `command_id`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingEntry {
    pub screen: Screen,
    #[serde(with = "path_serde::option", default)]
    pub cursor_path: Option<Path>,
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
    fn binding_entry_with_cursor_roundtrip() {
        json_roundtrip(BindingEntry {
            screen: Screen::Settings,
            cursor_path: Some(oxpath!("settings", "accounts")),
            mode: Some(Mode::Normal),
            key: KeyChord {
                modifiers: KeyModifierSet::default(),
                code: KeyCodeRepr::Char('a'),
            },
            command_id: CommandId("settings.account.add".to_string()),
        });
    }

    #[test]
    fn binding_entry_screen_wide_roundtrip() {
        json_roundtrip(BindingEntry {
            screen: Screen::Inbox,
            cursor_path: None,
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
}
