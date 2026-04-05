# Portable Agent State

**Date:** 2026-04-05
**Status:** Draft

## Overview

Replace the current split persistence model (SQLite metadata + JSONL messages +
in-memory ThreadView) with a unified, portable state system. A thread's complete
state is a directory that can be saved to disk, moved across machines, and
resumed identically. SQLite becomes a derived queryable index, not a source of
truth.

## Core Model: Ledger + View

A conversation has two distinct representations:

### Ledger

The immutable, append-only record of what actually happened. Every message,
tool call, and result is recorded with a content-addressed hash and sequence
number. Never rewritten.

Each entry:
```jsonl
{"seq":0,"hash":"a1b2c3","parent":null,"msg":{"role":"user","content":"..."}}
{"seq":1,"hash":"d4e5f6","parent":"a1b2c3","msg":{"role":"assistant","content":[...]}}
{"seq":2,"hash":"g7h8i9","parent":"d4e5f6","msg":{"role":"user","content":[{"type":"tool_result",...}]}}
```

- **seq** — monotonic sequence number. Used for range references in views.
- **hash** — content hash of the `msg` field (SHA-256 of the JSON-serialized
  message, truncated to 16 hex characters). Uniquely identifies this entry.
- **parent** — hash of the previous entry. Creates a hash chain for integrity
  verification. Null for the first entry.
- **msg** — the wire-format message (Anthropic Messages API format).

The hash chain provides:
- **O(1) integrity check** — compare last hash against SQLite cache
- **Tamper detection** — edited entries break the chain
- **Stable fork references** — forks reference parent entries by hash, not seq

### View

The projection the agent currently sees. Defines which ledger entries are
visible to the kernel when assembling the prompt. Can include ranges, masks,
and replacements (summaries).

```json
{
  "parent": null,
  "include": [{"start": 0, "end": 5}, {"start": 14}],
  "masks": [11, 12, 13],
  "replacements": {
    "6-10": {"role": "assistant", "content": "Previously I refactored..."}
  }
}
```

- **parent** — thread_id of the parent thread (for forks). Null for root
  threads.
- **parent_through** — sequence number through which to include parent ledger
  entries (for forks).
- **include** — ranges of sequence numbers to include. Default: all.
- **masks** — individual sequence numbers to hide from the agent.
- **replacements** — seq ranges mapped to summary messages that replace them.

A default view (no masks, no replacements, include all) produces the same
behavior as today: the agent sees the full conversation.

The view projection engine — how views are evaluated, how summarization
produces replacements, how forks compose parent views — is deferred to a
follow-on spec. This spec defines the data model only.

## Snapshot Lens Pattern

Each Store that holds essential state exposes a conventional StructFS path for
snapshot and restore:

- Read `snapshot` → returns a `Record` containing the store's complete
  restorable state
- Write `snapshot` with that Record → restores the store to that state

This is a **protocol over StructFS**, not a new trait. Stores opt in by
handling the `snapshot` path in their Reader/Writer implementations. The
Namespace routes it like any other path.

### Participating Stores

| Store | Snapshot contains |
|-------|-------------------|
| SystemProvider | System prompt string |
| ModelProvider | Model name + max_tokens |
| GateStore | Accounts, keys, bootstrap, provider configs |
| HistoryProvider | Seq number + hash of last entry (for change detection, not full history — history lives in the ledger) |

### Non-Participating Stores

| Store | Reason |
|-------|--------|
| ToolsProvider | Tools are host-provided, not conversation state |
| HostStore (ox-runtime) | Runtime middleware, no persistent state |
| SnapshotStore | The coordinator, not a participant |

## SnapshotStore

A new Store mounted in the Namespace that coordinates snapshots across all
participating stores. It reads `{mount}/snapshot` from each peer store and
assembles the thread directory. It writes `{mount}/snapshot` to each peer to
restore.

The SnapshotStore knows:
- Which mounts to snapshot (configured at creation)
- Where the thread directory lives on disk
- How to read/write the directory format

It exposes:
- Read `save` → triggers a full save to disk, returns the thread directory path
- Write `restore` with a path → loads state from a thread directory
- Read `status` → returns last save time, last hash, whether dirty

Future extensions (deferred):
- `snapshot/seq` — current sequence number for change detection
- `snapshot/delta/{from_seq}` — incremental changes since a point

## Thread Directory Format

Each thread's portable state is a directory:

```
~/.ox/threads/{thread_id}/
  context.json       — snapshot of non-history stores
  ledger.jsonl       — immutable append-only message record
  view.json          — projection manifest (what the agent sees)
  deltas.jsonl       — context change audit log
```

### context.json

Latest snapshot of all participating stores, keyed by mount name:

```json
{
  "version": 1,
  "thread_id": "t_abc123",
  "title": "Refactor auth middleware",
  "labels": ["backend"],
  "created_at": 1712345678,
  "updated_at": 1712345900,
  "system": "You are an expert software engineer...",
  "model": {"model": "claude-sonnet-4-20250514", "max_tokens": 4096},
  "gate": {
    "bootstrap": "anthropic",
    "accounts": {"anthropic": {"provider": "anthropic"}}
  }
}
```

Thread metadata (title, labels, timestamps) lives here as well — this is the
authoritative source. SQLite mirrors it.

API keys are excluded from context.json. The host provides them at runtime.
The gate snapshot includes account structure and provider config but omits
key values. This ensures thread directories are safe to transfer without
leaking credentials.

### ledger.jsonl

Append-only message log with content-addressed hashes. See Ledger section
above. One entry per line.

### view.json

The current view manifest. See View section above. Default for new threads:

```json
{
  "parent": null,
  "include": [{"start": 0}],
  "masks": [],
  "replacements": {}
}
```

### deltas.jsonl

Append-only log of context changes, timestamped:

```jsonl
{"ts":1712345678,"mount":"model","path":"id","before":"gpt-4o","after":"claude-sonnet-4-20250514"}
{"ts":1712345700,"mount":"system","path":"","before":"Old prompt","after":"New prompt"}
```

Not required for restoration (context.json is sufficient). Used for auditing,
debugging, and understanding when and why configuration changed.

## SQLite as Derived Index

SQLite's role changes from "system of record" to "queryable cache derived from
thread directories."

### Schema (revised)

```sql
CREATE TABLE threads (
    id            TEXT PRIMARY KEY,
    title         TEXT NOT NULL,
    parent_id     TEXT REFERENCES threads(id),
    inbox_state   TEXT NOT NULL DEFAULT 'inbox',
    thread_state  TEXT NOT NULL DEFAULT 'running',
    block_reason  TEXT,
    created_at    INTEGER NOT NULL,
    updated_at    INTEGER NOT NULL,
    token_count   INTEGER NOT NULL DEFAULT 0,
    last_seq      INTEGER NOT NULL DEFAULT -1,
    last_hash     TEXT
);
```

New fields:
- **last_seq** — sequence number of the last ledger entry
- **last_hash** — content hash of the last ledger entry

### Write-Through

Every state change writes to both the thread directory (authoritative) and
SQLite (index):
- Metadata changes (title, labels) → update context.json AND SQLite
- New messages → append to ledger.jsonl AND update last_seq/last_hash in SQLite
- Thread state transitions → derive from ledger AND cache in SQLite
- Token counts → accumulate AND cache in SQLite

### Thread State Derivation

Thread state is derived from the ledger rather than stored as independent
mutable state:

| Ledger ends with | Derived state |
|-----------------|---------------|
| User message, no assistant response | running |
| Complete assistant response, no tool calls | waiting_for_input |
| Assistant response with tool calls, tool results follow | running (mid-turn) |
| Tool calls without tool results | interrupted |
| Error entry | errored |

SQLite caches this for fast inbox queries, but the ledger is authoritative.

### Startup Reconciliation

On launch, a lightweight consistency check:

1. For each SQLite entry: does the thread directory exist?
   - No → remove from index
2. For each thread directory without an index entry:
   - Read context.json → insert into index
3. For each indexed thread: read last line of ledger.jsonl, compare hash with
   `last_hash` in SQLite
   - Match → index is valid, skip
   - Mismatch → re-derive state from directory

This is NOT a full rebuild. Most threads pass the hash check in O(1). Only
crashed/corrupted threads require re-derivation.

## Restoration Flow

To resume a thread from its directory:

1. Read `context.json` → write `{mount}/snapshot` for system, model, gate
2. Read `ledger.jsonl` + `view.json` → apply view projection → write matching
   entries to `history/append`
3. Thread is fully reconstituted with identical state

To transfer a thread to another machine:

1. Copy the thread directory (exclude secrets)
2. On the target: run restoration flow
3. Host provides its own API keys and tools

## Portability Properties

| Operation | What happens |
|-----------|-------------|
| Save | Write context.json + append to ledger.jsonl |
| Restore | Read context.json + replay ledger through view |
| Transfer | Copy directory, re-index on target |
| Fork | New directory, view.json references parent by hash |
| Time-travel | Restore from a previous context.json + ledger through seq N |
| Serverless | Load directory from object storage, run one turn, save back |

## Scope

### In Scope (this spec)

- Ledger/view data model with content-addressed hashes
- Snapshot lens pattern for StructFS stores
- Thread directory format (context.json, ledger.jsonl, view.json, deltas.jsonl)
- SQLite as derived index with hash-based reconciliation
- SnapshotStore as coordinator

### Deferred

- View projection engine (evaluation, summarization, masking logic)
- Fork mechanics (parent reference resolution, view composition)
- Delta-based incremental snapshots
- Network/object-storage sync protocol
- Secrets management for cross-machine transfer
