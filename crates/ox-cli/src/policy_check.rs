//! CliPolicyCheck — wraps PolicyGuard + broker approval flow.
//!
//! Implements [`ox_tools::policy_store::PolicyCheck`] for use in the CLI agent
//! worker. Evaluates tool calls against the Clash policy, blocking for TUI
//! approval when the policy returns Ask.

use ox_tools::policy_store::{PolicyCheck, PolicyDecision};
use structfs_core_store::{Path, Record};

use crate::policy::{CheckResult, PolicyGuard};

/// Policy check implementation for the CLI agent worker.
///
/// On `Ask` decisions, writes an approval request through the broker's
/// `approval/request` path and blocks until the TUI responds via
/// `approval/response`. The decision is encoded in the returned path.
pub(crate) struct CliPolicyCheck {
    pub guard: PolicyGuard,
    scoped_client: ox_broker::ClientHandle,
    broker_client: ox_broker::ClientHandle,
    thread_id: String,
    rt_handle: tokio::runtime::Handle,
}

impl CliPolicyCheck {
    pub fn new(
        guard: PolicyGuard,
        scoped_client: ox_broker::ClientHandle,
        broker_client: ox_broker::ClientHandle,
        thread_id: String,
        rt_handle: tokio::runtime::Handle,
    ) -> Self {
        Self {
            guard,
            scoped_client,
            broker_client,
            thread_id,
            rt_handle,
        }
    }

    /// Extract the tool wire name from a ToolStore path.
    ///
    /// Paths arrive as either wire names (`read_file`) or internal paths
    /// (`fs/read`). The first component is the tool name or module prefix.
    fn path_to_tool_name(path: &Path) -> String {
        if path.is_empty() {
            return String::new();
        }
        // Wire names are the first component. For internal paths like
        // "fs/read", the ToolStore resolves wire→internal before dispatching,
        // but PolicyStore sits in front, so we see the raw path which uses
        // wire names (e.g. "read_file", "shell").
        path.components[0].clone()
    }

    /// Convert a StructFS Record to a serde_json::Value for PolicyGuard.
    fn record_to_json(data: &Record) -> serde_json::Value {
        match data.as_value() {
            Some(v) => structfs_serde_store::value_to_json(v.clone()),
            None => serde_json::Value::Null,
        }
    }

    /// Update inbox thread state via the unscoped broker client.
    fn set_inbox_state(&self, state: ox_types::ThreadState) {
        if let Ok(tid_comp) = ox_kernel::PathComponent::try_new(&self.thread_id) {
            let update = ox_types::UpdateThread {
                id: None,
                thread_state: Some(state),
                inbox_state: None,
                updated_at: None,
            };
            self.rt_handle
                .block_on(
                    self.broker_client
                        .write_typed(&ox_path::oxpath!("inbox", "threads", tid_comp), &update),
                )
                .ok();
        }
    }

    /// Handle the Ask flow: write approval request to broker, block for response.
    fn handle_ask(&mut self, tool: &str, input: &serde_json::Value) -> PolicyDecision {
        let req = ox_types::ApprovalRequest {
            tool_name: tool.to_string(),
            tool_input: input.clone(),
        };

        // Signal blocked state to the inbox
        self.set_inbox_state(ox_types::ThreadState::BlockedOnApproval);

        // Write to approval/request — this blocks until the TUI responds.
        // Use Duration::MAX so deliberation time is never capped by the
        // broker's default 30-second timeout.
        let approval_client = self.scoped_client.with_timeout(std::time::Duration::MAX);
        let result = self.rt_handle.block_on(
            approval_client.write_typed(&structfs_core_store::path!("approval/request"), &req),
        );

        // Back to running after user responds
        self.set_inbox_state(ox_types::ThreadState::Running);

        let decision_str = match result {
            Ok(returned_path) => {
                // Decision encoded in path: "request/{decision}"
                if returned_path.components.len() >= 2 {
                    returned_path.components[1].clone()
                } else {
                    "deny_once".to_string()
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "approval request failed, defaulting to deny");
                return PolicyDecision::Deny(format!("approval error: {e}"));
            }
        };

        // Parse the decision string back to the enum. The path is a transport
        // mechanism (StructFS paths are always strings), so we deserialize here.
        let decision: ox_types::Decision = serde_json::from_value(serde_json::Value::String(
            decision_str.clone(),
        ))
        .unwrap_or_else(|_| {
            tracing::warn!(decision = %decision_str, "unknown approval decision, denying");
            ox_types::Decision::DenyOnce
        });

        match decision {
            ox_types::Decision::AllowOnce => PolicyDecision::Allow,
            ox_types::Decision::AllowSession => {
                let input = serde_json::json!({});
                self.guard.session_allow(tool, &input);
                PolicyDecision::Allow
            }
            ox_types::Decision::AllowAlways => {
                let input = serde_json::json!({});
                self.guard.persist_allow(tool, &input);
                PolicyDecision::Allow
            }
            ox_types::Decision::DenyOnce => PolicyDecision::Deny(format!("denied by user: {tool}")),
            ox_types::Decision::DenySession => {
                let input = serde_json::json!({});
                self.guard.session_deny(tool, &input);
                PolicyDecision::Deny(format!("denied by user (session): {tool}"))
            }
            ox_types::Decision::DenyAlways => {
                let input = serde_json::json!({});
                self.guard.persist_deny(tool, &input);
                PolicyDecision::Deny(format!("denied by user (always): {tool}"))
            }
        }
    }
}

impl PolicyCheck for CliPolicyCheck {
    fn check(&mut self, path: &Path, data: &Record) -> PolicyDecision {
        let tool_name = Self::path_to_tool_name(path);
        if tool_name.is_empty() {
            return PolicyDecision::Allow;
        }

        // Skip policy checks for internal plumbing paths — these are not
        // user-facing tool invocations.
        match tool_name.as_str() {
            "schemas" | "completions" | "turn" => return PolicyDecision::Allow,
            _ => {}
        }

        let input = Self::record_to_json(data);

        match self.guard.check(&tool_name, &input) {
            CheckResult::Allow => PolicyDecision::Allow,
            CheckResult::Deny(reason) => PolicyDecision::Deny(reason),
            CheckResult::Ask { tool, .. } => self.handle_ask(&tool, &input),
        }
    }
}

// Safety: PolicyGuard contains no interior mutability across threads.
// ClientHandle and Handle are documented as Send+Sync.
unsafe impl Sync for CliPolicyCheck {}
