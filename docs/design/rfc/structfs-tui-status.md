# StructFS TUI Rewrite — Status & Context

**Date:** 2026-04-06
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
- `ox-inbox/src/reconcile.rs` — hash-based consistency check on startup

## What's Next

### Phase C: StructFS TUI Rewrite

**Spec:** `docs/superpowers/specs/2026-04-06-structfs-tui-design.md`

Three implementation plans:

1. **Plan C1: BrokerStore** (plan written, ready to execute)
   - `docs/superpowers/plans/2026-04-06-broker-store-plan-c1.md`
   - New `ox-broker` crate — async StructFS router
   - 5 tasks, ~20 tests
   - Adapts appiware's `broker_store.rs` (at `../appiware/host/src/broker_store.rs`)
     to use ox's Record/Value/StoreError types instead of generic serde
   - Key types: BrokerStore, BrokerInner, ClientHandle (async, scoped), ServerHandle
   - Produces: working async broker that routes reads/writes between sync stores

2. **Plan C2: Stores** (plan not yet written)
   - UiStore, InputStore, ThreadStore (with StoreBacking trait), ConfigStore
   - Extended HistoryProvider with `history/turn/*` for streaming state
   - ApprovalStore for per-thread permission flow
   - All implement sync Reader/Writer, tested in isolation

3. **Plan C3: Integration** (plan not yet written)
   - Rewrite TUI event loop on top of broker
   - Mount stores, wire agent workers with scoped ClientHandles
   - Replace App struct, AppEvent channels, ThreadView mirrors

### Key Architecture Decisions (from spec)

- **Broker as bus:** All state in stores, all interaction through path reads/writes
- **Sync stores, async broker:** Stores implement Reader/Writer (unchanged). Broker
  wraps them in async tasks. Wasm guests stay synchronous.
- **Scoped clients:** Agent workers get ClientHandle scoped to `threads/{id}/`.
  They write `history/append`, broker resolves as `threads/{id}/history/append`.
  Worker doesn't know its full path (Plan 9 namespace model).
- **Command protocol:** Writes carry preconditions + transaction IDs for idempotency.
  `write(path!("ui/select_next"), {from: "t_abc", txn: "a7f3"})` — UiStore validates
  precondition, applies atomically.
- **No PersistenceStore:** Stores have platform-specific backings (StoreBacking trait).
  ThreadStore owns the portable bundle. Persistence is construction, not runtime.
- **Turn state in HistoryProvider:** `history/turn/streaming`, `history/turn/thinking`,
  `history/turn/tool`, `history/turn/tokens`. Commit finalizes to ledger. No separate
  LiveStore.
- **Branching:** Child thread references parent ledger through broker. Copy-on-delete
  materializes parent entries into child before deletion.
- **Error propagation:** Result flows back through write chain. No special error bus.
- **InputStore is a store:** Holds binding table, queryable for help. Keeps broker
  agnostic and TUI event loop platform-independent.

### Reference Files

- **Spec:** `docs/superpowers/specs/2026-04-06-structfs-tui-design.md`
- **Plan C1:** `docs/superpowers/plans/2026-04-06-broker-store-plan-c1.md`
- **Appiware broker (reference):** `../appiware/host/src/broker_store.rs`
- **Appiware store traits:** `../appiware/store/src/store.rs`
- **Current TUI:** `crates/ox-cli/src/app.rs`, `tui.rs`, `agents.rs`
- **Snapshot RFC:** `docs/design/rfc/snapshot-protocol.md`
- **Portable state spec:** `docs/superpowers/specs/2026-04-05-portable-agent-state-design.md`

## Execution

Start a new session, read `docs/superpowers/plans/2026-04-06-broker-store-plan-c1.md`,
and execute with subagent-driven development. Use `/jevan` persona. After C1,
write Plans C2 and C3.
