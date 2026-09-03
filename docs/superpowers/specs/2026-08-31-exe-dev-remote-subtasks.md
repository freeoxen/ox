# exe.dev remote ox startup and subtask messaging

**Date:** 2026-08-31

**Status:** Draft specification, not an implementation plan

**Depends on:** the existing ox Wasm agent host, broker substrate, inbox
durability, and the Isotope-shaped Assembly machinery currently implemented by
`ox-gateway`

## Summary

Ox may delegate a bounded piece of work to a new exe.dev VM. The local ox
runtime creates a durable subtask handle, provisions a VM from a pinned
`ox-worker` image, submits the task to a headless remote ox service, and then
observes or controls it through a remote StructFS store. SSH is the preferred
CLI carrier and HTTPS is an alternative carrier for the same StructFS `read`
and `write` frames; neither exposes a task-, conversation-, or
approval-specific REST API. The remote task
continues if the originating UI or phone is suspended. Reconnection is a
normal state transition, not a failure.

The local interface follows the handle convention already used by
`CompletionBrokerStore`, `WireStore`, and `TelemetryStore`:

```text
write subtasks/spawn <RemoteTaskSpec>
    -> subtasks/outstanding/<task-id>

read  subtasks/outstanding/<task-id>                    # non-blocking status
read  subtasks/outstanding/<task-id>/events/from/<seq>  # blocking when live
read  subtasks/outstanding/<task-id>/result             # blocking until terminal
write subtasks/outstanding/<task-id>/messages           # follow-up input
write subtasks/outstanding/<task-id>/approvals/<id>      # approval response
write subtasks/outstanding/<task-id>/cancel {}           # request cancellation
write subtasks/outstanding/<task-id> null                # local GC after terminal
```

The remote worker presents its public Assembly as one StructFS store. A generic
`RemoteStore` client mounts that store into the local runtime. The subtask
coordinator, CLI, agent, and Flutter app only perform path reads and writes;
they cannot call worker HTTP routes directly.

## Goals

- Start one disposable exe.dev VM for each remote ox task.
- Allow multiple tasks to be provisioning or running concurrently.
- Let a task continue while the local process is absent.
- Resume event delivery without gaps or duplication after reconnect.
- Support follow-up messages, approvals, cancellation, result retrieval, and
  explicit cleanup.
- Keep the remote child thread's ledger single-writer and independently
  recoverable.
- Reuse the gateway's enforced Assembly wiring and outstanding-handle patterns.
- Intermediate all cross-runtime communication through StructFS.
- Keep exe.dev and transport details inside host stores, outside the agent and
  coordinator Blocks.

## Non-goals

- Multiple StructFS wire encodings or transport negotiation in the MVP. The
  first transport is one versioned encoding over an SSH exec channel.
- Migrating a running Block between the phone and the VM.
- Sharing one mutable ledger between local and remote writers.
- General-purpose VM administration.
- Keeping an unbounded number of VMs alive. “Arbitrary” means the model is
  N-task rather than singleton; quotas and policy always impose a cap.
- Uploading the user's entire phone filesystem or secret store.
- Collaborative, simultaneous editing of one workspace by local and remote
  agents.

## Prerequisites verification manifest

- [x] **G1. ox-gateway already enforces Assembly-declared namespaces.**
  Verified with
  `nl -ba crates/ox-gateway/src/assembly.rs | sed -n '102,188p'` and
  `nl -ba crates/ox-gateway/src/broker_block.rs | sed -n '21,101p'`.
  `Manifest::wiring_for` builds the namespace at
  `crates/ox-gateway/src/assembly.rs:102`; `BlockBacking::resolve` refuses an
  unwired path at `crates/ox-gateway/src/broker_block.rs:42`.

- [x] **G2. The gateway has the required async-handle behavior.**
  Verified with
  `nl -ba crates/ox-gateway/src/wire_store.rs | sed -n '1,180p'`.
  `WireStore` documents its handle paths at
  `crates/ox-gateway/src/wire_store.rs:8`, blocks for output at line 96, and
  cancels a live run during GC at line 160.

- [x] **G3. Inbox threads can express a parent/child relationship, but task
  storage cannot yet describe a remote execution.**
  Verified with `nl -ba crates/ox-inbox/src/schema.rs | sed -n '6,42p'` and
  `nl -ba crates/ox-types/src/inbox.rs | sed -n '40,46p'`.
  `threads.parent_id` is present at `crates/ox-inbox/src/schema.rs:9`; the
  current `tasks` table has only identity, title, status, and timestamps at
  lines 27–34.

- [x] **G4. Normal tool execution currently writes a tool call and immediately
  reads the returned handle.**
  Verified with
  `nl -ba crates/ox-kernel/src/run.rs | sed -n '596,643p'`.
  The immediate read is at `crates/ox-kernel/src/run.rs:620`. Therefore the
  initial delegation tool must return a non-blocking status/reference; it must
  not make that first handle read wait for the remote task to finish.

- [x] **G5. The CLI has no headless agent service boundary today.**
  Verified with
  `rg -n "headless|Subcommand|AgentPool|run_one_turn" crates/ox-cli/src` and
  `nl -ba crates/ox-cli/src/main.rs | sed -n '60,95p'`.
  The only subcommand is `init`; `AgentPool` and `run_one_turn` remain inside
  the binary at `crates/ox-cli/src/agents.rs:101` and line 697. Remote reuse
  therefore requires a small extraction, not a second agent loop.

- [x] **G6. Thread snapshots exclude secret key material.**
  Verified with `nl -ba crates/ox-gate/src/lib.rs | sed -n '299,347p'` and
  `nl -ba crates/ox-inbox/src/snapshot.rs | sed -n '1,94p'`.
  Gate snapshots contain providers and account pointers, explicitly excluding
  keys at `crates/ox-gate/src/lib.rs:299`; ledger persistence is independently
  owned by `LedgerWriter` per `crates/ox-inbox/src/snapshot.rs:1`.

- [x] **G7. The pinned StructFS source does not define a network wire protocol
  for a remote Store.**
  Verified with
  `nl -ba /Users/alex/.cargo/git/checkouts/structfs-33a5c53178d143e8/80a613e/isotope/spec/00-overview.md | sed -n '102,112p'`
  and
  `nl -ba /Users/alex/.cargo/git/checkouts/structfs-33a5c53178d143e8/80a613e/packages/http/README.md | sed -n '1,58p'`.
  Isotope explicitly leaves wire format to the implementation; `structfs-http`
  is an outbound HTTP-request store, not a Store server/client transport.

- [x] **G8. The current ox async Store seam is repository-local while its data
  types are StructFS types.**
  Verified with
  `nl -ba crates/ox-broker/src/async_store.rs | sed -n '1,24p'` and
  `nl -ba crates/ox-gateway/src/wire_store.rs | sed -n '19,30p'`.
  `ox_broker::async_store::{AsyncReader, AsyncWriter}` is explicitly described
  as broker-internal at `crates/ox-broker/src/async_store.rs:1` and uses
  `structfs_core_store::{Path, Record, Error}` at lines 5 and 12–19. The first
  remote adapter targets this seam; adopting upstream StructFS async traits is
  a separate behavior-preserving migration.

## Components

### `ox-remote` library

A platform-neutral library containing:

- protocol types and validation;
- `RemoteSubtaskStore`;
- the task coordinator state machine;
- an `ExeControl` trait;
- a `RemoteStoreClient` implementing the current runtime-facing
  `ox_broker::async_store` read/write traits over StructFS data types;
- retry, reconciliation, and event-cursor rules;
- no Flutter, RuSSH, axum, reqwest, or CLI UI dependencies in its core types.

Production adapters may use RuSSH or reqwest. Tests use deterministic fake
control and remote-store clients.

### `RemoteSubtaskStore`

The store is mounted at `subtasks/`. A write to `spawn` performs only durable
acceptance:

1. Validate the request.
2. Allocate `task_id` and `attempt_id`.
3. Create a child inbox thread with `parent_id` when a parent is supplied.
4. Commit task state as `accepted`.
5. Schedule provisioning.
6. Return `outstanding/<task-id>`.

The returned handle is immediately readable and returns a status record. This
allows the existing kernel's write-then-read tool behavior to finish the
current agent turn while work proceeds remotely.

Reads of `events/from/<seq>` block only while the caller explicitly asks for
events. On Android, reconciliation issues bounded reads through `RemoteStore`
and appends returned records into the same local store.

### `ox-structfs-transport`

A reusable transport crate exposes any explicitly selected StructFS store over
an authenticated network edge and provides a client implementing the current
runtime-facing `ox_broker::async_store::{AsyncReader, AsyncWriter}` traits. It
knows about StructFS Paths, Records, returned Paths, and store-level errors. It
knows nothing about ox conversations or tasks.

The server is wired only to the remote Assembly's public store. It cannot
address the worker's substrate root or bypass Assembly namespace enforcement.

### `ExeControlStore`

exe.dev control operations are host capabilities, never direct calls from a
Wasm Block. The coordinator sees an `exe` store such as:

```text
write exe/vms <VmSpec>                      -> exe/vms/<name>
read  exe/vms/<name>                        -> VmStatus
write exe/vms/<name>/tokens <TokenSpec>     -> exe/vms/<name>/tokens/<id>
write exe/vms/<name>/delete {}              -> exe/vms/<name>/deletions/<id>
```

The host backing for that store uses exe.dev's documented HTTPS interface:

- `POST https://exe.dev/exec` with the CLI command as the body;
- a control bearer token restricted to the commands actually required;
- `ls`, `new`, `rm`, and `ssh-key generate-api-key` only;
- no interactive commands, stdin, or PTY assumptions;
- each request must finish within the control API's 30-second limit.

References:
[HTTPS API](https://exe.dev/docs/https-api),
[`new`](https://exe.dev/docs/cli-new),
[`ls`](https://exe.dev/docs/cli-ls),
[`rm`](https://exe.dev/docs/cli-rm), and
[VM-scoped HTTPS tokens](https://exe.dev/docs/https-tokens-for-vms).

### `ox-worker` image

`ox-worker` is a headless Linux binary plus the same ox/Isotope Wasm artifacts
used locally. The OCI image:

- is referenced by immutable digest in production;
- installs `ox-worker` so the VM's existing SSH daemon can execute
  `ox-worker structfs-stdio` without a PTY;
- may additionally expose one private HTTP port, conventionally 8000, when the
  HTTPS carrier is enabled;
- starts only the durable worker supervisor automatically; the SSH transport
  process is created per exec channel and attaches to that supervisor;
- stores task state, ledgers, and workspaces on the VM's persistent disk;
- serves the public ox Assembly store through `ox-structfs-transport`;
- implements a `health` path that does not claim readiness until storage,
  policy, and the agent artifact are usable;
- receives credentials through explicitly attached exe.dev integrations or a
  separate remote secret capability, never through the task envelope.

The supervisor exports the same framed public Store on a VM-local Unix socket.
`structfs-stdio` is a byte-stream bridge between the SSH channel and that
socket, not a second worker API. Consequently disconnecting SSH cannot stop the
conversation, and cross-process communication inside the VM is still StructFS.

exe.dev supports custom OCI images and chooses an HTTPS proxy target from an
image's exposed ports. Proxies are private by default. See
[custom images](https://exe.dev/docs/customization) and
[HTTP proxies](https://exe.dev/docs/proxy).

`ox-worker` must reuse a headless agent host extracted from `AgentPool` and
`run_one_turn`; it must not depend on TUI state or fork that orchestration.

## Local task model

The existing `tasks` row remains the inbox-friendly summary. Remote execution
details live in a separate table so local tasks are not forced into the remote
shape:

```sql
CREATE TABLE remote_tasks (
    task_id             TEXT PRIMARY KEY REFERENCES tasks(id),
    parent_thread_id    TEXT,
    child_thread_id     TEXT,
    attempt_id          TEXT NOT NULL,
    state               TEXT NOT NULL,
    remote_ref          TEXT NOT NULL UNIQUE,
    conversation_path   TEXT,
    vm_name             TEXT,
    transport_kind      TEXT,
    ssh_host            TEXT,
    ssh_user            TEXT,
    ssh_host_key        TEXT,
    https_url           TEXT,
    credential_handle   TEXT,
    image_digest        TEXT NOT NULL,
    last_remote_seq     INTEGER NOT NULL DEFAULT -1,
    lease_expires_at    INTEGER,
    cleanup_policy      TEXT NOT NULL,
    cleanup_state       TEXT NOT NULL DEFAULT 'not_started',
    error_code          TEXT,
    error_message       TEXT,
    created_at          INTEGER NOT NULL,
    updated_at          INTEGER NOT NULL
);
```

`credential_handle` is an opaque reference, never key or token material.
Android resolves it through a Keystore-backed secret Store; desktop uses the
existing secret substrate. `conversation_path` is persisted before the local
state advances to `running`, so `attach` can recover after a crash.

Remote events may be cached in a `remote_events(task_id, seq, body)` table or
as a per-task JSONL file. `(task_id, seq)` is unique. Replaying an event is a
no-op.

## State machine

```text
accepted
   -> provisioning
   -> booting
   -> submitting
   -> running <-> waiting_for_input
              <-> blocked_on_approval
   -> cancelling
   -> completed | failed | cancelled | lost

cleanup: not_started -> scheduled -> cleaning
                               -> cleaned | retained | cleanup_failed
```

`completed`, `failed`, `cancelled`, and `lost` are task outcomes. Cleanup is a
separate dimension: a failed task can still have a retained VM, and a completed
task can still be waiting for deletion.

Every transition is persisted before the coordinator performs the next
external side effect. State updates include `attempt_id` so a stale response or
retry cannot mutate a newer attempt.

`lost` means the VM cannot be found and no terminal worker record was
retrieved. A network timeout is not `lost`; it is a retryable reconciliation
condition.

## Provisioning and reconciliation

### Identity and idempotency

- Allocate the local task ID before contacting exe.dev.
- Derive a legal deterministic VM name from the task ID, for example
  `ox-<first-20-safe-id-chars>`.
- Add an `ox-task` tag and put the full task and attempt IDs in the VM comment
  or worker bootstrap configuration.
- Before `new`, run an exact-name `ls` query.
- If `new` times out or its response is lost, query `ls` again. Adopt the VM
  only when its identity metadata matches the current task attempt.
- A conflicting VM with the same name but different metadata is a hard error;
  never delete or adopt it automatically.

exe.dev's `/exec` API has no request nonce/replay protection, so local durable
identity and reconciliation provide the operation-level idempotency.

### Provisioning sequence

1. Persist `provisioning`.
2. Reconcile deterministic VM name with `ls`.
3. Issue `new --name=... --image=<digest> --cpu=... --memory=... --disk=...`
   plus allow-listed integrations and task metadata.
4. Persist the returned VM identity and `booting`.
5. Acquire the selected carrier credential: an ox-specific SSH identity for
   the SSH carrier, or a short-lived VM-scoped token for HTTPS.
6. Connect the chosen `RemoteStore` carrier and read `health` from the remote
   public store.
7. Persist `submitting`.
8. Write the idempotent conversation/task request to the remote public store.
9. Persist the remote acknowledgement and `running`.

Only the exe.dev control token needs `new`, `ls`, `rm`, and
`ssh-key generate-api-key`. It should have a short expiry. The data-plane SSH
identity is distinct so it can be revoked without affecting the user's
interactive SSH identity.

## CLI conversation contract

The current repository does not yet have these commands (see G5). The smallest
user-facing addition creates one remote ox conversation per disposable VM:

```console
$ ox remote new --repo owner/repository --rev main \
    --prompt "Investigate the parser failures and report back"
remote: r_01...
vm:     ox-r-01...
state:  running

$ ox remote attach r_01...
[remote conversation output]
> Try the failing fuzz seed as well

$ ox remote logs r_01... --from 42
$ ox remote send r_01... "Summarize the minimal fix"
$ ox remote stop r_01...
$ ox remote delete r_01... --vm
```

`new` is not a special remote-agent RPC. Internally it:

1. writes a VM specification to `exe/vms` and reads the returned handle until
   the VM is ready;
2. opens a carrier and mounts the worker's public store as a `RemoteStore`;
3. writes `CreateConversation` to `conversations` and stores the returned
   conversation path with the VM identity and transport settings;
4. prints a stable local remote reference.

`attach` repeatedly reads `conversations/<id>/events/from/<seq>` and writes user
input to `conversations/<id>/messages`. `logs`, `send`, `stop`, and `delete`
are the same path operations in noninteractive form. CLI parsing, terminal
rendering, and confirmation prompts remain outside StructFS; every operation
that crosses into exe.dev or the remote ox runtime goes through an explicitly
wired Store.

## StructFS wire transport

Isotope defines the semantic read/write protocol but deliberately does not
select a network wire format. Ox therefore defines one transport-neutral frame
protocol in `ox-structfs-transport`. Both HTTPS and SSH carriers, if retained,
carry exactly these frames and expose the same `RemoteStore` implementation.

### Frames

Each request contains:

- wire protocol version;
- `request_id`, unique for the logical operation;
- operation: `read` or `write`;
- path relative to the exported Store root;
- a Record for writes;
- an optional bounded wait/deadline for reads.

Each response echoes `request_id` and contains exactly one of:

- a read result: absent or one Record;
- a write result: the returned relative path;
- a store-level error with type, message, and retryability.

Records preserve both StructFS forms:

- parsed semantic Values; or
- raw bytes plus their `Format` hint.

The encoding must round-trip null, booleans, signed 64-bit integers, 64-bit
floats, UTF-8 strings, byte strings, arrays, and string-keyed maps without
loss. Frames are length-delimited and size-limited. The MVP may choose CBOR as
the canonical binary encoding and provide a JSON diagnostic codec, but the
encoding is fixed by wire-version rather than negotiated per request.

Writes are deduplicated by authenticated peer identity plus `request_id`. The
server persists the returned path or error before acknowledging the frame.
Reads may be replayed. A disconnected blocking read is simply reissued against
the same cursor-bearing path.

Multiple requests may be in flight. Responses can arrive out of order and are
correlated by request ID, so one blocking `events/from/<seq>` read cannot stall
unrelated status, message, or cancellation operations.

### SSH carrier with RuSSH

The preferred CLI MVP candidate is a RuSSH client using exe.dev's existing SSH
route:

1. Connect to the VM's recorded `ssh_host`/`ssh_user` and verify its host key.
2. Authenticate with an ox-specific SSH key.
3. Open a session channel without a PTY.
4. Execute `ox-worker structfs-stdio`.
5. Treat the resulting bidirectional channel stream as the framed StructFS
   connection.
6. Reconnect and reopen the exec channel after network loss; durable cursors
   and request IDs restore operation state.

RuSSH is Tokio-based; its channel can execute a command, split into concurrent
read/write halves, or become an `AsyncRead + AsyncWrite` stream. Pin at least a
patched release newer than the 0.62.4 server-channel vulnerability; the current
reviewed release at specification time is 0.63.1. References:
[RuSSH client](https://docs.rs/russh/latest/russh/client/),
[RuSSH channel API](https://docs.rs/russh/latest/russh/struct.Channel.html), and
[GHSA-m65r-rprj-r5rg](https://github.com/advisories/GHSA-m65r-rprj-r5rg).
RuSSH also requires an explicit `ring` or `aws-lc-rs` crypto backend; the
workspace must choose and test one rather than accepting an accidental feature
unification. See the [RuSSH repository](https://github.com/Eugeny/russh).

This design uses the VM's existing SSH server; `ox-worker` does not implement
an SSH server. An OpenSSH `Subsystem` entry could replace the exec command
later, but is not required.

Before adopting RuSSH on Android, a spike must verify its selected crypto
backend, host-key persistence, and signing with Android-protected key material.
If that is unsatisfactory, HTTPS remains an allowed carrier for the same
StructFS frames; it does not gain domain-specific endpoints.

### HTTPS carrier

The HTTPS alternative exposes one authenticated transport endpoint, such as
`POST /structfs/v1`, whose request and response bodies are StructFS frames.
`X-Exedev-Authorization` is consumed by the exe.dev proxy. Bounded long reads
replace a permanently open response. There are no `/tasks`, `/messages`, or
`/approvals` routes.

Carrier selection is a parent Assembly binding. Blocks and application code
see only the mounted `RemoteStore`.

## Remote worker public store

The worker's external contract is paths, Records, References, and returned
handles:

```text
read  health

write conversations <CreateConversation>
    -> conversations/<conversation-id>
read  conversations/<conversation-id>
read  conversations/<conversation-id>/events/from/<seq>
read  conversations/<conversation-id>/result
read  conversations/<conversation-id>/ledger/from/<seq>

write conversations/<conversation-id>/messages <ConversationMessage>
    -> conversations/<conversation-id>/messages/<message-id>
write conversations/<conversation-id>/approvals/<approval-id> <ApprovalResponse>
write conversations/<conversation-id>/control/cancel {}
write conversations/<conversation-id> null

read  conversations/<conversation-id>/artifacts
read  conversations/<conversation-id>/artifacts/<artifact-id>
```

A remote subtask is a conversation created with task metadata and an initial
prompt. The local `subtasks/outstanding/<task-id>` store is a durable local
projection and reference to that remote conversation. This lets the same remote
primitive support `ox remote new` interactive conversations and agent-spawned
background subtasks.

`events/from/<seq>` is the canonical resumable interface and may block for a
bounded interval. It returns all available sequenced events. Clients recover
solely by repeating reads with the last committed sequence.

### Conversation creation record

```json
{
  "schema_version": 1,
  "task_id": "k_...",
  "attempt_id": "a_...",
  "title": "Investigate failing parser tests",
  "prompt": "...",
  "parent": {
    "thread_id": "t_...",
    "context_entries": [],
    "attachments": []
  },
  "workspace": {
    "kind": "git",
    "repository": "owner/repository",
    "revision": "<commit>",
    "writeback": "none"
  },
  "agent": {
    "assembly_digest": "sha256:...",
    "role": "default"
  },
  "policy": {
    "profile": "remote-disposable-v1",
    "deadline_unix_ms": 0
  }
}
```

`context_entries` is a deliberately selected slice, not a copy of the full
local ledger. The record contains no API keys, exe.dev tokens, or flattened
secret snapshots. Transport `request_id` remains outside the semantic record.

### Events

Every worker event has a monotonically increasing sequence within one task:

```json
{
  "task_id": "k_...",
  "attempt_id": "a_...",
  "seq": 17,
  "time_unix_ms": 1788200000000,
  "kind": "log_entry",
  "body": {}
}
```

Required event kinds:

- `accepted`
- `state_changed`
- `log_entry`
- `approval_requested`
- `input_requested`
- `artifact_created`
- `completed`
- `failed`
- `cancelled`

The terminal event includes the final sequence and result summary. The worker
must never reuse a sequence number with different content.

### Messages and approvals

A follow-up message has its own stable `message_id`. Replaying it does not add a
second user entry. If the task is not waiting for input, the message is queued
for the next turn.

An approval response identifies the original `approval_id`, decision, optional
edited input, and responding user. First accepted response wins. Replays return
the stored decision; conflicting second decisions return a StructFS `conflict`
error.

The worker persists an approval request before emitting its event and persists
the response before waking the agent. An in-memory oneshot is permitted only as
a wakeup optimization.

## Ledger and result ownership

The remote worker is the sole writer of the remote child thread ledger. The
local device stores:

- the parent task record;
- a read-only replica or projection of remote events;
- the last verified remote sequence;
- a terminal result reference.

It does not independently append reconstructed entries to the remote hash
chain. After completion, the remote ledger may be imported as an immutable
child-thread bundle. The parent ledger records only delegation start, important
status transitions, and a final tool result/reference.

Artifact records contain digest, size, media type, logical name, and download
reference. Large artifact bytes never travel in an event record.

## Cancellation, leases, and cleanup

- Cancellation is idempotent and cooperative first. The worker stops accepting
  new turns, interrupts the current run when supported, commits `cancelled`, and
  flushes its ledger.
- If the worker cannot be reached, local state remains `cancelling`; cleanup may
  delete the VM after the configured grace period.
- Each task has a lease deadline. The local coordinator renews it while the task
  is wanted. The worker may self-stop after lease expiry, but it must retain the
  terminal record on disk.
- Default cleanup is `delete_on_success_retain_on_failure` during development
  and `delete_after_terminal_grace` for production.
- `rm <vm-name>` is issued only after the deterministic name has been reconciled
  to the task's recorded identity.

## Failure semantics

Errors exposed to Blocks use store-level categories:

- `unavailable`, retryable — network failure, exe.dev rate limit, worker booting;
- `timeout`, retryable — bounded control or worker request elapsed;
- `validation_failed`, permanent — invalid task or unsupported protocol;
- `forbidden`, permanent — missing capability or rejected credential;
- `conflict`, permanent until operator action — VM identity mismatch;
- `remote_failed`, permanent for the attempt — agent reached a terminal error.

Raw provider responses and tokens may appear in protected diagnostic logs but
not in user-visible error records.

## Security requirements

- Store the exe.dev control token in a platform secret store.
- Grant only the four required exe.dev commands and set an expiry.
- Use a distinct ox SSH identity, persist the verified VM host key, and reject
  changed or unknown host keys unless an explicit enrollment flow accepts
  them.
- Use a VM-scoped token for worker HTTPS; prefer
  `X-Exedev-Authorization` so the proxy strips it before forwarding.
- Keep the worker proxy private.
- Pin the worker image by digest and verify the expected agent Assembly digest.
- Reject task envelopes above configured size and attachment count limits.
- Treat remote event text, artifact names, and repository content as untrusted.
- Do not transmit the local Gate secret backing or `keys.json`.
- Disable repository writeback in the initial release. Add push/PR capabilities
  later as separate explicit integrations and policies.

## Observability

Each log record carries `task_id`, `attempt_id`, `vm_name`, and `request_id`
where applicable. Required counters and durations:

- accepted, active, terminal, retained, and leaked tasks;
- VM provision and boot latency;
- task run latency;
- event reconciliation lag;
- retries by operation and error class;
- cleanup failures;
- current VM count against configured limit.

No metric label may contain prompts, repository content, or tokens.

## Shortest path to a remote/background MVP

The MVP should prove the risky distributed properties without waiting for the
Flutter app or the complete Isotope runtime extraction.

### MVP slice

1. Extract the non-UI portions of `AgentPool`, worker construction, and
   `run_one_turn` into a reusable headless crate. Keep CLI behavior unchanged.
2. Implement the versioned StructFS frames, an in-memory transport conformance
   harness, and `ox-worker structfs-stdio` exposing only the public worker
   store.
3. Implement a RuSSH `RemoteStore`, including pinned host-key verification,
   multiplexing, reconnect, and write-request replay.
4. Build and manually publish one pinned `ox-worker` image.
5. Implement `ExeControlStore` plus durable `RemoteSubtaskStore` under the
   existing broker, using the gateway's handle-store conventions.
6. Add `ox remote new`, `ox remote attach`, `ox remote send`,
   `ox remote logs`, and `ox remote stop`. Each command resolves to reads or
   writes on `ExeControlStore`, `RemoteSubtaskStore`, or the mounted remote
   public store; no command calls a domain endpoint.
7. Add a reconciliation loop that can be killed and restarted while the VM
   continues. Demonstrate recovery from the stored event cursor.
8. Surface remote status and terminal result in the existing inbox/thread UI.

### Deliberate MVP cuts

- one task per VM;
- fixed worker image, CPU, memory, disk, and integrations;
- git clone by repository/revision only;
- no artifact upload beyond a final textual result and ledger download;
- bounded blocking StructFS reads; no streaming event protocol beyond the
  framed SSH channel;
- no reverse connection to the phone;
- no remote approval round-trip: approval-requiring actions fail closed under
  the MVP policy;
- no automatic resumption of the parent agent when the child finishes; the
  inbox surfaces the result and the user decides whether to continue the
  parent thread. Durable autonomous parent wakeup is a follow-on feature;
- no repository writeback;
- manual cleanup button plus an automatic terminal grace timer;
- desktop CLI is the first client; Android consumes the same store afterward.

This slice tests provisioning idempotency, worker durability, reconnection,
sequenced messaging, cancellation, and cleanup—the parts least helped by
building Flutter first.

### MVP exit criteria

- Starting two tasks creates two independently observable VMs and returns both
  handles without blocking the parent agent until either task completes.
- Killing the local ox process during a task and restarting it resumes from the
  last persisted event sequence without duplication.
- Replaying task submission does not start a second agent thread.
- An ambiguous `new` timeout is reconciled through `ls`; it does not create a
  second VM.
- Cancellation reaches a live worker and reaches a terminal local state.
- A completed task produces a final result and an immutable child ledger.
- Cleanup never deletes a VM whose recorded identity does not match the task.
- No local secret snapshot or exe.dev token appears in a task envelope, event,
  or ledger.

### Critical path and estimate

For one engineer familiar with the repository:

| Work | Estimate | Can overlap |
|---|---:|---|
| Extract reusable headless agent host | 3–5 days | wire fixture design |
| Implement StructFS frames and durable worker public store | 4–6 days | image/build automation |
| Implement and harden RuSSH `RemoteStore` | 3–5 days | worker work after fixtures |
| Implement exe.dev control and reconciliation | 3–5 days | worker work after fixtures |
| Implement `RemoteSubtaskStore` and CLI surface | 4–6 days | exe.dev adapter |
| Restart, duplicate, cancellation, and cleanup tests | 4–6 days | UI status work |

That is approximately 5–7 engineer-weeks for the durable MVP, or roughly 3–5
calendar weeks with two engineers after protocol fixtures are settled. A
hard-coded provisioning demonstration can be produced sooner, but it is not a
background-task MVP until restart reconciliation and idempotent cleanup pass.

## Deferred after MVP

- durable remote approval and follow-up-message UI;
- reusable warm VM pools;
- artifact manifests and resumable downloads;
- repository push and pull-request capabilities;
- an HTTPS carrier and additional carrier negotiation;
- routing a nested Assembly or individual Block to a remote runtime;
- multi-device task ownership and conflict resolution.

## Acceptance criteria for an implementing plan

- The plan does not create a competing runtime or copy gateway internals into a
  second implementation. Any gateway code moved into a shared crate is moved
  in a behavior-preserving task, followed immediately by the existing gateway
  parity tests. The remote MVP may use the current broker and the same handle
  conventions without waiting for the complete runtime extraction.
- Protocol fixtures pin every StructFS request/response frame, public-store
  record, event kind, and state transition before carrier implementation.
- Fake exe.dev and fake worker adapters cover timeout-after-success,
  duplicate-submit, disconnect, out-of-order response, and cleanup conflict.
- The plan names one authoritative writer for every persisted record and ledger.
- The implementation passes `./scripts/fmt.sh` and
  `./scripts/quality_gates.sh` before completion.
