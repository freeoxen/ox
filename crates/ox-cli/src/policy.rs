//! Policy enforcement for tool calls, backed by [clash](https://clash.rs).
//!
//! Evaluates tool invocations against a clash policy manifest (match-tree IR).
//! Policies are authored in Starlark or JSON, stored in `.clash/policy.json`.

// Policy methods are temporarily unused — they were called from HostEffects::execute_tool
// which has been removed. They will be re-wired when policy enforcement moves to ToolStore.
#![allow(dead_code)]

use clash::policy::Effect;
use clash::policy::manifest_edit;
use clash::policy::match_tree::{
    CompiledPolicy, Decision, Node, Observable, Pattern, PolicyManifest, QueryContext, Value,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Result of evaluating a tool call against the policy (simplified for TUI use).
#[allow(dead_code)]
pub enum CheckResult {
    Allow,
    Deny(String),
    Ask {
        tool: String,
        input_preview: String,
        /// Human-readable explanation of why this was asked.
        explanation: Vec<String>,
    },
}

/// Counters for policy decisions (displayed in status bar).
#[derive(Debug, Clone, Default)]
pub struct PolicyStats {
    pub allowed: u32,
    pub denied: u32,
    pub asked: u32,
}

/// Policy enforcement guard. Loads a clash policy manifest, evaluates tool calls,
/// and persists rule edits via clash's manifest_edit API.
pub struct PolicyGuard {
    /// Session-level compiled policy (in-memory, not persisted).
    session_policy: CompiledPolicy,
    /// Persistent policy manifest (loaded from and saved to disk).
    manifest: PolicyManifest,
    /// Path to the policy file.
    policy_path: PathBuf,
}

impl PolicyGuard {
    /// Load policy from the workspace. Tries `.clash/policy.json`, falls back to default.
    pub fn load(workspace: &Path) -> Self {
        let policy_path = workspace.join(".clash").join("policy.json");
        let manifest = if policy_path.exists() {
            match std::fs::read_to_string(&policy_path) {
                Ok(content) => {
                    serde_json::from_str(&content).unwrap_or_else(|_| default_manifest())
                }
                Err(_) => default_manifest(),
            }
        } else {
            default_manifest()
        };
        Self {
            session_policy: empty_policy(),
            manifest,
            policy_path,
        }
    }

    /// Create a guard that allows everything (--no-policy mode).
    pub fn permissive() -> Self {
        let mut manifest = default_manifest();
        manifest.policy.default_effect = Effect::Allow;
        Self {
            session_policy: empty_policy(),
            manifest,
            policy_path: PathBuf::new(),
        }
    }

    /// Evaluate a tool call against session rules first, then persistent policy.
    pub fn check(&self, tool_name: &str, input: &serde_json::Value) -> CheckResult {
        let ctx = build_query_context(tool_name, input);

        // Session rules (highest priority)
        let session_decision = self.session_policy.evaluate_ctx(&ctx);
        if session_decision.effect != Effect::Ask {
            return match session_decision.effect {
                Effect::Allow => CheckResult::Allow,
                Effect::Deny => CheckResult::Deny("denied by session rule".into()),
                Effect::Ask => unreachable!(),
            };
        }

        // Persistent policy
        let decision = self.manifest.policy.evaluate_ctx(&ctx);
        match decision.effect {
            Effect::Allow => CheckResult::Allow,
            Effect::Deny => {
                let reason = decision.reason.unwrap_or_else(|| "denied by policy".into());
                CheckResult::Deny(reason)
            }
            Effect::Ask => CheckResult::Ask {
                tool: tool_name.into(),
                input_preview: format_input_preview(tool_name, input),
                explanation: decision.human_explanation(),
            },
        }
    }

    /// Add a rule to the session-level policy (in-memory, lost on exit).
    pub fn session_allow(&mut self, tool_name: &str, input: &serde_json::Value) {
        let node = build_allow_node(tool_name, input);
        self.session_policy.tree.insert(0, node);
    }

    /// Add a deny rule to the session-level policy.
    pub fn session_deny(&mut self, tool_name: &str, input: &serde_json::Value) {
        let node = build_deny_node(tool_name, input);
        self.session_policy.tree.insert(0, node);
    }

    /// Add a persistent allow rule and save to disk.
    pub fn persist_allow(&mut self, tool_name: &str, input: &serde_json::Value) {
        let node = build_allow_node(tool_name, input);
        manifest_edit::upsert_rule(&mut self.manifest, node);
        self.save();
    }

    /// Add a persistent deny rule and save to disk.
    pub fn persist_deny(&mut self, tool_name: &str, input: &serde_json::Value) {
        let node = build_deny_node(tool_name, input);
        manifest_edit::upsert_rule(&mut self.manifest, node);
        self.save();
    }

    /// Add a named sandbox definition.
    #[allow(dead_code)]
    pub fn add_sandbox(
        &mut self,
        name: &str,
        sandbox: clash::policy::sandbox_types::SandboxPolicy,
        persist: bool,
    ) {
        if persist {
            self.manifest
                .policy
                .sandboxes
                .insert(name.to_string(), sandbox.clone());
            self.save();
        }
        self.session_policy
            .sandboxes
            .insert(name.to_string(), sandbox);
    }

    /// Insert an arbitrary node into the session policy.
    #[allow(dead_code)]
    pub fn add_session_node(&mut self, node: Node) {
        self.session_policy.tree.insert(0, node);
    }

    /// Insert an arbitrary node into the persistent policy and save.
    #[allow(dead_code)]
    pub fn add_persistent_node(&mut self, node: Node) {
        manifest_edit::upsert_rule(&mut self.manifest, node);
        self.save();
    }

    fn save(&self) {
        if self.policy_path.as_os_str().is_empty() {
            return;
        }
        if let Some(parent) = self.policy_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        if let Ok(json) = serde_json::to_string_pretty(&self.manifest) {
            std::fs::write(&self.policy_path, json).ok();
        }
    }
}

// ---------------------------------------------------------------------------
// Node construction helpers
// ---------------------------------------------------------------------------

/// Build a clash Node that matches a tool call and allows it.
fn build_allow_node(tool_name: &str, input: &serde_json::Value) -> Node {
    build_decision_node(tool_name, input, Decision::Allow(None))
}

/// Build a clash Node that matches a tool call and denies it.
fn build_deny_node(tool_name: &str, input: &serde_json::Value) -> Node {
    build_decision_node(tool_name, input, Decision::Deny)
}

/// Build a clash Node tree for a tool call with a given leaf decision.
fn build_decision_node(tool_name: &str, input: &serde_json::Value, decision: Decision) -> Node {
    match tool_name {
        "shell" => {
            let cmd = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
            let words: Vec<&str> = cmd.split_whitespace().collect();

            // Build nested conditions: ToolName → arg0 → arg1 → ... → Decision
            let leaf = Node::Decision(decision);
            let mut current = leaf;

            // Wrap from inside out — last arg first
            for (i, word) in words.iter().enumerate().rev() {
                current = Node::Condition {
                    observe: Observable::PositionalArg(i as i32),
                    pattern: Pattern::Literal(Value::Literal(word.to_string())),
                    children: vec![current],
                    doc: None,
                    source: None,
                    terminal: false,
                };
            }

            // Wrap with ToolName
            Node::Condition {
                observe: Observable::ToolName,
                pattern: Pattern::Literal(Value::Literal(tool_name.to_string())),
                children: vec![current],
                doc: None,
                source: Some("ox-cli".into()),
                terminal: false,
            }
        }
        "read_file" | "write_file" | "edit_file" => {
            let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("*");
            Node::Condition {
                observe: Observable::ToolName,
                pattern: Pattern::Literal(Value::Literal(tool_name.to_string())),
                children: vec![Node::Condition {
                    observe: Observable::NamedArg("path".into()),
                    pattern: Pattern::Literal(Value::Literal(path.to_string())),
                    children: vec![Node::Decision(decision)],
                    doc: None,
                    source: None,
                    terminal: false,
                }],
                doc: None,
                source: Some("ox-cli".into()),
                terminal: false,
            }
        }
        _ => Node::Condition {
            observe: Observable::ToolName,
            pattern: Pattern::Literal(Value::Literal(tool_name.to_string())),
            children: vec![Node::Decision(decision)],
            doc: None,
            source: Some("ox-cli".into()),
            terminal: false,
        },
    }
}

// ---------------------------------------------------------------------------
// Defaults
// ---------------------------------------------------------------------------

fn default_manifest() -> PolicyManifest {
    PolicyManifest {
        includes: vec![],
        policy: CompiledPolicy {
            sandboxes: HashMap::new(),
            tree: vec![
                // read_file → allow
                Node::Condition {
                    observe: Observable::ToolName,
                    pattern: Pattern::Literal(Value::Literal("read_file".into())),
                    children: vec![Node::Decision(Decision::Allow(None))],
                    doc: Some("standard: allow all file reads".into()),
                    source: Some("ox-cli-default".into()),
                    terminal: false,
                },
            ],
            default_effect: Effect::Ask,
            default_sandbox: None,
        },
    }
}

fn empty_policy() -> CompiledPolicy {
    CompiledPolicy {
        sandboxes: HashMap::new(),
        tree: vec![],
        default_effect: Effect::Ask,
        default_sandbox: None,
    }
}

/// Build a clash QueryContext for an ox tool call.
/// Handles positional arg parsing for shell commands (clash only does this for "Bash").
fn build_query_context(tool_name: &str, input: &serde_json::Value) -> QueryContext {
    let args = match tool_name {
        "shell" => {
            let cmd = input.get("command").and_then(|v| v.as_str()).unwrap_or("");
            cmd.split_whitespace().map(|s| s.to_string()).collect()
        }
        _ => vec![],
    };
    QueryContext {
        tool_name: tool_name.to_string(),
        args,
        tool_input: input.clone(),
        hook_type: None,
        agent_name: None,
        fs_op: None,
        fs_path: input
            .get("path")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        net_domain: None,
        mode: None,
    }
}

/// Format a human-readable preview of a tool call.
fn format_input_preview(tool_name: &str, input: &serde_json::Value) -> String {
    match tool_name {
        "shell" => input
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("(unknown)")
            .to_string(),
        "read_file" | "write_file" | "edit_file" => input
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("(unknown)")
            .to_string(),
        _ => {
            let s = serde_json::to_string(input).unwrap_or_default();
            if s.len() > 80 {
                format!("{}...", &s[..80])
            } else {
                s
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_allows_read() {
        let guard = PolicyGuard::load(Path::new("/nonexistent"));
        assert!(matches!(
            guard.check("read_file", &serde_json::json!({"path": "src/main.rs"})),
            CheckResult::Allow
        ));
    }

    #[test]
    fn default_policy_asks_for_shell() {
        let guard = PolicyGuard::load(Path::new("/nonexistent"));
        assert!(matches!(
            guard.check("shell", &serde_json::json!({"command": "rm -rf /"})),
            CheckResult::Ask { .. }
        ));
    }

    #[test]
    fn permissive_allows_everything() {
        let guard = PolicyGuard::permissive();
        assert!(matches!(
            guard.check("shell", &serde_json::json!({"command": "rm -rf /"})),
            CheckResult::Allow
        ));
    }

    #[test]
    fn persist_allow_is_precise() {
        let dir = tempfile::tempdir().unwrap();
        let policy_path = dir.path().join(".clash").join("policy.json");
        let mut guard = PolicyGuard {
            session_policy: empty_policy(),
            manifest: default_manifest(),
            policy_path: policy_path.clone(),
        };

        // Allow "cargo test" specifically
        guard.persist_allow("shell", &serde_json::json!({"command": "cargo test"}));

        // "cargo test" → allowed
        assert!(matches!(
            guard.check("shell", &serde_json::json!({"command": "cargo test"})),
            CheckResult::Allow
        ));

        // "cargo build" → still ask (different subcommand)
        assert!(matches!(
            guard.check("shell", &serde_json::json!({"command": "cargo build"})),
            CheckResult::Ask { .. }
        ));

        // File was saved in clash format
        assert!(policy_path.exists());
        let content = std::fs::read_to_string(&policy_path).unwrap();
        let _: PolicyManifest = serde_json::from_str(&content).unwrap();
    }

    #[test]
    fn session_rules_take_priority() {
        let mut guard = PolicyGuard::load(Path::new("/nonexistent"));

        // Default: shell → ask
        assert!(matches!(
            guard.check("shell", &serde_json::json!({"command": "ls"})),
            CheckResult::Ask { .. }
        ));

        // Add session allow
        guard.session_allow("shell", &serde_json::json!({"command": "ls"}));

        // Now: shell "ls" → allow
        assert!(matches!(
            guard.check("shell", &serde_json::json!({"command": "ls"})),
            CheckResult::Allow
        ));
    }
}
