//! CommandRegistry — plain Rust lookup and validation for commands.
//!
//! No StructFS dependency in the registry itself. The resolve method
//! returns (Path, Record) for convenience but the core logic is pure
//! validation over serde_json::Value.

use std::collections::HashMap;

use structfs_core_store::{Path, Record};

use crate::builtin_commands::builtin_commands;
use crate::command_def::{CommandDef, CommandError, CommandInvocation, ParamDef, ParamKind};

pub struct CommandRegistry {
    commands: Vec<CommandDef>,
    by_name: HashMap<String, usize>,
}

impl CommandRegistry {
    pub fn new() -> Self {
        CommandRegistry {
            commands: Vec::new(),
            by_name: HashMap::new(),
        }
    }

    pub fn from_builtins() -> Self {
        let mut r = Self::new();
        for static_def in builtin_commands() {
            r.register(static_def.to_command_def())
                .expect("built-in commands must have unique names");
        }
        r
    }

    pub fn register(&mut self, def: CommandDef) -> Result<(), CommandError> {
        if self.by_name.contains_key(&def.name) {
            return Err(CommandError::DuplicateName { name: def.name });
        }
        let idx = self.commands.len();
        self.by_name.insert(def.name.clone(), idx);
        self.commands.push(def);
        Ok(())
    }

    pub fn unregister(&mut self, name: &str) -> Result<(), CommandError> {
        let idx = self
            .by_name
            .remove(name)
            .ok_or_else(|| CommandError::UnknownCommand {
                name: name.to_string(),
            })?;
        self.commands.remove(idx);
        // Rebuild index since indices shifted
        self.by_name.clear();
        for (i, cmd) in self.commands.iter().enumerate() {
            self.by_name.insert(cmd.name.clone(), i);
        }
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&CommandDef> {
        self.by_name.get(name).map(|&idx| &self.commands[idx])
    }

    pub fn iter(&self) -> impl Iterator<Item = &CommandDef> {
        self.commands.iter()
    }

    pub fn user_facing(&self) -> impl Iterator<Item = &CommandDef> {
        self.commands.iter().filter(|c| c.user_facing)
    }

    /// Validate and resolve an invocation to a target path + record.
    pub fn resolve(&self, invocation: &CommandInvocation) -> Result<(Path, Record), CommandError> {
        let def = self
            .get(&invocation.command)
            .ok_or_else(|| CommandError::UnknownCommand {
                name: invocation.command.clone(),
            })?;

        let mut args = invocation.args.clone();

        for param in &def.params {
            match args.get(&param.name) {
                Some(value) => {
                    validate_param_value(def, param, value)?;
                }
                None if param.required => {
                    if let Some(ref default) = param.default {
                        args.insert(param.name.clone(), default.clone());
                    } else {
                        return Err(CommandError::MissingParam {
                            command: def.name.clone(),
                            param: param.name.clone(),
                        });
                    }
                }
                None => {} // optional, no default — omit
            }
        }

        let path = Path::parse(&def.target).map_err(|e| CommandError::UnknownCommand {
            name: format!("{}: bad target path: {e}", def.name),
        })?;

        // Convert serde_json::Value args to structfs Value for the Record
        let structfs_value = structfs_serde_store::json_to_value(serde_json::Value::Object(
            args.into_iter().collect(),
        ));

        Ok((path, Record::parsed(structfs_value)))
    }
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}

fn validate_param_value(
    def: &CommandDef,
    param: &ParamDef,
    value: &serde_json::Value,
) -> Result<(), CommandError> {
    match &param.kind {
        ParamKind::String => {
            if !value.is_string() {
                return Err(CommandError::TypeMismatch {
                    command: def.name.clone(),
                    param: param.name.clone(),
                    expected: "String".into(),
                    got: json_type_name(value).into(),
                });
            }
        }
        ParamKind::Integer => {
            if !value.is_i64() && !value.is_u64() {
                return Err(CommandError::TypeMismatch {
                    command: def.name.clone(),
                    param: param.name.clone(),
                    expected: "Integer".into(),
                    got: json_type_name(value).into(),
                });
            }
        }
        ParamKind::Bool => {
            if !value.is_boolean() {
                return Err(CommandError::TypeMismatch {
                    command: def.name.clone(),
                    param: param.name.clone(),
                    expected: "Bool".into(),
                    got: json_type_name(value).into(),
                });
            }
        }
        ParamKind::Enum(allowed) => match value.as_str() {
            Some(s) => {
                if !allowed.iter().any(|a| a == s) {
                    return Err(CommandError::InvalidValue {
                        command: def.name.clone(),
                        param: param.name.clone(),
                        allowed: allowed.clone(),
                        got: s.to_string(),
                    });
                }
            }
            None => {
                return Err(CommandError::TypeMismatch {
                    command: def.name.clone(),
                    param: param.name.clone(),
                    expected: "String (enum)".into(),
                    got: json_type_name(value).into(),
                });
            }
        },
    }
    Ok(())
}

fn json_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "Null",
        serde_json::Value::Bool(_) => "Bool",
        serde_json::Value::Number(_) => "Number",
        serde_json::Value::String(_) => "String",
        serde_json::Value::Array(_) => "Array",
        serde_json::Value::Object(_) => "Object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command_def::ParamKind;
    use std::collections::BTreeMap;

    fn test_registry() -> CommandRegistry {
        let mut r = CommandRegistry::new();
        r.register(CommandDef {
            name: "quit".into(),
            target: "ui/quit".into(),
            params: vec![],
            description: "Quit".into(),
            user_facing: true,
        })
        .unwrap();
        r.register(CommandDef {
            name: "open".into(),
            target: "ui/open".into(),
            params: vec![ParamDef {
                name: "thread_id".into(),
                kind: ParamKind::String,
                required: true,
                default: None,
            }],
            description: "Open thread".into(),
            user_facing: true,
        })
        .unwrap();
        r.register(CommandDef {
            name: "compose".into(),
            target: "ui/enter_insert".into(),
            params: vec![ParamDef {
                name: "context".into(),
                kind: ParamKind::Enum(vec!["compose".into(), "reply".into()]),
                required: true,
                default: Some(serde_json::Value::String("compose".into())),
            }],
            description: "Compose".into(),
            user_facing: true,
        })
        .unwrap();
        r
    }

    #[test]
    fn get_existing_command() {
        let r = test_registry();
        assert!(r.get("quit").is_some());
        assert!(r.get("nonexistent").is_none());
    }

    #[test]
    fn duplicate_name_rejected() {
        let mut r = test_registry();
        let result = r.register(CommandDef {
            name: "quit".into(),
            target: "ui/quit2".into(),
            params: vec![],
            description: "Dupe".into(),
            user_facing: true,
        });
        assert!(matches!(result, Err(CommandError::DuplicateName { .. })));
    }

    #[test]
    fn resolve_no_params() {
        let r = test_registry();
        let inv = CommandInvocation {
            command: "quit".into(),
            args: BTreeMap::new(),
        };
        let (path, _record) = r.resolve(&inv).unwrap();
        assert_eq!(path.to_string(), "ui/quit");
    }

    #[test]
    fn resolve_with_required_param() {
        let r = test_registry();
        let mut args = BTreeMap::new();
        args.insert(
            "thread_id".into(),
            serde_json::Value::String("t_123".into()),
        );
        let inv = CommandInvocation {
            command: "open".into(),
            args,
        };
        let (path, _record) = r.resolve(&inv).unwrap();
        assert_eq!(path.to_string(), "ui/open");
    }

    #[test]
    fn resolve_missing_required_param_fails() {
        let r = test_registry();
        let inv = CommandInvocation {
            command: "open".into(),
            args: BTreeMap::new(),
        };
        let err = r.resolve(&inv).unwrap_err();
        assert!(matches!(err, CommandError::MissingParam { .. }));
    }

    #[test]
    fn resolve_applies_default() {
        let r = test_registry();
        let inv = CommandInvocation {
            command: "compose".into(),
            args: BTreeMap::new(),
        };
        let (path, _) = r.resolve(&inv).unwrap();
        assert_eq!(path.to_string(), "ui/enter_insert");
    }

    #[test]
    fn resolve_enum_wrong_value_fails() {
        let r = test_registry();
        let mut args = BTreeMap::new();
        args.insert("context".into(), serde_json::Value::String("bogus".into()));
        let inv = CommandInvocation {
            command: "compose".into(),
            args,
        };
        let err = r.resolve(&inv).unwrap_err();
        assert!(matches!(err, CommandError::InvalidValue { .. }));
    }

    #[test]
    fn resolve_type_mismatch_fails() {
        let r = test_registry();
        let mut args = BTreeMap::new();
        args.insert("thread_id".into(), serde_json::json!(42));
        let inv = CommandInvocation {
            command: "open".into(),
            args,
        };
        let err = r.resolve(&inv).unwrap_err();
        assert!(matches!(err, CommandError::TypeMismatch { .. }));
    }

    #[test]
    fn resolve_unknown_command_fails() {
        let r = test_registry();
        let inv = CommandInvocation {
            command: "nope".into(),
            args: BTreeMap::new(),
        };
        let err = r.resolve(&inv).unwrap_err();
        assert!(matches!(err, CommandError::UnknownCommand { .. }));
    }

    #[test]
    fn user_facing_filter() {
        let mut r = CommandRegistry::new();
        r.register(CommandDef {
            name: "visible".into(),
            target: "t".into(),
            params: vec![],
            description: "".into(),
            user_facing: true,
        })
        .unwrap();
        r.register(CommandDef {
            name: "hidden".into(),
            target: "t".into(),
            params: vec![],
            description: "".into(),
            user_facing: false,
        })
        .unwrap();
        let names: Vec<_> = r.user_facing().map(|c| c.name.as_str()).collect();
        assert!(names.contains(&"visible"));
        assert!(!names.contains(&"hidden"));
    }

    #[test]
    fn from_builtins_populates_registry() {
        let r = CommandRegistry::from_builtins();
        assert!(r.get("quit").is_some());
        assert!(r.get("compose").is_some());
        assert!(r.get("approve").is_some());
    }

    #[test]
    fn unregister_removes_command() {
        let mut r = test_registry();
        assert!(r.get("quit").is_some());
        r.unregister("quit").unwrap();
        assert!(r.get("quit").is_none());
        // Other commands still accessible
        assert!(r.get("open").is_some());
    }
}
