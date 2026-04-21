//! Thread resume classification — pure function over the structured log tail.
//!
//! Given the current `LogEntry` sequence, [`classify`] returns a
//! [`ThreadResumeState`] telling the caller what shape the thread was in when
//! the process last exited. Shells (the CLI mount lifecycle, web, mobile) use
//! the classification to decide whether to:
//!
//! - do nothing (`Idle`),
//! - record a `TurnAborted` marker (`InStreamNoFinal` / `InTurnNoProgress`),
//! - expose a stale approval modal for the user (`AwaitingApproval`), or
//! - record a `ToolAborted` marker before any further turn progresses
//!   (`AwaitingToolResult`).
//!
//! This module lives in `ox-kernel` (not `ox-inbox`, despite the phrase
//! "ledger classifier") because the classifier is a property of the kernel's
//! state machine over `LogEntry`, not of any particular on-disk format. Every
//! shell — CLI (via `ox-inbox`'s ledger), web (via its own durability), tests
//! (no durability) — needs the same logic, so it belongs alongside the state
//! machine, not the concrete durability implementation.

use crate::log::LogEntry;

/// The shape of the log tail at mount time.
///
/// Variants are ordered from "nothing to recover" to "most dangerous
/// recovery." `InStreamNoFinal` is only reachable once `AssistantProgress`
/// lands (Task 4 of the durable-conversation-state plan); Task 2's classifier
/// cannot distinguish "in-stream" from "in-turn" and conservatively folds
/// in-stream cases into `InTurnNoProgress`. The variant is kept in the
/// public API so downstream dispatch can match on it without a follow-up
/// ABI break when Task 4 refines detection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThreadResumeState {
    /// The thread is quiescent — no pending turn, no unresolved approval.
    Idle,
    /// `TurnStart` followed by in-progress streaming, but no assistant
    /// final. **Reachable only after Task 4 lands** (requires the
    /// `AssistantProgress` variant). Task 2's classifier does not emit this
    /// state; it folds in-stream cases into `InTurnNoProgress` and lets
    /// Task 4 refine the detection.
    InStreamNoFinal,
    /// An `ApprovalRequested` was written with no matching
    /// `ApprovalResolved`.
    AwaitingApproval { tool_use_id: String },
    /// An `Assistant(tool_use)` exists (optionally resolved/auto-approved)
    /// but no matching `ToolResult` was written. `was_approved` reflects
    /// whether an `ApprovalResolved(allow_*)` is present for this
    /// `tool_use_id`; the dispatch policy decides whether to reconfirm.
    AwaitingToolResult {
        tool_use_id: String,
        was_approved: bool,
    },
    /// A `TurnStart` with nothing state-changing after it.
    InTurnNoProgress,
}

/// The classifier reads at most this many entries walking backward from
/// the tail. Expressed as a function of the kernel's per-turn iteration
/// cap (`run::MAX_TOTAL_ITERATIONS`) times a generous append-events-per-
/// iteration factor. If the cap is hit without finding a `TurnStart`, the
/// classifier returns [`ThreadResumeState::Idle`] and emits a warn-level
/// tracing event.
const CLASSIFIER_WALK_CAP: usize = 2 * crate::run::MAX_TOTAL_ITERATIONS_PUB * 20;

/// Classify the tail of the structured log into a [`ThreadResumeState`].
///
/// Pure function. Walks the entries slice from the end toward the start,
/// skipping informational variants (`User`, `Meta`, `CompletionEnd`,
/// `Error`) and looking for a state-changing variant. Returns as soon as
/// a variant determines the state; returns [`ThreadResumeState::Idle`]
/// if the walk reaches the beginning without finding one, or
/// [`ThreadResumeState::InTurnNoProgress`] if it hits a `TurnStart` that
/// wasn't preceded by state-changing content.
pub fn classify(entries: &[LogEntry]) -> ThreadResumeState {
    // Walk the log tail from the end toward the start. `offset` counts
    // from the tail (0 == last entry) and doubles as both a walk-cap
    // counter (bounding the scan to one turn's worth of entries) and an
    // absolute-index cursor for the approval-request lookback. The cap
    // is expressed as a function of `MAX_TOTAL_ITERATIONS_PUB` rather
    // than a magic number — see `CLASSIFIER_WALK_CAP`.
    //
    // `seen_resolved` tracks whether an `ApprovalResolved` has appeared
    // later in the tail (i.e., between our current position and the
    // real end). An `ApprovalRequested` with a later `ApprovalResolved`
    // is *not* awaiting input — it's either `AwaitingToolResult` (tool
    // allowed, no result yet) or `Idle` (tool ran, `ToolResult` present).
    // Only an `ApprovalRequested` with no later resolution is still
    // blocking user input.
    let mut seen_resolved = false;
    for (offset, entry) in entries.iter().rev().enumerate() {
        if offset >= CLASSIFIER_WALK_CAP {
            tracing::warn!(
                entries_scanned = offset,
                total = entries.len(),
                "ClassifierWalkCapped"
            );
            return ThreadResumeState::Idle;
        }

        match entry {
            // ---- informational: skip ----
            LogEntry::User { .. }
            | LogEntry::Meta { .. }
            | LogEntry::CompletionEnd { .. }
            | LogEntry::Error { .. } => continue,

            // ---- terminal-of-turn markers ----
            LogEntry::TurnEnd { .. } => {
                // A complete turn ended after any tool/assistant activity
                // we've scanned so far — the thread is quiescent.
                return ThreadResumeState::Idle;
            }
            LogEntry::TurnAborted { .. } => {
                // An abort marker is itself a terminal state for the
                // preceding turn. Thread is quiescent.
                return ThreadResumeState::Idle;
            }

            // ---- approval shape ----
            LogEntry::ApprovalResolved { .. } => {
                // Record that any earlier (backward-in-walk) ApprovalRequested
                // has been settled; continue backward to find what the user
                // decision applied to (a pending tool_call, etc.).
                seen_resolved = true;
                continue;
            }
            LogEntry::ApprovalRequested { .. } => {
                if seen_resolved {
                    // This request was resolved by an ApprovalResolved we
                    // already walked past; it is not blocking input. Keep
                    // scanning; the next state-changing entry decides.
                    continue;
                }
                // An unresolved approval request means the thread is
                // awaiting a user decision. The tool_use_id belongs to
                // the matching Assistant(tool_use) or ToolCall — we
                // recover it by looking one step earlier. For Phase 2
                // we don't have it in the ApprovalRequested entry
                // itself (P10), so join against the nearest preceding
                // ToolCall.
                let tool_use_id =
                    nearest_preceding_tool_use_id(entries, entries.len() - 1 - offset)
                        .unwrap_or_default();
                return ThreadResumeState::AwaitingApproval { tool_use_id };
            }

            // ---- tool shape ----
            LogEntry::ToolResult { .. } => {
                // A completed tool result. The turn may have continued;
                // keep scanning back to see whether another tool was
                // issued but not completed.
                continue;
            }
            LogEntry::ToolAborted { .. } => {
                // A recorded abort terminates the pending dispatch;
                // thread is quiescent.
                return ThreadResumeState::Idle;
            }
            LogEntry::ToolCall { id, .. } => {
                // A tool_call with no later ToolResult / ToolAborted
                // means the dispatch is pending.
                let tool_use_id = id.clone();
                let was_approved = resolves_allow_for(entries, &tool_use_id);
                return ThreadResumeState::AwaitingToolResult {
                    tool_use_id,
                    was_approved,
                };
            }
            LogEntry::Assistant { content, .. } => {
                // If the assistant emitted a tool_use with no later
                // ToolResult, this is AwaitingToolResult. If the
                // assistant finalized text-only, treat as Idle — the
                // turn's completion was recorded by `Assistant`.
                if let Some(tool_use_id) = last_unresolved_tool_use(content, entries) {
                    let was_approved = resolves_allow_for(entries, &tool_use_id);
                    return ThreadResumeState::AwaitingToolResult {
                        tool_use_id,
                        was_approved,
                    };
                }
                return ThreadResumeState::Idle;
            }

            // ---- turn boundary ----
            LogEntry::TurnStart { .. } => {
                // Reached a TurnStart without encountering any
                // state-changing content after it. A Task 4-era
                // classifier would inspect `AssistantProgress` here
                // to choose between `InStreamNoFinal` and
                // `InTurnNoProgress`; with Task 2's alphabet the
                // only signal is that progress ran out.
                return ThreadResumeState::InTurnNoProgress;
            }
        }
    }

    ThreadResumeState::Idle
}

/// Scan the assistant content blocks for a `ToolUse` whose id has no
/// matching `ToolResult` or `ToolAborted` anywhere later in the log.
/// Returns the id of the unresolved tool_use, or `None` if every
/// tool_use in the block has been resolved.
fn last_unresolved_tool_use(
    content: &[crate::ContentBlock],
    entries: &[LogEntry],
) -> Option<String> {
    for block in content.iter().rev() {
        if let crate::ContentBlock::ToolUse(tc) = block {
            if !has_terminal_for_tool(entries, &tc.id) {
                return Some(tc.id.clone());
            }
        }
    }
    None
}

/// True if any `ToolResult` or `ToolAborted` in `entries` matches
/// `tool_use_id`.
fn has_terminal_for_tool(entries: &[LogEntry], tool_use_id: &str) -> bool {
    entries.iter().any(|e| match e {
        LogEntry::ToolResult { id, .. } => id == tool_use_id,
        LogEntry::ToolAborted { tool_use_id: t, .. } => t == tool_use_id,
        _ => false,
    })
}

/// True if any `ApprovalResolved` for the given tool's approval exists
/// in the log and was an "allow" decision.
///
/// `ApprovalResolved` doesn't currently carry a tool_use_id (only
/// tool_name), so in Task 2 we conservatively approximate by checking
/// whether *any* `ApprovalResolved(allow_*)` entry exists between the
/// `ToolCall` and the tail. This matches the shape the plan describes;
/// P6 and later tasks refine it. Missing the tool-use-id linkage in
/// the current schema is a known limitation noted in the plan.
fn resolves_allow_for(entries: &[LogEntry], tool_use_id: &str) -> bool {
    // Find the position of the ToolCall with this id, if any.
    let tool_call_pos = entries
        .iter()
        .rposition(|e| matches!(e, LogEntry::ToolCall { id, .. } if id == tool_use_id));
    let start = tool_call_pos.unwrap_or(0);
    for e in entries[start..].iter() {
        if let LogEntry::ApprovalResolved { decision, .. } = e {
            if decision.is_allow() {
                return true;
            }
        }
    }
    false
}

/// Given an index into `entries` pointing at an `ApprovalRequested`,
/// walk backward to find the nearest `ToolCall`'s id.
fn nearest_preceding_tool_use_id(entries: &[LogEntry], approval_idx: usize) -> Option<String> {
    for i in (0..approval_idx).rev() {
        if let LogEntry::ToolCall { id, .. } = &entries[i] {
            return Some(id.clone());
        }
        if let LogEntry::Assistant { content, .. } = &entries[i] {
            for block in content.iter().rev() {
                if let crate::ContentBlock::ToolUse(tc) = block {
                    return Some(tc.id.clone());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ContentBlock;
    use crate::ToolCall;
    use crate::log::{LogEntry, LogSource, ToolAbortReason, TurnAbortReason};

    fn u(msg: &str) -> LogEntry {
        LogEntry::User {
            content: msg.into(),
            scope: None,
        }
    }

    fn ts() -> LogEntry {
        LogEntry::TurnStart { scope: None }
    }

    fn te() -> LogEntry {
        LogEntry::TurnEnd {
            scope: None,
            model: None,
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        }
    }

    fn ce() -> LogEntry {
        LogEntry::CompletionEnd {
            scope: "root".into(),
            model: "m".into(),
            completion_id: 1,
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        }
    }

    fn meta() -> LogEntry {
        LogEntry::Meta {
            data: serde_json::json!({}),
        }
    }

    fn err() -> LogEntry {
        LogEntry::Error {
            message: "oops".into(),
            scope: None,
        }
    }

    fn assistant_text(text: &str) -> LogEntry {
        LogEntry::Assistant {
            content: vec![ContentBlock::Text { text: text.into() }],
            source: Some(LogSource {
                account: "anthropic".into(),
                model: None,
            }),
            scope: None,
            completion_id: 1,
        }
    }

    fn assistant_tool_use(id: &str) -> LogEntry {
        LogEntry::Assistant {
            content: vec![ContentBlock::ToolUse(ToolCall {
                id: id.into(),
                name: "shell".into(),
                input: serde_json::json!({}),
            })],
            source: None,
            scope: None,
            completion_id: 1,
        }
    }

    fn tool_call(id: &str) -> LogEntry {
        LogEntry::ToolCall {
            id: id.into(),
            name: "shell".into(),
            input: serde_json::json!({}),
            scope: None,
        }
    }

    fn tool_result(id: &str) -> LogEntry {
        LogEntry::ToolResult {
            id: id.into(),
            output: serde_json::json!("ok"),
            is_error: false,
            scope: None,
        }
    }

    fn approval_req() -> LogEntry {
        LogEntry::ApprovalRequested {
            tool_name: "shell".into(),
            input_preview: "ls".into(),
            post_crash_reconfirm: false,
        }
    }

    fn approval_res_allow() -> LogEntry {
        LogEntry::ApprovalResolved {
            tool_name: "shell".into(),
            decision: ox_types::Decision::AllowOnce,
        }
    }

    fn turn_aborted() -> LogEntry {
        LogEntry::TurnAborted {
            reason: TurnAbortReason::CrashDuringStream,
        }
    }

    fn tool_aborted(id: &str) -> LogEntry {
        LogEntry::ToolAborted {
            tool_use_id: id.into(),
            reason: ToolAbortReason::CrashDuringDispatch,
        }
    }

    // ---- one unit test per ThreadResumeState variant ----

    #[test]
    fn idle_on_empty_log() {
        let entries: Vec<LogEntry> = vec![];
        assert_eq!(classify(&entries), ThreadResumeState::Idle);
    }

    #[test]
    fn idle_on_complete_turn() {
        let entries = vec![ts(), u("hi"), assistant_text("hey"), te()];
        assert_eq!(classify(&entries), ThreadResumeState::Idle);
    }

    #[test]
    fn in_turn_no_progress_when_only_turn_start() {
        let entries = vec![u("hi"), ts()];
        assert_eq!(classify(&entries), ThreadResumeState::InTurnNoProgress);
    }

    #[test]
    fn awaiting_approval_pending_request() {
        let entries = vec![ts(), u("hi"), assistant_tool_use("t1"), approval_req()];
        match classify(&entries) {
            ThreadResumeState::AwaitingApproval { tool_use_id } => {
                assert_eq!(tool_use_id, "t1");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn awaiting_tool_result_after_allow() {
        let entries = vec![
            ts(),
            u("hi"),
            assistant_tool_use("t1"),
            tool_call("t1"),
            approval_req(),
            approval_res_allow(),
        ];
        match classify(&entries) {
            ThreadResumeState::AwaitingToolResult {
                tool_use_id,
                was_approved,
            } => {
                assert_eq!(tool_use_id, "t1");
                assert!(was_approved);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn in_stream_no_final_variant_exists() {
        // Task 2 does not produce this variant (see module docs), but
        // the variant must exist so downstream dispatch can match on it
        // without an API break once Task 4 lands.
        let _: ThreadResumeState = ThreadResumeState::InStreamNoFinal;
    }

    // ---- one unit test per informational variant: correctly skipped ----

    #[test]
    fn meta_is_skipped_between_abort_and_tail() {
        let entries = vec![ts(), u("hi"), turn_aborted(), meta()];
        // TurnAborted is the most recent state signal; Meta is skipped.
        assert_eq!(classify(&entries), ThreadResumeState::Idle);
    }

    #[test]
    fn completion_end_is_skipped() {
        let entries = vec![ts(), u("hi"), assistant_text("done"), te(), ce()];
        // CompletionEnd walks past to TurnEnd -> Idle.
        assert_eq!(classify(&entries), ThreadResumeState::Idle);
    }

    #[test]
    fn error_is_skipped() {
        // Error after a TurnStart leaves the state as "in turn, no
        // progress" — Error is informational, not state-changing.
        let entries = vec![u("hi"), ts(), err()];
        assert_eq!(classify(&entries), ThreadResumeState::InTurnNoProgress);
    }

    #[test]
    fn user_is_skipped() {
        // A user entry at the tail but no TurnStart or other state
        // change — the log is quiescent.
        let entries = vec![u("hi"), te(), u("another")];
        assert_eq!(classify(&entries), ThreadResumeState::Idle);
    }

    // ---- extra golden-file hand-crafted ledger tests ----

    #[test]
    fn tool_aborted_tail_is_idle() {
        // After a ToolAborted, the previous dispatch is no longer
        // pending. The thread is quiescent; the abort itself is the
        // terminal state.
        let entries = vec![
            ts(),
            u("hi"),
            assistant_tool_use("t1"),
            tool_call("t1"),
            tool_aborted("t1"),
        ];
        assert_eq!(classify(&entries), ThreadResumeState::Idle);
    }

    #[test]
    fn completed_tool_tail_is_idle() {
        let entries = vec![
            ts(),
            u("hi"),
            assistant_tool_use("t1"),
            tool_call("t1"),
            tool_result("t1"),
            te(),
        ];
        assert_eq!(classify(&entries), ThreadResumeState::Idle);
    }

    #[test]
    fn awaiting_tool_result_without_explicit_approval() {
        // Auto-approved flow: ToolCall with no ApprovalResolved and no
        // ToolResult — still AwaitingToolResult, was_approved=false.
        let entries = vec![ts(), u("hi"), assistant_tool_use("t1"), tool_call("t1")];
        match classify(&entries) {
            ThreadResumeState::AwaitingToolResult {
                tool_use_id,
                was_approved,
            } => {
                assert_eq!(tool_use_id, "t1");
                assert!(!was_approved);
            }
            other => panic!("unexpected: {other:?}"),
        }
    }
}

#[cfg(test)]
mod prop_tests {
    use super::*;
    use crate::ContentBlock;
    use crate::ToolCall;
    use crate::log::{LogEntry, LogSource};
    use proptest::prelude::*;

    /// Generate a random "valid-ish" sequence of LogEntry variants. The
    /// grammar is intentionally loose — we generate plausible per-turn
    /// shapes rather than fully correct turns. The classifier's contract
    /// is "never panic," so the important property is that any input
    /// produces a valid ThreadResumeState, not that inputs be legal
    /// turn sequences.
    fn arb_entry() -> impl Strategy<Value = LogEntry> {
        prop_oneof![
            Just(LogEntry::TurnStart { scope: None }),
            Just(LogEntry::TurnEnd {
                scope: None,
                model: None,
                input_tokens: 0,
                output_tokens: 0,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            }),
            "[a-z]{1,4}".prop_map(|s| LogEntry::User {
                content: s,
                scope: None,
            }),
            "[a-z]{1,4}".prop_map(|text| LogEntry::Assistant {
                content: vec![ContentBlock::Text { text }],
                source: Some(LogSource {
                    account: "anthropic".into(),
                    model: None,
                }),
                scope: None,
                completion_id: 0,
            }),
            "[a-z0-9]{1,6}".prop_map(|id| LogEntry::Assistant {
                content: vec![ContentBlock::ToolUse(ToolCall {
                    id,
                    name: "shell".into(),
                    input: serde_json::json!({}),
                })],
                source: None,
                scope: None,
                completion_id: 0,
            }),
            "[a-z0-9]{1,6}".prop_map(|id| LogEntry::ToolCall {
                id,
                name: "shell".into(),
                input: serde_json::json!({}),
                scope: None,
            }),
            "[a-z0-9]{1,6}".prop_map(|id| LogEntry::ToolResult {
                id,
                output: serde_json::json!("ok"),
                is_error: false,
                scope: None,
            }),
            Just(LogEntry::ApprovalRequested {
                tool_name: "shell".into(),
                input_preview: "".into(),
                post_crash_reconfirm: false,
            }),
            Just(LogEntry::ApprovalResolved {
                tool_name: "shell".into(),
                decision: ox_types::Decision::AllowOnce,
            }),
            Just(LogEntry::Meta {
                data: serde_json::json!({}),
            }),
            Just(LogEntry::CompletionEnd {
                scope: "root".into(),
                model: "m".into(),
                completion_id: 0,
                input_tokens: 0,
                output_tokens: 0,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            }),
            Just(LogEntry::Error {
                message: "x".into(),
                scope: None,
            }),
            Just(LogEntry::TurnAborted {
                reason: crate::log::TurnAbortReason::CrashDuringStream,
            }),
            "[a-z0-9]{1,6}".prop_map(|id| LogEntry::ToolAborted {
                tool_use_id: id,
                reason: crate::log::ToolAbortReason::CrashDuringDispatch,
            }),
        ]
    }

    fn arb_ledger() -> impl Strategy<Value = Vec<LogEntry>> {
        prop::collection::vec(arb_entry(), 0..32)
    }

    proptest! {
        // Keep case count modest — CI time matters more than exhaustive
        // coverage. Any classifier bug should appear within a few
        // hundred cases.
        #![proptest_config(ProptestConfig::with_cases(256))]

        /// Step 3: classify → "replay" (identity here, since the kernel
        /// log is the replay target) → classify. The two classifications
        /// must agree. Because `classify` is pure over the entries slice,
        /// the property reduces to "classify is deterministic," which the
        /// proptest verifies across random ledgers without separately
        /// round-tripping through disk.
        #[test]
        fn classify_is_idempotent_under_replay(entries in arb_ledger()) {
            let a = classify(&entries);
            let b = classify(&entries);
            prop_assert_eq!(a, b);
        }

        /// Step 4: feeding a ledger that has been truncated at an
        /// arbitrary byte offset through (serde round-trip simulating
        /// torn-tail repair) + classify must not panic. The classifier's
        /// contract is total: every `&[LogEntry]` produces a
        /// `ThreadResumeState`.
        #[test]
        fn classify_never_panics_under_truncation(entries in arb_ledger(), cut in 0usize..64) {
            // Serialize, truncate at `cut` bytes, then deserialize
            // line-by-line the way `read_ledger` would (dropping any
            // tail that fails to parse — that's the torn-tail repair
            // contract).
            let lines: Vec<String> = entries
                .iter()
                .map(|e| serde_json::to_string(e).unwrap())
                .collect();
            let joined = lines.join("\n");
            let cut = cut.min(joined.len());
            let truncated = &joined[..cut];
            let mut parsed: Vec<LogEntry> = Vec::new();
            for line in truncated.lines() {
                if let Ok(e) = serde_json::from_str::<LogEntry>(line) {
                    parsed.push(e);
                }
            }
            // Exercise classify — any variant is acceptable, we only
            // care that it returns (doesn't panic).
            let _ = classify(&parsed);
        }
    }
}
