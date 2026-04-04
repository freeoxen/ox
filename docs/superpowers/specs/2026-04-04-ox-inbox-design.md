# ox-inbox: Agent Thread Inbox for ox-cli

**Date:** 2026-04-04 **Status:** Draft

## Overview

Reimagine ox-cli as an inbox-style agent manager — like Google Inbox — where
users create, monitor, triage, and archive concurrent agent threads. The inbox
surfaces rich structured state (tasks, lifecycle phase, labels) to help users
manage attention across many parallel agents.

## Core Concepts

### Inbox as Attention Management

The inbox answers "do I need to look at this, and why?" — not "what did the
agent say?" Thread previews show state, tasks, and urgency before you ever open
the thread. The user imposes structure via labels and tags; within that
structure, urgency and recency provide default sort.

### Two Orthogonal State Axes

- **Inbox state** (`inbox` | `done`): The user's attention decision. "Done"
  archives the thread out of the inbox view. It persists on disk and is
  searchable/reopenable. Marking a running thread "done" doesn't stop it.
- **Thread state** (`running` | `waiting_for_input` | `blocked_on_approval` |
  `completed` | `errored`): The agent's lifecycle phase. Drives display priority
  — blocked/errored threads surface above running/completed ones.

### Thread Tree

Human-created threads are roots. Agent-spawned sub-threads are children linked
by `parent_id`. Each node in the tree is an agent with its own JSONL
conversation log. The tree is the real shape of work.

## Architecture

Three layers, strictly separated:

``` ox-cli TUI reads/writes via StructFS paths ↕ ox-inbox — InboxStore (impl
Store) StructFS Reader/Writer interface, query routing, thread lifecycle ↙
↘ SQLite              JSONL Files metadata            conversation content ```

### InboxStore (`ox-inbox` crate)

Implements StructFS `Store` (Reader + Writer). This is the **only public
interface**. ox-cli never touches SQLite or JSONL directly.

- `read(&mut self, path: &Path) -> Result<Option<Record>, StoreError>`
- `write(&mut self, path: &Path, record: Record) -> Result<Option<Path>,
  StoreError>`

The store routes reads/writes to SQLite (metadata) or JSONL files (conversation
content) based on the path.

### SQLite (Metadata)

Holds everything that drives filtering, sorting, and display. Fast queries at
thousands of threads.

### JSONL (Conversation Content)

One file per agent instance, append-only. Each line is a wire-format message
(same JSON shape ox already uses). Stored at
`~/.ox/threads/{thread_id}/{agent_id}.jsonl`. Inspectable, streamable, cheap to
append.

## StructFS Path Schema

Paths are resources. Records carry data. No verbs in paths, no query parameters.

### Reads

| Path | Returns |
|------|---------|
| `inbox/threads` | All inbox threads (metadata only) |
| `inbox/threads/{id}` | Single thread metadata |
| `inbox/threads/{id}/messages` | Conversation content from JSONL |
| `inbox/threads/{id}/children` | Sub-agent threads |
| `inbox/threads/{id}/tasks` | Extracted task state |
| `inbox/done` | Archived threads |
| `inbox/labels` | All known labels |
| `inbox/labels/{name}` | Threads with that label |
| `inbox/by-state/{state}` | Threads by internal state |
| `inbox/search/{query}` | Search thread titles and labels |

### Writes

| Path | Record | Effect |
|------|--------|--------|
| `inbox/threads` | `{title, labels?, parent_id?}` | Create thread, returns ID via `Option<Path>` |
| `inbox/threads/{id}` | `{inbox_state?, title?, state?, block_reason?}` | Update metadata fields |
| `inbox/threads/{id}/messages` | `{role, content}` | Append to JSONL |
| `inbox/threads/{id}/labels` | `["label1", "label2"]` | Set labels |

## Data Model

### SQLite Schema

```sql
CREATE TABLE threads (
    id            TEXT PRIMARY KEY,
    title         TEXT NOT NULL,
    parent_id     TEXT REFERENCES threads(id),
    inbox_state   TEXT NOT NULL DEFAULT 'inbox',  -- 'inbox' | 'done'
    thread_state  TEXT NOT NULL DEFAULT 'running', -- 'running' | 'waiting_for_input' | 'blocked_on_approval' | 'completed' | 'errored'
    block_reason  TEXT,
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL,
    token_count   INTEGER DEFAULT 0
);

CREATE TABLE labels (
    thread_id     TEXT NOT NULL REFERENCES threads(id),
    label         TEXT NOT NULL,
    PRIMARY KEY (thread_id, label)
);

CREATE TABLE tasks (
    id            TEXT PRIMARY KEY,
    thread_id     TEXT NOT NULL REFERENCES threads(id),
    title         TEXT NOT NULL,
    status        TEXT NOT NULL DEFAULT 'pending',  -- 'pending' | 'in_progress' | 'completed'
    created_at    TEXT NOT NULL,
    updated_at    TEXT NOT NULL
);

CREATE INDEX idx_threads_inbox_state ON threads(inbox_state);
CREATE INDEX idx_threads_thread_state ON threads(thread_state);
CREATE INDEX idx_threads_updated_at ON threads(updated_at);
CREATE INDEX idx_labels_label ON labels(label);
CREATE INDEX idx_tasks_thread_id ON tasks(thread_id);
```

### JSONL Files

Location: `~/.ox/threads/{thread_id}/{agent_id}.jsonl`

Each line is one wire-format JSON message:

```jsonl
{"role":"user","content":"Refactor the auth middleware to use the new session token format"}
{"role":"assistant","content":[{"type":"text","text":"I'll start by reading..."},{"type":"tool_use","id":"t1","name":"read_file","input":{"path":"src/middleware/auth.rs"}}]}
{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"...file contents..."}]}
```

The root agent's `agent_id` equals the `thread_id`. Sub-agents get their own ID, linked via `parent_id` in the threads table.

## Thread Runtime — Agents as Wasm Modules

### Execution Model

Each agent thread is a **Wasmtime component instance**. The Wasm boundary is pure StructFS — the module imports only:

- `read(path: &Path) -> Result<Option<Record>, StoreError>`
- `write(path: &Path, record: Record) -> Result<Option<Path>, StoreError>`

The module exports an entry point (e.g., `run()`). Internally it drives its own kernel loop: reading `prompt`, writing `history/append`, etc. The module never does I/O directly.

### Host as StructFS Middleware

The host provides the `read`/`write` implementations that the Wasm module imports. From the module's perspective, it's just calling into a StructFS store. From the host's perspective, it's a dispatch table — most paths delegate to an in-memory namespace, but some trigger real-world effects:

- Module reads/writes `gate/complete` → host performs HTTP fetch to LLM API
- Module writes `tools/execute/{name}` → host runs the tool with policy checks, writes result back
- Module writes `history/append` → host appends to JSONL, updates SQLite metadata (token count, timestamps)
- Module writes `inbox/threads` with `parent_id` → host spawns a sub-agent (new Wasm instance)

All other reads/writes pass through to the module's StructFS namespace.

### Why Wasm Over OS Threads

- **Scale:** Wasm instances are kilobytes, not megabytes. Hundreds of idle agents cost nearly nothing.
- **Sandboxing:** Each instance accesses only what the host provides. No ambient authority.
- **Fast instantiation:** Wasmtime pre-compiles modules; instantiation is microseconds.
- **Host-controlled scheduling:** The host decides when to pump each instance. Idle agents (waiting, completed, blocked) consume zero host threads.

### Host Thread Architecture

A small thread pool drives active instances. The TUI main thread receives events from active instances via channels, multiplexed across all open threads. Approval gates pause an instance — the host simply doesn't call back in until the user responds.

### Resumption

When ox-cli starts, threads in `running` or `blocked_on_approval` state from a previous session are marked `errored` (the process died). Threads in `waiting_for_input` or `completed` can be reopened — JSONL is replayed into a fresh namespace, and a new Wasm instance is spawned.

## TUI Design

### Navigation Model

- **Inbox view:** Home screen, always available as a tab. Filterable list of all inbox threads.
- **Thread tabs:** Opening a thread from the inbox adds a tab. Closing a tab doesn't stop the thread. Tabs are the user's working set.
- **Full-screen per tab:** Each tab (inbox or thread) takes the full screen. No split panes.

### Inbox View Layout

```
┌─────────────────────────────────────────────────┐
│ [■ Inbox]                                       │  ← tab bar
├─────────────────────────────────────────────────┤
│ / search   [all] [backend] [infra] [blocked]    │  ← filter bar (labels + states)
├─────────────────────────────────────────────────┤
│ ● BLOCKED  Refactor auth middleware      3m ago │
│   [backend]  Waiting for approval: shell "cargo │
│              test"                              │
│   ☑ 3/5 tasks · 12.4k tokens · 2 sub-agents    │
│─────────────────────────────────────────────────│
│ ● RUNNING  Add pagination to /api/users  1m ago │
│   [backend] [api]  Running: edit_file           │
│   ☑ 1/3 tasks · 8.2k tokens                    │
│─────────────────────────────────────────────────│
│ ● WAITING  Write migration for user_s.. 12m ago │
│   [infra]  Agent finished, awaiting review      │
│   ☑ 5/5 tasks · 24.1k tokens · 1 sub-agent     │
│─────────────────────────────────────────────────│
│ ● COMPLETED  Fix typo in README          1h ago │
│   ☑ 1/1 tasks · 1.2k tokens                    │
├─────────────────────────────────────────────────┤
│ 4 threads · 2 need attention                    │
│ n new  Enter open  d done  / search  ? help     │
└─────────────────────────────────────────────────┘
```

Each thread row shows:
- **State indicator** — colored dot + label (RUNNING green, BLOCKED orange, WAITING purple, COMPLETED dim, ERRORED red)
- **Title** — bold for active, dimmed for completed
- **Recency** — time since last update
- **Labels** — small tags
- **Current activity** — what the agent is doing or why it's blocked
- **Progress** — task completion ratio, token count, sub-agent count

Default sort: user-defined labels as primary grouping, then urgency (blocked > errored > waiting > running > completed), then recency within each group.

### Thread View Layout

```
┌─────────────────────────────────────────────────┐
│ [■ Inbox] [Refactor auth middleware] [Add pag…] │  ← tab bar
├─────────────────────────────────────────────────┤
│ > Refactor the auth middleware to use the new   │
│   session token format from the compliance spec │
│                                                 │
│ I'll start by reading the current auth          │
│ middleware and the compliance spec...           │
│                                                 │
│ [read_file] src/middleware/auth.rs              │
│ [read_file] docs/compliance-spec.md             │
│                                                 │
│ I see the issue. The current middleware stores  │
│ raw session tokens in cookies...                │
│                                                 │
│ [edit_file] src/middleware/auth.rs              │
│                                                 │
│ ▓ Approval needed: shell "cargo test"           │
├─────────────────────────────────────────────────┤
│ │ Type a message...                             │
├─────────────────────────────────────────────────┤
│ ● BLOCKED · 3/5 tasks · 12.4k tokens           │
│ y approve  n deny  Ctrl+W close tab  Ctrl+T    │
│ inbox                                           │
└─────────────────────────────────────────────────┘
```

Same conversation view as current ox-cli: streaming text, tool calls, approval dialogs. Wrapped in the tab navigation.

### Key Bindings

**Inbox view:**
- `n` — compose new thread
- `Enter` — open selected thread as tab
- `d` — mark selected thread done (archive)
- `/` — search
- `j`/`k` or `↑`/`↓` — navigate thread list
- Label filter keys shown in filter bar

**Thread view:**
- `Enter` — submit message
- `y`/`n`/`s`/`a` — approval shortcuts (same as current ox-cli)
- `c` — customize rule (same as current ox-cli)
- `Ctrl+W` — close tab (thread keeps running)
- `Ctrl+T` — switch to inbox tab
- `Ctrl+←`/`Ctrl+→` — switch between tabs

## V1 Scope

### In Scope

- `ox-inbox` crate: InboxStore (StructFS Store impl), SQLite metadata, JSONL conversation storage
- Thread CRUD: create, list, filter by label, filter by state, search titles, mark done
- Thread tree: parent/child relationships for sub-agents
- TUI: inbox view, tab management, compose new thread, filter bar, search
- Wasm execution: agents as Wasmtime component instances, StructFS-only boundary
- Session migration: existing `~/.ox/sessions/` format migrated to new thread store

### Deferred

- External triggers (file watchers, webhooks, cron)
- JSONL full-text search indexing into SQLite
- Bulk operations (select multiple, mark all done)
- Quick actions from inbox view (snooze, pin)
- Compound filters (label + state intersection via path composition)
- ox-web inbox integration
