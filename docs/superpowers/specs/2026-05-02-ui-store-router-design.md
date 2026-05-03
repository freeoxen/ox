# UiStore router design

`UiStore` does five distinct jobs jammed into one type. This design splits
it along **contract** lines so the bug class that hid the settings-screen
inertness for an entire release becomes impossible by construction.

## Problem

`UiStore` today mixes three contracts on one writer:

- **State** — generic key/value reads that the rest of the system snapshots
  (`screen`, `active_thread`, `mode`, `selected_row`, `scroll`, `viewport`,
  `search_chips`, …). Already factored as sub-stores for `command_line/*`
  and `input/*` (active editor); inline for everything else.
- **Commands** — writes whose first path component is a verb
  (`select_next`, `toggle_shortcuts`, `approve`, `set_input`, `clear_input`,
  `go_to_settings`, …). Resolved by a 200-line `match cmd_name` in
  `resolve_path_command` that returns a `UiCommand` enum the store then
  dispatches.
- **Pending action mailbox** — single-slot `Option<PendingAction>` set by
  several command arms, drained transactionally by the event loop.

The contract mismatch caused the settings-screen bug. The new framework
writes path-shaped state to `ui/settings/cursor`,
`ui/settings/index/selected`, `ui/settings/_request_exit`, etc. Those land
on `UiStore::write`, miss every `match cmd_name` arm, and bottom out in
`resolve_path_command("settings", _)` → `unknown command path`. Every
keystroke on the settings screen silently no-op'd while the dispatch
layer reported `Handled`.

The immediate fix (already shipped) composes a `LocalConfig` sub-store
inside `UiStore` for `ui/settings/*`. That closes the bug. It does not
fix the underlying shape: `UiStore` is still a contract-mixed monolith
where the next state-shaped sub-region hits the same wall.

## Target shape

`UiStore` is a router over three sub-stores, one per contract:

```
ui/                         (UiStore — pure router; ~30 lines)
├── state/                  (UiStateStore — generic K/V + named state regions)
│   ├── screen
│   ├── thread/
│   ├── inbox/
│   ├── history/
│   ├── command_line/       (existing sub-store; moves under state/)
│   ├── editor/             (existing active-editor; moves under state/)
│   └── settings/           (the LocalConfig the immediate fix added)
│
├── commands/               (UiCommandStore — handler registry by verb path)
│   ├── nav/{go_to_inbox,go_to_thread,go_to_settings,go_to_history,quit}
│   ├── inbox/{select_next,select_prev,select_first,select_last,
│   │          search/insert_char,search/delete_char,search/save_chip,…}
│   ├── thread/{scroll_up,scroll_down,scroll_to_top,scroll_to_bottom,
│   │           set_viewport_height,set_scroll_max,…}
│   ├── editor/{set_input,clear_input,replace,edit,toggle_mode,submit}
│   ├── modal/{toggle_shortcuts,dismiss_shortcuts,toggle_usage,
│   │          dismiss_usage,toggle_thread_info,…}
│   └── approval/{approve,confirm}
│
└── pending/                (UiPendingMailbox — single-slot read-clear)
    └── action
```

The router (`UiStore`) owns nothing but the three sub-stores. Reads and
writes match the first path component, strip it, and delegate. No logic.

```rust
impl Writer for UiStore {
    fn write(&mut self, to: &Path, data: Record) -> Result<Path, StoreError> {
        match first_component(to)? {
            "state"    => self.state.write(strip(to), data),
            "commands" => self.commands.write(strip(to), data),
            "pending"  => self.pending.write(strip(to), data),
            unknown    => Err(StoreError::store(
                "ui", "write", format!("unknown sub-store: {unknown}"),
            )),
        }
    }
}
```

## Why this is the right cut

**One contract per sub-store.** No more "writes are sometimes commands,
sometimes state." That mismatch is the bug class. With three sub-stores:

- A state-shaped write that lands on `state/` always succeeds (any path
  is valid; the substrate is generic).
- A typed-command write that lands on `commands/` either matches a
  registered handler or returns `Err` immediately — no silent fall-
  through.
- A pending action write hits a single slot with read-clear semantics
  enforced in code, not by convention.

A bug of the settings-class becomes "I wrote to the wrong sub-store" —
a typed mistake the substrate rejects loudly.

**Commands become a registry, not a god-match.** Today's
`resolve_path_command` is a 200-line `match cmd_name { ... }` that has to
know about every command in the system. Replace with:

```rust
pub trait UiCommandHandler: Send + Sync {
    fn path(&self) -> &'static str;             // "inbox/select_next"
    fn run(
        &self,
        state: &mut UiStateStore,
        payload: &Value,
    ) -> Result<Vec<Effect>, StoreError>;
}

pub struct UiCommandStore {
    handlers: HashMap<&'static str, Box<dyn UiCommandHandler>>,
}
```

Registration at startup. Adding a new command is a struct + impl, not an
arm in a match. Same shape as the new framework's `Command` trait — one
mental model for the entire UI command surface.

**Pending becomes typed.** The `Option<PendingAction>` field, set in 11
places and read in one, becomes `UiPendingMailbox` with `set(action)` and
`take() -> Option<PendingAction>`. The read-then-clear lifecycle is a
method, not a side effect distributed across the codebase.

**Snapshot stays cheap.** `UiStore::read("")` becomes
`self.state.snapshot()`. No command surface in the picture; the renderer
pipeline keeps its current shape, just reads from `ui/state/*` instead
of `ui/*`.

## Migration

This is a real refactor — call it 1–2k lines of mostly-mechanical churn.
Do it in phases so each phase ships independently and the tree stays
green throughout.

### Phase R0 — composition foundation (shipped 2026-05-02)

- `UiStore` gains a `settings: LocalConfig` field; `read`/`write`
  delegate `settings/*` to the sub-store, mirroring `command_line/*`.
- Regression test `production_ui_store_routes_settings_writes` pins
  the substrate by mounting the real `UiStore` at `ui/`. Falsified by
  removing the write arm; restored.
- `dispatch::send_key` captures per-write failures, logs first
  failure at `error` level with the rejected path, `debug_assert!`s
  in dev. Substrate rejection is no longer silently warned-and-ignored.

### Phase R3 — `UiPendingMailbox` (shipped 2026-05-02, out of order)

R3 lands ahead of R1/R2 because it's the smallest, most self-contained
sub-store and serves as the worked example for the pattern. The
`Option<PendingAction>` field on `UiStore` becomes a typed
`UiPendingMailbox` (`crates/ox-ui/src/ui_pending.rs`) with
`set`/`peek`/`take`/`clear`/`as_value` methods. The 11 setter sites
sweep cleanly via codemod; the wire shape at `ui/pending_action`
is unchanged. Six new unit tests cover the mailbox in isolation.

A behavioral improvement falls out: `set` over a held action now
`tracing::warn!`s the supersession (previously silent), which catches
the "event loop didn't drain last tick" failure mode the prior bare
`Option<T>` field couldn't see.

### Phase R1 — extract `UiStateStore` (shipped 2026-05-02)

All six legacy `UiStore` fields (`inbox`, `screen`, `pending`,
`status`, `command_line`, `settings`) live on a private
`UiStateStore` struct, with `UiStore` holding it as a single
`state: UiStateStore` field. Field accesses across the file moved
through a `self.state.X` codemod. Wire shape unchanged; 411 ox-cli +
165 ox-ui tests stayed green.

### Phase R2 (method extraction) — shipped 2026-05-02

Every state-mutating method (snapshot, screen-guards, value-readers,
the five `handle_X` command handlers, `dispatch_command`,
`resolve_path_command`, `resolve_path_command_direct`,
`close_search_prompt`, `active_editor_mut`) moved from `impl UiStore`
to `impl UiStateStore`. `UiStore` is now a 50-line router whose
`Reader`/`Writer` impls delegate to `self.state` for everything
except the sub-store routing arms it owns directly.

What didn't ship in R2: replacing `resolve_path_command`'s 200-line
`match cmd_name` with a handler registry (`HashMap<&'static str,
Box<dyn UiCommandHandler>>`). The architectural payoff there is real
— per-verb test isolation, a `Command`-trait shape that mirrors the
new framework — but the migration is ~50 small struct + impl pairs
that needs dedicated time to land cleanly. Method extraction is the
prerequisite that's now done; handler-registry migration is the next
PR.

### Phase R4: namespace migration

Now move paths under `state/`, `commands/`, `pending/`:

- `ui/screen` → `ui/state/screen`
- `ui/select_next` → `ui/commands/inbox/select_next`
- `ui/pending_action` → `ui/pending/action`
- etc.

Codemod every caller: bindings table, settings/, command store,
broker_setup. The `UiStore::write` router becomes the trivial 5-line
dispatch shown above.

### Phase R5: cleanup

Remove the old top-level paths from `UiStore` (the compatibility shims
that kept Phase R1–R3 backwards-compatible). The router is final.

Each phase passes the existing test suite at every step. R4 is the
disruptive one — codemod-able in a single PR with the help of the
binding-table grep being exhaustive over the verb surface.

## What's not in scope

- Replacing the `UiCommand` enum with handler-driven dispatch end-to-end:
  Phase R2 introduces handlers but keeps the enum as the dispatch
  payload type. A future refactor can drop the enum and let handlers
  own their state mutation directly.
- Persistent UI state: today nothing under `ui/` persists across
  process restarts. Some state (e.g. last-active screen, settings
  cursor) might want to. Out of scope; flag as a separate decision.
- Auto-derived sub-store interfaces from a typed schema: tempting but
  premature. The three sub-stores are stable enough to hand-write.

## Why now

The settings-screen bug was the first instance of the contract-mixed
shape biting. The framework's path-shaped writes are going to keep
showing up — every screen the new framework rebuilds will need its own
state region under `ui/`. Carving them out as broker-sibling mounts (as
the original quick-fix attempted) flattens the natural composition tree
and hides ownership. Carving them in as `UiStore` sub-stores reuses the
existing pattern and keeps `ui/` semantically one tree.

The refactor pays for itself the first time someone adds a new screen
that needs state-shaped writes — they grep `command_line:` /
`settings:`, see the pattern, add their sub-store, done.
