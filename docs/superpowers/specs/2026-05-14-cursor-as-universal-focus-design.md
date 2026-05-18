# Cursor as universal focus authority

**Date:** 2026-05-14
**Status:** Design — pending implementation plan
**Crates touched:** `ox-cli` (dispatcher, every compound widget, IndexRenderer, tests)

## 1. Summary

`ui/settings/focused` (the cursor) is currently the page-level focus pointer. Every compound widget (compose, manual-model, pending-delete, edit-mode) carries its OWN focus + mode state at separate paths — `new_account/active`, `manual_model/stage`, `pending_delete: Option<AccountName>`, `edit_mode + edit_field_path`. The dispatcher reads four discriminators to assemble its scope path. The cursor and the compound widgets' focuses can disagree about "where the user is."

This design unifies everything into the cursor. The cursor is THE focus authority for the screen — for rows, for compound widgets, for sub-elements of compound widgets. Mode discriminators retire. `compute_scope_path` becomes `cursor.ancestors()`. Compound widgets identify themselves by virtue of being IN the cursor's ancestry.

The design is recursive: cursor path depth = focus tree depth. Whole-widget focus is cursor at widget root; sub-element focus is cursor at widget root + sub-element. Same mechanism for all widgets.

## 2. Goals & non-goals

### Goals

- `ui/settings/focused` is the single source of truth for "what is currently focused" — applies to page rows, compound-widget roots, and compound-widget sub-elements alike.
- `compute_scope_path` becomes `cursor.ancestors()`. No reads from discriminator paths.
- Mode discriminators (`new_account/active`, `manual_model/stage`, `pending_delete`, `edit_mode + edit_field_path`) retire — active mode is implicit in cursor's path segments.
- Every compound widget follows the same pattern: open writes cursor to widget's path; commit writes cursor to the target's next position; cancel restores the saved-on-open cursor.
- Renderer's `cursor_for_lists` workaround retires.
- All existing scope ordering / bindings / dispatch tests migrate to assert cursor positions rather than discriminator flags.

### Non-goals

- Renaming data paths. `ui/settings/new_account/*` (compose buffers), `ui/settings/edit_buffer`, etc. stay where they are. Only the cursor moves; data paths are orthogonal. Cohesion-rename is a future refactor.
- New View enum variants. No widening of ListItem / FormRow.
- Touching inbox/threads renderers.
- A general "FocusController" abstraction. Each widget's open/cancel/commit commands directly manipulate cursor; no framework primitive needed.

## 3. Cursor path encodings

Synthetic widget paths under `settings/_<widget_name>/`. Sub-elements (when present) are additional path segments. Data associated with the widget stays at its existing `ui/settings/<existing_path>` location.

| Widget                    | Cursor path                                  | Sub-element segment | Data location (unchanged) |
|---------------------------|----------------------------------------------|---------------------|---------------------------|
| Compose form, whole-form  | `settings/_compose_form`                     | (none)              | `ui/settings/new_account/*`  |
| Compose form, Name field  | `settings/_compose_form/name`                | field id            | `ui/settings/new_account/*`  |
| Manual-model, Id stage    | `settings/_manual_model/id`                  | stage id            | `ui/settings/manual_model/*` |
| Pending-delete confirm    | `settings/_confirm_delete`                   | (none)              | `ui/settings/pending_delete/target_account` (renamed from the value-carrying flag) |
| Edit-mode on a field      | `settings/_edit`                             | (none)              | `ui/settings/edit_mode/{target_path, buffer}` (renamed) |

Per row, cursor stays at the row's path (e.g., `settings/accounts/alpha`) when no compound widget is active — same as today.

## 4. The dispatcher's `compute_scope_path`

```rust
fn compute_scope_path(snap: &mut dyn Reader) -> Vec<BindingScope> {
    let cursor = read_cursor(snap);
    match cursor {
        Some(path) => path
            .ancestors()
            .map(BindingScope::Exact)
            .collect(),
        None => vec![],
    }
}
```

`path.ancestors()` walks `[root_first_segment, root + second, ..., full_path]`. Implement via the existing `Path` API — likely already supported.

No discriminator reads. No conditional scope insertion. The mode is implicit in which scopes are on the cursor's ancestor chain.

## 5. Bindings under cursor-as-focus

Per-scope binding registration. Each scope on a possible cursor path can host bindings at any phase.

### Compose form

- `Exact(settings/_compose_form)`:
  - Capture: Esc → `accounts.compose.cancel`; Tab/Down → `accounts.compose.focus_next`; BackTab/Up → `accounts.compose.focus_prev`.
  - Bubble: Enter → `accounts.compose.commit`.
- `Exact(settings/_compose_form/name)`, `Exact(settings/_compose_form/endpoint)`, `Exact(settings/_compose_form/key)` (text fields):
  - Target: printable ASCII → `accounts.compose.insert_char`; Backspace → `accounts.compose.delete_back`.
- `Exact(settings/_compose_form/protocol)`, `Exact(settings/_compose_form/auth)` (selector fields):
  - Target: h/Left → `accounts.compose.cycle_back`; l/Right → `accounts.compose.cycle_forward`.

Per-field registration via helper (`register_compose_text_field(reg, "name")`, `register_compose_selector_field(reg, "protocol")`).

### Manual-model

Analogous: form-level bindings at `Exact(settings/_manual_model)`; per-stage bindings at `Exact(settings/_manual_model/<stage>)`.

### Pending-delete

- `Exact(settings/_confirm_delete)`:
  - Capture: Esc → `accounts.confirm.cancel`.
  - Target: y → `accounts.confirm.delete`; n → `accounts.confirm.cancel`.

### Edit-mode

- `Exact(settings/_edit)`:
  - Capture: Esc → `edit.cancel`.
  - Bubble: Enter → `edit.commit`.
  - Target: printable → `edit.insert_char`; Backspace → `edit.delete_back`.

The current `_compose_field_text` / `_compose_field_selector` / `_manual_model/<stage>` scopes retire — replaced by per-element scopes on the cursor path.

## 6. Open / commit / cancel commands

### Pattern

Each compound widget follows the same shape:

```rust
fn <widget>_open(data) -> Vec<Write> {
    let cursor_before = read_cursor(data);  // for restore
    let mut writes = vec![
        Write { path: <widget>_cursor_saved_path, record: path_to_value_or_null(cursor_before) },
        Write { path: oxpath!("ui", "settings", "focused"), record: path_to_value(&<widget>_focus_path()) },
        // ... plus widget-specific data writes (initialize buffers, target_account, etc.)
    ];
    writes
}

fn <widget>_cancel(data) -> Vec<Write> {
    let saved = read_typed::<Path>(data, &<widget>_cursor_saved_path);
    vec![
        Write { path: <widget>_data_root, record: Record::parsed(Value::Null) },  // cascade clear
        Write { path: oxpath!("ui", "settings", "focused"), record: path_to_value(&saved.unwrap_or(default)) },
    ]
}

fn <widget>_commit(data) -> Vec<Write> {
    // ... materialize the widget's effect ...
    let target_cursor = <widget>_target_after_commit(data);  // e.g., new account's row
    let mut writes = build_materialization_writes(data);
    writes.push(Write { path: oxpath!("ui", "settings", "focused"), record: path_to_value(&target_cursor) });
    writes.push(Write { path: <widget>_data_root, record: Record::parsed(Value::Null) });
    writes
}
```

### Compose

- `open`: cursor moves to `settings/_compose_form/name`. Saves `cursor_saved` for restore. Initializes draft buffers.
- `focus_next/prev`: reads cursor, maps cursor's leaf to AccountField, computes next/prev field, writes new cursor.
- `commit`: T12's existing target writes (`ui/settings/focused = settings/accounts/<path_id>`) take over. The compose data subtree gets Null-cascaded.
- `cancel`: reads `cursor_saved`, Null-cascades compose data, restores cursor.

### Manual-model

- `open`: cursor moves to `settings/_manual_model/<initial_stage>`. Saves prior cursor.
- `commit_stage`: cursor moves to next stage's path OR to finalize.
- `commit`: cursor moves to the new model's row; manual_model data cleared.
- `cancel`: restores saved cursor; manual_model data cleared.

### Pending-delete

- `open` (was `accounts.delete_confirm`): cursor moves to `settings/_confirm_delete`. Target account stored at data path. Cursor_saved holds the previous cursor (the account row that was selected).
- `confirm` (was `accounts.confirm.delete`): cursor moves to the next account row (or accounts header if no accounts left). Pending-delete data cleared. Delete writes emitted.
- `cancel`: restores saved cursor. Pending-delete data cleared.

### Edit-mode

- `open` (was `field.edit_start` or similar — locate via grep): cursor moves to `settings/_edit`. Target field path + initial buffer stored at data paths. Cursor_saved holds the field row's path.
- `commit`: writes buffer to the target field's data path; restores cursor to the field row.
- `cancel`: restores cursor to the field row; edit data cleared.

## 7. Renderer cleanup

- `cursor_for_lists = if compose_active { None } else { cursor.as_ref() }` removed. Cursor naturally doesn't match account rows during compose.
- `read_focused_field` helper retires. `View::Form.focused` derived from cursor via `cursor_to_compose_field` helper.
- Selector kind detection (`field_kind`): for compose, the cursor's leaf segment IS the field's identity; kind is derived via `field_kind`.

## 8. Tests

### Migration

Every test that today seeds or asserts:
- `ui/settings/new_account/active`
- `ui/settings/new_account/focused_field`
- `ui/settings/manual_model/stage`
- `ui/settings/pending_delete`
- `ui/settings/edit_mode` or `edit_field_path`

migrates to seeding/asserting `ui/settings/focused` (the cursor) at the appropriate path.

### New tests

- `compute_scope_path_is_cursor_ancestors`: cursor at `settings/_compose_form/name` → scope path is `[settings, settings/_compose_form, settings/_compose_form/name]`.
- `no_mode_active_when_cursor_outside_widget_namespace`: cursor at `settings/accounts/alpha` → no `_compose_form` / `_manual_model` / etc. scopes on the path.
- `compose_open_writes_cursor_to_name_field`: pressing `a` puts cursor at `settings/_compose_form/name`.
- `pending_delete_open_writes_cursor_to_confirm_delete`: pressing `d` puts cursor at `settings/_confirm_delete`.
- Similar opens for edit-mode and manual-model.
- Cancel for each restores `cursor_saved`.
- Commit for each routes cursor to the appropriate post-commit target.

## 9. Risks

- **Big test churn.** Every dispatcher / compound-widget / rendering test that relied on mode discriminators needs migration.
- **Save/restore correctness.** Every open must save; every cancel must read save BEFORE the data root's Null write (read-then-emit is safe at command-build time; writes apply later). Need to be careful about ordering.
- **Per-element binding registration scales.** Compose has 5 fields, 3 of which need ~96 printable bindings each = ~288 extra entries vs today's 96 at `_compose_field_text`. Lookup is still O(scope-binding-count) but each `bind_target` call adds an entry. Use helpers to factor.
- **Path API.** `Path::ancestors()` may or may not exist on `structfs_core_store::Path`. If not, add it (cheap — walk from root) or compute inline.
- **Mutual exclusion.** Currently the dispatcher debug-asserts ≤1 compound widget active. Under cursor-as-focus, mutual exclusion is enforced by which path the cursor is at — cursor can only be in one widget's namespace at a time. Debug assert can retire or be reframed as "cursor is in a known namespace."

## 10. Plan

Single big task with sub-commits. Sub-commits aligned to logical units:

1. **Add path-walking helpers**: `Path::ancestors()` (or a free function `path_ancestors(&Path)`); `field_focus_path` and `cursor_to_field` for compose.
2. **Migrate compose**: open/focus/commit/cancel cursor writes; retire `new_account/active` discriminator + `focused_field` path; revert renderer workaround; per-field bindings registration.
3. **Migrate manual-model**: cursor-driven stage transitions; retire `manual_model/stage` discriminator; per-stage bindings registration matching the new scope shape.
4. **Migrate pending-delete**: cursor-driven open/cancel/confirm; retire `pending_delete: Option<AccountName>` flag (target moves to `ui/settings/pending_delete/target_account`).
5. **Migrate edit-mode**: cursor-driven open/cancel/commit; retire `edit_mode` flag.
6. **Refactor `compute_scope_path`** to cursor-ancestors-only. Retire all discriminator reads. Remove mutual-exclusion debug assert (or reframe).
7. **Test migrations + new invariant tests**: migrate every test that touches a mode discriminator; add new tests asserting cursor-driven behavior.
8. **Docs**: update `architecture.md` to describe pure cursor-as-focus.

Plan file: `docs/superpowers/plans/2026-05-14-cursor-as-universal-focus.md`.
