# history screen on horns

Status: design — 2026-05-23
Audience: implementers of the port; readers of the resulting screen.

## Goal

Port the history explorer (`ox-cli/src/history_state.rs`,
`ox-cli/src/history_view.rs`, plus the event-loop wiring that owns
them) from the legacy event-loop integration to horns, in the
StructFS-native shape established by the settings screen. Reach
feature parity — list, scroll, select, expand/collapse, pretty/raw
toggle, full/truncated toggle, ledger banner, ascend — including
mouse.

The port keeps the legacy history implementation alive in the
binary for the duration of the work, behind a runtime toggle the
user can flip on the history screen. Both implementations stay
green and feature-equivalent; the toggle exists so any divergence
shows up immediately by visual comparison rather than by bug
report. Legacy retirement is a follow-up change, not in scope
here.

The port is also the occasion to do two pieces of framework work
that were always on the path to a second screen:

1. Reshape screen installation so a *screen* is a **mount** rather
   than an ad-hoc collection of paths plus a `SettingsHandle`. The
   first install (settings) is migrated to the new shape as part
   of this work.
2. Add a `HitTarget` variant to `horns_core::View` so mouse
   routing is declarative rather than a render side-effect, and
   route clicks through it in the host loop.

## Motivation

The legacy history wiring is the last screen-shaped feature in
`ox-cli` that does not run on horns. It carries the patterns the
framework was designed to eliminate:

- A stateful `HistoryExplorer` owned by the event loop, fed slice
  parameters frame by frame.
- A `HistoryHitMap` returned as a side-effect of drawing, consumed
  by mouse dispatch.
- Per-frame branching in `tui::draw` that knows about history
  specifically.

These work, but they are the reason `event_loop.rs` is 1000+ lines
and the reason adding a new screen always seems to require touching
the legacy loop. Porting history is the last step before that loop
can shrink to a switchboard.

## Dual-implementation during port

For the duration of this work both implementations live in the
binary:

- **Legacy.** The current `HistoryExplorer` /
  `history_view::draw_history` / `HistoryHitMap` code in
  `ox-cli` is untouched. The legacy event loop continues to
  drive it when active.
- **Horns.** The new history screen mount under
  `ui/_horns/history/`. Installed at boot regardless of which
  implementation is active so its subscriptions can warm
  derivations in the background.

The active implementation is held at the broker path
`ui/history_engine` with value `"legacy"` or `"horns"`. A binding
on the history screen (chord TBD during implementation — likely
F12 or a `g`-prefixed chord) writes a toggle command that flips
the value. Both implementations bind the chord; either one being
visible lets the user flip to the other.

The host state machine (`main.rs`) gains a third state alongside
`Legacy` and `Settings`: `HistoryOnHorns`. Transitions:

- `ScreenSnapshot::History(_)` with `ui/history_engine = "legacy"`
  → stay in Legacy (existing event loop draws history in place).
- `ScreenSnapshot::History(_)` with `ui/history_engine = "horns"`
  → pivot to HistoryOnHorns (host loop yields the terminal to
  `run_horns_screen_loop` pointed at the history mount).
- `ScreenSnapshot::Settings(_)` → pivot to Settings as today.
- `ScreenSnapshot::Inbox(_)` or `Thread(_)` → return to Legacy.
- Toggle write while in either history state → exit current
  state, re-enter on the other side, both with the same
  `selected` / `expanded` since those live in the broker (legacy
  in `UiSnapshot`, horns in `state/<thread>/...`).

The toggle is **per-thread-visit, not persistent across
restarts**. The startup value defaults to `"legacy"` until
retirement. Persisting the preference is a trivial follow-up
(add `ui/history_engine` to a settings store) but unnecessary
for the comparison purpose.

### Pre-port modularity

The existing history code is already adequately factored:
`LogCache`, `HistoryLayout`, and `parse_log_entries` are
broker-agnostic and reusable. The coupling that prevents
coexistence is at the *dispatch* layer — `event_loop.rs:140`
owns a single `HistoryExplorer`, `tui.rs:127` calls
`draw_history` unconditionally on `ScreenSnapshot::History`,
and `event_loop.rs:1067` routes mouse via the returned hit map.

The pre-port refactor confines its changes to the dispatch
layer:

1. Add the `ui/history_engine` path and a write-handler that
   normalizes the value (defaulting to `"legacy"` on any
   read miss or invalid value).
2. Gate the legacy history calls in `event_loop.rs` and
   `tui.rs` on `ui/history_engine = "legacy"`. When `"horns"`,
   the legacy code path returns `LegacyExit::ToHorns(History)`
   the same way it currently returns `LegacyExit::ToHorns` for
   settings.
3. Generalize `horns_loop::run_horns_settings_loop` into
   `run_horns_screen_loop(screen: &Screen)` (this is the same
   work the rest of the spec requires).

That's the entire pre-port refactor. No new traits, no
extracting "engine" interfaces, no moving history code into a
new crate. The legacy implementation stays exactly where it
is; what changes is who decides whether to call it.

## What S-tier looks like

The history screen on horns is determined by five structural
decisions, in order of leverage:

1. **A screen is a mount.** Identity is the mount prefix. State
   and derivations both live in the broker subtree under that
   prefix. Tear-down is `unmount`. No `install_id` parameter, no
   `SettingsHandle { subscription_ids }` bookkeeping that lives
   outside the mount.
2. **Snapshot scope is per-mount via the existing
   `<mount>/snapshot/state` convention.** The screen mount opts
   into persistence by implementing that path (already used by
   `system` and `gate`). Derivations don't appear in the
   projection, so they don't appear in snapshots — and parser
   changes can't poison saved snapshots.
3. **Derivations are subscriptions writing to cache paths inside
   the mount.** `parse_log_entries` becomes a `ParseSubscription`.
   Per-entry layout becomes a `LayoutSubscription`. The renderer
   reads the cache and composes; it never parses, never wraps,
   never measures. Nothing lives in a Rust struct's
   `RefCell<HashMap>` — every memo is a broker write at a named
   path.
4. **The host loop is screen-agnostic.** It writes inputs to
   `<active_mount>/input/key` and `<active_mount>/input/area`,
   composes `<active_mount>/render/output` into the frame,
   watches `<active_mount>/_request_exit`, and routes mouse
   clicks via hit-target paths. Switching screens is "switch
   which mount is active." Both screens stay mounted; one is
   active at a time.
5. **Mouse is a `View` variant, not a side-channel.**
   `View::HitTarget { on_click, child }` carries clickable
   regions declaratively. The host walks the View tree
   post-render to translate click coordinates into the carried
   `Command`. Renderers never see mouse.

## Architecture

### Mount layout

```
ui/_horns/history/                       screen mount root
├── snapshot/state                       projection of the state subtree
├── input/key                            host writes key chords here
├── input/area                           host writes terminal Rect here
├── input/click                          host writes resolved click commands
├── _request_exit                        host watches; mount writes to signal exit
├── render/output                        mount writes serialized View here
├── render/tick                          wake the render subscription
├── focused                              cursor (Path) the registry indexes on
├── state/                               snapshotted subtree
│   ├── active_thread                    current thread id (single value)
│   └── <thread_id>/
│       ├── selected                     integer entry index
│       ├── expanded/<N>                 presence: entry N is expanded
│       ├── pretty/<N>                   presence: pretty-print toggle on
│       └── full/<N>                     presence: show full content
└── cache/                               ephemeral subtree (NOT snapshotted)
    ├── parsed/<N>                       LogDisplayEntry for active thread
    └── layout/<N>                       laid-out lines + hit-target geometry
```

`<mount>/snapshot/state` projects everything under `state/` — and
nothing else. Restore writes back to `snapshot/state`; the mount
reconstitutes `state/`. The cache subtree is implicitly ephemeral
because it is not included in the projection.

This mirrors `ox-inbox`'s convention exactly: implementing
`snapshot/state` is opt-in; the snapshot orchestrator's allowlist
controls whether the screen participates in `context.json`. Whether
the history screen mount goes in the allowlist is a separate
decision tracked under [Snapshot orchestration](#snapshot-orchestration).

### Screen mount = ScreenStore

A horns screen is a `ScreenStore`, a small store struct that:

- Implements `structfs_core_store::{Reader, Writer}` so the
  broker's `mount(prefix, store)` accepts it directly.
- Backs reads and writes of its subtree with an in-memory map
  (effectively the same shape as a `MemoryStore`).
- Implements `snapshot/state` as a virtual read that builds a
  single Value from the `state/` subtree (and a write that
  unpacks it).
- Holds the `SubscriptionId`s it registered with the broker
  dispatcher and unregisters them on drop.

The screen is mounted with
`broker.mount(prefix, ScreenStore::new(...))`. On
`broker.unmount(prefix)`, the store is dropped, the subscription
ids are unregistered, and the screen is gone. One handle, one
lifecycle.

The settings screen migrates to the same `ScreenStore` shape in
this work. The current `SettingsHandle { subscription_ids: Vec<…> }`
and the scattered path constructors (`render_output_path`,
`input_key_path`, …) become methods on a `Screen` that joins the
mount prefix with a relative path. There is no half-generalized
state where one screen uses installs and another uses mounts; both
move together.

### Derivation subscriptions

#### ParseSubscription

- **Watches:** `PrefixSuffix { prefix: threads, suffix: log/entries }`
  — matches `threads/<any_id>/log/entries`, the bare path read
  today by `view_state.rs:140` / `view_state.rs:169`. This shape
  is necessary because a subscription's `watches()` is fixed at
  registration; the active-thread filter happens inside `handle`.
- **Reads:** the triggering path's last component to extract the
  thread id; `state/active_thread`; the new array at the
  triggering path; the count of already-parsed entries at
  `cache/parsed/`.
- **Writes:** when the triggering thread id matches
  `state/active_thread`, one `cache/parsed/<N>` per new entry,
  using the same `parse_log_entries` function that exists today.
  When ids don't match, the handler returns no writes.

`log/entries` is currently written as a single `Value::Array`,
not as individual paths under a prefix. The subscription does
the same tail-diff `LogCache::sync` does today: the array
length minus `cache/parsed/`'s entry count yields the new tail;
only those entries are parsed.

A second subscription, watching `Exact(state/active_thread)`,
clears `cache/parsed/` and `cache/layout/` when the active
thread changes. On the next firing of the ParseSubscription
(either because the new thread's log was written, or because
the bootstrap self-write described under
[Cold-start cache warm](#cold-start-cache-warm) fires it), the
cache repopulates for the new thread.

#### LayoutSubscription

- **Watches:** `Prefix(cache/parsed)`,
  `Prefix(state/<thread>/expanded)`,
  `Prefix(state/<thread>/pretty)`,
  `Prefix(state/<thread>/full)`, `Exact(input/area)`.
- **Reads:** `cache/parsed/<N>`, `state/.../expanded/<N>`,
  `state/.../pretty/<N>`, `state/.../full/<N>`, `input/area`.
- **Writes:** `cache/layout/<N>` — a `View` fragment for entry
  N (a `Stack` of styled lines with toggle controls wrapped in
  `HitTarget` variants for the per-entry clickable regions).
  Coordinates for the hit targets are *not* in this cell;
  they're determined by where ratatui draws the fragment, which
  the host's post-render walk resolves to absolute terminal
  coordinates.

The watched set is broader than per-entry, so a width change
or a toggle on any entry will re-fire the subscription. The
handler inspects the change and re-lays only affected entries
— for a toggle on N, recompute `cache/layout/<N>`; for an area
change, recompute all visible entries.

The cost of layout is bounded by visible-window size, not by
total log length. Off-screen entries are not laid out until they
scroll into view.

#### RenderSubscription

- **Watches:** `Prefix(cache/layout)`,
  `Exact(state/<thread>/selected)`, `Exact(focused)`,
  `Exact(input/area)`, `Exact(render/tick)`.
- **Reads:** the active thread's visible window of
  `cache/layout/<N>`, `state/.../selected`, the ledger banner,
  the scroll offset.
- **Writes:** the composed `View` to `render/output`.

This is the same shape as the existing settings
`RenderSubscription`; it just has a longer `watches()` array.
The serialized View carries `HitTarget` variants for every
clickable region (per-entry summary rows, pretty/raw toggle,
full/truncated toggle).

### View vocabulary additions

```rust
pub enum View {
    // ... existing variants ...

    /// Wraps a child View with a clickable region. The host walks
    /// the post-render View tree, intersects mouse coordinates
    /// with the rendered geometry of each HitTarget, and writes
    /// the `on_click` UiCommand to a known dispatch path.
    HitTarget {
        on_click: UiCommand,
        child: Box<View>,
    },
}
```

`HitTarget` wraps any child View. The geometry it occupies is
the child's geometry as drawn — the translator already knows
where each node lands, so click resolution falls out of
post-render layout inspection; no separate hit map needs to be
produced.

`UiCommand` carries the existing typed command vocabulary
(`HistoryCommand::SelectAt(n)`, `HistoryCommand::ToggleExpand(n)`,
`HistoryCommand::TogglePretty(n)`, `HistoryCommand::ToggleFull(n)`).
The host writes the resolved command to
`ui/_horns/<active_mount>/input/click` (a new sibling of
`input/key`), which a click subscription consumes.

### Host loop

The host loop's job shrinks to:

```rust
loop {
    poll crossterm  →  write to <active>/input/{key,click,area}
    pull <active>/render/output            →  compose into terminal frame
    watch <active>/_request_exit           →  switch active screen or exit
    on screen switch                       →  flip the `<active>` reference
}
```

When invoked, the loop knows nothing about settings or history
specifically. It takes a `&Screen` (the active mount) and
operates on its standard paths. Settings exits to the legacy
inbox screen the same way it does today
(`UiCommand::Global(GlobalCommand::GoToInbox)` write); history
exits the same way.

What *does* still distinguish settings, history-on-horns, and
the legacy screens is the **outer state machine** in `main.rs`
that decides which loop owns the terminal:

- `ScreenSnapshot::Inbox` / `Thread` → legacy event loop.
- `ScreenSnapshot::Settings` → `run_horns_screen_loop(settings)`.
- `ScreenSnapshot::History` and `ui/history_engine = "horns"` →
  `run_horns_screen_loop(history)`.
- `ScreenSnapshot::History` and `ui/history_engine = "legacy"`
  → legacy event loop, with its existing history branch
  active.

Each loop returns control to `main.rs` when it observes a
condition that requires a state change (screen change,
engine-toggle write, exit request). The outer state machine
re-dispatches based on the new state.

### Settings migration

Settings is reshaped from "install into broker side-tables" to
"mount a `ScreenStore` at `ui/_horns/settings/`." The migration:

- `SettingsHandle { subscription_ids }` is deleted; the mount
  handle carries the same ids inside the store and unregisters
  on drop.
- The path constructors in `settings/mod.rs`
  (`render_output_path`, `input_key_path`, `input_area_path`,
  `render_tick_path`, `theme_path`, `bindings_prefix`,
  `commands_prefix`, `cursor_path`) become methods on `Screen`
  that join the mount prefix with a relative path. Existing
  callers that take a `&Path` keep working because the joined
  path is identical.
- The `horns_loop::run_horns_settings_loop` function becomes
  `run_horns_screen_loop(screen: &Screen)`; settings calls it
  with the settings screen, history calls it with the history
  screen.
- The host state machine in `main.rs` learns to dispatch to
  either screen based on `UiSnapshot.screen`.

The migration is one PR with the history port, not a separate
prior PR — splitting it would create a half-generalized state
with two registration mechanisms alive simultaneously.

## Data flow

A complete cycle for "user presses `j` to move selection down":

1. Host loop's crossterm poll receives `KeyEvent`.
2. Host writes `KeyChord` to `ui/_horns/history/input/key`.
3. `KeyDispatchSubscription` (already exists for settings;
   reused per-screen via the mount) fires, looks up the binding
   under `<mount>/bindings/`, resolves to
   `HistoryCommand::SelectNext`.
4. Command runs: `state/<tid>/selected = current + 1`.
5. `RenderSubscription` fires on the `selected` write, re-reads,
   composes a new View (same layout cells, new highlight
   position), writes `render/output`.
6. `horns_ratatui::ViewRenderSubscription` (mounted at install)
   fires on `render/output`, draws to terminal.

A complete cycle for "user clicks the pretty/raw toggle on entry 7":

1. Host loop's crossterm poll receives `MouseEvent`.
2. Host reads the most recent View from `<mount>/render/output`.
3. Host walks the View tree, intersects click coordinates with
   `HitTarget` regions, finds `HistoryCommand::TogglePretty(7)`.
4. Host writes the command to `ui/_horns/history/input/click`.
5. Click subscription dispatches into the command registry.
6. Command runs: toggles presence at `state/<tid>/pretty/7`.
7. `LayoutSubscription` fires on `state/<tid>/pretty/7`, re-lays
   entry 7, writes `cache/layout/7`.
8. `RenderSubscription` fires on `cache/layout/7`, re-composes,
   writes `render/output`. Draw follows.

A complete cycle for "user switches threads":

1. Inbox screen writes `HistoryCommand::OpenThread { id }` (or
   the equivalent for whichever screen initiated the switch).
2. A handler on history's input writes `state/active_thread = id`.
3. Active-thread subscription fires, deletes `cache/parsed/`
   and `cache/layout/`, then synthesizes a self-write of the
   new thread's `threads/<id>/log/entries` array to its own
   path (the bootstrap pattern from
   [Cold-start cache warm](#cold-start-cache-warm)).
4. ParseSubscription fires on that self-write, sees the full
   array vs. an empty cache, parses, writes `cache/parsed/<N>`
   for each entry.
5. LayoutSubscription fires per parsed entry, writes
   `cache/layout/<N>`.
6. RenderSubscription fires, composes, writes `render/output`.

Per-thread state under `state/<thread_id>/` is untouched by
thread switches — switching back to a prior thread restores
selection and expansion exactly as today.

## Thread switch semantics

Today (`history_state.rs:244-251`): single `HistoryExplorer`
instance, cache cleared on switch, per-thread selection/expansion
preserved by virtue of living in the broker's `UiSnapshot`.

After the port: identical behavior. State subtree is keyed by
`<thread_id>`; cache subtree is scoped to the active thread and
cleared on switch by the active-thread subscription.

This is the minimal-deviation choice. Caching parses for inactive
threads (warm second-visit) is a UX improvement deferred to
follow-up work; it would localize to switching the cache key
from `cache/parsed/<N>` to `cache/<thread>/parsed/<N>` and
bounding with an LRU, no architectural change.

## Snapshot orchestration

The screen mount implements `snapshot/state` per the
`PARTICIPATING_MOUNTS` convention in `ox-inbox/src/snapshot.rs`.
The question of whether history screen state actually
participates in `context.json` is **out of scope for this port**
— the mount is ready for it, the orchestrator's allowlist is the
place to decide, and the existing behavior (UI state lives in
`UiSnapshot` and is not persisted to disk across CLI invocations)
is preserved by not adding the mount to the allowlist.

If a future change wants history selection/expansion to persist
across restarts, the change is: add the appropriate alias to
`PARTICIPATING_MOUNTS`. No code in this port needs to know.

## What changes, what stays, what's new

**Stays unchanged (legacy implementation):**

- `crates/ox-cli/src/history_state.rs` —
  `HistoryExplorer`, `LogCache`, `HistoryLayout`. Untouched
  during this work. Retirement is a follow-up.
- `crates/ox-cli/src/history_view.rs` — `draw_history`,
  `HistoryHitMap`, `HitEntry`, `ToolbarHit`. Untouched.
- The `history_explorer` ownership in `event_loop.rs` and the
  `draw_history` call in `tui.rs` stay; they are now
  gated on `ui/history_engine = "legacy"`.

**Migrated (in place):**

- `parse_log_entries` (in `crates/ox-cli/src/parse.rs`) stays
  where it is and is *shared* between legacy and horns
  callers. The horns `ParseSubscription` calls it; the legacy
  `LogCache::sync` calls it. Tests against `parse_log_entries`
  carry over unchanged.
- `crates/ox-cli/src/settings/{mod.rs,bootstrap.rs}` is
  reshaped from the install pattern to the mount pattern; the
  renderers, commands, and bindings under `settings/` keep
  their current shape and re-register against the screen
  mount.
- `crates/ox-cli/src/horns_loop.rs` is generalized to
  `run_horns_screen_loop(screen: &Screen)`. Settings calls it
  with its screen; history-on-horns calls it with the history
  screen.

**New:**

- `crates/horns-core/src/screen.rs` — `Screen` (the
  mount-handle type), `ScreenStore` (the backing store).
- `crates/horns-core/src/view.rs` — `HitTarget` variant.
- `crates/horns-ratatui/src/render.rs` — `HitTarget` rendering
  (transparent: draws the child, records the geometry).
- `crates/ox-cli/src/history/` — `mod.rs`, `bindings.rs`,
  `bootstrap.rs`, `commands.rs`, `renderers/`,
  `subscriptions.rs` (ParseSubscription, LayoutSubscription).
  This module sits alongside `history_state.rs` /
  `history_view.rs` rather than replacing them.
- A small toggle path/handler:
  `crates/ox-cli/src/history_engine.rs` (or under
  `ox-ui`/`ox-types` if cleaner) holding the
  `HistoryEngine` enum and its serde wiring at
  `ui/history_engine`.

**Deferred to legacy retirement (separate change):**

- Deletion of `history_state.rs`, `history_view.rs`, and the
  gating branches.
- Removal of the `ui/history_engine` toggle path itself.
- Removal of the toggle key binding.

## Mouse routing

The host walks the View tree returned from
`<mount>/render/output` after each draw. The walk produces a
flat `Vec<(Rect, UiCommand)>` of hit-target regions; on
`MouseEvent::Down`, the host finds the innermost hit and writes
its `UiCommand` to `<mount>/input/click`. A click subscription
on that path dispatches the command through the registry.

Today's mouse handling in `event_loop.rs:1067` (a function
taking `&HistoryHitMap` directly) is deleted. The new mouse
path is screen-agnostic and the same code handles future
screens that introduce clickable regions.

The host's tree walk is a small function in `horns_ratatui`
that mirrors the translator's geometry — adding it alongside
the translator keeps the two in lockstep. A test compares
"View → drawn → walk" against "View → walk directly" to guard
against drift.

## Cold-start cache warm

On screen mount (or on thread switch into a thread with
existing log entries), the cache subtree is empty. The
`ParseSubscription` is write-triggered, so it does not fire
spontaneously on existing entries.

The mount's bootstrap reads the current array at
`threads/<active_thread>/log/entries` and synthesizes one
self-write back to the path. This fires the parse subscription,
which sees the full array vs. an empty cache and parses
everything. The self-write is idempotent (same value in, same
value out) and cascade-bounded.

For long logs the cost is parsing N entries on mount, bounded
by the time the user takes to scroll into view of any given
entry. Visible-window layout happens after parsing per-entry
layout; both are fast for terminal-sized N. If profile data
shows mount-time parsing as a bottleneck for unusually long
logs, a future change introduces a side-car parse cache on
disk, separate from the state snapshot — but not in this port.

## Compatibility and rollout

The port lands in one PR. Settings migrates to the mount shape
in the same PR; `run_horns_settings_loop` is renamed and
generalized; `event_loop.rs` gains the `ui/history_engine`
gate around its existing history branches; the horns history
mount and the toggle binding go in. The default value of
`ui/history_engine` is `"legacy"`, so behavior on first boot
after the PR is identical to today — the user opts in by
pressing the toggle on the history screen.

The dual-implementation period lasts until the user is
satisfied parity is reached. Retirement is its own PR: flip
the default to `"horns"`, observe, delete the legacy code,
remove the gate and the toggle. That PR is small and
mechanical because everything has been kept ready for it.

The risk is that some interaction (a key chord, a mouse case,
a ledger-banner edge case) doesn't carry over to horns.
Mitigations are baked into the dual-implementation choice:

- The user can flip back to legacy mid-thread if the horns
  version misrenders, without losing context or restarting.
- Snapshot tests of the horns renderer's `View` output, plus
  the existing legacy tests, both run in CI — both implementations
  stay green continuously.
- The legacy `parse_log_entries` is reused unchanged, so
  message *content* is identical between the two; any
  divergence is in layout, hit targets, or scroll behavior
  and surfaces by direct comparison.

## Testing

- **Unit tests on subscriptions.** `ParseSubscription::handle`
  takes a `SubCtx` and returns `Vec<Write>`; tests exercise
  the parse → write cascade without spinning up a terminal.
  Same for `LayoutSubscription` and the click handler.
- **Snapshot tests on the renderer.** The renderer is a pure
  `(Reader) -> View`; snapshot tests seed broker state, run
  the renderer, assert the View structure. The existing
  snapshot test harness under `crates/ox-cli/src/snapshots`
  extends to history.
- **Round-trip test for `snapshot/state`.** Build a history
  screen state in the broker, read `snapshot/state`, write it
  to a fresh mount, assert the projected state matches.
- **End-to-end test for thread switch.** Mount history with
  thread A, populate, switch to thread B, switch back to A:
  selection and expansion preserved, cache cold-then-warmed.
- **Hit-target geometry test.** Render a known View, walk for
  hit targets, assert each region matches the translator's
  drawn geometry.
- **Parity test between implementations.** Seed a thread with
  a fixture log. Drive identical input sequences through both
  implementations. Assert the rendered terminal buffers (or a
  semantic projection of them — visible entries, selection,
  expanded set) match. This is the test that polices
  divergence during the dual period.

Existing tests for `parse_log_entries`,
`HistoryExplorer::sync`, and `HistoryLayout` all remain
green — the legacy code is untouched.

## Out of scope

- **Branch tree visualization.** ox-history supports a tree of
  entries (branches via parent pointers); the screen today
  only shows the active branch as a linear list. Branch viz
  is its own project.
- **Mouse drag, mouse wheel for sub-entry scroll.** Click
  only; wheel scroll on the active-window axis is preserved
  (it's handled by the host loop, not the screen).
- **Search.** Today's `draw_history_search` overlay
  (`tui.rs:248`) is a separate dialog; it remains in legacy
  shape until the search dialog itself is ported.
- **Persisting history UI state across CLI restarts.** The
  mount is ready for it; the orchestrator allowlist decision
  is a separate change.
- **LRU cache for inactive threads' parses.** Cache stays
  scoped to active thread; second-visit is cold. UX-tier
  improvement, defer.
- **Legacy retirement.** Deletion of `history_state.rs`,
  `history_view.rs`, the `event_loop.rs`/`tui.rs` gating,
  the toggle path and binding, and any tests that exercise
  only the legacy path. Separate change once the
  dual-implementation comparison has run long enough for the
  user to be confident. Designed to be mechanical.
- **Persisting `ui/history_engine` across restarts.** The
  default is `"legacy"` each boot during the dual period.
  Persisting the user's last choice would be a one-line
  addition to a settings store; not done because retirement
  removes the toggle entirely.

## Open questions

- **Where the `HitTarget` walk lives.** The mouse walk needs
  to know what geometry the translator drew. Cleanest is a
  helper in `horns-ratatui` that re-runs the same layout
  pass the translator ran, recording regions. Alternative:
  translator emits the hit table as a sibling output
  alongside the rendered frame. The former is symmetrical
  with the translator; the latter is faster but couples the
  translator's output shape. Pick during implementation;
  either works.
- **Whether `input/click` is a distinct path from `input/key`.**
  A click is structurally different (carries a resolved
  command, not a chord that needs binding lookup), so a
  separate path keeps the KeyDispatchSubscription simple.
  Confirming this is the right factoring during
  implementation is fine — if it turns out clicks can route
  through the same dispatcher with a one-line "skip binding
  lookup if command already resolved" branch, that's
  acceptable too.
