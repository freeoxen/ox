# StructFS TUI Architecture

**Date:** 2026-04-06
**Status:** Draft

## Overview

Replace the ox-cli TUI's ad-hoc state management (App struct, event
channels, ThreadView mirrors) with a StructFS-native architecture where
all state lives in stores, all interaction flows through path-based
read/write operations, and an async BrokerStore routes operations between
participants.

The same stores and paths work for both the CLI (ratatui) and the web
playground (ox-web/Svelte). The rendering layer is the only
platform-specific code.

## Architecture

The application is a **BrokerStore** — an async StructFS request/response
multiplexer adapted from appiware's broker_store.rs. All participants
connect to the broker through client/server handles.

Three kinds of participants:

### Stores (servers)

Synchronous `Reader`/`Writer` implementations wrapped by the broker's
async server infrastructure. Each store owns its state and handles
reads/writes to its path prefix. The store doesn't know it's inside a
broker — it implements the standard StructFS traits.

Stores that need to communicate with other stores hold a `ClientStore`
handle and write to paths through the broker. They are simultaneously
a server (receiving requests on their prefix) and a client (sending
requests to other prefixes). This is the appiware pattern.

### The TUI (client)

Holds an async `ClientStore` handle. The event loop:
1. Reads state paths for rendering
2. Polls terminal events
3. Writes input events into the namespace
4. Repeat

The TUI never mutates state directly. It writes to `input/` and reads
from everywhere else.

### Agent workers (client, sync-over-async)

Each agent holds a `ClientStore` handle that presents synchronous
`Reader`/`Writer` to the Wasm guest. The host bridge awaits the broker
while the guest blocks at the Wasm call boundary. From the agent's
perspective, the API is unchanged: `read(path!("prompt"))`,
`write(path!("history/append"), record)`.

The client handle is scoped to the thread's prefix — the worker writes
to `history/append` but the broker resolves it as
`threads/{thread_id}/history/append`. The worker doesn't know its
full path in the namespace, just like a Plan 9 process doesn't know
where its namespace is mounted in the parent.

## Namespace Tree

```
/ (BrokerStore root)
|
+-- ui/                         UiStore (in-memory state machine)
|   +-- screen                  "inbox" | "thread" | "compose"
|   +-- active_thread           thread_id or null
|   +-- mode                    "normal" | "insert"
|   +-- insert_context          "compose" | "reply" | "search" | null
|   +-- selected_row            inbox cursor position
|   +-- scroll                  viewport offset
|   +-- input                   text buffer
|   +-- cursor                  cursor position in input
|   +-- modal                   null | {type, ...}
|   +-- select_next             write: command (see Command Protocol)
|   +-- select_prev             write: command
|   +-- open_selected           write: command
|   +-- ... (more commands)
|
+-- input/                      InputStore (key-to-command translator)
|   +-- normal/{key}            write: translates key to command write
|   +-- insert/{key}            write: translates key to command write
|   +-- approval/{key}          write: translates key to command write
|
+-- inbox/                      InboxStore (SQLite-backed, unchanged)
|   +-- threads                 read: visible thread list
|   +-- threads/{id}            read: metadata, write: update
|   +-- done                    read: archived threads
|   +-- by_state/{state}        read: filtered views
|   +-- search/{query}          read: search results
|
+-- threads/                    ThreadRegistry (dynamic mounts)
|   +-- {id}/                   one sub-namespace per active thread
|       +-- system/             SystemProvider
|       +-- history/            HistoryProvider (extended with turn state)
|       |   +-- messages        read: committed + in-progress messages
|       |   +-- append          write: add a completed message
|       |   +-- count           read: message count
|       |   +-- commit          write: finalize in-progress turn
|       |   +-- snapshot/       snapshot paths (Plan A)
|       |   +-- turn/           per-turn transient state (clears on commit)
|       |       +-- streaming   write/read: accumulating text delta
|       |       +-- thinking    write/read: bool, agent is mid-turn
|       |       +-- tool        write/read: current tool call {name, status}
|       |       +-- tokens      write/read: {in, out} for this turn
|       +-- model/              ModelProvider
|       +-- tools/              ToolsProvider
|       +-- gate/               GateStore
|       +-- approval/           ApprovalStore (per-thread)
|           +-- request         write: agent posts approval request (blocks)
|           +-- response        write: TUI posts decision (unblocks request)
|           +-- pending         read: current pending request or null
|
+-- persistence/                PersistenceStore
|   +-- save/{id}               write: snapshot save for thread
|   +-- restore/{id}            write: restore thread from disk
|   +-- reconcile               write: startup reconciliation
|
+-- config/                     ConfigStore (settings files, read-only)
    +-- theme                   color scheme
    +-- provider                default provider
    +-- model                   default model
    +-- bindings                key-to-command mapping
```

## Command Protocol

Writes to stores follow a two-phase pattern:

### Phase 1: Interpret

The InputStore receives a raw key event, reads UI context (mode, screen,
current selection) to determine the correct command, and writes a
self-contained command to the target store through its client handle.

Reading context is necessary for interpretation (e.g., `j` means
`select_next` in normal mode but inserts a character in insert mode).
This phase is serialized per input source.

### Phase 2: Apply

The target store receives the command and applies it atomically. The
command is self-contained, idempotent, and parallelizable. It carries:

- **Precondition:** What the caller believes is true (e.g., the current
  selection is thread `t_abc123`). The store validates this before
  applying. If stale, the write is rejected.
- **Transaction ID:** Unique identifier for deduplication. If the same
  txn has already been applied, the write is a no-op.
- **Action:** Implicit in the path (e.g., `ui/select_next`).

Example:

```
write(path!("ui/select_next"), {
    "from": "t_abc123",    // precondition: current selection
    "txn": "a7f3e2b1"     // deduplication
})
```

The UiStore checks: is the current selection `t_abc123`? If yes, advance
to the next row. If no, reject. The txn ID prevents double-application
on retry.

This is optimistic concurrency (compare-and-swap) over StructFS paths.
Commands from the TUI and commands from agent workers apply in parallel
without coordination because they target different stores or carry
independent preconditions.

### Data Paths vs Command Paths

The same store exposes both:

- **Data paths** (reads): `ui/selected_row` returns the current value.
  Used by the render loop.
- **Command paths** (writes): `ui/select_next` transitions state.
  Used by the InputStore.

Reads are data. Writes are commands. Different paths, different purposes.
The render loop never writes. The InputStore never reads from the stores
it commands (it reads context from `ui/mode` to interpret, but the
resulting command is self-contained).

## Error Propagation

Errors propagate through the same write resolution chain that created
them. No special error bus, error log, or error paths. Just `Result`.

Every write through the broker returns `Result<Path, StoreError>`. When
a write triggers cascading writes to other stores, each store in the
chain receives the `Result` from its downstream write and decides how
to handle it: propagate, transform, or absorb.

Example — user opens a thread but disk read fails:

1. TUI writes `input/normal/Enter`
2. InputStore writes `ui/open_selected` through its client handle
3. UiStore writes `threads/t_abc/history/messages` to trigger mount
4. ThreadRegistry reads thread directory from disk — fails
5. ThreadRegistry returns `Err(StoreError)` for the mount
6. UiStore's downstream write got `Err` — returns `Err` for
   `ui/open_selected`
7. InputStore's downstream write got `Err` — returns `Err` for
   `input/normal/Enter`
8. TUI's `client.write()` returns `Err` — displays in status bar

```
if let Err(e) = client.write(&key_path, event_data).await {
    status_message = e.to_string();
}
```

Each store in the chain has full control. The UiStore could absorb the
error (set screen to thread with an error state instead of propagating)
or propagate it (let the TUI handle it). The decision is local to the
store.

For agent-side errors (disk full on ledger append, network failure),
the agent's write returns `Err` and the agent decides how to handle
it — retry, abort the turn, or surface it to the user by writing
to a path the TUI reads (e.g., a thread-scoped error path). That's
a domain decision, not an infrastructure mechanism.

## Store Responsibilities

### UiStore

Pure in-memory state machine. Owns screen, mode, selection, scroll,
input buffer, cursor, modal state. Reads return current values. Writes
are commands that transition state atomically.

The UiStore owns its invariants: clamping selection to valid range,
enforcing valid mode transitions, managing the modal stack. External
writers send commands with preconditions; the UiStore decides the
outcome.

### InputStore

Mounted at `input/`, handles the translation from raw key events to
self-contained commands. The TUI writes `input/normal/j` and the
InputStore produces a write to `ui/select_next` with the appropriate
precondition and transaction ID.

The InputStore holds the binding table (loaded from ConfigStore at
mount time or on config reload). Its reads return the current bindings
— `read("input/normal")` lists all normal-mode key bindings, useful
for help screens and discoverability. Its writes translate key events
into command writes to target stores through its client handle.

This is a store, not a function in the event loop, because:
- The broker must be agnostic to what runs over it. Something must be
  mounted at `input/` to handle writes. The broker routes, not interprets.
- The binding table is queryable state. Help screens, command palettes,
  and plugins read it through the standard StructFS interface.
- It's swappable. Vim bindings, emacs bindings, or a custom mapping
  are different InputStore implementations mounted at the same prefix.
- It's testable in isolation: mount it, write key events, observe
  the command writes it produces through its client handle.
- It keeps the TUI event loop platform-agnostic. The event loop writes
  raw input; the InputStore handles semantics. The web playground
  writes the same paths from DOM events without duplicating translation
  logic.

### InboxStore

Unchanged from current implementation. SQLite-backed thread metadata
index. Reads return thread lists and metadata. Writes create, update,
and archive threads.

### HistoryProvider (extended)

The existing HistoryProvider gains turn-scoped transient state for
real-time streaming. The path hierarchy:

- `history/messages` — read returns all messages: committed entries
  plus the in-progress turn (if any) as a partial message. The TUI
  reads one path to get the full conversation including live content.
- `history/append` — write adds a completed message.
- `history/commit` — write finalizes the in-progress turn: the
  accumulated streaming text becomes a committed message, turn state
  clears.
- `history/turn/streaming` — write appends text delta to the
  in-progress turn. Read returns accumulated text so far.
- `history/turn/thinking` — write/read: whether the agent is mid-turn.
- `history/turn/tool` — write/read: current tool call name and status.
- `history/turn/tokens` — write/read: token counts for this turn.

On `commit`, all `turn/` state clears and the accumulated content
becomes a committed message. This replaces the separate LiveStore —
the HistoryProvider is the single source of truth for both persisted
and in-flight message content.

### ThreadRegistry

Dynamic mount manager. When a thread becomes active, creates a
sub-namespace at `threads/{id}/` containing the agent's stores
(SystemProvider, HistoryProvider, ModelProvider, ToolsProvider, GateStore)
plus an ApprovalStore. When the thread is deactivated, triggers a
persistence save and unmounts.

For threads that are not active but need to be displayed (inbox list
with message counts), the ThreadRegistry can mount a lightweight
read-only projection backed by the thread directory files (ledger.jsonl
+ context.json) instead of the full agent namespace.

### ApprovalStore

Per-thread store for the permission/approval flow. The agent writes to
`approval/request` with tool name and input preview. The write blocks
in the broker until a response is written.

The TUI reads `approval/pending` to discover pending requests. The user
can navigate to other threads while one is blocked — the pending write
stays in the broker. When the user writes to `approval/response`, the
broker resolves the blocked write and the agent continues.

### PersistenceStore

Wraps the snapshot coordinator (from Plan B). Writing to
`persistence/save/{id}` reads the thread's stores through the broker,
assembles context.json + ledger entries, writes to disk, and updates the
SQLite index. Writing to `persistence/restore/{id}` reads the thread
directory and writes store snapshots back through the broker.

Writing to `persistence/reconcile` triggers startup reconciliation
(hash-based consistency check between SQLite and thread directories).

### ConfigStore

Read-only projection of settings files. Theme, default provider/model,
keybindings. Writing to a reload path triggers re-read from disk.

## Event Loop

The TUI main loop holds a `ClientStore` handle to the broker:

```
loop {
    // 1. Read state, render frame
    let screen = client.read(path!("ui/screen"));
    let active = client.read(path!("ui/active_thread"));

    match screen:
        "inbox" =>
            read inbox/threads, ui/selected_row, ui/scroll, config/theme
            draw inbox
        "thread" =>
            read threads/{id}/history/messages  (includes in-progress turn)
            read threads/{id}/history/turn/thinking
            read threads/{id}/history/turn/tool
            read threads/{id}/approval/pending
            read ui/input, ui/cursor
            draw thread

    draw modals (from ui/modal)
    draw status bar

    // 2. Poll terminal event (50ms timeout)
    if key_event:
        let mode = client.read(path!("ui/mode"));
        client.write(path!("input/{mode}/{key}"), event_data);

    // 3. Loop
```

All reads go through the broker to the owning store. The TUI is a
pure I/O shell: terminal in, terminal out, broker in between.

## Agent Worker

Each worker holds a scoped `ClientStore` handle. The scope maps the
worker's local paths to its thread prefix in the global namespace:

- Worker writes `history/append` -> broker sees `threads/{id}/history/append`
- Worker reads `prompt` -> broker sees `threads/{id}/prompt`
- Worker writes `history/turn/streaming` -> broker sees `threads/{id}/history/turn/streaming`

The agent loop:

```
loop {
    write history/turn/thinking = true
    read prompt (blocks until user sends input)
    call LLM (outside the broker, pure HTTP)
    write history/turn/streaming with deltas (during streaming)
    write history/turn/tokens with usage
    write history/commit (finalizes turn, clears turn/ state)
    write history/turn/thinking = false
```

The Wasm guest sees synchronous Reader/Writer. The host bridge awaits
the broker. The guest blocks at the Wasm boundary. From the agent's
perspective, the API is identical to today.

## What Disappears

| Current | Replaced By |
|---------|-------------|
| `App` struct with 20+ fields | UiStore + reads from broker |
| `AppEvent` enum + mpsc channel | Writes to history/turn/ through broker |
| `AppControl` + approval channel | ApprovalStore with blocking write |
| `ThreadView` struct (TUI-side mirror) | Direct reads from thread stores |
| `cached_threads` (per-frame refresh) | Read from InboxStore (internal caching) |
| `handle_normal_key()` / `handle_insert_key()` | InputStore write handlers |
| `save_history()` / `save_thread_state()` | PersistenceStore write |
| `load_thread_messages()` | ThreadRegistry mount + store reads |

## What Stays

- ratatui for terminal rendering (platform-specific)
- crossterm for terminal I/O
- Agent kernel logic (run_turn, tool execution)
- StructFS Reader/Writer traits (unchanged)
- All existing stores (SystemProvider, HistoryProvider, ModelProvider, ToolsProvider, GateStore, InboxStore)
- Snapshot coordinator and ledger (Plan A + Plan B)
- Wasm runtime for agent execution

## Sync/Async Boundary

- **Wasm guest:** synchronous Reader/Writer. Cannot change.
- **Host stores:** synchronous Reader/Writer implementations. Don't know they're inside a broker.
- **Broker:** async routing over channels. Wraps sync stores in async server tasks.
- **Host bridge:** awaits broker on behalf of blocking Wasm guest.
- **TUI event loop:** async, awaits broker reads/writes.
- **Agent HTTP transport:** async, happens outside the broker.

The BrokerStore is the only async component. Everything it wraps is
synchronous. This means existing stores work without modification.

## Persistence Model

Persistence is a construction concern, not a runtime concern.

When the ThreadRegistry mounts a thread, it constructs each store
with a **backing** appropriate to the platform. On the CLI, the
HistoryProvider is backed by `ledger.jsonl`. On the web, it might be
backed by IndexedDB. The store implements Reader/Writer and delegates
to the backing. The store doesn't know what the backing is.

```
trait StoreBacking {
    fn load(&self) -> Result<Value, Error>;
    fn save(&self, value: &Value) -> Result<(), Error>;
    fn append(&self, value: &Value) -> Result<(), Error>;
}
```

Stores can cache in memory for read performance, but the backing is
authoritative. Writes go through to the backing. The turn state
(`history/turn/*`) is purely in-memory until `commit` flushes it
through the backing.

There is no PersistenceStore. Each store handles its own durability
through its backing. The ThreadStore owns the bundled representation
(the thread directory as a unit) and coordinates construction and
teardown of its child stores from/to the bundle.

### ThreadStore and Bundles

The ThreadStore is the single owner of a thread's portable state.
It knows the bundle format and constructs child stores from it.

- `read("threads/{id}/bundle")` — returns the complete portable
  representation (context + ledger + view)
- `write("threads/{id}/bundle", data)` — restores from a portable
  representation

The ThreadStore replaces both the snapshot coordinator and the
PersistenceStore from the earlier design. Export, transfer, and
fork all operate on the bundle.

### SQLite as Derived Cache

SQLite (via InboxStore) is a queryable cache derived from thread
directories. It is never the authority. The ThreadStore writes
through to SQLite when metadata changes (title, last_seq, last_hash,
updated_at). The InboxStore reads from SQLite for list queries.

On startup, reconciliation ensures SQLite matches the thread
directories on disk. Thread directories win on any conflict.

## Branching and Fork

A branch creates a new thread that references a parent's ledger up to
a specific sequence number. The child doesn't copy the parent's
messages — it references them.

### Branch Structure

```
Thread t_def (branched from t_abc at seq 2):
  bundle:
    parent: "t_abc"
    parent_through: 2
    context.json: snapshot of t_abc's context at branch point
    ledger.jsonl: only the child's own entries
    view.json: include parent[0:2] + own[0:N]
```

The child's HistoryProvider resolves history by reading the parent's
ledger through the broker (`threads/t_abc/history/ledger`), taking
entries through the cutoff, then appending its own. The chain
resolves recursively — a branch of a branch reads through two parents.

### Copy-on-Delete

When a parent thread is deleted, any children referencing it get the
relevant ledger entries materialized into their own bundle before
deletion proceeds:

1. Find all children with `parent: t_abc`
2. For each child: read parent's ledger entries through `parent_through`,
   prepend them to the child's own ledger
3. Clear the child's parent reference (it's now self-contained)
4. Delete the parent

After materialization, the child's history resolves identically — the
entries are in its own ledger instead of referenced through the parent.
No broken links, no cascade constraints.

This is also how export works: materializing parent references produces
a self-contained bundle that can be transferred without the parent.

## Store Lifecycle

### Lazy Mount

Threads are not mounted on app startup. The ThreadRegistry handles
reads/writes to `threads/{id}/**` and mounts lazily on first access.
The inbox list comes from InboxStore (SQLite cache) — no thread needs
to be mounted for the list view.

When the TUI opens a thread, the first read to
`threads/{id}/history/messages` triggers the ThreadRegistry to:

1. Read the thread directory from disk (the authority, not SQLite)
2. Construct the ThreadStore from the bundle
3. ThreadStore constructs child stores (HistoryProvider, SystemProvider,
   etc.) with appropriate backings
4. Mount the ThreadStore at `threads/{id}/` in the broker
5. Serve the original read

### Unmount

Unmount happens on explicit lifecycle events: archive, delete, or
eviction under memory pressure. The sequence:

1. Signal the agent worker to stop (drop its prompt channel)
2. Worker finishes current operation or aborts cleanly
3. Worker's last write completes (or fails)
4. ThreadStore flushes turn state — uncommitted turns are either
   committed or discarded
5. ThreadStore unmounts from the broker
6. Broker rejects any further writes to the unmounted prefix

### Pending Writes on Unmount

Agent writes go through the broker to the ThreadStore's child stores.
Once the ThreadStore unmounts, the broker has no server for that
prefix. Any late writes get an error response. The worker handles this
gracefully — the same as any write failure.

The approval flow is the main concern: if an agent is blocked on
`approval/request` when the thread unmounts, the broker resolves the
blocked write with an error. The agent treats this as a denial.

## Cross-Platform

The namespace tree and store implementations are platform-agnostic.
Platform-specific code is limited to:

- **TUI event loop:** polls terminal / listens to DOM events, writes
  to `input/`, reads state, renders
- **Store backings:** file I/O on CLI, IndexedDB on web, REST API
  for remote

The InputStore, UiStore, InboxStore, ThreadRegistry, ThreadStore,
ApprovalStore, ConfigStore, and all agent stores — shared across
platforms.

## Testing

Every store is independently testable with synchronous Reader/Writer
calls and in-memory backings. No broker needed for unit tests.

Integration tests mount stores in a BrokerStore and verify cross-store
interactions: write to `input/normal/j`, read `ui/selected_row`, assert
it changed. Write to `threads/{id}/history/append`, read
`threads/{id}/history/messages`, assert it appears.

The command protocol (precondition + txn ID) makes tests deterministic:
commands either apply or reject, never produce unexpected intermediate
states.

Branch tests: create parent thread, branch from it, verify child
reads parent history through the broker. Delete parent, verify child's
ledger was materialized and history still resolves.

## Scope

### In Scope (this spec)

- BrokerStore adapted from appiware for ox's StructFS types
- UiStore, InputStore, ApprovalStore, ThreadStore, ThreadRegistry
- HistoryProvider extended with turn state and branch resolution
- StoreBacking trait for platform-agnostic persistence
- ConfigStore for settings
- TUI event loop rewrite on top of broker
- Agent worker integration via scoped client handles

### Deferred

- Web playground (ox-web) integration (store backings for browser)
- Fork tool implementation (branching is designed, not built)
- Plugin/scripting system using command paths
- Command palette / REPL mode
- View projection engine (masks, replacements, summarization)
- Network/remote broker transport
- Memory eviction policy for mounted ThreadStores
