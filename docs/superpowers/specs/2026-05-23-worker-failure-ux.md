# Worker-failure notification UX

> **Status: spec, not plan.** This is a product/UX question, not a code
> refactor. Implementation belongs in a separate plan once the surface
> is decided.

## Problem

Agent-worker startup failures currently surface via two channels:

1. **Operator** — `tracing::error!` with full context (workspace, error
   detail). Visible to anyone running with `RUST_LOG=error`; invisible
   in normal use.
2. **In-thread history** — a synthesized assistant turn ("⚠ Agent
   failed to start: ..."). Visible if and only if the user is viewing
   that thread when the worker dies.

The second channel is the user-facing surface today. It has a hole: if
the user clicked "start agent" from the inbox and then switched away
(or never selected the new thread), they see *nothing* — the failure
lands in a thread they aren't looking at.

Currently this affects:

- Policy-load refusals (`.clash/policy.json` malformed) — pinned by
  `crates/ox-cli/tests/policy_refusal.rs`.
- Future worker-startup failure modes (transport setup errors, etc.)
  will inherit this same gap.

## Why it matters

The audit's "audience-channel" discipline says every error has to land
*where the user is looking*. For mid-conversation errors, that's the
thread's history. For startup failures, the user might not have a
thread to look at yet — or they might have multiple threads and be
viewing the wrong one. The history-only surface is correct for
"this turn failed"; it's incomplete for "this agent never started."

A user who configured a restrictive policy that contains a typo will
click "start" and stare at silence. They'll think the agent is slow.
They'll click again. Nothing changes. Eventually they'll check the
thread list and find a thread with a single ⚠ message. The delay
between action and feedback is the failure.

## Surfaces to consider

### Option A — Inbox-level status badge

Each thread in the inbox carries a `status` field. Set
`ThreadState::Failed(reason)` when the worker refuses to start, and
render it in the inbox list:

```
▸ my-thread        Failed: policy.json couldn't be parsed
▸ other-thread     Idle
```

**Pros:** No new mechanism. Uses existing thread state. Visible from
the inbox without entering a specific thread.

**Cons:** User has to be looking at the inbox. Doesn't help when they
just hit "start" and are watching their previous thread.

### Option B — Status-bar toast

A transient notification at the bottom of the TUI: "⚠ Agent failed to
start in thread 'X' — open thread for details." Auto-dismisses after
~5 seconds or on any keypress.

**Pros:** Visible regardless of which thread the user is viewing.
Directs them to the failed thread.

**Cons:** New mechanism — needs a toast/notification subscription on
the broker side, render slot in the TUI, and dismiss logic. Probably
~2 days of UI work.

### Option C — Modal block on first failure

A modal dialog ("Agent failed to start: ..."). User must dismiss before
continuing. The modal links to the relevant thread for more detail.

**Pros:** Impossible to miss.

**Cons:** Interruptive. Wrong for the case where multiple agents are
in flight and one fails — you don't want to lose your current thread's
context for an unrelated worker death.

### Option D — Bell + inbox highlight

Subtle: ring the terminal bell, highlight the failed thread in the
inbox list with a color or symbol until the user opens it. No
intrusive popup; user notices the highlight when they next look at
the inbox.

**Pros:** Low-interruption. Persistent until acknowledged. Reuses the
inbox view.

**Cons:** Some terminals suppress the bell. Highlight alone is easy to
miss if the user isn't scanning the inbox.

## Recommendation

**Option A + Option D** combined:

- Set `ThreadState::Failed(reason)` on the inbox-level thread state.
  The reason is the same text we already write to the synthesized
  assistant turn. The inbox renderer displays the failure status next
  to the thread title.
- Use a distinctive color/symbol (⚠ red) and persistent — the
  highlight stays until the user opens the thread.
- Optional: ring the bell (configurable via `~/.ox/config.toml` so
  users on noise-sensitive terminals can disable).

**Why this combo:** the inbox status badge gives a persistent,
unambiguous signal in a place the user routinely looks. The bell is
the "right now" attention-grabber. Together they cover the
"user-switched-away" gap without introducing a new toast/modal
mechanism.

**What this needs:**

1. `ThreadState::Failed { reason: String }` variant on the inbox
   thread-state enum.
2. Worker writes this state before exiting on startup failure (in
   addition to the synthesized assistant turn).
3. Inbox renderer surfaces the variant with a distinct visual.
4. Optional bell on transition into `Failed` state, gated by a
   config flag.

Estimated implementation: ~half-day for (1)-(3), another quarter-day
for (4) including the config wiring.

## Out of scope

- Toast/modal mechanism. Worth considering separately if there's a
  *second* class of "user must know now" event that doesn't fit the
  inbox-status shape. For policy-load failures, the inbox status is
  sufficient.
- Cross-process notifications (OS-level notification when running
  agents in the background). Different scope entirely; not blocked by
  this spec.

## Acceptance criteria for the implementing plan

- A user with a malformed `.clash/policy.json` who clicks "start
  agent" sees the failure surfaced in *both* the thread's history
  (existing behavior) *and* the inbox-level thread status, regardless
  of which thread they're viewing.
- The integration test `crates/ox-cli/tests/policy_refusal.rs` is
  extended to also assert on the inbox-level state, not just the
  thread history.
- The bell (when enabled) fires exactly once per state transition,
  not on every render frame.

## When to do this

When user feedback says "I didn't know my agent failed" — the
explicit triggering signal. Until then, the in-thread history surface
is the minimum-viable shape and the test pins it.
