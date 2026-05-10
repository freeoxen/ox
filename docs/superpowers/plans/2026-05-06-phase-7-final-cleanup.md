# Phase 7: Final cleanup — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Retire transitional cruft now that Phases 0-6 have removed every synthetic display path and sentinel. Three targeted cleanups: drop the `_`-prefix banner-error rule from `accounts.compose.commit` (no longer protecting anything); delete `CreateAccountRequest` (zero production callers since Phase 1); remove the convergence note from `ui_framework.md` (the docs and code now agree).

**Architecture:** Single atomic commit. None of these depend on each other; they're all dead-code/dead-doc removals.

**Tech Stack:** Rust workspace; `ox-cli` (compose-commit cleanup), `ox-types` (deprecated type), `docs/`.

**Spec:** [docs/superpowers/specs/2026-05-06-settings-substrate-convergence-design.md](../specs/2026-05-06-settings-substrate-convergence-design.md) §5 Phase 7.

---

## File Map

**Modify:**
- `crates/ox-cli/src/settings/commands/account_model.rs` — drop the `_`-prefix branch in `accounts_compose_commit`; drop the corresponding test (`accounts_compose_commit_with_underscore_prefix_emits_banner`).
- `crates/ox-types/src/settings.rs` — delete `CreateAccountRequest` type + its roundtrip test.
- `docs/ui_framework.md` — delete the "Convergence note" section.
- `docs/ui_framework/reference.md` — remove `CreateAccountRequest` references (the type-section line + the file-map mention).

---

## Conventions used in this plan

- All `cargo` commands run synchronously. Do not background.
- Commits are NEW commits (no `--amend`). No `Co-Authored-By` trailer.
- No phase / PR / spec-section / Task-N comments.

---

## Task 1: Atomic cleanup commit

### Sub-task 1.1: Drop the `_`-prefix banner rule from `accounts.compose.commit`

**File:**
- Modify: `crates/ox-cli/src/settings/commands/account_model.rs`

The current `accounts_compose_commit` (around lines 484-500) has a `_`-prefix check that emits a banner-error. The rule was load-bearing while `settings/accounts/_new` was a synthetic ghost-row identifier (Phase 0-2 era). After Phase 3 retired the synthetic row and Phase 6 dropped the last synthetic display path, no display tree paths can collide with user-supplied account names. The data tree's only sentinel was `_create_now` (Phase 1 retired). The rule is purely vestigial.

- [ ] **Step 1: Drop the branch**

In `accounts_compose_commit`, locate the block:

```rust
// `_`-prefix: kept transitionally; Phase 7 retires this rule once
// there are no remaining sentinel paths to collide with.
if trimmed.starts_with('_') {
    return vec![banner_error(format!(
        "Account name '{}' starts with '_', which is reserved. Try a name without the leading underscore.",
        trimmed
    ))];
}
```

Delete it entirely. Comments + body. The remaining flow goes straight from "empty/whitespace check" → `PathComponent::try_new` validation. Names like `_personal` and `_my_acct` now create accounts at `config/gate/accounts/_personal` etc.

- [ ] **Step 2: Drop the underscore-prefix test**

In account_model.rs's `#[cfg(test)] mod tests`, delete `accounts_compose_commit_with_underscore_prefix_emits_banner` (and any sibling tests like `accounts_compose_commit_with_underscore_prefix_keeps_buffer_open` that exercise the rule).

The remaining tests (`accounts_compose_commit_writes_account_record_and_cascade`, `accounts_compose_commit_with_empty_buffer_silent_no_op`, `accounts_compose_commit_with_invalid_name_emits_banner`, `accounts_compose_commit_with_interior_underscore_writes_account_record`) continue to pin the expected behavior.

- [ ] **Step 3: Add a positive test for `_`-prefix names**

Add a regression guard pinning that `_`-prefixed names now create real accounts (not banner errors):

```rust
#[test]
fn accounts_compose_commit_with_leading_underscore_writes_account_record() {
    let mut snap = SettingsSnapshot::empty();
    snap.insert(
        &oxpath!("ui", "settings", "new_account", "buffer"),
        Value::String("_personal".into()),
    );
    let writes = run_cmd(&AccountsComposeCommit::new(), &mut snap);
    let by_path: std::collections::BTreeMap<_, _> = writes
        .iter()
        .map(|w| (w.path.to_string(), w.record.clone()))
        .collect();

    // Account record materialized at config/gate/accounts/_personal.
    let acct = by_path
        .get("config/gate/accounts/_personal")
        .expect("account record write at canonical _personal path");
    let cfg: ox_gate::AccountConfig = match acct {
        Record::Parsed(v) => structfs_serde_store::from_value(v.clone()).unwrap(),
        other => panic!("unexpected: {other:?}"),
    };
    assert_eq!(cfg.provider, "anthropic");

    // No banner write — the leading-underscore rule is gone.
    assert!(
        !by_path.contains_key("ui/global/banner"),
        "_-prefixed names must no longer emit a reservation banner"
    );
}
```

### Sub-task 1.2: Delete `CreateAccountRequest`

**File:**
- Modify: `crates/ox-types/src/settings.rs`

The type was the wire payload for `AccountCreateSubscription`'s `_create_now` sentinel. Phase 1 deleted both. The type's only remaining references in `crates/` are its definition and its own roundtrip test in `ox-types`.

- [ ] **Step 1: Verify no production callers**

```
grep -rn 'CreateAccountRequest' crates/ 2>/dev/null
```

Expected hits: only `crates/ox-types/src/settings.rs` (the type definition + its test). If anything else surfaces, investigate before deleting.

- [ ] **Step 2: Delete the type + its test**

In `crates/ox-types/src/settings.rs`, delete:
- The `pub struct CreateAccountRequest { ... }` definition (around lines 115-124, including the doc comment).
- Any test in the same file that references `CreateAccountRequest` (e.g., `create_account_request_roundtrip` around line 304-320).

- [ ] **Step 3: Build**

```
cargo build --workspace
```

Expected: PASS. If anything fails to compile, a residual reference remains; grep again.

### Sub-task 1.3: Remove the convergence note + CreateAccountRequest references in framework docs

**Files:**
- Modify: `docs/ui_framework.md`
- Modify: `docs/ui_framework/reference.md`

- [ ] **Step 1: Remove the convergence note from `ui_framework.md`**

Delete the entire `## Convergence note` section (around lines 139-148):

```markdown
## Convergence note

The framework's day-one implementation predates these commitments and
contains transitional shapes that the codebase is converging away
from: `_create_now` / `delete_now` sentinel paths in `config/gate/…`,
synthetic `_new` / `_delete` cursor scopes, and `RowKind::AccountAdd`
/ `ModelEmptyState` / `ModelAddManual` rows in the visible-rows
projection. New work should target the architecture this doc
describes; existing surface is being migrated. Where this doc and
the code disagree, the doc is the target.
```

After deletion, the section between "## Six invariants you must keep" and "## Branch / SHA" reads cleanly. No replacement section needed — the convergence is complete.

- [ ] **Step 2: Remove `CreateAccountRequest` from `reference.md`**

In `docs/ui_framework/reference.md`, find and remove:
- The `pub struct CreateAccountRequest { pub name: String }` line (around line 342, in the Settings UI records subsection).
- The `# CreateAccountRequest` mention in the file-map subsection (around line 602).

After removal, the surrounding doc text should still flow cleanly. If the surrounding context references the type, rephrase to omit it.

### Sub-task 1.4: Build, test, commit

- [ ] **Step 1: Build**

```
cargo build --workspace
```

Expected: PASS.

- [ ] **Step 2: Run tests + clippy**

```
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: PASS.

- [ ] **Step 3: Verify cleanups**

```
grep -rn 'starts_with(.\?_.\?)' crates/ 2>/dev/null
grep -rn 'CreateAccountRequest' crates/ 2>/dev/null
grep -n 'Convergence note' docs/ui_framework.md
```

Expected: zero hits for each. (The first grep may surface unrelated `starts_with('_')` checks elsewhere — verify they're not related to the retired account-name rule.)

- [ ] **Step 4: Commit**

```
git add -u
git commit -m "chore(settings): retire underscore-prefix rule + CreateAccountRequest

The substrate convergence (Phases 0-6) removed every synthetic
display path and every sentinel that the underscore-prefix
banner-error rule was protecting against. With nothing to
collide with, the rule is vestigial. Drop the banner-error branch
in accounts.compose.commit; pin the new behavior with a positive
regression test (_-prefixed names create accounts cleanly now).

CreateAccountRequest was the wire payload for the deleted
AccountCreateSubscription. Zero production callers remain. Drop
the type and its roundtrip test.

Remove the convergence note from ui_framework.md — the docs and
the code now agree. The substrate convergence is complete."
```

---

## Task 2: Final verification

- [ ] **Step 1: Workspace tests**

```
cargo test --workspace
```

Expected: PASS.

- [ ] **Step 2: Clippy**

```
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: PASS.

- [ ] **Step 3: Verify the substrate-convergence end-state**

The end-state checklist from the spec's §1 + §2 + §4:

```
# 1. Every path under config/ names a real fact about the world or a
# per-instance async-action trigger.
grep -rn 'config/gate/accounts/_' crates/ 2>/dev/null | grep -v 'docs/'
```

Expected: zero hits in source code. The only `_`-prefixed paths under `config/gate/accounts` should be in test invariants asserting absence (e.g., "no _create_now write" comments, which describe the absence of the deleted sentinel).

```
# 2. Every path under ui/ names a real entity or a UI-state value with
# semantic meaning. No synthetic-affordance identifier paths.
grep -rn 'settings/accounts/_\|settings/models/.*/_' crates/ 2>/dev/null | grep -v test
```

Expected: zero hits.

```
# 3. RowKind has no synthetic variants.
grep -A1 'pub enum RowKind' crates/ox-cli/src/settings/visible_rows.rs
```

Expected: variant list contains only `Entry`, `Account`, `Model`, `AccountField`, `ModelField`. No `AccountAdd`, `ModelEmptyState`, `ModelAddManual`.

```
# 4. UI modes live at typed UI-state paths.
grep -rn 'ui/settings/new_account/buffer\|ui/settings/pending_delete\|ui/settings/manual_model/account' crates/ox-cli/src/ 2>/dev/null | head -10
```

Expected: hits in production code (renderer reads, dispatcher reads, command writes). The substrate is using these paths.

```
# 5. Subscriptions are reactive observers or async-only action triggers.
ls crates/ox-gate/src/subscriptions/ 2>/dev/null
```

Expected: `account_test.rs`, `catalog_refresh.rs`, `account_delete.rs` (the renamed cleanup), `config_save.rs`, `mod.rs`, `util.rs`. No `account_create.rs`.

- [ ] **Step 4: Smoke-test in the TUI (final pass)**

Ask the user to drive the entire create-edit-delete-refresh cycle to confirm everything still works:

1. Open settings.
2. Press `a`. Compose a name (e.g., `personal`). Press Enter. Account row appears, expanded.
3. Press `j` to navigate to the API key field. Press Enter to begin editing. Type a key. Press Enter to commit.
4. Press `r` to refresh the catalog. Wait for the catalog to populate.
5. Navigate to the Models entry, expand it, find your account's model rows.
6. Press `m` while focused near a model row. Compose a manual model entry (id, ctx, out). Press Enter through each stage.
7. Verify the manual model appears in the catalog.
8. Navigate back to the account, press `d`. Inline confirmation banner appears at the top.
9. Press `y`. Account, key, models all gone.

Confirm: no errors, no orphan UI states, no unexpected behavior.

If anything misbehaves, investigate. Otherwise: **the substrate convergence is complete**.

---

## Self-review checklist

- [x] `_`-prefix banner-error rule dropped from `accounts.compose.commit` (Sub-task 1.1).
- [x] `CreateAccountRequest` type and its test deleted (Sub-task 1.2).
- [x] Convergence note removed from `ui_framework.md` (Sub-task 1.3).
- [x] `CreateAccountRequest` references removed from `reference.md` (Sub-task 1.3).
- [x] Workspace green + clippy clean + grep clean (Task 2).
