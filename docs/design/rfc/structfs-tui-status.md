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

### Phase C: StructFS TUI Rewrite (CURRENT — C1/C2/C3a/C3b/C4/C5/C6/C7 complete)

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

#### C5: Draw Rewrite (complete, 10 tests in view_state + 60 total in ox-cli)
- `crates/ox-cli/src/view_state.rs` — ViewState struct, fetch_view_state(), parse_chat_messages(),
  StreamingTurn, InboxThread, parse_inbox_threads()
- `crates/ox-cli/src/tui.rs` — event loop fetches ViewState per frame, draw is pure (&ViewState in,
  Frame out), drain_agent_events replaces inline handle_event, borrow scoping for pending_action
- `crates/ox-cli/src/inbox_view.rs` — draw_inbox/draw_filter_bar take &ViewState
- `crates/ox-cli/src/tab_bar.rs` — draw_tabs takes &ViewState
- `crates/ox-cli/src/app.rs` — 7 fields removed (selected_row, inbox_scroll, cached_threads,
  last_content_height, last_viewport_height, should_quit, scroll), 6 methods removed
  (send_input, refresh_visible_threads, ensure_selected_visible, get_visible_threads,
  open_selected_thread, archive_selected_thread), StreamingTurn + drain_agent_events added
- `crates/ox-cli/src/state_sync.rs` — deleted (-143 lines)
- Net: -151 lines

#### C6: Events Through Broker (complete, 61 ox-cli + 28 ox-broker + 53 ox-ui tests)
- `crates/ox-broker/src/async_store.rs` — AsyncReader/AsyncWriter traits, BoxFuture
  (broker-internal, not exported to StructFS)
- `crates/ox-broker/src/server.rs` — async_server_loop (reads inline, writes spawned as tasks)
- `crates/ox-broker/src/lib.rs` — mount_async for async stores
- `crates/ox-ui/src/approval_store.rs` — AsyncReader + AsyncWriter with deferred write:
  write("request") blocks until write("response") resolves via tokio oneshot
- `crates/ox-cli/src/agents.rs` — CliEffects writes turn/*, approval/*, inbox/* through broker;
  no more event_tx/control_tx mpsc channels; agent_worker commits turns and writes inbox
  metadata through broker
- `crates/ox-cli/src/view_state.rs` — reads turn/thinking, turn/tool, turn/tokens,
  approval/pending from broker; no more thread_views/streaming_turns references
- `crates/ox-cli/src/app.rs` — deleted: AppEvent, AppControl, ApprovalResponse, ApprovalState,
  ThreadView, StreamingTurn, event_rx, control_rx, thread_views, streaming_turns,
  pending_approval, handle_event, drain_agent_events, update_streaming
- `crates/ox-cli/src/tui.rs` — approval dialog reads from ViewState, writes response through
  broker; no drain_agent_events or control_rx polling
- Net: -439 lines

#### C7: ThreadRegistry (complete, 5 tests in thread_registry + 61 total ox-cli)
- `crates/ox-cli/src/thread_registry.rs` — AsyncStore at `threads/`, owns per-thread stores,
  lazy-mounts from disk on first access, internal routing (Namespace pattern)
- ThreadNamespace: per-thread store collection (system, history, model, tools, gate, approval),
  sync Reader/Writer for save/restore, approval routed async separately
- `crates/ox-context/src/lib.rs` — ToolsProvider now writable (accepts schemas writes)
- `crates/ox-cli/src/broker_setup.rs` — mounts ThreadRegistry at `threads/`, removed global
  ApprovalStore (now per-thread inside ThreadNamespace)
- `crates/ox-cli/src/agents.rs` — worker skips mounting (ThreadRegistry lazy-mounts),
  writes tool schemas + API key through adapter after lazy mount
- `crates/ox-cli/src/thread_mount.rs` — deleted (replaced by ThreadRegistry)
- Fixes: thread history loading for previously-saved threads (lazy mount on view)

#### Phase 1: App Convergence (complete, 64 ox-ui + 61 ox-cli tests)
- Search state (chips + live_query) moved into UiStore with 6 new commands
- `SearchState` struct, `handle_search_key` deleted from ox-cli
- `active_thread`, `mode`, `input`, `cursor` removed from App (UiStore is single source of truth)
- `InputMode`, `InsertContext` enums deleted from app.rs (ViewState uses strings)
- `sync_mode_to_broker` deleted
- `open_thread` deleted (broker `ui/open` command only)
- `history_up`/`history_down` take explicit parameters, return new state
- `tui.rs` split: event_loop.rs (~507), key_handlers.rs (~295), dialogs.rs (~365), tui.rs (~151)
- App fields reduced from 13 to 8: pool, model, provider, input_history, history_cursor, input_draft, approval_selected, pending_customize
- **Spec:** `docs/superpowers/specs/2026-04-07-app-convergence-design.md`
- **Plan:** `docs/superpowers/plans/2026-04-07-app-convergence.md`

#### Phase 2: S-Tier Polish (complete, 64 ox-ui + 65 ox-cli tests)
- `cmd!` macro eliminates broker command boilerplate (broker_cmd.rs, ~90 lines)
- Rendering types extracted to types.rs (ChatMessage, ThreadView, CustomizeState, etc.)
- Dialog state (approval_selected, pending_customize) moved from App to event-loop-local DialogState
- Parsing functions extracted to parse.rs (parse_chat_messages, parse_inbox_threads, search_matches + 10 tests)
- App reduced to 6 fields: pool, model, provider, input_history, history_cursor, input_draft
- No file over 500 lines; largest is event_loop.rs at 445
- **Spec:** `docs/superpowers/specs/2026-04-07-s-tier-polish-design.md`
- **Plan:** `docs/superpowers/plans/2026-04-07-s-tier-polish.md`

#### Phase 3: StoreBacking + ConfigStore (complete, 75 ox-ui + 67 ox-cli + 48 ox-inbox tests)
- StoreBacking trait in ox-kernel for platform-agnostic persistence
- JsonFileBacking in ox-inbox (load/save with atomic write, 4 tests)
- ConfigStore in ox-ui with 4-layer cascade: ephemeral thread → saved thread → global → default (11 tests)
- ConfigStore mounted at `config/` in broker, initialized from CLI args
- ThreadRegistry routes model config reads/writes to ConfigStore via broker client
- Workers read config from ConfigStore at spawn (no baked-in config)
- App reduced to 4 fields: pool, input_history, history_cursor, input_draft
- "model" removed from snapshot PARTICIPATING_MOUNTS
- **Spec:** `docs/superpowers/specs/2026-04-08-storebacking-configstore-design.md`
- **Plan:** `docs/superpowers/plans/2026-04-08-storebacking-configstore.md`

#### Phase 4a: Config System Completion — Store Utilities + Config Handles (complete)
- New `ox-store-util` crate: ReadOnly, Masked, LocalConfig, StoreBacking (moved from ox-kernel)
- ConfigStore refactored to path-based namespace (model/id, gate/api_key — no set_ commands)
- Stores read config through Reader handle via with_config() builder
- ModelProvider reads from config handle, falls back to local fields for standalone
- ThreadRegistry wires ReadOnly<SyncClientAdapter> config handles at mount time
- Phase 3 ThreadRegistry redirect reverted — stores own their reads
- **Spec:** `docs/superpowers/specs/2026-04-08-config-completion-design.md`
- **Plan:** `docs/superpowers/plans/2026-04-08-config-completion-a.md`

#### Phase 4b: Config System Last Mile (complete, 14/14 quality gates)
- ModelProvider collapsed into GateStore — single source of truth for all LLM config
- GateStore: max_tokens on AccountConfig, convenience model/max_tokens paths, with_config()
- synthesize_prompt reads gate/model and gate/max_tokens (no model/ mount)
- figment integration: defaults → ~/.ox/config.toml → OX_* env vars → CLI flags
- TomlFileBacking persists runtime config to ~/.ox/config.toml (API keys excluded)
- ConfigStore persistence via StoreBacking
- ThreadRegistry wires ReadOnly config handles into GateStore
- Agent worker reads config through thread stores, not direct broker reads
- Config namespace: all paths under gate/ — no model/ section
- **Spec:** `docs/superpowers/specs/2026-04-08-config-completion-design.md`
- **Plan:** `docs/superpowers/plans/2026-04-08-config-completion-b.md`

#### Phase 4c: ConfigStore S-Tier Refactor (complete, 14/14 quality gates)
- Cascade<A, B> wrapper in ox-store-util — layered reads with fallback
- ConfigStore simplified: two flat layers (base + runtime), no masking, no thread scoping
- Thread config handles: Cascade<LocalConfig, ReadOnly<SyncClientAdapter>>
- Masking removed from ConfigStore — consumers use Masked wrapper
- _raw suffix hack removed — GateStore reads gate/api_key directly
- ConfigStore: ~80 lines (was ~240)
- **Plan:** `docs/superpowers/plans/2026-04-08-config-store-stier.md`

#### Phase 5: Accounts-First Config Redesign (complete, 14/14 quality gates)
- AccountConfig shrunk to {provider, key} — model/max_tokens moved to GateStore Defaults
- GateStore: defaults/{account,model,max_tokens} paths replace bootstrap/model/max_tokens
- Per-account key resolution from config handle (any account, not just default)
- figment types: GateConfig with accounts HashMap + DefaultsConfig
- CLI: --account replaces --provider/--api-key
- Config paths: gate/defaults/* namespace, gate/accounts/{name}/* namespace
- Legacy env var mapping (ANTHROPIC_API_KEY/OPENAI_API_KEY) removed
- Completion tools accept model/max_tokens as parameters
- Backwards-compatible snapshot restore (legacy "bootstrap" field)
- **Spec:** `docs/superpowers/specs/2026-04-08-accounts-config-design.md`
- **Plan:** `docs/superpowers/plans/2026-04-08-accounts-config.md`

## What's Next

### Remaining work:

Config system complete (Phases 4a–4c). Remaining feature-level work:
completions-as-tools unification, runtime config UI (model/provider switcher
in TUI), per-thread config persistence on save, web platform IndexedDB backing.

### Key Architecture Decisions
- **Scroll commands match visual direction**: scroll_up = visual up = see older
- **ViewState per-frame snapshot**: fetch_view_state reads all state from broker each frame
- **Draw is pure**: &ViewState in, Frame out — no &mut App, no side effects, testable
- **No mpsc channels for agent data**: all events through broker turn/* paths
- **Approval through broker**: deferred write blocks agent until TUI writes response
- **AsyncReader/AsyncWriter**: broker-internal traits, not StructFS — stores return detached futures
- **pending_action pattern**: UiStore sets a string field, TUI reads from ViewState after draw
- **Search through broker**: search_insert_char/search_save_chip/search_dismiss_chip through UiStore (no escape hatches)
- **Two InboxStore instances**: one in App/AgentPool, one in broker (same SQLite)
- **HostStore generic over backend**: `HostStore<B, E>` works with Namespace (ox-web) or SyncClientAdapter (ox-cli)
- **ThreadRegistry owns thread lifecycle**: lazy mount from disk, internal routing, single opaque mount at `threads/`
- **Workers don't mount**: ThreadRegistry lazy-mounts on first access, workers write config via adapter
- **SyncClientAdapter uses Handle::block_on**: works from plain OS threads (not tokio tasks)
- **ProviderConfig constructed directly**: workers don't read GateStore for transport config

### Reference Files
- **Spec:** `docs/superpowers/specs/2026-04-06-structfs-tui-design.md`
- **Plans:** `docs/superpowers/plans/2026-04-06-broker-store-plan-c1.md` (executed)
- **Plans:** `docs/superpowers/plans/2026-04-06-core-stores-plan-c2.md` (executed)
- **Plans:** `docs/superpowers/plans/2026-04-06-broker-wiring-plan-c3a.md` (executed)
- **Plans:** `docs/superpowers/plans/2026-04-06-async-event-loop-plan-c3b.md` (executed)
- **Plans:** `docs/superpowers/plans/2026-04-07-agent-worker-bridge.md` (executed)
- **Spec:** `docs/superpowers/specs/2026-04-07-draw-rewrite-design.md`
- **Plans:** `docs/superpowers/plans/2026-04-07-draw-rewrite.md` (executed)
- **Spec:** `docs/superpowers/specs/2026-04-07-events-through-broker-design.md`
- **Plans:** `docs/superpowers/plans/2026-04-07-events-through-broker.md` (executed)
- **Spec:** `docs/superpowers/specs/2026-04-07-thread-registry-design.md`
- **Plans:** `docs/superpowers/plans/2026-04-07-thread-registry.md` (executed)
