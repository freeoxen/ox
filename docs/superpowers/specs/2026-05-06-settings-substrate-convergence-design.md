# Settings substrate convergence — direct writes + modes-as-state

**Date:** 2026-05-06
**Status:** Design — pending implementation plan
**Crates touched:** `ox-cli`, `ox-gate`
**Related docs:** `docs/ui_framework.md`, `docs/ui_framework/architecture.md`

## 1. Summary

Brings the settings screen's runtime shape into alignment with the
three architectural commitments the rewritten UI framework docs
articulate:

1. **A write IS the action.** Subscriptions are reactive observers
   and async-only action triggers — never RPC translators for state
   changes the CLI can perform with a single write.
2. **Mode is state, not place.** Composing, confirming, and inline
   editing are values at named UI-state paths — not cursor scopes
   and not synthetic display rows.
3. **The display tree names only real things.** Synthetic affordances
   are renderer-side decorations reading UI-mode state, never rows in
   the visible-rows projection.

The day-one settings implementation predates these commitments and
contains transitional shapes that violate all three: `_create_now` /
`delete_now` sentinels in the data tree, `_new` / `_delete` cursor
scopes that are modes pretending to be pages, and `RowKind::AccountAdd`
/ `ModelEmptyState` / `ModelAddManual` synthetic rows in the
projection. This spec describes the migration that retires those
shapes.

The user-visible behavior does not change. The framework's substrate
becomes consistent with what its docs now claim.

## 2. Goals & non-goals

### Goals

- Every path under `config/` names either a real fact about the world
  or a per-instance async-action trigger (`…/<verb>_now`). No
  collection-level sentinels (`_create_now` is gone).
- Every path under `ui/` and `settings/` names either a real entity
  (an account, a model) or a UI-state value with semantic meaning. No
  synthetic-affordance identifier paths (`settings/accounts/_new`,
  `_delete`, `_empty`, `_add` are all gone).
- The `RowKind` enum drops `AccountAdd`, `ModelEmptyState`, and
  `ModelAddManual`. The visible-rows projection is a pure function of
  the data tree to real-thing rows.
- UI modes live at typed UI-state paths. The dispatcher routes
  through a mode-aware pass that consults these paths in priority
  order before row-keyed dispatch.
- Subscriptions are reduced to: reactive observers (Prefix watches,
  filtered by change shape) and async-only action triggers
  (PrefixSuffix on `_now`). No subscription is a sentinel translator.
- The `_`-prefix-name reservation rule (in `edit.commit` and
  `account_create.rs`) is unnecessary and removed.
- After this spec lands, the convergence note in `ui_framework.md`
  can be removed.

### Non-goals

- Renaming or restructuring data-tree paths beyond removing
  sentinels. `config/gate/accounts/<name>` stays. The TOML config
  format is unchanged.
- Pulling modes out of the broker into in-memory state. Path-based
  MVU is the substrate; modes live in the namespace.
- Introducing crate-level abstractions for "mode" or "subscription
  kind." Direct paths and direct writes only.
- The `config/save` action trigger and its subscription. Already in
  the right shape (async-only action; file IO).
- The `_request_exit` cross-component signal at
  `ui/settings/_request_exit`. Legitimate sentinel — no data-tree
  home for "please exit."
- Renaming `_edit_mode` cursor scope. The dispatcher uses it as a
  binding scope for printable-key routing during inline edits; it's
  a real binding scope, not a mode pretending to be a place.

## 3. Background

The settings screen redesign (2026-04-27) introduced path-based MVU
as the framework. Day-one implementations (Phases R + S) leaned on
sentinel-as-RPC and cursor-scope-as-mode patterns to ship the screen
without rebuilding the full substrate. Those patterns shipped working
features; they also shipped technical debt that the framework's docs
now explicitly forbid.

The inline new-connection branch (2026-05-05) replaced the new-account
modal with an inline ghost row, but stopped short of the substrate
refactor. It introduced `RowKind::AccountAdd` (a synthetic row),
relied on `_create_now` (a sentinel), and added a `_`-prefix
reservation rule to keep the synthetic ghost-row path from colliding
with real account names. A code review identified the convergence gap;
the framework docs were rewritten to describe the target architecture;
this spec describes the substrate work that closes the gap.

The framework docs are the contract this work is reviewed against.
Where the docs and the code currently disagree, the code converges
to the docs.

## 4. Target architecture

### 4.1 Data-tree paths

After this work:

| Path | Shape | Meaning |
|---|---|---|
| `config/gate/accounts/{name}` | `AccountConfig` | A write here creates the account; a `Null` write deletes it. |
| `config/gate/accounts/{name}/models` | `Vec<ModelInfo>` | Catalog. |
| `config/gate/accounts/{name}/test_status` | `AccountTestStatus` | Latest connectivity test outcome. |
| `config/gate/accounts/{name}/refresh_status` | `CatalogRefreshStatus` | Latest catalog refresh outcome. |
| `config/gate/accounts/{name}/validation_status` | `ValidationDiagnostics` | Per-field validation diagnostics. |
| `config/gate/accounts/{name}/test_now` | `Null` | Async trigger: run a connectivity test for this account. |
| `config/gate/accounts/{name}/refresh_now` | `Null` | Async trigger: refresh this account's catalog. |
| `config/gate/providers/{name}` | `ProviderConfig` | Endpoint + dialect + auth scheme. |
| `config/gate/completions/primary` | `CompletionRole` | (account, model) — the primary completion target. |
| `config/save` | `Null` | Async trigger: persist runtime config to disk. |
| `secret/keys/{name}` | `ApiKey` | Per-account API key (mounted separately). |

What's gone:

- `config/gate/accounts/_create_now` — replaced by direct writes to
  `config/gate/accounts/{name}`.
- `config/gate/accounts/{name}/delete_now` — replaced by `Null`
  writes to `config/gate/accounts/{name}`.

### 4.2 Display-tree paths

After this work:

| Path | Shape | Meaning |
|---|---|---|
| `ui/settings/cursor` | `Path` | Currently-displayed page. Page navigation only. |
| `ui/settings/_request_exit` | `bool` | Cross-component signal: switch screens. |
| `ui/settings/focused_row` | `Path` | Identifier path of focused row in the visible-rows projection. |
| `ui/settings/expanded` | `Vec<String>` | Expanded accordion entries. |
| `ui/settings/accounts/selected` | `Option<String>` | Selected account. |
| `ui/settings/models/selected` | `Option<ModelKey>` | Selected (account, model). |
| `ui/global/banner` | `GlobalBanner` | App-wide banner. |
| `settings/index/entries/{id}` | `SettingsIndexEntry` | Index page row metadata. |

UI mode state (presence indicates the user is in that mode; `Null`
clears the mode):

| Path | Shape | Mode |
|---|---|---|
| `ui/settings/new_account/buffer` | `Option<String>` | Composing a new account name. |
| `ui/settings/pending_delete` | `Option<String>` | Showing delete confirmation for that account. |
| `ui/settings/edit_buffer` | `Option<String>` | Inline-editing a real field; carries live typed text. |
| `ui/settings/edit_field_path` | `Option<Path>` | Which row's field is being edited (when `edit_buffer` set). |
| `ui/settings/manual_model` | `Option<ManualModelDraft>` | Composing a new model entry inline. |

What's gone:

- The `settings/accounts/_new` cursor scope — replaced by
  `new_account/buffer` mode state.
- The `settings/accounts/_delete` cursor scope — replaced by
  `pending_delete` mode state.
- The `ui/settings/manual_model/{stage,buffer,staged_id,staged_ctx,account}`
  scattered atoms — collapsed into a single typed `ManualModelDraft`.

`ManualModelDraft` is a new type in `ox-types::settings`:

```rust
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManualModelDraft {
    pub account:    String,
    pub stage:      ManualModelStage,
    pub buffer:     String,
    pub staged_id:  Option<String>,
    pub staged_ctx: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManualModelStage {
    Id,
    Ctx,
    Out,
}
```

### 4.3 Subscriptions

After this work, four subscriptions remain in `ox-gate`:

**`AccountTestSubscription`** (async action trigger, unchanged in
shape).
- Watches: `PrefixSuffix { prefix: config/gate/accounts, suffix: test_now }`.
- Reads AccountConfig + ApiKey; writes `Testing` status; spawns
  `transport.test_connection`; writes `Success`/`Failed`.
- Holds a supersession map.

**`CatalogRefreshSubscription`** (async action trigger, unchanged in
shape).
- Watches: `PrefixSuffix { prefix: config/gate/accounts, suffix: refresh_now }`.
- Same pattern as test; writes `Refreshing`/`Success`/`Failed` and
  the catalog itself.
- Holds a supersession map.

**`AccountDeleteCleanupSubscription`** (reactive observer, transformed
from the old `AccountDeleteSubscription`).
- Watches: `Prefix(config/gate/accounts)`.
- Filters: `change.before.is_some() && change.after.is_none()` AND
  the path is at account-record depth (`prefix.len() + 1`). The
  filter is necessary because `Prefix` matches all child writes too.
- Cleans up side data: drops `secret/keys/<name>`; drops
  `config/gate/providers/<name>` if no other account references it;
  clears `ui/settings/accounts/selected` if it matched the deleted
  name.
- The actual delete (the `Null` write to
  `config/gate/accounts/<name>`) was performed by the CLI.

**`CatalogFetchOnCreateSubscription`** (reactive observer, new —
replaces what `AccountCreateSubscription` did beyond the create
itself).
- Watches: `Prefix(config/gate/accounts)`.
- Filters: `change.before.is_none() && change.after.is_some()` AND
  the path is at account-record depth.
- Spawns `transport.fetch_catalog` for the new account; writes
  catalog + status as `CatalogRefreshSubscription` does.
- Optional: hold the same supersession map shape so a rapid
  delete-then-recreate doesn't race.

**`ConfigSaveSubscription`** (async action trigger, unchanged).
- Watches: `Exact(config/save)`.

What's gone:

- `AccountCreateSubscription` — replaced by direct writes from the
  CLI plus `CatalogFetchOnCreateSubscription` for the catalog
  follow-up.
- `AccountDeleteSubscription` — transformed into
  `AccountDeleteCleanupSubscription`.

### 4.4 Mode-aware dispatch

The dispatcher's `dispatch_settings_key` gains a mode-aware pass that
consults UI-state mode paths in priority order before row-keyed
dispatch. The pass is one helper function on the snapshot:

```rust
fn active_mode(snap: &mut dyn Reader) -> Option<ActiveMode> {
    if let Some(buffer) = read_typed::<String>(snap, &new_account_buffer_path) {
        return Some(ActiveMode::ComposingNewAccount { buffer });
    }
    if let Some(target) = read_typed::<String>(snap, &pending_delete_path) {
        return Some(ActiveMode::ConfirmingDelete { target });
    }
    if let Some(draft) = read_typed::<ManualModelDraft>(snap, &manual_model_path) {
        return Some(ActiveMode::ComposingManualModel { draft });
    }
    if let (Some(buffer), Some(field_path)) = (
        read_typed::<String>(snap, &edit_buffer_path),
        read_typed::<Path>(snap, &edit_field_path_path),
    ) {
        return Some(ActiveMode::EditingField { field_path, buffer });
    }
    None
}
```

`ActiveMode` is a dispatcher-internal enum (not stored in the
broker — the modes are stored as their constituent UI-state paths;
the enum is just a typed view at dispatch time):

```rust
enum ActiveMode {
    ComposingNewAccount { buffer: String },
    ConfirmingDelete    { target: String },
    EditingField        { field_path: Path, buffer: String },
    ComposingManualModel { draft: ManualModelDraft },
}
```

Priority order matches the order in `active_mode()`. The first match
wins. If none match, the dispatcher falls through to row-keyed
dispatch (the existing path).

The pass routes keys per mode:

| Mode | Keys handled |
|---|---|
| `ComposingNewAccount` | Printable → append to buffer; Backspace → pop; Enter → commit create; Esc → clear buffer |
| `ConfirmingDelete` | `y` → commit delete; `n` / Esc → clear pending |
| `EditingField` | Printable → append (with per-field rules); Backspace → pop; Enter → commit field; Esc → clear |
| `ComposingManualModel` | Per-stage; Enter advances stages; Esc clears |

**Mutual exclusion.** The CLI is responsible for ensuring only one
mode is active at a time. Entering a mode clears any other mode
that's set. In practice the dispatch helpers can't enter conflicting
modes (each opens its own and clears unrelated state), but the
mutual-exclusion invariant is something tests must pin: opening any
mode while another is active produces clean state, not interleaved.

The mode-aware pass's existence is the only structural change to the
dispatcher; everything else (cursor-scope binding lookup, command
invocation, write dispatching) is unchanged.

## 5. Migration plan

The work decomposes into seven phases. Each phase ships
independently, leaves the workspace green, and does not depend on
later phases for correctness.

### Phase 1: Eliminate `_create_now` and `AccountCreateSubscription`

CLI's `edit.commit` AccountAdd arm currently writes a
`CreateAccountRequest` to `config/gate/accounts/_create_now`; the
subscription validates and materializes the account.

After:

- The arm validates the buffer locally (PathComponent check), then
  writes the `AccountConfig` directly to
  `config/gate/accounts/<name>` plus the existing UI cascade (focus,
  expansion, selection, banner on invalid name).
- `AccountCreateSubscription` is deleted.
- A new `CatalogFetchOnCreateSubscription` watches
  `Prefix(config/gate/accounts)` for new entries and spawns the
  catalog fetch.
- `CreateAccountRequest` type stays for now — it's referenced by the
  existing inline-create code; Phase 7 cleans it up if no callers
  remain.
- E2E test `add_account_create_flow` no longer needs the
  poll-for-materialization step (the CLI's writes are synchronous);
  it does still need to poll for catalog fetch.

### Phase 2: Eliminate `delete_now` sentinel

CLI's delete-confirmation `y` keypress currently writes `Null` to
`config/gate/accounts/<name>/delete_now`; the subscription reads
that, deletes the record + side data, and clears selection.

After:

- The CLI writes `Null` to `config/gate/accounts/<name>` directly.
- `AccountDeleteSubscription` is renamed and refocused as
  `AccountDeleteCleanupSubscription`. New watch:
  `Prefix(config/gate/accounts)`. The filter at the top of `handle`
  rejects writes that aren't null at account-record depth.
- The cleanup body is unchanged in spirit but takes its trigger from
  the actual delete rather than the sentinel.
- Tests update.

This phase MUST land after Phase 1, because Phase 1 introduces
`CatalogFetchOnCreateSubscription` which also watches `Prefix` and
filters for *new* entries (`before.is_none()`); Phase 2 introduces a
sibling that filters for *deletions* (`after.is_none()`). The two
must coexist cleanly.

### Phase 3: Lift inline-create into `new_account/buffer` mode state

After Phase 1, the inline-create flow still uses
`edit_field_path = settings/accounts/_new` + `edit_buffer` + `edit_mode`
and a synthetic `RowKind::AccountAdd` row. This phase replaces all of
that with `ui/settings/new_account/buffer: Option<String>`.

After:

- `accounts.add` writes `Some("")` to `new_account/buffer` (and
  ensures the accounts section is in the expanded set, focuses the
  affordance row — see below). Does not write `edit_mode`,
  `edit_field_path`, or `edit_buffer`.
- The dispatcher's mode-aware pass handles `new_account/buffer`:
  printable → append; Backspace → pop; Enter → commit-create; Esc →
  clear.
- Commit-create writes `AccountConfig` to
  `config/gate/accounts/<name>`, plus UI cascade (focus the new row,
  expand, clear `new_account/buffer`).
- `RowKind::AccountAdd` is dropped from the visible-rows enum and
  every match site that handles it.
- The renderer (`index.rs::render`) reads `new_account/buffer` and
  decorates the accounts section header: when `Some(buffer)`,
  prepend an inline `Name▸ <buffer>▏` line; when `None`, prepend a
  static `+ New connection` affordance line.
- `focused_row` for the affordance: TBD — needs decision. See §6.

The "no synthetic display rows" invariant is satisfied: the
affordance is renderer decoration, not a row in the projection.

### Phase 4: Lift delete-confirm into `pending_delete` mode state

The delete-confirm modal at `settings/accounts/_delete` is the last
remaining cursor-scope-as-mode in the codebase.

After:

- `accounts.delete_confirm` reads the selected account, writes
  `Some(<name>)` to `ui/settings/pending_delete`. Does not write
  `cursor`.
- The dispatcher's mode-aware pass handles `pending_delete`:
  `y` → write `Null` to `config/gate/accounts/<name>` + clear
  pending; `n` / `Esc` → clear pending.
- `crates/ox-cli/src/settings/renderers/overlay_delete_account.rs`
  is deleted.
- The `_delete` cursor scope's bindings are deleted from
  `bindings.rs`. The `accounts.cancel` command becomes the
  cancel-pending action (rename or repurpose).
- `dim_buffer` in `view_render.rs` is deleted (no remaining
  modal users).
- `View::Modal` enum variant: kept if any future page-level modal is
  anticipated, deleted if we're committed to the no-modal direction.
  Recommendation: delete. The framework's principles imply
  page-level modals are rare; bringing them back would be a
  considered design choice that re-adds the variant.
- The renderer composes a `View::Banner` (or similar) inline above
  the accounts list when `pending_delete` is set.
- Tests update.

### Phase 5: Lift manual-model into `manual_model` typed sum

The current `manual_model/{stage,buffer,staged_id,staged_ctx,account}`
scattered atoms become a single typed value at
`ui/settings/manual_model: Option<ManualModelDraft>`.

After:

- `ManualModelDraft` and `ManualModelStage` types added to
  `ox-types::settings`.
- All reads/writes of `manual_model/*` become reads/writes of
  `ui/settings/manual_model: Option<ManualModelDraft>`.
- The dispatcher's mode-aware pass handles `manual_model`:
  per-stage routing as today.
- `RowKind::ModelAddManual` is dropped.
- The renderer reads `manual_model` and decorates the models section
  with the inline three-stage form (when `Some`) or the static
  `+ add model manually` affordance (when `None`).
- `begin_manual_model` command writes `Some(ManualModelDraft {
  account: <name>, stage: Id, buffer: String::new(), staged_id: None,
  staged_ctx: None })`.
- Tests update — substantial rewrite of `manual_model_*` tests in
  `edit.rs`.

### Phase 6: Lift model empty-state into renderer decoration

After Phase 5, only `RowKind::ModelEmptyState` remains as a
synthetic row.

After:

- `RowKind::ModelEmptyState` is dropped.
- The renderer reads `models: Vec<ModelInfo>` for each connection.
  When empty, render a `no models — press r to refresh` line as a
  decoration below the connection row. Same depth indent as a real
  model row.
- Activation moves from "Enter on the synthetic row" to "press `r`
  while focused on the connection." The `r` keystroke is already
  bound at `Prefix(settings/models)` to `account.refresh`; the
  affordance text just describes what's already available.
- `focused_row` cannot point at a non-existent row, so `j`/`k`
  navigation no longer lands on the empty-state line. The user
  navigates to the connection itself, presses `r`. UX is slightly
  different but cleaner.
- Tests update.

### Phase 7: Cleanup

The substrate is consistent at this point; this phase removes the
no-longer-needed scaffolding.

After:

- `_`-prefix rejection in `edit.commit` AccountAdd arm: removed (no
  synthetic paths to collide with).
- `_`-prefix rejection in any remaining subscription: removed.
- `_`-prefix banner-error tests: removed.
- `safe_component` callers in `visible_rows::row_path` for accounts:
  reviewed — `safe_component` was added partly to mask synthetic-vs-real
  collisions; with the synthetic rows gone it may be reduced to its
  original purpose (sanitizing TOML-loaded names with hyphens).
- The convergence note in `ui_framework.md` is removed.
- `CreateAccountRequest` type: deleted if no callers remain.
- `View::Modal` enum variant: deleted (decision deferred from
  Phase 4).
- `dim_buffer`: confirmed deletable in Phase 4; remove if not yet
  done.

## 6. Open design questions

### 6.1 Where does `focused_row` point during compose-new-account mode?

The `+ New connection` affordance line is a renderer decoration, not
a row in the projection. But `focused_row` and `j`/`k` navigation
expect rows in the projection.

Options:

- **A. The affordance has its own UI-state path.** Something like
  `ui/settings/affordance_focused: bool`. When `new_account/buffer`
  is set, the renderer auto-focuses the affordance and `j`/`k` are
  intercepted by the mode-aware pass. Heavyweight.
- **B. `focused_row` is `None` while in compose mode.** The renderer
  understands "compose mode + no focused row" as "focus the
  affordance line." `j`/`k` are intercepted by the mode-aware pass
  (no-op or some other behavior). Simpler.
- **C. `focused_row` points at the first real account row even
  during compose mode.** The user enters compose mode by pressing
  `a`, types in the affordance line at the top of the section, and
  the focused row indicator is somewhere irrelevant. `j`/`k` still
  navigate real rows during compose mode. Simplest; least
  consistent.

Recommendation: B. The mode-aware pass already intercepts j/k while
in mode (no-op until the user commits or cancels). `focused_row =
None` during compose mode is honest: there is no focused real row.
The renderer renders the affordance with its own visual focus
treatment.

### 6.2 Mutual exclusion of modes — guarded or trusted?

The mode-aware pass relies on at most one mode being active at a
time. In practice every "open mode" command clears the others before
setting its own. Should the dispatcher enforce this with an
assertion, or trust the commands?

Recommendation: trust the commands; pin the invariant with tests.
Adding runtime checks for "did the command clear other modes" adds
boilerplate without preventing the bug class (the bug is in the
command, not in the dispatcher). Tests that verify "opening mode X
clears mode Y" catch regressions where they happen.

### 6.3 `View::Modal` variant — keep or delete?

Phase 4 deletes the last user. Should the variant stay (in case a
future page-level modal is added) or go (forcing future modals to
re-justify the variant)?

Recommendation: delete. The framework's principles imply page-level
modals are rare and require deliberate design. If a future feature
needs one, the design discussion will surface whether a real modal
or another mode-state pattern is the right shape; the variant
re-appears as part of that decision.

## 7. Test strategy

Unit tests:
- Per-phase, the existing tests are updated to reflect new shapes.
  Most updates are mechanical (path renames, write-shape changes).
- New tests pin the mode-aware dispatch: each mode's open / commit /
  cancel produces the expected writes; opening any mode while
  another is active clears the other.
- New tests pin the reactive subscriptions' filter logic: the
  cleanup subscription fires only on null writes at account-record
  depth; the catalog-fetch subscription fires only on new entries.

E2E tests (`crates/ox-cli/tests/settings_e2e.rs`):
- `add_account_create_flow` — already rewritten in the inline branch.
  Needs updating for Phase 1 (no more sentinel polling) and Phase 3
  (mode-state path instead of `edit_field_path`).
- `add_connection_inline_ghost_row_accepts_typing` — Phase 3 changes
  the mode-state path; the test's setup and snapshot assertions
  update.
- `delete_account_flow` — Phase 4 reshapes this entirely. The test
  drives `d` to open `pending_delete`, asserts the inline banner
  renders, presses `y`, asserts the account is gone.
- New test: `manual_model_inline_form_completes` — drives the
  three-stage form via `manual_model` mode-state path.
- New test: `model_empty_state_refreshes_on_r` — confirms the
  decoration line appears for accounts with empty catalogs and that
  `r` triggers refresh.

Snapshot tests:
- The accordion snapshot tests pick up new shapes for the
  affordances. Each phase that lifts a synthetic row into a renderer
  decoration produces a snapshot diff that the implementer accepts
  after visual review.

## 8. What gets deleted

Files:
- `crates/ox-cli/src/settings/renderers/overlay_delete_account.rs`
- (Phase 7) any `dim_buffer`-related code if not removed in Phase 4

Subscriptions:
- `AccountCreateSubscription` (Phase 1)
- `AccountDeleteSubscription` (Phase 2; replaced by `AccountDeleteCleanupSubscription`)

Bindings (deleted from `bindings.rs`):
- All bindings at the `_delete` cursor scope (Phase 4).

Commands:
- Possibly `accounts.cancel` if it's exclusively a `_delete`-modal
  helper. Verify in Phase 4. (Counter-evidence: it might have been
  reused for other cancellations during the inline branch.)

Types:
- `RowKind::AccountAdd` (Phase 3)
- `RowKind::ModelAddManual` (Phase 5)
- `RowKind::ModelEmptyState` (Phase 6)
- `View::Modal` variant (Phase 7, decision deferred)
- `CreateAccountRequest` (Phase 7, if no callers remain)

UI-state paths (no longer written/read):
- `ui/settings/manual_model/stage`
- `ui/settings/manual_model/buffer`
- `ui/settings/manual_model/staged_id`
- `ui/settings/manual_model/staged_ctx`
- `ui/settings/manual_model/account`
- `ui/settings/edit_field_path` and `ui/settings/edit_buffer` *for
  AccountAdd usage* — the existing-field-edit usage stays.

Data-tree paths (no longer written/read):
- `config/gate/accounts/_create_now`
- `config/gate/accounts/{name}/delete_now`

Tests:
- `accounts_create_*` tests in `account_model.rs` (Phase 1)
- `_`-prefix rejection tests in `edit.rs` and `account_create.rs`
  (Phase 7)

## 9. What gets added

Types (`ox-types::settings`):
- `ManualModelDraft`
- `ManualModelStage`

Subscriptions:
- `CatalogFetchOnCreateSubscription` (Phase 1)
- `AccountDeleteCleanupSubscription` (Phase 2; replaces
  `AccountDeleteSubscription`)

Dispatcher:
- `active_mode(snap: &mut dyn Reader) -> Option<ActiveMode>` helper
  in `crates/ox-cli/src/settings/dispatch.rs`.
- `ActiveMode` enum in the same file.
- A new top-of-`dispatch_settings_key` pass that consults
  `active_mode` before row-keyed dispatch.

UI-state paths:
- `ui/settings/new_account/buffer`
- `ui/settings/pending_delete`
- `ui/settings/manual_model` (typed)

Renderers:
- New decoration logic in `index.rs::render` reading the three
  mode-state paths and the per-account model count.

## 10. Risks

- **Mode mutual-exclusion drift.** A future command opens a mode
  without clearing others; both modes appear active; dispatcher
  picks the wrong one. Mitigation: tests pin the invariant; the
  mode-aware pass's priority order is documented (compose-new
  before pending-delete before edit-field before manual-model).
- **Reactive-subscription filter bugs.** The cleanup subscription
  watching `Prefix` will receive *every* write under
  `config/gate/accounts`. Forgetting to filter for null writes at
  account-record depth would cause cleanup to fire on, e.g.,
  `models` writes. Mitigation: the filter is the first thing the
  handler does; tests verify it explicitly.
- **Snapshot test churn.** Every accordion-rendering snapshot
  changes shape during Phase 3-6. Mitigation: phases land
  independently with their snapshot updates; reviewer accepts each
  pass individually.
- **E2E test fragility during transition.** Mid-phase intermediate
  states may break individual e2e tests. Mitigation: each phase's
  plan addresses its e2e test impact explicitly; intermediate
  states must keep the e2e suite green.
- **Missed callers.** A command somewhere still writes
  `_create_now` or reads `manual_model/buffer`. Mitigation: each
  phase begins with a `grep` audit; the implementer reports the hit
  list before deletion.

## 11. Execution

Each phase becomes its own implementation plan
(`docs/superpowers/plans/`). The plans are independent and can land
in the order the phases describe (1 → 7) or paused/reordered as
needed (Phases 5 and 6 are independent of 3 and 4 once 1+2 land).

Phase 1 is the safest first move and has the largest leverage —
removing the `_create_now` sentinel and `AccountCreateSubscription`
also retires the most prominent example of the anti-pattern in the
codebase.

After Phase 7 lands, the convergence note in `ui_framework.md`
is removed and the framework docs no longer carry a "the code
disagrees with this; here's why" caveat.
