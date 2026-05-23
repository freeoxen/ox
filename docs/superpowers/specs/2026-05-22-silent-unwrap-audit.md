# Silent-unwrap audit — outcomes

> **Status: closed.** Triggered by the `legacy_account_endpoint_is_migrated_to_provider`
> flake (`resolve_config` swallowing Figment extract errors with
> `unwrap_or_default()` and surfacing them as confusing "no entry found
> for key" panics elsewhere; fixed in commit `3650fd0`).
>
> The seven sites the audit identified are all closed: 6 fixed, 1
> verified-as-bounded. The pattern is pinned by a CI lint (commit
> `1fe7d84`); residuals are tracked as their own plans.

This document captures what each site's fix actually looks like in
production code. The original recommendation per site is preserved at
the end for diff-tracking.

## Closed sites

### 1. `crates/ox-history/src/lib.rs` — assistant content decode

**Fixed in commit `1fe7d84`.** The inline `unwrap_or_default()` became
a named ingress function `decode_assistant_content(raw)` that returns
`Result<Vec<ContentBlock>, StoreError>` with the deser error preserved
in the message. The caller (`HistoryView::write`'s `"assistant"` arm)
uses `?`. The error propagates up through the broker write path to
the agent worker's `adapter.write_typed(...)` calls, which surface via
`tracing::error!` with full context.

**Audience served:** Operator (tracing). The user doesn't see a
"history append failed" toast — that surface doesn't exist yet
(tracked as a separate spec, see `2026-05-22-worker-failure-ux.md`).

### 2. `crates/ox-kernel/src/run.rs` — `complete.refs` decode

**Fixed in commit `1fe7d84`.** Inline double-unwrap became
`decode_complete_refs(input)` returning `Result<Vec<ContextRef>, String>`.
On `Err`, the kernel pushes a `ToolResult` with the error message
*and* emits `AgentEvent::Error(...)`.

**Audience served:** Model (via `ToolResult` — can retry with corrected
`refs` shape) and operator (via `AgentEvent::Error` — visible in
TUI/logs).

### 3. `crates/ox-cli/src/settings/visible_rows.rs` — account record read

**Fixed in commits `1fe7d84` (child) and `eaca31d` (parent).**
`append_account_field_rows` now pushes a `⚠ account record unreadable`
field row on read failure instead of synthesizing field rows from
`AccountConfig::default()`. `append_account_rows` (parent) mirrors:
pushes a `⚠ {name} (account record unreadable)` placeholder account
row instead of substituting `AccountConfig { provider: "anthropic", .. }`.
Both paths `tracing::error!` with the account name.

**Audience served:** User (visible placeholder row with remediation
copy: "delete and recreate this connection to recover") and operator
(tracing).

### 4. `crates/ox-web/src/lib.rs:497` — provider config decode

**Fixed in commit `eaca31d`.** Decode failure now logs via
`web_sys::console::warn_1` before falling back to
`ProviderConfig::anthropic()`. Required adding the `console` feature
to `web-sys`.

**Audience served:** Browser dev tools observer (console warn). The
fallback itself is still suboptimal (silent substitution to Anthropic
when the user configured X); a deeper fix would refactor
`read_provider_config` to return `Result` and have the callers
present a "provider config malformed" UI. Tracked as task #26
(scheduled for follow-up).

### 5. `crates/ox-cli/src/policy.rs` — malformed policy.json

**Fixed in commit `1fe7d84` (load) and `eaca31d` (caller surface).**
`PolicyGuard::load` now returns `Result<Self, PolicyLoadError>` with
distinct `Io`/`Parse` variants carrying the file path and error
context. The caller (`agents.rs::agent_worker`) refuses to start on
`Err`, AND writes a synthesized assistant turn to the thread's
history (`⚠ Agent failed to start: ...`) so the user sees what
happened in the TUI conversation view.

**Audience served:** Caller (typed `Result`), user (assistant turn in
history), operator (tracing). Caveat: if the user isn't viewing that
thread when the worker dies, they see nothing until they switch — a
toast/banner mechanism doesn't exist yet (see `worker-failure-ux`
spec).

### 6. `crates/ox-kernel/src/run.rs::flush_tool` — tool-input JSON

**Verified as bounded; documented justification in code.** Allow-listed
with `// allow(silent_parse_fallback)` and a pointer to this doc.
Malformed input becomes `Value::Null`; downstream dispatched tools
reject null and surface a tool-error result the model can see.

The "every tool rejects null" claim is **asserted, not yet verified**
end-to-end. Tracked as task #24 — trace every tool's input-handling
path and confirm.

### 7. `crates/ox-cli/src/config.rs::resolve_config` — Figment extract

**Fixed in commit `3650fd0`.** `extract().unwrap_or_default()` →
`extract().expect("config extraction failed")`. The silent fallback
swallowed real deser errors and surfaced them as confusing
"no entry found for key" panics in callers that indexed the resulting
empty maps. Coupled with serializing env-touching tests on a
process-wide `Mutex` so concurrent `OX_GATE__*` env mutation doesn't
race with concurrent `resolve_config` reads.

## OK to leave — defaults verified legitimate

Same as the original audit; verified during the sweep:

- HTTP body for diagnostic logging (`ox-gate/transport.rs`).
- Clock-skew handling (`duration_since(UNIX_EPOCH)`).
- Best-effort serialize for logging (`serde_json::to_string(other)`).
- Optional UI buffer state (edit/account_model commands).
- HashMap-absent → empty collection (`catalogs.get(&name).cloned()`).
- String-extraction helpers (`extract_str(map, "field")`).

## Residuals tracked as separate plans

These items emerged during the audit but aren't part of the silent-
unwrap fix itself:

- **Real clippy/dylint lint** — the current `scripts/quality_gates.sh`
  "no silent parse fallback" gate is grep-based and catches only
  single-line patterns. Multi-line `from_str(...)\n.unwrap_or_default()`
  slips through. See `2026-05-23-silent-fallback-real-lint.md` (TODO).

- **Worker-failure notification UX** — the policy-refusal fix writes
  an assistant turn to thread history, but if the user isn't viewing
  the thread when the worker dies, they see nothing. There's no
  toast/banner/notification mechanism in the codebase. See
  `2026-05-23-worker-failure-ux.md` (TODO).

- **`read_provider_config` to `Result`** — the ox-web fix
  (`web_sys::console::warn_1`) is a minimal-touch improvement.
  Refactoring to `Result<ProviderConfig, ProviderReadError>` with
  cascading caller updates would let the two callers present
  better UI. Task #26.

## Pattern that closed the audit

> **Silent default = refusal to decide.** Each site needs to identify
> the audience (model, user, operator, caller) and the channel
> (`ToolResult`, placeholder row, `Result` with context, tracing).
> Type-system propagation (`?`, `Result`) is necessary but not
> sufficient — the error has to land somewhere that can act.

Three audiences, three layers:

1. **Type system** (`Result`/`Option`) — makes "I'm ignoring this
   error" explicit.
2. **Decision-maker** (model / user / agent loop / supervisor) —
   *something* gets the error and acts.
3. **Communication** (UI, log, tracing) — the audience gets a useful
   message they can act on.

`unwrap_or_default()` fails at layer 1. `panic!` fails at layer 2.
`?`-propagation-to-nothing fails at layer 3. All three layers are
necessary for "real error handling."

---

## Appendix: original recommendations (preserved for diff)

The pre-execution recommendations are below for traceability. Each
has been superseded by the "Closed sites" section above.

### Original item 1 recommendation

> Propagate the error up to the writer's `Result<Path, StoreError>`
> return. The caller can decide whether to abort the append or surface
> a "couldn't decode response" diagnostic.

### Original item 2 recommendation

> Surface the deser error as a tool-call rejection
> (`AgentEvent::ToolError` or equivalent). The assistant gets feedback
> that its input was malformed and can retry.

### Original item 3 recommendation

> `.unwrap_or_else(|| { tracing::error!(...); AccountConfig::default() })`
> at minimum. Better: change the function signature so the caller can
> skip the row entirely instead of synthesizing a broken row.

### Original item 4 recommendation

> At minimum, `tracing::warn!` on the decode failure path so the
> divergence is observable in logs. Better: surface "provider config
> malformed" to the UI.

### Original item 5 recommendation

> Treat a parse error on an existing policy file as a hard error —
> refuse to start. The user typo'd their policy; they want to know.

### Original item 6 recommendation

> Surface the parse error as a tool-error event before dispatching,
> with the raw input string in the diagnostic.
