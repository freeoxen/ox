//! Durable worker-ingress evidence and applied-mark helpers.

use ox_kernel::{Record, Value, path};
use sha2::{Digest as _, Sha256};
use structfs_core_store::Writer as _;

/// Stable identity for the currently unresolved durable approval evidence.
/// Runtime-only waiter state is deliberately excluded so the identity survives
/// restart. A post-crash re-request for the same durable `ToolCall.id` keeps
/// the same identity; a later tool call necessarily has a distinct tool ID.
pub fn derive_unresolved_approval_id(
    thread_id: &str,
    entries: &[ox_kernel::log::LogEntry],
) -> Option<String> {
    let request_index = entries
        .iter()
        .rposition(|entry| matches!(entry, ox_kernel::log::LogEntry::ApprovalRequested { .. }))?;
    if entries[request_index + 1..].iter().any(|entry| {
        matches!(
            entry,
            ox_kernel::log::LogEntry::ApprovalResolved { .. }
                | ox_kernel::log::LogEntry::TurnEnd { .. }
                | ox_kernel::log::LogEntry::TurnAborted { .. }
        )
    }) {
        return None;
    }
    let tool_name = match &entries[request_index] {
        ox_kernel::log::LogEntry::ApprovalRequested { tool_name, .. } => tool_name,
        _ => unreachable!(),
    };
    let (tool_id, preceding_name) =
        entries[..request_index]
            .iter()
            .rev()
            .find_map(|entry| match entry {
                ox_kernel::log::LogEntry::ToolCall { id, name, .. } => Some((id, name)),
                _ => None,
            })?;
    if preceding_name != tool_name {
        return None;
    }
    let mut digest = Sha256::new();
    for value in ["ox-approval-v1", thread_id, tool_id] {
        digest.update((value.len() as u64).to_be_bytes());
        digest.update(value.as_bytes());
    }
    Some(format!("a_{:x}", digest.finalize()))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IngressPromptState {
    Missing,
    MarkerOnly,
    UserWithoutTurn,
    InFlight,
    Terminal,
    Conflict,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IngressControlEvidence {
    Missing,
    MarkerOnly,
    Applied,
}

pub(crate) fn append_ingress_marker(
    adapter: &mut ox_broker::SyncClientAdapter,
    operation: &str,
    semantic_id: &str,
    request_hash: &str,
) -> Result<(), String> {
    adapter
        .write(
            &path!("log/append"),
            Record::parsed(ingress_marker_value(operation, semantic_id, request_hash)),
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
}

pub(crate) fn ingress_prompt_state(
    adapter: &mut ox_broker::SyncClientAdapter,
    operation: &str,
    semantic_id: &str,
    request_hash: &str,
) -> Result<IngressPromptState, String> {
    let entries = adapter
        .read_typed::<Vec<ox_kernel::log::LogEntry>>(&path!("log/entries"))
        .map_err(|error| error.to_string())?
        .unwrap_or_default();
    Ok(classify_ingress_prompt(
        &entries,
        operation,
        semantic_id,
        request_hash,
    ))
}

pub(crate) fn classify_ingress_prompt(
    entries: &[ox_kernel::log::LogEntry],
    operation: &str,
    semantic_id: &str,
    request_hash: &str,
) -> IngressPromptState {
    let Some(marker_index) = entries.iter().rposition(|entry| match entry {
        ox_kernel::log::LogEntry::Meta { data } => {
            data.get("kind").and_then(serde_json::Value::as_str) == Some("worker_ingress")
                && data.get("operation").and_then(serde_json::Value::as_str) == Some(operation)
                && data.get("semantic_id").and_then(serde_json::Value::as_str) == Some(semantic_id)
        }
        _ => false,
    }) else {
        return IngressPromptState::Missing;
    };
    if let ox_kernel::log::LogEntry::Meta { data } = &entries[marker_index]
        && data.get("request_hash").and_then(serde_json::Value::as_str) != Some(request_hash)
    {
        return IngressPromptState::Conflict;
    }
    let after = &entries[marker_index + 1..];
    if !matches!(after.first(), Some(ox_kernel::log::LogEntry::User { .. })) {
        return IngressPromptState::MarkerOnly;
    }
    let mut saw_turn_start = false;
    for entry in &after[1..] {
        match entry {
            ox_kernel::log::LogEntry::TurnStart { .. } => saw_turn_start = true,
            ox_kernel::log::LogEntry::TurnEnd { .. }
            | ox_kernel::log::LogEntry::TurnAborted { .. } => {
                return IngressPromptState::Terminal;
            }
            ox_kernel::log::LogEntry::Meta { data }
                if data.get("kind").and_then(serde_json::Value::as_str)
                    == Some("worker_ingress") =>
            {
                break;
            }
            _ => {}
        }
    }
    if saw_turn_start {
        IngressPromptState::InFlight
    } else {
        IngressPromptState::UserWithoutTurn
    }
}

fn ingress_control_evidence(
    entries: &[ox_kernel::log::LogEntry],
    operation: &str,
    semantic_id: &str,
    request_hash: &str,
    applied: impl Fn(&ox_kernel::log::LogEntry) -> bool,
) -> Result<IngressControlEvidence, String> {
    let Some(marker_index) = entries.iter().rposition(|entry| match entry {
        ox_kernel::log::LogEntry::Meta { data } => {
            data.get("kind").and_then(serde_json::Value::as_str) == Some("worker_ingress")
                && data.get("operation").and_then(serde_json::Value::as_str) == Some(operation)
                && data.get("semantic_id").and_then(serde_json::Value::as_str) == Some(semantic_id)
        }
        _ => false,
    }) else {
        return Ok(IngressControlEvidence::Missing);
    };
    if let ox_kernel::log::LogEntry::Meta { data } = &entries[marker_index]
        && data.get("request_hash").and_then(serde_json::Value::as_str) != Some(request_hash)
    {
        return Err(format!(
            "conflict: durable {operation} marker for '{semantic_id}' has a different request hash"
        ));
    }
    if entries[marker_index + 1..].iter().any(applied) {
        Ok(IngressControlEvidence::Applied)
    } else {
        Ok(IngressControlEvidence::MarkerOnly)
    }
}

pub(crate) fn ingress_cancel_evidence(
    entries: &[ox_kernel::log::LogEntry],
    semantic_id: &str,
    request_hash: &str,
) -> Result<IngressControlEvidence, String> {
    ingress_control_evidence(entries, "cancel", semantic_id, request_hash, |entry| {
        matches!(
            entry,
            ox_kernel::log::LogEntry::TurnAborted {
                reason: ox_kernel::log::TurnAbortReason::UserCanceled
            }
        )
    })
}

pub(crate) fn ingress_decision_evidence(
    entries: &[ox_kernel::log::LogEntry],
    semantic_id: &str,
    request_hash: &str,
    requested: ox_types::Decision,
) -> Result<IngressControlEvidence, String> {
    let evidence =
        ingress_control_evidence(entries, "decision", semantic_id, request_hash, |entry| {
            matches!(
                entry,
                ox_kernel::log::LogEntry::ApprovalResolved { decision, .. }
                    if *decision == requested
            )
        })?;
    if evidence == IngressControlEvidence::MarkerOnly {
        let marker_index = entries.iter().rposition(|entry| match entry {
            ox_kernel::log::LogEntry::Meta { data } => {
                data.get("kind").and_then(serde_json::Value::as_str) == Some("worker_ingress")
                    && data.get("operation").and_then(serde_json::Value::as_str) == Some("decision")
                    && data.get("semantic_id").and_then(serde_json::Value::as_str)
                        == Some(semantic_id)
            }
            _ => false,
        });
        if marker_index.is_some_and(|index| {
            entries[index + 1..].iter().any(|entry| {
                matches!(
                    entry,
                    ox_kernel::log::LogEntry::ApprovalResolved { decision, .. }
                        if *decision != requested
                )
            })
        }) {
            return Err(format!(
                "conflict: approval '{semantic_id}' was durably resolved with a different decision"
            ));
        }
    }
    Ok(evidence)
}

pub(crate) async fn dispatch_worker_decision_task(
    client: ox_broker::ClientHandle,
    thread_id: String,
    semantic_id: String,
    request_hash: String,
    envelope: ox_inbox::worker_ingress::DecisionEnvelope,
    failpoints: crate::agents::IngressFailpoints,
) {
    let scoped = client.scoped(&format!("threads/{thread_id}"));
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut retry_delay = std::time::Duration::from_millis(10);
    loop {
        let entries = match scoped
            .read_typed::<Vec<ox_kernel::log::LogEntry>>(&path!("log/entries"))
            .await
        {
            Ok(Some(entries)) => entries,
            Ok(None) => Vec::new(),
            Err(error) => {
                tracing::error!(%semantic_id, %error, "decision ingress evidence read failed");
                return;
            }
        };
        let evidence = match ingress_decision_evidence(
            &entries,
            &semantic_id,
            &request_hash,
            envelope.decision,
        ) {
            Ok(evidence) => evidence,
            Err(error) => {
                tracing::error!(%semantic_id, %error, "decision ingress conflict");
                return;
            }
        };
        if evidence == IngressControlEvidence::Applied {
            mark_ingress_control_applied(
                &client,
                "decisions",
                &thread_id,
                &semantic_id,
                "approvals",
            )
            .await;
            return;
        }
        let pending = match scoped.read(&path!("approval/pending")).await {
            Ok(pending) => pending,
            Err(error) => {
                tracing::error!(%semantic_id, %error, "decision ingress pending read failed");
                return;
            }
        };
        if pending.as_ref().and_then(Record::as_value) != Some(&Value::Null) && pending.is_some() {
            if evidence == IngressControlEvidence::Missing {
                let marker = ingress_marker_value("decision", &semantic_id, &request_hash);
                if let Err(error) = scoped
                    .write(&path!("log/append"), Record::parsed(marker))
                    .await
                {
                    tracing::error!(%semantic_id, %error, "decision ingress marker append failed");
                    return;
                }
            }
            let conditional_path = match structfs_core_store::Path::parse(&format!(
                "approval/respond_if/{semantic_id}"
            )) {
                Ok(path) => path,
                Err(error) => {
                    tracing::error!(%semantic_id, %error, "invalid approval identity path");
                    return;
                }
            };
            if let Err(error) = scoped
                .write_typed(
                    &conditional_path,
                    &ox_types::ApprovalResponse {
                        decision: envelope.decision,
                    },
                )
                .await
            {
                tracing::error!(%semantic_id, %error, "decision ingress response failed");
                return;
            }
            if failpoints.take(crate::agents::IngressBoundary::AfterDecisionResponseBeforeMark) {
                return;
            }
            mark_ingress_control_applied(
                &client,
                "decisions",
                &thread_id,
                &semantic_id,
                "approvals",
            )
            .await;
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            tracing::warn!(
                %semantic_id,
                "decision ingress timed out waiting for its approval; leaving it accepted for reconciliation"
            );
            return;
        }
        tokio::time::sleep(retry_delay).await;
        retry_delay = (retry_delay * 2).min(std::time::Duration::from_millis(250));
    }
}

pub(crate) async fn mark_ingress_control_applied(
    client: &ox_broker::ClientHandle,
    kind: &str,
    thread_id: &str,
    semantic_id: &str,
    result_kind: &str,
) {
    let encoded_id = encode_ingress_path_id(semantic_id);
    let Ok(path) =
        structfs_core_store::Path::parse(&format!("inbox/worker/{kind}/{encoded_id}/applied"))
    else {
        return;
    };
    let result = format!("conversations/{thread_id}/{result_kind}/{semantic_id}");
    if let Err(error) = client
        .write(&path, Record::parsed(Value::String(result)))
        .await
    {
        tracing::error!(%semantic_id, %error, "failed to mark ingress control applied");
    }
}

fn ingress_marker_value(operation: &str, semantic_id: &str, request_hash: &str) -> Value {
    structfs_serde_store::json_to_value(serde_json::json!({
        "type": "meta",
        "data": {
            "kind": "worker_ingress",
            "operation": operation,
            "semantic_id": semantic_id,
            "request_hash": request_hash,
        }
    }))
}

pub(crate) fn mark_ingress_prompt_applied(
    broker_client: &ox_broker::ClientHandle,
    rt_handle: &tokio::runtime::Handle,
    thread_id: &str,
    kind: ox_inbox::worker_ingress::IntentKind,
    semantic_id: &str,
) {
    let encoded_id = encode_ingress_path_id(semantic_id);
    let path = structfs_core_store::Path::parse(&format!(
        "inbox/worker/{}/{encoded_id}/applied",
        kind.path_component()
    ));
    let result = if kind == ox_inbox::worker_ingress::IntentKind::Create {
        format!("conversations/{thread_id}")
    } else {
        format!("conversations/{thread_id}/ledger")
    };
    if let Ok(path) = path
        && let Err(error) =
            rt_handle.block_on(broker_client.write(&path, Record::parsed(Value::String(result))))
    {
        tracing::error!(%semantic_id, %error, "failed to mark ingress prompt applied");
    }
}

fn encode_ingress_path_id(id: &str) -> String {
    let mut encoded = String::with_capacity(1 + id.len() * 2);
    encoded.push('i');
    for byte in id.as_bytes() {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

#[cfg(test)]
mod approval_identity_tests {
    use super::*;
    use ox_kernel::log::LogEntry;

    fn call(id: &str, name: &str) -> LogEntry {
        LogEntry::ToolCall {
            id: id.into(),
            name: name.into(),
            input: serde_json::json!({}),
            scope: None,
        }
    }
    fn request(name: &str, reconfirm: bool) -> LogEntry {
        LogEntry::ApprovalRequested {
            tool_name: name.into(),
            input_preview: String::new(),
            post_crash_reconfirm: reconfirm,
        }
    }

    #[test]
    fn approval_identity_is_strict_and_stable_across_reconfirm() {
        let first = vec![call("tool-1", "shell"), request("shell", false)];
        let reconfirm = vec![
            call("tool-1", "shell"),
            request("shell", false),
            request("shell", true),
        ];
        assert_eq!(
            derive_unresolved_approval_id("t_a", &first),
            derive_unresolved_approval_id("t_a", &reconfirm)
        );
        assert_ne!(
            derive_unresolved_approval_id("t_a", &first),
            derive_unresolved_approval_id("t_b", &first)
        );
        assert_eq!(
            derive_unresolved_approval_id("t_a", &[request("shell", false)]),
            None
        );
        assert_eq!(
            derive_unresolved_approval_id("t_a", &[call("x", "fs"), request("shell", false)]),
            None
        );
        let resolved = vec![
            call("tool-1", "shell"),
            request("shell", false),
            LogEntry::ApprovalResolved {
                tool_name: "shell".into(),
                decision: ox_types::Decision::AllowOnce,
            },
        ];
        assert_eq!(derive_unresolved_approval_id("t_a", &resolved), None);
    }
}
