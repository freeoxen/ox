//! ClashSandboxPolicy — translates ox-tools AccessIntent into Clash sandbox
//! profiles for OS-level enforcement of tool execution.
//!
//! Each tool invocation gets an ephemeral per-call sandbox: `fs_read` of
//! `src/lib.rs` spawns a subprocess that can only read that file (plus
//! basic process operation). Shell commands get broader workspace access.

use std::path::PathBuf;

use clash::policy::sandbox_types::{
    Cap, NetworkPolicy, PathMatch, RuleEffect, SandboxPolicy as ClashPolicy, SandboxRule,
};
use ox_tools::sandbox::{AccessIntent, SandboxPolicy};

/// Clash-backed sandbox policy that compiles platform-specific profiles
/// from [`AccessIntent`] declarations and wraps commands with OS-level
/// enforcement (sandbox-exec on macOS, Landlock on Linux).
pub struct ClashSandboxPolicy {
    workspace: PathBuf,
}

impl ClashSandboxPolicy {
    pub fn new(workspace: PathBuf) -> Self {
        Self { workspace }
    }

    /// Translate an AccessIntent into a Clash SandboxPolicy.
    fn intent_to_policy(&self, intent: &AccessIntent) -> ClashPolicy {
        match intent {
            AccessIntent::ReadFile(path) => ClashPolicy {
                default: Cap::READ | Cap::EXECUTE,
                rules: vec![
                    // Allow reading the specific file
                    SandboxRule {
                        effect: RuleEffect::Allow,
                        caps: Cap::READ,
                        path: path.to_string_lossy().to_string(),
                        path_match: PathMatch::Literal,
                        doc: Some("allow reading the target file".into()),
                        follow_worktrees: false,
                    },
                    // Allow reading workspace (for executor binary resolution)
                    SandboxRule {
                        effect: RuleEffect::Allow,
                        caps: Cap::READ | Cap::EXECUTE,
                        path: self.workspace.to_string_lossy().to_string(),
                        path_match: PathMatch::Subpath,
                        doc: Some("workspace read access".into()),
                        follow_worktrees: false,
                    },
                ],
                network: NetworkPolicy::Deny,
                doc: Some(format!("read: {}", path.display())),
            },

            AccessIntent::WriteFile(path) => ClashPolicy {
                default: Cap::READ | Cap::EXECUTE,
                rules: vec![
                    SandboxRule {
                        effect: RuleEffect::Allow,
                        caps: Cap::READ | Cap::WRITE | Cap::CREATE,
                        path: path.to_string_lossy().to_string(),
                        path_match: PathMatch::Literal,
                        doc: Some("allow writing the target file".into()),
                        follow_worktrees: false,
                    },
                    // Allow creating parent directories
                    SandboxRule {
                        effect: RuleEffect::Allow,
                        caps: Cap::READ | Cap::WRITE | Cap::CREATE,
                        path: path.parent().unwrap_or(path).to_string_lossy().to_string(),
                        path_match: PathMatch::Subpath,
                        doc: Some("allow creating parent directories".into()),
                        follow_worktrees: false,
                    },
                    SandboxRule {
                        effect: RuleEffect::Allow,
                        caps: Cap::READ | Cap::EXECUTE,
                        path: self.workspace.to_string_lossy().to_string(),
                        path_match: PathMatch::Subpath,
                        doc: Some("workspace read access".into()),
                        follow_worktrees: false,
                    },
                ],
                network: NetworkPolicy::Deny,
                doc: Some(format!("write: {}", path.display())),
            },

            AccessIntent::ReadWriteFile(path) => ClashPolicy {
                default: Cap::READ | Cap::EXECUTE,
                rules: vec![
                    SandboxRule {
                        effect: RuleEffect::Allow,
                        caps: Cap::READ | Cap::WRITE | Cap::CREATE,
                        path: path.to_string_lossy().to_string(),
                        path_match: PathMatch::Literal,
                        doc: Some("allow read+write on target file".into()),
                        follow_worktrees: false,
                    },
                    SandboxRule {
                        effect: RuleEffect::Allow,
                        caps: Cap::READ | Cap::EXECUTE,
                        path: self.workspace.to_string_lossy().to_string(),
                        path_match: PathMatch::Subpath,
                        doc: Some("workspace read access".into()),
                        follow_worktrees: false,
                    },
                ],
                network: NetworkPolicy::Deny,
                doc: Some(format!("edit: {}", path.display())),
            },

            AccessIntent::ShellInWorkspace(workspace) => ClashPolicy {
                default: Cap::READ | Cap::EXECUTE,
                rules: vec![
                    // Full workspace access for shell commands
                    SandboxRule {
                        effect: RuleEffect::Allow,
                        caps: Cap::READ | Cap::WRITE | Cap::CREATE | Cap::DELETE | Cap::EXECUTE,
                        path: workspace.to_string_lossy().to_string(),
                        path_match: PathMatch::Subpath,
                        doc: Some("full workspace access for shell".into()),
                        follow_worktrees: false,
                    },
                    // Allow /tmp for scratch files
                    SandboxRule {
                        effect: RuleEffect::Allow,
                        caps: Cap::READ | Cap::WRITE | Cap::CREATE | Cap::DELETE,
                        path: "$TMPDIR".to_string(),
                        path_match: PathMatch::Subpath,
                        doc: Some("temp directory access".into()),
                        follow_worktrees: false,
                    },
                ],
                network: NetworkPolicy::Allow,
                doc: Some(format!("shell in: {}", workspace.display())),
            },
        }
    }
}

impl SandboxPolicy for ClashSandboxPolicy {
    fn apply(
        &self,
        intent: &AccessIntent,
        cmd: std::process::Command,
    ) -> Result<std::process::Command, String> {
        let clash_policy = self.intent_to_policy(intent);
        let cwd = match intent {
            AccessIntent::ShellInWorkspace(ws) => ws.clone(),
            _ => self.workspace.clone(),
        };

        // Compile to platform profile. Falls back to passthrough on
        // unsupported platforms (returns the command unchanged).
        let profile = match clash::sandbox::compile_sandbox_profile(&clash_policy, &cwd) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "sandbox profile compilation failed, running unsandboxed");
                return Ok(cmd);
            }
        };

        // On macOS: wrap with sandbox-exec -p <profile> -- <original command>
        let program = cmd.get_program().to_os_string();
        let args: Vec<_> = cmd.get_args().map(|a| a.to_os_string()).collect();

        let mut wrapped = std::process::Command::new("sandbox-exec");
        wrapped.args(["-p", &profile, "--"]);
        wrapped.arg(&program);
        for arg in &args {
            wrapped.arg(arg);
        }

        // Inherit environment and working directory from the original command
        if let Some(dir) = cmd.get_current_dir() {
            wrapped.current_dir(dir);
        }

        Ok(wrapped)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_intent_produces_deny_network_policy() {
        let policy = ClashSandboxPolicy::new(PathBuf::from("/workspace"));
        let clash = policy.intent_to_policy(&AccessIntent::ReadFile(PathBuf::from(
            "/workspace/src/lib.rs",
        )));
        assert!(matches!(clash.network, NetworkPolicy::Deny));
        assert!(clash.rules.iter().any(|r| r.path.contains("lib.rs")));
    }

    #[test]
    fn shell_intent_allows_network() {
        let policy = ClashSandboxPolicy::new(PathBuf::from("/workspace"));
        let clash =
            policy.intent_to_policy(&AccessIntent::ShellInWorkspace(PathBuf::from("/workspace")));
        assert!(matches!(clash.network, NetworkPolicy::Allow));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn apply_wraps_with_sandbox_exec() {
        let policy = ClashSandboxPolicy::new(PathBuf::from("/tmp/test-ws"));
        let cmd = std::process::Command::new("/usr/bin/echo");
        let intent = AccessIntent::ReadFile(PathBuf::from("/tmp/test-ws/file.txt"));
        let wrapped = policy.apply(&intent, cmd).unwrap();
        assert_eq!(wrapped.get_program(), "sandbox-exec");
    }
}
