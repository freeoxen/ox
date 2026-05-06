# Inline new-connection ghost row Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the dimmed name-only modal at `settings/accounts/_new` with a synthetic `+ New connection` ghost row at the top of the expanded Accounts section, reusing the existing inline-edit machinery.

**Architecture:** Add a `RowKind::AccountAdd` variant to the visible-rows projection; extend `tree.activate` and `edit.commit` to route through it; rewire the `accounts.add` keystroke and the `account_create` subscription to land focus on the new account in the accordion. Delete the `_new` cursor scope and its dedicated bindings/commands.

**Tech Stack:** Rust, ratatui, structfs-core-store, ox_broker subscriptions.

**Spec:** [docs/superpowers/specs/2026-05-05-inline-new-connection-design.md](../specs/2026-05-05-inline-new-connection-design.md)

---

## File Map

**Create:**
- (none)

**Modify:**
- `crates/ox-gate/src/subscriptions/account_create.rs` — change cursor target, add focused_row + expand writes (Task 1).
- `crates/ox-cli/src/settings/visible_rows.rs` — new `RowKind::AccountAdd`, prepend ghost row in `append_account_rows`, update existing row-count tests (Tasks 2–3).
- `crates/ox-cli/src/settings/commands/edit.rs` — new `begin_account_add` helper, new `AccountAdd` arm in `commit` (Tasks 4, 6).
- `crates/ox-cli/src/settings/commands/tree.rs` — route `AccountAdd` activation through `begin_account_add` (Task 4).
- `crates/ox-cli/src/settings/renderers/index.rs` — decorate `AccountAdd` row label during edit (Task 5).
- `crates/ox-cli/src/settings/commands/account_model.rs` — rewrite `AccountsAdd` run body, delete `AccountsCreate` and `accounts.new.*` commands and their tests (Tasks 7, 9).
- `crates/ox-cli/src/settings/bindings.rs` — delete `register_account_new` and the binding-test references to it (Task 9).
- `crates/ox-cli/src/settings/renderers/mod.rs` — drop the overlay registration (Task 8).
- `crates/ox-cli/src/settings/snapshot/replay_tests.rs` (or wherever the modal-typing replay test lives) — replace with inline-typing test (Task 10).

**Delete:**
- `crates/ox-cli/src/settings/renderers/overlay_new_account.rs` (Task 8).

---

## Task 1: Subscription cursor target → settings/index, plus focus + expand on the new account

This task lands first as a standalone fix: even without the inline ghost row, it cures the `unknown cursor: settings/accounts/_detail` error and leaves users on their freshly-created account inside the accordion.

**Files:**
- Modify: `crates/ox-gate/src/subscriptions/account_create.rs:73-129` (handler) and the `create_writes_default_config_selection_cursor` test in the same file.

- [ ] **Step 1: Update the existing test to assert the new write set**

In `crates/ox-gate/src/subscriptions/account_create.rs`, replace the body of the test `create_writes_default_config_selection_cursor` (around line 175) with:

```rust
#[test]
fn create_writes_default_config_selection_focus_and_expansion() {
    let mut reader = InMemoryReader::new();
    let writes = drive(
        &mut reader,
        &CreateAccountRequest {
            name: "alpha".into(),
        },
    );

    // 1. Account record at the canonical path.
    let acct_write = writes
        .iter()
        .find(|w| w.path.to_string() == "config/gate/accounts/alpha")
        .expect("missing account write");
    let cfg: AccountConfig =
        structfs_serde_store::from_value(acct_write.record.as_value().unwrap().clone())
            .unwrap();
    assert_eq!(cfg.provider, DEFAULT_PROVIDER);

    // 2. Selection.
    let sel_write = writes
        .iter()
        .find(|w| w.path.to_string() == "ui/settings/accounts/selected")
        .expect("missing selection write");
    let sel: Option<String> =
        structfs_serde_store::from_value(sel_write.record.as_value().unwrap().clone()).unwrap();
    assert_eq!(sel.as_deref(), Some("alpha"));

    // 3. Page cursor → settings/index (return to accordion; covers the
    //    transitional case where the modal was the entry point).
    let cur_write = writes
        .iter()
        .find(|w| w.path.to_string() == "ui/settings/cursor")
        .expect("missing cursor write");
    match cur_write.record.as_value() {
        Some(Value::Array(segs)) => {
            let parts: Vec<String> = segs
                .iter()
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    _ => panic!("non-string segment"),
                })
                .collect();
            assert_eq!(parts.join("/"), "settings/index");
        }
        other => panic!("cursor must be Value::Array, got {other:?}"),
    }

    // 4. Focused row → settings/accounts/alpha so the user lands on the
    //    new account's row inside the accordion.
    let focus_write = writes
        .iter()
        .find(|w| w.path.to_string() == "ui/settings/focused_row")
        .expect("missing focused_row write");
    match focus_write.record.as_value() {
        Some(Value::Array(segs)) => {
            let parts: Vec<String> = segs
                .iter()
                .map(|v| match v {
                    Value::String(s) => s.clone(),
                    _ => panic!("non-string segment"),
                })
                .collect();
            assert_eq!(parts.join("/"), "settings/accounts/alpha");
        }
        other => panic!("focused_row must be Value::Array, got {other:?}"),
    }

    // 5. Expanded set contains both `settings/accounts` and
    //    `settings/accounts/alpha` so the user immediately sees the
    //    field rows.
    let exp_write = writes
        .iter()
        .find(|w| w.path.to_string() == "ui/settings/expanded")
        .expect("missing expanded write");
    let set: Vec<String> =
        structfs_serde_store::from_value(exp_write.record.as_value().unwrap().clone()).unwrap();
    assert!(
        set.iter().any(|s| s == "settings/accounts"),
        "expanded set must include settings/accounts; got {set:?}"
    );
    assert!(
        set.iter().any(|s| s == "settings/accounts/alpha"),
        "expanded set must include settings/accounts/alpha; got {set:?}"
    );

    // 6. _create_now cleared.
    let null = writes.iter().any(|w| {
        w.path.to_string() == "config/gate/accounts/_create_now"
            && matches!(w.record.as_value(), Some(Value::Null))
    });
    assert!(null, "create_now must be cleared after handling");
}
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cargo test -p ox-gate --lib subscriptions::account_create::tests::create_writes_default_config_selection_focus_and_expansion
```

Expected: FAIL — current handler writes cursor=`settings/accounts/_detail` and emits no focused_row or expanded write.

- [ ] **Step 3: Implement the new handler write set**

In `crates/ox-gate/src/subscriptions/account_create.rs`, replace the `vec![...]` block at the end of `handle` (the four-element vec around lines 117-128) with:

```rust
// 1. Materialize the default config.
// 2. Select the new account.
// 3. Drive the focused row to the new account's row inside the
//    accordion; page cursor stays at settings/index (where the
//    accordion lives).
// 4. Add the new account to the expanded set so its field rows
//    are immediately visible.
// 5. Clear _create_now so the same name can be created again
//    in a session.
let new_account_row = oxpath!("settings", "accounts", PathComponent::try_new(req.name.clone()).unwrap());
let mut expanded: Vec<String> = ctx
    .snapshot
    .read(&oxpath!("ui", "settings", "expanded"))
    .ok()
    .flatten()
    .and_then(|r| r.as_value().cloned())
    .and_then(|v| structfs_serde_store::from_value::<Vec<String>>(v).ok())
    .unwrap_or_default();
let accounts_key = "settings/accounts".to_string();
let new_row_key = format!("settings/accounts/{}", req.name);
if !expanded.iter().any(|s| s == &accounts_key) {
    expanded.push(accounts_key);
}
if !expanded.iter().any(|s| s == &new_row_key) {
    expanded.push(new_row_key);
}
let expanded_value = structfs_serde_store::to_value(&expanded)
    .unwrap_or(structfs_core_store::Value::Null);

vec![
    write_typed(&acct_path, &cfg),
    write_typed(
        &oxpath!("ui", "settings", "accounts", "selected"),
        &Some(req.name.clone()),
    ),
    write_path(
        &oxpath!("ui", "settings", "cursor"),
        &oxpath!("settings", "index"),
    ),
    write_path(&oxpath!("ui", "settings", "focused_row"), &new_account_row),
    Write {
        path: oxpath!("ui", "settings", "expanded"),
        record: Record::parsed(expanded_value),
    },
    null_write(oxpath!("config", "gate", "accounts", "_create_now")),
]
```

Add `use structfs_core_store::Record;` to the imports if not already present. (`Record` is referenced both in the new code and the existing helper definitions; check for an existing import first.)

- [ ] **Step 4: Run the test to verify it passes**

```bash
cargo test -p ox-gate --lib subscriptions::account_create::tests::create_writes_default_config_selection_focus_and_expansion
```

Expected: PASS.

- [ ] **Step 5: Run the full ox-gate test suite to catch regressions**

```bash
cargo test -p ox-gate --lib
```

Expected: PASS (every test in ox-gate). The other two tests in this module (`create_rejects_invalid_name_with_banner` and `create_inert_when_after_record_is_missing`) don't inspect the cursor/focus writes, so they should still pass.

- [ ] **Step 6: Commit**

```bash
git add crates/ox-gate/src/subscriptions/account_create.rs
git commit -m "fix(gate): account_create lands the new account in the accordion

Cursor went to settings/accounts/_detail, which has no renderer, so
the post-create UI flashed an unknown-cursor banner. Send the cursor
back to settings/index, point focused_row at the new account's row,
and add it to the expanded set so its field rows are visible."
```

---

## Task 2: Add `RowKind::AccountAdd` variant

**Files:**
- Modify: `crates/ox-cli/src/settings/visible_rows.rs:19-50` (RowKind enum).

- [ ] **Step 1: Add the variant**

In `crates/ox-cli/src/settings/visible_rows.rs`, in the `RowKind` enum, add a new variant after `ModelAddManual`:

```rust
    /// Synthetic ghost row at the top of the expanded Accounts section.
    /// Activating it (Enter) opens the inline name prompt that
    /// ultimately fires `accounts.create`. The new connection lands as
    /// a real `Account` row when the subscription replies.
    AccountAdd,
```

- [ ] **Step 2: Verify the crate still compiles**

```bash
cargo check -p ox-cli
```

Expected: Compiles. No callers exist yet — the variant is unreachable but well-formed. (You may see a `dead_code` warning; that goes away as soon as Task 3 starts emitting it.)

- [ ] **Step 3: Commit**

```bash
git add crates/ox-cli/src/settings/visible_rows.rs
git commit -m "refactor(settings): add RowKind::AccountAdd variant

Placeholder for the next task — the synthetic ghost row that will
front the accounts section when expanded."
```

---

## Task 3: Prepend the `+ New connection` ghost row when Accounts is expanded

**Files:**
- Modify: `crates/ox-cli/src/settings/visible_rows.rs:133-195` (`append_account_rows`).
- Modify: `crates/ox-cli/src/settings/visible_rows.rs` tests `expanded_accounts_inlines_account_rows` and `expanded_account_inlines_field_rows`.

- [ ] **Step 1: Write a new ghost-row test**

In the `tests` module of `visible_rows.rs`, add:

```rust
#[test]
fn expanded_accounts_section_starts_with_account_add_ghost_row() {
    let mut snap = SettingsSnapshot::empty();
    write_index_entries(&mut snap);
    write_account(&mut snap, "alpha");
    snap.insert(
        &oxpath!("ui", "settings", "expanded"),
        expanded_set_to_value(&["settings/accounts".to_string()]),
    );
    let rows = enumerate(&mut snap);
    // Accounts header + ghost + alpha + Models header = 4
    assert_eq!(rows.len(), 4);
    assert!(matches!(&rows[0].kind, RowKind::Entry { entry_id } if entry_id == "accounts"));
    assert!(matches!(&rows[1].kind, RowKind::AccountAdd));
    assert_eq!(rows[1].depth, 1);
    assert_eq!(rows[1].label, "+ New connection");
    assert!(!rows[1].expandable);
    assert_eq!(
        rows[1].path,
        oxpath!("settings", "accounts", "_new"),
    );
    assert!(matches!(&rows[2].kind, RowKind::Account { name } if name == "alpha"));
}

#[test]
fn collapsed_accounts_section_has_no_ghost_row() {
    let mut snap = SettingsSnapshot::empty();
    write_index_entries(&mut snap);
    write_account(&mut snap, "alpha");
    let rows = enumerate(&mut snap);
    // Collapsed: just the two top-level entries.
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| !matches!(r.kind, RowKind::AccountAdd)));
}
```

- [ ] **Step 2: Run the new tests; expect failure**

```bash
cargo test -p ox-cli --lib settings::visible_rows::tests::expanded_accounts_section_starts_with_account_add_ghost_row
cargo test -p ox-cli --lib settings::visible_rows::tests::collapsed_accounts_section_has_no_ghost_row
```

Expected: FAIL on the count assertion / kind matcher.

- [ ] **Step 3: Implement the ghost row in `append_account_rows`**

In `crates/ox-cli/src/settings/visible_rows.rs`, at the very top of `append_account_rows` (right after the function signature, before the `child_names_under` call), insert:

```rust
    // The ghost "+ New connection" row sits at the top of the section
    // when accounts is expanded. Activating it (Enter) routes through
    // edit.rs's begin_account_add helper, which seeds inline edit mode
    // pointing at this row's path. The path identifier — settings/accounts/_new
    // — is reserved: no real account uses it, and the renderer never
    // tries to render it as a cursor.
    rows.push(VisibleRow {
        path: oxpath!("settings", "accounts", "_new"),
        depth: 1,
        label: "+ New connection".into(),
        secondary: None,
        badge: None,
        kind: RowKind::AccountAdd,
        expandable: false,
        expanded: false,
    });
```

- [ ] **Step 4: Run the new tests; expect pass**

```bash
cargo test -p ox-cli --lib settings::visible_rows::tests::expanded_accounts_section_starts_with_account_add_ghost_row
cargo test -p ox-cli --lib settings::visible_rows::tests::collapsed_accounts_section_has_no_ghost_row
```

Expected: PASS.

- [ ] **Step 5: Update existing tests whose row counts changed**

Two existing tests assert specific row counts that just shifted up by 1:

`expanded_accounts_inlines_account_rows` (currently expects 4 rows for "Accounts header + 2 accounts + Models header"). Update:

```rust
#[test]
fn expanded_accounts_inlines_account_rows() {
    let mut snap = SettingsSnapshot::empty();
    write_index_entries(&mut snap);
    write_account(&mut snap, "alpha");
    write_account(&mut snap, "beta");
    snap.insert(
        &oxpath!("ui", "settings", "expanded"),
        expanded_set_to_value(&["settings/accounts".to_string()]),
    );
    let rows = enumerate(&mut snap);
    // Accounts header + ghost + 2 accounts + Models header = 5
    assert_eq!(rows.len(), 5);
    assert!(matches!(&rows[1].kind, RowKind::AccountAdd));
    assert!(matches!(&rows[2].kind, RowKind::Account { name } if name == "alpha"));
    assert_eq!(rows[2].depth, 1);
    assert!(matches!(&rows[3].kind, RowKind::Account { name } if name == "beta"));
    assert!(matches!(&rows[4].kind, RowKind::Entry { entry_id } if entry_id == "models"));
}
```

`account_row_secondary_indicates_shared_provider` doesn't assert on count, but `find` calls work without changes — leave it.

- [ ] **Step 6: Update the matching index renderer test**

`crates/ox-cli/src/settings/renderers/index.rs` has `expanded_renders_inline_children_with_indent` (line ~404) that asserts 4 rows for "Accounts (▾) + alpha (▸) + beta (▸) + Models (▸)". Update its row-count assertion to 5 and adjust the index expectations:

```rust
#[test]
fn expanded_renders_inline_children_with_indent() {
    let mut snap = SettingsSnapshot::empty();
    write_index(&mut snap);
    write_account(&mut snap, "alpha");
    write_account(&mut snap, "beta");
    snap.insert(
        &oxpath!("ui", "settings", "expanded"),
        expanded_set_to_value(&["settings/accounts".to_string()]),
    );
    let (_title, items, _selected) = assert_list(render(&mut snap));
    // Accounts (▾) + ghost + alpha (▸) + beta (▸) + Models (▸) = 5
    assert_eq!(items.len(), 5);
    assert!(items[0].primary.starts_with("▾ "));
    // Ghost row (depth 1, no expand glyph).
    assert!(
        items[1].primary.contains("+ New connection"),
        "expected ghost row at index 1; got {:?}",
        items[1].primary
    );
    // Depth-1 rows are indented two spaces and carry their own
    // expand glyph because they're expandable too.
    assert!(
        items[2].primary.starts_with("  ▸ "),
        "expected depth-1 indented expand glyph; got {:?}",
        items[2].primary
    );
    assert!(items[2].primary.ends_with("alpha"));
    assert!(items[3].primary.ends_with("beta"));
    assert!(items[4].primary.starts_with("▸ "));
}
```

`expanded_account_inlines_field_rows` asserts 8 rows for "Accounts (▾) + alpha (▾) + 5 fields + Models (▸)". Update to 9:

```rust
#[test]
fn expanded_account_inlines_field_rows() {
    let mut snap = SettingsSnapshot::empty();
    write_index(&mut snap);
    write_account(&mut snap, "alpha");
    snap.insert(
        &oxpath!("ui", "settings", "expanded"),
        expanded_set_to_value(&[
            "settings/accounts".to_string(),
            "settings/accounts/alpha".to_string(),
        ]),
    );
    let (_title, items, _selected) = assert_list(render(&mut snap));
    // Accounts (▾) + ghost + alpha (▾) + 5 field rows + Models (▸) = 9.
    assert_eq!(items.len(), 9);
    // First field row is "Name: alpha", indented to depth 2.
    assert!(items[3].primary.contains("Name: alpha"));
    assert!(items[3].primary.starts_with("    "));
    assert!(items[4].primary.contains("Protocol:"));
    assert!(items[7].primary.contains("Key:"));
}
```

- [ ] **Step 7: Run the visible-rows + index renderer tests**

```bash
cargo test -p ox-cli --lib settings::visible_rows::tests
cargo test -p ox-cli --lib settings::renderers::index::tests
```

Expected: PASS.

- [ ] **Step 8: Run the full ox-cli suite to catch other off-by-one fallout**

```bash
cargo test -p ox-cli --lib
```

Expected: PASS (or, if another test asserts a hard count that shifted by one, update it the same way — call out the test name and adjust the count and any positional indices).

- [ ] **Step 9: Commit**

```bash
git add crates/ox-cli/src/settings/visible_rows.rs crates/ox-cli/src/settings/renderers/index.rs
git commit -m "feat(settings): + New connection ghost row at top of accounts section

Adds a synthetic depth-1 row to visible_rows when the Accounts section
is expanded. The row is non-expandable and uses settings/accounts/_new
as its identifier path. Activation behavior comes in the next task."
```

---

## Task 4: Activate the ghost row → enter inline edit mode

**Files:**
- Modify: `crates/ox-cli/src/settings/commands/edit.rs` (add helper).
- Modify: `crates/ox-cli/src/settings/commands/tree.rs:153-239` (`activate`).
- Modify: `crates/ox-cli/src/settings/commands/tree.rs` tests.

- [ ] **Step 1: Write a failing test in `tree.rs`**

In `crates/ox-cli/src/settings/commands/tree.rs`'s `tests` module (after `activate_on_endpoint_field_enters_inline_edit_mode`), add:

```rust
#[test]
fn activate_on_account_add_ghost_enters_inline_edit_mode() {
    let mut snap = SettingsSnapshot::empty();
    snap.insert(
        &oxpath!("settings", "index", "entries", "accounts"),
        to_value(&entry("accounts", "settings/accounts")).unwrap(),
    );
    snap.insert(
        &oxpath!("ui", "settings", "expanded"),
        crate::settings::visible_rows::expanded_set_to_value(&["settings/accounts".to_string()]),
    );
    snap.insert(
        &oxpath!("ui", "settings", "focused_row"),
        crate::settings::commands::navigation::path_to_value(&oxpath!(
            "settings", "accounts", "_new"
        )),
    );

    let writes = run(&TreeActivate::new(), &mut snap);
    let by_path: std::collections::BTreeMap<_, _> = writes
        .iter()
        .map(|w| (w.path.to_string(), w.record.clone()))
        .collect();

    // edit_field_path → settings/accounts/_new
    let efp = by_path
        .get("ui/settings/edit_field_path")
        .expect("edit_field_path write");
    match efp {
        Record::Parsed(v) => {
            let parts: Vec<String> = match v {
                Value::Array(segs) => segs
                    .iter()
                    .map(|s| match s {
                        Value::String(s) => s.clone(),
                        _ => panic!(),
                    })
                    .collect(),
                _ => panic!("expected array, got {v:?}"),
            };
            assert_eq!(parts.join("/"), "settings/accounts/_new");
        }
        other => panic!("unexpected record: {other:?}"),
    }

    // edit_buffer = ""
    let buf = by_path
        .get("ui/settings/edit_buffer")
        .expect("edit_buffer write");
    match buf {
        Record::Parsed(Value::String(s)) => assert!(s.is_empty()),
        other => panic!("unexpected buffer record: {other:?}"),
    }

    // edit_mode = true
    let em = by_path.get("ui/settings/edit_mode").expect("edit_mode write");
    match em {
        Record::Parsed(Value::Bool(true)) => {}
        other => panic!("unexpected edit_mode record: {other:?}"),
    }
}
```

(If the existing helper `entry` in tree.rs's tests requires a different signature, adapt to match — it already exists for the other tree-activate tests. The shape is: `fn entry(id: &str, target: &str) -> SettingsIndexEntry`.)

- [ ] **Step 2: Run; expect failure**

```bash
cargo test -p ox-cli --lib settings::commands::tree::tests::activate_on_account_add_ghost_enters_inline_edit_mode
```

Expected: FAIL — `tree.activate` currently returns `Vec::new()` for `RowKind::AccountAdd` (it falls through the `_` arm in the leaf-row match — actually, since `AccountAdd` is a *new* variant, the match isn't exhaustive and the crate may not even compile until you add an arm). If compile fails, that's a stronger signal — proceed to Step 3 either way.

- [ ] **Step 3: Add the `begin_account_add` helper in `edit.rs`**

In `crates/ox-cli/src/settings/commands/edit.rs`, after `begin_edit_model_field_inner` (around line 215) add:

```rust
/// Begin inline edit on the synthetic AccountAdd ghost row. Seeds an
/// empty buffer at the ghost row's path. Public so `tree.activate` and
/// `accounts.add` can both call it.
pub(crate) fn begin_account_add() -> Vec<Write> {
    enter_edit_mode(oxpath!("settings", "accounts", "_new"), String::new())
}
```

`enter_edit_mode` already exists (line 260) and writes `edit_field_path`, `edit_buffer`, and `edit_mode=true` — exactly what we need.

- [ ] **Step 4: Add the `AccountAdd` arm in `tree::activate`**

In `crates/ox-cli/src/settings/commands/tree.rs`'s leaf-row `match &row.kind { ... }` block (the `else` branch around line 193), add an arm before `RowKind::Entry { .. } | RowKind::Account { .. } | RowKind::Model { .. } => Vec::new(),`:

```rust
            RowKind::AccountAdd => super::edit::begin_account_add(),
```

- [ ] **Step 5: Run; expect pass**

```bash
cargo test -p ox-cli --lib settings::commands::tree::tests::activate_on_account_add_ghost_enters_inline_edit_mode
```

Expected: PASS. Also rerun the full tree::tests to confirm no other regressions:

```bash
cargo test -p ox-cli --lib settings::commands::tree::tests
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/ox-cli/src/settings/commands/edit.rs crates/ox-cli/src/settings/commands/tree.rs
git commit -m "feat(settings): tree.activate on AccountAdd ghost enters inline edit

Adds begin_account_add to edit.rs (seeds an empty buffer at
settings/accounts/_new) and routes tree.activate through it. Pressing
Enter on the focused ghost row now flips edit_mode on; printable keys
follow through edit.insert_char as for any other field row."
```

---

## Task 5: Renderer decorates `AccountAdd` row label during edit

**Files:**
- Modify: `crates/ox-cli/src/settings/renderers/index.rs:204-250` (`decorate_row_label`).
- Modify: `crates/ox-cli/src/settings/renderers/index.rs` tests.

- [ ] **Step 1: Write a failing test**

In `crates/ox-cli/src/settings/renderers/index.rs`'s `tests` module, add:

```rust
#[test]
fn account_add_ghost_row_renders_inline_buffer_during_edit() {
    let mut snap = SettingsSnapshot::empty();
    write_index(&mut snap);
    snap.insert(
        &oxpath!("ui", "settings", "expanded"),
        expanded_set_to_value(&["settings/accounts".to_string()]),
    );
    snap.insert(
        &oxpath!("ui", "settings", "focused_row"),
        path_to_value(&oxpath!("settings", "accounts", "_new")),
    );
    snap.insert(&oxpath!("ui", "settings", "edit_mode"), Value::Bool(true));
    snap.insert(
        &oxpath!("ui", "settings", "edit_field_path"),
        path_to_value(&oxpath!("settings", "accounts", "_new")),
    );
    snap.insert(
        &oxpath!("ui", "settings", "edit_buffer"),
        Value::String("per".into()),
    );
    let (_title, items, selected) = assert_list(render(&mut snap));
    let i = selected.expect("ghost row is selected");
    // Ghost row should render its "Name▸ per▏" label, not "+ New connection".
    assert!(
        items[i].primary.contains("Name▸ per\u{258F}"),
        "expected inline-edit decoration; got {:?}",
        items[i].primary
    );
}

#[test]
fn account_add_ghost_row_renders_plain_label_when_not_editing() {
    let mut snap = SettingsSnapshot::empty();
    write_index(&mut snap);
    snap.insert(
        &oxpath!("ui", "settings", "expanded"),
        expanded_set_to_value(&["settings/accounts".to_string()]),
    );
    let (_title, items, _selected) = assert_list(render(&mut snap));
    // Ghost row at index 1 (Accounts header at 0).
    assert!(
        items[1].primary.contains("+ New connection"),
        "expected plain ghost label; got {:?}",
        items[1].primary
    );
}
```

- [ ] **Step 2: Run; expect failure**

```bash
cargo test -p ox-cli --lib settings::renderers::index::tests::account_add_ghost_row_renders_inline_buffer_during_edit
```

Expected: FAIL — `decorate_row_label` falls through to `return row.label.clone()` for `AccountAdd`, so the rendered primary still contains "+ New connection".

- [ ] **Step 3: Add an `AccountAdd` arm in `decorate_row_label`**

In `crates/ox-cli/src/settings/renderers/index.rs`, in the `match &row.kind { ... }` block of `decorate_row_label`, before the catch-all `_ => return row.label.clone()`, add:

```rust
        RowKind::AccountAdd => "Name",
```

The function then formats `"Name▸ {buffer}\u{258F}"` exactly as it does for the other field rows.

- [ ] **Step 4: Run; expect pass**

```bash
cargo test -p ox-cli --lib settings::renderers::index::tests::account_add_ghost_row_renders_inline_buffer_during_edit
cargo test -p ox-cli --lib settings::renderers::index::tests::account_add_ghost_row_renders_plain_label_when_not_editing
```

Expected: PASS.

- [ ] **Step 5: Run the index-renderer suite**

```bash
cargo test -p ox-cli --lib settings::renderers::index::tests
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/ox-cli/src/settings/renderers/index.rs
git commit -m "feat(settings): index renderer decorates AccountAdd row mid-edit

The ghost row picks up the same Name▸ <buffer>▏ overlay used for
in-place field edits when edit_mode is active and the focused field
path matches its row path."
```

---

## Task 6: `edit.commit` writes `_create_now` for the AccountAdd row

**Files:**
- Modify: `crates/ox-cli/src/settings/commands/edit.rs:370-404` (`commit`).
- Modify: `crates/ox-cli/src/settings/commands/edit.rs` tests.

- [ ] **Step 1: Write failing tests**

In `crates/ox-cli/src/settings/commands/edit.rs`'s `tests` module, add:

```rust
#[test]
fn commit_account_add_writes_create_request_and_clears_state() {
    use ox_types::settings::CreateAccountRequest;
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

    // _create_now carries the CreateAccountRequest.
    let create = by_path
        .get("config/gate/accounts/_create_now")
        .expect("_create_now write");
    let req: CreateAccountRequest = match create {
        Record::Parsed(v) => structfs_serde_store::from_value(v.clone()).unwrap(),
        other => panic!("unexpected record: {other:?}"),
    };
    assert_eq!(req.name, "alpha");

    // Edit state is cleared.
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

#[test]
fn commit_account_add_with_empty_buffer_keeps_edit_mode_open() {
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
        Value::String("   ".into()),
    );

    let writes = run(&Commit::new(), &mut snap);
    // No _create_now write; no edit-state clear — user can keep typing.
    assert!(
        writes.is_empty(),
        "expected no writes for empty/whitespace buffer; got {writes:?}"
    );
}
```

- [ ] **Step 2: Run; expect failure**

```bash
cargo test -p ox-cli --lib settings::commands::edit::tests::commit_account_add_writes_create_request_and_clears_state
cargo test -p ox-cli --lib settings::commands::edit::tests::commit_account_add_with_empty_buffer_keeps_edit_mode_open
```

Expected: FAIL — current `commit` falls through to `_ => Vec::new()` for `AccountAdd` and clears state without writing the request.

- [ ] **Step 3: Add the `AccountAdd` arm in `commit`**

In `crates/ox-cli/src/settings/commands/edit.rs`, in the `commit` function (around line 391-404), update the `match row.map(|r| r.kind)` block:

```rust
    let row = visible_rows::enumerate(data)
        .into_iter()
        .find(|r| r.path == field_path);
    let mut writes: Vec<Write> = match row.map(|r| r.kind) {
        Some(RowKind::AccountField { account, field }) => {
            commit_account_field(data, &account, field, &buffer)
        }
        Some(RowKind::ModelField {
            account,
            model_id,
            field,
        }) => commit_model_field(data, &account, &model_id, field, &buffer),
        Some(RowKind::AccountAdd) => {
            // Empty / whitespace name: silently no-op so edit mode stays
            // open and the user can keep typing. The subscription will
            // reject invalid names with a banner once we get here.
            let trimmed = buffer.trim();
            if trimmed.is_empty() {
                return Vec::new();
            }
            let req = ox_types::settings::CreateAccountRequest {
                name: trimmed.to_string(),
            };
            let value = match to_value(&req) {
                Ok(v) => v,
                Err(_) => return Vec::new(),
            };
            vec![Write {
                path: oxpath!("config", "gate", "accounts", "_create_now"),
                record: Record::parsed(value),
            }]
        }
        _ => Vec::new(),
    };
    writes.extend(clear_edit_state());
    writes
```

The `early return Vec::new()` for empty-buffer keeps edit mode open (no clear-state writes appended), matching the manual-model id-stage convention.

`CreateAccountRequest` is already in scope via `use ox_types::settings::{AccountField, ModelField, ModelKey};` — extend that to include `CreateAccountRequest`:

```rust
use ox_types::settings::{AccountField, CreateAccountRequest, ModelField, ModelKey};
```

Then the inline reference can be `CreateAccountRequest { name: ... }` instead of the fully-qualified path.

- [ ] **Step 4: Run; expect pass**

```bash
cargo test -p ox-cli --lib settings::commands::edit::tests::commit_account_add_writes_create_request_and_clears_state
cargo test -p ox-cli --lib settings::commands::edit::tests::commit_account_add_with_empty_buffer_keeps_edit_mode_open
```

Expected: PASS.

- [ ] **Step 5: Run the edit-tests suite**

```bash
cargo test -p ox-cli --lib settings::commands::edit::tests
```

Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/ox-cli/src/settings/commands/edit.rs
git commit -m "feat(settings): edit.commit on AccountAdd writes _create_now

Routes the ghost row's commit through the same broker subscription the
modal used (config/gate/accounts/_create_now). Empty/whitespace name
silently no-ops to keep edit mode open."
```

---

## Task 7: Rewire `accounts.add` (the `a` keystroke) to the inline flow

**Files:**
- Modify: `crates/ox-cli/src/settings/commands/account_model.rs:32-66` (`AccountsAdd`).
- Modify: `crates/ox-cli/src/settings/commands/account_model.rs:1122-1160` (`accounts_add_writes_new_cursor_and_isolates_input_scope` test).

- [ ] **Step 1: Replace the existing test**

In `crates/ox-cli/src/settings/commands/account_model.rs`, replace `accounts_add_writes_new_cursor_and_isolates_input_scope` with:

```rust
#[test]
fn accounts_add_expands_section_focuses_ghost_and_enters_edit() {
    let mut snap = SettingsSnapshot::empty();
    let writes = run_cmd(&AccountsAdd::new(), &mut snap);
    let by_path: std::collections::BTreeMap<_, _> = writes
        .iter()
        .map(|w| (w.path.to_string(), w.record.clone()))
        .collect();

    // expanded set must contain settings/accounts.
    let exp = by_path.get("ui/settings/expanded").expect("expanded write");
    let set: Vec<String> = match exp {
        Record::Parsed(v) => structfs_serde_store::from_value(v.clone()).unwrap(),
        other => panic!("unexpected: {other:?}"),
    };
    assert!(
        set.iter().any(|s| s == "settings/accounts"),
        "expanded set must include settings/accounts; got {set:?}"
    );

    // focused_row → settings/accounts/_new (the ghost row).
    let focus = by_path
        .get("ui/settings/focused_row")
        .expect("focused_row write");
    match focus {
        Record::Parsed(v) => {
            let parts: Vec<String> = match v {
                Value::Array(segs) => segs
                    .iter()
                    .map(|s| match s {
                        Value::String(s) => s.clone(),
                        _ => panic!(),
                    })
                    .collect(),
                _ => panic!(),
            };
            assert_eq!(parts.join("/"), "settings/accounts/_new");
        }
        other => panic!("unexpected: {other:?}"),
    }

    // edit_field_path → settings/accounts/_new.
    let efp = by_path
        .get("ui/settings/edit_field_path")
        .expect("edit_field_path write");
    match efp {
        Record::Parsed(Value::Array(segs)) => {
            let parts: Vec<String> = segs
                .iter()
                .map(|s| match s {
                    Value::String(s) => s.clone(),
                    _ => panic!(),
                })
                .collect();
            assert_eq!(parts.join("/"), "settings/accounts/_new");
        }
        other => panic!("unexpected: {other:?}"),
    }

    // edit_buffer = "".
    match by_path.get("ui/settings/edit_buffer").unwrap() {
        Record::Parsed(Value::String(s)) => assert!(s.is_empty()),
        other => panic!("unexpected: {other:?}"),
    }

    // edit_mode = true.
    match by_path.get("ui/settings/edit_mode").unwrap() {
        Record::Parsed(Value::Bool(true)) => {}
        other => panic!("unexpected: {other:?}"),
    }
}

#[test]
fn accounts_add_preserves_existing_expanded_entries() {
    let mut snap = SettingsSnapshot::empty();
    snap.insert(
        &oxpath!("ui", "settings", "expanded"),
        crate::settings::visible_rows::expanded_set_to_value(&["settings/models".to_string()]),
    );
    let writes = run_cmd(&AccountsAdd::new(), &mut snap);
    let exp = writes
        .iter()
        .find(|w| w.path == oxpath!("ui", "settings", "expanded"))
        .expect("expanded write");
    let set: Vec<String> = match &exp.record {
        Record::Parsed(v) => structfs_serde_store::from_value(v.clone()).unwrap(),
        other => panic!("unexpected: {other:?}"),
    };
    assert!(
        set.iter().any(|s| s == "settings/models"),
        "must not drop pre-existing entries; got {set:?}"
    );
    assert!(
        set.iter().any(|s| s == "settings/accounts"),
        "must add settings/accounts; got {set:?}"
    );
}
```

- [ ] **Step 2: Run; expect failure**

```bash
cargo test -p ox-cli --lib settings::commands::account_model::tests::accounts_add_expands_section_focuses_ghost_and_enters_edit
cargo test -p ox-cli --lib settings::commands::account_model::tests::accounts_add_preserves_existing_expanded_entries
```

Expected: FAIL — current `accounts.add` writes the modal cursor, clears focused_row, etc.

- [ ] **Step 3: Rewrite the `AccountsAdd` command**

In `crates/ox-cli/src/settings/commands/account_model.rs`, replace the `AccountsAdd` `command!` block (lines 32-66) with:

```rust
command! {
    struct_name: AccountsAdd,
    id: "accounts.add",
    title: "Add Connection",
    description: "Open the inline new-connection prompt at the top of the accounts section.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "accounts")),
    run: |snap, _ctx| accounts_add(snap),
}
```

Then add the helper near the other run-body helpers in the file (e.g. after `accounts_create` if you haven't deleted it yet — Task 9 deletes it):

```rust
fn accounts_add(data: &mut dyn Reader) -> Vec<Write> {
    use crate::settings::visible_rows::{expanded_set_to_value, read_expanded_set};

    let mut expanded = read_expanded_set(data);
    let accounts_key = "settings/accounts".to_string();
    if !expanded.iter().any(|s| s == &accounts_key) {
        expanded.push(accounts_key);
    }

    let mut writes = vec![Write {
        path: oxpath!("ui", "settings", "expanded"),
        record: Record::parsed(expanded_set_to_value(&expanded)),
    }];

    // Focus the ghost row before flipping edit_mode so the renderer's
    // overlay locks onto the right row.
    writes.push(Write {
        path: oxpath!("ui", "settings", "focused_row"),
        record: Record::parsed(path_to_value(&oxpath!(
            "settings", "accounts", "_new"
        ))),
    });

    // Reuse edit.rs's helper so the seed shape matches what tree.activate
    // produces from the same row.
    writes.extend(super::edit::begin_account_add());
    writes
}
```

`path_to_value` is already used in this file via `super::navigation::path_to_value` — pull it into local scope with `use super::navigation::path_to_value;` at the top of the file if it isn't already.

- [ ] **Step 4: Run; expect pass**

```bash
cargo test -p ox-cli --lib settings::commands::account_model::tests::accounts_add_expands_section_focuses_ghost_and_enters_edit
cargo test -p ox-cli --lib settings::commands::account_model::tests::accounts_add_preserves_existing_expanded_entries
```

Expected: PASS.

- [ ] **Step 5: Run the full ox-cli test suite**

```bash
cargo test -p ox-cli --lib
```

Expected: PASS. The modal still exists (its bindings haven't been deleted yet), but `accounts.add` no longer routes through it — the modal becomes orphaned. This intermediate state is fine.

- [ ] **Step 6: Commit**

```bash
git add crates/ox-cli/src/settings/commands/account_model.rs
git commit -m "feat(settings): accounts.add opens the inline ghost row

The 'a' keystroke now expands the accounts section if needed, focuses
the synthetic + New connection row, and enters inline edit mode in one
shot. The modal renderer is still wired but no longer reachable from
the keybinding — cleanup follows in subsequent tasks."
```

---

## Task 8: Delete the modal renderer

**Files:**
- Delete: `crates/ox-cli/src/settings/renderers/overlay_new_account.rs`.
- Modify: `crates/ox-cli/src/settings/renderers/mod.rs:10-20`.

- [ ] **Step 1: Delete the renderer file**

```bash
git rm crates/ox-cli/src/settings/renderers/overlay_new_account.rs
```

- [ ] **Step 2: Drop the registration**

In `crates/ox-cli/src/settings/renderers/mod.rs`, remove the `pub mod overlay_new_account;` declaration and the `overlay_new_account::register(reg);` call inside `register_all`. The file should look like:

```rust
//! Concrete `Renderer` impls per page (Phases J/K).
//!
//! Each renderer is a pure `&mut dyn Reader -> View` function. Composition
//! is value-shaped: overlay renderers recurse into the `RendererRegistry` to
//! get the background View and wrap it in a `View::Modal`.
//!
//! `register_all` is invoked once at settings-screen startup to install
//! every renderer at its prescribed cursor path.

pub mod index;
pub mod overlay_delete_account;
pub(crate) mod util;

/// Register every settings renderer at its prescribed cursor path.
pub fn register_all(reg: &mut crate::settings::registry::RendererRegistry) {
    index::register(reg);
    overlay_delete_account::register(reg);
}
```

- [ ] **Step 3: Verify the crate still compiles**

```bash
cargo build -p ox-cli
```

Expected: Compiles. Any compile error here means a stale reference to `overlay_new_account` somewhere — search and remove (`grep -rn overlay_new_account crates/ox-cli/`).

- [ ] **Step 4: Run the ox-cli suite**

```bash
cargo test -p ox-cli --lib
```

Expected: PASS. The modal-typing replay test (Task 10) may still pass at this point if it doesn't depend on the renderer; leave it for now.

- [ ] **Step 5: Commit**

```bash
git add crates/ox-cli/src/settings/renderers/
git commit -m "chore(settings): delete the new-account modal renderer

Replaced by the inline ghost row at settings/accounts/_new. The path
stays as a row identifier in the visible-rows projection but no
renderer renders it anymore."
```

---

## Task 9: Delete `_new` cursor scope bindings and modal-only commands

**Files:**
- Modify: `crates/ox-cli/src/settings/bindings.rs:212-264` (delete `register_account_new`); also remove its call site (search for `register_account_new(`).
- Modify: `crates/ox-cli/src/settings/commands/account_model.rs` — delete `AccountsCreate`, `accounts_create` helper, the `AccountsCreate` registration, and the `accounts_create_*` tests.
- Modify: `crates/ox-cli/src/settings/commands/account_model.rs` — also delete `AccountsNewInsertChar` and `AccountsNewDeleteBack` if they live in this file (search for them; they may live alongside).
- Modify: any other file that imports / calls the deleted helpers.

- [ ] **Step 1: Find every reference to the modal commands and bindings**

```bash
grep -rn "accounts.new.insert_char\|accounts.new.delete_back\|accounts.create\|register_account_new\|AccountsCreate\|AccountsNewInsertChar\|AccountsNewDeleteBack\|new_account/name_input" crates/ox-cli/src/
```

Make a list of every hit; each one is either the definition you're deleting, a registration to remove, a test to delete, or — surprisingly — a reachable reference that needs handling. Anything reachable that isn't in the inline flow is a bug; investigate before deleting.

The expected hit list is:
- `bindings.rs`: definition of `register_account_new`, its call site, plus a binding test (`accounts_a_resolves_to_accounts_add` is fine — it tests the keybinding name, which we kept).
- `account_model.rs`: `AccountsCreate` command + its registration + `accounts_create` helper + `accounts_create_*` tests.
- The two `accounts.new.*` commands (likely in `account_model.rs` or a sibling). If they live in their own file, that file is to be deleted.
- Any test that constructs `ui/settings/new_account/name_input`.

- [ ] **Step 2: Delete `register_account_new` and its call site in `bindings.rs`**

In `crates/ox-cli/src/settings/bindings.rs`, delete the entire `register_account_new` function (lines ~212-264). Find its caller (likely a top-level `register_all_settings_bindings` or similar in the same file) and remove the `register_account_new(reg);` call.

- [ ] **Step 3: Delete the modal-only commands and tests**

In `crates/ox-cli/src/settings/commands/account_model.rs`:

- Delete the `AccountsCreate` `command!` block (around lines 115-123).
- Delete the `accounts_create` function (around lines 434-456).
- Find the registration line `reg.register(Box::new(AccountsCreate::new()));` (mentioned at line 998 earlier) and delete it.
- Delete the two tests `accounts_create_writes_request_when_name_present` and `accounts_create_inert_when_name_empty_or_missing` (lines 1199-1235).
- Delete the `accounts.new.insert_char` and `accounts.new.delete_back` commands and their helpers and tests (search the file).

If `AccountsNewInsertChar` and `AccountsNewDeleteBack` live in their own file, delete the file with `git rm` and remove the corresponding `mod` declaration. Also remove their registrations from the command-registry call site (search for `AccountsNewInsertChar::new()` etc.).

- [ ] **Step 4: Verify the crate compiles**

```bash
cargo build -p ox-cli
```

Expected: Compiles. Any error names a missed reference — fix it inline.

- [ ] **Step 5: Run the ox-cli test suite**

```bash
cargo test -p ox-cli --lib
```

Expected: PASS. If the binding tests (`bindings.rs`) had a test asserting the `_new` cursor scope contained specific bindings, delete that test — the scope no longer exists.

- [ ] **Step 6: Run the full workspace tests**

```bash
cargo test --workspace
```

Expected: PASS. This catches any external crate that referenced the deleted symbols.

- [ ] **Step 7: Commit**

```bash
git add -u
git commit -m "chore(settings): delete the new-account modal cursor scope and commands

Removes register_account_new and the dedicated _new cursor bindings;
deletes AccountsCreate (its body folds into edit.commit's AccountAdd
arm) and the accounts.new.{insert_char,delete_back} commands. The
inline flow uses edit_mode and edit.{insert_char,delete_back} like
every other field row."
```

---

## Task 10: Replace the modal-typing replay test with an inline-typing test

**Files:**
- Modify: `crates/ox-cli/tests/settings_e2e.rs:1178-1241` (the test `add_connection_modal_accepts_lowercase_and_uppercase_typing`).
- Delete: snapshots `crates/ox-cli/tests/snapshots/settings_e2e__new_connection_modal_just_opened.snap` and `settings_e2e__new_connection_modal_after_{1_T,2_e,3_s,4_t}.snap`.

The framework is `insta` (see `Cargo.toml:54`). The test uses an `E2eHarness` defined elsewhere in the same file; `populate_index`, `render_settings_to_string`, and `h.dispatch(&str)` are already in scope.

- [ ] **Step 1: Read the existing test for context**

```bash
sed -n '1178,1241p' crates/ox-cli/tests/settings_e2e.rs
```

Note the helpers it relies on (`E2eHarness::new`, `populate_index`, `h.write_path`, `h.dispatch`, `render_settings_to_string`) — they all stay; we just change the body.

- [ ] **Step 2: Delete the old snapshot files**

```bash
git rm crates/ox-cli/tests/snapshots/settings_e2e__new_connection_modal_just_opened.snap \
       crates/ox-cli/tests/snapshots/settings_e2e__new_connection_modal_after_1_T.snap \
       crates/ox-cli/tests/snapshots/settings_e2e__new_connection_modal_after_2_e.snap \
       crates/ox-cli/tests/snapshots/settings_e2e__new_connection_modal_after_3_s.snap \
       crates/ox-cli/tests/snapshots/settings_e2e__new_connection_modal_after_4_t.snap
```

- [ ] **Step 3: Replace the test body**

In `crates/ox-cli/tests/settings_e2e.rs`, replace the test (lines 1178-1241) with:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn add_connection_inline_ghost_row_accepts_typing() {
    // End-to-end render assertion for the inline new-connection ghost
    // row's typing surface. Opens it via `a` from the Connections
    // section, types a mix of upper- and lowercase chars, captures the
    // rendered frame after each keystroke. Each press must produce a
    // visibly-different frame and the cumulative input must appear in
    // the focused ghost row's inline buffer — covers case-sensitive
    // dispatch routing AND inline-edit write-back with one shape.
    let h = E2eHarness::new().await;
    populate_index(&h).await;

    // Cursor + focused_row land us under settings/accounts so the
    // `a` binding (Prefix(settings/accounts)) resolves to accounts.add.
    h.write_path(
        &oxpath!("ui", "settings", "cursor"),
        &oxpath!("settings", "accounts"),
    )
    .await;
    h.write_path(
        &oxpath!("ui", "settings", "focused_row"),
        &oxpath!("settings", "accounts"),
    )
    .await;

    assert!(matches!(h.dispatch("a").await, KeyDispatchOutcome::Handled));

    let frame_after_a = render_settings_to_string(&h, 80, 24).await;
    insta::assert_snapshot!("new_connection_inline_just_opened", &frame_after_a);

    let mut prior_frame = frame_after_a.clone();
    let mut snap_idx = 1;
    for ch in "Test".chars() {
        let key = ch.to_string();
        assert!(
            matches!(h.dispatch(&key).await, KeyDispatchOutcome::Handled),
            "dispatch returned Unhandled for {key:?}"
        );
        let frame = render_settings_to_string(&h, 80, 24).await;
        let snap_name = format!("new_connection_inline_after_{}_{}", snap_idx, ch);
        insta::assert_snapshot!(snap_name, &frame);
        assert_ne!(
            prior_frame, frame,
            "typing {ch:?} into the inline ghost row must produce a \
             visible change in the rendered frame"
        );
        prior_frame = frame;
        snap_idx += 1;
    }

    // The final frame's inline buffer must contain the full word —
    // catches the case where each char produces *some* visual change
    // (e.g. cursor blink) but the actual write-back doesn't fill in.
    let final_frame = prior_frame;
    assert!(
        final_frame.contains("Test"),
        "rendered ghost row must show 'Test' in the inline buffer after typing it; \
         got:\n{final_frame}"
    );
}
```

- [ ] **Step 4: Run the test and review the new snapshots**

```bash
cargo test -p ox-cli --test settings_e2e add_connection_inline_ghost_row_accepts_typing
```

Expected: FAIL on the first run (insta records the new snapshots as pending). Review them:

```bash
cargo insta review
```

Confirm in each snapshot that:
- After `a`: the ghost row is focused and shows `Name▸ \u{258F}` (empty buffer).
- After each typed character: the buffer extends ("T", "Te", "Tes", "Test").
- The snapshot does not contain a `View::Modal`-shaped border around an isolated frame — the ghost row is inline within the accordion list.

Accept the snapshots.

- [ ] **Step 5: Re-run to confirm green**

```bash
cargo test -p ox-cli --test settings_e2e add_connection_inline_ghost_row_accepts_typing
```

Expected: PASS.

- [ ] **Step 6: Run the full workspace tests**

```bash
cargo test --workspace
```

Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add -u crates/ox-cli/tests/
git commit -m "test(settings): replay snapshot test for inline new-connection typing

Replaces add_connection_modal_accepts_lowercase_and_uppercase_typing
(the modal is gone). Drives the same E2E shape through the inline
ghost row: 'a' opens it, four characters route through edit.insert_char,
each press produces a visibly-different frame, and the cumulative
input lands in the inline buffer."
```

---

## Final verification

- [ ] **Run the full workspace test suite**

```bash
cargo test --workspace
```

Expected: PASS.

- [ ] **Run clippy**

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: No warnings or errors.

- [ ] **Smoke test the UI yourself**

The user should drive this — the harness can't run the interactive TUI. Ask the user to:

1. Open settings (whatever key opens it).
2. Press `a` from anywhere in the accounts subtree (or with the cursor on the Accounts header).
3. Confirm: the accounts section expands if needed, the cursor lands on the `+ New connection` row, and typing fills in the inline buffer (no modal appears).
4. Type a valid name, press Enter. Confirm: the new account appears expanded with its field rows, focus is on the new row, no error banner.
5. Press `a` again, type an invalid name (e.g. `bad-name`), press Enter. Confirm: error banner appears, edit mode clears.
6. Press `a` again, press Esc without typing. Confirm: edit mode dismisses, ghost row stays focused.
