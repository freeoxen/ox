# Phase 2: Eliminate `delete_now` sentinel — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move account-deletion off the `…/delete_now` sentinel pattern. The CLI writes `Null` to `config/gate/accounts/<name>` directly; the subscription transforms from `AccountDeleteSubscription` (PrefixSuffix watch on `delete_now`) into `AccountDeleteCleanupSubscription` (Prefix watch on `config/gate/accounts`, filtered to null writes at account-record depth) and handles only the cross-cutting side-data cleanup that remains.

**Architecture:** The user's null-write to the canonical account path IS the delete. The subscription becomes a reactive observer of that delete: it cleans up the API key, the synthesized provider record, the `accounts/selected` pointer if it matched, and writes the modal-era cursor-back behavior unchanged (Phase 4 will rebuild that when the delete-confirm modal becomes mode state). User-visible behavior is unchanged.

**Tech Stack:** Rust workspace; `ox-cli` (the delete command), `ox-gate` (the transformed subscription).

**Spec:** [docs/superpowers/specs/2026-05-06-settings-substrate-convergence-design.md](../specs/2026-05-06-settings-substrate-convergence-design.md) §5 Phase 2.

---

## File Map

**Modify:**
- `crates/ox-cli/src/settings/commands/account_model.rs` — `accounts_delete` writes Null to `config/gate/accounts/<name>` directly, not to the `delete_now` subpath. Update its test.
- `crates/ox-gate/src/subscriptions/account_delete.rs` — rename the type to `AccountDeleteCleanupSubscription`, change the watch pattern, add the depth+null filter, drop the now-redundant account-record null-write from the cleanup body. Update its tests.
- `crates/ox-gate/src/subscriptions/mod.rs` — update the registration to use the new type name; update the doc comment listing the registration order.

**Delete:**
- (none — the file stays at `account_delete.rs`; only the type name and behavior change)

---

## Conventions used in this plan

- All `cargo` commands run synchronously. Do not background.
- Commits are NEW commits (no `--amend`). No `Co-Authored-By` trailer.
- No phase / PR / spec-section comments in code. Doc comments explaining WHY are fine.

---

## Important: atomic landing

The CLI write (Task 1) and the subscription transform (Task 2) MUST land in the same commit, or the workspace is broken between them:

- If only Task 1 lands: CLI writes Null to the canonical path; the OLD subscription watches `PrefixSuffix(delete_now)` and doesn't fire; account record is deleted but side data (key, provider) becomes orphan.
- If only Task 2 lands: CLI still writes Null to `…/delete_now`; the NEW subscription's filter at the top of `handle` rejects writes that aren't at account-record depth; nothing happens; the user can't delete accounts.

For this reason, Task 1 and Task 2 below are **drafted as a single commit**. The plan presents them as separate sections for review clarity, but the implementer applies all the edits and commits once. Steps 1–4 of Task 1 and Steps 1–4 of Task 2 happen in the same working tree before the build / test / commit at the end.

---

## Task 1+2: Atomic substrate transform

Apply both halves before building or committing.

### Task 1: CLI writes Null to canonical account path

**File:**
- Modify: `crates/ox-cli/src/settings/commands/account_model.rs`

The current `accounts_delete` (around lines 353-362):

```rust
fn accounts_delete(data: &mut dyn Reader) -> Vec<Write> {
    let name = match read_selected_account(data) {
        Some(n) => n,
        None => return Vec::new(),
    };
    match account_request_path(&name, "delete_now") {
        Some(p) => vec![null_write(p)],
        None => Vec::new(),
    }
}
```

`account_request_path(&name, "delete_now")` builds `config/gate/accounts/<name>/delete_now`. After Phase 2, we want `config/gate/accounts/<name>` (no suffix). The `account_request_path` helper still has callers (`test_now`, `refresh_now`), so it stays.

- [ ] **Step 1.1: Replace the function body**

```rust
fn accounts_delete(data: &mut dyn Reader) -> Vec<Write> {
    use ox_kernel::PathComponent;
    let name = match read_selected_account(data) {
        Some(n) => n,
        None => return Vec::new(),
    };
    let comp = match PathComponent::try_new(&name) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    vec![Write {
        path: oxpath!("config", "gate", "accounts", comp),
        record: Record::parsed(Value::Null),
    }]
}
```

If `ox_kernel::PathComponent` isn't already imported at the file level, the function-scoped `use` keeps the change self-contained. The `null_write` helper isn't used here (it's a gate-side helper); construct the `Write` directly with `Value::Null`.

- [ ] **Step 1.2: Update the test**

The existing test `accounts_delete_writes_delete_now_when_selected` (around line 1137) asserts the old shape. Replace its body with the new shape:

```rust
#[test]
fn accounts_delete_writes_null_to_canonical_account_path_when_selected() {
    let mut snap = SettingsSnapshot::empty();
    select_account(&mut snap, "alpha");
    let writes = run_cmd(&AccountsDelete::new(), &mut snap);
    let comp = ox_kernel::PathComponent::try_new("alpha").unwrap();
    let expected_path = oxpath!("config", "gate", "accounts", comp);
    let hit = writes.iter().any(|w| {
        w.path == expected_path && matches!(&w.record, Record::Parsed(Value::Null))
    });
    assert!(
        hit,
        "expected Null write at config/gate/accounts/alpha; got {writes:?}"
    );
}
```

The companion `accounts_delete_inert_without_selection` test (around line 1149) does NOT need to change — its assertion is `writes.is_empty()`, which still holds when no account is selected.

### Task 2: Transform the subscription into a reactive observer

**File:**
- Modify: `crates/ox-gate/src/subscriptions/account_delete.rs` (rename the type and change the handler).
- Modify: `crates/ox-gate/src/subscriptions/mod.rs` (update the registration).

Current shape: `AccountDeleteSubscription` watches `PrefixSuffix { prefix: config/gate/accounts, suffix: delete_now }`. Its handler extracts the instance name via `instance_segment`, then nulls the account record + key + provider, conditionally clears selection, writes cursor.

After: `AccountDeleteCleanupSubscription` watches `Prefix(config/gate/accounts)`. Its handler filters at the top: only react when (a) the change path is at account-record depth (prefix.len() + 1 components) AND (b) `change.after` is `Null`. The body drops the redundant account-record null-write (the user already did it) but keeps everything else.

- [ ] **Step 2.1: Rename the type and update its constructor**

In `crates/ox-gate/src/subscriptions/account_delete.rs`, replace the type definition + impl block headers:

```rust
pub const ID: &str = "gate.account_delete_cleanup";

pub struct AccountDeleteCleanupSubscription {
    id: SubscriptionId,
    watches: Vec<PathPattern>,
}

impl Default for AccountDeleteCleanupSubscription {
    fn default() -> Self {
        Self::new()
    }
}

impl AccountDeleteCleanupSubscription {
    pub fn new() -> Self {
        Self {
            id: SubscriptionId(ID.to_string()),
            watches: vec![PathPattern::Prefix(oxpath!("config", "gate", "accounts"))],
        }
    }
}
```

Update the corresponding `impl Subscription for ...` block to reference the new type name.

Update the module-level doc comment at the top of the file:

```rust
//! `AccountDeleteCleanupSubscription` — fires on null writes at
//! `config/gate/accounts/{name}` (account-record depth).
//!
//! Reactive observer of account deletion. The CLI's `accounts.delete`
//! command writes `Null` to the canonical account path; this
//! subscription watches the broader `Prefix(config/gate/accounts)`
//! pattern and filters at the top of `handle` for null writes at
//! account-record depth (one component below the prefix).
//!
//! Cleanup body fans out the cross-cutting work the CLI shouldn't do
//! itself: deletes the API key, deletes the synthesized provider
//! record, clears the `accounts/selected` pointer if it matched the
//! deleted account, and pops the cursor back to the (modal-era)
//! accounts page. The cursor write preserves Phase-2 behavior;
//! Phase 4's mode-state delete-confirm rebuild reshapes the
//! cursor cascade.
```

- [ ] **Step 2.2: Rewrite the handler**

Replace the current `handle` body with:

```rust
fn handle(&self, ctx: SubCtx<'_>) -> Vec<Write> {
    use structfs_core_store::Value;

    let prefix = oxpath!("config", "gate", "accounts");

    // Filter 1: only react at account-record depth (prefix + 1 component).
    // Writes to children (`.../models`, `.../test_status`, etc.) get
    // skipped here.
    if ctx.change.path.len() != prefix.len() + 1 {
        return vec![];
    }

    // Filter 2: only react to deletes (Null writes). Updates and
    // creates fall through.
    let Some(record) = ctx.change.after.as_ref() else {
        return vec![];
    };
    if !matches!(record.as_value(), Some(Value::Null)) {
        return vec![];
    }

    // Extract the account name. The path's last component is the
    // account identifier; we already validated depth above.
    let name = ctx.change.path.components.last().cloned().unwrap_or_default();
    if name.is_empty() {
        return vec![];
    }

    // Side-data cleanup. The account record itself is already gone
    // (the user's null-write triggered us); we don't repeat that.
    let mut writes: Vec<Write> = Vec::new();

    // Delete the API key.
    if let Ok(p) = secret_key_path(&name) {
        writes.push(null_write(p));
    }
    // Delete the synthesized provider entry. v1 has one provider
    // per account named after the account; nothing to do for users
    // who hand-edit shared providers.
    if let Ok(p) = provider_path(&name) {
        writes.push(null_write(p));
    }

    // Clear selection if it pointed at the deleted account.
    let selected_path = oxpath!("ui", "settings", "accounts", "selected");
    let selected: Option<String> = read_typed_via_reader(ctx.snapshot, &selected_path);
    if selected.as_deref() == Some(name.as_str()) {
        writes.push(null_write(selected_path));
    }

    // Cursor back to the accounts list (modal-era behavior; Phase 4
    // rebuilds this when delete-confirm becomes mode state).
    writes.push(write_path(
        &oxpath!("ui", "settings", "cursor"),
        &oxpath!("settings", "accounts"),
    ));

    writes
}
```

The imports at the top of the file may need adjustment:
- `instance_segment` is no longer used; remove it from the `use crate::subscriptions::util::{...}` line.
- `account_path` is no longer used (we don't read or write the account path here); remove it from the same line.
- `crate::AccountConfig` is no longer used (we don't read the account record); remove it.
- `read_typed_via_reader`, `null_write`, `provider_path`, `secret_key_path`, `write_path` stay.

Run `cargo build -p ox-gate` to surface any leftover unused imports.

- [ ] **Step 2.3: Update the subscription's tests**

The existing tests in `account_delete.rs::tests` drive the subscription with a `delete_now` trigger path and assert specific writes. Update them for the new trigger shape.

The `trigger_path` helper:

```rust
fn trigger_path(name: &str) -> Path {
    let comp = ox_kernel::PathComponent::try_new(name).unwrap();
    oxpath!("config", "gate", "accounts", comp)  // canonical account path, not delete_now subpath
}
```

The `drive` helper's `PathChange` should reflect a delete (after = Null):

```rust
fn drive(reader: &mut InMemoryReader, name: &str) -> Vec<Write> {
    let sub = AccountDeleteCleanupSubscription::new();
    let path = trigger_path(name);
    let change = PathChange {
        path,
        before: Some(Record::parsed(/* anything non-null; the handler doesn't read before */ Value::Null)),
        after: Some(Record::parsed(Value::Null)),
    };
    let spawn = TestSpawn::new();
    let writer = Arc::new(CapturingWriter::new()) as Arc<dyn AsyncWriter>;
    let ctx = SubCtx {
        snapshot: reader,
        change: &change,
        spawn: &spawn,
        writer,
    };
    sub.handle(ctx)
}
```

Existing test assertions also need adjustment:
- The `account_record_is_deleted` (or similarly-named) assertion expecting a null-write at `config/gate/accounts/<name>` should be removed or changed — that write is now done by the user, not the subscription. Look for any test that asserted the subscription produces an account-record null-write and adjust it.
- Tests asserting key/provider/selection/cursor writes still hold; those writes still happen.

ADD two new tests pinning the filters:

```rust
#[test]
fn cleanup_skips_writes_to_child_paths() {
    let mut reader = InMemoryReader::new();
    populate_anthropic_account(&mut reader, "alpha", "sk-test");
    // Drive a write to a child path (e.g. .../models). The subscription
    // must not fire its cleanup body — that path isn't an account
    // record, even though it matches the Prefix watch.
    let sub = AccountDeleteCleanupSubscription::new();
    let comp = ox_kernel::PathComponent::try_new("alpha").unwrap();
    let path = oxpath!("config", "gate", "accounts", comp, "models");
    let change = PathChange {
        path,
        before: None,
        after: Some(Record::parsed(Value::Null)),
    };
    let spawn = TestSpawn::new();
    let writer = Arc::new(CapturingWriter::new()) as Arc<dyn AsyncWriter>;
    let ctx = SubCtx {
        snapshot: &mut reader,
        change: &change,
        spawn: &spawn,
        writer,
    };
    let writes = sub.handle(ctx);
    assert!(writes.is_empty(), "child-path writes must not trigger cleanup; got {writes:?}");
}

#[test]
fn cleanup_skips_non_null_writes_at_account_depth() {
    let mut reader = InMemoryReader::new();
    populate_anthropic_account(&mut reader, "alpha", "sk-test");
    // An update (non-null write) at the account-record path should not
    // trigger the cleanup body — the account isn't being deleted.
    let sub = AccountDeleteCleanupSubscription::new();
    let comp = ox_kernel::PathComponent::try_new("alpha").unwrap();
    let path = oxpath!("config", "gate", "accounts", comp);
    let cfg = crate::AccountConfig { provider: "anthropic".into() };
    let change = PathChange {
        path,
        before: None,
        after: Some(Record::parsed(structfs_serde_store::to_value(&cfg).unwrap())),
    };
    let spawn = TestSpawn::new();
    let writer = Arc::new(CapturingWriter::new()) as Arc<dyn AsyncWriter>;
    let ctx = SubCtx {
        snapshot: &mut reader,
        change: &change,
        spawn: &spawn,
        writer,
    };
    let writes = sub.handle(ctx);
    assert!(writes.is_empty(), "non-null writes at account depth must not trigger cleanup; got {writes:?}");
}
```

- [ ] **Step 2.4: Update the registration in mod.rs**

In `crates/ox-gate/src/subscriptions/mod.rs`, the `register_all` function references `account_delete::AccountDeleteSubscription`. Update to:

```rust
broker.register_subscription(Arc::new(account_delete::AccountDeleteCleanupSubscription::new()));
```

The doc comment listing the registration order also names `account_delete` as `instance-segment trigger, fires on …/delete_now`; update to:

```rust
/// 3. `account_delete_cleanup` — reactive observer, fires on null
///    writes to `config/gate/accounts/<name>` (account-record depth)
```

(Adjust the line number to match the post-Phase-1 numbering — Phase 1 already removed the old `4. account_create` line.)

### Final steps for the combined task

- [ ] **Step F.1: Build the workspace**

```
cargo build -p ox-cli
cargo build -p ox-gate
```

Expected: both PASS. Any unused-import warning needs fixing per Step 2.2's note.

- [ ] **Step F.2: Run the gate tests**

```
cargo test -p ox-gate --lib
```

Expected: PASS. Test count should reflect: existing `account_delete` tests still passing (modulo the rename), plus 2 new tests for the filters.

- [ ] **Step F.3: Run the ox-cli lib + e2e**

```
cargo test -p ox-cli --lib
cargo test -p ox-cli --test settings_e2e
```

Expected: PASS. The e2e `delete_account_flow` should still pass — the user-visible behavior is unchanged. The test polls for the account record being gone; that still works (the user's null-write does it directly now, instead of the subscription doing it).

If the e2e test relies on a specific sequence of subscription writes (e.g. asserts the account record is null AFTER the cleanup), the assertion may need adjustment — the account record is now null BEFORE the cleanup runs (the CLI deleted it). If a test poll waits for "all the cleanup to finish," it still works; if it specifically asserted "the subscription wrote the null at the account path," that one needs updating.

- [ ] **Step F.4: Commit**

```
git add -u
git commit -m "feat(settings): direct delete + reactive cleanup subscription

CLI's accounts.delete writes Null to config/gate/accounts/<name>
directly. The user's write IS the delete; no sentinel indirection.

AccountDeleteSubscription is renamed to
AccountDeleteCleanupSubscription and refocused as a reactive
observer of the actual delete. Watches Prefix(config/gate/accounts)
with a top-of-handler filter for null writes at account-record
depth. Body still does the cross-cutting cleanup the CLI shouldn't
do directly: secret key, provider record, conditional selection
clear. The modal-era cursor-back write is preserved (Phase 4
rebuilds it when the delete-confirm modal becomes mode state).

Two new filter tests pin the boundary: writes to child paths
(.../models) and non-null writes at account depth must not fire
the cleanup body."
```

---

## Task 3: Final verification

- [ ] **Step 1: Full workspace test run**

```
cargo test --workspace
```

Expected: PASS. Test count delta: gate +2 (the new filter tests), cli ±0 (assertion updates, no count change).

- [ ] **Step 2: Clippy**

```
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: PASS.

- [ ] **Step 3: Verify no stragglers**

```
grep -rn '/delete_now\|delete_now"\|delete_now)' crates/ 2>/dev/null
```

Expected: zero hits in source code. Acceptable hits in `docs/` (the spec, this plan, the framework docs reference the historical name). The CLI's `account_request_path("delete_now")` call is the one to verify is GONE — it should be, per Step 1.1.

```
grep -rn 'AccountDeleteSubscription' crates/ 2>/dev/null
```

Expected: zero hits — all renamed to `AccountDeleteCleanupSubscription`.

- [ ] **Step 4: Smoke-test in the TUI**

The harness can't run the interactive TUI. Ask the user to:

1. Open settings.
2. Create a connection (e.g. `personal`), provide an API key.
3. Press `d` to open delete confirmation.
4. Press `y` to confirm delete.
5. Confirm: the connection is gone from the accordion, the API key is gone, no error banner.
6. Verify there's no orphan provider record (only relevant if you have multiple connections sharing a provider; otherwise skip).

If anything misbehaves, it's a regression — investigate before declaring Phase 2 complete.

---

## Self-review checklist

- [x] CLI's `accounts_delete` writes Null to canonical account path (Task 1).
- [x] `AccountDeleteSubscription` renamed to `AccountDeleteCleanupSubscription` (Task 2.1).
- [x] Watch pattern changed from `PrefixSuffix(delete_now)` to `Prefix(config/gate/accounts)` (Task 2.1).
- [x] Top-of-handler filter rejects child-path writes and non-null writes (Task 2.2).
- [x] Account-record null-write removed from the cleanup body (the user does it now) (Task 2.2).
- [x] Side-data cleanup (key, provider, selection, cursor) preserved (Task 2.2).
- [x] Subscription registration updated (Task 2.4).
- [x] Tests updated; new filter tests added (Task 2.3 + Step F.2).

Spec requirements not addressed by this plan (intentionally):
- The cursor write at the end of the cleanup still goes to `settings/accounts` (the modal-era page). Phase 4 rebuilds the delete flow on mode state and reshapes this cursor cascade. Preserving today's behavior keeps Phase 2 a substrate-only refactor.
