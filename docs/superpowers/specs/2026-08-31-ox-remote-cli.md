# ox remote CLI extension

**Date:** 2026-08-31

**Status:** Draft specification, not an implementation plan

**Scope:** the desktop `ox` CLI, exe.dev VM lifecycle, the remote ox worker,
and the StructFS transport required to connect them. Flutter, Android, agent-
initiated subtasks, and transparent placement of arbitrary Blocks are outside
this specification.

## Summary

The `ox remote` command family creates and operates durable ox conversations
on exe.dev nodes. A node runs one long-lived, headless instance of the same
execution core used by the local CLI and may host multiple conversations. The
local command applies a placement policy, connects with RuSSH, mounts the
worker's public StructFS Store, creates a conversation by writing to that
Store, and records a stable local reference. The remote conversation continues
after the local CLI exits.

All ox-side communication is expressed as StructFS reads and writes:

```text
CLI command
    -> local RemoteManagerStore
        -> ExeControlStore
            -> exe.dev SSH API adapter
        -> RemoteStore
            -> framed StructFS over a RuSSH exec channel
                -> remote worker public Store
```

exe.dev's native SSH command API and the SSH byte stream are host adapters at
the edge. No CLI handler, coordinator, or agent calls an exe.dev command,
RuSSH method, worker HTTP route, or domain RPC directly.

The initial user experience is:

```console
$ ox remote new --repo https://github.com/acme/widget.git \
    --rev 8f3b29e --prompt "Investigate the parser failures"
remote: r_4c4f...
vm:     ox-4c4f...
state:  running

$ ox remote attach r_4c4f...
[assistant] I reproduced the failure in parser::tests::escaped_delimiter.
> Also check the minimized fuzz seed.

$ ox remote show r_4c4f...
$ ox remote logs r_4c4f... --from 20 --follow
$ ox remote send r_4c4f... --prompt "Summarize the smallest safe fix"
$ ox remote cancel r_4c4f...
$ ox remote vm delete r_4c4f...
```

## Goals

- Create a remote ox conversation on a selected or newly provisioned exe.dev
  node from one command.
- Let the conversation continue without a resident local ox process.
- Reattach, send follow-up messages, stream ledger-backed output, inspect state,
  cancel work,
  and explicitly delete the VM.
- Recover safely when any local command is killed or loses its connection.
- Prevent a retry from creating a second VM, conversation, or user message.
- Expose both human-readable and stable machine-readable output.
- Keep transport, provisioning, and worker details behind StructFS Stores.
- Reuse the existing broker, `AgentPool`, `ThreadRegistry`, Wasmtime runtime,
  tool execution, approval, ledger, and restore machinery instead of creating
  a second remote-agent implementation.
- Preserve one authoritative writer for the remote conversation ledger.
- Allow independent conversations to share a node without a blocked approval,
  model request, or cursor read blocking unrelated conversations.

## Non-goals

- Android or Flutter integration.
- Agent-initiated background subtasks in the first CLI release.
- A permanently running local daemon.
- Warm pools and automatic cross-node migration in the MVP.
- Synchronizing uncommitted local workspace changes.
- Automatically pushing branches or opening pull requests.
- Interactive shell access to the VM through ox.
- Generic SSH command execution.
- Exposing the remote worker's substrate root or internal Assembly Stores.
- Transport negotiation. Wire version 1 uses one fixed encoding over SSH.
- Replacing exe.dev's own CLI or providing general VM administration.
- A second executor, conversation registry, scheduler, durability layer, or
  event journal in the remote worker.
- Per-conversation host processes or containers. Wasm execution and sandboxed
  tool subprocesses remain the existing isolation boundaries.

## Prerequisites verification manifest

- [x] **C1. The current binary has only one subcommand and always proceeds
  into the TUI startup path.**
  Verified with
  `nl -ba crates/ox-cli/src/main.rs | sed -n '60,130p'` and
  `nl -ba crates/ox-cli/src/main.rs | sed -n '174,242p'`.
  `Commands` contains only `Init` at `crates/ox-cli/src/main.rs:87`; broker and
  Ratatui setup begin at lines 176 and 235. Remote commands therefore require
  an explicit non-TUI dispatch path before terminal initialization.

- [x] **C2. The reusable agent loop is currently inside the CLI module tree.**
  Verified with
  `nl -ba crates/ox-cli/src/agents.rs | sed -n '92,235p'` and
  `nl -ba crates/ox-cli/src/agents.rs | sed -n '637,715p'`.
  `AgentPool` owns loaded agent modules and worker state at
  `crates/ox-cli/src/agents.rs:101`; its prompt loop starts at line 637 and
  `run_one_turn` at line 697. The worker needs a behavior-preserving packaging
  of this executor, not another agent host.

- [x] **C3. Broker setup is the current namespace assembly point.**
  Verified with
  `nl -ba crates/ox-cli/src/broker_setup.rs | sed -n '1,152p'` and
  `nl -ba crates/ox-broker/src/broker.rs | sed -n '23,77p'`.
  `broker_setup::setup` documents and mounts the CLI Stores at
  `crates/ox-cli/src/broker_setup.rs:24`; `BrokerInner::route` applies
  longest-prefix routing at `crates/ox-broker/src/broker.rs:64`.

- [x] **C4. The current async Store seam is repository-local but carries
  StructFS data types.**
  Verified with
  `nl -ba crates/ox-broker/src/async_store.rs | sed -n '1,24p'`.
  `ox_broker::async_store::{AsyncReader, AsyncWriter}` accepts StructFS
  `Path`, `Record`, and `Error` at `crates/ox-broker/src/async_store.rs:5` and
  lines 11–19. The first `RemoteStore` targets this seam.

- [x] **C5. The broker already supports scoped clients and per-client
  timeouts.**
  Verified with
  `nl -ba crates/ox-broker/src/client.rs | sed -n '17,117p'` and
  `nl -ba crates/ox-broker/src/lib.rs | sed -n '292,362p'`.
  `ClientHandle::with_timeout` is at `crates/ox-broker/src/client.rs:53`,
  scoping is at line 63, and the broker default timeout is 30 seconds at
  `crates/ox-broker/src/lib.rs:358`. Cursor-following commands must select
  their own bounded read timeout rather than changing the global default.

- [x] **C6. `ox-inbox` owns `~/.ox/ox.db`, schema initialization, and thread
  creation through StructFS.**
  Verified with
  `nl -ba crates/ox-inbox/src/lib.rs | sed -n '42,81p'`,
  `nl -ba crates/ox-inbox/src/schema.rs | sed -n '1,65p'`, and
  `nl -ba crates/ox-inbox/src/writer.rs | sed -n '22,75p'`.
  The database is opened at `crates/ox-inbox/src/lib.rs:58`, the schema is
  initialized at line 60, and `write threads` dispatches to durable thread
  creation at `crates/ox-inbox/src/writer.rs:30`.

- [x] **C7. Existing inbox tasks are too narrow to own remote conversation
  recovery state.**
  Verified with
  `nl -ba crates/ox-inbox/src/schema.rs | sed -n '21,42p'` and
  `nl -ba crates/ox-inbox/src/reader.rs | sed -n '193,217p'`.
  A task currently contains only ID, thread ID, title, status, and timestamps at
  `crates/ox-inbox/src/schema.rs:27`. Remote conversations need separate tables
  rather than overloading this row.

- [x] **C8. Current configuration has only the gate namespace; secrets are a
  separately backed Store.**
  Verified with
  `nl -ba crates/ox-config/src/config.rs | sed -n '17,89p'` and
  `nl -ba crates/ox-cli/src/broker_setup.rs | sed -n '89,109p'`.
  `OxConfig` contains only `gate` at `crates/ox-config/src/config.rs:18`.
  `config/` and `secret/` use distinct backing files at
  `crates/ox-cli/src/broker_setup.rs:89` and line 98.

- [x] **C9. No exe.dev, RuSSH, remote-conversation, or StructFS wire
  implementation exists in current source.**
  Verified with
  `rg -n "exe\\.dev|russh|RemoteStore|RemoteSubtask|structfs-stdio|StructFs.*Frame" crates Cargo.toml`.
  The search returned no matches. All named components in this specification
  are additions.

- [x] **C10. The pinned StructFS source defines `Record` and `Value` shapes but
  intentionally leaves the network wire format unspecified.**
  Verified with
  `nl -ba /Users/alex/.cargo/git/checkouts/structfs-33a5c53178d143e8/80a613e/packages/core-store/src/record.rs | sed -n '37,73p'`,
  `nl -ba /Users/alex/.cargo/git/checkouts/structfs-33a5c53178d143e8/80a613e/packages/core-store/src/value.rs | sed -n '20,40p'`, and
  `nl -ba /Users/alex/.cargo/git/checkouts/structfs-33a5c53178d143e8/80a613e/isotope/spec/00-overview.md | sed -n '102,112p'`.
  `Record` is raw bytes plus `Format` or a parsed `Value`; `Value` includes
  null, booleans, i64, f64, strings, bytes, arrays, and string-keyed maps.

- [x] **C11. exe.dev exposes programmatic VM control over SSH and returns the
  connection fields the client needs.**
  Verified on 2026-08-31 against the official
  [exe.dev API documentation](https://exe.dev/docs/api), which documents
  `ssh exe.dev ls --json` and the `ssh_dest`, `ssh_host`, and optional
  `ssh_user` fields. The official [`new`](https://exe.dev/docs/cli-new),
  [`ls`](https://exe.dev/docs/cli-ls), and [`rm`](https://exe.dev/docs/cli-rm)
  references document the required lifecycle commands.

- [x] **C12. RuSSH can open an exec channel and expose it as a concurrent byte
  stream.**
  Verified on 2026-08-31 against RuSSH 0.63.1
  [client documentation](https://docs.rs/russh/latest/russh/client/) and
  [`Channel`](https://docs.rs/russh/latest/russh/struct.Channel.html).
  `client::connect` returns a handle that opens channels; `Channel::exec`,
  `split`, and `into_stream` provide the required no-PTY carrier behavior.

- [x] **C13. `AgentPool` already runs multiple conversations independently and
  shares one compiled agent module.**
  Verified with
  `nl -ba crates/ox-cli/src/agents.rs | sed -n '95,301p'`.
  The pool owns `HashMap<String, ThreadHandle>` at
  `crates/ox-cli/src/agents.rs:103`, loads one module at lines 160–162, clones
  it for each worker at line 266, and starts one worker thread and mailbox per
  ox thread at lines 261–299. Remote execution must reuse this concurrency
  model. The current pool-wide workspace at line 104 is the configuration seam
  that must become per-thread for shared-node workspaces.

- [x] **C14. Each agent turn already receives a fresh Wasmtime Store and
  instance.**
  Verified with
  `nl -ba crates/ox-runtime/src/engine.rs | sed -n '35,105p;262,290p'`.
  `AgentRuntime` owns reusable engine/linker infrastructure at
  `crates/ox-runtime/src/engine.rs:35-70`; `AgentModule::run` constructs a new
  Store and instance at lines 262–283. A per-conversation runtime or host
  process would duplicate the existing Wasm isolation boundary.

- [x] **C15. `ThreadRegistry` already owns lazy per-conversation namespaces,
  approval routing, restore, and per-thread durability.**
  Verified with
  `nl -ba crates/ox-cli/src/thread_registry.rs | sed -n '200,430p;598,730p'`.
  Restore replays before installing `LedgerWriter` at
  `crates/ox-cli/src/thread_registry.rs:200-211` and `:393-407`; the registry
  lazily mounts a thread at `:621-670` and routes approval paths at `:683-728`.
  The public worker Store must adapt to this registry, not add another
  conversation catalog or durability path.

- [x] **C16. The existing ledger is already the ordered durable event source.**
  Verified with
  `nl -ba crates/ox-kernel/src/log.rs | sed -n '209,285p'` and
  `nl -ba crates/ox-inbox/src/ledger_writer.rs | sed -n '1,38p'`.
  `SharedLog::append` commits before making an entry visible at
  `crates/ox-kernel/src/log.rs:266-283`; the per-thread writer owns ordered,
  hash-chained sync commits at `crates/ox-inbox/src/ledger_writer.rs:1-24`.
  Remote cursors therefore project this ledger; the worker does not write a
  separate event stream.

- [x] **C17. Async broker requests are independently spawned.**
  Verified with
  `nl -ba crates/ox-broker/src/server.rs | sed -n '90,139p'`.
  `async_server_loop` spawns each read and write at
  `crates/ox-broker/src/server.rs:110-138`, so a parked cursor read need not
  block another conversation. Public and transport-facing Stores must use the
  async mount path; the synchronous server at lines 37–61 is not suitable.

## User experience principles

1. A remote reference names the conversation, not merely its current VM.
2. `new` does not report success until the remote worker has durably accepted
   the conversation.
3. Losing or closing a terminal never implies cancellation.
4. Cancellation and VM deletion are distinct, explicit operations.
5. Human output goes to stdout, progress and diagnostics to stderr.
6. `--json` output is stable, contains no ANSI escapes, and emits one JSON
   value unless a command explicitly documents JSON Lines.
7. Every mutation accepts or internally creates an idempotency key.
8. Unknown SSH host keys require an explicit trust decision; changed keys are
   always rejected.
9. No command accepts an arbitrary remote shell fragment.

## Command grammar

The existing global `--account`, `--model`, `--workspace`, `--max-tokens`, and
`--no-policy` flags retain their current meaning for local TUI operation. The
new top-level grammar is:

```text
ox remote <COMMAND> [OPTIONS]

Commands:
  new             provision a VM and create a conversation
  list            list locally known remote conversations
  show            show a conversation and its node
  attach          follow events and optionally send messages
  send            enqueue one follow-up message
  logs            read or follow ordered remote ledger output
  cancel          request conversation cancellation
  approve         answer a pending remote approval
  reconcile       reconcile durable local state with exe.dev and the worker
  doctor          verify authentication, image, and transport prerequisites
  vm delete       delete a node VM after identity and liveness verification
```

All commands accept:

```text
--json                    stable machine-readable output
--identity <PATH>         command-scoped SSH private-key override
--connect-timeout <DUR>   TCP/SSH connection timeout
--operation-timeout <DUR> non-streaming Store operation timeout
```

The default identity order is: explicitly configured identity, compatible
identity offered by `SSH_AUTH_SOCK`, then conventional OpenSSH identity files.
The implementation must not silently try every private key on disk.

### `ox remote new`

```text
ox remote new
    (--prompt <TEXT> | --prompt-file <PATH> | --stdin)
    [--repo <URL> --rev <REVISION>]
    [--title <TEXT>]
    [--cpu <COUNT>]
    [--memory <SIZE>]
    [--disk <SIZE>]
    [--integration <NAME>]...
    [--ttl <DURATION>]
    [--placement <fresh-node|prefer-existing>]
    [--node <NODE>]
    [--attach]
    [--accept-new-host-key]
```

Exactly one prompt source is required. `--stdin` reads until EOF and is valid
in non-TTY automation. `--repo` and `--rev` must be supplied together in the
MVP; omitting both creates an empty workspace. A revision may be a branch for
interactive use, but machine output records the resolved commit reported by
the worker.

`--attach` enters attach mode after remote acceptance. Without it, `new`
prints the reference and exits. This is still a background conversation: the
command waits through provisioning because there is no local daemon, but it
does not wait for the agent turn to complete.

Placement is independent of conversation identity. `--placement fresh-node`
provisions a node, while `prefer-existing` selects a compatible known node
with advertised capacity and falls back to provisioning. `--node` means
`require_node`: use that exact verified node or fail, and is mutually exclusive
with `--placement`. The MVP defaults to `fresh-node` as a rollout policy. This
default does not constrain node cardinality; tests must create two independent
conversations on one explicitly selected node.

Resource flags are validated locally against configured policy. The MVP may
ship fixed defaults and reject overrides until budget enforcement exists.
`--integration` accepts validated exe.dev integration identifiers, never raw
environment variables or credentials.

Human output after acceptance is exactly sufficient to recover:

```text
remote: r_<id>
vm:     <vm-name>
state:  running
attach: ox remote attach r_<id>
```

JSON output is a `RemoteConversationSummary` record.

### `ox remote list`

```text
ox remote list [--all] [--refresh]
```

The default reads local durable state and does not contact every VM. `--refresh`
reconciles each nonterminal conversation with bounded concurrency before
printing. `--all` includes old terminal records and records whose VMs have
already been deleted; the default view includes active and recently terminal
conversations.

### `ox remote show`

```text
ox remote show <REMOTE> [--refresh]
```

`<REMOTE>` resolves an exact local reference, an exact VM name, or an
unambiguous reference prefix. Ambiguous prefixes fail and print candidates.
The result includes conversation state, VM identity, transport endpoint,
cursor, timestamps, cleanup state, and last error. It never prints keys,
tokens, complete prompts, or integration credentials.

### `ox remote attach`

```text
ox remote attach <REMOTE> [--from <SEQ>] [--read-only]
```

Attach first reconciles the target, renders all ledger entries after the locally
committed cursor or explicit `--from`, then follows with bounded reads.

In an interactive TTY, each submitted line becomes one message. EOF and the
first Ctrl-C detach without cancelling the conversation. `--read-only` never
reads stdin. Attach is line-oriented in the MVP; integrating the existing full
TUI is deferred.

Approval-request entries are rendered, but attach never converts a single keypress into
an approval. The user runs the explicit `approve` command or enters a clearly
labelled confirmation flow.

### `ox remote send`

```text
ox remote send <REMOTE>
    (--prompt <TEXT> | --prompt-file <PATH> | --stdin)
    [--message-id <ID>]
```

The client generates and persists `message_id` before the write unless one is
provided. A retry reuses that ID. The worker either starts another turn,
supplies requested input, or durably queues the message; it never silently
drops a message because the agent is busy.

### `ox remote logs`

```text
ox remote logs <REMOTE>
    [--from <SEQ>]
    [--limit <COUNT>]
    [--follow]
    [--jsonl]
```

Without `--follow`, the command prints available ledger entries and exits. `--follow`
uses bounded cursor reads and reconnects with exponential backoff. `--jsonl`
emits one complete `RemoteLedgerEntry` per line; progress remains on stderr.

### `ox remote cancel`

```text
ox remote cancel <REMOTE> [--wait] [--timeout <DURATION>]
```

Cancellation is cooperative and idempotent. It does not delete the VM. With
`--wait`, the command follows state until `cancelled`, another terminal state,
or timeout. A timeout leaves local state as `cancelling`, not `failed`.

### `ox remote approve`

```text
ox remote approve <REMOTE> <APPROVAL_ID>
    (--allow | --deny)
    [--edited-input <JSON>]
```

The decision is displayed in full before submission. First accepted response
wins. Repeating the same decision succeeds; a conflicting decision returns a
conflict error. The MVP may fail all approval-requiring operations closed and
defer this command, but the path and record contract are reserved now.

### `ox remote reconcile`

```text
ox remote reconcile [<REMOTE>] [--repair-cursor]
```

Without a target, reconciliation visits all nonterminal or incompletely
cleaned records with bounded concurrency. It is safe to repeat.

`--repair-cursor` verifies cached ledger hashes against the worker and rebuilds
the local ledger-envelope cache. It does not rewrite the remote ledger. Reconciliation
never adopts or deletes a VM whose worker-reported identity conflicts with the
stored attempt.

### `ox remote doctor`

Doctor performs read-only checks unless the user explicitly requests a probe
VM in a later version. It verifies:

- config validity and worker image digest;
- SSH identity availability;
- host-key policy and known-hosts file permissions;
- authenticated `whoami` and `ls --json` through `ExeControlStore`;
- local database migration status;
- supported wire version and crypto backend in the local build.

Secrets and private-key material are never included in doctor output.

### `ox remote vm delete`

```text
ox remote vm delete (<REMOTE> | --node <NODE>) [--yes] [--force-running]
```

Deletion resolves the owning node and exact VM name, verifies the stored SSH
host key, and reads the worker's node-attempt identity before calling `rm`. A
mismatch is a hard conflict. If the VM never became reachable enough to
establish identity, ox refuses automatic deletion and prints a manual exe.dev
recovery command; it does not guess that an exact-name VM is safe to remove.

By default, a node with any live conversation cannot be deleted.
`--force-running` enumerates and requests cancellation for every live
conversation on the node and displays that unflushed work may be lost. `--yes`
suppresses only the interactive confirmation; it does not bypass identity,
enumeration, or state checks.

## Output and process contract

### Streams

- stdout: requested data, summaries, conversation output, or JSON;
- stderr: progress, retries, warnings, and diagnostics;
- no ANSI escapes when stdout is not a TTY or `NO_COLOR` is set;
- prompt text and remote content are not echoed to diagnostic logs.

### Exit codes

| Code | Meaning |
|---:|---|
| 0 | operation succeeded or detach completed normally |
| 2 | CLI usage or local validation error |
| 3 | remote reference or path not found |
| 4 | authentication, authorization, or host-key failure |
| 5 | transport or exe.dev temporarily unavailable |
| 6 | identity, idempotency, or approval conflict |
| 7 | remote conversation reached `failed` |
| 8 | requested wait timed out while durable work remains active |
| 10 | local persistence or invariant failure |

For JSON commands, failures write one `CliError` object to stderr and no
partial JSON value to stdout. JSON Lines commands may have emitted complete
ledger entries before a failure; they never emit a partial line.

### Signals

- Ctrl-C during `attach` or `logs --follow` detaches and exits 0.
- Ctrl-C during provisioning persists `interrupted` context and exits 130; it
  does not issue `rm`.
- SIGTERM follows the same durable checkpoint path, then exits 143.
- Cancellation of the remote conversation requires `ox remote cancel`.

## Architecture

### Reuse budget

Remote execution is an adapter around the local executor. New code may expose,
configure, or harden an existing owner, but must not reimplement it.

| Capability | Existing owner to reuse | Permitted remote addition |
|---|---|---|
| conversation creation and index | `InboxStore` | durable remote create-id mapping |
| conversation namespace | `ThreadRegistry` | public-path adapter |
| concurrent execution | `AgentPool` worker-per-thread model | headless command handle and bounded admission |
| Wasm execution | `AgentRuntime` / `AgentModule` | cancellation and resource configuration |
| filesystem and shell tools | `ToolStore` + `ox-tool-exec` | fail-closed Linux policy validation |
| approvals | `ApprovalStore` and thread approval paths | public record adapter |
| durable ordered history | `SharedLog` + per-thread `LedgerWriter` | bounded ledger cursor projection |
| crash restore and resume | `ThreadRegistry`, snapshot restore, resume classifier | invoke unchanged at worker startup |
| namespace routing | `BrokerStore` + `mount_async` | headless mount composition |
| config and secrets | existing `config/` and separate `secret/` mounts | worker policy configuration |

Before adding a worker-side table, queue, state machine, Store, or log, the
implementing plan must state why the existing owner cannot carry the
requirement. Remote ingress metadata is justified only because transport
retries need stable create, message, approval, and cancel IDs before invoking
the existing executor.

### Public operation mapping

The worker Store is deliberately thin:

| Public operation | Existing machinery |
|---|---|
| create | `InboxStore` thread creation, then `AgentPool::ensure_worker` |
| initial or follow-up message | durable ingress ID, then existing prompt mailbox |
| summary or result | inbox thread row plus existing log/history projection |
| ledger cursor | bounded projection of existing `ledger.jsonl` envelopes |
| pending approval or response | existing `threads/<id>/approval/*` paths |
| cancel | cancellation control added at the existing worker/runtime seam |

The existing worker thread ID (`t_...`) is the remote worker conversation ID.
The local coordinator may retain a stable `r_...` reference, but the worker
does not invent a second conversation identity.

The worker uses its normal inbox root, `ox.db`, and thread directories. The
only worker persistence added by this project is ingress idempotency metadata:
one row keyed by each create ID, message ID, approval ID, or cancel ID. A
dispatcher applies accepted rows to the existing operation and recovers
accepted-but-not-applied rows after restart. These rows are an input outbox,
not a conversation model or history stream.

### Repository shape

```text
crates/ox-executor/            packaged local/headless execution core
crates/ox-structfs-transport/  frame codec and carrier-independent RemoteStore
crates/ox-remote/              records, state machine, Stores, reconciliation
crates/ox-worker/              thin headless host and public Store server
crates/ox-cli/                 argument parsing, rendering, command dispatch
crates/ox-inbox/               ox.db schema and local remote-record persistence
```

Names are recommendations. Dependency rules are normative:

- `ox-executor` cannot depend on Ratatui, crossterm, or CLI rendering.
- `ox-structfs-transport` cannot know about conversations or exe.dev.
- `ox-remote` cannot depend on CLI presentation.
- `ox-worker` and `ox-cli` both depend on `ox-executor`; neither may carry a
  private copy of the executor modules.
- only `ox-inbox` migrates and directly writes `~/.ox/ox.db`.

### CLI startup split

`main` parses arguments and initializes the inbox root and tracing, then
dispatches before terminal initialization:

```text
no subcommand  -> existing full broker + TUI
init           -> existing setup flow
remote ...     -> headless remote broker + command handler
```

Remote dispatch also occurs before the current local-account setup check.
Creating a remote conversation does not require a usable local completion
account: model credentials are worker-side integrations. Invalid remote config
still fails before external state changes.

The headless remote broker mounts only what the command needs:

```text
inbox/     InboxStore, including durable remote records
config/    configuration Store
secret/    secret handles and future bearer tokens
exe/       ExeControlStore
remote/    RemoteManagerStore
```

It does not initialize Ratatui, crossterm, UI Stores, settings subscriptions,
or a local `ExecutionCore`.

## Local Store contracts

### `RemoteManagerStore`

The CLI talks only to this Store for conversation operations:

```text
write remote/conversations <CreateRemoteConversation>
    -> remote/conversations/<remote-id>

read  remote/conversations
read  remote/conversations/<remote-id>
read  remote/conversations/<remote-id>/ledger/from/<seq>
read  remote/conversations/<remote-id>/result

write remote/conversations/<remote-id>/messages <RemoteMessage>
    -> remote/conversations/<remote-id>/messages/<message-id>
write remote/conversations/<remote-id>/approvals/<approval-id> <ApprovalResponse>
write remote/conversations/<remote-id>/control/cancel {}
write remote/conversations/<remote-id>/control/reconcile {}
write remote/conversations/<remote-id>/control/delete-vm <DeleteVmRequest>

write remote/reconcile <ReconcileRequest>
    -> remote/reconciliations/<reconciliation-id>
read  remote/reconciliations/<reconciliation-id>
```

Conversation creation returns the local handle only after durable local
acceptance. The CLI continues reading that handle until the remote worker has
accepted the conversation. This Store never blocks the initial write on VM
creation.

### `ExeControlStore`

The coordinator sees typed VM capabilities:

```text
write exe/vms <VmSpec>                  -> exe/vms/<vm-name>
read  exe/vms/<vm-name>                 -> VmStatus or absent
write exe/vms/<vm-name>/delete {}       -> exe/vms/<vm-name>/deletions/<id>
read  exe/identity                      -> ExeIdentity
```

Its production adapter uses an authenticated RuSSH connection to `exe.dev`,
executes only typed allow-listed `new --json`, `ls --json`, `rm --json`, and
`whoami --json` commands, and converts their JSON into StructFS Records.

For every command it requires a successful SSH exec acknowledgement and zero
exit status, caps stdout/stderr, parses exactly one expected JSON shape from
stdout, and treats stderr as diagnostic input. Provider JSON is never passed
through as an untyped public Record.

The adapter does not concatenate untrusted strings. It validates every VM
name, resource value, image reference, tag, comment, and integration, then uses
one audited command encoder with correct argument quoting. There is no
`exe/raw`, `exe/exec`, or generic command path.

`VmSpec` may include only two coordinator-generated environment fields,
`OX_NODE_ID` and `OX_NODE_ATTEMPT_ID`, used by the worker's health identity.
Neither the public CLI nor an agent can supply arbitrary VM environment values.

## Remote worker public Store

The SSH transport exports exactly one public Store:

```text
read  health
read  capabilities
read  capacity

write conversations <CreateConversation>
    -> conversations/<thread-id>
read  conversations
read  conversations/<thread-id>
read  conversations/<thread-id>/result
read  conversations/<thread-id>/ledger/from/<seq>

write conversations/<thread-id>/messages <RemoteMessage>
    -> conversations/<thread-id>/messages/<message-id>
read  conversations/<thread-id>/approvals/pending
write conversations/<thread-id>/approvals/<approval-id> <ApprovalResponse>
write conversations/<thread-id>/control/cancel <CancelRequest>

read  conversations/<thread-id>/artifacts
read  conversations/<thread-id>/artifacts/<artifact-id>
```

Paths are relative to the public Store root. The transport server cannot
address the substrate root, internal host capabilities, other Assemblies, or
filesystem paths.

`ledger/from/<seq>` is the canonical live and recovery interface. It returns a
bounded page of the existing per-thread hash-chained ledger envelopes. If none
are available and the conversation is live, it may block until its request
deadline. CLI display events are projections of these entries and are not
persisted by a second worker-side journal.

The `health` record includes worker version, wire versions, Assembly digest,
`node_id`, and `node_attempt_id`. The last two values originate from the
reserved bootstrap environment fields and are the authoritative node-attempt
check used for adoption and deletion. Per-conversation local `remote_id`
values are not VM identity.

`capacity` reports bounded active-turn, prompt-ingress, disk, and configured
conversation capacity. It is advisory for `prefer_existing`; the worker's
atomic create acceptance is authoritative. Status, approval, cancellation,
and ledger reads are not admitted through the active-turn queue.

## Record contracts

All records have `schema_version: 1`. Unknown additive fields are ignored;
missing required fields and unknown enum values are validation errors.

### `CreateRemoteConversation`

```json
{
  "schema_version": 1,
  "operation_id": "op_...",
  "title": "Investigate parser failures",
  "prompt": "...",
  "workspace": {
    "kind": "git",
    "repository": "https://github.com/acme/widget.git",
    "revision": "8f3b29e",
    "writeback": "none"
  },
  "resources": {
    "cpu": 2,
    "memory_mib": 4096,
    "disk_gib": 20
  },
  "integrations": ["github", "llm"],
  "ttl_seconds": 14400,
  "attach_after_create": false
}
```

The local Store validates this record, allocates `remote_id` and the durable
operation IDs, applies placement, and strips local-only fields before writing
the remote `CreateConversation`. When placement provisions a node, it also
allocates `node_id`, `node_attempt_id`, and a deterministic VM name.

### Remote `CreateConversation`

```json
{
  "schema_version": 1,
  "create_id": "c_...",
  "title": "Investigate parser failures",
  "prompt": "...",
  "workspace": {
    "kind": "git",
    "repository": "https://github.com/acme/widget.git",
    "revision": "8f3b29e",
    "writeback": "none"
  },
  "agent": {
    "assembly_digest": "sha256:...",
    "role": "default"
  },
  "policy": {
    "profile": "remote-cli-v1",
    "deadline_unix_ms": 1788214400000
  }
}
```

`create_id` is stable across transport retries and reconnects. The worker
returns the original conversation path when it sees the same ID again. It
returns `conflict` if the same ID carries different semantic content.

No record contains SSH private keys, exe.dev bearer tokens, flattened secret
Stores, or local environment variables.

### `RemoteConversationSummary`

```json
{
  "schema_version": 1,
  "remote_id": "r_...",
  "state": "running",
  "node": {
    "node_id": "n_...",
    "node_attempt_id": "na_...",
    "name": "ox-4c4f...",
    "ssh_host": "ox-4c4f....exe.xyz",
    "ssh_user": null,
    "image_digest": "sha256:..."
  },
  "conversation_path": "conversations/t_...",
  "last_ledger_seq": 17,
  "cleanup_state": "not_started",
  "created_at_unix_ms": 1788200000000,
  "updated_at_unix_ms": 1788200010000,
  "last_error": null
}
```

### `RemoteMessage`

```json
{
  "schema_version": 1,
  "message_id": "m_...",
  "content": "Also check the minimized fuzz seed",
  "created_at_unix_ms": 1788200020000
}
```

`message_id` is the semantic idempotency key. The worker persists a message
before acknowledging its write.

### `RemoteLedgerEntry`

```json
{
  "schema_version": 1,
  "thread_id": "t_...",
  "seq": 17,
  "hash": "sha256:...",
  "parent": "sha256:...",
  "msg": {}
}
```

The envelope is a wire projection of the existing `LedgerEntry { seq, hash,
parent, msg }`; it does not define another event type or sequence. Sequence and
hash-chain validation use the ledger's existing rules. Conversation lifecycle
comes from the existing inbox/thread state and `result` projection, while
approval and input state come from their existing Stores.

The worker's existing per-thread `LedgerWriter` is the sole ledger writer. The
local database may cache immutable ledger envelopes and terminal references
for display and recovery; it never reconstructs, renumbers, or appends a
competing local log. A later import may attach the completed remote ledger as
an immutable bundle.

## Local persistence

`ox-inbox` remains the sole schema and write owner for `~/.ox/ox.db`. It gains
StructFS paths under `inbox/remotes` and the following logical tables:

```sql
CREATE TABLE remote_nodes (
    node_id                TEXT PRIMARY KEY,
    node_attempt_id        TEXT NOT NULL UNIQUE,
    vm_name                TEXT NOT NULL UNIQUE,
    state                  TEXT NOT NULL,
    ssh_dest               TEXT,
    ssh_host               TEXT,
    ssh_user               TEXT,
    ssh_host_fingerprint   TEXT,
    worker_identity_verified_at_ms INTEGER,
    image_digest           TEXT NOT NULL,
    lease_expires_at_ms    INTEGER,
    cleanup_policy         TEXT NOT NULL,
    cleanup_state          TEXT NOT NULL DEFAULT 'not_started',
    error_code             TEXT,
    error_message          TEXT,
    created_at_ms          INTEGER NOT NULL,
    updated_at_ms          INTEGER NOT NULL
);

CREATE TABLE remote_conversations (
    remote_id             TEXT PRIMARY KEY,
    node_id               TEXT NOT NULL REFERENCES remote_nodes(node_id),
    create_id             TEXT NOT NULL UNIQUE,
    operation_id          TEXT NOT NULL UNIQUE,
    title                 TEXT NOT NULL,
    state                 TEXT NOT NULL,
    thread_id             TEXT,
    conversation_path     TEXT,
    last_ledger_seq       INTEGER NOT NULL DEFAULT -1,
    last_ledger_hash      TEXT,
    error_code            TEXT,
    error_message         TEXT,
    created_at_ms         INTEGER NOT NULL,
    updated_at_ms         INTEGER NOT NULL,
    UNIQUE (node_id, thread_id)
);

CREATE TABLE remote_ledger_cache (
    remote_id      TEXT NOT NULL REFERENCES remote_conversations(remote_id),
    seq            INTEGER NOT NULL,
    envelope_cbor  BLOB NOT NULL,
    hash            TEXT NOT NULL,
    parent_hash     TEXT,
    observed_at_ms INTEGER NOT NULL,
    PRIMARY KEY (remote_id, seq)
);

CREATE TABLE remote_operations (
    operation_id   TEXT PRIMARY KEY,
    remote_id      TEXT NOT NULL REFERENCES remote_conversations(remote_id),
    kind           TEXT NOT NULL,
    state          TEXT NOT NULL,
    request_cbor   BLOB NOT NULL,
    result_cbor    BLOB,
    created_at_ms  INTEGER NOT NULL,
    updated_at_ms  INTEGER NOT NULL
);
```

The exact SQL may change during implementation, but these ownership and
uniqueness constraints may not:

- one local row per remote reference;
- one durable node row per node attempt and deterministic VM identity;
- many remote conversation rows may reference one node row;
- one worker thread ID per `(node_id, remote_id)` mapping;
- one cached immutable ledger envelope per `(remote_id, seq)`;
- one durable request and result per mutating operation;
- cursor advancement and ledger-envelope insertion in one transaction;
- credentials stored only by reference or external SSH agent, never in these
  tables.

These tables are local orchestration state owned by `ox-inbox`, not a worker
conversation catalog. The worker continues to use its normal inbox `ox.db` and
thread directories. Its only new rows are idempotent ingress metadata for
create, message, approval, and cancel retries.

## State model

```text
accepted
  -> provisioning
  -> booting
  -> connecting
  -> creating_conversation
  -> running <-> waiting_for_input
             <-> blocked_on_approval
  -> cancelling
  -> completed | failed | cancelled | lost

cleanup:
  not_started -> scheduled -> deleting -> deleted
                                     -> retained | delete_failed
```

Conversation outcome and node cleanup are independent. A node may remain
active after one hosted conversation completes, and cleanup applies to the
node only after all hosted conversations are terminal or explicitly cancelled.

Every state transition is committed before the next external side effect.
Node updates compare `node_attempt_id`; stale operations cannot mutate a newer
node attempt.

`lost` means an exact-name exe.dev reconciliation found no VM and no terminal
worker result can be recovered. A timeout, SSH disconnect, or unknown worker
state is `unavailable`, not `lost`.

## Creation flow

1. Parse and validate the command without touching external state.
2. Generate `remote_id`, `operation_id`, and `create_id`.
3. Persist the complete local request and state `accepted`.
4. Apply placement: `require_node` verifies the requested node;
   `prefer_existing` selects a compatible healthy node with capacity; and
   `fresh_node` generates `node_id`, `node_attempt_id`, and deterministic VM
   name `ox-<stable-safe-suffix>`.
5. For a new node, read `exe/vms/<name>` through `ExeControlStore`.
6. If absent, write `VmSpec` to `exe/vms`. Include an ox tag, a comment, and
   reserved non-secret worker environment fields containing node and node-
   attempt identity. Users cannot override those fields.
7. If present, treat it as a candidate only; final adoption requires the
   worker `health` record to report this node-attempt identity.
8. Persist the returned SSH fields and `booting`.
9. Connect with RuSSH, verify or enroll the VM host key, open a session channel
   without a PTY, and execute the fixed command `ox-worker structfs-stdio`.
10. Read remote `health` until ready within the boot deadline and verify its
   node and node-attempt identity before adopting the VM.
11. Persist the conversation-to-node assignment and
    `creating_conversation`.
12. Write remote `CreateConversation` to `conversations` using the durable
    `create_id`.
13. Persist the returned existing `t_...` thread ID and conversation path
    before state `running`.
14. Print the stable reference; optionally enter attach mode.

The command does not fork into the background. Once step 13 commits, the
remote execution core owns continued execution and the local process may exit.

The requested TTL is also committed in worker policy. At expiry the worker
stops accepting input, cooperatively cancels active work, and exposes a
`cancelled` result with reason `ttl_expired`. The worker cannot delete
its own exe.dev VM in the MVP; deletion remains an explicit local control-plane
operation.

## Reconciliation rules

Every remote command performs targeted reconciliation before mutation. `list`
does so only with `--refresh`; explicit `reconcile` can process all records.

For each state:

- `accepted` or `provisioning`: query the deterministic VM name and resume
  creation; a same-name VM remains only a candidate until worker identity is
  verified;
- `booting` or `connecting`: reconnect and read `health`;
- `creating_conversation`: replay the same `create_id` and persist the returned
  path;
- active conversation: read status and ledger entries from
  `last_ledger_seq + 1`;
- `cancelling`: replay cancellation and seek a terminal result;
- terminal with pending cleanup: reconcile exact VM identity, then apply
  cleanup policy;
- VM absent before a remote terminal record: mark `lost` only after the exact
  provider query succeeds.

An ambiguous exe.dev timeout is always followed by `ls <exact-name>`. The
client must never issue a second `new` until that read proves the VM absent.

## RuSSH StructFS transport

### Connection

The data-plane client:

1. resolves `ssh_host` and optional `ssh_user` from the authenticated exe.dev
   response rather than constructing a hostname;
2. verifies the host key against `~/.ox/remote/known_hosts`;
3. authenticates with the selected ox/exe.dev SSH identity;
4. opens one session channel without agent forwarding, port forwarding, env
   injection, shell, or PTY;
5. executes exactly `ox-worker structfs-stdio`;
6. treats the channel as an ordered byte stream carrying versioned frames.

The first connection to a newly created host prompts with algorithm and
fingerprint. Noninteractive use refuses it unless `--accept-new-host-key` was
explicitly supplied. Once stored, a changed key is always a conflict and
cannot be bypassed by that flag.

The implementation pins a patched RuSSH version and selects exactly one crypto
backend (`ring` or `aws-lc-rs`) in workspace dependencies. RuSSH's repository
states that one is required; feature-unification must not choose it
accidentally. Versions affected by
[GHSA-m65r-rprj-r5rg](https://github.com/advisories/GHSA-m65r-rprj-r5rg)
are prohibited. Agent forwarding is prohibited.

### Wire version 1

Each frame is:

```text
4-byte unsigned big-endian payload length
CBOR payload
```

The default maximum payload is 16 MiB. Length zero, an oversized length,
truncated CBOR, duplicate map keys, unknown required enum values, and nesting
beyond the configured limit close the channel and produce `protocol_error`.

A request payload contains:

```text
version:       1
request_id:    string
operation:     read | write
path:          array of UTF-8 path components
record:        RecordEnvelope, writes only
deadline_ms:   positive integer, reads only and optional
```

A response contains the same version and request ID plus exactly one of:

```text
read:   absent | RecordEnvelope
write:  returned path component array
error:  { code, message, retryable }
```

`RecordEnvelope` preserves the two StructFS `Record` variants:

```text
parsed: one StructFS Value encoded directly in CBOR
raw:    { bytes, format }
```

Parsed values round-trip all current StructFS variants without JSON
coercion—especially i64, byte strings, and non-finite f64 handling. Raw records
retain exact bytes and their `Format` string.

Multiple requests may be in flight. Responses can arrive out of order and are
correlated by `request_id`; one blocking cursor read cannot stall a message or
cancellation write.

### Retry and idempotency

Transport request IDs remain stable until a response is durably recorded.
After disconnect, reads may be replayed and cursor reads resume from the last
committed sequence.

The transport server caches recent write results by authenticated SSH key
fingerprint and request ID. This cache is an optimization, not the sole
correctness mechanism. Semantic writes also carry durable IDs (`create_id`,
`message_id`, approval ID), so replay remains safe after server restart or key
rotation.

## Worker process model

The pinned custom OCI image contains:

- `ox-worker`, built for the exe.dev VM architecture;
- the same agent Wasm and packaged `ExecutionCore` used by local ox;
- one long-lived `ox-worker serve` process started at boot;
- persistent state and workspace directories on the VM disk.

`ox-worker serve` constructs exactly one `ExecutionCore`. The core retains the
existing `AgentPool` worker and mailbox per active thread, shared compiled
`AgentModule`, `ThreadRegistry`, broker mounts, approval path, and per-thread
`LedgerWriter`. It introduces no per-conversation host process, registry,
scheduler, or log. Wasm instances and sandboxed tool subprocesses remain the
execution-isolation boundaries.

The process exposes a thin public Store on a VM-local Unix socket using the
same frames. `ox-worker structfs-stdio` bridges stdin/stdout to that socket. It
does not own an agent turn, so closing an SSH channel cannot stop a
conversation or another connection.

Worker stderr is diagnostic only and never shares the framed stdout stream.
The bridge exits if framing fails or the local worker service is unavailable.

Provider and repository credentials arrive through explicitly configured
exe.dev integrations or worker-side secret capabilities. They are never placed
in the conversation record, VM comment, CLI arguments, or local ledger cache.

## Execution-core packaging

The packaging of execution-only code from `crates/ox-cli` moves these behaviors
without changing them:

- agent Wasm loading;
- per-thread worker construction;
- prompt-to-history append;
- one-turn execution;
- token accounting;
- config snapshot and ledger durability;
- resume classification and approval wakeup.

CLI-specific policy prompts, TUI event delivery, and rendering remain adapters.
The remote worker supplies its own policy and effect adapters. Existing remount
and crash-harness tests must pass against `ExecutionCore` before worker-only
behavior is added. The local CLI must then consume the same core; retaining its
old private modules would create two executors.

A future full Isotope runtime may replace the implementation behind
`ExecutionCore`. The worker public Store, StructFS transport, placement state,
and CLI contracts remain unchanged. This project does not pre-implement or
fork that future runtime.

## Authentication and configuration

The config schema gains:

```toml
[remote.exe]
control_host = "exe.dev"
worker_image = "registry.example/ox-worker@sha256:..."
identity = "~/.ssh/id_ed25519_exe"
known_hosts = "~/.ox/remote/known_hosts"
connect_timeout = "10s"
operation_timeout = "30s"
boot_timeout = "3m"
max_concurrent_vms = 4
default_ttl = "4h"
cleanup_policy = "retain"

[remote.defaults]
cpu = 2
memory_mib = 4096
disk_gib = 20
integrations = ["llm"]
```

Environment overrides follow the existing Figment convention, for example
`OX_REMOTE__EXE__CONTROL_HOST`. Command flags override resolved config for one
invocation and are not persisted automatically.

The MVP uses SSH for exe.dev control and worker data, so it requires no exe.dev
HTTPS bearer token. An encrypted private key may be unlocked interactively or
through an SSH agent. Ox never copies the private key into `keys.json`.

## Security requirements

- Pin the worker image by digest in non-development config.
- Verify both exe.dev and VM SSH host keys.
- Use an ox-specific SSH key where practical so it can be revoked separately.
- Never enable SSH agent forwarding.
- Execute only the fixed worker command on the VM.
- Fail closed if remote sandbox profile compilation or enforcement fails; the
  remote profile never selects `PermissivePolicy`.
- Expose only audited trusted native capability adapters. Filesystem and shell
  work continues through policy-constrained `ox-tool-exec` subprocesses.
- Validate and encode exe.dev command arguments from typed records.
- Never place prompt text, credentials, or repository secrets in VM names,
  tags, or comments.
- Treat remote text, filenames, Markdown, event bodies, and artifact metadata
  as untrusted terminal input.
- Sanitize control characters and terminal escape sequences before rendering.
- Cap prompt, message, event batch, frame, artifact metadata, and local cache
  sizes.
- Disable repository writeback in the MVP.
- Require an explicit confirmation before VM deletion.
- File permissions for `~/.ox/remote` and its known-hosts file must exclude
  group and world writes; creation uses user-only permissions.

## Failure model

Store errors normalize to these codes:

| Code | Retryable | Meaning |
|---|---:|---|
| `validation_failed` | no | malformed path, record, CLI value, or unsupported option |
| `not_found` | no | unknown local reference, VM, conversation, or artifact |
| `unauthenticated` | no | no usable SSH identity |
| `forbidden` | no | rejected key, command, integration, or worker capability |
| `host_key_unknown` | no | first contact needs explicit trust |
| `host_key_changed` | no | stored and presented SSH host keys differ |
| `unavailable` | yes | DNS, TCP, SSH, exe.dev, or worker temporarily unavailable |
| `timeout` | yes | bounded operation elapsed |
| `conflict` | no | VM identity, idempotency content, cursor hash, or approval conflict |
| `protocol_error` | no | malformed or unsupported StructFS frame |
| `remote_failed` | no | remote agent reached a failed terminal state |
| `local_persistence` | maybe | database or filesystem durability failure |

Raw exe.dev responses may be retained in protected debug logs after redaction.
They are not exposed as stable error records.

## Observability

Each local diagnostic record includes available identifiers from:

```text
remote_id, operation_id, transport_request_id, node_id,
node_attempt_id, vm_name, conversation_path, ledger_seq
```

Required durations and counters include:

- control SSH connect and command latency;
- VM provision and boot latency;
- worker SSH connect and handshake latency;
- Store reads and writes by result class;
- reconnect and replay counts;
- ledger reconciliation lag;
- active, terminal, retained, and leaked VMs;
- cleanup failures.

Metric labels and ordinary logs never include prompts, message content,
repository contents, tokens, or private-key paths.

## Concurrency and resource limits

- Multiple `ox remote new` processes may run concurrently.
- Database uniqueness and compare-by-attempt updates serialize ownership, not a
  process-global lock.
- A short per-remote advisory lease prevents two local processes from actively
  reconciling the same record; lease expiry permits recovery after a crash.
- `list --refresh` and global reconciliation use bounded concurrency.
- One SSH connection may multiplex Store requests for one node. Multiple
  connections and conversations may address that node concurrently.
- Local configuration enforces a maximum active VM count before `new`.
- Provider quota rejection never triggers automatic deletion of unrelated VMs.
- The existing per-thread workers remain independent. A bounded node-wide
  active-turn permit limits CPU/model pressure, while status, approval,
  cancellation, and ledger reads bypass turn admission.
- Prompt queues, concurrent cursor reads, frames, and artifact metadata are
  bounded. Overload is explicit and retryable; it never silently drops input.

## No-daemon lifecycle

The CLI has no resident coordinator in the MVP:

- `new` remains alive through durable remote acceptance;
- the remote execution core continues the conversation afterward;
- `attach` and `logs --follow` remain alive only while following;
- every later command reconciles its target before use;
- an interrupted `new` is resumed by the next targeted command or explicit
  `reconcile`.

This is sufficient for remote/background conversations. A daemon is needed
only for unsolicited local notifications, automatic parent-agent wakeup, or
continuous cleanup and is deferred.

## Shortest path to MVP

### Slice 0: transport proof

- Define golden CBOR fixtures for every frame and StructFS Value variant.
- Implement an in-memory duplex carrier and transport conformance suite.
- Implement a fake public worker Store behind `structfs-stdio`.
- Demonstrate concurrent blocking read and write correlation.

### Slice 1: remote conversation on a pre-provisioned node

- Package the reusable `ExecutionCore` with local parity tests.
- Build the thin worker host, Unix Store listener, and stdio bridge.
- Implement RuSSH host verification, authentication, exec, framing, and
  reconnect.
- Against a manually created VM, create a conversation, detach, reattach, send
  a message, and recover ledger output by cursor.

This is the first end-to-end proof, but not the MVP because provisioning and
local crash recovery are manual.

### Slice 2: automated durable `remote new`

- Add remote config and headless command dispatch.
- Add inbox schema and `inbox/remotes` persistence paths.
- Implement `ExeControlStore` over the exe.dev SSH API.
- Implement deterministic VM naming, identity reconciliation, and worker-image
  provisioning.
- Implement `RemoteManagerStore` and `new`, `list`, `show`, and `reconcile`.
- Prove kill-and-resume at every creation transition.

### Slice 3: operable background conversations

- Add `attach`, `send`, `logs`, and `cancel`.
- Add VM deletion with identity verification.
- Add JSON/JSONL output and stable exit codes.
- Add restart, duplicate, cursor, host-key, cancellation, and cleanup tests.

This is the CLI MVP.

### Deliberate MVP cuts

- `fresh-node` as the default placement policy, with `require_node` available
  and shared-node behavior covered by acceptance tests;
- SSH-only control and data plane;
- fixed wire encoding and worker image;
- explicit repository URL and revision, or an empty workspace;
- no dirty-worktree upload;
- no artifact byte transfer beyond ledger/result retrieval;
- remote policy fails approval-requiring operations closed;
- retain VM by default until explicit deletion;
- no local daemon, desktop notification, or parent-agent wakeup;
- no repository writeback;
- line-oriented attach rather than the full TUI.

### Estimate

For one engineer already familiar with the repository:

| Work | Estimate |
|---|---:|
| Execution-core packaging and local parity | 3–5 days |
| Runtime limits and fail-closed tool policy | 3–5 days |
| StructFS frame codec and conformance harness | 3–5 days |
| Thin durable worker host and public Store | 3–5 days |
| RuSSH control/data adapters and host-key handling | 4–6 days |
| Inbox/ingress persistence, placement, and reconciliation | 5–8 days |
| CLI commands, rendering, JSON, and signals | 3–5 days |
| Failure-injection and end-to-end tests | 4–6 days |

The durable CLI MVP is approximately 6–9 engineer-weeks. The pre-provisioned
vertical slice should be demonstrable after roughly 2–3 engineer-weeks; it is
not a background-conversation MVP until automated recovery and identity-safe
cleanup pass.

## Test requirements

### Unit and fixture tests

- every StructFS Value and both Record variants round-trip through wire v1;
- malformed, oversized, truncated, duplicate-key, and over-nested frames fail;
- exe.dev typed command encoding rejects injection characters and invalid
  identifiers;
- every command has human, JSON, and error-output snapshots;
- prefix resolution rejects ambiguity;
- Ctrl-C behavior never emits cancellation;
- terminal control sequences in remote content are neutralized.

### Store conformance tests

- `RemoteStore` matches local Store read/write/absent/error semantics;
- returned paths remain relative to the exported root;
- blocking reads do not stall independent operations;
- reconnect replays reads and unresolved writes safely;
- worker namespace tests prove internal paths are unreachable;
- semantic IDs remain idempotent after transport cache loss.

### State-machine tests

Kill the local process immediately before and after every durable transition
and external side effect:

- before and after `new`;
- after exe.dev accepted `new` but before the response was persisted;
- after VM persistence but before worker connection;
- after remote conversation acceptance but before local path persistence;
- after message persistence but before acknowledgement;
- during cursor transaction;
- during cancellation;
- after successful `rm` but before cleanup persistence.

Each restart must converge without duplicate node, conversation, message,
ledger entry, or deletion.

### End-to-end tests

- two concurrent `new` commands create independently addressable VMs;
- two conversations created with `require_node` on the same node make progress
  independently when either conversation is waiting for approval or a cursor;
- a remote conversation continues after its creator exits;
- attach resumes from the last committed ledger entry without gaps or
  duplicates;
- changed host keys fail closed;
- ambiguous `new` timeouts reconcile through exact-name `ls`;
- cancellation survives disconnect;
- VM deletion refuses mismatched identity;
- no secret appears in frames, database rows, ledger cache, or logs.

Tests use fake exe.dev and in-process SSH/Store fixtures by default. A gated
live exe.dev smoke test owns tagged disposable VMs and always reports leaked
resources instead of hiding cleanup failure.

## MVP acceptance criteria

- `ox remote new` provisions a VM and returns a durable reference only after
  the remote conversation is accepted.
- The command can exit while the remote agent continues.
- `attach`, `send`, `logs`, `show`, `list`, `cancel`, `reconcile`, and
  `vm delete` operate using that reference.
- Killing `new` at any transition and running `reconcile` converges on at most
  one placement outcome and one worker thread for that create ID.
- Replaying a message creates at most one remote user entry.
- Ledger replay is ordered, gap-free, and duplicate-free locally.
- Ctrl-C during attach detaches and never cancels.
- VM deletion verifies provider and node-attempt identity and refuses to delete
  a node with other live conversations unless explicitly forced.
- The worker exposes only its public StructFS Store.
- CLI, coordinator, and agent code contain no direct RuSSH or exe.dev calls;
  those remain carrier and host-Store adapters.
- Existing local TUI behavior and crash/remount tests remain unchanged after
  execution-core packaging.
- No private key, bearer token, or secret Store snapshot crosses in a
  conversation record or appears in normal output.

## Deferred after the CLI MVP

- approval command and interactive approval UI;
- artifact byte download and resumable transfer;
- warm node pools and automatic placement rebalancing;
- automatic cleanup daemon and desktop notifications;
- agent-initiated remote subtasks and parent wakeup;
- dirty-worktree snapshot transport;
- repository push and pull-request capabilities;
- HTTPS carrier for environments where SSH is unavailable;
- OpenSSH subsystem registration instead of the fixed exec command;
- full TUI integration;
- Android and Flutter clients;
- routing arbitrary Assemblies or Blocks to remote runtimes.

## Requirements for the implementing plan

- Include a Prerequisites verification manifest for every code, schema, Store,
  and file-format seam touched by each task.
- Extract the agent host in a behavior-preserving task and run existing
  remount/crash parity tests before worker changes.
- Commit wire and record fixtures before implementing either peer.
- Introduce database migrations through `ox-inbox`; no second writer may open
  and mutate ox-owned tables behind its Store.
- Name the authoritative writer and idempotency key for every durable record.
- Include fake exe.dev, fake SSH, fake worker, and failure-injection coverage.
- Keep each task independently reviewable and executable by one sub-agent.
- Run `./scripts/fmt.sh --check` and `./scripts/quality_gates.sh` before claiming
  implementation completion.
