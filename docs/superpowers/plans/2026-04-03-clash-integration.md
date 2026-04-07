# Clash Integration: Policy Enforcement with TUI Approval Flow

## Context

ox-cli's agentic loop executes tool calls (read_file, write_file, edit_file, shell, complete_*) without any permission checks. The model can `shell("rm -rf /")` and it runs. We need a policy layer.

Clash (https://clash.rs, `clash` crate on crates.io) is a policy enforcement engine for coding agents. It evaluates tool calls against rules (Starlark-compiled match trees) and returns Allow/Deny/Ask. Currently it integrates with Claude Code via hooks — but hooks are fire-and-forget, limited to y/n prompts.

ox-cli's Ratatui TUI can do what Claude Code can't: a rich approval dialog where "Ask" becomes a four-way choice (allow once, allow always, deny once, deny always) with rule persistence. This is the deep integration.

## Changes

### 1. Add `clash` dependency

**Modify:** `crates/ox-cli/Cargo.toml`

Add `clash` from crates.io. We use:
- `clash::policy::{Effect, CompiledPolicy, compile_multi_level_to_tree}`
- `clash::policy_loader::{try_load_policy, compile_policies}`
- `clash::settings::ClashSettings`
- `clash::permissions::check_permission`
- `clash::hooks::ToolUseHookInput`
- `clash::policy::manifest_edit` — for persisting "allow always" rules

### 2. New `policy.rs` module

**Create:** `crates/ox-cli/src/policy.rs`

Wraps clash's API into ox-cli's needs:

```rust
pub struct PolicyGuard {
    settings: ClashSettings,
}

impl PolicyGuard {
    pub fn load(workspace: &Path) -> Result<Self, String>;
    pub fn check(&self, tool_name: &str, input: &serde_json::Value) -> PolicyDecision;
    pub fn persist_allow(&mut self, tool_name: &str, input: &serde_json::Value) -> Result<(), String>;
    pub fn persist_deny(&mut self, tool_name: &str, input: &serde_json::Value) -> Result<(), String>;
}

pub enum PolicyDecision {
    Allow,
    Deny(String),
    Ask { tool: String, input_preview: String },
}
```

`check()` constructs a `ToolUseHookInput` from ox's tool name + input JSON, calls `check_permission`, and maps the `HookOutput` to our `PolicyDecision`.

`persist_allow/deny` use clash's `manifest_edit` to write a new rule to the project policy file and reload.

### 3. Bidirectional approval channel

**Modify:** `crates/ox-cli/src/app.rs`

Add a new `AppEvent` variant for permission requests:

```rust
AppEvent::PermissionRequest {
    tool: String,
    input: serde_json::Value,
    input_preview: String,
    respond: mpsc::Sender<ApprovalResponse>,
}
```

Where `ApprovalResponse` is:
```rust
pub enum ApprovalResponse {
    AllowOnce,
    AllowAlways,
    DenyOnce,
    DenyAlways,
}
```

The agent thread sends a `PermissionRequest` and blocks on `respond.recv()`. The TUI thread receives it, shows a dialog, sends the response back.

Add `PolicyGuard` to the agent thread. Pass it alongside tools.

### 4. Policy check in the tool loop

**Modify:** `crates/ox-cli/src/app.rs` — `run_streaming_loop`

Before `tool.execute(tc.input.clone())` at line 474, insert:

```rust
match policy.check(&tc.name, &tc.input) {
    PolicyDecision::Allow => { /* proceed */ }
    PolicyDecision::Deny(reason) => {
        results.push(ToolResult { tool_use_id: tc.id.clone(), content: format!("denied: {reason}") });
        continue;
    }
    PolicyDecision::Ask { tool, input_preview } => {
        let (resp_tx, resp_rx) = mpsc::channel();
        event_tx.send(AppEvent::PermissionRequest {
            tool, input: tc.input.clone(), input_preview, respond: resp_tx,
        }).ok();
        match resp_rx.recv() {
            Ok(ApprovalResponse::AllowOnce) => { /* proceed */ }
            Ok(ApprovalResponse::AllowAlways) => {
                policy.persist_allow(&tc.name, &tc.input).ok();
                /* proceed */
            }
            Ok(ApprovalResponse::DenyOnce) => {
                results.push(ToolResult { tool_use_id: tc.id.clone(), content: "denied by user".into() });
                continue;
            }
            Ok(ApprovalResponse::DenyAlways) => {
                policy.persist_deny(&tc.name, &tc.input).ok();
                results.push(ToolResult { tool_use_id: tc.id.clone(), content: "denied by user".into() });
                continue;
            }
            Err(_) => {
                results.push(ToolResult { tool_use_id: tc.id.clone(), content: "denied: TUI disconnected".into() });
                continue;
            }
        }
    }
}
```

### 5. TUI permission dialog

**Modify:** `crates/ox-cli/src/tui.rs`

Add an approval mode to the TUI. When a `PermissionRequest` arrives:

- Set `app.pending_approval = Some(ApprovalState { tool, input_preview, respond, selected: 0 })`
- The draw function renders a modal overlay:
  ```
  ┌─ Permission Required ────────────────────────┐
  │                                               │
  │  [shell] sh -c "rm -rf target/"               │
  │                                               │
  │  > Allow once                                 │
  │    Allow always (add rule)                    │
  │    Deny once                                  │
  │    Deny always (add rule)                     │
  │                                               │
  └───────────────────────────────────────────────┘
  ```
- Up/Down changes selection, Enter confirms
- The response is sent back over the channel

Key bindings change when in approval mode — normal input is disabled, only Up/Down/Enter work.

### 6. Theme additions

**Modify:** `crates/ox-cli/src/theme.rs`

Add slots:
```rust
pub approval_border: Style,      // dialog border
pub approval_title: Style,       // "Permission Required"
pub approval_tool: Style,        // tool name in dialog
pub approval_preview: Style,     // input preview
pub approval_selected: Style,    // highlighted option
pub approval_option: Style,      // unselected option
```

### 7. App state additions

**Modify:** `crates/ox-cli/src/app.rs`

Add to `App`:
```rust
pub pending_approval: Option<ApprovalState>,
pub policy_stats: PolicyStats,  // allowed/denied/asked counts
```

`PolicyStats` counters show in the status bar: `✓12 ✗2 ?1`.

### 8. CLI flag

**Modify:** `crates/ox-cli/src/main.rs`

Add `--no-policy` flag to disable policy enforcement (runs all tools without checks, like current behavior).

## File summary

| File | Change |
|------|--------|
| `Cargo.toml` | Add `clash` dependency |
| `src/policy.rs` | New — PolicyGuard wrapping clash API |
| `src/app.rs` | ApprovalResponse enum, PermissionRequest event, policy check in tool loop, PolicyGuard in agent thread |
| `src/tui.rs` | Approval dialog overlay, modal key handling |
| `src/theme.rs` | 6 new approval-related style slots |
| `src/main.rs` | `--no-policy` flag, PolicyGuard initialization |

## Verification

1. `cargo check -p ox-cli` — compiles with clash dependency
2. `cargo test -p ox-cli` — existing 29 tests still pass
3. Manual test: create `.clash/policy.star` with `default = ask()`, run ox, verify dialog appears on tool calls
4. Manual test: "Allow always" → verify rule persisted to policy file
5. Manual test: `--no-policy` → verify tools run without checks
