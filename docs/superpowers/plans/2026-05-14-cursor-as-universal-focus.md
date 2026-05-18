# Cursor as Universal Focus Authority — Implementation Plan

> **For agentic workers:** subagent-driven-development.

**Goal:** Unify all focus state into `ui/settings/focused`. The cursor's path encodes the full focus state (which row / widget / sub-element). `compute_scope_path = cursor.ancestors()`. Mode discriminators (`new_account/active`, `manual_model/stage`, `pending_delete`, `edit_mode + edit_field_path`) retire.

**Spec:** `docs/superpowers/specs/2026-05-14-cursor-as-universal-focus-design.md`.

This is a substantial refactor touching dispatcher, every compound widget, the renderer, and many tests. Sub-commits land logical units; the final commit retires the last discriminator.

---

## Task CF-1: Path ancestors + compose cursor migration

**Files:**
- Possibly modify: `crates/ox-cli/` or wherever path helpers live (`Path::ancestors` if missing).
- Modify: `crates/ox-cli/src/settings/commands/account_model.rs` (compose commands).
- Modify: `crates/ox-cli/src/settings/bindings.rs` (compose bindings reshape).
- Modify: `crates/ox-cli/src/settings/dispatch.rs` (compose handling in `compute_scope_path`).
- Modify: `crates/ox-cli/src/settings/renderers/index.rs` (revert `cursor_for_lists`; `compose_form_view` derives focused from cursor).

**Steps:**

1. **Path ancestors helper.** Check if `structfs_core_store::Path` has `ancestors()` or `iter_ancestors()` or similar. If not, add a free helper `path_ancestors(&Path) -> Vec<Path>` that returns `[len-0, len-1, ..., len-N]` of progressively-longer prefixes ending at the full path.

2. **Add `field_focus_path` + `cursor_to_field`** in `account_model.rs`:

```rust
pub(crate) fn field_focus_path(f: AccountField) -> Path {
    let name = match f {
        AccountField::Name => "name",
        AccountField::Protocol => "protocol",
        AccountField::Endpoint => "endpoint",
        AccountField::Auth => "auth",
        AccountField::Key => "key",
    };
    oxpath!("settings", "_compose_form", PathComponent::try_new(name).unwrap())
}

pub(crate) fn cursor_to_field(cursor: &Path) -> Option<AccountField> {
    // Cursor at settings/_compose_form/<name> → Some(field).
    // Anything else → None.
}
```

3. **Reshape `accounts.compose.open`**: save previous cursor, write cursor to `field_focus_path(Name)`, no longer write `new_account/active` or `focused_field`. The active mode is now implicit in cursor being under `settings/_compose_form/`.

4. **Reshape `accounts.compose.focus_next/prev`**: read cursor, map via `cursor_to_field`, write new cursor (no `focused_field` writes).

5. **Reshape `accounts.compose.cancel`**: read `cursor_saved`, emit Null at `new_account` subtree, restore cursor to saved value (or default to `settings/accounts`).

6. **`accounts.compose.commit`**: already writes cursor to new account row. Stays unchanged except for any `focused_field` writes (none expected).

7. **Reshape `compose_form_view`** in `index.rs`: derive `View::Form.focused: Option<usize>` from cursor via `cursor_to_field`. Default to None if cursor is not at a field path.

8. **Reshape compose bindings** in `bindings.rs`:
   - At `Exact(settings/_compose_form)`: Esc (Capture), Tab/Down (Capture → focus_next), BackTab/Up (Capture → focus_prev), Enter (Bubble → commit).
   - Per text field at `Exact(settings/_compose_form/<field>)`: printable + Backspace (Target).
   - Per selector field at `Exact(settings/_compose_form/<field>)`: h/l/Left/Right (Target).
   - Drop `_compose_field_text` and `_compose_field_selector` scopes — they're no longer on the cursor path. Their bindings move to per-field exact scopes.
   - Use helper functions to factor per-text-field and per-selector-field registration.

9. **Refactor `compute_scope_path`** (start the migration; complete it across all widgets in CF-6):
   - Add a cursor-ancestors-based path. For compose-related cursor positions, this REPLACES the existing `if compose_active { ... }` branch.
   - During this task, keep the other discriminator branches (`pending_delete`, `manual_model_stage`, `edit_mode_active`) until later tasks migrate them.

10. **Revert renderer workaround**: drop the `cursor_for_lists = if compose_active { None } else { cursor.as_ref() }` line. Cursor naturally doesn't match account rows when at form-field path.

11. **Migrate compose tests**: every existing test that seeds `new_account/active` or `focused_field` switches to seeding cursor at the appropriate path. Assertions about writes to `focused_field` switch to assertions about writes to `ui/settings/focused`.

12. **Add new tests:**
    - `cursor_at_compose_form_name_on_open`.
    - `compose_cancel_restores_saved_cursor`.
    - `compute_scope_path_includes_compose_form_ancestor_when_cursor_at_field`.
    - `no_compose_scope_when_cursor_at_account_row`.

13. **Run suite**: lib + e2e green. Reproducer green.

14. **Commit**: `compose: cursor-driven focus; retire new_account/active and focused_field`.

---

## Task CF-2: Manual-model cursor migration

Mirror of CF-1 for manual-model. Cursor paths: `settings/_manual_model/<stage>`. Per-stage bindings move to `Exact(settings/_manual_model/<stage>)`. Retire `ui/settings/manual_model/stage` flag.

Detailed steps similar to CF-1 but for manual-model commands (`models.compose_manual.*`).

Commit: `manual-model: cursor-driven stage transitions; retire stage discriminator`.

---

## Task CF-3: Pending-delete cursor migration

Cursor moves to `settings/_confirm_delete`. Target account moves from being the value of the flag (`pending_delete: Option<AccountName>`) to a separate data path `ui/settings/pending_delete/target_account: String`.

- `accounts.delete_confirm`: writes cursor to `settings/_confirm_delete`, target_account, and cursor_saved.
- `accounts.confirm.delete`: reads target_account, emits delete writes, restores cursor to next account (or accounts header), clears pending_delete data.
- `accounts.confirm.cancel`: restores saved cursor; clears pending_delete data.

Bindings move from `Exact(settings/_pending_delete)` to `Exact(settings/_confirm_delete)`. Phases preserved.

Migrate tests. Add new ones.

Commit: `pending-delete: cursor-driven; retire pending_delete value flag`.

---

## Task CF-4: Edit-mode cursor migration

Cursor moves to `settings/_edit`. Target field path moves to `ui/settings/edit_mode/target_path: Path`. Buffer at `ui/settings/edit_mode/buffer: String`.

- Edit-open command: writes cursor to `settings/_edit`, target_path, initial buffer, cursor_saved.
- `edit.commit`: writes buffer to target_path; restores cursor to target_path (the field row).
- `edit.cancel`: restores saved cursor.

Migrate tests. Add new ones.

Commit: `edit-mode: cursor-driven; retire edit_mode discriminator`.

---

## Task CF-5: `compute_scope_path` collapses to cursor ancestors

Now that every compound widget moves cursor, `compute_scope_path` no longer needs any discriminator reads.

Replace its body with:

```rust
fn compute_scope_path(snap: &mut dyn Reader) -> Vec<BindingScope> {
    let cursor = read_cursor(snap);
    match cursor {
        Some(path) => path_ancestors(&path)
            .into_iter()
            .map(BindingScope::Exact)
            .collect(),
        None => vec![],
    }
}
```

Remove the mutual-exclusion debug_assert (or reframe it as "cursor is in at most one widget namespace at a time" — actually that's automatically true since cursor is a single path).

Migrate tests:
- The 10 scope_path ordering tests from S-tier-3: update to assert cursor's ancestor chain instead of mode-discriminator outputs.
- Any test that asserts compound-widget scopes are present: convert to seeding cursor at a path under that widget's namespace.

Commit: `dispatch: compute_scope_path is cursor.ancestors(); mode discriminators retired`.

---

## Task CF-6: Documentation + final verification

Update `docs/ui_framework/architecture.md`:
- "Hierarchical dispatch" section: describe cursor-as-focus principle. Scope path is the cursor's ancestor chain.
- "Modeling state: modes" section: mode is encoded in cursor's path segments. Flags + value paths for modes retire — the cursor's location is the mode indicator.

Final verification:
- Full lib suite green.
- Full e2e suite green.
- Reproducer green.
- `grep -rn "new_account/active\|manual_model/stage\|edit_mode_active\|pending_delete: Option" crates/ox-cli/` returns no matches (or only matches in deletion lines).

Commit: `docs: cursor as universal focus authority; mode discriminators retired`.

---

## Self-review checklist (run at end of each task)

- [ ] Mode discriminator path is no longer written by any command.
- [ ] Mode discriminator path is no longer read by any helper.
- [ ] Cursor at the widget's namespace IS the mode indicator.
- [ ] Open command writes cursor; cancel restores saved cursor; commit writes target cursor.
- [ ] Tests that seeded the old discriminator have been migrated to seed cursor.
- [ ] `compute_scope_path` reads only the cursor (after CF-5).
- [ ] No debug_assert on mutual-exclusion (retires in CF-5; cursor is by-construction in one namespace).
- [ ] Renderer's `cursor_for_lists` workaround is gone (retired in CF-1).
- [ ] `compute_scope_path` body is `cursor.ancestors()` (after CF-5).
- [ ] `architecture.md` reflects the new model.
