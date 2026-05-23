# Horns settings tests — quality debt

Open problems with the settings-screen integration tests, surfaced during
the broker-mounted horns refactor. Each is a known-incorrect shape that
ships green today because of incidental properties of the current code.
The first one fails closed (silently miss regressions); the second fails
open (flake on slow runs).

Neither blocks merging the broker-mounted loop work. Both are worth
fixing before the test surface grows further.

## Problem 1 — Tests render inline; production renders through `RenderSubscription`

**Location:** `crates/ox-cli/tests/horns_settings_ui.rs::render_settings`

The test helper constructs a `RendererRegistry` + `SettingsSnapshot` and
calls `.render()` directly:

```rust
async fn render_settings(client: &ClientHandle) -> View {
    let mut renderers = settings::RendererRegistry::new();
    settings::renderers::register_all(&mut renderers);
    let mut snap = fetch_settings_view_state(client).await;
    // … inline render against the local registry …
}
```

Production never does this. The framework's `RenderSubscription`
(horns-core/src/install.rs) serializes the View through
`structfs_serde_store::to_value`, writes the resulting `Record` to
`render_output_path()`, and the ratatui backend's
`ViewRenderSubscription` (horns-ratatui/src/install.rs) reads that path,
deserializes, and draws.

`press_chord` writes through the broker, so **dispatch** is
production-realistic. **Render** is half-bypassed.

### What this misses

- View serde round-trip regressions. A newly-added View variant whose
  serde representation doesn't survive `Value` round-trip would render
  fine in tests and crash/blank in production.
- `RenderSubscription` wiring breaks — e.g., a watch pattern dropped
  from `watches`, the render-tick coupling silently severed (KeyDispatch
  bumps tick → RenderSubscription is supposed to wake; if the tick path
  changes on one side, only one side notices).
- `LocalConfig::write`'s Map-flatten + `LocalConfig::read`'s
  re-assembly mangling a nested View field. Today the View's serde
  output is uniformly Map-shaped, so the flatten/unflatten preserves
  it — but any future View variant with a non-Map serialization at a
  sub-field could subtly corrupt.

### Why the inline path was used

Predates the broker-mount refactor. Inline rendering was the only way to
get a View when the broker reader didn't support enumeration
(`child_names_under` and friends couldn't fetch from a snapshot). Tasks
1-4 fixed the substrate; the test helper is leftover scaffolding.

### Fix shape (not a commitment)

Read the View from `settings::render_output_path()` after each
`press_chord`. Drop the inline registry/snapshot construction. The
production path becomes the tested path. Initial render fires when the
test seeds focus or render_tick (both already happen in setup).

Caveat: confirm `View` deserializes losslessly through
`client.read_typed::<View>(...)` before bulk-converting. If a variant
breaks, fix the View serde first.

---

## Problem 2 — `sleep(10ms)` for subscription-spawned async work

**Location:** `crates/ox-cli/tests/horns_settings_ui.rs::press_chord`,
also `crates/ox-cli/tests/settings_e2e.rs::poll_until` callers and any
helper that writes a chord then asserts.

```rust
async fn press_chord(client: &ClientHandle, chord: KeyChord) {
    client.write_typed(&settings::input_key_path(), &chord).await.unwrap();
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(10)).await;
}
```

The sleep is for *spawned* async work — subscriptions calling
`ctx.spawn(...)` to fire off network calls (account-test,
catalog-refresh) or other async cascades.

Subscription **handlers** are synchronous from the broker's write path:
by the time `client.write_typed(...).await` returns, every cascading
write the handler returned has been applied. That part is deterministic.
The sleep is for everything the handler spawned — fire-and-forget tasks
the broker has no handle on.

### Why 10ms today

Because today's spawns are fast: an in-process MockTransport for tests,
no real network. 10ms is comfortably more than the spawned future needs
to complete. It works.

### When it breaks

- A spawned task takes >10ms (slow CI runner, real network latency leaked
  into a test, garbage-collected pause on the test thread).
- Spawn depth grows: one subscription spawns work that writes a path
  watched by another subscription that spawns more work. The cascade now
  has multiple async steps; 10ms isn't enough.
- A future change makes a subscription handler itself async (currently
  it's `fn handle(...) -> Vec<Write>`, sync). The handler's own work
  would no longer settle before `write_typed` returns.

### What's missing from the broker

There is no "all spawned work for this cascade is done" primitive. The
broker takes `ctx.spawn(future)` and hands the future to the runtime;
the join handle is dropped. Tests have no way to await it.

### Fix shape (not a commitment)

Two layers:

1. **Broker tracks outstanding spawns.** A `JoinSet` or `AtomicUsize`
   the spawn method increments and the spawned future decrements
   on completion. Exposes `BrokerStore::wait_quiescent().await` that
   resolves when the count hits zero (with a Notify wakeup).

2. **`press_chord` calls `broker.wait_quiescent()` instead of sleeping.**
   Test signatures take `&broker` alongside `&client`. No timing
   constant in the codebase.

Open design question: long-lived background spawns (a periodic poller, a
file-watcher) shouldn't block test quiescence forever. The spawn surface
probably needs a `SpawnKind::Cascade | SpawnKind::Background` distinction;
`wait_quiescent` only waits on Cascade. Want to think through that
carefully before building it, since the wrong split means tests either
hang or finish early.

### Ordering

The fix requires a real broker feature, not just a test rewrite. Worth
doing the next time a test goes flaky — the flakiness is the right
forcing function. Doing it speculatively risks designing the
Cascade/Background split for hypotheticals.
