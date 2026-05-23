# Silent-unwrap audit

Triggered by the `legacy_account_endpoint_is_migrated_to_provider` flake:
`resolve_config` swallowed Figment extract errors with
`unwrap_or_default()` and surfaced them as confusing "no entry found
for key" panics elsewhere. Fixed in commit `3650fd0`.

This audit sweeps the codebase for similar shapes — silent fallbacks
that hide real errors behind suspiciously-empty defaults. Each entry
classifies the risk and notes whether the default is wrong.

## High risk — fix these

### 1. `crates/ox-history/src/lib.rs:336`

```rust
let content: Vec<ContentBlock> =
    serde_json::from_value(content_json).unwrap_or_default();
self.shared.append(LogEntry::Assistant {
    content,  // ← may be silently empty
    ...
})?;
```

**Why it's wrong.** When deserializing a streamed assistant message's
content, if the JSON shape doesn't match `Vec<ContentBlock>` (schema
drift, an unknown variant added by the model server, a malformed
streaming chunk), the entire assistant turn is appended with an empty
content array. The user sees an empty assistant message and has no
indication anything went wrong. The on-disk log now contains a
content-less assistant entry that downstream replay will treat as
"the assistant said nothing."

**Recommended fix.** Propagate the error up to the writer's
`Result<Path, StoreError>` return. The caller can decide whether to
abort the append or surface a "couldn't decode response" diagnostic.

### 2. `crates/ox-kernel/src/run.rs:1540`

```rust
let inner_refs: Vec<ContextRef> =
    serde_json::from_value(tc.input.get("refs").cloned().unwrap_or_default())
        .unwrap_or_default();
```

**Why it's wrong.** Double-unwrap on the `refs` argument of a tool
call. If the assistant emits a `refs` field that doesn't deserialize
into `Vec<ContextRef>` — a typo in the LLM's output, a schema we
don't yet handle — the completion frame proceeds with **no context
refs at all**. The user asked the assistant to look at three files;
the assistant tried to pass them through; the parser silently
dropped them; the completion runs without them. Silent behavioral
regression.

**Recommended fix.** Surface the deser error as a tool-call rejection
(`AgentEvent::ToolError` or equivalent). The assistant gets feedback
that its input was malformed and can retry.

### 3. `crates/ox-cli/src/settings/visible_rows.rs:415`

```rust
// `read_typed::<AccountConfig>(...).unwrap_or_default()` would
// return `AccountConfig { provider: "" }` here — the empty
// provider then fails `PathComponent::try_new` and the bound
// provider can't be resolved, which silently empties the
// Endpoint / Auth field rows and locks the Protocol carousel at
// its idx-0 fallback.
let acct = read_account_assembling_flat(data, name).unwrap_or_default();
```

**Why it's wrong.** The author *documented* the failure mode in the
comment immediately above the call — empty default cascades into
silently-empty field rows and a stuck carousel — and shipped the
silent-default call anyway. `read_account_assembling_flat` returning
None means the broker has no readable account at this name; the
caller is enumerating visible accounts from `child_names_under`, so
that should be impossible in well-formed data. If it happens, it's a
state-machine bug we want loud.

**Recommended fix.** `.unwrap_or_else(|| { tracing::error!(account = %name,
"account row visible but record unreadable"); AccountConfig::default() })`
at minimum. Better: change the function signature so the caller can
skip the row entirely (`return Vec::new()` from the field-rows
appender) instead of synthesizing a broken row.

## Medium risk — worth a comment + tracing

### 4. `crates/ox-web/src/lib.rs:497`

```rust
match ctx.read(&provider_path) {
    Ok(Some(Record::Parsed(v))) => {
        structfs_serde_store::from_value(v).unwrap_or_else(|_| ProviderConfig::anthropic())
    }
    _ => ProviderConfig::anthropic(),
}
```

**Why it's borderline.** When the broker returns a non-empty Value
that doesn't decode as `ProviderConfig`, the web frontend falls back
to Anthropic. The user thinks they're talking to LM Studio (or
whatever the broker's provider record says); they're actually
talking to Anthropic. Silent provider substitution = silently routing
the user's request to a different endpoint than configured.

**Recommended fix.** At minimum, `tracing::warn!` on the decode
failure path so the divergence is observable in logs. Better: surface
"provider config malformed" to the UI so the user knows their config
is broken instead of getting unexpected results.

### 5. `crates/ox-cli/src/policy.rs:54`

```rust
let manifest = if policy_path.exists() {
    match std::fs::read_to_string(&policy_path) {
        Ok(content) => {
            serde_json::from_str(&content).unwrap_or_else(|_| default_manifest())
        }
        Err(_) => default_manifest(),
    }
} else {
    default_manifest()
};
```

**Why it's borderline.** Security-relevant. If `.clash/policy.json`
has a syntax error, the policy guard falls back to the **default
manifest** without telling the user. The user thinks they have policy
X; they actually have permissive defaults. Wrong policy = wrong
access decisions. The pattern of "file exists, contents don't parse,
fall back to defaults" is exactly how the Equifax breach happened in
spirit if not in detail.

**Recommended fix.** Treat a parse error on an existing policy file
as a hard error — refuse to start. The user typo'd their policy;
they want to know. The "file doesn't exist → defaults" path is fine
(opt-in policy); the "file exists but is malformed → defaults" path
is a footgun.

### 6. `crates/ox-kernel/src/run.rs:1586`

```rust
let input: serde_json::Value =
    serde_json::from_str(&input_json).unwrap_or(serde_json::Value::Null);
```

**Why it's borderline.** Tool-use input from the streamed assistant
response. If the JSON is malformed, the tool runs with `null` input.
Bounded blast radius — most tools will reject `null` and surface an
error — but the user's request gets a confusing failure instead of a
clear "the assistant emitted unparseable JSON" diagnostic.

**Recommended fix.** Surface the parse error as a tool-error event
before dispatching, with the raw input string in the diagnostic.

## OK to leave — defaults are legitimate

These all returned silent defaults that the user looking at the data
would describe as "field is absent / not set," not as "we lost
information." Leave them.

- **HTTP body for diagnostic logging** —
  `crates/ox-gate/src/transport.rs:{457,463,537,592}`. The body is
  consumed only for a `tracing::warn!` line on an already-failing
  response. `""` is fine.

- **Clock-skew handling** — `crates/ox-gate/src/validation.rs:72`,
  `duration_since(UNIX_EPOCH).unwrap_or_default()`. Pre-epoch clock
  → 0ms. Fine.

- **Best-effort serialize for logging** —
  `serde_json::to_string(other).unwrap_or_default()` across
  `ox-kernel/src/{log.rs,lib.rs,run.rs}`, `ox-history/src/lib.rs`,
  `ox-wasm/src/lib.rs`. Diagnostic logs; empty string on serialize
  failure beats panicking the log path.

- **Optional UI buffer state** —
  `crates/ox-cli/src/settings/commands/{edit,account_model}.rs`
  hits for edit-buffer reads. Empty buffer is the canonical
  "no buffer yet" — exactly what the user wants.

- **HashMap-absent → empty collection** —
  `crates/ox-gate/src/lib.rs:406`, `self.catalogs.get(&name).cloned().unwrap_or_default()`.
  An account with no cached catalog → empty Vec. Correct.

- **String-extraction helpers from Map** —
  `crates/ox-ui/src/{input_store,ui_store}.rs`, `extract_str(map, "field").unwrap_or_default()`.
  Optional fields in serialized state; absence = `""` is OK for
  the surfaces that consume these (status text, descriptions).

- **`config.gate.accounts[…]` indexing in tests** — fine *because*
  the env-touching tests now serialize on a process-wide `Mutex`
  (commit `3650fd0`) and `resolve_config` `expect`s its extract.
  The shape we fixed.

## Recommended action

Three high-risk items (1, 2, 3) are worth dedicated fixes. Five min
each, big payoff in observability. Suggest a single commit titled
`fix: stop swallowing parse/decode errors in history/run/visible_rows`
that:

- Returns an error from `HistoryView::write`'s assistant branch on
  decode failure.
- Emits a `ToolError` event when tool-call `refs` won't parse.
- Removes the silent `unwrap_or_default` in `visible_rows.rs:415`
  and surfaces the failure as a `tracing::error!` (or skips the row
  entirely).

The two medium-risk items (4, 5) are worth doing in a separate
commit because the policy-parse change is security-adjacent and
deserves its own review:

- `ox-web/src/lib.rs:497`: add `tracing::warn!` on the decode-failure
  path before falling back.
- `ox-cli/src/policy.rs:54`: refuse to start when an existing
  `policy.json` won't parse. Treat parse-error as "user told us
  about the file but lied about its shape" — exactly the case where
  silent defaults are dangerous.

Item 6 (tool-input JSON) can wait — the failure path is bounded and
surfaces eventually.
