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
|       +-- history/            HistoryProvider
|       +-- model/              ModelProvider
|       +-- tools/              ToolsProvider
|       +-- gate/               GateStore
|       +-- snapshot/           snapshot paths (Plan A)
|       +-- live/               LiveStore (event buffer)
|       |   +-- streaming       current text delta
|       |   +-- thinking        bool
|       |   +-- tool            current tool call
|       |   +-- tokens          {in, out}
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

Key-to-command translator. Receives raw key events on paths like
`input/normal/j`. Reads context from `ui/` paths (mode, screen) through
its client handle. Writes the resulting command to the target store
through the same client handle.

Stateless — all state lives in the UiStore and the config bindings.
The InputStore is a pure function from (key, context) to command.

Remapping: change the bindings in ConfigStore, or mount a different
InputStore implementation (vim mode, emacs mode, custom).

### InboxStore

Unchanged from current implementation. SQLite-backed thread metadata
index. Reads return thread lists and metadata. Writes create, update,
and archive threads.

### ThreadRegistry

Dynamic mount manager. When a thread becomes active, creates a
sub-namespace at `threads/{id}/` containing the agent's stores
(SystemProvider, HistoryProvider, ModelProvider, ToolsProvider, GateStore)
plus a LiveStore and ApprovalStore. When the thread is deactivated,
triggers a persistence save and unmounts.

For threads that are not active but need to be displayed (inbox list
with message counts), the ThreadRegistry can mount a lightweight
read-only projection backed by the thread directory files (ledger.jsonl
+ context.json) instead of the full agent namespace.

### LiveStore

Per-thread in-memory buffer for real-time agent output. The agent worker
writes text deltas, tool call status, thinking state, and token counts
through its scoped client handle. The TUI reads for display.

Replaces the AppEvent channel and ThreadView struct for content delivery.
The agent writes `live/streaming` with each text delta. The TUI reads
`live/streaming` to get the current accumulated text.

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
            read threads/{id}/history/messages
            read threads/{id}/live/streaming, live/thinking, live/tool
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
- Worker writes `live/streaming` -> broker sees `threads/{id}/live/streaming`

The agent loop:

```
loop {
    read prompt (blocks until user sends input)
    call LLM (outside the broker, pure HTTP)
    write history/append with response
    write live/streaming with deltas (during streaming)
    write live/thinking = false when done
    trigger persistence/save/{id}
```

The Wasm guest sees synchronous Reader/Writer. The host bridge awaits
the broker. The guest blocks at the Wasm boundary. From the agent's
perspective, the API is identical to today.

## What Disappears

| Current | Replaced By |
|---------|-------------|
| `App` struct with 20+ fields | UiStore + reads from broker |
| `AppEvent` enum + mpsc channel | Writes to LiveStore through broker |
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

## Cross-Platform

The namespace tree and store implementations are platform-agnostic.
The TUI event loop is the only platform-specific code:

- **CLI (ratatui):** polls crossterm, writes to `input/`, reads state, draws terminal
- **Web (Svelte):** listens to DOM events, writes to `input/`, reads state, updates Svelte stores

The InputStore, UiStore, InboxStore, ThreadRegistry, LiveStore,
ApprovalStore, PersistenceStore, ConfigStore — all shared.

## Recursive Composition (Rio Model)

A thread's sub-namespace contains the same types of stores as the
root namespace: state, input handling, persistence. When fork arrives,
a child thread is another mount under `threads/` with its own
sub-namespace. The parent thread's history can reference the child by
path. The TUI navigates into child threads the same way it navigates
from the inbox into a thread.

The ThreadRegistry is the mount manager at each level. The root has
one. A parent thread with children has one too. The structure is
recursive — namespaces containing namespaces, all the way down.

## Testing

Every store is independently testable with synchronous Reader/Writer
calls. No broker needed for unit tests.

Integration tests mount stores in a BrokerStore and verify cross-store
interactions: write to `input/normal/j`, read `ui/selected_row`, assert
it changed. Write to `threads/{id}/history/append`, read
`threads/{id}/live/streaming`, assert the delta appeared.

The command protocol (precondition + txn ID) makes tests deterministic:
commands either apply or reject, never produce unexpected intermediate
states.

## Scope

### In Scope (this spec)

- BrokerStore adapted from appiware for ox's StructFS types
- UiStore, InputStore, LiveStore, ApprovalStore, ThreadRegistry
- PersistenceStore wrapping snapshot coordinator
- ConfigStore for settings
- TUI event loop rewrite on top of broker
- Agent worker integration via scoped client handles

### Deferred

- Web playground (ox-web) integration
- Fork tool and recursive thread namespaces
- Plugin/scripting system using command paths
- Command palette / REPL mode
- View projection engine (masks, replacements)
- Network/remote broker transport
