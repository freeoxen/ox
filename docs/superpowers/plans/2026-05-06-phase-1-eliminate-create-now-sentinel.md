# Phase 1: Eliminate `_create_now` and `AccountCreateSubscription` — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move the new-account materialization out of the `AccountCreateSubscription` RPC pattern and into a direct write from the CLI's `edit.commit` AccountAdd arm. The CLI writes the `AccountConfig` to `config/gate/accounts/<name>` directly, plus the same UI cascade the subscription used to do; the subscription is deleted with no replacement.

**Architecture:** The CLI's commit handler does the validation locally (PathComponent::try_new + the existing `_`-prefix check), constructs an `AccountConfig::default()`, reads the existing `expanded` set, and returns the same six writes the subscription used to return — except the `_create_now` null-write goes away (no sentinel to clear). The `AccountCreateSubscription` file is deleted, its registration removed, its tests gone. User-visible behavior is unchanged: the create flow becomes fully synchronous in the CLI.

**Tech Stack:** Rust workspace; `ox-cli` (the commit arm), `ox-gate` (the deleted subscription).

**Spec:** [docs/superpowers/specs/2026-05-06-settings-substrate-convergence-design.md](../specs/2026-05-06-settings-substrate-convergence-design.md) §5 Phase 1.

---

## File Map

**Modify:**
- `crates/ox-cli/src/settings/commands/edit.rs` — rewrite the `Some(RowKind::AccountAdd) => { ... }` arm of `commit`. Move the cascade logic from the deleted subscription into here. Update tests.
- `crates/ox-gate/src/subscriptions/mod.rs` — drop `pub mod account_create;`, drop the `register_all` call, drop the doc-comment reference at line 33.
- `crates/ox-gate/src/subscriptions/util.rs` — drop `account_create.rs` from the doc comment at line 12.
- `crates/ox-cli/tests/settings_e2e.rs` — `add_account_create_flow` no longer needs to poll for materialization (CLI's writes are synchronous now). Update assertions accordingly.

**Delete:**
- `crates/ox-gate/src/subscriptions/account_create.rs` (entire file, including its `#[cfg(test)] mod tests`).

---

## Conventions used in this plan

- All `cargo` commands run synchronously. Do not background.
- Commits are NEW commits (no `--amend`). No `Co-Authored-By` trailer.
- No phase / PR / spec-section comments in code. Doc comments explaining WHY are fine.
- TDD where new behavior is being added; for deletions, the existing tests' disappearance is the verification.

---

## Task 1: Move cascade logic into `edit.commit`'s AccountAdd arm

**Files:**
- Modify: `crates/ox-cli/src/settings/commands/edit.rs` — the `Some(RowKind::AccountAdd) => { ... }` arm in `commit` (currently around lines 408–429), plus the matching tests in the file's `#[cfg(test)] mod tests`.

The current arm writes a `CreateAccountRequest` to `config/gate/accounts/_create_now`; the subscription handles materialization. After this task, the arm does the materialization itself: validates locally, builds `AccountConfig::default()`, reads `expanded`, returns the full cascade write set.

- [ ] **Step 1: Update the existing `commit_account_add_writes_create_request_and_clears_state` test to assert the new write set**

In `crates/ox-cli/src/settings/commands/edit.rs`'s test module (search for `fn commit_account_add_writes_create_request_and_clears_state`), replace the body's assertions with the new expected shape. The test name should also change to reflect what it now exercises — rename to `commit_account_add_writes_account_record_and_cascade`. Setup is the same (seed `edit_buffer = "alpha"`); the assertions change:

```rust
#[test]
fn commit_account_add_writes_account_record_and_cascade() {
    use ox_gate::AccountConfig;
    let mut snap = SettingsSnapshot::empty();
    snap.insert(
        &oxpath!("settings", "index", "entries", "accounts"),
        to_value(&SettingsIndexEntry {
            id: "accounts".into(),
            label: "Accounts".into(),
            description: String::new(),
            target_cursor: Path::parse("settings/accounts").unwrap(),
            badge: BadgeSource::None,
        })
        .unwrap(),
    );
    snap.insert(
        &oxpath!("ui", "settings", "expanded"),
        expanded_set_to_value(&["settings/accounts".to_string()]),
    );
    snap.insert(&oxpath!("ui", "settings", "edit_mode"), Value::Bool(true));
    snap.insert(
        &oxpath!("ui", "settings", "edit_field_path"),
        path_to_value(&oxpath!("settings", "accounts", "_new")),
    );
    snap.insert(
        &oxpath!("ui", "settings", "edit_buffer"),
        Value::String("alpha".into()),
    );

    let writes = run(&Commit::new(), &mut snap);
    let by_path: std::collections::BTreeMap<_, _> = writes
        .iter()
        .map(|w| (w.path.to_string(), w.record.clone()))
        .collect();

    // 1. Account record materialized at config/gate/accounts/alpha.
    let acct = by_path
        .get("config/gate/accounts/alpha")
        .expect("account record write");
    let cfg: AccountConfig = match acct {
        Record::Parsed(v) => structfs_serde_store::from_value(v.clone()).unwrap(),
        other => panic!("unexpected record: {other:?}"),
    };
    assert_eq!(cfg.provider, "anthropic");

    // 2. Selection.
    let sel = by_path
        .get("ui/settings/accounts/selected")
        .expect("selected write");
    let selected: Option<String> = match sel {
        Record::Parsed(v) => structfs_serde_store::from_value(v.clone()).unwrap(),
        other => panic!("unexpected: {other:?}"),
    };
    assert_eq!(selected.as_deref(), Some("alpha"));

    // 3. Cursor → settings/index.
    let cur = by_path.get("ui/settings/cursor").expect("cursor write");
    match cur {
        Record::Parsed(Value::Array(segs)) => {
            let parts: Vec<String> = segs
                .iter()
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    _ => panic!(),
                })
                .collect();
            assert_eq!(parts.join("/"), "settings/index");
        }
        other => panic!("unexpected: {other:?}"),
    }

    // 4. focused → settings/accounts/alpha.
    let focused = by_path
        .get("ui/settings/focused")
        .expect("focused write");
    match focused {
        Record::Parsed(Value::Array(segs)) => {
            let parts: Vec<String> = segs
                .iter()
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    _ => panic!(),
                })
                .collect();
            assert_eq!(parts.join("/"), "settings/accounts/alpha");
        }
        other => panic!("unexpected: {other:?}"),
    }

    // 5. expanded set contains both settings/accounts and settings/accounts/alpha.
    let exp = by_path
        .get("ui/settings/expanded")
        .expect("expanded write");
    let set: Vec<String> = match exp {
        Record::Parsed(v) => structfs_serde_store::from_value(v.clone()).unwrap(),
        other => panic!("unexpected: {other:?}"),
    };
    assert!(
        set.iter().any(|s| s == "settings/accounts"),
        "expanded must include settings/accounts; got {set:?}"
    );
    assert!(
        set.iter().any(|s| s == "settings/accounts/alpha"),
        "expanded must include settings/accounts/alpha; got {set:?}"
    );

    // 6. NO _create_now write (the sentinel is gone).
    assert!(
        !by_path.contains_key("config/gate/accounts/_create_now"),
        "_create_now sentinel must not be written; got writes: {writes:?}"
    );

    // 7. Edit state cleared.
    assert!(matches!(
        by_path.get("ui/settings/edit_mode").unwrap(),
        Record::Parsed(Value::Bool(false))
    ));
    assert!(matches!(
        by_path.get("ui/settings/edit_buffer").unwrap(),
        Record::Parsed(Value::Null)
    ));
    assert!(matches!(
        by_path.get("ui/settings/edit_field_path").unwrap(),
        Record::Parsed(Value::Null)
    ));
}
```

(If `BadgeSource`, `SettingsIndexEntry`, or `expanded_set_to_value` aren't yet imported in the test module, add the imports as needed — the existing tests in this file already use them, so the imports are likely present.)

- [ ] **Step 2: Add a new test for invalid PathComponent name**

The CLI now does PathComponent validation (previously the subscription did). Pin the banner-error behavior:

```rust
#[test]
fn commit_account_add_with_invalid_name_emits_banner_keeps_edit_mode_open() {
    let mut snap = SettingsSnapshot::empty();
    snap.insert(
        &oxpath!("settings", "index", "entries", "accounts"),
        to_value(&SettingsIndexEntry {
            id: "accounts".into(),
            label: "Accounts".into(),
            description: String::new(),
            target_cursor: Path::parse("settings/accounts").unwrap(),
            badge: BadgeSource::None,
        })
        .unwrap(),
    );
    snap.insert(
        &oxpath!("ui", "settings", "expanded"),
        expanded_set_to_value(&["settings/accounts".to_string()]),
    );
    snap.insert(&oxpath!("ui", "settings", "edit_mode"), Value::Bool(true));
    snap.insert(
        &oxpath!("ui", "settings", "edit_field_path"),
        path_to_value(&oxpath!("settings", "accounts", "_new")),
    );
    snap.insert(
        &oxpath!("ui", "settings", "edit_buffer"),
        Value::String("bad-name".into()),  // hyphen rejected by PathComponent::try_new
    );

    let writes = run(&Commit::new(), &mut snap);
    // Exactly one write: the banner. No account record, no UI cascade,
    // no clear_edit_state — edit mode stays open so the user can fix.
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].path, oxpath!("ui", "global", "banner"));
    let banner: ox_types::settings::GlobalBanner = match &writes[0].record {
        Record::Parsed(v) => structfs_serde_store::from_value(v.clone()).unwrap(),
        other => panic!("unexpected record: {other:?}"),
    };
    match banner {
        ox_types::settings::GlobalBanner::Error { message, .. } => {
            assert!(
                message.contains("Invalid"),
                "banner must mention the rule; got {message:?}"
            );
            assert!(
                message.contains("bad-name"),
                "banner must mention the offending name; got {message:?}"
            );
        }
        other => panic!("expected Error banner, got {other:?}"),
    }
}
```

- [ ] **Step 3: Run the tests; expect FAIL**

```
cargo test -p ox-cli --lib settings::commands::edit::tests::commit_account_add_writes_account_record_and_cascade
cargo test -p ox-cli --lib settings::commands::edit::tests::commit_account_add_with_invalid_name_emits_banner_keeps_edit_mode_open
```

Expected: both FAIL.
- The first one fails because the current arm writes a `CreateAccountRequest` to `_create_now`, not an `AccountConfig` to `config/gate/accounts/alpha`.
- The second one fails because the current arm doesn't do PathComponent validation — it forwards the buffer to the subscription, which would emit the banner; but Run runs the command synchronously and there's no subscription in scope, so the test sees only the `_create_now` write.

- [ ] **Step 4: Rewrite the `Some(RowKind::AccountAdd) => { ... }` arm**

In `crates/ox-cli/src/settings/commands/edit.rs`'s `commit` function, replace the AccountAdd arm body with:

```rust
Some(RowKind::AccountAdd) => {
    use ox_gate::AccountConfig;
    use ox_kernel::PathComponent;

    let trimmed = buffer.trim();
    // Empty/whitespace: silent no-op so edit mode stays open.
    if trimmed.is_empty() {
        return Vec::new();
    }
    // `_`-prefix: kept as a transitional rule. After Phase 3 lifts
    // the inline-create flow into mode state and there's no
    // synthetic ghost-row path to collide with, this rule and its
    // banner go away (Phase 7 cleanup).
    if trimmed.starts_with('_') {
        return vec![banner_error(format!(
            "Account name '{}' starts with '_', which is reserved. Try a name without the leading underscore.",
            trimmed
        ))];
    }
    // Validate locally: any name we'd write to
    // `config/gate/accounts/<name>` must be a real PathComponent.
    let comp = match PathComponent::try_new(trimmed.to_string()) {
        Ok(c) => c,
        Err(_) => {
            return vec![banner_error(format!(
                "Invalid account name: '{}'",
                trimmed
            ))];
        }
    };

    // Materialize a default AccountConfig at the canonical path.
    let cfg = AccountConfig {
        provider: "anthropic".to_string(),
    };

    // UI cascade — same shape AccountCreateSubscription used to
    // produce. Cursor stays at settings/index (the accordion); the
    // new account is surfaced via focused + expansion so its field
    // rows are immediately visible in place.
    let new_account_row = oxpath!("settings", "accounts", comp.clone());
    let mut expanded: Vec<String> = super::super::renderers::util::read_typed(
        data,
        &oxpath!("ui", "settings", "expanded"),
    )
    .unwrap_or_default();
    let accounts_key = "settings/accounts".to_string();
    let new_row_key = format!("settings/accounts/{}", trimmed);
    if !expanded.iter().any(|s| s == &accounts_key) {
        expanded.push(accounts_key);
    }
    if !expanded.iter().any(|s| s == &new_row_key) {
        expanded.push(new_row_key);
    }

    let acct_path = oxpath!("config", "gate", "accounts", comp);
    let cfg_value = match to_value(&cfg) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let selected_value = match to_value(&Some(trimmed.to_string())) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };
    let expanded_value = match to_value(&expanded) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    vec![
        Write {
            path: acct_path,
            record: Record::parsed(cfg_value),
        },
        Write {
            path: oxpath!("ui", "settings", "accounts", "selected"),
            record: Record::parsed(selected_value),
        },
        Write {
            path: oxpath!("ui", "settings", "cursor"),
            record: Record::parsed(path_to_value(&oxpath!("settings", "index"))),
        },
        Write {
            path: oxpath!("ui", "settings", "focused"),
            record: Record::parsed(path_to_value(&new_account_row)),
        },
        Write {
            path: oxpath!("ui", "settings", "expanded"),
            record: Record::parsed(expanded_value),
        },
    ]
}
```

(Imports: this file already imports `oxpath!`, `Write`, `Record`, `Value`, `to_value`. Add `use ox_gate::AccountConfig;` and `use ox_kernel::PathComponent;` at the top of the file if they aren't already there. The `path_to_value` reference is `super::navigation::path_to_value` — check the file's imports; if it's already in scope as `path_to_value`, use that name.)

- [ ] **Step 5: Run the new tests; expect PASS**

```
cargo test -p ox-cli --lib settings::commands::edit::tests::commit_account_add_writes_account_record_and_cascade
cargo test -p ox-cli --lib settings::commands::edit::tests::commit_account_add_with_invalid_name_emits_banner_keeps_edit_mode_open
```

Expected: both PASS.

- [ ] **Step 6: Run the rest of the edit::tests module**

```
cargo test -p ox-cli --lib settings::commands::edit::tests
```

Expected: PASS. The other AccountAdd tests (`commit_account_add_with_empty_buffer_keeps_edit_mode_open`, `commit_account_add_with_underscore_prefix_emits_banner_keeps_edit_mode_open`, `commit_account_add_with_interior_underscore_writes_create_request`) need updating too:

- The empty-buffer test still passes — the early `return Vec::new()` for empty/whitespace is unchanged.
- The underscore-prefix test still passes — the same banner-error path, same message.
- `commit_account_add_with_interior_underscore_writes_create_request` is now misnamed (no `_create_now` write happens). Rename to `commit_account_add_with_interior_underscore_writes_account_record` and update its assertion to look for `config/gate/accounts/alpha_beta` instead of `config/gate/accounts/_create_now`. Mirror the assertions from Step 1's test (account record, selected, cursor, focused, expanded, no _create_now, edit state cleared) — abbreviate where appropriate but at least pin the account record and the no-_create_now invariant.

- [ ] **Step 7: Run the full ox-cli lib tests**

```
cargo test -p ox-cli --lib
```

Expected: PASS. The change is local to edit.rs's commit arm; no other lib tests should break.

- [ ] **Step 8: Commit**

```
git add crates/ox-cli/src/settings/commands/edit.rs
git commit -m "feat(settings): edit.commit AccountAdd writes the account directly

The arm now validates the buffer locally (PathComponent::try_new),
constructs AccountConfig::default(), reads the existing expanded
set, and returns the full UI cascade — same writes the
AccountCreateSubscription used to produce, minus the _create_now
null-write (no sentinel to clear). The create flow is now fully
synchronous in the CLI; no broker round-trip via subscription.

Tests updated: the create-request test renamed and rewritten to
assert the materialized AccountConfig + cascade. New invalid-name
test pins the banner-error path the CLI now owns."
```

---

## Task 2: Delete `AccountCreateSubscription`

**Files:**
- Delete: `crates/ox-gate/src/subscriptions/account_create.rs` (the entire file).
- Modify: `crates/ox-gate/src/subscriptions/mod.rs` — drop the `pub mod account_create;` declaration, drop the registration in `register_all`, drop the doc-comment line referencing it.
- Modify: `crates/ox-gate/src/subscriptions/util.rs` — drop `account_create.rs` from the doc comment listing the consumers.

- [ ] **Step 1: Delete the subscription file**

```
git rm crates/ox-gate/src/subscriptions/account_create.rs
```

This removes the file and its `#[cfg(test)] mod tests` block. The tests it contained (e.g.
`create_writes_default_config_selection_focus_and_expansion`,
`create_rejects_invalid_name_with_banner`,
`create_rejects_underscore_prefix_with_banner`,
`create_inert_when_after_record_is_missing`) are deleted — the
behavior they pinned now lives in `edit.commit`'s AccountAdd arm
and is covered by Task 1's tests.

- [ ] **Step 2: Drop the module declaration + registration**

In `crates/ox-gate/src/subscriptions/mod.rs`:

- Remove the line `pub mod account_create;` (around line 10).
- Remove the registration line in `register_all`:
  `broker.register_subscription(Arc::new(account_create::AccountCreateSubscription::new()));`
  (around line 48).
- Update the doc comment listing the registration order (around lines 27–34): drop the `4. account_create — exact trigger, fires on …/_create_now` line and renumber the remaining items if the list is numbered.

- [ ] **Step 3: Drop the util.rs doc-comment reference**

In `crates/ox-gate/src/subscriptions/util.rs`, the module-level doc comment (around line 12) lists the subscriptions that consume the helpers. Remove `account_create.rs` from that list.

Before:
```rust
//! Centralizing these here keeps `account_test.rs`, `catalog_refresh.rs`,
//! `account_delete.rs`, `account_create.rs`, and `config_save.rs`
//! focused on the subscription's domain logic instead of path plumbing.
```

After:
```rust
//! Centralizing these here keeps `account_test.rs`, `catalog_refresh.rs`,
//! `account_delete.rs`, and `config_save.rs` focused on the
//! subscription's domain logic instead of path plumbing.
```

- [ ] **Step 4: Verify the workspace compiles**

```
cargo build -p ox-gate
cargo build -p ox-cli
```

Expected: both PASS. If anything fails to compile, it means a stale reference somewhere imports from `ox_gate::subscriptions::account_create` or `AccountCreateSubscription`. Find with:

```
grep -rn 'account_create\|AccountCreate' crates/ tests/ 2>/dev/null
```

Expected after the edits in Steps 1–3: zero hits in source code (excluding doc comments / unrelated identifiers). The `ox-wasm` link failure is pre-existing and unrelated.

- [ ] **Step 5: Run the gate test suite**

```
cargo test -p ox-gate --lib
```

Expected: PASS. The deleted tests don't appear; the rest are unaffected. Test count should drop by exactly the number of tests that lived in `account_create.rs` (4 expected, per the file's prior contents).

- [ ] **Step 6: Run the ox-cli lib + e2e tests**

```
cargo test -p ox-cli --lib
cargo test -p ox-cli --test settings_e2e
```

Expected: PASS for ox-cli --lib. The e2e test `add_account_create_flow` may or may not pass at this point depending on whether it polled for materialization through the subscription — Task 3 addresses that.

If `add_account_create_flow` fails here, the failure is expected — Task 3 fixes it. Don't try to fix it in Task 2; just note the failure mode and proceed.

- [ ] **Step 7: Commit**

```
git add -u
git commit -m "chore(gate): delete AccountCreateSubscription

The subscription was an RPC-translation layer between the CLI's
_create_now write and the actual AccountConfig materialization. The
materialization now lives in the CLI's edit.commit AccountAdd arm
(prior commit), so the subscription is dead code. Drop the file,
the registration, and the four tests that pinned its behavior —
those properties are now under test in edit.rs."
```

---

## Task 3: Update `add_account_create_flow` e2e test

**Files:**
- Modify: `crates/ox-cli/tests/settings_e2e.rs` — the `add_account_create_flow` test function.

The test currently drives the modal-era entry path (writes cursor=`settings/accounts/_new` + writes `name_input` directly, then dispatches `Enter`), and then polls for the materialized account through the subscription's cascade. After Phase 1, the CLI's writes are synchronous — no polling needed.

**Important:** the test still uses the modal-era *entry* path (cursor=_new + name_input). Phase 3 will change that. For Phase 1, leave the entry path alone; only update the post-Enter assertions to reflect synchronous materialization.

Wait — actually, the modal-era entry path doesn't work post-Phase 1 either. The CLI's `accounts.create` command (which the modal era's Enter binding fired) was deleted in the inline-new-connection branch. The CLI now expects the AccountAdd commit path (mode-state buffer, then Enter routes through edit.commit). The test's setup needs review.

- [ ] **Step 1: Read the current `add_account_create_flow`**

```
grep -n 'fn add_account_create_flow' crates/ox-cli/tests/settings_e2e.rs
```

Read the test in full. Note its setup, dispatch sequence, and assertions. Its current shape after the inline-new-connection branch was:

1. Set cursor=`settings/accounts/_new`.
2. Write `ui/settings/new_account/name_input = "anthropic_personal"` directly.
3. Dispatch `Enter`.
4. Poll for materialization at `config/gate/accounts/anthropic_personal`.
5. Assert cursor=`settings/index` + focused=`settings/accounts/anthropic_personal` (post-5cb1117 patch).
6. Assert `selected = Some("anthropic_personal")`.

After Phase 1, the broker has no `AccountCreateSubscription` to materialize the account. The Enter binding at `_new` cursor scope was already deleted in the inline branch (Task 9). So step 3's dispatch is unhandled — nothing happens.

This test needs to be rewritten for the inline mode-state entry path **OR** to drive the AccountAdd commit directly through `accounts.add → typing → Enter`. The latter is closer to what real users do.

- [ ] **Step 2: Rewrite the test to use the inline ghost-row entry path**

Replace `add_account_create_flow` with:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn add_account_create_flow() {
    let h = E2eHarness::new().await;
    populate_index(&h).await;

    // Land cursor at settings/index and focused under settings/accounts so
    // the `a` binding (Prefix(settings/accounts)) resolves to accounts.add.
    h.write_path(
        &oxpath!("ui", "settings", "cursor"),
        &oxpath!("settings", "index"),
    )
    .await;
    h.write_path(
        &oxpath!("ui", "settings", "focused"),
        &oxpath!("settings", "accounts"),
    )
    .await;

    // `a` opens inline edit on the AccountAdd ghost row. Type the name,
    // press Enter to commit. The CLI's edit.commit writes the
    // AccountConfig directly — no subscription cascade, no polling.
    assert!(matches!(h.dispatch("a").await, KeyDispatchOutcome::Handled));
    for ch in "anthropic_personal".chars() {
        let key = ch.to_string();
        assert!(
            matches!(h.dispatch(&key).await, KeyDispatchOutcome::Handled),
            "dispatch returned Unhandled for {key:?}"
        );
    }
    assert!(matches!(h.dispatch("Enter").await, KeyDispatchOutcome::Handled));

    // The CLI's writes are synchronous — no polling needed.
    let comp = ox_kernel::PathComponent::try_new("anthropic_personal").unwrap();
    let account: AccountConfig = h
        .client
        .read_typed(&oxpath!("config", "gate", "accounts", comp))
        .await
        .expect("read account record")
        .expect("account record present");
    assert_eq!(account.provider, "anthropic");

    // Cursor settled at settings/index, focused at the new account.
    let cursor = h.current_cursor().await.expect("cursor present");
    assert_eq!(cursor, oxpath!("settings", "index"));
    let focused = h.focused().await.expect("focused present");
    assert_eq!(
        focused,
        oxpath!(
            "settings",
            "accounts",
            ox_kernel::PathComponent::try_new("anthropic_personal").unwrap()
        )
    );

    let selected: Option<String> = h
        .client
        .read_typed(&oxpath!("ui", "settings", "accounts", "selected"))
        .await
        .expect("read selected")
        .flatten();
    assert_eq!(selected.as_deref(), Some("anthropic_personal"));
}
```

Drop any `poll_until` invocation that was waiting for the subscription cascade. The CLI's writes happen during `dispatch("Enter").await` — they're observable immediately after.

If the test referenced helpers (`poll_until`, `populate_index`, `h.write_path`, `h.current_cursor`, `h.focused`) that don't exist, check the harness file (around `crates/ox-cli/tests/settings_e2e.rs` near the top); they should all be there from prior tests. If something is missing, lift the helper from a sibling test in the same file.

- [ ] **Step 3: Run the e2e test**

```
cargo test -p ox-cli --test settings_e2e add_account_create_flow
```

Expected: PASS.

- [ ] **Step 4: Run the full e2e suite**

```
cargo test -p ox-cli --test settings_e2e
```

Expected: PASS. No other test depends on the old modal-era entry path.

- [ ] **Step 5: Commit**

```
git add crates/ox-cli/tests/settings_e2e.rs
git commit -m "test(settings/e2e): add_account_create_flow drives the inline path

The test previously drove the modal-era entry (cursor=_new +
direct name_input write + Enter), which both relied on the
deleted accounts.create command AND polled for the subscription
cascade. After Phase 1, neither exists.

Rewrites the test to drive the inline ghost-row path: 'a' opens
edit mode on the AccountAdd ghost, characters route through
edit.insert_char, Enter routes through edit.commit. The CLI's
writes are synchronous; the post-Enter reads are immediate."
```

---

## Task 4: Final verification

- [ ] **Step 1: Full workspace test run**

```
cargo test --workspace
```

Expected: PASS — every crate's tests are green. Test count should drop by exactly 4 (the deleted `AccountCreateSubscription` tests).

- [ ] **Step 2: Clippy on every target**

```
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: PASS — no warnings.

- [ ] **Step 3: Verify no stragglers**

```
grep -rn 'AccountCreateSubscription\|_create_now\|account_create' crates/ tests/ 2>/dev/null
```

Expected:
- Zero hits for `AccountCreateSubscription`.
- Zero hits for `_create_now` (the path is gone — no read or write sites).
- Zero hits for `account_create` (the module name is gone too).

If any hits remain in source, something was missed. Acceptable hits in `docs/` (the spec, this plan, the framework docs) — those reference the historical name in context.

- [ ] **Step 4: Smoke-test in the TUI**

The harness can't run the interactive TUI. Ask the user to:

1. Open settings.
2. Press `a` to enter compose mode on the AccountAdd ghost row.
3. Type a name (e.g. `personal`), press Enter.
4. Confirm: the new account row appears expanded with field rows; `focused` is on the new row; no error banner.
5. Press `a` again, type a name with a hyphen (e.g. `bad-name`), press Enter.
6. Confirm: error banner appears ("Invalid account name: 'bad-name'"); edit mode stays open; the bad name is still in the buffer.
7. Press `a`, type `_underscore`, press Enter.
8. Confirm: error banner about reserved underscore prefix.
9. Press `a`, press Esc without typing.
10. Confirm: edit mode dismisses cleanly; no error.

If anything misbehaves, it's a regression — investigate before declaring Phase 1 complete.

---

## Self-review checklist

- [x] CLI's `edit.commit` AccountAdd arm validates locally and writes the AccountConfig + UI cascade directly (Task 1).
- [x] PathComponent::try_new check + banner-error landed in the CLI (Task 1, Step 2 test).
- [x] `AccountCreateSubscription` deleted with no replacement (Task 2).
- [x] E2E test rewired to the inline path with no subscription polling (Task 3).
- [x] Workspace green + clippy clean + no stragglers (Task 4).

Spec requirements not addressed by this plan (intentionally):
- `CatalogFetchOnCreateSubscription` — dropped from the spec in commit `eb1cd0d`. No auto-fetch on create; catalog refresh stays user-triggered.
- `CreateAccountRequest` type cleanup — the type stays for now. Phase 7 cleans it up if no callers remain after later phases land.
