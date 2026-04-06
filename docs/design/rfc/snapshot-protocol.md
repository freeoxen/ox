# RFC: StructFS Snapshot Lens

**Status:** Draft
**Date:** 2026-04-05

## Problem

Stores in a StructFS namespace hold state that needs to persist across
sessions, transfer across machines, and survive crashes. Currently each store
is opaque — there is no standard way to extract a store's essential state or
restore it from a previous extraction.

## The Snapshot Lens

A **lens** in StructFS is a virtual prefix over a shared namespace that
provides a different view of the same underlying data. The `meta/` lens
provides introspective information about paths. The `snapshot` lens provides
a serializable extraction of essential state.

A store that supports snapshots exposes a `snapshot` path. Reading it returns
the store's complete restorable state as a struct. Writing it restores the
store to that state.

This is a **convention**, not a new interface. Stores opt in by handling the
`snapshot` path in their existing read/write implementations. The namespace
routes it like any other path.

## Structure

A snapshot is a map with two fields:

```json
{
  "hash": "e3b0c44298fc1c14",
  "state": <store-specific value>
}
```

- **hash** — a content hash of the `state` field. Enables cheap change
  detection without comparing the full state. Computed by serializing `state`
  to JSON (StructFS maps have sorted keys, so output is deterministic),
  SHA-256 hashing the bytes, and truncating to 16 hex characters.

- **state** — the store's essential restorable data. Shape is store-defined.

### Consistency

Reading `snapshot` returns the full struct. Reading `snapshot/hash` returns
just the hash string. Reading `snapshot/state` returns just the state value.
This follows StructFS's consistency property: if reading `/foo` returns a
struct containing field `bar`, then reading `/foo/bar` returns that field's
value.

### Writing

Writing to `snapshot` with a map containing a `state` field restores the
store. The `hash` field in the written value is ignored — the store
recomputes it from the state.

Writing to `snapshot/state` also restores the store (equivalent to writing
the full snapshot with just the state).

## Store Snapshots

### SystemProvider

The state is the system prompt string.

**Read `snapshot`:**
```json
{
  "hash": "a7c3e9f012b84d56",
  "state": "You are an expert software engineer working in a coding CLI. You have tools for reading files, writing files, editing files, and running shell commands. Always read a file before modifying it. Be concise."
}
```

**Read `snapshot/state`:**
```json
"You are an expert software engineer working in a coding CLI. You have tools for reading files, writing files, editing files, and running shell commands. Always read a file before modifying it. Be concise."
```

**Read `snapshot/hash`:**
```json
"a7c3e9f012b84d56"
```

**Write `snapshot`:**
```json
{
  "state": "You are a helpful assistant."
}
```

After this write, the store's prompt is "You are a helpful assistant."
Reading `snapshot` returns the new state with a recomputed hash.

### ModelProvider

The state is a map with the model identifier and token limit.

**Read `snapshot`:**
```json
{
  "hash": "b2d4f6a8c0e1g3h5",
  "state": {
    "max_tokens": 4096,
    "model": "claude-sonnet-4-20250514"
  }
}
```

**Write `snapshot`:**
```json
{
  "state": {
    "max_tokens": 8192,
    "model": "gpt-4o"
  }
}
```

### HistoryProvider

The state is a summary — NOT the full message history. Messages are persisted
separately in the ledger file. The state contains enough to verify consistency.

**Read `snapshot`:**
```json
{
  "hash": "c3e5g7i9k1m3o5q7",
  "state": {
    "count": 12,
    "last_hash": "d4e5f6a7b8c9d0e1"
  }
}
```

`count` is the number of messages. `last_hash` is the content hash of the
last message in the history (matching the hash chain in the ledger).

**Read `snapshot` (empty history):**
```json
{
  "hash": "0000000000000000",
  "state": {
    "count": 0,
    "last_hash": null
  }
}
```

**Write `snapshot` is not supported.** History is restored by replaying
ledger entries through the `append` path, not by writing a snapshot. Writing
to `snapshot` returns an error.

### GateStore

The state is a map of provider configurations and account structures. API
keys are excluded — the host provides them at runtime.

**Read `snapshot`:**
```json
{
  "hash": "d4f6h8j0l2n4p6r8",
  "state": {
    "bootstrap": "anthropic",
    "providers": {
      "anthropic": {
        "dialect": "anthropic",
        "endpoint": "https://api.anthropic.com/v1/messages",
        "version": "2023-06-01"
      },
      "openai": {
        "dialect": "openai",
        "endpoint": "https://api.openai.com/v1/chat/completions",
        "version": ""
      }
    },
    "accounts": {
      "anthropic": {
        "model": "claude-sonnet-4-20250514",
        "provider": "anthropic"
      },
      "openai": {
        "model": "gpt-4o",
        "provider": "openai"
      }
    }
  }
}
```

Account entries have no `key` field. Keys are omitted so that snapshots are
safe to transfer without leaking credentials.

**Write `snapshot`:**

Writing replaces the store's providers, accounts, and bootstrap. Existing
API keys set at runtime are cleared — the host must re-provide them.

### ToolsProvider

ToolsProvider does **not** participate in the snapshot lens. Tools are
host-provided, not conversation state.

A read from `snapshot` returns null (the path does not exist in this store).

## Discovery

A coordinator discovers which mounts participate by reading `snapshot` from
each mount:

```
read("system/snapshot")    →  returns data    → participates
read("model/snapshot")     →  returns data    → participates
read("history/snapshot")   →  returns data    → participates
read("gate/snapshot")      →  returns data    → participates
read("tools/snapshot")     →  returns null    → does not participate
```

No registry, no configuration. The coordinator reads and observes.

## Assembled Context

When a coordinator reads snapshots from all participating mounts and
assembles them, the result is a map keyed by mount name. Only the `state`
field from each snapshot is included (the hash is a derived property,
recomputed on restore):

```json
{
  "system": "You are an expert software engineer...",
  "model": {
    "max_tokens": 4096,
    "model": "claude-sonnet-4-20250514"
  },
  "gate": {
    "bootstrap": "anthropic",
    "providers": { "...": "..." },
    "accounts": { "...": "..." }
  },
  "history": {
    "count": 12,
    "last_hash": "d4e5f6a7b8c9d0e1"
  }
}
```

This is what gets written to `context.json` in a thread directory. To
restore: iterate the map, write each value to `{mount}/snapshot`.

## Non-Requirements

- Stores are NOT required to support the snapshot lens.
- The protocol does NOT define incremental snapshots. Full snapshot only.
- The protocol does NOT handle cross-store consistency. The coordinator may
  verify consistency but the lens itself is per-store.
