# Phase 6: Lift model empty-state into renderer decoration — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Drop the last synthetic `RowKind` variant — `ModelEmptyState` — from the visible-rows projection. The renderer reads the data tree to identify empty-catalog accounts and emits decoration ListItems (empty-state line + manual-model affordance) at the right position in the rendered list. After Phase 6, `visible_rows::enumerate` produces only real-thing rows; every synthetic display element is a renderer decoration with `focus: None`.

**Architecture:** Single commit. No two-commit dance — the change is contained: the `append_model_rows` projection stops emitting `ModelEmptyState` rows, and the renderer's existing decoration logic (which already iterates `ModelEmptyState` rows post-Phase 5) is replaced by an iteration over `child_names_under("config/gate/accounts")` that finds empty-catalog accounts and inserts decorations at the right alphabetical position. The user-visible behavior change: `j`/`k` no longer lands on the empty-state line (it's `focus: None` decoration); the `r` key already bound at `Prefix(settings/models)` continues to refresh the focused account's catalog.

**Tech Stack:** Rust workspace; `ox-cli` (visible_rows, renderer, tests).

**Spec:** [docs/superpowers/specs/2026-05-06-settings-substrate-convergence-design.md](../specs/2026-05-06-settings-substrate-convergence-design.md) §5 Phase 6.

---

## File Map

**Modify:**
- `crates/ox-cli/src/settings/visible_rows.rs` — drop `RowKind::ModelEmptyState` variant + the synthetic-row push in `append_model_rows`. Update the affected tests.
- `crates/ox-cli/src/settings/renderers/index.rs` — replace the iterate-over-`ModelEmptyState`-rows logic with iterate-over-accounts-and-insert-at-alphabetical-position logic. Drop `RowKind::ModelEmptyState` from any `decorate_row_label` arm.
- `crates/ox-cli/src/settings/commands/tree.rs` — drop the `RowKind::ModelEmptyState` arm in `activate` (currently fires the refresh trigger; replaced by `r` keybinding).
- `crates/ox-cli/src/settings/bindings.rs` — bind `r` at `Prefix(settings/accounts)` in addition to the existing `Prefix(settings/models)` binding, so refresh works from both sections (otherwise empty-catalog accounts become unreachable for refresh once their Models-section row stops being focusable).

---

## Conventions used in this plan

- All `cargo` commands run synchronously. Do not background.
- Commits are NEW commits (no `--amend`). No `Co-Authored-By` trailer.
- No phase / PR / spec-section / Task-N comments in code.

---

## Task 1: Atomic substrate switch

### Sub-task 1.1: Drop `RowKind::ModelEmptyState`

**File:**
- Modify: `crates/ox-cli/src/settings/visible_rows.rs`

- [ ] **Step 1: Remove the variant**

In the `RowKind` enum (around line 32), delete:
```rust
ModelEmptyState { account: String },
```

The compiler will surface every match site that handles it.

- [ ] **Step 2: Remove the synthetic-row push**

In `append_model_rows` (around lines 320-345), delete the push of the `ModelEmptyState` row:

```rust
if models.is_empty() {
    let path = row_path(&[
        "settings",
        "models",
        &safe_component(account_name),
        "_empty",
    ]);
    rows.push(VisibleRow {
        path,
        depth: 1,
        label: format!("{} / (no models — Enter to refresh)", account_name),
        secondary: None,
        badge: None,
        kind: RowKind::ModelEmptyState {
            account: account_name.clone(),
        },
        expandable: false,
        expanded: false,
    });
    continue;
}
```

After deletion, the `if models.is_empty() { continue; }` shape stays — empty-catalog accounts contribute no rows to the visible-rows projection. The renderer takes over.

- [ ] **Step 3: Update tests in visible_rows**

Two tests assert on `ModelEmptyState`:
- `empty_catalog_yields_one_empty_state_row_per_connection` (around line 1147)
- `empty_catalog_row_has_unique_path_per_connection` (around line 1180)

Delete both tests. Their semantic is covered by the renderer's tests in the next sub-task.

Also check `expanded_models_inlines_model_pairs` and `model_row_secondary_carries_ctx_and_out_metadata` for hard-coded row counts that included the empty-state rows. Update the expected counts to drop the empty-state contribution.

### Sub-task 1.2: Drop `tree::activate`'s `ModelEmptyState` arm

**File:**
- Modify: `crates/ox-cli/src/settings/commands/tree.rs`

- [ ] **Step 1: Remove the arm**

In `activate`, delete the `RowKind::ModelEmptyState { account }` arm that wrote the refresh trigger. The `r` keybinding (already bound at `Prefix(settings/models)` to `account.refresh`) handles the refresh action when the user is focused anywhere in the Models section. The empty-state line itself is now non-focusable, so the user navigates to a real adjacent row and presses `r`.

- [ ] **Step 2: Update or delete tests**

Any test in tree.rs that exercised the `ModelEmptyState` activation path (e.g., `activate_on_empty_state_row_writes_refresh_trigger` if it exists) should be deleted. The `r`-key activation is covered by existing `account.refresh` tests.

### Sub-task 1.2.1: Bind `r` at `Prefix(settings/accounts)`

**File:**
- Modify: `crates/ox-cli/src/settings/bindings.rs`

After Phase 6 drops the empty-state row, an empty-catalog account has no focusable rows in the Models section. `r` (currently `Prefix(settings/models)` → `account.refresh`) becomes unreachable for that account. Binding `r` at `Prefix(settings/accounts)` lets the user refresh from the Accounts section (where the connection's row IS focusable) — the `account.refresh` command already reads the focused row to determine which account to refresh and works for any row kind that carries an account name.

- [ ] **Step 1: Add the binding**

In `register_row_prefixes` (or wherever `bind_prefix(reg, accounts_subtree, ..., 'a', "accounts.add")` and `bind_prefix(reg, accounts_subtree, ..., 'd', "accounts.delete_confirm")` are registered), add:

```rust
bind_prefix(
    reg,
    accounts_subtree.clone(),
    no_mods(),
    KeyCodeRepr::Char('r'),
    "account.refresh",
);
```

The Models-section binding stays. Both fire `account.refresh`; the command reads the focused row to know which account.

- [ ] **Step 2: Verify `account.refresh` works from any row kind**

Read `crates/ox-cli/src/settings/commands/account_model.rs::account_refresh`. Confirm it extracts the account name from the focused row regardless of whether it's an `Account`, `AccountField`, `Model`, or `ModelField` row. If it ONLY handles Model/ModelField (the historical Models-section context), extend it to handle Account/AccountField too.

The simplest extension: a helper that takes a `&VisibleRow` and returns `Option<String>` for the account name across all row kinds. The existing helper(s) likely already cover this — `read_selected_account` reads `ui/settings/accounts/selected`, but that may not be set when the user is just navigating. Prefer reading the focused row directly.

- [ ] **Step 3: Build + test**

```
cargo build -p ox-cli
cargo test -p ox-cli --lib settings::commands::account_model::tests
```

Expected: PASS.

### Sub-task 1.3: Renderer reads accounts directly and emits decorations

**File:**
- Modify: `crates/ox-cli/src/settings/renderers/index.rs`

The current Phase 5 code iterates the items vector looking for `ModelEmptyState` rows and inserts the manual-model decoration after each. After Phase 6, no `ModelEmptyState` rows exist; the renderer needs to find empty-catalog accounts by reading the data tree.

- [ ] **Step 1: Replace the iteration logic**

Replace the existing post-processing block that iterates ModelEmptyState rows with one that:
1. Reads all account names from `child_names_under("config/gate/accounts")`.
2. For each account in alphabetical order: read its `models: Vec<ModelInfo>`; if empty, find its insertion position in the items vector and insert TWO decoration ListItems (empty-state line + manual-model affordance/form).

The insertion position for an empty-catalog account is "after the last Model row of the alphabetically-previous account" OR "right after the Models entry header" if no earlier accounts have model rows yet.

```rust
// Before this block, the items vector contains the Models entry
// header followed by Model rows for non-empty-catalog accounts.
// Empty-catalog accounts contribute nothing to visible_rows.
//
// Walk the data tree's account list and identify empty-catalog
// accounts. For each one, find its insertion point in the items
// vector and insert decoration ListItems.

let manual_account: Option<String> = read_typed(
    ctx.data,
    &oxpath!("ui", "settings", "manual_model", "account"),
);

// Find the index of the Models entry header in the items vector;
// also find the index of each Model row keyed by account name.
let models_header_idx: Option<usize> = rows
    .iter()
    .position(|r| {
        matches!(&r.kind, RowKind::Entry { entry_id } if entry_id == "models")
    });
let mut last_model_idx_per_account: std::collections::BTreeMap<String, usize> =
    std::collections::BTreeMap::new();
for (i, row) in rows.iter().enumerate() {
    if let RowKind::Model { account, .. } = &row.kind {
        last_model_idx_per_account.insert(account.clone(), i);
    }
}

if let Some(models_idx) = models_header_idx {
    // Only emit decorations when the Models section is expanded.
    let expanded = rows.get(models_idx).map(|r| r.expanded).unwrap_or(false);
    if expanded {
        let account_names = crate::settings::renderers::util::child_names_under(
            ctx.data,
            "config/gate/accounts",
        );
        // Sorted iteration matches the order append_model_rows uses.
        let mut sorted_accounts: Vec<String> = account_names
            .into_iter()
            .filter(|n| ox_kernel::PathComponent::try_new(n).is_ok())
            .collect();
        sorted_accounts.sort();

        // For each empty-catalog account, find the insertion position.
        // Process in REVERSE alphabetical order so insertions don't
        // invalidate the indices we computed for earlier accounts.
        let mut empty_accounts: Vec<String> = sorted_accounts
            .iter()
            .filter(|name| {
                let comp = match ox_kernel::PathComponent::try_new(name.as_str()) {
                    Ok(c) => c,
                    Err(_) => return false,
                };
                let models: Vec<ox_gate::ModelInfo> = read_typed(
                    ctx.data,
                    &oxpath!("config", "gate", "accounts", comp, "models"),
                )
                .unwrap_or_default();
                models.is_empty()
            })
            .cloned()
            .collect();
        empty_accounts.sort();
        empty_accounts.reverse();

        for name in empty_accounts {
            // Insertion point: after the last Model row of the
            // alphabetically-previous account, or right after the
            // Models header if no earlier account has model rows.
            let prev_account = sorted_accounts
                .iter()
                .filter(|n| n.as_str() < name.as_str())
                .filter_map(|n| last_model_idx_per_account.get(n))
                .max()
                .copied();
            let insert_idx = match prev_account {
                Some(idx) => idx + 1,
                None => models_idx + 1,
            };

            // Build the empty-state line.
            let empty_state = ListItem {
                primary: format!("  {} / (no models — press r to refresh)", name),
                primary_spans: None,
                secondary: None,
                badge: None,
                focus: None,
            };

            // Build the manual-model affordance/form line.
            let in_mode_for_this_account = manual_account.as_deref() == Some(name.as_str());
            let manual_primary = if in_mode_for_this_account {
                let stage = read_typed::<ox_types::settings::ManualModelStage>(
                    ctx.data,
                    &oxpath!("ui", "settings", "manual_model", "stage"),
                );
                let buffer: String = read_typed(
                    ctx.data,
                    &oxpath!("ui", "settings", "manual_model", "buffer"),
                )
                .unwrap_or_default();
                let prompt = match stage {
                    Some(ox_types::settings::ManualModelStage::Id) => "Model id",
                    Some(ox_types::settings::ManualModelStage::Ctx) => "Max context",
                    Some(ox_types::settings::ManualModelStage::Out) => "Max output",
                    None => "Model id",
                };
                format!("    {prompt}▸ {buffer}\u{258F}")
            } else {
                "    + add model manually (m)".to_string()
            };
            let manual = ListItem {
                primary: manual_primary,
                primary_spans: None,
                secondary: None,
                badge: None,
                focus: None,
            };

            // Insert in reverse order at the same index so they end
            // up empty_state -> manual.
            items.insert(insert_idx, manual);
            items.insert(insert_idx, empty_state);

            // Bump selected if it pointed at index >= insert_idx.
            selected = selected.map(|s| if s >= insert_idx { s + 2 } else { s });
        }
    }
}
```

This replaces the Phase-5-era post-processing that walked items looking for ModelEmptyState rows. Delete that earlier block when adding this one.

- [ ] **Step 2: Drop `RowKind::ModelEmptyState` from `decorate_row_label` if present**

Search `decorate_row_label` for `ModelEmptyState`. If an arm exists, remove it. (May not exist — the empty-state line wasn't editable, so it likely just had the catch-all early return.)

- [ ] **Step 3: Update or add renderer tests**

Tests asserting on the empty-state line need updating. Tests that exercised activation via `Enter` on the synthetic row are gone (Sub-task 1.2 dropped that). Add or adapt tests pinning:
- An empty-catalog account produces an empty-state ListItem (with `focus: None`) in the rendered output.
- The empty-state line appears at the alphabetically correct position.
- The manual-model affordance appears below the empty-state line.
- `j`/`k` traversal does NOT land on the empty-state ListItem (it's `focus: None`).

### Sub-task 1.4: Build, test, commit

- [ ] **Step 1: Build**

```
cargo build -p ox-cli
```

Expected: PASS. Any compile error names a `RowKind::ModelEmptyState` reference that needs cleaning.

- [ ] **Step 2: Run tests + clippy**

```
cargo test -p ox-cli --lib
cargo test -p ox-cli --test settings_e2e
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: PASS. The e2e test that verifies empty-catalog rendering may need its assertion updated for the new shape (the synthetic row's path is gone; the rendered text is unchanged or close).

- [ ] **Step 3: Commit**

```
git add -u
git commit -m "feat(settings): model empty-state lifts into renderer decoration

Drops RowKind::ModelEmptyState — the last synthetic RowKind variant
in the visible-rows projection. visible_rows::append_model_rows now
contributes nothing for empty-catalog accounts; the renderer reads
the data tree directly, identifies empty-catalog accounts, and
inserts decoration ListItems (empty-state line + manual-model
affordance) at the alphabetically-correct position. tree::activate
sheds its ModelEmptyState arm.

User-visible behavior shift: j/k traversal no longer lands on the
empty-state line (it's now focus: None decoration). The 'r' key,
already bound at Prefix(settings/models) to account.refresh, is
the only way to trigger refresh. The empty-state line's text is
updated to 'press r to refresh' to make the keybinding
discoverable.

After this commit, visible_rows::enumerate produces only
real-thing rows; every synthetic UI element is a renderer-side
decoration. Phase 7 retires the underscore-prefix banner-error
rule and other transitional cruft."
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

- [ ] **Step 3: Verify no stragglers**

```
grep -rn 'RowKind::ModelEmptyState\|ModelEmptyState' crates/ 2>/dev/null
```

Expected: zero hits.

```
grep -rn 'no models — Enter to refresh\|_empty' crates/ 2>/dev/null
```

Expected: zero hits except possibly in test invariants asserting absence.

- [ ] **Step 4: Smoke-test in the TUI**

Ask the user to:

1. Open settings → expand the Models entry.
2. Verify: for each connection with no models, you see the line `<account> / (no models — press r to refresh)` followed by `+ add model manually (m)`. Both should appear visually distinct (decoration styling per existing convention) and at the correct alphabetical position relative to other accounts' model rows.
3. Press `j`/`k` repeatedly. Verify navigation skips both decoration lines — focus only lands on real Model rows and the Models entry header.
4. Press `r` while focused on... a Model row of an empty-catalog account? But empty-catalog accounts have no Model rows. Hmm.

   Actually the user has to press `r` from somewhere in the Models section to trigger refresh on a specific account. `r` is bound at `Prefix(settings/models)` and `account.refresh` reads the focused row's account from the row's kind. With no Model rows for empty-catalog accounts, the user can't focus into the account's section to press `r`.

   Verify this works as expected: focus on an adjacent account's Model row, press `r` — does it refresh the WRONG account? If yes, the user can't refresh empty-catalog accounts at all. This may be a real issue — flag for follow-up.

   Alternative: the user navigates to the connection's row in the Connections (accounts) section, NOT the Models section, and presses `r` from there. Does `r` work at `Prefix(settings/accounts)` too? Check.

5. Press `m` while focused near the empty-catalog account. Verify the inline manual-model form opens for that account.

If anything misbehaves, particularly the `r`-can't-find-empty-catalog-account issue, file a follow-up. The pure substrate refactor for Phase 6 may need a UX patch.

---

## Self-review checklist

- [x] `RowKind::ModelEmptyState` variant dropped (Sub-task 1.1).
- [x] `append_model_rows` no longer pushes the synthetic row (Sub-task 1.1).
- [x] `tree::activate`'s ModelEmptyState arm dropped (Sub-task 1.2).
- [x] Renderer iterates accounts directly and inserts empty-state + manual-model decorations at the alphabetically-correct position (Sub-task 1.3).
- [x] Workspace green + clippy clean + grep clean (Task 2).

Spec requirements not addressed by this plan (intentionally deferred):
- The smoke-test step flagged a potential UX issue: empty-catalog accounts may be unreachable for `r`-refresh because they have no focusable Model rows. If real, this needs a follow-up — possibly extending `account.refresh` to find the account by some other means, or making the empty-state line focusable after all (via a new RowKind variant or a special focus-id scheme). Documented as a known issue; out of scope for the substrate refactor.
- Phase 7 retires the `_`-prefix banner-error rule (no synthetic display paths remain to collide with) and any other transitional cruft.
