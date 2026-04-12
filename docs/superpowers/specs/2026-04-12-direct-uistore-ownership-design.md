# Direct UiStore Ownership (Future)

**Date:** 2026-04-12
**Status:** Draft — future work, after screen architecture is complete
**Depends on:** S-Tier Screen Architecture (2026-04-12)

## Problem

UiStore is mounted in the async broker. Every UI state read/write round-trips through an async channel, even though UiStore is shell-local state that no agent worker reads. This creates:

- Async overhead for synchronous operations (scroll, selection, mode transitions)
- The `InputSession` dual-state problem (optimistic local cache needed for low-latency keystrokes)
- Extract-state-then-drop-borrow ceremony in the event loop
- `pending_action` as a polled field instead of a return value from write

## Design

Pull UiStore off the broker. Each platform shell owns it directly.

**TUI (ox-cli):** `let mut ui_store = UiStore::new();` in the event loop. `ui_store.write(cmd)` is synchronous. `ui_store.snapshot()` returns the current state for rendering. No broker, no async, no round-trip.

**Web (ox-web):** `Rc<RefCell<UiStore>>` — Wasm is single-threaded, so `borrow_mut()` is effectively synchronous. Same pattern ox-web already uses.

**Mobile (future):** Own a `UiStore` in the platform's event loop. Types are serde-serializable for FFI if needed.

**Broker stays for cross-thread stores:** threads, history, tools, approval, config, inbox. These are genuinely async (agent workers on separate OS threads). UiStore is not one of them.

## What this eliminates

- `InputSession` optimistic local cache — writes are synchronous, no need for a local copy
- `pending_action` field — `UiStore::write(cmd)` can return the action directly as an enum
- The ViewState extract-then-drop pattern — the event loop owns the store, no borrow conflicts
- `TextInputStore` as a sub-store inside UiStore — input state is just fields on the screen state, written synchronously
- Async overhead on every UI interaction

## What this preserves

- `UiStore` implements StructFS `Reader`/`Writer` — same typed protocol, same serde boundary
- `UiCommand` and `UiSnapshot` types unchanged — the screen-scoped architecture from the prior spec
- The store can still be mounted in the broker for debug/inspection if desired (read-only projection)
- Agent workers are unaffected — they never read UiStore

## Migration path

1. Remove UiStore mount from `broker_setup.rs`
2. Create UiStore directly in `event_loop.rs` (or wherever the shell's main loop lives)
3. Replace `client.write_typed(&oxpath!("ui"), &cmd)` with `ui_store.write(&path, record)`
4. Replace `client.read_typed(&oxpath!("ui"))` with `ui_store.snapshot()`
5. Remove `InputSession` — direct writes to UiStore are synchronous
6. Change `pending_action` from a polled field to a return value from `write`
7. Simplify the event loop — no more scoped borrow blocks, no more extract-then-drop
8. Update bindings dispatch — InputStore currently dispatches through the broker to UiStore; with direct ownership, this becomes a direct function call
