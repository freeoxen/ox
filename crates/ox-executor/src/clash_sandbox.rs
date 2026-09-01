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

/// Whether inability to install an OS sandbox is compatible or fatal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SandboxEnforcement {
    /// Preserve the interactive CLI's historical platform fallback.
    BestEffort,
    /// Reject the invocation unless Clash enters a kernel-enforced sandbox.
    Required,
}

/// Clash-backed sandbox policy that compiles platform-specific profiles
/// from [`AccessIntent`] declarations and wraps commands with OS-level
/// enforcement (sandbox-exec on macOS, Landlock on Linux).
pub struct ClashSandboxPolicy {
    workspace: PathBuf,
    enforcement: SandboxEnforcement,
}

impl ClashSandboxPolicy {
    pub fn new(workspace: PathBuf) -> Self {
        Self {
            workspace,
            enforcement: SandboxEnforcement::BestEffort,
        }
    }

    pub fn required(workspace: PathBuf) -> Self {
        Self {
            workspace,
            enforcement: SandboxEnforcement::Required,
        }
    }

    /// Translate an AccessIntent into a Clash SandboxPolicy.
    fn intent_to_policy(&self, intent: &AccessIntent) -> ClashPolicy {
        if self.enforcement == SandboxEnforcement::Required {
            return self.remote_intent_to_policy(intent);
        }
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

    /// Fail-closed remote policy. Unlike the compatibility policy, this never
    /// grants READ on `/`: Clash's Linux backend translates default caps into
    /// a root Landlock grant, which would make every host file readable.
    fn remote_intent_to_policy(&self, intent: &AccessIntent) -> ClashPolicy {
        let mut rules = runtime_read_rules();
        let (network, doc) = match intent {
            AccessIntent::ReadFile(path) => {
                rules.push(allow_rule(path, Cap::READ, PathMatch::Literal));
                (
                    NetworkPolicy::Deny,
                    format!("remote read: {}", path.display()),
                )
            }
            AccessIntent::WriteFile(path) => {
                rules.push(allow_rule(
                    path.parent().unwrap_or(path),
                    Cap::READ | Cap::WRITE | Cap::CREATE,
                    PathMatch::Subpath,
                ));
                (
                    NetworkPolicy::Deny,
                    format!("remote write: {}", path.display()),
                )
            }
            AccessIntent::ReadWriteFile(path) => {
                rules.push(allow_rule(
                    path,
                    Cap::READ | Cap::WRITE | Cap::CREATE,
                    PathMatch::Literal,
                ));
                (
                    NetworkPolicy::Deny,
                    format!("remote edit: {}", path.display()),
                )
            }
            AccessIntent::ShellInWorkspace(workspace) => {
                rules.push(allow_rule(
                    workspace,
                    Cap::READ | Cap::WRITE | Cap::CREATE | Cap::DELETE | Cap::EXECUTE,
                    PathMatch::Subpath,
                ));
                (
                    NetworkPolicy::Deny,
                    format!("remote shell in: {}", workspace.display()),
                )
            }
        };
        ClashPolicy {
            default: Cap::empty(),
            rules,
            network,
            doc: Some(doc),
        }
    }
}

fn allow_rule(path: &std::path::Path, caps: Cap, path_match: PathMatch) -> SandboxRule {
    SandboxRule {
        effect: RuleEffect::Allow,
        caps,
        path: path.to_string_lossy().into_owned(),
        path_match,
        doc: Some("minimal tool runtime capability".into()),
        follow_worktrees: false,
    }
}

fn runtime_read_rules() -> Vec<SandboxRule> {
    // Programs and dynamic linkers needed by shell tools. No home, workspace,
    // /etc, /proc, or root-wide grant is implicit.
    [
        "/bin",
        "/usr/bin",
        "/usr/local/bin",
        "/lib",
        "/lib64",
        "/usr/lib",
        "/usr/local/lib",
        "/dev/null",
        "/System/Library",
    ]
    .into_iter()
    .filter(|path| std::path::Path::new(path).exists())
    .map(|path| {
        allow_rule(
            std::path::Path::new(path),
            Cap::READ | Cap::EXECUTE,
            PathMatch::Subpath,
        )
    })
    .collect()
}

impl SandboxPolicy for ClashSandboxPolicy {
    fn apply(
        &self,
        intent: &AccessIntent,
        cmd: std::process::Command,
    ) -> Result<std::process::Command, String> {
        let mut clash_policy = self.intent_to_policy(intent);
        let cwd = match intent {
            AccessIntent::ShellInWorkspace(ws) => ws.clone(),
            _ => self.workspace.clone(),
        };

        if self.enforcement == SandboxEnforcement::Required {
            let program = PathBuf::from(cmd.get_program());
            clash_policy.rules.push(allow_rule(
                &program,
                Cap::READ | Cap::EXECUTE,
                PathMatch::Literal,
            ));
            if let Ok(canonical) = program.canonicalize()
                && canonical != program
            {
                clash_policy.rules.push(allow_rule(
                    &canonical,
                    Cap::READ | Cap::EXECUTE,
                    PathMatch::Literal,
                ));
            }
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            {
                let policy_json = serde_json::to_string(&clash_policy)
                    .map_err(|error| format!("failed to serialize sandbox policy: {error}"))?;
                let program = cmd.get_program().to_os_string();
                let args: Vec<_> = cmd.get_args().map(|arg| arg.to_os_string()).collect();
                let mut wrapped = std::process::Command::new(program);
                wrapped
                    .env_clear()
                    .env("PATH", "/usr/local/bin:/usr/bin:/bin")
                    .env("HOME", &self.workspace)
                    .env("TMPDIR", &self.workspace)
                    .env("LANG", "C.UTF-8")
                    .env("LC_ALL", "C.UTF-8");
                wrapped
                    .arg("--clash-sandbox")
                    .arg(policy_json)
                    .arg(&cwd)
                    .args(args);
                if let Some(dir) = cmd.get_current_dir() {
                    wrapped.current_dir(dir);
                }
                return Ok(wrapped);
            }
            #[cfg(not(any(target_os = "linux", target_os = "macos")))]
            return Err("required Clash enforcement is unavailable on this platform".to_string());
        }

        // Best-effort is retained only for local compatibility. Linux Clash
        // cannot be applied by compiling a profile; historical local behavior
        // was passthrough there.
        #[cfg(target_os = "linux")]
        return Ok(cmd);

        #[cfg(not(target_os = "linux"))]
        let profile = match clash::sandbox::compile_sandbox_profile(&clash_policy, &cwd) {
            Ok(profile) => profile,
            Err(error) => {
                tracing::warn!(%error, "sandbox profile compilation failed, running unsandboxed");
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

    #[test]
    fn required_policy_has_no_root_read_default_and_denies_network() {
        let policy = ClashSandboxPolicy::required(PathBuf::from("/workspace"));
        let clash = policy
            .remote_intent_to_policy(&AccessIntent::ShellInWorkspace(PathBuf::from("/workspace")));
        assert!(clash.default.is_empty());
        assert!(matches!(clash.network, NetworkPolicy::Deny));
        assert!(!clash.rules.iter().any(|rule| rule.path == "/"));
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn required_policy_uses_clash_launcher() {
        let policy = ClashSandboxPolicy::required(PathBuf::from("/workspace"));
        let cmd = std::process::Command::new("/opt/ox/bin/ox-tool-exec");
        let wrapped = policy
            .apply(
                &AccessIntent::ReadFile(PathBuf::from("/workspace/file")),
                cmd,
            )
            .unwrap();
        assert_eq!(wrapped.get_program(), "/opt/ox/bin/ox-tool-exec");
        assert_eq!(
            wrapped.get_args().next().and_then(|arg| arg.to_str()),
            Some("--clash-sandbox")
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn required_policy_scrubs_environment_and_scopes_home_and_tmp() {
        let workspace = PathBuf::from("/conversation/workspace");
        let policy = ClashSandboxPolicy::required(workspace.clone());
        let mut cmd = std::process::Command::new("/opt/ox/bin/ox-tool-exec");
        cmd.env("OX_SENTINEL_SECRET", "must-not-survive");
        let wrapped = policy
            .apply(&AccessIntent::ReadFile(workspace.join("file")), cmd)
            .unwrap();
        let environment: std::collections::BTreeMap<_, _> = wrapped
            .get_envs()
            .filter_map(|(key, value)| value.map(|value| (key.to_owned(), value.to_owned())))
            .collect();
        assert!(!environment.contains_key(std::ffi::OsStr::new("OX_SENTINEL_SECRET")));
        assert_eq!(
            environment.get(std::ffi::OsStr::new("HOME")),
            Some(&workspace.as_os_str().to_owned())
        );
        assert_eq!(
            environment.get(std::ffi::OsStr::new("TMPDIR")),
            Some(&workspace.as_os_str().to_owned())
        );
    }
}
