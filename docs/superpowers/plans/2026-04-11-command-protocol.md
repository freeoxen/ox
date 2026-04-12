# Command Protocol Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Implement the command pattern and protocol from `docs/design/rfc/command-protocol.md` — a `CommandStore` (StructFS store) with a typed `CommandRegistry`, serializable command definitions, built-in catalog, and integration with the existing `InputStore` binding system.

**Architecture:** Static `CommandDef` types define the command vocabulary. A `CommandRegistry` (plain Rust, no StructFS dep) validates and resolves `CommandInvocation` values. A `CommandStore` wraps the registry as a StructFS Reader/Writer — reads discover commands, writes invoke them. The `InputStore` gains an `Action::Invoke` variant that dispatches through the `CommandStore` instead of directly to target paths.

**Tech Stack:** Rust, StructFS (structfs-core-store, structfs-serde-store), serde, ox-broker

**Spec:** `docs/design/rfc/command-protocol.md`

---

### Task 1: Static command types — `CommandDef`, `ParamDef`, `ParamKind`

**Files:**
- Create: `crates/ox-ui/src/command_def.rs`
- Modify: `crates/ox-ui/src/lib.rs`

- [ ] **Step 1: Write the failing test**

In `crates/ox-ui/src/command_def.rs`:

```rust
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
                default: Some(structfs_core_store::Value::String("compose".to_string())),
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
}
```

- [ ] **Step 2: Write the types to make the test compile**

In `crates/ox-ui/src/command_def.rs`, above the test module:

```rust
//! Serializable command definition types.
//!
//! These types define the command vocabulary — what actions the system
//! can perform, what parameters they accept, and how to invoke them.
//! All types derive Serialize/Deserialize for StructFS Value round-tripping.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use structfs_core_store::Value;

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
    pub default: Option<Value>,
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
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandInvocation {
    pub command: String,
    pub args: BTreeMap<String, Value>,
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
            Self::MissingParam { command, param } => write!(f, "{command}: missing required parameter '{param}'"),
            Self::TypeMismatch { command, param, expected, got } => write!(f, "{command}: parameter '{param}' expected {expected}, got {got}"),
            Self::InvalidValue { command, param, allowed, got } => write!(f, "{command}: parameter '{param}' must be one of {allowed:?}, got '{got}'"),
            Self::DuplicateName { name } => write!(f, "command '{name}' already registered"),
        }
    }
}

impl std::error::Error for CommandError {}
```

- [ ] **Step 3: Register the module in lib.rs**

In `crates/ox-ui/src/lib.rs`, add the module declaration and re-exports:

```rust
pub mod command_def;
```

And add re-exports:

```rust
pub use command_def::{CommandDef, CommandError, CommandInvocation, ParamDef, ParamKind};
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ox-ui command_def -- --nocapture`
Expected: 3 tests pass

- [ ] **Step 5: Commit**

```bash
git add crates/ox-ui/src/command_def.rs crates/ox-ui/src/lib.rs
git commit -m "feat: add serializable command definition types (CommandDef, ParamDef, CommandInvocation, CommandError)"
```

---

### Task 2: Static definition helpers — `StaticCommandDef`

**Files:**
- Modify: `crates/ox-ui/src/command_def.rs`
- Modify: `crates/ox-ui/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Append to the test module in `crates/ox-ui/src/command_def.rs`:

```rust
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
```

- [ ] **Step 2: Write the static types**

Add above the test module in `crates/ox-ui/src/command_def.rs`:

```rust
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
            default: self.default.map(|s| Value::String(s.to_string())),
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
```

- [ ] **Step 3: Add re-exports to lib.rs**

Add to the `pub use command_def::` line:

```rust
pub use command_def::{
    CommandDef, CommandError, CommandInvocation, ParamDef, ParamKind,
    StaticCommandDef, StaticParamDef, StaticParamKind,
};
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ox-ui command_def -- --nocapture`
Expected: 5 tests pass

- [ ] **Step 5: Commit**

```bash
git add crates/ox-ui/src/command_def.rs crates/ox-ui/src/lib.rs
git commit -m "feat: add StaticCommandDef for zero-allocation compile-time command definitions"
```

---

### Task 3: Built-in command catalog

**Files:**
- Create: `crates/ox-ui/src/builtin_commands.rs`
- Modify: `crates/ox-ui/src/lib.rs`

- [ ] **Step 1: Write the failing test**

In `crates/ox-ui/src/builtin_commands.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_not_empty() {
        let cmds = builtin_commands();
        assert!(!cmds.is_empty());
    }

    #[test]
    fn all_names_are_unique() {
        let cmds = builtin_commands();
        let mut names = std::collections::HashSet::new();
        for cmd in cmds {
            assert!(names.insert(cmd.name), "duplicate command name: {}", cmd.name);
        }
    }

    #[test]
    fn compose_command_exists() {
        let cmds = builtin_commands();
        let compose = cmds.iter().find(|c| c.name == "compose").expect("compose command missing");
        assert_eq!(compose.target, "ui/enter_insert");
        assert!(compose.user_facing);
        assert_eq!(compose.params.len(), 1);
        assert_eq!(compose.params[0].name, "context");
        assert!(compose.params[0].required);
    }

    #[test]
    fn quit_command_exists() {
        let cmds = builtin_commands();
        let quit = cmds.iter().find(|c| c.name == "quit").expect("quit command missing");
        assert_eq!(quit.target, "ui/quit");
        assert!(quit.user_facing);
        assert!(quit.params.is_empty());
    }

    #[test]
    fn internal_commands_not_user_facing() {
        let cmds = builtin_commands();
        let set_row = cmds.iter().find(|c| c.name == "set_row_count").expect("set_row_count missing");
        assert!(!set_row.user_facing);
    }

    #[test]
    fn all_convert_to_command_def() {
        let cmds = builtin_commands();
        for cmd in cmds {
            let def = cmd.to_command_def();
            assert!(!def.name.is_empty());
            assert!(!def.target.is_empty());
        }
    }
}
```

- [ ] **Step 2: Write the catalog**

In `crates/ox-ui/src/builtin_commands.rs`, above the test module:

```rust
//! Built-in command catalog — every action the system can perform.

use crate::command_def::{StaticCommandDef, StaticParamDef, StaticParamKind};

/// Returns the complete built-in command catalog.
pub fn builtin_commands() -> &'static [StaticCommandDef] {
    BUILTIN_COMMANDS
}

static BUILTIN_COMMANDS: &[StaticCommandDef] = &[
    // -- Navigation --
    StaticCommandDef { name: "select_next",          target: "ui/select_next",          params: &[], description: "Move selection down",     user_facing: true },
    StaticCommandDef { name: "select_prev",          target: "ui/select_prev",          params: &[], description: "Move selection up",       user_facing: true },
    StaticCommandDef { name: "select_first",         target: "ui/select_first",         params: &[], description: "Jump to first item",      user_facing: true },
    StaticCommandDef { name: "select_last",          target: "ui/select_last",          params: &[], description: "Jump to last item",       user_facing: true },
    StaticCommandDef { name: "scroll_up",            target: "ui/scroll_up",            params: &[], description: "Scroll viewport up",      user_facing: true },
    StaticCommandDef { name: "scroll_down",          target: "ui/scroll_down",          params: &[], description: "Scroll viewport down",    user_facing: true },
    StaticCommandDef { name: "scroll_to_top",        target: "ui/scroll_to_top",        params: &[], description: "Scroll to top",           user_facing: true },
    StaticCommandDef { name: "scroll_to_bottom",     target: "ui/scroll_to_bottom",     params: &[], description: "Scroll to bottom",        user_facing: true },
    StaticCommandDef { name: "scroll_page_up",       target: "ui/scroll_page_up",       params: &[], description: "Scroll one page up",      user_facing: true },
    StaticCommandDef { name: "scroll_page_down",     target: "ui/scroll_page_down",     params: &[], description: "Scroll one page down",    user_facing: true },
    StaticCommandDef { name: "scroll_half_page_up",  target: "ui/scroll_half_page_up",  params: &[], description: "Scroll half page up",     user_facing: true },
    StaticCommandDef { name: "scroll_half_page_down",target: "ui/scroll_half_page_down",params: &[], description: "Scroll half page down",   user_facing: true },

    // -- Screen transitions --
    StaticCommandDef { name: "open", target: "ui/open", params: &[
        StaticParamDef { name: "thread_id", kind: StaticParamKind::String, required: true, default: None },
    ], description: "Open a thread", user_facing: true },
    StaticCommandDef { name: "close",          target: "ui/close",          params: &[], description: "Back to inbox",                user_facing: true },
    StaticCommandDef { name: "settings",       target: "ui/go_to_settings", params: &[], description: "Open settings screen",         user_facing: true },
    StaticCommandDef { name: "inbox",          target: "ui/go_to_inbox",    params: &[], description: "Return to inbox",              user_facing: true },
    StaticCommandDef { name: "open_selected",  target: "ui/open_selected",  params: &[], description: "Open currently selected thread", user_facing: true },
    StaticCommandDef { name: "quit",           target: "ui/quit",           params: &[], description: "Quit the application",         user_facing: true },

    // -- Mode transitions --
    StaticCommandDef { name: "compose", target: "ui/enter_insert", params: &[
        StaticParamDef { name: "context", kind: StaticParamKind::Enum(&["compose", "reply", "search"]), required: true, default: Some("compose") },
    ], description: "Open compose input", user_facing: true },
    StaticCommandDef { name: "reply", target: "ui/enter_insert", params: &[
        StaticParamDef { name: "context", kind: StaticParamKind::Enum(&["compose", "reply", "search"]), required: true, default: Some("reply") },
    ], description: "Open reply input", user_facing: true },
    StaticCommandDef { name: "search", target: "ui/enter_insert", params: &[
        StaticParamDef { name: "context", kind: StaticParamKind::Enum(&["compose", "reply", "search"]), required: true, default: Some("search") },
    ], description: "Open search input", user_facing: true },
    StaticCommandDef { name: "exit_insert", target: "ui/exit_insert", params: &[], description: "Exit insert mode", user_facing: true },

    // -- Text input --
    StaticCommandDef { name: "send_input", target: "ui/send_input", params: &[], description: "Send current input",  user_facing: true },
    StaticCommandDef { name: "clear_input", target: "ui/clear_input", params: &[], description: "Clear input buffer", user_facing: true },

    // -- Thread actions --
    StaticCommandDef { name: "archive_selected", target: "ui/archive_selected", params: &[], description: "Archive selected thread", user_facing: true },

    // -- Search --
    StaticCommandDef { name: "search_insert_char", target: "ui/search_insert_char", params: &[
        StaticParamDef { name: "char", kind: StaticParamKind::String, required: true, default: None },
    ], description: "Append to search query", user_facing: false },
    StaticCommandDef { name: "search_delete_char", target: "ui/search_delete_char", params: &[], description: "Delete last search char",    user_facing: false },
    StaticCommandDef { name: "search_clear",       target: "ui/search_clear",       params: &[], description: "Clear search query",         user_facing: true },
    StaticCommandDef { name: "search_save_chip",   target: "ui/search_save_chip",   params: &[], description: "Save query as search chip",  user_facing: false },
    StaticCommandDef { name: "search_dismiss_chip", target: "ui/search_dismiss_chip", params: &[
        StaticParamDef { name: "index", kind: StaticParamKind::Integer, required: true, default: None },
    ], description: "Remove a search chip", user_facing: false },

    // -- Modals --
    StaticCommandDef { name: "show_modal",    target: "ui/show_modal",    params: &[], description: "Show a modal dialog",    user_facing: false },
    StaticCommandDef { name: "dismiss_modal", target: "ui/dismiss_modal", params: &[], description: "Dismiss current modal",  user_facing: true },

    // -- Approval --
    StaticCommandDef { name: "approve", target: "approval/response", params: &[
        StaticParamDef { name: "decision", kind: StaticParamKind::Enum(&["allow_once", "deny_once", "allow_session", "allow_always", "deny_always"]), required: true, default: None },
    ], description: "Respond to approval request", user_facing: true },

    // -- Internal --
    StaticCommandDef { name: "set_row_count", target: "ui/set_row_count", params: &[
        StaticParamDef { name: "count", kind: StaticParamKind::Integer, required: true, default: None },
    ], description: "Set list row count", user_facing: false },
    StaticCommandDef { name: "set_scroll_max", target: "ui/set_scroll_max", params: &[
        StaticParamDef { name: "max", kind: StaticParamKind::Integer, required: true, default: None },
    ], description: "Set max scroll position", user_facing: false },
    StaticCommandDef { name: "set_viewport_height", target: "ui/set_viewport_height", params: &[
        StaticParamDef { name: "height", kind: StaticParamKind::Integer, required: true, default: None },
    ], description: "Set viewport height", user_facing: false },
    StaticCommandDef { name: "set_input", target: "ui/set_input", params: &[
        StaticParamDef { name: "text", kind: StaticParamKind::String, required: false, default: None },
        StaticParamDef { name: "cursor", kind: StaticParamKind::Integer, required: false, default: None },
    ], description: "Set input content", user_facing: false },
    StaticCommandDef { name: "set_status", target: "ui/set_status", params: &[
        StaticParamDef { name: "text", kind: StaticParamKind::String, required: false, default: None },
    ], description: "Set status bar message", user_facing: false },
    StaticCommandDef { name: "clear_pending_action", target: "ui/clear_pending_action", params: &[], description: "Clear pending action flag", user_facing: false },
];
```

- [ ] **Step 3: Register the module in lib.rs**

In `crates/ox-ui/src/lib.rs`:

```rust
pub mod builtin_commands;
```

And add re-export:

```rust
pub use builtin_commands::builtin_commands;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ox-ui builtin_commands -- --nocapture`
Expected: 6 tests pass

- [ ] **Step 5: Commit**

```bash
git add crates/ox-ui/src/builtin_commands.rs crates/ox-ui/src/lib.rs
git commit -m "feat: add built-in command catalog with all 38 ox actions"
```

---

### Task 4: CommandRegistry — lookup + validation

**Files:**
- Create: `crates/ox-ui/src/command_registry.rs`
- Modify: `crates/ox-ui/src/lib.rs`

- [ ] **Step 1: Write the failing tests**

In `crates/ox-ui/src/command_registry.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::command_def::ParamKind;
    use structfs_core_store::Value;

    fn test_registry() -> CommandRegistry {
        let mut r = CommandRegistry::new();
        r.register(CommandDef {
            name: "quit".into(),
            target: "ui/quit".into(),
            params: vec![],
            description: "Quit".into(),
            user_facing: true,
        }).unwrap();
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
        }).unwrap();
        r.register(CommandDef {
            name: "compose".into(),
            target: "ui/enter_insert".into(),
            params: vec![ParamDef {
                name: "context".into(),
                kind: ParamKind::Enum(vec!["compose".into(), "reply".into()]),
                required: true,
                default: Some(Value::String("compose".into())),
            }],
            description: "Compose".into(),
            user_facing: true,
        }).unwrap();
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
        let inv = CommandInvocation { command: "quit".into(), args: BTreeMap::new() };
        let (path, _record) = r.resolve(&inv).unwrap();
        assert_eq!(path.to_string(), "ui/quit");
    }

    #[test]
    fn resolve_with_required_param() {
        let r = test_registry();
        let mut args = BTreeMap::new();
        args.insert("thread_id".into(), Value::String("t_123".into()));
        let inv = CommandInvocation { command: "open".into(), args };
        let (path, record) = r.resolve(&inv).unwrap();
        assert_eq!(path.to_string(), "ui/open");
        let map = match record.as_value().unwrap() {
            Value::Map(m) => m,
            _ => panic!("expected map"),
        };
        assert_eq!(map.get("thread_id"), Some(&Value::String("t_123".into())));
    }

    #[test]
    fn resolve_missing_required_param_fails() {
        let r = test_registry();
        let inv = CommandInvocation { command: "open".into(), args: BTreeMap::new() };
        let err = r.resolve(&inv).unwrap_err();
        assert!(matches!(err, CommandError::MissingParam { .. }));
    }

    #[test]
    fn resolve_applies_default() {
        let r = test_registry();
        let inv = CommandInvocation { command: "compose".into(), args: BTreeMap::new() };
        let (_path, record) = r.resolve(&inv).unwrap();
        let map = match record.as_value().unwrap() {
            Value::Map(m) => m,
            _ => panic!("expected map"),
        };
        assert_eq!(map.get("context"), Some(&Value::String("compose".into())));
    }

    #[test]
    fn resolve_enum_wrong_value_fails() {
        let r = test_registry();
        let mut args = BTreeMap::new();
        args.insert("context".into(), Value::String("bogus".into()));
        let inv = CommandInvocation { command: "compose".into(), args };
        let err = r.resolve(&inv).unwrap_err();
        assert!(matches!(err, CommandError::InvalidValue { .. }));
    }

    #[test]
    fn resolve_type_mismatch_fails() {
        let r = test_registry();
        let mut args = BTreeMap::new();
        args.insert("thread_id".into(), Value::Integer(42));
        let inv = CommandInvocation { command: "open".into(), args };
        let err = r.resolve(&inv).unwrap_err();
        assert!(matches!(err, CommandError::TypeMismatch { .. }));
    }

    #[test]
    fn resolve_unknown_command_fails() {
        let r = test_registry();
        let inv = CommandInvocation { command: "nope".into(), args: BTreeMap::new() };
        let err = r.resolve(&inv).unwrap_err();
        assert!(matches!(err, CommandError::UnknownCommand { .. }));
    }

    #[test]
    fn user_facing_filter() {
        let mut r = CommandRegistry::new();
        r.register(CommandDef {
            name: "visible".into(), target: "t".into(), params: vec![],
            description: "".into(), user_facing: true,
        }).unwrap();
        r.register(CommandDef {
            name: "hidden".into(), target: "t".into(), params: vec![],
            description: "".into(), user_facing: false,
        }).unwrap();
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
}
```

- [ ] **Step 2: Write the CommandRegistry implementation**

In `crates/ox-ui/src/command_registry.rs`, above the test module:

```rust
//! CommandRegistry — plain Rust lookup and validation for commands.
//!
//! No StructFS dependency. Independently testable.

use std::collections::{BTreeMap, HashMap};

use structfs_core_store::{Path, Record, Value};

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

    /// Create a registry pre-populated with all built-in commands.
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
    pub fn resolve(
        &self,
        invocation: &CommandInvocation,
    ) -> Result<(Path, Record), CommandError> {
        let def = self.get(&invocation.command).ok_or_else(|| {
            CommandError::UnknownCommand { name: invocation.command.clone() }
        })?;

        let mut args = invocation.args.clone();

        // Validate and apply defaults
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

        let path = Path::parse(&def.target).map_err(|e| {
            CommandError::UnknownCommand { name: format!("{}: bad target path: {e}", def.name) }
        })?;

        Ok((path, Record::parsed(Value::Map(args))))
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
    value: &Value,
) -> Result<(), CommandError> {
    match &param.kind {
        ParamKind::String => {
            if !matches!(value, Value::String(_)) {
                return Err(CommandError::TypeMismatch {
                    command: def.name.clone(),
                    param: param.name.clone(),
                    expected: "String".into(),
                    got: value_type_name(value).into(),
                });
            }
        }
        ParamKind::Integer => {
            if !matches!(value, Value::Integer(_)) {
                return Err(CommandError::TypeMismatch {
                    command: def.name.clone(),
                    param: param.name.clone(),
                    expected: "Integer".into(),
                    got: value_type_name(value).into(),
                });
            }
        }
        ParamKind::Bool => {
            if !matches!(value, Value::Bool(_)) {
                return Err(CommandError::TypeMismatch {
                    command: def.name.clone(),
                    param: param.name.clone(),
                    expected: "Bool".into(),
                    got: value_type_name(value).into(),
                });
            }
        }
        ParamKind::Enum(allowed) => {
            match value {
                Value::String(s) => {
                    if !allowed.contains(s) {
                        return Err(CommandError::InvalidValue {
                            command: def.name.clone(),
                            param: param.name.clone(),
                            allowed: allowed.clone(),
                            got: s.clone(),
                        });
                    }
                }
                _ => {
                    return Err(CommandError::TypeMismatch {
                        command: def.name.clone(),
                        param: param.name.clone(),
                        expected: "String (enum)".into(),
                        got: value_type_name(value).into(),
                    });
                }
            }
        }
    }
    Ok(())
}

fn value_type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "Null",
        Value::Bool(_) => "Bool",
        Value::Integer(_) => "Integer",
        Value::Float(_) => "Float",
        Value::String(_) => "String",
        Value::Array(_) => "Array",
        Value::Map(_) => "Map",
    }
}
```

- [ ] **Step 3: Register the module in lib.rs**

In `crates/ox-ui/src/lib.rs`:

```rust
pub mod command_registry;
```

And add re-export:

```rust
pub use command_registry::CommandRegistry;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ox-ui command_registry -- --nocapture`
Expected: 10 tests pass

- [ ] **Step 5: Commit**

```bash
git add crates/ox-ui/src/command_registry.rs crates/ox-ui/src/lib.rs
git commit -m "feat: add CommandRegistry with validation, resolution, and builtin population"
```

---

### Task 5: CommandStore — StructFS Reader/Writer

**Files:**
- Create: `crates/ox-ui/src/command_store.rs`
- Modify: `crates/ox-ui/src/lib.rs`

- [ ] **Step 1: Write the failing tests**

In `crates/ox-ui/src/command_store.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};
    use structfs_core_store::{Reader, Writer, path};

    type DispatchLog = Arc<Mutex<Vec<(String, BTreeMap<String, Value>)>>>;

    fn mock_dispatcher() -> (CommandDispatcher, DispatchLog) {
        let log: DispatchLog = Arc::new(Mutex::new(Vec::new()));
        let log_clone = log.clone();
        let dispatcher: CommandDispatcher = Box::new(move |path, data| {
            let fields = match data.as_value() {
                Some(Value::Map(m)) => m.clone(),
                _ => BTreeMap::new(),
            };
            log_clone.lock().unwrap().push((path.to_string(), fields));
            Ok(path.clone())
        });
        (dispatcher, log)
    }

    fn test_store() -> (CommandStore, DispatchLog) {
        let (dispatcher, log) = mock_dispatcher();
        let mut store = CommandStore::from_builtins();
        store.set_dispatcher(dispatcher);
        (store, log)
    }

    #[test]
    fn read_all_commands() {
        let (mut store, _) = test_store();
        let result = store.read(&path!("commands")).unwrap().unwrap();
        match result.as_value().unwrap() {
            Value::Array(arr) => assert!(!arr.is_empty()),
            _ => panic!("expected array"),
        }
    }

    #[test]
    fn read_single_command() {
        let (mut store, _) = test_store();
        let result = store.read(&path!("commands/quit")).unwrap().unwrap();
        match result.as_value().unwrap() {
            Value::Map(m) => {
                assert_eq!(m.get("name"), Some(&Value::String("quit".into())));
            }
            _ => panic!("expected map"),
        }
    }

    #[test]
    fn read_unknown_command_returns_none() {
        let (mut store, _) = test_store();
        let result = store.read(&path!("commands/nonexistent")).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn read_user_facing() {
        let (mut store, _) = test_store();
        let result = store.read(&path!("user_facing")).unwrap().unwrap();
        match result.as_value().unwrap() {
            Value::Array(arr) => {
                // All should be user_facing
                for item in arr {
                    if let Value::Map(m) = item {
                        assert_eq!(m.get("user_facing"), Some(&Value::Bool(true)));
                    }
                }
            }
            _ => panic!("expected array"),
        }
    }

    #[test]
    fn invoke_dispatches_to_target() {
        let (mut store, log) = test_store();
        let mut args = BTreeMap::new();
        args.insert("command".to_string(), Value::String("quit".into()));
        args.insert("args".to_string(), Value::Map(BTreeMap::new()));
        store.write(&path!("invoke"), Record::parsed(Value::Map(args))).unwrap();

        let log = log.lock().unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].0, "ui/quit");
    }

    #[test]
    fn invoke_validates_params() {
        let (mut store, _) = test_store();
        // open requires thread_id
        let mut args = BTreeMap::new();
        args.insert("command".to_string(), Value::String("open".into()));
        args.insert("args".to_string(), Value::Map(BTreeMap::new()));
        let result = store.write(&path!("invoke"), Record::parsed(Value::Map(args)));
        assert!(result.is_err());
    }

    #[test]
    fn register_adds_command() {
        let (mut store, _) = test_store();
        let mut def_map = BTreeMap::new();
        def_map.insert("name".to_string(), Value::String("custom".into()));
        def_map.insert("target".to_string(), Value::String("plugin/action".into()));
        def_map.insert("params".to_string(), Value::Array(vec![]));
        def_map.insert("description".to_string(), Value::String("Custom cmd".into()));
        def_map.insert("user_facing".to_string(), Value::Bool(true));
        store.write(&path!("register"), Record::parsed(Value::Map(def_map))).unwrap();

        // Should now be discoverable
        let result = store.read(&path!("commands/custom")).unwrap();
        assert!(result.is_some());
    }
}
```

- [ ] **Step 2: Write the CommandStore**

In `crates/ox-ui/src/command_store.rs`, above the test module:

```rust
//! CommandStore — StructFS Reader/Writer over CommandRegistry.
//!
//! Reads discover commands. Writes invoke or register them.

use std::collections::BTreeMap;

use structfs_core_store::{Error as StoreError, Path, Reader, Record, Value, Writer};

use crate::command_def::{CommandDef, CommandInvocation};
use crate::command_registry::CommandRegistry;

/// Callback for dispatching resolved commands to target stores.
pub type CommandDispatcher =
    Box<dyn FnMut(&Path, Record) -> Result<Path, StoreError> + Send + Sync>;

pub struct CommandStore {
    registry: CommandRegistry,
    dispatcher: Option<CommandDispatcher>,
}

impl CommandStore {
    pub fn new(registry: CommandRegistry) -> Self {
        CommandStore { registry, dispatcher: None }
    }

    /// Create a CommandStore pre-populated with all built-in commands.
    pub fn from_builtins() -> Self {
        Self::new(CommandRegistry::from_builtins())
    }

    pub fn set_dispatcher(&mut self, dispatcher: CommandDispatcher) {
        self.dispatcher = Some(dispatcher);
    }

    pub fn registry(&self) -> &CommandRegistry {
        &self.registry
    }
}

impl Reader for CommandStore {
    fn read(&mut self, from: &Path) -> Result<Option<Record>, StoreError> {
        let c = &from.components;
        match c.len() {
            0 => {
                // All commands
                let defs: Vec<Value> = self.registry.iter()
                    .map(|d| structfs_serde_store::to_value(d).unwrap())
                    .collect();
                Ok(Some(Record::parsed(Value::Array(defs))))
            }
            1 if c[0] == "commands" => {
                let defs: Vec<Value> = self.registry.iter()
                    .map(|d| structfs_serde_store::to_value(d).unwrap())
                    .collect();
                Ok(Some(Record::parsed(Value::Array(defs))))
            }
            1 if c[0] == "user_facing" => {
                let defs: Vec<Value> = self.registry.user_facing()
                    .map(|d| structfs_serde_store::to_value(d).unwrap())
                    .collect();
                Ok(Some(Record::parsed(Value::Array(defs))))
            }
            2 if c[0] == "commands" => {
                match self.registry.get(&c[1]) {
                    Some(def) => {
                        let value = structfs_serde_store::to_value(def).unwrap();
                        Ok(Some(Record::parsed(value)))
                    }
                    None => Ok(None),
                }
            }
            _ => Ok(None),
        }
    }
}

impl Writer for CommandStore {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        let action = if to.is_empty() { "" } else { to.components[0].as_str() };
        let value = data.as_value().ok_or_else(|| {
            StoreError::store("command", "write", "write data must contain a value")
        })?;

        match action {
            "invoke" => {
                let invocation: CommandInvocation =
                    structfs_serde_store::from_value(value.clone()).map_err(|e| {
                        StoreError::store("command", "invoke", format!("bad invocation: {e}"))
                    })?;
                let (path, record) = self.registry.resolve(&invocation).map_err(|e| {
                    StoreError::store("command", "invoke", e.to_string())
                })?;
                let dispatcher = self.dispatcher.as_mut().ok_or_else(|| {
                    StoreError::store("command", "invoke", "no dispatcher configured")
                })?;
                dispatcher(&path, record)
            }
            "register" => {
                let def: CommandDef =
                    structfs_serde_store::from_value(value.clone()).map_err(|e| {
                        StoreError::store("command", "register", format!("bad command def: {e}"))
                    })?;
                self.registry.register(def).map_err(|e| {
                    StoreError::store("command", "register", e.to_string())
                })?;
                Ok(Path::parse("commands").unwrap())
            }
            "unregister" => {
                let name = match value {
                    Value::Map(m) => m.get("name").and_then(|v| match v {
                        Value::String(s) => Some(s.clone()),
                        _ => None,
                    }),
                    Value::String(s) => Some(s.clone()),
                    _ => None,
                }.ok_or_else(|| {
                    StoreError::store("command", "unregister", "missing name")
                })?;
                self.registry.unregister(&name).map_err(|e| {
                    StoreError::store("command", "unregister", e.to_string())
                })?;
                Ok(Path::parse("commands").unwrap())
            }
            _ => Err(StoreError::store("command", "write", format!("unknown path: {action}"))),
        }
    }
}
```

- [ ] **Step 3: Add `unregister` to CommandRegistry**

In `crates/ox-ui/src/command_registry.rs`, add to `impl CommandRegistry`:

```rust
    pub fn unregister(&mut self, name: &str) -> Result<(), CommandError> {
        let idx = self.by_name.remove(name).ok_or_else(|| {
            CommandError::UnknownCommand { name: name.to_string() }
        })?;
        self.commands.remove(idx);
        // Rebuild index since indices shifted
        self.by_name.clear();
        for (i, cmd) in self.commands.iter().enumerate() {
            self.by_name.insert(cmd.name.clone(), i);
        }
        Ok(())
    }
```

- [ ] **Step 4: Register the module in lib.rs**

In `crates/ox-ui/src/lib.rs`:

```rust
pub mod command_store;
```

And add re-export:

```rust
pub use command_store::CommandStore;
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p ox-ui command_store -- --nocapture`
Expected: 7 tests pass

- [ ] **Step 6: Commit**

```bash
git add crates/ox-ui/src/command_store.rs crates/ox-ui/src/command_registry.rs crates/ox-ui/src/lib.rs
git commit -m "feat: add CommandStore — StructFS Reader/Writer for command discovery and invocation"
```

---

### Task 6: Add `Action::Invoke` to InputStore

**Files:**
- Modify: `crates/ox-ui/src/input_store.rs`

- [ ] **Step 1: Write the failing test**

Append to the test module in `crates/ox-ui/src/input_store.rs`:

```rust
    #[test]
    fn dispatch_invoke_action() {
        let bindings = vec![Binding {
            context: BindingContext {
                mode: "normal".to_string(),
                key: "q".to_string(),
                screen: None,
            },
            action: Action::Invoke {
                command: "quit".to_string(),
                args: BTreeMap::new(),
            },
            description: "quit".to_string(),
        }];
        let mut store = InputStore::new(bindings);
        let (dispatcher, log) = mock_dispatcher();
        store.set_dispatcher(dispatcher);

        store
            .write(&path!("key"), key_event("normal", "q", "inbox"))
            .unwrap();

        let log = log.lock().unwrap();
        assert_eq!(log.len(), 1);
        // Invoke writes to "command/invoke" with a CommandInvocation
        assert_eq!(log[0].0, "command/invoke");
        assert_eq!(log[0].1.get("command"), Some(&Value::String("quit".into())));
    }

    #[test]
    fn dispatch_invoke_with_args() {
        let mut args = BTreeMap::new();
        args.insert("context".to_string(), Value::String("compose".to_string()));
        let bindings = vec![Binding {
            context: BindingContext {
                mode: "normal".to_string(),
                key: "c".to_string(),
                screen: None,
            },
            action: Action::Invoke {
                command: "compose".to_string(),
                args,
            },
            description: "compose".to_string(),
        }];
        let mut store = InputStore::new(bindings);
        let (dispatcher, log) = mock_dispatcher();
        store.set_dispatcher(dispatcher);

        store
            .write(&path!("key"), key_event("normal", "c", "inbox"))
            .unwrap();

        let log = log.lock().unwrap();
        assert_eq!(log[0].0, "command/invoke");
        let inv_args = match log[0].1.get("args") {
            Some(Value::Map(m)) => m,
            _ => panic!("expected args map"),
        };
        assert_eq!(inv_args.get("context"), Some(&Value::String("compose".into())));
    }
```

- [ ] **Step 2: Add the Invoke variant to Action and handle it in execute_action**

In `crates/ox-ui/src/input_store.rs`, modify the `Action` enum:

```rust
pub enum Action {
    /// Write a single command to a target path.
    Command {
        target: Path,
        fields: Vec<ActionField>,
    },
    /// Command invocation through the command registry.
    Invoke {
        command: String,
        args: BTreeMap<String, Value>,
    },
    /// Execute a sequence of command actions in order.
    Macro(Vec<Action>),
}
```

Add `use std::collections::BTreeMap;` to the imports if not already present (it is already imported).

Then modify `execute_action` to handle the new variant:

```rust
    fn execute_action(
        &mut self,
        action: &Action,
        event_context: &BTreeMap<String, Value>,
    ) -> Result<Path, StoreError> {
        match action {
            Action::Command { target, fields } => {
                // ... existing code unchanged ...
            }
            Action::Invoke { command, args } => {
                // Build a CommandInvocation and dispatch to command/invoke
                let mut inv_map = BTreeMap::new();
                inv_map.insert("command".to_string(), Value::String(command.clone()));
                inv_map.insert("args".to_string(), Value::Map(args.clone()));
                let target = Path::parse("command/invoke").unwrap();
                let dispatcher = self.dispatcher.as_mut().ok_or_else(|| {
                    StoreError::store("input", "dispatch", "no dispatcher configured")
                })?;
                dispatcher(&target, Record::parsed(Value::Map(inv_map)))
            }
            Action::Macro(steps) => {
                // ... existing code unchanged ...
            }
        }
    }
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test -p ox-ui input_store -- --nocapture`
Expected: All existing tests pass + 2 new tests pass

- [ ] **Step 4: Commit**

```bash
git add crates/ox-ui/src/input_store.rs
git commit -m "feat: add Action::Invoke variant for command-based dispatch in InputStore"
```

---

### Task 7: Mount CommandStore in ox-cli broker

**Files:**
- Modify: `crates/ox-cli/src/broker_setup.rs`

- [ ] **Step 1: Write the failing test**

Append to the test module in `crates/ox-cli/src/broker_setup.rs`:

```rust
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn command_store_is_mounted() {
        let handle = make_test_handle().await;
        let client = handle.client();

        // Should be able to read the command catalog
        let result = client.read(&path!("command/commands")).await.unwrap();
        assert!(result.is_some());
        match result.unwrap().as_value().unwrap() {
            structfs_core_store::Value::Array(arr) => assert!(!arr.is_empty()),
            _ => panic!("expected array"),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn command_store_reads_single_command() {
        let handle = make_test_handle().await;
        let client = handle.client();

        let result = client.read(&path!("command/commands/quit")).await.unwrap();
        assert!(result.is_some());
    }
```

- [ ] **Step 2: Check the existing test helper and add the mount**

First, look at how `make_test_handle` works — it calls `setup()`. Add the CommandStore mount in `setup()`:

In `crates/ox-cli/src/broker_setup.rs`, add to the `setup` function, after the InputStore mount:

```rust
    // Mount CommandStore
    {
        let command_dispatch_client = broker.client();
        let mut command_store = ox_ui::CommandStore::from_builtins();
        command_store.set_dispatcher(Box::new(move |target, data| {
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current()
                    .block_on(command_dispatch_client.write(target, data))
            })
        }));
        servers.push(broker.mount(path!("command"), command_store).await);
    }
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test -p ox-cli broker_setup -- --nocapture`
Expected: All existing tests pass + 2 new tests pass

- [ ] **Step 4: Run the full test suite**

Run: `cargo test -p ox-ui && cargo test -p ox-cli`
Expected: All pass

- [ ] **Step 5: Commit**

```bash
git add crates/ox-cli/src/broker_setup.rs
git commit -m "feat: mount CommandStore at command/ in broker namespace"
```

---

### Task 8: Integration test — end-to-end invoke through CommandStore

**Files:**
- Modify: `crates/ox-ui/src/lib.rs` (integration test module)

- [ ] **Step 1: Write the integration test**

Add to the existing `integration_tests` module in `crates/ox-ui/src/lib.rs`:

```rust
    /// End-to-end: Action::Invoke → CommandStore → UiStore state change.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn invoke_action_through_command_store() {
        let broker = ox_broker::BrokerStore::default();

        // Mount UiStore with rows so selection can advance
        let mut ui = UiStore::new();
        ui.write(
            &path!("set_row_count"),
            Record::parsed(Value::Map({
                let mut m = BTreeMap::new();
                m.insert("count".to_string(), Value::Integer(10));
                m
            })),
        )
        .unwrap();
        broker.mount(path!("ui"), ui).await;

        // Mount CommandStore
        let cmd_client = broker.client();
        let mut cmd_store = CommandStore::from_builtins();
        cmd_store.set_dispatcher(Box::new(move |target, data| {
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(cmd_client.write(target, data))
            })
        }));
        broker.mount(path!("command"), cmd_store).await;

        // Mount InputStore that dispatches through CommandStore
        let input_client = broker.client();
        let bindings = vec![Binding {
            context: BindingContext {
                mode: "normal".to_string(),
                key: "j".to_string(),
                screen: None,
            },
            action: Action::Invoke {
                command: "select_next".to_string(),
                args: BTreeMap::new(),
            },
            description: "Move down".to_string(),
        }];
        let mut input = InputStore::new(bindings);
        input.set_dispatcher(Box::new(move |target, data| {
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(input_client.write(target, data))
            })
        }));
        broker.mount(path!("input"), input).await;

        let client = broker.client();

        // Verify initial state
        let row = client.read(&path!("ui/selected_row")).await.unwrap().unwrap();
        assert_eq!(row.as_value().unwrap(), &Value::Integer(0));

        // Dispatch "j" → InputStore → CommandStore → UiStore
        let mut event = BTreeMap::new();
        event.insert("mode".to_string(), Value::String("normal".to_string()));
        event.insert("key".to_string(), Value::String("j".to_string()));
        event.insert("screen".to_string(), Value::String("inbox".to_string()));
        client
            .write(&path!("input/key"), Record::parsed(Value::Map(event)))
            .await
            .unwrap();

        // Verify state changed
        let row = client.read(&path!("ui/selected_row")).await.unwrap().unwrap();
        assert_eq!(row.as_value().unwrap(), &Value::Integer(1));
    }
```

- [ ] **Step 2: Run the integration test**

Run: `cargo test -p ox-ui invoke_action_through_command_store -- --nocapture`
Expected: PASS

- [ ] **Step 3: Run the full workspace test suite**

Run: `cargo test -p ox-ui && cargo test -p ox-cli`
Expected: All pass

- [ ] **Step 4: Commit**

```bash
git add crates/ox-ui/src/lib.rs
git commit -m "test: add end-to-end integration test for Action::Invoke through CommandStore"
```

---

### Task 9: Keybinding changes

**Files:**
- Modify: `crates/ox-cli/src/bindings.rs`

- [ ] **Step 1: Change `i` → `c` for compose/reply**

In `crates/ox-cli/src/bindings.rs`, in `normal_mode()`, change the three `"i"` bindings:

```rust
    // Enter insert mode — screen determines context
    out.push(bind_screen(
        "normal",
        "c",
        "inbox",
        cmd_with("ui/enter_insert", vec![static_field("context", "compose")]),
        "Compose new thread",
    ));
    out.push(bind_screen(
        "normal",
        "c",
        "thread",
        cmd_with("ui/enter_insert", vec![static_field("context", "reply")]),
        "Reply in thread",
    ));
```

Remove the old `"i"` inbox and `"i"` thread bindings. Keep `"/"` for search unchanged.

- [ ] **Step 2: Change `Esc` → `Ctrl+q` for exit insert**

In `insert_mode()`, change:

```rust
    out.push(bind(
        "insert",
        "Ctrl+q",
        cmd("ui/exit_insert"),
        "Exit insert mode",
    ));
```

Remove the old `"Esc"` binding for exit_insert.

- [ ] **Step 3: Run tests to verify bindings still compile and pass**

Run: `cargo test -p ox-cli bindings -- --nocapture`
Expected: All existing tests pass (the tests check that modes have bindings and that descriptions are non-empty, not specific key names)

- [ ] **Step 4: Run the full test suite**

Run: `cargo test -p ox-cli`
Expected: All pass

- [ ] **Step 5: Commit**

```bash
git add crates/ox-cli/src/bindings.rs
git commit -m "feat: rebind i→c for compose/reply, Esc→Ctrl+q for exit insert"
```

---

### Task 10: Workspace check

**Files:** None (verification only)

- [ ] **Step 1: Run cargo check for all targets**

Run: `cargo check`
Expected: No errors

- [ ] **Step 2: Run the full test suite**

Run: `cargo test`
Expected: All tests pass

- [ ] **Step 3: Run clippy**

Run: `cargo clippy -- -D warnings`
Expected: No warnings

- [ ] **Step 4: Run format check**

Run: `cargo fmt -- --check`
Expected: No formatting issues (or run `cargo fmt` to fix)

- [ ] **Step 5: Final commit if any fixes needed**

```bash
git add -A
git commit -m "chore: fix clippy/fmt issues from command protocol implementation"
```

Plan complete and saved to `docs/superpowers/plans/2026-04-11-command-protocol.md`. Two execution options:

**1. Subagent-Driven (recommended)** - I dispatch a fresh subagent per task, review between tasks, fast iteration

**2. Inline Execution** - Execute tasks in this session using executing-plans, batch execution with checkpoints

Which approach?