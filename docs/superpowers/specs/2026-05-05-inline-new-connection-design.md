# Inline new-connection row (replacing the modal)

## Background

The settings screen currently opens a `View::Modal` overlay when the user presses `a`
to add a connection. The overlay collects only a `Name`, then writes a
`CreateAccountRequest` and pops the cursor to `settings/accounts/_detail`. Three
visible bugs surface:

1. `dim_buffer` dims the whole modal area before the foreground draws over it.
   Ratatui's `Style::patch` semantics let the `DIM` modifier leak through empty
   cells inside the foreground frame, so the modal itself reads as inactive.
2. The form has a single `Name` row; Protocol/Endpoint/Auth/Key are gathered
   later via the accordion's inline edit machinery.
3. After submit, `account_create.rs:123-126` drives the cursor to
   `settings/accounts/_detail`, but no renderer is registered there. The
   fallback emits `unknown cursor: settings/accounts/_detail`.

The accordion already has everything we need to do this inline: a single
visible-rows enumeration, an `edit_mode`/`edit_buffer`/`edit_field_path` state
machine with `edit.insert_char`/`edit.delete_back`/`edit.commit`/`edit.cancel`,
and a `decorate_row_label` path in the index renderer that overlays the live
buffer onto the focused row.

## Goal

Replace the modal with a synthetic ghost row at the top of the expanded
Accounts section, labelled `+ New connection`. Activating the row enters inline
edit mode in place; committing fires the existing `accounts.create` flow. The
delete-confirm modal is unaffected.

## Design

### Visible-rows ghost row

`visible_rows::enumerate` gains a new variant:

```rust
pub enum RowKind {
    // …existing variants…
    /// Synthetic ghost row that, when activated, opens the inline name
    /// prompt that ultimately materializes a new account.
    AccountAdd,
}
```

`append_account_rows` prepends one row when the Accounts entry is expanded:

- `path = settings/accounts/_new`
- `depth = 1`
- `label = "+ New connection"`
- `kind = AccountAdd`
- `expandable = false`, `expanded = false`
- `secondary = None`, `badge = None`

The ghost row only exists in the visible-rows projection; nothing renders the
path itself, so no renderer registration is needed.

### Activation (`a` key and Enter on ghost row)

`accounts.add` (bound to `a` under `Prefix(settings/accounts)`) becomes:

1. Add `settings/accounts` to the expanded set if not already present.
2. Set `ui/settings/focused_row = settings/accounts/_new` (the ghost row).
3. Seed inline edit state: `edit_field_path = settings/accounts/_new`,
   `edit_buffer = ""`, `edit_mode = true`.

`tree.activate` on a focused `RowKind::AccountAdd` row runs the same shape
(without step 1, since the ghost row is already focused).

Both paths reuse the same helper, so the keystroke entry point and the
explicit-Enter entry point produce identical writes.

### Renderer (label decoration)

`decorate_row_label` in `index.rs` already overlays the buffer onto whichever
row matches `edit_state.field_path`. Add an `AccountAdd` branch that produces
`"Name▸ <buffer>\u{258F}"` — same shape as the existing field branches. Glyph
column stays `"  "` (the row isn't expandable). Indent stays at the depth-1
two-space prefix.

When edit mode is off, the row renders as plain `+ New connection` with no
decoration.

### Commit (`edit.commit`)

`edit::commit` already routes by `RowKind` for AccountField / ModelField /
manual_model. Add an `AccountAdd` branch:

- Read `edit_buffer`. Empty/whitespace → return `Vec::new()` (silent reject,
  edit mode persists, matches manual_model id stage).
- Build `CreateAccountRequest { name }` and write it to
  `config/gate/accounts/_create_now` (the same trigger the modal used).
- Clear edit state (`edit_mode=false`, buffer=Null, field_path=Null).

`AccountCreateSubscription` keeps validating the name and rejecting via the
banner on failure.

### Subscription cursor change

`account_create.rs` currently writes
`ui/settings/cursor = settings/accounts/_detail`. Change it to:

- `ui/settings/focused_row = settings/accounts/<name>` (point at the row that
  just appeared).
- Drop the `ui/settings/cursor` write entirely; the page-level cursor stays at
  `settings/index` because the accordion never left it.
- Add `settings/accounts/<name>` to `ui/settings/expanded` so the user lands on
  an expanded row with its Protocol/Endpoint/Auth/Key fields visible — they can
  immediately Tab/`j` down and edit.
- The existing `ui/settings/accounts/selected` write stays.
- The existing null-write to `_create_now` stays.

### Esc / cancel

`edit.cancel` already clears edit state. With the ghost row in the visible
list, Esc dismisses edit mode but leaves the user focused on the ghost row,
ready to retry. No new code path.

## Code to delete

- `crates/ox-cli/src/settings/renderers/overlay_new_account.rs` (entire file).
- The `register` call for it in `renderers/mod.rs`.
- `register_account_new` in `bindings.rs` (the whole `_new` cursor scope and
  every binding inside it: `accounts.create`, `accounts.cancel` for `_new`,
  `accounts.new.insert_char`, `accounts.new.delete_back`, Backspace).
- The `AccountsCreate` command (`accounts.create`) — its run body folds into
  the `AccountAdd` arm of `edit::commit`.
- The `accounts.new.insert_char` and `accounts.new.delete_back` commands and
  their registration.
- The replay snapshot test for new-connection modal typing.

`accounts.cancel` itself stays (still wired into the delete-confirm modal).
`dim_buffer` stays (still used by the delete-confirm modal).

## Tests

New / updated tests:

- `visible_rows::tests`: ghost row appears as the first depth-1 row when
  Accounts is expanded; absent when collapsed.
- `tree::tests`: `tree.activate` on an `AccountAdd` row produces the
  edit-state writes (mirror of `activate_on_endpoint_field_enters_inline_edit_mode`).
- `account_model::tests`: `accounts.add` expands the Accounts section,
  focuses the ghost row, and enters edit mode (replacing the existing
  `accounts_add_writes_new_cursor_and_isolates_input_scope` test).
- `edit::tests`: `commit` on an `AccountAdd` row writes `CreateAccountRequest`
  to `_create_now` and clears edit state. Empty buffer → no-op.
- `account_create::tests`: subscription writes `focused_row` to the new
  account's path, expands the account row, and does not touch
  `ui/settings/cursor`.
- Snapshot/replay test for new-connection inline typing (replacement for the
  modal-typing test).

## Out of scope

- The delete-confirm modal stays as-is.
- The `dim_buffer` ratatui-style-patching bug stays unfixed (still affects the
  delete-confirm modal cosmetically). If we revisit modals later, fix it then.
- No changes to the post-creation field-edit flow; the user fills in
  Protocol/Endpoint/Auth/Key through the inline accordion, exactly as today.
