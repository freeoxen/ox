# Unified ToolStore S-Tier Completion Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Complete the ToolStore migration to S-tier — fix the CLI crash, wire PolicyStore for permission enforcement, drive the kernel loop through TurnStore, and ensure the system works *better* than before the migration.

**Starting state:** Branch `unified-tool-store` has 27 commits. The ox-tools crate exists with ToolStore, FsModule, OsModule, CompletionModule, TurnStore, PolicyStore, NativeTool/FnTool, NameMap. Old types (Tool, FnTool, ToolRegistry, run_turn, ToolsProvider, completion_tool) are deleted. All consumers migrated to ToolStore. But: CLI is broken (CompletionTransport not injected into the right ToolStore), PolicyStore is unwired (tools execute without permission checks), TurnStore is unused, and synthesize_prompt reads stale gate paths.

**Branch:** `unified-tool-store` (continue from current HEAD)

---

## Bug 1: CLI Crash — "no CompletionTransport injected"

### Root Cause

In `crates/ox-cli/src/agents.rs`, agent_worker injects the `CliCompletionTransport` into `tool_store.completions_mut().set_transport(...)`. This tool_store lives on `CliEffects`. But `HostStore` has its own separate `tool_store: Option<ToolStore>` field that's `None` — it's never set via `with_tool_store()`.

When the Wasm agent writes to `tools/completions/complete/{account}`, HostStore routes to `self.tool_store` (None → falls through to backend, which doesn't handle it) instead of `self.effects.tool_store` (the one with the transport).

### Fix

There should be ONE ToolStore, not two. The ToolStore should move from CliEffects to HostStore, or HostStore should use the CliEffects' ToolStore.

**Option A (cleanest):** Remove `tool_store: Option<ToolStore>` from HostStore. Instead, HostStore routes `tools/*` to `self.effects` via a new trait method. The HostEffects trait gains access to the ToolStore:

```rust
pub trait HostEffects: Send {
    fn emit_event(&mut self, event: AgentEvent);
    fn tool_store(&mut self) -> &mut ToolStore;
}
```

HostStore's `handle_read` and `handle_write` call `self.effects.tool_store()` for tools/* routing.

**Option B (simpler):** In agent_worker, after creating CliEffects and before creating HostStore, move the ToolStore from CliEffects into the HostStore via `with_tool_store()`. But this breaks the ownership ping-pong since CliEffects needs to own the ToolStore between turns.

**Option C (simplest):** In agent_worker, create the HostStore with `with_tool_store()` using the CliEffects' ToolStore. But ToolStore can't be in two places.

**Recommended: Option A.** HostEffects provides the ToolStore. HostStore delegates to it. One ToolStore, one owner (CliEffects), one path.

### Task 1: Fix CLI crash

**Files:**
- Modify: `crates/ox-runtime/src/host_store.rs`
- Modify: `crates/ox-cli/src/agents.rs`

Steps:
- [ ] Add `fn tool_store(&mut self) -> &mut ox_tools::ToolStore;` to `HostEffects` trait
- [ ] Remove `tool_store: Option<ToolStore>` field from `HostStore`
- [ ] Remove `with_tool_store()` method from `HostStore`
- [ ] Update `handle_read` and `handle_write` to call `self.effects.tool_store()` for `tools/*` routing
- [ ] Implement `tool_store()` on `CliEffects` (returns `&mut self.tool_store`)
- [ ] Update MockEffects in tests to provide a ToolStore (use `ToolStore::empty()`)
- [ ] Verify: `cargo test --workspace` passes
- [ ] Verify: manually test the CLI with a real prompt (if API key available), or verify the Wasm agent path compiles correctly
- [ ] Commit

---

## Bug 2: PolicyStore Not Wired — Tools Execute Without Permission Checks

### Root Cause

The old `CliEffects::execute_tool()` called `self.policy.check(&call.name, &call.input)` before executing. That method was deleted in Cut 3. Now tool writes go straight through the ToolStore with no policy check. PolicyGuard exists but is dead code (`#![allow(dead_code)]` on policy.rs).

### Fix

Wrap the ToolStore in PolicyStore before the Wasm agent can write to it. The PolicyStore intercepts writes and checks PolicyGuard before forwarding.

But the approval flow is the hard part. When PolicyGuard returns `Ask`, the old code wrote an approval request to the broker, blocked until the TUI responded, then continued. This was synchronous blocking inside `HostEffects::execute_tool`. Now tool execution happens inside `ToolStore::write()` which is a synchronous `Writer` impl.

**Approach:** The PolicyStore's write method needs to:
1. Check PolicyGuard
2. If Allow → forward to inner store
3. If Deny → return StoreError
4. If Ask → write approval request to broker, block for response, then forward or deny

For the Ask case, PolicyStore needs a handle to the broker. This means PolicyStore's policy function isn't just `FnMut(&Path, &Record) -> PolicyDecision` — it needs to be a richer callback that can block.

**Revised PolicyStore design:**

```rust
pub trait PolicyCheck: Send + Sync {
    /// Check whether a write should be allowed.
    /// May block for user approval (Ask flow).
    fn check(&mut self, path: &Path, data: &Record) -> PolicyDecision;
}
```

PolicyDecision gains Ask handling internally — the `PolicyCheck` impl for the CLI does the broker write/read/block inside `check()`.

```rust
struct CliPolicyCheck {
    guard: PolicyGuard,
    scoped_client: ClientHandle,
    rt_handle: tokio::runtime::Handle,
}

impl PolicyCheck for CliPolicyCheck {
    fn check(&mut self, path: &Path, data: &Record) -> PolicyDecision {
        // Convert StructFS path to tool name for PolicyGuard
        let tool_name = path_to_tool_name(path);
        let input = record_to_json(data);
        match self.guard.check(&tool_name, &input) {
            CheckResult::Allow => PolicyDecision::Allow,
            CheckResult::Deny(reason) => PolicyDecision::Deny(reason),
            CheckResult::Ask { tool, input_preview, .. } => {
                // Broker-based approval flow (same as old execute_tool)
                let approval_client = self.scoped_client.with_timeout(Duration::MAX);
                // ... write request, read response, handle allow_once/session/always/deny ...
            }
        }
    }
}
```

Then in agent_worker:
```rust
let policy_check = CliPolicyCheck { guard: policy, scoped_client, rt_handle };
let gated_store = PolicyStore::new(tool_store, policy_check);
// CliEffects holds gated_store instead of raw tool_store
```

**Important:** This also fixes the approval timeout bug from the original conversation. The approval request write uses `with_timeout(Duration::MAX)`, and the response read should too (using the same client). The old code had mismatched timeouts — the fix is structural in the new PolicyCheck impl.

### Task 2: Wire PolicyStore with approval flow

**Files:**
- Modify: `crates/ox-tools/src/policy_store.rs` — change from `FnMut` closure to `PolicyCheck` trait
- Create: `crates/ox-cli/src/policy_check.rs` — CliPolicyCheck implementing PolicyCheck with broker approval flow
- Modify: `crates/ox-cli/src/agents.rs` — wrap ToolStore in PolicyStore before putting it in CliEffects
- Modify: `crates/ox-cli/src/policy.rs` — remove `#![allow(dead_code)]`

Steps:
- [ ] Replace `PolicyStore<S, F>` with `PolicyStore<S, P: PolicyCheck>`
- [ ] Add `PolicyCheck` trait to policy_store.rs
- [ ] Create CliPolicyCheck in ox-cli that wraps PolicyGuard + broker approval flow
- [ ] **Fix the approval timeout bug:** response read uses `approval_client` (Duration::MAX), not `self.scoped_client` (30s default)
- [ ] Wrap ToolStore in PolicyStore in agent_worker
- [ ] Update CliEffects to hold `PolicyStore<ToolStore, CliPolicyCheck>` instead of bare `ToolStore`
- [ ] Update HostEffects::tool_store() return type to accommodate PolicyStore (may need `&mut dyn Store` instead of `&mut ToolStore`)
- [ ] Remove `#![allow(dead_code)]` from policy.rs
- [ ] Remove `policy: PolicyGuard` field from CliEffects (it's inside CliPolicyCheck now)
- [ ] Test: verify policy check runs on tool writes
- [ ] Test: verify denied tools return error to Wasm agent
- [ ] Commit

---

## Bug 3: synthesize_prompt reads stale paths

### Root Cause

`synthesize_prompt` in ox-context reads `gate/defaults/model` and `gate/defaults/max_tokens`. In the CLI, the gate config lives inside the ToolStore's CompletionModule (at `tools/completions/defaults/model`). The `gate/` prefix is no longer mounted in the per-thread namespace — it's inside the ToolStore.

### Fix

`synthesize_prompt` should read model/max_tokens from `tools/completions/defaults/model` and `tools/completions/defaults/max_tokens` instead of `gate/defaults/model`.

Or: the ToolStore should expose `gate/defaults/model` as a read path that delegates to CompletionModule. This way synthesize_prompt doesn't change — the ToolStore just makes the paths available.

**Recommended:** Update synthesize_prompt to read from the ToolStore paths. It already reads `tools/schemas` for tool definitions. It should read `tools/completions/defaults/model` for the model.

### Task 3: Fix synthesize_prompt paths

**Files:**
- Modify: `crates/ox-context/src/lib.rs` — update synthesize_prompt to read from `tools/completions/defaults/*`

Steps:
- [ ] Change `path!("gate/defaults/model")` to `path!("tools/completions/defaults/model")` in synthesize_prompt
- [ ] Change `path!("gate/defaults/max_tokens")` to `path!("tools/completions/defaults/max_tokens")`
- [ ] Update the Wasm agent (ox-wasm) which reads `gate/defaults/model` and `gate/defaults/account` at startup — change to `tools/completions/defaults/model` and `tools/completions/defaults/account`
- [ ] Update ox-cli agent_worker which reads `gate/defaults/account` via adapter — change path
- [ ] Update ox-cli agent_worker which writes tool schemas via adapter — verify `tools/schemas` write path still works through ToolStore
- [ ] Verify: all tests pass, CLI starts without error
- [ ] Commit

---

## Feature 1: TurnStore Drives the Kernel Loop

### Current State

The Wasm agent (ox-wasm/src/lib.rs) has a manual loop: initiate_completion → write to completions → read response → consume_events → complete_turn → for each tool call: write → read result → write results to history → loop.

TurnStore exists with PendingEffect/EffectOutcome types but nothing uses it.

### Design

The kernel loop should be:
```rust
loop {
    // What needs doing?
    let pending = bridge.read(&path!("tools/turn/pending"))?;
    if pending.is_none() { break; }

    // Do it
    bridge.write(&path!("tools/turn/execute"), pending)?;

    // What came back?
    let results = bridge.read(&path!("tools/turn/results"))?;

    // Record it
    bridge.write(&path!("history/append"), results)?;
}
```

The TurnStore reads history to determine what's needed (completion or tool execution), dispatches through the ToolStore, collects results.

**However:** This is a significant redesign of the kernel loop. The current three-phase kernel methods (initiate_completion, consume_events, complete_turn) would need to be integrated into TurnStore's logic. This is the "kernel becomes a pure state machine" vision from the architecture discussion.

### Scoping Decision

For S-tier completion, TurnStore integration means:
1. TurnStore reads from history to detect pending tool calls
2. TurnStore dispatches tool executions through the ToolStore
3. TurnStore handles completion requests through the ToolStore
4. The Wasm agent loop simplifies to the four-operation pattern above

This is a large change. It should be a separate plan after the bugs are fixed and PolicyStore is wired.

### Task 4: Wire TurnStore into the kernel loop

**Files:**
- Modify: `crates/ox-tools/src/turn.rs` — add Reader/Writer impl that integrates with ToolStore
- Modify: `crates/ox-tools/src/lib.rs` — ToolStore routes `turn/*` to TurnStore
- Modify: `crates/ox-wasm/src/lib.rs` — simplify agent loop to use turn/pending, turn/execute, turn/results
- Modify: `crates/ox-kernel/src/lib.rs` — the three-phase methods may move into TurnStore or be called by it

Steps:
- [ ] TurnStore implements Reader + Writer
- [ ] Write to `turn/execute` triggers: read pending effects, dispatch each through ToolStore (completions and tools), collect results
- [ ] Read from `turn/pending` extracts unresolved tool calls from latest assistant message in history
- [ ] Read from `turn/results` returns collected execution outcomes
- [ ] TurnStore needs access to the ToolStore (either by reference or by being part of ToolStore)
- [ ] Update Wasm agent loop to the simplified four-operation pattern
- [ ] Kernel three-phase methods (initiate_completion, consume_events, complete_turn) integrate into TurnStore's execute logic
- [ ] Test: verify sequential tool execution works through TurnStore
- [ ] Test: verify completion + tool calls in same turn work
- [ ] Commit

---

## Feature 2: Kernel step() Method

### Task 5: Add Kernel::step() returning effects

The plan originally called for a `step()` method that makes the kernel a pure state machine. With TurnStore handling execution, `step()` becomes thin:

```rust
impl Kernel {
    pub fn step(
        &mut self,
        context: &mut dyn Store,
        outcomes: &[ToolOutcome],
    ) -> Result<Vec<ToolEffect>, KernelError> { ... }
}
```

This may or may not be needed depending on how TurnStore integration goes. If TurnStore drives everything, step() might be redundant. Evaluate after Task 4.

---

## Task Order and Dependencies

```
Task 1 (fix CLI crash)         — CRITICAL, do first
Task 3 (fix synthesize paths)  — CRITICAL, do second (CLI still broken without this)
Task 2 (wire PolicyStore)      — HIGH, do third (security regression)
Task 4 (TurnStore integration) — MEDIUM, do fourth (architectural completion)
Task 5 (Kernel step)           — LOW, evaluate after Task 4
```

Tasks 1 and 3 are bugs that make the CLI non-functional. Fix those first.
Task 2 is a security regression — tools execute without permission checks.
Task 4 is the architectural vision — the kernel loop driven by TurnStore.

## Verification Criteria for S-Tier

- [ ] CLI works: user can type a prompt, get a response, tool calls execute with permission checks
- [ ] 15/15 quality gates pass
- [ ] No `#![allow(dead_code)]` on policy.rs — PolicyGuard is actively used
- [ ] No `tool_store: Option<ToolStore>` on HostStore — single ToolStore, single owner
- [ ] TurnStore drives the kernel loop (or conscious decision to defer with documented rationale)
- [ ] synthesize_prompt reads from ToolStore paths, not stale gate/ paths
- [ ] Approval flow works: Ask decisions block for TUI response with correct timeout
- [ ] Zero references to: ToolRegistry, gate/complete, tools/execute, ToolsProvider, SimpleStore, pending_events
