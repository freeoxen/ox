# Audit Log Design

**Date:** 2026-04-13
**Status:** Draft
**Goal:** Extend the structured log to capture every meaningful event in a thread's lifecycle, and update the history explorer to display the full audit trail.

## Motivation

The structured log (`SharedLog` / `LogEntry`) already records LLM messages, tool calls, and tool results. But approval requests/decisions, turn boundaries, and errors are not logged — they happen ephemerally and are lost. For debugging (especially the looping output bug) and for audit compliance, the log should be the complete record of everything that happened.

## Approach

Small, surgical changes. The log is already the right primitive. Five new `LogEntry` variants, five write sites, one read path change in the history explorer. No new abstractions.

## LogEntry Extensions

Add five new variants to `LogEntry` in `crates/ox-kernel/src/log.rs`:

```rust
#[serde(rename = "turn_start")]
TurnStart {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
},

#[serde(rename = "turn_end")]
TurnEnd {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
},

#[serde(rename = "approval_requested")]
ApprovalRequested {
    tool_name: String,
    input_preview: String,
},

#[serde(rename = "approval_resolved")]
ApprovalResolved {
    tool_name: String,
    decision: String,
},

#[serde(rename = "error")]
Error {
    message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
},
```

Backward compatible: old `ledger.jsonl` files only have the original 5 `type` values. Serde deserializes them fine. New entries get new `type` tags.

## Write Sites

### TurnStart / TurnEnd — `ox-kernel/src/run.rs`

`run_turn()` already calls `emit(AgentEvent::TurnStart)` and `emit(AgentEvent::TurnEnd)`. Add log appends at the same points.

**TurnStart** — right after `emit(AgentEvent::TurnStart)` (~line 624):
```rust
emit(AgentEvent::TurnStart);
// Append to log
let entry = serde_json::json!({
    "type": "turn_start",
    "scope": scope,
});
let val = structfs_serde_store::json_to_value(entry);
let _ = context.write(&path!("log/append"), Record::parsed(val));
```

**TurnEnd** — right before `emit(AgentEvent::TurnEnd)` (~line 675). Read token counts from `history/turn/tokens` first:
```rust
// Read token usage before emitting turn end
let (input_tokens, output_tokens) = read_token_usage(context);
let entry = serde_json::json!({
    "type": "turn_end",
    "scope": scope,
    "input_tokens": input_tokens,
    "output_tokens": output_tokens,
});
let val = structfs_serde_store::json_to_value(entry);
let _ = context.write(&path!("log/append"), Record::parsed(val));
emit(AgentEvent::TurnEnd);
```

A helper `read_token_usage(context) -> (u64, u64)` reads from `history/turn/tokens`.

**Error** — at the `AgentEvent::Error` emit site (~line 344):
```rust
emit(AgentEvent::Error(e.clone()));
let entry = serde_json::json!({
    "type": "error",
    "message": e,
});
let val = structfs_serde_store::json_to_value(entry);
let _ = context.write(&path!("log/append"), Record::parsed(val));
```

### ApprovalRequested / ApprovalResolved — `ox-ui/src/approval_store.rs`

ApprovalStore needs access to `SharedLog` to append entries directly.

**Constructor change:**
```rust
pub struct ApprovalStore {
    pending: Option<ApprovalRequest>,
    deferred_tx: Option<tokio::sync::oneshot::Sender<String>>,
    log: SharedLog,
}

impl ApprovalStore {
    pub fn new(log: SharedLog) -> Self {
        ApprovalStore {
            pending: None,
            deferred_tx: None,
            log,
        }
    }
}
```

**In `write("request")`** — after setting `self.pending`:
```rust
self.pending = Some(req.clone());
self.log.append(LogEntry::ApprovalRequested {
    tool_name: req.tool_name.clone(),
    input_preview: req.input_preview.clone(),
});
```

Note: `ApprovalStore` is in `ox-ui` which depends on `ox-kernel` (for `SharedLog` and `LogEntry`). Check the dependency graph — if `ox-ui` doesn't depend on `ox-kernel`, the `SharedLog` type needs to be available. Looking at the workspace: ox-ui depends on ox-types but not ox-kernel directly. Two options:
- (a) Add ox-kernel as a dependency of ox-ui
- (b) Have ThreadNamespace (in ox-cli) write the log entries when it routes approval writes, keeping ApprovalStore unchanged

Option (b) is cleaner — it avoids adding a cross-crate dependency. ThreadNamespace already routes writes. In `thread_registry.rs`, when a write to `approval/request` or `approval/response` succeeds, also append to the log:

```rust
// In ThreadNamespace's async write handler for approval paths:
"approval" => {
    let result = self.approval.write(&sub_path, data.clone()).await;
    if result.is_ok() {
        match sub_path.components[0].as_str() {
            "request" => {
                if let Some(val) = data.as_value() {
                    let req: ApprovalRequest = structfs_serde_store::from_value(val.clone()).ok()?;
                    self.log.shared().append(LogEntry::ApprovalRequested {
                        tool_name: req.tool_name,
                        input_preview: req.input_preview,
                    });
                }
            }
            "response" => {
                if let Some(val) = data.as_value() {
                    let resp: ApprovalResponse = structfs_serde_store::from_value(val.clone()).ok()?;
                    // Read the tool_name from the pending request before it's cleared
                    let tool_name = self.approval.pending_tool_name().unwrap_or_default();
                    self.log.shared().append(LogEntry::ApprovalResolved {
                        tool_name,
                        decision: resp.decision,
                    });
                }
            }
            _ => {}
        }
    }
    result
}
```

Actually, there's a timing issue: the response write clears `pending` before we can read `tool_name`. Better approach: log the approval request in ThreadNamespace BEFORE routing to ApprovalStore, and log the resolution BEFORE routing the response (so pending is still set). Or, simpler: just capture tool_name from the pending state before routing.

Revisiting: the cleanest approach is to have ThreadNamespace log both events. It already has access to both `self.log` (LogStore with SharedLog) and `self.approval`. For the response, read the tool_name from `self.approval` before routing the write (which clears it).

## History Explorer Changes

### ViewState — read from log instead of messages

In `view_state.rs`, the `ScreenSnapshot::History` arm currently reads from `threads/{tid}/history/messages`. Change to read from `threads/{tid}/log/entries`:

```rust
ScreenSnapshot::History(snap) => {
    let tid = &snap.thread_id;
    let log_path = ox_path::oxpath!("threads", tid, "log", "entries");
    if let Ok(Some(record)) = client.read(&log_path).await {
        if let Some(val) = record.as_value() {
            raw_messages = vec![val.clone()]; // single Value::Array
        }
    }
    // turn state for streaming indicator
    let turn_path = ox_path::oxpath!("threads", tid, "history", "turn");
    if let Ok(Some(t)) = client.read_typed::<ox_history::TurnState>(&turn_path).await {
        turn = t;
    }
}
```

Actually, `raw_messages` is `Vec<Value>` where each element is one message. The log returns a JSON array of LogEntry objects. So we should unpack it:

```rust
if let Some(Value::Array(arr)) = record.as_value() {
    raw_messages = arr.clone();
}
```

Same pattern as before, just different source path.

### Parse layer — `parse.rs`

Replace or extend `parse_history_entries` to handle all LogEntry types. The function already receives `&[Value]` — each Value is now a LogEntry (tagged with `"type"`).

New parsing logic dispatches on the `type` field:

| type | Display |
|------|---------|
| `user` | Role badge: user, summary of content |
| `assistant` | Role badge: assistant, summary of content blocks |
| `tool_call` | Role badge: tool, name + input preview |
| `tool_result` | Role badge: result, output preview (expandable) |
| `turn_start` | Dim separator: `── turn start ──` |
| `turn_end` | Dim separator: `── turn end (Xin / Yout) ──` |
| `approval_requested` | Yellow: `[approval?] tool_name — "input_preview"` |
| `approval_resolved` | Green/Red: `[allow_once] tool_name` or `[denied] tool_name` |
| `error` | Red: `[error] message` |
| `meta` | Dim: `[meta] data preview` |

The `HistoryEntry` struct gains a `kind` field (or the role field becomes more general) to distinguish rendering. The duplicate detection still applies to User/Assistant entries.

### Theme additions

Add styles for the new entry types:

```rust
pub history_turn_boundary: Style,    // dim separator lines
pub history_approval_ask: Style,     // yellow for approval requested
pub history_approval_allow: Style,   // green for allowed
pub history_approval_deny: Style,    // red for denied
```

## Ledger Persistence

No changes needed. `ledger.jsonl` serializes LogEntry via serde. New variants get new `"type"` tags. Old ledger files with only the original 5 types deserialize correctly — serde just won't produce the new variants from old data.

## Testing

- **ox-kernel/log.rs:** Add tests for serialization/deserialization of new LogEntry variants (round-trip through JSON).
- **ox-kernel/run.rs:** Verify TurnStart/TurnEnd/Error entries appear in the log after run_turn.
- **ox-cli/parse.rs:** Update parse tests to handle all LogEntry types, including the new ones.
- **Thread registry:** Test that approval request/response writes produce log entries.

## Implementation Order

1. **ox-kernel/log.rs:** Add 5 new LogEntry variants with serde attributes. Add round-trip tests.
2. **ox-kernel/run.rs:** Add TurnStart/TurnEnd/Error log writes at existing emit sites.
3. **ox-cli/thread_registry.rs:** Add approval log writes when routing approval request/response.
4. **ox-cli/parse.rs:** Update `parse_history_entries` (or new `parse_log_entries`) to handle all LogEntry types.
5. **ox-cli/view_state.rs:** Change history screen to read from `log/entries` instead of `history/messages`.
6. **ox-cli/history_view.rs:** Update rendering for new entry types.
7. **ox-cli/theme.rs:** Add new styles for turn boundaries, approval badges.
8. **Compile, test, fmt, clippy.**
