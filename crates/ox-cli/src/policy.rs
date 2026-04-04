//! Policy enforcement for tool calls.
//!
//! Evaluates tool invocations against a rule set and returns Allow/Deny/Ask.
//! The rule format and evaluation model match [clash](https://clash.rs) —
//! when the clash crate is available, PolicyGuard delegates to it directly.

use std::path::{Path, PathBuf};

/// The effect a policy rule produces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    Allow,
    Deny,
    Ask,
}

/// Filesystem access entry in a sandbox configuration.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FsEntry {
    pub path: String,
    pub read: bool,
    pub write: bool,
    pub create: bool,
    pub delete: bool,
}

impl FsEntry {
    pub fn perms_display(&self) -> String {
        format!(
            "{}{}{}{}",
            if self.read { "r" } else { "-" },
            if self.write { "w" } else { "-" },
            if self.create { "c" } else { "-" },
            if self.delete { "d" } else { "-" },
        )
    }
}

/// Sandbox configuration — constraints on what an allowed tool can access.
/// Maps to clash's kernel-enforced sandbox model (Landlock/Seatbelt).
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SandboxConfig {
    /// Whether the tool can make network requests.
    #[serde(default = "default_true")]
    pub network: bool,
    /// Filesystem access rules.
    #[serde(default)]
    pub fs: Vec<FsEntry>,
}

fn default_true() -> bool {
    true
}

/// A policy rule: match a tool name (and optionally input patterns), produce an effect.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Rule {
    /// Tool name to match (None = match all tools).
    pub tool: Option<String>,
    /// Optional argument key to match on (e.g. "command", "path").
    pub arg_key: Option<String>,
    /// Optional argument pattern (glob-style) to match the value.
    pub arg_pattern: Option<String>,
    /// The effect when this rule matches.
    pub effect: String, // "allow", "deny", "ask"
    /// Optional sandbox constraints for allowed tools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sandbox: Option<SandboxConfig>,
}

impl Rule {
    fn effect(&self) -> Effect {
        match self.effect.as_str() {
            "allow" => Effect::Allow,
            "deny" => Effect::Deny,
            _ => Effect::Ask,
        }
    }

    fn matches(&self, tool_name: &str, input: &serde_json::Value) -> bool {
        // Tool name check
        if let Some(ref t) = self.tool {
            if t != tool_name {
                return false;
            }
        }
        // Argument check
        if let Some(ref key) = self.arg_key {
            let val = input.get(key).and_then(|v| v.as_str()).unwrap_or("");
            if let Some(ref pattern) = self.arg_pattern {
                if !glob_match(pattern, val) {
                    return false;
                }
            }
        }
        true
    }
}

/// Simple glob matching — supports * as wildcard.
fn glob_match(pattern: &str, value: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if let Some(suffix) = pattern.strip_prefix('*') {
        return value.ends_with(suffix);
    }
    if let Some(prefix) = pattern.strip_suffix('*') {
        return value.starts_with(prefix);
    }
    if pattern.contains('*') {
        let parts: Vec<&str> = pattern.split('*').collect();
        if parts.len() == 2 {
            return value.starts_with(parts[0]) && value.ends_with(parts[1]);
        }
    }
    pattern == value
}

/// A policy manifest: default effect + ordered rules (first match wins).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PolicyManifest {
    pub default: String, // "allow", "deny", "ask"
    pub rules: Vec<Rule>,
}

impl PolicyManifest {
    pub fn permissive() -> Self {
        Self {
            default: "allow".to_string(),
            rules: vec![],
        }
    }
}

impl Default for PolicyManifest {
    fn default() -> Self {
        // Default: allow reads, ask for everything else
        Self {
            default: "ask".to_string(),
            rules: vec![
                Rule {
                    tool: Some("read_file".into()),
                    arg_key: None,
                    arg_pattern: None,
                    effect: "allow".into(),
                    sandbox: None,
                },
            ],
        }
    }
}

/// Result of evaluating a tool call against the policy.
#[derive(Debug, Clone)]
pub enum PolicyDecision {
    Allow,
    Deny(String),
    Ask {
        tool: String,
        input_preview: String,
    },
}

/// Counters for policy decisions (displayed in status bar).
#[derive(Debug, Clone, Default)]
pub struct PolicyStats {
    pub allowed: u32,
    pub denied: u32,
    pub asked: u32,
}

/// Policy enforcement guard. Loads rules, evaluates tool calls, persists edits.
///
/// Three rule layers, checked in order (first match wins):
/// 1. Session rules — in-memory, lost on exit
/// 2. Persistent rules — from `.ox/policy.json`
/// 3. Default effect — from manifest
pub struct PolicyGuard {
    session_rules: Vec<Rule>,
    manifest: PolicyManifest,
    policy_path: PathBuf,
}

impl PolicyGuard {
    /// Load policy from the workspace. Tries `.ox/policy.json`, falls back to default.
    pub fn load(workspace: &Path) -> Self {
        let policy_path = workspace.join(".ox").join("policy.json");
        let manifest = if policy_path.exists() {
            match std::fs::read_to_string(&policy_path) {
                Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
                Err(_) => PolicyManifest::default(),
            }
        } else {
            PolicyManifest::default()
        };
        Self {
            session_rules: Vec::new(),
            manifest,
            policy_path,
        }
    }

    /// Create a guard that allows everything (--no-policy mode).
    pub fn permissive() -> Self {
        Self {
            session_rules: Vec::new(),
            manifest: PolicyManifest::permissive(),
            policy_path: PathBuf::new(),
        }
    }

    /// Evaluate a tool call against the policy.
    /// Checks session rules first, then persistent rules, then default.
    pub fn check(&self, tool_name: &str, input: &serde_json::Value) -> PolicyDecision {
        // Session rules (highest priority)
        for rule in &self.session_rules {
            if rule.matches(tool_name, input) {
                return match rule.effect() {
                    Effect::Allow => PolicyDecision::Allow,
                    Effect::Deny => PolicyDecision::Deny("denied by session rule".into()),
                    Effect::Ask => PolicyDecision::Ask {
                        tool: tool_name.into(),
                        input_preview: format_input_preview(tool_name, input),
                    },
                };
            }
        }
        // Persistent rules
        for rule in &self.manifest.rules {
            if rule.matches(tool_name, input) {
                return match rule.effect() {
                    Effect::Allow => PolicyDecision::Allow,
                    Effect::Deny => PolicyDecision::Deny("denied by policy".into()),
                    Effect::Ask => PolicyDecision::Ask {
                        tool: tool_name.into(),
                        input_preview: format_input_preview(tool_name, input),
                    },
                };
            }
        }
        // Fall through to default
        match self.manifest.default.as_str() {
            "allow" => PolicyDecision::Allow,
            "deny" => PolicyDecision::Deny("denied by default policy".into()),
            _ => PolicyDecision::Ask {
                tool: tool_name.into(),
                input_preview: format_input_preview(tool_name, input),
            },
        }
    }

    /// Add an arbitrary rule to the session layer.
    pub fn add_session_rule(&mut self, rule: Rule) {
        self.session_rules.insert(0, rule);
    }

    /// Add an arbitrary rule to the persistent layer and save.
    pub fn add_persistent_rule(&mut self, rule: Rule) {
        self.manifest.rules.insert(0, rule);
        self.save();
    }

    /// Add a session-scoped allow rule (in-memory, lost on exit).
    pub fn session_allow(&mut self, tool_name: &str, input: &serde_json::Value) {
        let rule = make_rule_from_call(tool_name, input, "allow");
        self.session_rules.insert(0, rule);
    }

    /// Add a session-scoped deny rule (in-memory, lost on exit).
    pub fn session_deny(&mut self, tool_name: &str, input: &serde_json::Value) {
        let rule = make_rule_from_call(tool_name, input, "deny");
        self.session_rules.insert(0, rule);
    }

    /// Add a persistent allow rule for this tool+input pattern.
    pub fn persist_allow(&mut self, tool_name: &str, input: &serde_json::Value) {
        let rule = make_rule_from_call(tool_name, input, "allow");
        self.manifest.rules.insert(0, rule);
        self.save();
    }

    /// Add a persistent deny rule for this tool+input pattern.
    pub fn persist_deny(&mut self, tool_name: &str, input: &serde_json::Value) {
        let rule = make_rule_from_call(tool_name, input, "deny");
        self.manifest.rules.insert(0, rule);
        self.save();
    }

    fn save(&self) {
        if self.policy_path.as_os_str().is_empty() {
            return; // permissive mode — no file
        }
        if let Some(parent) = self.policy_path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        if let Ok(json) = serde_json::to_string_pretty(&self.manifest) {
            std::fs::write(&self.policy_path, json).ok();
        }
    }
}

/// Format a human-readable preview of a tool call for the approval dialog.
fn format_input_preview(tool_name: &str, input: &serde_json::Value) -> String {
    match tool_name {
        "shell" => input
            .get("command")
            .and_then(|v| v.as_str())
            .unwrap_or("(unknown command)")
            .to_string(),
        "read_file" | "write_file" | "edit_file" => input
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("(unknown path)")
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

/// Generate a rule from a tool call. For shell commands, matches the command prefix.
/// For file tools, matches the exact path.
fn make_rule_from_call(tool_name: &str, input: &serde_json::Value, effect: &str) -> Rule {
    match tool_name {
        "shell" => {
            let cmd = input
                .get("command")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let binary = cmd.split_whitespace().next().unwrap_or(cmd);
            Rule {
                tool: Some("shell".into()),
                arg_key: Some("command".into()),
                arg_pattern: Some(format!("{binary}*")),
                effect: effect.into(),
                sandbox: None,
            }
        }
        "read_file" | "write_file" | "edit_file" => {
            let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("*");
            Rule {
                tool: Some(tool_name.into()),
                arg_key: Some("path".into()),
                arg_pattern: Some(path.to_string()),
                effect: effect.into(),
                sandbox: None,
            }
        }
        _ => Rule {
            tool: Some(tool_name.into()),
            arg_key: None,
            arg_pattern: None,
            effect: effect.into(),
            sandbox: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_allows_read() {
        let guard = PolicyGuard {
            session_rules: vec![],
            manifest: PolicyManifest::default(),
            policy_path: PathBuf::new(),
        };
        assert!(matches!(
            guard.check("read_file", &serde_json::json!({"path": "src/main.rs"})),
            PolicyDecision::Allow
        ));
    }

    #[test]
    fn default_policy_asks_for_shell() {
        let guard = PolicyGuard {
            session_rules: vec![],
            manifest: PolicyManifest::default(),
            policy_path: PathBuf::new(),
        };
        assert!(matches!(
            guard.check("shell", &serde_json::json!({"command": "rm -rf /"})),
            PolicyDecision::Ask { .. }
        ));
    }

    #[test]
    fn permissive_allows_everything() {
        let guard = PolicyGuard::permissive();
        assert!(matches!(
            guard.check("shell", &serde_json::json!({"command": "rm -rf /"})),
            PolicyDecision::Allow
        ));
    }

    #[test]
    fn deny_rule_blocks() {
        let guard = PolicyGuard {
            session_rules: vec![],
            manifest: PolicyManifest {
                default: "allow".into(),
                rules: vec![Rule {
                    tool: Some("shell".into()),
                    arg_key: Some("command".into()),
                    arg_pattern: Some("rm*".into()),
                    effect: "deny".into(),
                    sandbox: None,
                }],
            },
            policy_path: PathBuf::new(),
        };
        assert!(matches!(
            guard.check("shell", &serde_json::json!({"command": "rm -rf target/"})),
            PolicyDecision::Deny(_)
        ));
        // Non-matching command falls through to default (allow)
        assert!(matches!(
            guard.check("shell", &serde_json::json!({"command": "cargo test"})),
            PolicyDecision::Allow
        ));
    }

    #[test]
    fn first_matching_rule_wins() {
        let guard = PolicyGuard {
            session_rules: vec![],
            manifest: PolicyManifest {
                default: "deny".into(),
                rules: vec![
                    Rule {
                        tool: Some("shell".into()),
                        arg_key: Some("command".into()),
                        arg_pattern: Some("cargo*".into()),
                        effect: "allow".into(),
                        sandbox: None,
                    },
                    Rule {
                        tool: Some("shell".into()),
                        arg_key: None,
                        arg_pattern: None,
                        effect: "deny".into(),
                        sandbox: None,
                    },
                ],
            },
            policy_path: PathBuf::new(),
        };
        assert!(matches!(
            guard.check("shell", &serde_json::json!({"command": "cargo test"})),
            PolicyDecision::Allow
        ));
        assert!(matches!(
            guard.check("shell", &serde_json::json!({"command": "ls"})),
            PolicyDecision::Deny(_)
        ));
    }

    #[test]
    fn persist_allow_adds_rule() {
        let dir = tempfile::tempdir().unwrap();
        let policy_path = dir.path().join(".ox").join("policy.json");
        let mut guard = PolicyGuard {
            session_rules: vec![],
            manifest: PolicyManifest::default(),
            policy_path: policy_path.clone(),
        };

        // Shell commands start as "ask" (default)
        assert!(matches!(
            guard.check("shell", &serde_json::json!({"command": "cargo test"})),
            PolicyDecision::Ask { .. }
        ));

        // Persist allow
        guard.persist_allow("shell", &serde_json::json!({"command": "cargo test"}));

        // Now it's allowed
        assert!(matches!(
            guard.check("shell", &serde_json::json!({"command": "cargo build"})),
            PolicyDecision::Allow
        ));

        // File was written
        assert!(policy_path.exists());
    }

    #[test]
    fn glob_match_patterns() {
        assert!(glob_match("*", "anything"));
        assert!(glob_match("cargo*", "cargo test"));
        assert!(glob_match("*.rs", "main.rs"));
        assert!(glob_match("src/*", "src/main.rs"));
        assert!(!glob_match("cargo*", "npm test"));
        assert!(glob_match("exact", "exact"));
        assert!(!glob_match("exact", "not_exact"));
    }

    #[test]
    fn input_preview_formatting() {
        assert_eq!(
            format_input_preview("shell", &serde_json::json!({"command": "ls -la"})),
            "ls -la"
        );
        assert_eq!(
            format_input_preview("read_file", &serde_json::json!({"path": "src/main.rs"})),
            "src/main.rs"
        );
    }
}
