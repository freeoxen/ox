//! Command data shapes. The Command trait and registry impl land in
//! this file after Task 4 moves them from ox-cli.

use serde::{Deserialize, Serialize};
use structfs_core_store::Path;

use crate::path_serde;

/// Stable identifier for a registered command. Newtype around String so
/// command IDs can't accidentally swap with arbitrary text fields. Wire
/// shape is the bare string (`#[serde(transparent)]`).
#[derive(Hash, Eq, PartialEq, Clone, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CommandId(pub String);

/// Human-facing presentation of a command (palette / help / shortcut hints).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandDisplay {
    pub name: String,
    pub description: String,
}

/// The cursor scope a command applies to. `cursor_path = None` means
/// screen-wide. (Screen itself is no longer a framework concept; hosts
/// install one horns instance per screen at disjoint prefixes.)
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandScope {
    #[serde(with = "path_serde::option", default)]
    pub cursor_path: Option<Path>,
}

#[cfg(test)]
mod tests {
    use ox_path::oxpath;

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
        json_roundtrip(CommandScope { cursor_path: None });
    }

    #[test]
    fn command_scope_with_cursor_roundtrip() {
        json_roundtrip(CommandScope {
            cursor_path: Some(oxpath!("settings", "accounts")),
        });
    }
}
