# StructFS TUI Rewrite — Status & Context

**Date:** 2026-04-07
**Purpose:** Handoff document for continuing implementation in a new session

## What's Done

### Plan A: Snapshot Lens (merged to main)
Per-store `snapshot` path handling in SystemProvider, ModelProvider,
HistoryProvider (read-only), GateStore (keys excluded). Shared helpers
in `ox-kernel/src/snapshot.rs`. ToolsProvider returns None.

### Plan B: Thread Directory + Snapshot Coordinator (merged to main)
- `ox-inbox/src/ledger.rs` — content-addressed append-only JSONL with SHA-256 hash chain
- `ox-inbox/src/thread_dir.rs` — context.json (ContextFile with serde flatten) + view.json
- `ox-inbox/src/snapshot.rs` — save/restore namespace state to/from thread directories
- `ox-inbox/src/schema.rs` — last_seq/last_hash columns in SQLite
- `ox-cli/src/agents.rs` — replaced save_history with snapshot coordinator

### Architecture Fix: Single Source of Truth (merged to main)
- Removed `threads/{id}/messages` read/write from InboxStore (reader.rs, writer.rs)
- Deleted `ox-inbox/src/jsonl.rs`
- TUI reads messages from ledger.jsonl directly (`load_thread_messages` in app.rs)
- SaveResult write-through to SQLite via `SaveComplete` event on AppEvent
- Startup reconciliation in `ox-inbox/src/reconcile.rs`
- Real thread title flow through save_thread_state
- Message count derived from last_seq in inbox display

### Phase C: StructFS TUI Rewrite (CURRENT — C1/C2/C3a/C3b/C4 complete)

**Spec:** `docs/superpowers/specs/2026-04-06-structfs-tui-design.md`

#### C1: BrokerStore (complete, 23 tests)
- `crates/ox-broker/` — new crate
- Async router: reply channel on Request, component-based routing, scoped clients
- `BrokerStore`, `ClientHandle`, `Request` public API
- Path-typed mount/unmount, zero string conversion in hot paths
- Backpressure test, cross-store communication test (block_in_place pattern)

#### C2: Core Stores (complete, 53 tests in ox-ui + 16 in ox-history)
- `crates/ox-ui/` — new crate
- **Command protocol** (`command.rs`): parse preconditions + txn from writes, TxnLog dedup
- **UiStore** (`ui_store.rs`): screen, mode, selection, scroll, input, cursor, modal, status,
  pending_action. Commands: select_next/prev, open/close, enter/exit_insert, set_input,
  clear_input, insert_char, delete_char, scroll_up/down/to_top/to_bottom, page/half-page
  scroll, select_first/last, set_row_count, set_scroll_max, set_viewport_height,
  send_input/quit/open_selected/archive_selected (pending_action), show/dismiss_modal
- **InputStore** (`input_store.rs`): context-aware binding resolution (mode, key, screen),
  runtime bind/unbind/macro, CommandDispatcher callback, queryable bindings for help
- **ApprovalStore** (`approval_store.rs`): request/response/pending paths
- **TurnState** (`ox-history/src/turn.rs`): streaming, thinking, tool, tokens, commit

#### C3a: Broker Wiring (complete, 7 tests)
- `crates/ox-cli/src/bindings.rs` — default key binding table (normal/insert/approval modes,
  screen-specific, vim navigation g/G/Ctrl+d/u/f/b)
- `crates/ox-cli/src/broker_setup.rs` — BrokerSetup mounts UiStore + InputStore + InboxStore +
  ApprovalStore, InputStore gets block_in_place dispatcher
- Integration tests: all stores reachable, key dispatch through broker, screen-specific routing

#### C3b: Async Event Loop (complete, 99 total tests)
- `crates/ox-cli/src/key_encode.rs` — crossterm KeyEvent → string
- `crates/ox-cli/src/state_sync.rs` — bidirectional sync: UiStore → App (each frame) +
  App → UiStore (after App methods that change state)
- `crates/ox-cli/src/tui.rs` — `run_async` replaces sync `run`:
  - Key events → InputStore dispatch through broker
  - Text editing (compose/reply) → insert_char/delete_char through broker
  - Search text editing → handle_search_key (direct, search state not in UiStore yet)
  - Mouse scroll → broker, mouse click on approval → direct
  - Approval/customize dialogs → direct (dialog state machines)
  - pending_action field for app-level commands (send, open, archive, quit)
  - Scroll bar via ratatui Scrollbar widget, content-height-based scroll_max
- `crates/ox-cli/src/main.rs` — tokio runtime, BrokerSetup, run_async
- Dead code removed: old run(), handle_normal_key, handle_mouse, App mode transition methods

#### C4: Agent Worker Bridge (complete, 4 tests in thread_mount + 15 in ox-runtime + 3 in ox-broker)
- `crates/ox-context/src/lib.rs` — extracted `synthesize_prompt()` as standalone public function
- `crates/ox-runtime/src/host_store.rs` — `HostStore<B, E>` generic over backend (Namespace or
  SyncClientAdapter), owns tool_results directly, intercepts prompt synthesis
- `crates/ox-runtime/src/engine.rs` — `AgentState<B, E>` and `AgentModule::run<B, E>` generic
- `crates/ox-broker/src/sync_adapter.rs` — `SyncClientAdapter` (sync Reader/Writer over async
  ClientHandle via `Handle::block_on`), `BrokerStore` derives Clone
- `crates/ox-cli/src/thread_mount.rs` — `mount_thread()`/`unmount_thread()` for per-thread
  store lifecycle, `restore_thread_state()` via SyncClientAdapter
- `crates/ox-cli/src/agents.rs` — agent_worker uses scoped ClientHandle through broker,
  HostStore<SyncClientAdapter, CliEffects>, mounts/unmounts on worker thread
- `crates/ox-cli/src/main.rs` — restructured init: runtime + broker before App::new

## What's Next

### Remaining for full spec completion:

1. **Draw Rewrite** (highest value next)
   - Read directly from broker client instead of synced App fields
   - Eliminates the sync bridge (state_sync.rs becomes unnecessary)

3. **Search State in UiStore**
   - Move search.live_query and search.chips into UiStore
   - Eliminates the last handle_search_key direct-mutation path

4. **StoreBacking Trait**
   - Platform-agnostic persistence abstraction
   - Stores cache in memory, backings are authoritative

5. **ConfigStore**
   - Read-only settings projection (theme, provider, model, bindings)

### Key Architecture Decisions
- **Scroll commands match visual direction**: scroll_up = visual up = see older
- **Bidirectional sync**: UiStore → App (each frame), App → UiStore (after App methods)
- **pending_action pattern**: UiStore sets a string field, TUI reads it and calls App methods
- **Search is the last escape hatch**: handle_search_key bypasses broker (7 lines)
- **Two InboxStore instances**: one in App/AgentPool, one in broker (same SQLite)
- **HostStore generic over backend**: `HostStore<B, E>` works with Namespace (ox-web) or SyncClientAdapter (ox-cli)
- **Workers mount their own stores**: worker thread calls `rt_handle.block_on(mount_thread(...))`, owns lifecycle
- **SyncClientAdapter uses Handle::block_on**: works from plain OS threads (not tokio tasks)
- **ProviderConfig constructed directly**: workers don't read GateStore for transport config

### Reference Files
- **Spec:** `docs/superpowers/specs/2026-04-06-structfs-tui-design.md`
- **Plans:** `docs/superpowers/plans/2026-04-06-broker-store-plan-c1.md` (executed)
- **Plans:** `docs/superpowers/plans/2026-04-06-core-stores-plan-c2.md` (executed)
- **Plans:** `docs/superpowers/plans/2026-04-06-broker-wiring-plan-c3a.md` (executed)
- **Plans:** `docs/superpowers/plans/2026-04-06-async-event-loop-plan-c3b.md` (executed)
- **Plans:** `docs/superpowers/plans/2026-04-07-agent-worker-bridge.md` (executed)
