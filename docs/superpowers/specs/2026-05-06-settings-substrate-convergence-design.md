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

A small framework primitive lands first — the focus model — that
separates focus identity from the visible-rows projection (matching
how Flutter, SwiftUI, Compose, and React Aria all handle focus, but
with focus identity persisted in the broker as a `Path`). The
substrate phases compose against it.

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
- The framework's focus model separates focus identity (a typed
  `FocusId`) from rendering. Renderers tag widgets focusable
  per-item; the dispatcher derives traversal by walking the View
  tree. Decorations and affordances render but never focus.
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
| `ui/settings/focused` | `Option<FocusId>` | Identity of the currently-focused widget; see §4.5. (Renamed from `focused_row`; new typed wrapper.) |
| `ui/settings/expanded` | `Vec<String>` | Expanded accordion entries. |
| `ui/settings/accounts/selected` | `Option<String>` | Selected account. |
| `ui/settings/models/selected` | `Option<ModelKey>` | Selected (account, model). |
| `ui/global/banner` | `GlobalBanner` | App-wide banner. |
| `settings/index/entries/{id}` | `SettingsIndexEntry` | Index page row metadata. |

UI mode state. A mode's state is one or more values at a named
UI-state path or sub-tree. Single-buffer modes live at a single
path with `Option<T>` typing; the type's presence indicates the
mode is active. Multi-field modes live at a sub-tree with one path
serving as the discriminator (its presence indicates the mode is
active); the other paths carry the rest of the form's state.
Either shape is fine — pick the smallest one that fits the data.
The substrate's strength is per-path granularity; a keystroke
should be one path write, not a re-serialize-the-whole-struct.

Single-buffer modes:

| Path | Shape | Mode |
|---|---|---|
| `ui/settings/new_account/buffer` | `Option<String>` | Composing a new account name. Discriminator: presence of buffer. |
| `ui/settings/pending_delete` | `Option<String>` | Showing delete confirmation for the named account. Discriminator: presence of value. |
| `ui/settings/edit_buffer` | `Option<String>` | Inline-editing a real field; carries live typed text. |
| `ui/settings/edit_field_path` | `Option<Path>` | Which row's field is being edited (when `edit_buffer` set). |

Multi-field mode (manual model entry; existing scattered-atoms shape
preserved, with a designated discriminator path):

| Path | Shape | Mode role |
|---|---|---|
| `ui/settings/manual_model/account` | `Option<String>` | **Discriminator.** Presence = in mode; carries the target account name. |
| `ui/settings/manual_model/stage` | `ManualModelStage` | Current stage of the three-step form. |
| `ui/settings/manual_model/buffer` | `String` | Live typed text for the current stage. |
| `ui/settings/manual_model/staged_id` | `Option<String>` | Value committed in stage 1 (id). |
| `ui/settings/manual_model/staged_ctx` | `Option<u32>` | Value committed in stage 2 (max_context_size). |

`ManualModelStage` is the only new type, in `ox-types::settings`:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManualModelStage {
    Id,
    Ctx,
    Out,
}
```

(Today the stage path holds a `String` matched against `"id"` /
`"ctx"` / `"out"`; promoting to a typed enum is a small win without
collapsing the rest of the form into one record.)

What's gone:

- The `settings/accounts/_new` cursor scope — replaced by
  `new_account/buffer` mode state.
- The `settings/accounts/_delete` cursor scope — replaced by
  `pending_delete` mode state.

The `manual_model/*` sub-tree keeps its scattered-atoms shape; the
only changes there are (a) `manual_model/account` is now formally
the mode discriminator (the dispatcher reads it to determine mode)
and (b) `stage` becomes typed.

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
    ComposingNewAccount  { buffer: String },
    ConfirmingDelete     { target: String },
    EditingField         { field_path: Path, buffer: String },
    ComposingManualModel { account: String, stage: ManualModelStage,
                           buffer: String, staged_id: Option<String>,
                           staged_ctx: Option<u32> },
}
```

`ComposingManualModel` is reconstructed by reading each
`manual_model/*` path; the variant just gives the dispatcher a typed
view. This is one extra read per keystroke vs. a single typed-sum
read, but each read is cheap and the per-path shape keeps individual
keystrokes as single-path writes. The trade-off favors the substrate
over the dispatcher's read count.

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

### 4.5 Framework primitive: focus model

This is the only change to ox-view. It's a framework primitive
update that the substrate phases (especially Phase 3 onward) build
on, so it lands first as Phase 0.

#### The pattern across declarative UI frameworks

Flutter, SwiftUI, Jetpack Compose, and React Aria all separate
focus from rendering. Different mechanisms — Flutter's parallel
FocusNode tree, SwiftUI's `@FocusState` property bindings,
Compose's `Modifier.focusable()`, React Aria's explicit `Item`
identity — converge on the same architectural commitment:

1. Focus is opt-in per component, not implicit.
2. Focus state is separate from layout state.
3. Traversal order is derived from the widget tree but customizable.
4. Selection ≠ focus. Distinct concepts.

Our framework today conflates focus with the visible-rows
projection: `visible_rows::enumerate` is both the focus enumeration
AND the data-derived row list, and `focused_row: Path` indexes by
data-tree-derived path. That conflation is what makes "non-navigable
decorations" awkward — there's no place in the projection for a
thing that renders but isn't focusable.

#### The StructFS-native version

Focus identity is a `Path`, persisted in the broker. None of the
four frameworks above can do this — their focus state is in-process
memory. Ours can be observed by subscriptions, restored across
crashes, and queried by other components. That's the substrate's
gift.

```rust
/// Newtype wrapper signaling "this Path is a focus identity, not a
/// data-tree path." Renderers tag focusable widgets with a FocusId;
/// the dispatcher consults the View's focus enumeration to walk
/// j/k navigation.
pub struct FocusId(pub Path);

pub struct ListItem {
    pub primary:   String,
    pub secondary: Option<String>,
    pub badge:     Option<String>,
    /// `Some` = focusable; the FocusId is this item's focus identity.
    /// `None` = decoration; j/k skips it; cannot be the focus target.
    pub focus:     Option<FocusId>,
}

impl View {
    /// Walk the View tree and extract focusable items in traversal
    /// order. Used by the dispatcher's j/k stepping.
    pub fn focus_enumeration(&self) -> Vec<FocusId> { /* ... */ }
}
```

`focused: Option<FocusId>` lives at `ui/settings/focused`
(replacing `focused_row: Path`). The dispatcher's j/k logic:

```rust
fn step_focus(view: &View, focused: Option<&FocusId>, dir: Direction) -> Option<FocusId> {
    let enumeration = view.focus_enumeration();
    if enumeration.is_empty() { return None; }
    let current = focused
        .and_then(|f| enumeration.iter().position(|e| e == f))
        .unwrap_or(0);
    let next = match dir {
        Direction::Next => (current + 1) % enumeration.len(),
        Direction::Prev => (current + enumeration.len() - 1) % enumeration.len(),
    };
    Some(enumeration[next].clone())
}
```

Renderers mark items as focusable by setting `focus: Some(...)`.
The FocusId for a real account row is its display-tree path
(`settings/accounts/<name>`); for any future focusable affordance,
it's the affordance's UI-state path. Decorations and inline prompts
emit `focus: None` and are simply skipped by traversal — no
synthetic identity needed.

#### What this gives us

- **Real rows navigable, decorations not.** The "+ New connection"
  affordance line emits as `focus: None`. j/k skips it. `focused`
  always points at a real row.
- **Activation by FocusId.** Pressing Enter reads `focused`,
  resolves it through the View's focus enumeration, dispatches
  based on what kind of FocusId it is. For `RowKind::Real`, that's
  the existing path-equality dispatch.
- **Affordances accessed by key, not navigation.** `a` is bound at
  `Prefix(settings/accounts)` to `accounts.add` — pressing it from
  any focused row in the section opens compose mode. The affordance
  line is a discoverable visual hint; the actual entry point is the
  keybinding.
- **§6.1 dissolves.** "Where does focused_row point during compose
  mode?" was awkward because the synthetic affordance row had no
  good answer. With the focus model, `focused` points at whatever
  real row was focused before compose was entered; compose mode is
  driven by `new_account/buffer`, not by focus moving anywhere.

#### What survives unchanged

- `visible_rows::enumerate` keeps existing as a *renderer
  convenience* — it produces the rows the index renderer maps into
  ListItems. It's no longer the source of truth for navigation;
  navigation is the View's focus enumeration. Renderers that want
  to compose lists by walking visible_rows still can.
- The View enum's variant set is unchanged. Only `ListItem` gains a
  field. (Future widgets that need focus distinction — e.g.
  `FormRow` — will gain similar fields when the need arrives. YAGNI
  for now.)
- The translator (`view_render.rs`) doesn't need to know about
  focus — `focused` is dispatcher state, not render state. (The
  renderer chooses how to highlight the focused item by reading
  `focused` and matching it against ListItem.focus when emitting
  the `selected: Option<usize>` index.)

#### Compared to the four frameworks

- **Flutter**: same separation of focus from rendering; ours is
  derived from the View tree (one source) where Flutter has two
  parallel trees.
- **SwiftUI**: same opt-in-per-widget shape (`.focused()` modifier
  ↔ our `focus: Some(...)` field).
- **Compose**: same per-widget annotation pattern
  (`Modifier.focusable()` ↔ our `focus: Some(...)`).
- **React Aria**: same explicit identity for focusable items, same
  separability of selection and focus.
- **Unique to us**: focus identity is a `Path`, persisted in the
  broker. Survives crashes, observable by subscriptions,
  cross-component shareable.

## 5. Migration plan

The work decomposes into a framework-primitive Phase 0 and seven
substrate phases. Each phase ships independently, leaves the
workspace green, and does not depend on later phases for
correctness. Phase 0 lands first because Phases 3, 5, and 6
compose against the focus model it introduces.

### Phase 0: Land the focus model framework primitive

Adds the `FocusId` newtype, the `focus: Option<FocusId>` field on
`ListItem`, and the `View::focus_enumeration()` walk. Renames
`ui/settings/focused_row: Path` to `ui/settings/focused: Option<FocusId>`.
Updates the dispatcher's j/k stepping to walk `focus_enumeration`.

After:

- `ox-view::ListItem` gains the `focus` field. Existing call sites
  initialize with `focus: Some(FocusId(<row's display path>))` for
  real rows. Add the `View::focus_enumeration` method.
- The dispatcher's `tree.next` / `tree.prev` switch from
  `position_of(visible_rows, focused_row)` to
  `view.focus_enumeration().position(|f| f == &focused)`. The
  step-and-write logic is otherwise unchanged.
- `ui/settings/focused_row: Path` becomes `ui/settings/focused: Option<FocusId>`.
  All callers update the path string and the type.
- `tree.activate` resolves the focused FocusId back to a row by
  matching the FocusId against the visible-rows projection (via
  the renderer's mapping from rows to ListItem focus values). The
  existing `RowKind` dispatch follows from there.
- The renderer continues to map `visible_rows::enumerate` into
  `ListItem`s — it just sets `focus: Some(FocusId(row.path.clone()))`
  on each. No structural change to the projection in this phase.
- Tests: substantial, but mechanical. Existing tests asserting
  on `focused_row` writes need the new path + new type. New tests
  pin the focus_enumeration walk for representative View shapes.

This phase doesn't touch any synthetic rows, mode states, or
sentinels. It's pure framework work that subsequent phases build
on.

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
  ensures the accounts section is in the expanded set). Does not
  write `edit_mode`, `edit_field_path`, or `edit_buffer`. Does not
  move `focused`.
- The dispatcher's mode-aware pass handles `new_account/buffer`:
  printable → append; Backspace → pop; Enter → commit-create; Esc →
  clear.
- Commit-create writes `AccountConfig` to
  `config/gate/accounts/<name>`, plus UI cascade (write `focused` to
  the new row's FocusId, expand, clear `new_account/buffer`).
- `RowKind::AccountAdd` is dropped from the visible-rows enum and
  every match site that handles it.
- The renderer (`index.rs::render`) reads `new_account/buffer` and
  decorates the accounts section: when `Some(buffer)`, prepend an
  inline `Name▸ <buffer>▏` line emitted as
  `ListItem { focus: None, … }`; when `None`, prepend a static
  `+ New connection` affordance line, also `focus: None`. Either
  way the affordance is renderer decoration, not a row in the
  projection, and not in the focus enumeration.

The "no synthetic display rows" invariant is satisfied: the
affordance is renderer decoration, not a row in the projection.
Phase 0's focus model is what makes this clean — without it the
affordance line would either need a synthetic FocusId or sit
awkwardly outside the navigable list.

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
  is deleted (the renderer is the cursor-scope artifact, not the
  modal pattern itself).
- The `_delete` cursor scope's bindings are deleted from
  `bindings.rs`. The `accounts.cancel` command becomes the
  cancel-pending action (rename or repurpose).
- The renderer composes an inline confirmation banner above the
  accounts list when `pending_delete` is set. (Inline rather than
  modal — the action is small and the user's already on the
  accounts page.)
- `View::Modal`, `dim_buffer`, and the modal rendering primitives
  in `view_render.rs` **are kept**. Modals are a legitimate
  rendering pattern and we expect to use them for future features
  (help screens, larger interactive prompts, anything that genuinely
  earns a centered overlay). The anti-pattern was driving modals
  via cursor scopes and dedicated renderers; the variant itself is
  fine. Future modal use cases follow the modes-as-state pattern: a
  UI-state path indicates "this modal is showing"; the renderer
  reads that path and composes
  `View::Modal { background: <current_page>, foreground: <modal_body>, dim: true }`;
  bindings while the modal is showing are mode-aware bindings, not
  cursor-scope bindings. Phase 7 doesn't revisit this decision.
- Tests update.

### Phase 5: Formalize `manual_model` as a mode + drop `ModelAddManual`

The `manual_model/*` sub-tree's existing scattered-atoms shape stays.
Three changes:

1. `manual_model/account` is formally the mode discriminator. The
   dispatcher's mode-aware pass reads it to determine "in mode";
   when set, route per-stage as today.
2. `manual_model/stage` becomes typed (`ManualModelStage` enum) instead
   of a stringly-typed `"id"` / `"ctx"` / `"out"` value.
3. `RowKind::ModelAddManual` is dropped from `visible_rows`. The
   renderer reads `manual_model/account` to detect "in mode" and
   composes the inline three-stage form decoration when set; when
   unset, renders a static `+ add model manually` affordance line.

The `manual_model/*` paths themselves don't change shape (still five
paths under the sub-tree). The dispatcher and renderer just consult
the discriminator instead of inferring mode from row identity.

`begin_manual_model` already writes the right paths; this phase
mostly removes the synthetic-row plumbing rather than restructuring
the form's state. The mode-aware dispatch pass replaces the existing
"focused row is `RowKind::ModelAddManual` so route to manual_model
commit" flow.

Tests: update `manual_model_*` tests in `edit.rs` to reflect the new
discriminator-driven mode-detection (vs. the previous focused-row
detection).

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
- The empty-state line emits as `ListItem { focus: None, … }` —
  Phase 0's focus model skips it during j/k traversal. `focused`
  always points at a real model row or its parent connection. The
  user navigates to the connection itself, presses `r`. UX is
  slightly different but cleaner.
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

`View::Modal`, `dim_buffer`, and the modal rendering primitives
**stay** (per §6.2). Future modal use cases follow modes-as-state
for their state shape and use these primitives for their visual
treatment.

## 6. Open design questions

### 6.1 Where does focus point during compose-new-account mode? — resolved

Resolved by §4.5's focus model. The "+ New connection" affordance
line emits as `ListItem { focus: None, … }` — j/k traversal skips
it. `focused: Option<FocusId>` keeps pointing at whatever real row
was focused before compose was entered. Compose mode is driven by
`new_account/buffer`; the mode-aware pass intercepts printable keys
+ Backspace + Enter + Esc while the buffer is set; j/k continues to
navigate real rows in the projection (no-op or freely allowed
depending on UX choice — see below).

Sub-question: what should j/k do during compose mode? Three
plausible answers:

- **No-op.** Once you're typing, j/k chars get appended to the
  buffer (they're printable). Already handled by the mode-aware
  pass — no extra work.
- **Continue navigating real rows.** The user can scroll the list
  underneath while composing. Visually distracting but harmless.
- **Cancel compose mode and navigate.** "Pressing j escapes the
  prompt." Surprises users who expect modes to be explicit.

Recommendation: no-op (j/k as printable chars get appended). Matches
how every other inline-edit field behaves.

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

### 6.3 `View::Modal` variant — resolved: kept

Decided after the spec's first review: `View::Modal`, `dim_buffer`,
and the framework's modal rendering primitives stay. Modals are
legitimate UI for help screens, complex prompts, or anything that
genuinely earns a centered overlay; deleting the variant would
force a future feature to re-justify it from scratch.

The anti-pattern wasn't the variant — it was driving modals via
cursor scopes and dedicated cursor-scope renderers. Future modal
use cases follow modes-as-state: a UI-state path indicates the
modal is showing; the renderer reads it and composes a
`View::Modal { background, foreground, dim }`; bindings while the
modal is showing are mode-aware, not cursor-scope. The variant is
a rendering primitive; modes-as-state is the state shape.

Phase 4 retires the only existing modal renderer
(`overlay_delete_account.rs`) because it was cursor-scope-driven —
not because the modal pattern itself is being walked back.

## 7. Test strategy

Unit tests:
- Per-phase, the existing tests are updated to reflect new shapes.
  Most updates are mechanical (path renames, write-shape changes).
- Phase 0 adds `View::focus_enumeration` tests pinning that the
  enumeration walks ListItems in order, includes only items with
  `focus: Some(...)`, and produces a stable order across renders
  with the same input data.
- Phase 0 also pins the dispatcher's j/k stepping: stepping past
  the end wraps; stepping with `focused: None` lands at index 0;
  stepping when the prior `focused` is no longer in the enumeration
  (row vanished) lands at index 0.
- New tests pin the mode-aware dispatch: each mode's open / commit /
  cancel produces the expected writes; opening any mode while
  another is active clears the other.
- New tests pin the reactive subscriptions' filter logic: the
  cleanup subscription fires only on null writes at account-record
  depth; the catalog-fetch subscription fires only on new entries.

E2E tests (`crates/ox-cli/tests/settings_e2e.rs`):
- `add_account_create_flow` — already rewritten in the inline branch.
  Needs updating for Phase 0 (focused → FocusId), Phase 1 (no more
  sentinel polling), and Phase 3 (mode-state path instead of
  `edit_field_path`).
- `add_connection_inline_ghost_row_accepts_typing` — Phase 0 changes
  the focused-state path + type; Phase 3 changes the mode-state
  path; the test's setup and snapshot assertions update.
- `delete_account_flow` — Phase 4 reshapes this entirely. The test
  drives `d` to open `pending_delete`, asserts the inline banner
  renders, presses `y`, asserts the account is gone.
- New test: `manual_model_inline_form_completes` — drives the
  three-stage form via `manual_model` mode-state path.
- New test: `model_empty_state_refreshes_on_r` — confirms the
  decoration line appears for accounts with empty catalogs and that
  `r` triggers refresh.
- New test (Phase 0): `j_skips_decorations` — confirms that j/k
  navigation lands only on items with `focus: Some(...)`, never on
  decorations like banners or affordances.

Snapshot tests:
- The accordion snapshot tests pick up new shapes for the
  affordances. Each phase that lifts a synthetic row into a renderer
  decoration produces a snapshot diff that the implementer accepts
  after visual review.

## 8. What gets deleted

Files:
- `crates/ox-cli/src/settings/renderers/overlay_delete_account.rs`
  (Phase 4)

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
- `CreateAccountRequest` (Phase 7, if no callers remain)

`View::Modal`, `dim_buffer`, and the modal rendering primitives are
**not** deleted. They remain available for future modal use cases;
see §6.3.

UI-state paths (no longer written/read):
- `ui/settings/edit_field_path` and `ui/settings/edit_buffer` *for
  AccountAdd usage* — the existing-field-edit usage stays.
- `ui/settings/focused_row: Path` — replaced by
  `ui/settings/focused: Option<FocusId>` (Phase 0).

The `manual_model/*` sub-tree paths stay (the form's state shape is
unchanged). What changes there is conceptual: `manual_model/account`
is formally the mode discriminator, and `manual_model/stage` is
typed.

Data-tree paths (no longer written/read):
- `config/gate/accounts/_create_now`
- `config/gate/accounts/{name}/delete_now`

Tests:
- `accounts_create_*` tests in `account_model.rs` (Phase 1)
- `_`-prefix rejection tests in `edit.rs` and `account_create.rs`
  (Phase 7)

## 9. What gets added

Framework (`ox-view`) — Phase 0:
- `FocusId(pub Path)` newtype.
- `ListItem.focus: Option<FocusId>` field.
- `View::focus_enumeration(&self) -> Vec<FocusId>` method.

Types (`ox-types::settings`):
- `ManualModelStage` enum (replaces the stringly-typed stage value
  at `manual_model/stage`).

Subscriptions:
- `CatalogFetchOnCreateSubscription` (Phase 1)
- `AccountDeleteCleanupSubscription` (Phase 2; replaces
  `AccountDeleteSubscription`)

Dispatcher:
- `active_mode(snap: &mut dyn Reader) -> Option<ActiveMode>` helper
  in `crates/ox-cli/src/settings/dispatch.rs`.
- `ActiveMode` enum in the same file (dispatcher-internal; not
  serialized to the broker).
- A new top-of-`dispatch_settings_key` pass that consults
  `active_mode` before row-keyed dispatch.
- j/k stepping switches from `position_of(visible_rows, focused_row)`
  to `view.focus_enumeration().position(|f| f == &focused)` (Phase 0).

UI-state paths:
- `ui/settings/focused: Option<FocusId>` — Phase 0 (replaces
  `focused_row: Path`).
- `ui/settings/new_account/buffer` — Phase 3.
- `ui/settings/pending_delete` — Phase 4.
- The `manual_model/*` sub-tree's role formalizes — Phase 5 — but
  no new paths.

Renderers:
- All ListItem-emitting renderers gain `focus: Some(FocusId(...))`
  for navigable items, `focus: None` for decorations (Phase 0).
- New decoration logic in `index.rs::render` reading the mode-state
  paths and the per-account model count, composing inline
  affordances and prompts (Phase 3-6).

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
- **Phase 0 ripples through every focus-keyed test.** Renaming
  `focused_row → focused` and changing the type from `Path` to
  `Option<FocusId>` touches a large fraction of the existing test
  suite — easily 50+ assertions. Mitigation: Phase 0 is a single
  cohesive PR; all tests update in one commit; CI gates merge.

## 11. Execution

Each phase becomes its own implementation plan
(`docs/superpowers/plans/`). The plans are independent and can land
in the order the phases describe (Phase 0 → Phase 1 → … → Phase 7)
or selectively reordered with caveats:

- **Phase 0 lands first.** Phases 3, 5, and 6 explicitly compose
  against the focus model — the `focus: None` decoration shape is
  load-bearing for the affordance + empty-state + manual-model
  renderer logic. Trying to land Phase 3 against the old
  visible_rows-as-focus-source model means re-introducing the
  workarounds the focus model retires.
- **Phases 1 and 2 are independent of Phase 0.** They can land in
  parallel with Phase 0 if convenient (different files), but
  conventional ordering puts Phase 0 first to clear the framework
  primitive question.
- **Phases 3 and 4 are independent of each other** but both depend
  on Phase 0.
- **Phases 5 and 6 are independent of 3 and 4** but depend on
  Phase 0.
- **Phase 7 lands last.** It depends on every prior phase's
  cleanup having happened.

Phase 0 is the safest first move strategically — it's pure
framework work, no behavior change, and unlocks every subsequent
phase. Phase 1 retires the most prominent example of the
sentinel-as-RPC anti-pattern in the codebase.

After Phase 7 lands, the convergence note in `ui_framework.md`
is removed and the framework docs no longer carry a "the code
disagrees with this; here's why" caveat.
