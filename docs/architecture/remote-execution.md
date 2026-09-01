# Remote Execution Architecture

**Status:** Target architecture for the `ox remote` CLI project

**Date:** 2026-09-01

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
  `rg -n "struct ThreadHandle|pub struct AgentPool|load_module_from_bytes|fn spawn_worker|thread::spawn" crates/ox-executor/src/agents.rs`.
  `AgentPool` owns `HashMap<String, ThreadHandle>` at
  `crates/ox-executor/src/agents.rs:284-305`, loads the agent module once at
  line 371, and starts one mailbox and OS worker thread per ox thread at
  lines 557–607. If this changes, remote
  work is blocked for ~2–4 days to re-establish local/remote executor parity.

- [x] **R2. A turn already gets a fresh Wasmtime Store and instance.**
  Verified with
  `nl -ba crates/ox-runtime/src/engine.rs | sed -n '35,190p;370,420p'`.
  `AgentRuntime` owns the reusable engine and compiled-module infrastructure at
  `crates/ox-runtime/src/engine.rs:42-158`; `AgentModule::run` constructs a new
  Store and instance at lines 378–390. If this ceases to be true, the remote
  isolation and resource-control design is blocked for ~3–5 days.

- [x] **R3. `ThreadRegistry` already owns lazy thread namespaces, restore, and
  approval routing.**
  Verified with
  `nl -ba crates/ox-executor/src/thread_registry.rs | sed -n '199,430p;598,775p'`.
  Replay precedes durability installation at
  `crates/ox-executor/src/thread_registry.rs:211-399` and `:400-407`; lazy mount is
  at `:633-683`; approval paths are routed at `:695-820`. If these ownership
  seams move, extraction is blocked for ~2–3 days; a second registry is not an
  acceptable workaround.

- [x] **R4. Existing log publication is commit-before-visibility and the
  ledger writer is already per thread.**
  Verified with
  `nl -ba crates/ox-kernel/src/log.rs | sed -n '221,298p'` and
  `nl -ba crates/ox-inbox/src/ledger_writer.rs | sed -n '1,38p'`.
  `SharedLog::append` commits before publishing at
  `crates/ox-kernel/src/log.rs:279-296`; `LedgerWriter` owns one ordered,
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

- [x] **R8. Durable worker ingress now reuses `InboxStore`, the existing
  executor, and the existing per-thread log rather than creating another
  conversation store.**
  Verified with
  `rg -n "accept_worker_|pending_worker_intents|apply_worker_create|mark_worker_intent_applied" crates/ox-inbox/src/worker_ingress.rs` and
  `rg -n "dispatch_worker_ingress|classify_ingress_prompt|finalize_pending_cancels" crates/ox-executor/src/{agents,ingress}.rs`.
  Acceptance and global ordering are at
  `crates/ox-inbox/src/worker_ingress.rs:236-288` and `:421-463`; stable create
  materialization is at `:495-534`; startup dispatch is at
  `crates/ox-executor/src/agents.rs:1091-1096`; prompt evidence classification
  is at `crates/ox-executor/src/ingress.rs:56-112`; cancellation finalization
  is at `agents.rs:1886-2007`. If these seams stop using the existing
  inbox/log/worker owners, remote execution is blocked for ~2–4 days to remove
  the duplicate state machine.

- [x] **R7. Tool subprocess isolation now has distinct compatibility and
  fail-closed enforcement modes.**
  Verified with
  `nl -ba crates/ox-tools/src/sandbox.rs | sed -n '55,335p'`,
  `nl -ba crates/ox-executor/src/clash_sandbox.rs | sed -n '15,320p'`, and
  `nl -ba crates/ox-tools/src/bin/ox-tool-exec.rs | sed -n '95,135p'`.
  `sandboxed_exec_with_options` bounds and supervises the subprocess at
  `crates/ox-tools/src/sandbox.rs:168-320`; required policy uses the hidden
  executor launcher and fails instead of falling through at
  `crates/ox-executor/src/clash_sandbox.rs:267-305`; the launcher calls Clash's
  actual platform backend at `crates/ox-tools/src/bin/ox-tool-exec.rs:106-133`.
  Linux deployment remains blocked until the worker-image job executes the
  Landlock/seccomp escape tests; estimated CI/image work is <1 day.

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

The implemented ingress uses four tables with a transactionally allocated
cross-kind `accepted_seq` and indexes for ordered recovery
(`crates/ox-inbox/src/schema.rs:36-77`). Canonical domain-separated request
hashes and typed envelopes live at
`crates/ox-inbox/src/worker_ingress.rs:18-43` and `:140-161`. Identical retries
return the stable receipt/result; the same semantic ID with a different hash is
a conflict. The execution control loop drains accepted/unapplied rows at
startup and through a bounded explicit command (`agents.rs:1091-1125`,
`:1235-1245`).

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

`ApprovalStore` and `LogEntry::ApprovalRequested` currently have no intrinsic
approval ID. The Task 6 public adapter must therefore derive the exposed
`approval_id` from the currently unresolved durable approval evidence and
validate that identity again immediately before accepting/dispatching a
response. Task 3's `worker_decisions.approval_id` is only the durable response
idempotency key; by itself it must not authorize a stale response against a
later pending approval on the same thread.

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

The executor exposes two explicit hardening profiles. Local defaults preserve
the existing unlimited Wasmtime configuration and Clash compatibility fallback.
`ExecutorConfig::remote` selects memory, fuel, epoch timeout, bounded tool I/O,
tool timeout, node-wide turn permits, and an empty-by-default trusted-native-tool
allowlist. `PolicyProfile::RemoteEnforced` uses a Clash launcher which enters
Landlock/seccomp on Linux or Seatbelt on macOS before `ox-tool-exec` handles a
request. The remote policy has no root-wide read default and denies shell
network access. Required launchers also clear the worker environment before
re-exec: only an audited `PATH`, locale, and conversation-scoped `HOME` and
`TMPDIR` survive. Provider credentials and arbitrary worker environment values
therefore cannot be read by a remote shell through environment inheritance.

One cancellation token is owned by each existing sequential thread worker. It
is reset only after the prior turn and all of its subprocesses have joined.
Public cancel control sets it; the active tool supervisor kills and reaps the
tool process group, while the active Wasmtime Store observes it through its own
epoch callback. The engine epoch is continuously ticked, but each Store
independently extends its deadline, so cancelling conversation A cannot trap B.
The active-turn permit is acquired immediately around `AgentModule::run` and is
released by RAII on success, error, trap, cancellation, or unwind.

Linux escape coverage lives at
`crates/ox-tools/tests/sandbox_limits.rs:198-284`: it proves an allowed workspace
read while rejecting an out-of-workspace read, write, and loopback connection.
Because the development host is macOS, those `cfg(target_os = "linux")` tests
must execute in the final worker image before it reports ready.

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
in-flight request IDs, and returns typed errors. `RemoteStore` has bounded send
and in-flight admission, per-request deadlines, cancellation-safe correlation
cleanup, out-of-order response dispatch, stable disconnect failure, and no
automatic write retry. The server rejects only a deadline already expired at
admission; once a Store operation starts, loss of its response channel does not
cancel it.

Every connection receives the same explicitly supplied `ExportRoot`, not the
worker broker. The adapter prepends that root to request paths and verifies that
write-result paths strip the exact root, including rejection of sibling and
prefix-collision paths. The long-lived service owns the Unix accept loop. Its
stdio command is only a byte bridge: request-side EOF half-closes Unix and
drains pending replies, while dropping the bridge leaves the service and
admitted Store operations alive.

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
