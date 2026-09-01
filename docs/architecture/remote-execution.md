# Remote Execution Architecture

**Status:** Target architecture for the `ox remote` CLI project

**Date:** 2026-08-31

## Purpose

Remote execution packages the executor already used by the local CLI and hosts
it on an exe.dev node. It does not introduce another agent runtime.

One long-lived `ox-worker serve` process owns one `ExecutionCore`. The core is a
behavior-preserving extraction of the current broker, `AgentPool`,
`ThreadRegistry`, Wasmtime host, tools, approvals, ledger writer, snapshot, and
resume paths. A node may run multiple independent ox conversations.

The MVP defaults to `fresh_node`, so ordinary creation initially provisions a
new node. That is a placement policy, not a storage or worker cardinality.
`require_node` must support creating multiple conversations on one selected
node from the first compatible schema and protocol version.

## Prerequisites verification manifest

- [x] **R1. `AgentPool` already owns concurrent per-thread workers and one
  shared compiled module.**
  Verified with
  `nl -ba crates/ox-executor/src/agents.rs | sed -n '95,365p'`.
  `AgentPool` owns `HashMap<String, ThreadHandle>` at
  `crates/ox-executor/src/agents.rs:131-145`, loads the agent module once at
  line 190, clones it at line 338, and starts one mailbox and OS worker thread
  per ox thread at lines 328–366. If this changes before extraction, remote
  work is blocked for ~2–4 days to re-establish local/remote executor parity.

- [x] **R2. A turn already gets a fresh Wasmtime Store and instance.**
  Verified with
  `nl -ba crates/ox-runtime/src/engine.rs | sed -n '35,105p;262,290p'`.
  `AgentRuntime` owns the reusable engine and compiled-module infrastructure at
  `crates/ox-runtime/src/engine.rs:35-70`; `AgentModule::run` constructs a new
  Store and instance at lines 262–283. If this ceases to be true, the remote
  isolation and resource-control design is blocked for ~3–5 days.

- [x] **R3. `ThreadRegistry` already owns lazy thread namespaces, restore, and
  approval routing.**
  Verified with
  `nl -ba crates/ox-executor/src/thread_registry.rs | sed -n '199,430p;598,775p'`.
  Replay precedes durability installation at
  `crates/ox-executor/src/thread_registry.rs:199-211` and `:392-407`; lazy mount is
  at `:620-670`; approval paths are routed at `:682-775`. If these ownership
  seams move, extraction is blocked for ~2–3 days; a second registry is not an
  acceptable workaround.

- [x] **R4. Existing log publication is commit-before-visibility and the
  ledger writer is already per thread.**
  Verified with
  `nl -ba crates/ox-kernel/src/log.rs | sed -n '209,285p'` and
  `nl -ba crates/ox-inbox/src/ledger_writer.rs | sed -n '1,38p'`.
  `SharedLog::append` commits before publishing at
  `crates/ox-kernel/src/log.rs:266-283`; `LedgerWriter` owns one ordered,
  hash-chained ledger at `crates/ox-inbox/src/ledger_writer.rs:1-24`. If this
  contract changes, remote cursor work is blocked for ~3–5 days to preserve
  durability and ordering.

- [x] **R5. Existing async broker requests do not block one another.**
  Verified with
  `nl -ba crates/ox-broker/src/server.rs | sed -n '37,61p;90,139p'`.
  The async loop spawns reads and writes independently at
  `crates/ox-broker/src/server.rs:110-138`; the sync loop is sequential at
  lines 37–61. Remote public Stores are therefore required to use
  `mount_async`. If async mounting cannot be preserved, shared-node transport
  is blocked for ~2–4 days to remove head-of-line blocking.

- [x] **R6. `InboxStore` owns thread identity and the ox database.**
  Verified with
  `nl -ba crates/ox-inbox/src/lib.rs | sed -n '42,82p'`,
  `nl -ba crates/ox-inbox/src/schema.rs | sed -n '1,129p'`, and
  `nl -ba crates/ox-inbox/src/writer.rs | sed -n '22,96p'`.
  `InboxStore::open` initializes `ox.db` at
  `crates/ox-inbox/src/lib.rs:47-60`; writing `threads` generates the existing
  `t_...` identity and directory at `crates/ox-inbox/src/writer.rs:62-95`.
  If another owner is introduced, remote persistence is blocked for ~3–5 days
  to restore one-writer ownership.

- [x] **R7. Tool subprocess isolation already exists, but the CLI Clash
  adapter can currently fall back to unsandboxed execution.**
  Verified with
  `nl -ba crates/ox-tools/src/sandbox.rs | sed -n '54,125p'` and
  `nl -ba crates/ox-executor/src/clash_sandbox.rs | sed -n '142,162p'`.
  `sandboxed_exec` starts `ox-tool-exec` under a policy at
  `crates/ox-tools/src/sandbox.rs:68-124`; Clash returns the original command
  after policy compilation failure at
  `crates/ox-executor/src/clash_sandbox.rs:154-160`. Remote deployment is blocked
  until that profile fails closed; estimated hardening scope is ~2–4 days.

## Reuse budget

New remote code may wrap, expose, configure, or harden the local executor. It
may not recreate it.

| Capability | Existing owner | Remote addition |
|---|---|---|
| thread creation and index | `InboxStore` | durable create-id ingress |
| namespace and restore | `ThreadRegistry` | public-path adapter |
| concurrent execution | `AgentPool` | headless control handle and limits |
| Wasm execution | `AgentRuntime` / `AgentModule` | resource and cancel controls |
| tools | `ToolStore` + `ox-tool-exec` | fail-closed remote policy |
| approvals | `ApprovalStore` / thread approval paths | public record adapter |
| durable history | `SharedLog` + `LedgerWriter` | bounded ledger cursor |
| broker routing | `BrokerStore` + `mount_async` | headless mount composition |
| config and secrets | existing distinct mounts | remote policy configuration |

Remote ingress tables are the one justified durability addition: transport
retries require stable create, message, approval, and cancel IDs before the
existing action is invoked. They live in the worker's existing inbox database
and do not model conversations or duplicate the ledger.

## Topology

```text
ox remote command
  -> RemoteManagerStore
       -> ExeControlStore -> typed exe.dev SSH commands
       -> RemoteStore -> StructFS frames over RuSSH
            -> ox-worker structfs-stdio
                 -> node-local Unix socket
                      -> ox-worker serve
                           -> ExecutionCore
                                -> BrokerStore
                                -> InboxStore
                                -> ThreadRegistry
                                -> AgentPool
                                     -> worker t_a
                                     -> worker t_b
                                     -> worker t_c
```

`ox-worker structfs-stdio` is a stateless carrier bridge. EOF detaches the
client and does not stop the service, a turn, or a conversation.

## ExecutionCore boundary

`ExecutionCore` contains the execution-only portions of today's CLI:

- common broker mounts for inbox, config, secrets, and threads;
- `AgentPool`, its prompt loop, and compiled-module sharing;
- `ThreadRegistry`, thread namespaces, restore, resume, and commit drain;
- runtime, policy, tools, approvals, token accounting, and config snapshots;
- a bounded headless command channel for quick create, ensure-worker, enqueue,
  cancel, and shutdown control operations.

Local CLI and `ox-worker` both construct this exact core with different UI and
policy adapters. Actual turns remain on the existing per-thread workers; the
control channel must never hold a core-wide mutex while waiting for a turn,
approval, completion provider, or cursor read.

The current pool-wide workspace configuration becomes a per-thread execution
configuration. The local CLI supplies its current workspace for every local
thread. The worker supplies a thread-owned workspace directory. This is a
configuration generalization, not a new workspace executor.

## Public Store mapping

```text
read  health
read  capabilities
read  capacity
write conversations <CreateConversation> -> conversations/<thread_id>
read  conversations
read  conversations/<thread_id>
write conversations/<thread_id>/messages <RemoteMessage>
read  conversations/<thread_id>/ledger/from/<seq>
read  conversations/<thread_id>/result
read  conversations/<thread_id>/approvals/pending
write conversations/<thread_id>/approvals/<approval_id> <ApprovalResponse>
write conversations/<thread_id>/control/cancel <CancelRequest>
```

| Public operation | Existing operation |
|---|---|
| create | `InboxStore` thread create, then `AgentPool::ensure_worker` |
| message | durable ingress ID, then existing prompt mailbox |
| summary/result | inbox thread row and log/history projection |
| ledger cursor | existing `ledger.jsonl` envelopes |
| approval | existing `threads/<id>/approval/*` routes |
| cancel | cancellation added at the existing worker/runtime seam |

The returned worker conversation ID is the existing `t_...` thread ID. The
adapter never exposes arbitrary broker paths, tools, config, secrets,
filesystem paths, or commands.

## Isolation and concurrency

Wasm Store/instance boundaries and sandboxed tool subprocesses provide
execution isolation. Existing per-thread worker threads provide concurrency;
they are not a security boundary. A per-conversation process, container,
runtime, or broker is out of scope.

Node-wide limits add bounded active-turn permits, prompt queues, cursor reads,
frame sizes, artifact metadata, Wasmtime resources, and provider concurrency.
Turn admission cannot gate status, approval, cancellation, or ledger reads. A
conversation waiting on approval or model I/O must not hold a global control
lock or prevent unrelated worker threads and async Store requests from making
progress.

If stronger tenant-level host isolation becomes necessary, placement assigns
that tenant a separate node. It does not create a second execution architecture.

## Identity, placement, and writer ownership

The local coordinator owns node, node-attempt, remote-reference, and operation
IDs. The worker health record echoes node and node-attempt identity. The worker
owns existing thread IDs.

Placement policies are:

- `fresh_node`: provision a compatible node, then create the thread;
- `prefer_existing`: choose a verified compatible node with capacity, otherwise
  provision;
- `require_node`: create on the named verified node or fail.

The local inbox owns orchestration and placement rows. The worker uses its
normal `ox.db`, thread directories, and ledgers. There is no worker placement
catalog, per-conversation manifest, or parallel event log.

| State | Authoritative writer |
|---|---|
| local remote and node operation | local `InboxStore` |
| exe.dev VM | provider through `ExeControlStore` |
| worker conversation | worker `InboxStore` |
| ingress intent | worker `InboxStore` semantic-ID row |
| conversation history | existing per-thread `LedgerWriter` |
| approval state | existing `ApprovalStore` |
| config snapshot | existing snapshot path |

## Transport boundary

All coordinator/worker operations are StructFS reads and writes. RuSSH and the
stdio/Unix streams are carriers. Wire v1 uses bounded length-prefixed canonical
CBOR frames, preserves all StructFS `Record`/`Value` variants, allows multiple
in-flight request IDs, and returns typed errors.

Public and remote proxy Stores use async mounts. One parked ledger-cursor read
therefore cannot block a message, approval, cancellation, or another
conversation. Semantic ingress IDs, not an in-memory transport cache, provide
retry correctness across disconnects and service restarts.

## Future Isotope runtime

A future full Isotope runtime plugs in behind `ExecutionCore`. It may replace
the current pre-Isotope executor internals after local parity is established,
without changing the worker public Store, StructFS transport, remote identity,
placement, or orchestration contracts. This project neither implements that
runtime nor builds a remote-only approximation of it.
