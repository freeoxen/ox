//! Serializable command definition types.
//!
//! These types define the command vocabulary — what actions the system
//! can perform, what parameters they accept, and how to invoke them.
//! All types derive Serialize/Deserialize for StructFS Value round-tripping.
//!
//! Parameter defaults and invocation arguments use `serde_json::Value` as the
//! serializable representation. At the StructFS boundary, callers convert via
//! `structfs_serde_store::json_to_value` / `value_to_json`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A command definition — metadata about a single action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandDef {
    pub name: String,
    pub target: String,
    pub params: Vec<ParamDef>,
    pub description: String,
    pub user_facing: bool,
}

/// Parameter schema for a command.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParamDef {
    pub name: String,
    pub kind: ParamKind,
    pub required: bool,
    /// Default value, represented as JSON for serde round-tripping.
    /// Convert to `structfs_core_store::Value` via `structfs_serde_store::json_to_value`.
    pub default: Option<serde_json::Value>,
}

/// Expected value type for a parameter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ParamKind {
    String,
    Integer,
    Bool,
    Enum(Vec<String>),
}

/// A concrete request to execute a command with bound parameters.
///
/// Arguments use `serde_json::Value` as the serializable representation.
/// Convert to `structfs_core_store::Value` via `structfs_serde_store::json_to_value`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandInvocation {
    pub command: String,
    pub args: BTreeMap<String, serde_json::Value>,
}

/// Errors from command validation and resolution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CommandError {
    UnknownCommand { name: String },
    MissingParam { command: String, param: String },
    TypeMismatch { command: String, param: String, expected: String, got: String },
    InvalidValue { command: String, param: String, allowed: Vec<String>, got: String },
    DuplicateName { name: String },
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownCommand { name } => write!(f, "unknown command: {name}"),
            Self::MissingParam { command, param } => {
                write!(f, "{command}: missing required parameter '{param}'")
            }
            Self::TypeMismatch { command, param, expected, got } => {
                write!(f, "{command}: parameter '{param}' expected {expected}, got {got}")
            }
            Self::InvalidValue { command, param, allowed, got } => {
                write!(
                    f,
                    "{command}: parameter '{param}' must be one of {allowed:?}, got '{got}'"
                )
            }
            Self::DuplicateName { name } => write!(f, "command '{name}' already registered"),
        }
    }
}

impl std::error::Error for CommandError {}

// ---------------------------------------------------------------------------
// Static definition helpers (compile-time, zero-allocation)
// ---------------------------------------------------------------------------

pub struct StaticCommandDef {
    pub name: &'static str,
    pub target: &'static str,
    pub params: &'static [StaticParamDef],
    pub description: &'static str,
    pub user_facing: bool,
}

pub struct StaticParamDef {
    pub name: &'static str,
    pub kind: StaticParamKind,
    pub required: bool,
    pub default: Option<&'static str>,
}

pub enum StaticParamKind {
    String,
    Integer,
    Bool,
    Enum(&'static [&'static str]),
}

impl StaticCommandDef {
    pub fn to_command_def(&self) -> CommandDef {
        CommandDef {
            name: self.name.to_string(),
            target: self.target.to_string(),
            params: self.params.iter().map(|p| p.to_param_def()).collect(),
            description: self.description.to_string(),
            user_facing: self.user_facing,
        }
    }
}

impl StaticParamDef {
    pub fn to_param_def(&self) -> ParamDef {
        ParamDef {
            name: self.name.to_string(),
            kind: self.kind.to_param_kind(),
            required: self.required,
            default: self.default.map(|s| serde_json::Value::String(s.to_string())),
        }
    }
}

impl StaticParamKind {
    pub fn to_param_kind(&self) -> ParamKind {
        match self {
            Self::String => ParamKind::String,
            Self::Integer => ParamKind::Integer,
            Self::Bool => ParamKind::Bool,
            Self::Enum(values) => ParamKind::Enum(values.iter().map(|s| s.to_string()).collect()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_def_serializes_to_value() {
        let def = CommandDef {
            name: "compose".to_string(),
            target: "ui/enter_insert".to_string(),
            params: vec![ParamDef {
                name: "context".to_string(),
                kind: ParamKind::Enum(vec![
                    "compose".to_string(),
                    "reply".to_string(),
                    "search".to_string(),
                ]),
                required: true,
                default: Some(serde_json::Value::String("compose".to_string())),
            }],
            description: "Open compose input".to_string(),
            user_facing: true,
        };
        let value = structfs_serde_store::to_value(&def).unwrap();
        let round_tripped: CommandDef = structfs_serde_store::from_value(value).unwrap();
        assert_eq!(round_tripped.name, "compose");
        assert_eq!(round_tripped.params.len(), 1);
        assert!(round_tripped.user_facing);
    }

    #[test]
    fn command_invocation_round_trips() {
        let inv = CommandInvocation {
            command: "scroll_up".to_string(),
            args: std::collections::BTreeMap::new(),
        };
        let value = structfs_serde_store::to_value(&inv).unwrap();
        let round_tripped: CommandInvocation = structfs_serde_store::from_value(value).unwrap();
        assert_eq!(round_tripped.command, "scroll_up");
        assert!(round_tripped.args.is_empty());
    }

    #[test]
    fn command_error_serializes() {
        let err = CommandError::UnknownCommand { name: "bogus".to_string() };
        let value = structfs_serde_store::to_value(&err).unwrap();
        let round_tripped: CommandError = structfs_serde_store::from_value(value).unwrap();
        match round_tripped {
            CommandError::UnknownCommand { name } => assert_eq!(name, "bogus"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn static_def_converts_to_command_def() {
        let static_def = StaticCommandDef {
            name: "quit",
            target: "ui/quit",
            params: &[],
            description: "Quit the application",
            user_facing: true,
        };
        let def = static_def.to_command_def();
        assert_eq!(def.name, "quit");
        assert_eq!(def.target, "ui/quit");
        assert!(def.params.is_empty());
        assert!(def.user_facing);
    }

    #[test]
    fn static_def_with_enum_param_converts() {
        let static_def = StaticCommandDef {
            name: "compose",
            target: "ui/enter_insert",
            params: &[StaticParamDef {
                name: "context",
                kind: StaticParamKind::Enum(&["compose", "reply", "search"]),
                required: true,
                default: None,
            }],
            description: "Open compose input",
            user_facing: true,
        };
        let def = static_def.to_command_def();
        assert_eq!(def.params.len(), 1);
        assert_eq!(def.params[0].name, "context");
        match &def.params[0].kind {
            ParamKind::Enum(values) => {
                assert_eq!(values, &["compose", "reply", "search"]);
            }
            _ => panic!("expected Enum"),
        }
    }
}
