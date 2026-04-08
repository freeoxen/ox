# ThreadRegistry Design

**Date:** 2026-04-07
**Status:** Draft
**Depends on:** C6 (Events Through Broker — complete)

## Overview

Replace the ad-hoc `thread_mount.rs` functions with a ThreadRegistry —
an AsyncStore mounted at `threads/` that owns per-thread store lifecycle.
It routes `{id}/{store}/{path}` internally, lazy-mounts thread stores
from disk on first access, and presents a single opaque mount to the
broker.

This eliminates the bug where thread history doesn't load for
previously-saved threads (stores weren't mounted until a worker spawned)
and completes the architectural intent from the original spec.

## Architecture

The ThreadRegistry is a **Namespace with lazy construction.** It holds
a `HashMap<String, ThreadNamespace>` where each ThreadNamespace contains
the per-thread stores (SystemProvider, HistoryProvider, ModelProvider,
ToolsProvider, GateStore, ApprovalStore). On first access to a thread
ID, it constructs the stores from the thread directory on disk and
restores from snapshot.

From the broker's perspective, `threads/` is one store. The broker
doesn't know about individual thread stores. The ThreadRegistry routes
internally — same pattern as `ox_context::Namespace`.

## Routing

The ThreadRegistry is mounted at `threads/` in the broker. It receives
paths like `t_abc/history/messages`. It splits on the first component
to get the thread ID (`t_abc`) and the sub-path (`history/messages`).

```
Request: threads/t_abc/history/messages
  → Broker routes to ThreadRegistry (mounted at "threads/")
  → ThreadRegistry receives path "t_abc/history/messages"
  → Split: thread_id = "t_abc", sub_path = "history/messages"
  → Look up ThreadNamespace for "t_abc" (lazy-mount if absent)
  → Route sub_path to HistoryProvider
  → Return result
```

## Lazy Mount

When the ThreadRegistry receives a request for a thread ID it hasn't
seen before:

1. Check if thread directory exists at `{inbox_root}/threads/{id}/`
2. If it exists: construct stores, restore from snapshot
3. If it doesn't exist: return NoRoute (thread doesn't exist)

Construction from disk:
- Read `context.json` → restore SystemProvider, ModelProvider, GateStore
  from snapshot state
- Read `ledger.jsonl` → replay messages through HistoryProvider
- Create empty ToolsProvider (tool schemas come from the worker later)
- Create fresh ApprovalStore

After construction, the thread is cached in the HashMap. Subsequent
requests route directly — no disk I/O.

## ThreadNamespace

A per-thread store collection. Holds sync stores (HistoryProvider,
SystemProvider, ModelProvider, ToolsProvider, GateStore) and one async
store (ApprovalStore).

```rust
struct ThreadNamespace {
    system: SystemProvider,
    history: HistoryProvider,
    model: ModelProvider,
    tools: ToolsProvider,
    gate: GateStore,
    approval: ApprovalStore,
}
```

Routing within a ThreadNamespace splits on the first component of the
sub-path:

```
"system" → system store (remainder path)
"history" → history store (remainder path)
"model" → model store (remainder path)
"tools" → tools store (remainder path)
"gate" → gate store (remainder path)
"approval" → approval store (remainder path)
```

For sync stores (everything except approval), the read/write calls
the store directly and wraps the result in `Box::pin(ready(...))`.

For the ApprovalStore (async), the read/write returns the store's
async future directly.

## AsyncStore Implementation

The ThreadRegistry implements `AsyncReader + AsyncWriter`:

```rust
impl AsyncReader for ThreadRegistry {
    fn read(&mut self, from: &Path) -> BoxFuture<...> {
        let (thread_id, sub_path) = split_first_component(from);
        let ns = self.ensure_mounted(thread_id);
        ns.read(&sub_path)  // returns BoxFuture
    }
}

impl AsyncWriter for ThreadRegistry {
    fn write(&mut self, to: &Path, data: Record) -> BoxFuture<...> {
        let (thread_id, sub_path) = split_first_component(to);
        let ns = self.ensure_mounted(thread_id);
        ns.write(&sub_path, data)  // returns BoxFuture
    }
}
```

`ensure_mounted` is synchronous (it blocks on disk I/O for lazy mount).
Since the async server loop calls `store.read()` / `store.write()`
sequentially, this is fine — the lazy mount happens once per thread
and is fast (read JSON files, construct stores).

## What Gets Replaced

### thread_mount.rs → deleted
- `mount_thread()` → ThreadRegistry lazy mount
- `unmount_thread()` → ThreadRegistry.unmount(id)
- `restore_thread_state()` → ThreadRegistry lazy mount (does restore)
- `ThreadConfig` → ThreadRegistry holds the config needed to construct
- `ThreadMountHandles` → gone (ThreadRegistry owns lifecycle)

### agent_worker mounting → simplified
Currently the worker calls `rt_handle.block_on(mount_thread(...))` on
startup and `rt_handle.block_on(unmount_thread(...))` on exit.

After: the worker does NOT mount stores. They're already mounted
(by the ThreadRegistry on first TUI access, or by create_thread).
The worker just uses its scoped SyncClientAdapter as before.

BUT: the worker needs to update ToolsProvider with tool schemas
(standard_tools + completion_tools). Currently this is done during
mount_thread via ThreadConfig. With ThreadRegistry lazy mount from
disk, the initial ToolsProvider is empty. The worker writes tool
schemas through its adapter:

```
adapter.write(&path!("tools/schemas"), tool_schemas_value)
```

This requires ToolsProvider to accept writes to "schemas" (currently
read-only). Add a write handler for "schemas" to ToolsProvider.

### broker_setup.rs → simplified
Remove the global ApprovalStore mount. The ThreadRegistry handles
per-thread ApprovalStore internally.

Keep: UiStore, InputStore, InboxStore mounts.
Add: ThreadRegistry mount at `threads/`.

### New thread creation
`AgentPool.create_thread()` creates the thread in InboxStore and
spawns a worker. The worker's first write to `threads/{id}/...`
triggers lazy mount in the ThreadRegistry. But there's no thread
directory yet (new thread, no snapshot).

Solution: the ThreadRegistry handles the "no directory" case by
constructing fresh stores with default config. The worker writes
the system prompt, model, API key, and tool schemas through its
adapter, populating the stores.

`create_thread` writes initial config to the thread directory
BEFORE the worker starts. Then the ThreadRegistry lazy-mounts from
the directory normally. The thread directory is always the authority.

## Unmount

ThreadRegistry.unmount(id):
1. Flush turn state (commit any uncommitted turns)
2. Save snapshot to disk
3. Remove from the HashMap

Called on:
- Archive/delete (explicit lifecycle event)
- App exit (via ThreadRegistry destructor or explicit shutdown)
- Memory pressure eviction (deferred — not implemented yet)

## Config

The ThreadRegistry needs:
- `inbox_root: PathBuf` — where thread directories live
- Default system prompt, model, max_tokens — for new threads without
  snapshots (or read from a config file)

It does NOT need:
- API keys (GateStore gets keys from the worker via writes)
- Tool schemas (ToolsProvider gets schemas from the worker)
- BrokerStore reference (it routes internally, not via the broker)

## What Stays the Same

- Worker uses scoped SyncClientAdapter — unchanged
- ViewState reads from broker — unchanged (path resolution unchanged,
  `threads/{id}/history/messages` still works)
- fetch_view_state — unchanged
- CliEffects — unchanged (writes to turn/*, approval/*)
- UiStore, InputStore, InboxStore — unchanged

## Testing

### Unit tests (ThreadRegistry in isolation)
- Construct ThreadRegistry with temp dir
- Create a thread directory with context.json + ledger.jsonl
- Read history/messages → verify lazy mount + correct messages
- Read system → verify snapshot restored
- Second read → no disk I/O (cached)
- Read nonexistent thread → NoRoute

### Integration tests (through broker)
- Mount ThreadRegistry at `threads/` in broker
- Write thread data to disk (thread directory)
- Read through broker client → verify messages appear
- Verify scoped client works for worker path

### Approval flow
- Write approval/request through scoped client → blocks
- Read approval/pending → returns request
- Write approval/response → unblocks request write

## Files

| File | Action | Responsibility |
|------|--------|---------------|
| `crates/ox-cli/src/thread_registry.rs` | Create | ThreadRegistry + ThreadNamespace, lazy mount, internal routing |
| `crates/ox-cli/src/broker_setup.rs` | Modify | Mount ThreadRegistry at `threads/`, remove ApprovalStore mount |
| `crates/ox-cli/src/agents.rs` | Modify | Worker skips mounting, writes tool schemas via adapter |
| `crates/ox-cli/src/thread_mount.rs` | Delete | Replaced by ThreadRegistry |
| `crates/ox-cli/src/main.rs` | Modify | Remove thread_mount module |
| `crates/ox-context/src/lib.rs` | Modify | ToolsProvider accepts writes to "schemas" |
