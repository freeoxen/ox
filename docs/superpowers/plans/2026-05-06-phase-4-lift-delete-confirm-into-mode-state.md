# Phase 4: Lift delete-confirm into `pending_delete` mode state — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the `_delete` modal cursor scope with a `ui/settings/pending_delete: Option<String>` mode-state path. The renderer reads `pending_delete` and emits an inline confirmation banner above the accordion when it's set; the dispatcher routes `y`/`n`/`Esc` to new `accounts.confirm.{delete,cancel}` commands at a `_pending_delete` synthetic scope; `overlay_delete_account.rs` is deleted; `accounts.cancel` is dead and removed; `AccountDeleteCleanupSubscription`'s cursor write is dropped (the user never left `settings/index`).

**Architecture:** Two-commit landing, mirroring Phase 3. Commit A introduces dormant infrastructure (commands + bindings + dispatcher pass + renderer banner gated on `pending_delete = Some`). Commit B switches: `accounts.delete_confirm` writes `pending_delete`, the modal renderer + cursor-scope bindings + `accounts.delete` command + `accounts.cancel` command are deleted, and the subscription's cursor write is dropped.

**Tech Stack:** Rust workspace; `ox-cli` (commands, bindings, dispatcher, renderer), `ox-gate` (subscription cursor cleanup).

**Spec:** [docs/superpowers/specs/2026-05-06-settings-substrate-convergence-design.md](../specs/2026-05-06-settings-substrate-convergence-design.md) §5 Phase 4.

---

## File Map

**Modify:**
- `crates/ox-cli/src/settings/commands/account_model.rs` — add `accounts.confirm.{delete,cancel}` commands + helpers; rewrite `accounts.delete_confirm` (Commit B); delete `AccountsDelete` and `AccountsCancel` commands (Commit B).
- `crates/ox-cli/src/settings/bindings.rs` — add `register_pending_delete` (Commit A); delete `register_account_delete` and its caller (Commit B).
- `crates/ox-cli/src/settings/dispatch.rs` — extend the binding-lookup chain to include the `_pending_delete` synthetic scope before the `_compose_new_account` scope.
- `crates/ox-cli/src/settings/renderers/index.rs` — read `pending_delete` and prepend an inline confirmation banner when set.
- `crates/ox-cli/src/settings/renderers/mod.rs` — drop the `overlay_delete_account` registration (Commit B).
- `crates/ox-gate/src/subscriptions/account_delete.rs` — drop the cursor-write from the cleanup body (Commit B).

**Delete:**
- `crates/ox-cli/src/settings/renderers/overlay_delete_account.rs` (Commit B).

---

## Conventions used in this plan

- All `cargo` commands run synchronously. Do not background.
- Commits are NEW commits (no `--amend`). No `Co-Authored-By` trailer.
- No phase / PR / spec-section comments in code. Doc comments explaining WHY are fine.

---

## Task 1: Commit A — add pending-delete infrastructure (dormant)

This task adds all the new pieces without activating them. Nothing writes `pending_delete` yet, so the dispatcher pass never fires, the new commands are never invoked, and the renderer's `if let Some(_)` branch is dead. User-visible behavior is unchanged.

### Sub-task 1.1: Add the two `accounts.confirm.*` commands

**File:**
- Modify: `crates/ox-cli/src/settings/commands/account_model.rs`

- [ ] **Step 1: Add the `command!` blocks**

After the existing `accounts.compose.*` commands, add:

```rust
command! {
    struct_name: AccountsConfirmDelete,
    id: "accounts.confirm.delete",
    title: "Confirm delete",
    description: "Delete the pending account record; clear the pending-delete pointer.",
    screen: Screen::Settings,
    cursor: None,
    run: |snap, _ctx| accounts_confirm_delete(snap),
}

command! {
    struct_name: AccountsConfirmCancel,
    id: "accounts.confirm.cancel",
    title: "Cancel delete",
    description: "Dismiss the delete-confirmation banner without deleting.",
    screen: Screen::Settings,
    cursor: None,
    run: |_snap, _ctx| vec![Write {
        path: oxpath!("ui", "settings", "pending_delete"),
        record: Record::parsed(Value::Null),
    }],
}
```

- [ ] **Step 2: Add the helper**

```rust
fn accounts_confirm_delete(data: &mut dyn Reader) -> Vec<Write> {
    use ox_kernel::PathComponent;

    // Read the pending account name. If unset, the dispatch shouldn't
    // have routed here — defensive no-op.
    let name: String = read_typed(
        data,
        &oxpath!("ui", "settings", "pending_delete"),
    )
    .unwrap_or_default();
    if name.is_empty() {
        return Vec::new();
    }
    let comp = match PathComponent::try_new(&name) {
        Ok(c) => c,
        Err(_) => {
            // Pending pointer somehow got an invalid name. Clear it
            // defensively so we don't leave the user stuck in
            // confirmation mode.
            return vec![Write {
                path: oxpath!("ui", "settings", "pending_delete"),
                record: Record::parsed(Value::Null),
            }];
        }
    };

    vec![
        // The actual delete — Null write to the canonical account
        // path. The AccountDeleteCleanupSubscription watches Prefix
        // for null writes at account-record depth and does the
        // cross-cutting side-data cleanup.
        Write {
            path: oxpath!("config", "gate", "accounts", comp),
            record: Record::parsed(Value::Null),
        },
        // Clear the pending pointer.
        Write {
            path: oxpath!("ui", "settings", "pending_delete"),
            record: Record::parsed(Value::Null),
        },
    ]
}
```

- [ ] **Step 3: Register the commands**

In account_model.rs's `register` function, add:

```rust
reg.register(Box::new(AccountsConfirmDelete::new()));
reg.register(Box::new(AccountsConfirmCancel::new()));
```

- [ ] **Step 4: Build**

```
cargo build -p ox-cli
```

Expected: PASS.

### Sub-task 1.2: Add bindings at `_pending_delete` synthetic scope

**File:**
- Modify: `crates/ox-cli/src/settings/bindings.rs`

- [ ] **Step 1: Add `register_pending_delete`**

```rust
/// Register the pending-delete confirmation mode's bindings at the
/// synthetic `settings/_pending_delete` cursor scope. The dispatcher
/// routes to this scope when `ui/settings/pending_delete` is `Some(_)`.
fn register_pending_delete(reg: &mut BindingRegistry) {
    let scope = oxpath!("settings", "_pending_delete");
    bind(reg, Some(scope.clone()), no_mods(), KeyCodeRepr::Char('y'), "accounts.confirm.delete");
    bind(reg, Some(scope.clone()), no_mods(), KeyCodeRepr::Char('n'), "accounts.confirm.cancel");
    bind(reg, Some(scope), no_mods(), KeyCodeRepr::Esc, "accounts.confirm.cancel");
}
```

- [ ] **Step 2: Call it from the top-level register**

Add `register_pending_delete(reg);` in the top-level `register` function, alongside the existing `register_compose_new_account(reg);` and `register_account_delete(reg);` calls.

- [ ] **Step 3: Build**

```
cargo build -p ox-cli
```

Expected: PASS.

### Sub-task 1.3: Add the dispatcher's pending-delete pass

**File:**
- Modify: `crates/ox-cli/src/settings/dispatch.rs`

The current chain (post-Phase-3): compose → edit → focused-row → page cursor. Phase 4 adds pending-delete BEFORE compose (highest priority among modes — "user is confirming" is a more specific state than "user is composing").

Actually no — the modes are mutually exclusive by design (only one can be `Some` at a time, since opening one mode clears others). Order matters only as a defensive tiebreaker. Order pending-delete first because it's a "ready-to-take-action" state (y/n) that should win if somehow both were set.

- [ ] **Step 1: Add the buffer-reader helper**

```rust
/// Read `ui/settings/pending_delete`. Returns `Some(_)` when the
/// user is being asked to confirm a delete (pending-delete mode).
fn read_pending_delete(snapshot: &mut dyn Reader) -> Option<String> {
    use ox_path::oxpath;
    use structfs_core_store::Value;
    let record = snapshot
        .read(&oxpath!("ui", "settings", "pending_delete"))
        .ok()
        .flatten()?;
    match record.as_value()? {
        Value::String(s) => Some(s.clone()),
        _ => None,
    }
}
```

- [ ] **Step 2: Extend the dispatch chain**

In `dispatch_settings_key`, add a pending-delete pass before the compose-mode pass:

```rust
let pending_delete_active = read_pending_delete(snapshot).is_some();
let pending_delete_scope = ox_path::oxpath!("settings", "_pending_delete");
let compose_active = read_compose_buffer(snapshot).is_some();
let compose_scope = ox_path::oxpath!("settings", "_compose_new_account");
let edit_mode_active = read_edit_mode(snapshot);
let edit_scope = ox_path::oxpath!("settings", "_edit_mode");

let cmd_id = if pending_delete_active {
    bindings.lookup(screen, &pending_delete_scope, mode, key)
} else {
    None
}
.or_else(|| {
    if compose_active {
        bindings.lookup(screen, &compose_scope, mode, key)
    } else {
        None
    }
})
.or_else(|| {
    if edit_mode_active {
        bindings.lookup(screen, &edit_scope, mode, key)
    } else {
        None
    }
})
.or_else(|| {
    read_focused(snapshot)
        .as_ref()
        .and_then(|focus| bindings.lookup(screen, focus, mode, key))
})
.or_else(|| bindings.lookup(screen, cursor, mode, key));
```

Update the comment block above the chain to list five passes (1. pending-delete, 2. compose, 3. edit, 4. focused-row, 5. page cursor).

- [ ] **Step 3: Add a unit test**

```rust
#[test]
fn pending_delete_routes_to_pending_delete_scope_when_set() {
    let mut cmds = CommandRegistry::new();
    cmds.register(Box::new(WriteSentinel::new()));

    let mut bindings = BindingRegistry::new();
    bindings.register(BindingEntry {
        screen: Screen::Settings,
        scope: ox_types::BindingScope::Exact(oxpath!("settings", "_pending_delete")),
        mode: None,
        key: key_char('y'),
        command_id: cmd_id("test.sentinel"),
    });

    let renderers = RendererRegistry::new();
    let mut reader = LocalConfig::default();
    reader
        .write(
            &oxpath!("ui", "settings", "pending_delete"),
            Record::parsed(Value::String("alpha".into())),
        )
        .unwrap();

    let writes = dispatch_settings_key(
        &mut reader,
        Screen::Settings,
        &oxpath!("settings", "index"),
        None,
        &key_char('y'),
        &cmds,
        &bindings,
        &renderers,
    );

    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].path, oxpath!("ui", "sentinel"));
}
```

- [ ] **Step 4: Run dispatch tests**

```
cargo test -p ox-cli --lib settings::dispatch::tests
```

Expected: PASS.

### Sub-task 1.4: Renderer reads pending_delete and emits the banner (gated)

**File:**
- Modify: `crates/ox-cli/src/settings/renderers/index.rs`

For Commit A, the renderer reads `pending_delete` and emits a banner ONLY when `Some(_)`. When None, no banner — the existing modal renderer continues to handle the confirmation UI for now (Commit B drops the modal).

- [ ] **Step 1: Add the banner emission**

In `index.rs::render`, after the affordance insertion logic from Phase 3, add:

```rust
// Pending-delete confirmation banner. Emitted as a ListItem prepended
// to the items vector when ui/settings/pending_delete is Some(name).
// Decoration only — focus: None; j/k skips it.
let pending: Option<String> = read_typed(
    ctx.data,
    &oxpath!("ui", "settings", "pending_delete"),
);
if let Some(name) = pending {
    let banner = ListItem {
        primary: format!("Delete '{}'? y / n", name),
        primary_spans: None,
        secondary: None,
        badge: None,
        focus: None,
    };
    items.insert(0, banner);
    selected = selected.map(|s| s + 1);
}
```

The banner sits at the top of the rendered list (index 0). Selection bumps by 1 unconditionally.

- [ ] **Step 2: Run the renderer tests**

```
cargo test -p ox-cli --lib settings::renderers::index::tests
```

Expected: PASS — existing tests don't seed `pending_delete` so the new code path is dead.

### Sub-task 1.5: Commit A

- [ ] **Step 1: Run lib + e2e + clippy**

```
cargo test -p ox-cli --lib
cargo test -p ox-cli --test settings_e2e
cargo clippy -p ox-cli --all-targets -- -D warnings
```

Expected: all PASS.

- [ ] **Step 2: Commit**

```
git add -u
git commit -m "feat(settings): add pending-delete infrastructure (dormant)

Adds the two accounts.confirm.{delete, cancel} commands; bindings
at the settings/_pending_delete synthetic scope (y → confirm,
n / Esc → cancel); the dispatcher's pending-delete pass that
consults ui/settings/pending_delete before the compose-mode and
edit-mode passes; renderer logic that prepends an inline
confirmation banner when pending_delete is Some.

All dormant — nothing writes pending_delete yet, so the dispatcher
pass never fires and the renderer's banner branch is dead. Commit
B flips the switch by rewiring accounts.delete_confirm and
deleting the modal renderer + bindings."
```

---

## Task 2: Commit B — switch substrate, drop modal + dead code

### Sub-task 2.1: Rewrite `accounts.delete_confirm`

**File:**
- Modify: `crates/ox-cli/src/settings/commands/account_model.rs`

The current `accounts.delete_confirm` writes cursor to `settings/accounts/_delete` and clears focused/edit_mode/edit_field_path. After Phase 4, it writes `Some(<name>)` to `pending_delete`.

- [ ] **Step 1: Replace the `command!` block body**

```rust
command! {
    struct_name: AccountsDeleteConfirm,
    id: "accounts.delete_confirm",
    title: "Delete Connection…",
    description: "Open the delete-confirmation banner for the selected Connection.",
    screen: Screen::Settings,
    cursor: Some(oxpath!("settings", "accounts")),
    run: |snap, _ctx| accounts_delete_confirm(snap),
}
```

Add the helper:

```rust
fn accounts_delete_confirm(data: &mut dyn Reader) -> Vec<Write> {
    let name = match read_selected_account(data) {
        Some(n) => n,
        None => return Vec::new(),
    };
    vec![Write {
        path: oxpath!("ui", "settings", "pending_delete"),
        record: Record::parsed(Value::String(name)),
    }]
}
```

- [ ] **Step 2: Replace the existing test**

In account_model.rs's tests, the existing `accounts_delete_confirm_writes_delete_cursor_and_isolates_input_scope` test asserts the old shape. Replace:

```rust
#[test]
fn accounts_delete_confirm_writes_pending_delete_when_selected() {
    let mut snap = SettingsSnapshot::empty();
    select_account(&mut snap, "alpha");
    let writes = run_cmd(&AccountsDeleteConfirm::new(), &mut snap);
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].path, oxpath!("ui", "settings", "pending_delete"));
    match &writes[0].record {
        Record::Parsed(Value::String(s)) => assert_eq!(s, "alpha"),
        other => panic!("expected pending_delete = Some(\"alpha\"); got {other:?}"),
    }
}

#[test]
fn accounts_delete_confirm_inert_without_selection() {
    let mut snap = SettingsSnapshot::empty();
    let writes = run_cmd(&AccountsDeleteConfirm::new(), &mut snap);
    assert!(writes.is_empty());
}
```

### Sub-task 2.2: Delete `AccountsDelete`, `AccountsCancel`, and the modal renderer

**File:**
- Modify: `crates/ox-cli/src/settings/commands/account_model.rs` — delete `AccountsDelete` command + `accounts_delete` helper + tests; delete `AccountsCancel` command + tests.
- Delete: `crates/ox-cli/src/settings/renderers/overlay_delete_account.rs`.
- Modify: `crates/ox-cli/src/settings/renderers/mod.rs` — drop the `overlay_delete_account` mod declaration + its registration.

- [ ] **Step 1: Delete `AccountsDelete` + `accounts_delete`**

In account_model.rs, delete:
- The `AccountsDelete` `command!` block.
- The `accounts_delete` helper function.
- The registration line `reg.register(Box::new(AccountsDelete::new()));`.
- The tests `accounts_delete_writes_null_to_canonical_account_path_when_selected` and `accounts_delete_inert_without_selection`.

The y-key in pending-delete mode now routes to `accounts.confirm.delete`, which has its own helper `accounts_confirm_delete`. The old `accounts.delete` command is unreachable.

- [ ] **Step 2: Delete `AccountsCancel`**

In account_model.rs, delete:
- The `AccountsCancel` `command!` block.
- The registration line `reg.register(Box::new(AccountsCancel::new()));`.
- The test `accounts_cancel_returns_to_accordion_index`.

The command was used only by the deleted `_new` and `_delete` modal cursor scopes. After Phase 4 deletes the `_delete` scope, no callers remain.

- [ ] **Step 3: Delete `overlay_delete_account.rs`**

```
git rm crates/ox-cli/src/settings/renderers/overlay_delete_account.rs
```

- [ ] **Step 4: Drop the registration in `renderers/mod.rs`**

In `crates/ox-cli/src/settings/renderers/mod.rs`, delete:
- The `pub mod overlay_delete_account;` line.
- The `overlay_delete_account::register(reg);` call in `register_all`.

The `_delete` cursor scope no longer has a renderer. Since Commit B also deletes its bindings (Sub-task 2.3) and `accounts.delete_confirm` no longer writes the cursor, nothing routes the user to that scope. Defensive deletion.

### Sub-task 2.3: Delete the `_delete` cursor scope bindings

**File:**
- Modify: `crates/ox-cli/src/settings/bindings.rs`

- [ ] **Step 1: Delete `register_account_delete`**

In `crates/ox-cli/src/settings/bindings.rs`, delete the entire `register_account_delete` function (around lines 212-235) and the `register_account_delete(reg);` call in the top-level `register` function.

- [ ] **Step 2: Delete the binding test**

The test `account_delete_y_resolves_to_delete` (around line 674) asserts the old `accounts.delete` binding at the `_delete` cursor scope. Delete it. The new pending-delete binding (`y` at `_pending_delete` scope → `accounts.confirm.delete`) is implicitly tested by Phase 3's pattern; if you want explicit coverage, add a sibling test pinning the new binding.

### Sub-task 2.4: Drop the cursor-write from `AccountDeleteCleanupSubscription`

**File:**
- Modify: `crates/ox-gate/src/subscriptions/account_delete.rs`

The subscription currently writes `cursor → settings/accounts` after handling a delete. With the modal gone, the user never left `settings/index`. The cursor write is unnecessary and would actually move the user away from a renderable cursor.

- [ ] **Step 1: Delete the cursor write**

Find the line in the cleanup body that writes the cursor:

```rust
writes.push(write_path(
    &oxpath!("ui", "settings", "cursor"),
    &oxpath!("settings", "accounts"),
));
```

Delete it. If `write_path` is no longer used elsewhere in the file, drop it from the imports too.

Update the doc comment near the cursor-write reference (in the module-level docs and in any comment near the cleanup body) to remove "pops the cursor back to the (modal-era) accounts page" or similar — the cursor isn't being touched anymore.

- [ ] **Step 2: Update the test**

The test `cleanup_pops_cursor_back_to_accounts_list` (or whatever it's called) asserts the cursor write. Either:
- Delete it (the behavior is gone), OR
- Replace with a test asserting the cleanup body does NOT write the cursor (safety guard against regression).

The latter is more useful as a regression guard:

```rust
#[test]
fn cleanup_does_not_touch_cursor() {
    let mut reader = InMemoryReader::new();
    populate_anthropic_account(&mut reader, "alpha", "sk-test");
    let writes = drive(&mut reader, "alpha");
    assert!(
        !writes.iter().any(|w| w.path == oxpath!("ui", "settings", "cursor")),
        "cleanup must not touch the cursor; got {writes:?}"
    );
}
```

### Sub-task 2.5: Renderer banner unconditional

The Commit-A banner code is gated `if let Some(name) = pending`. After Commit B, that's still the right shape — the banner should appear ONLY when pending_delete is set. No change to the renderer logic.

(This sub-task exists for symmetry with Phase 3's renderer change; Phase 4's renderer logic is correct as written in Commit A.)

### Sub-task 2.6: Build, test, commit

- [ ] **Step 1: Build**

```
cargo build -p ox-cli
cargo build -p ox-gate
```

Expected: PASS.

- [ ] **Step 2: Run tests + clippy**

```
cargo test -p ox-cli --lib
cargo test -p ox-gate --lib
cargo test -p ox-cli --test settings_e2e
cargo clippy --workspace --all-targets -- -D warnings
```

Expected: PASS.

The e2e `delete_account_flow` test currently drives `d → y` through the modal cursor scope. After Commit B, `d` writes `pending_delete = Some(<name>)`, the dispatcher routes `y` through the `_pending_delete` scope, and `accounts.confirm.delete` writes Null to the canonical path. The subscription's cleanup runs as before. End state is the same. The test should pass without changes; if it doesn't, update the assertions.

- [ ] **Step 3: Commit**

```
git add -u
git commit -m "feat(settings): delete-confirm lifts into pending_delete mode state

accounts.delete_confirm now writes Some(<name>) to
ui/settings/pending_delete instead of moving the cursor to a modal
scope. The dispatcher's pending-delete pass (added in Commit A)
routes y/n/Esc through the new accounts.confirm.{delete, cancel}
commands. y writes Null to config/gate/accounts/<name> directly;
the AccountDeleteCleanupSubscription's reactive observer (Phase 2)
handles side-data cleanup as before.

Deletes the modal infrastructure: overlay_delete_account.rs
renderer, the _delete cursor scope's bindings, the AccountsDelete
command (its body folded into accounts.confirm.delete), and the
AccountsCancel command (no remaining callers — the _new modal was
deleted in the inline-new-connection branch; the _delete modal is
gone now).

The cleanup subscription's cursor write is dropped — with the
modal gone, the user never left settings/index. Cleanup body now
contains only side-data deletion and conditional selection clear."
```

---

## Task 3: Final verification

- [ ] **Step 1: Full workspace test run**

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
grep -rn '_delete"\|accounts\.delete\b\|accounts\.cancel\|overlay_delete_account\|AccountsDelete\b\|AccountsCancel' crates/ 2>/dev/null
```

Expected: zero hits in source code (`accounts.confirm.delete` is OK; `accounts.delete_confirm` is OK; `AccountDeleteCleanup` is OK). Hits in tests asserting absence (e.g. "no `_delete` cursor write") are fine.

- [ ] **Step 4: Smoke-test in the TUI**

Ask the user to:

1. Open settings.
2. Create or select an account.
3. Press `d` — confirm an inline banner appears at the top: "Delete '<name>'? y / n".
4. Press `n` — confirm the banner dismisses cleanly; account still present.
5. Press `d` again, press `Esc` — same as `n`; banner dismisses.
6. Press `d`, press `y` — confirm the account is deleted; banner gone; focus reasonable; no error banner.
7. Verify the API key is also gone (`secret/keys/<name>`) and the synthesized provider record (`config/gate/providers/<name>`) is gone.

If anything misbehaves, it's a regression — investigate before declaring Phase 4 complete.

---

## Self-review checklist

- [x] `accounts.delete_confirm` writes `pending_delete` instead of moving cursor (Sub-task 2.1).
- [x] Two new commands `accounts.confirm.{delete, cancel}` at `_pending_delete` synthetic scope (Sub-tasks 1.1, 1.2).
- [x] Dispatcher routes y/n/Esc through pending-delete scope when active (Sub-task 1.3).
- [x] Renderer prepends inline banner when pending_delete is set (Sub-task 1.4).
- [x] `overlay_delete_account.rs` deleted (Sub-task 2.2).
- [x] `_delete` cursor scope bindings deleted (Sub-task 2.3).
- [x] `AccountsDelete` and `AccountsCancel` commands deleted; their tests deleted (Sub-task 2.2).
- [x] `AccountDeleteCleanupSubscription` cursor write dropped (Sub-task 2.4).
- [x] Workspace green + clippy clean + grep clean (Task 3).

Spec requirements not addressed by this plan (intentionally deferred):
- `View::Modal`, `dim_buffer`, and the modal rendering primitives stay (per spec §6.3). Phase 4 retires the cursor-scope-driven modal renderer; it does NOT retire the visual modal pattern.
