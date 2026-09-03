# ox Android application: embedded Isotope runtime with Flutter UI

**Date:** 2026-08-31

**Status:** Draft specification, not an implementation plan

**Related:**
[`2026-08-31-exe-dev-remote-subtasks.md`](2026-08-31-exe-dev-remote-subtasks.md)

## Summary

The Android application embeds the complete ox execution core in a native Rust
library. That core owns the Isotope runtime, Assemblies, Blocks, agent turns,
stores, persistence, approvals, and remote-subtask coordination. Flutter is a
UI and input layer mounted only to the runtime's public StructFS store. Every
semantic exchange across the Flutter/Rust boundary is a StructFS read or write;
there is no parallel command/event API.

“The agent runs locally” means the orchestration and capability decisions run
on the phone. Model inference may still use an HTTP provider unless the user
configures an on-device model service. Linux shell, git, builds, and other
heavyweight coding capabilities normally run inside a delegated remote ox VM.

```text
┌──────────────────── Flutter / Dart ─────────────────────┐
│ screens, navigation, text editing, accessibility, theme │
└───────────────────────┬─────────────────────────────────┘
                        │ framed StructFS read / write
┌───────────────────────▼─────────────────────────────────┐
│ ox-mobile native library                                │
│                                                        │
│  Isotope runtime                                       │
│  ├── root ox Assembly                                  │
│  ├── agent supervisor + agent Blocks                   │
│  ├── nested gateway/completion Assembly                │
│  ├── inbox, ledgers, approvals, policy                 │
│  └── remote subtask coordinator                        │
│                                                        │
│  Android host capabilities                             │
│  ├── app-private storage / SQLite                      │
│  ├── HTTP                                               │
│  ├── Keystore-backed secrets                           │
│  ├── notifications / background scheduling            │
│  └── time, random, connectivity, content URIs          │
└─────────────────────────────────────────────────────────┘
```

## Goals

- Ship one Android application whose execution core is the same Rust/Isotope
  system used by desktop and remote ox.
- Make Flutter replaceable: no agent or persistence semantics live in Dart.
- Preserve threads and remote tasks across process death and device reboot.
- Run ordinary agent reasoning locally while routing Linux-specific work to
  remote subtasks.
- Present streaming turns, child tasks, approvals, errors, and notifications
  through stable projections.
- Intermediate Flutter/Rust, local/remote, and Android-background/runtime
  communication through StructFS stores.
- Keep every Block capability explicit in its Assembly namespace.
- Reuse and generalize `ox-gateway` rather than create a second runtime.

## Non-goals

- Embedding a Linux distribution or unrestricted shell in the Android app.
- Keeping the app process alive indefinitely without an Android-visible reason.
- Giving Dart access to internal Block stores or the substrate root. Dart sees
  exactly one public application store.
- Reimplementing the kernel, gateway, inbox, or policy engine in Dart/Kotlin.
- Running all model inference on-device in the first release.
- Complete Isotope deployment management such as canary and rolling Assembly
  activation in the mobile MVP.
- iOS support in this specification. The native boundary should not preclude it.

## Prerequisites verification manifest

- [x] **A1. ox-gateway already contains the runtime nucleus to generalize.**
  Verified with
  `nl -ba crates/ox-gateway/src/assembly.rs | sed -n '1,227p'`,
  `nl -ba crates/ox-gateway/src/broker_block.rs | sed -n '21,185p'`, and
  `nl -ba crates/ox-gateway/gateway.assembly.yaml`.
  The manifest is load-bearing, wiring is longest-prefix, and Block reads can
  park on the async broker. The current gateway-specific construction begins at
  `crates/ox-gateway/src/main.rs:24`.

- [x] **A2. The current Wasm executor has a narrow synchronous StructFS ABI.**
  Verified with
  `rg -n "store_read|store_write|store_result|pub fn run" crates/ox-runtime/src/engine.rs crates/ox-wasm/src/lib.rs`.
  `AgentModule::run` is at `crates/ox-runtime/src/engine.rs:95`; guest imports
  are declared at `crates/ox-wasm/src/lib.rs:16`.

- [x] **A3. Agent supervision is currently owned by the CLI binary.**
  Verified with
  `nl -ba crates/ox-cli/src/agents.rs | sed -n '100,215p'` and
  `nl -ba crates/ox-cli/src/agents.rs | sed -n '637,735p'`.
  `AgentPool` owns workers at `crates/ox-cli/src/agents.rs:101`, and the prompt
  loop and `run_one_turn` live at lines 637 and 697. Mobile therefore needs an
  extraction of this logic, not FFI calls into the CLI.

- [x] **A4. Thread durability already separates config snapshots from the
  per-append ledger writer.**
  Verified with
  `nl -ba crates/ox-inbox/src/snapshot.rs | sed -n '1,94p'` and
  `nl -ba crates/ox-kernel/src/log.rs | sed -n '209,284p'`.
  `snapshot.rs:1` states the ownership split; `SharedLog::append` commits
  through its `Durability` implementation before publication at
  `crates/ox-kernel/src/log.rs:266`.

- [x] **A5. Current inbox tasks need a remote-execution extension.**
  Verified with
  `nl -ba crates/ox-inbox/src/schema.rs | sed -n '6,42p'`.
  Parent threads already exist at line 9; task rows at lines 27–34 do not carry
  executor, VM, cursor, lease, or error information.

- [x] **A6. Existing filesystem and shell tools are process-backed and cannot
  be treated as Android host capabilities.**
  Verified with
  `rg -n "std::process::Command|external executor" crates/ox-tools/src`.
  `crates/ox-tools/src/sandbox.rs:68` constructs an external command, while
  `crates/ox-tools/src/fs.rs:13` and `os.rs:11` describe their external
  executor boundary.

- [ ] **A7. The current Wasmtime configuration is acceptable on supported
  Android devices.**
  `rustup target list --installed` contains no Android target, and no Android
  native library exists in this repository. This must be resolved by a
  measured ARM64 spike before committing to the engine. Failure blocks mobile
  runtime packaging for approximately 1–2 weeks while an interpreter or AOT
  backend is introduced; it does not change the FFI or Assembly architecture.

- [x] **A8. The existing async Store boundary already carries StructFS Paths,
  Records, and errors.**
  Verified with
  `nl -ba crates/ox-broker/src/async_store.rs | sed -n '1,24p'` and
  `nl -ba crates/ox-gateway/src/wire_store.rs | sed -n '19,30p'`.
  The repository-local traits accept `structfs_core_store` types at
  `crates/ox-broker/src/async_store.rs:5` and lines 12–19; the mobile public
  store and remote transport can target this current seam without inventing a
  second domain API.

## Repository shape

The intended layering is:

```text
crates/ox-runtime/          low-level Wasm guest executor and host ABI
crates/ox-isotope/          generalized Assembly/runtime coordinator
crates/ox-agent-host/       reusable agent/thread supervision
crates/ox-structfs-transport/ framed Store transport and carrier adapters
crates/ox-remote/           remote subtask stores and coordinator
crates/ox-mobile/           C ABI, Android lifecycle facade, root Assembly
crates/ox-gateway/          gateway Assembly and HTTP edge consumer
crates/ox-worker/           headless Linux remote worker consumer
apps/ox-mobile/             Flutter application
```

Names are recommendations; the dependency directions are requirements.
`ox-isotope`, `ox-agent-host`, and `ox-remote` cannot depend on Flutter or the
CLI. `ox-mobile` cannot depend on TUI crates.

## Generalizing the gateway runtime

The extraction begins behavior-preservingly:

| Current gateway code | General runtime responsibility |
|---|---|
| `assembly::Manifest` | `AssemblyManifest` parser and validator |
| `assembly::WiringTable` | immutable per-Block namespace router |
| `broker_block::BlockBacking` | `BlockNamespace` over bound host/import stores |
| `codec_block::module()` | artifact registry and compiled-module cache |
| `run_broker/run_wire/run_stats` | generic Block instance runner |
| runner closures on handle stores | scheduler spawn/cancel interface |
| manual bindings in `main.rs` | parent Assembly import bindings |

Gateway-specific routes, wire records, telemetry shapes, completion semantics,
and its Wasm artifact remain in `ox-gateway`.

After the extraction, `ox-isotope` adds the runtime behavior the gateway did
not need:

- Block identity and generation;
- created, starting, running, stopping, stopped, and failed states;
- lazy startup and bounded instance pools;
- cancellation, graceful shutdown, timeout escalation, and restart limits;
- runtime-provided `/iso/self`, `/iso/server`, and `/iso/shutdown` stores;
- request queueing and `respond_to` correlation for Blocks that serve stores;
- fuel, memory, queue, and concurrency limits;
- runtime event publication and persisted Assembly identity.

The mobile MVP does not require `/iso/assemblies` rollout strategies,
cross-machine transparent routing, or general deadlock detection. These remain
later conformance work.

## Root mobile Assembly

The embedded application loads a versioned, immutable root Assembly. Its
conceptual shape is:

```yaml
assembly: ox-mobile
version: 1
imports:
  storage: Android app-private durable storage
  secret: Android Keystore-backed secret handles
  http-out: Android/reqwest HTTP transport
  remote: RuSSH or HTTPS RemoteStore carrier
  platform: lifecycle, connectivity, notifications, content URIs
  sys: time and random
blocks:
  shell: embedded:ox-mobile-shell-wasm
  agent-supervisor: embedded:ox-agent-supervisor-wasm
  subtask-coordinator: embedded:ox-subtask-coordinator-wasm
assemblies:
  gateway: embedded:ox-gateway-assembly
public: shell
```

The exact Block split can evolve. The load-bearing rules are:

- Flutter communicates only with the public application surface.
- That surface is a StructFS store; UI intent, projections, subscriptions, and
  reconciliation use only its path/read/write contract.
- Agent Blocks cannot access Android services unless wired to an appropriate
  capability store.
- Secrets and raw network sockets remain host edges.
- Remote provisioning is visible to agent logic as the `subtasks` store, not
  as exe.dev HTTP calls.
- Remote worker access is a mounted `RemoteStore`; RuSSH and HTTPS are carrier
  bindings below that store boundary.
- The nested gateway/completion service uses the generalized version of the
  existing gateway Assembly and host HTTP edge.

Android services required by Rust but implemented in Kotlin—Keystore,
notifications, WorkManager scheduling, and content URI access—are exposed as
manifest-bound host Stores. Their JNI bridge carries the same generic
request/response frames plus a bootstrap-assigned import ID. It does not expose
per-feature native methods, and an import ID cannot address any Store that was
not wired into the root Assembly.

## Native boundary

### Packaging

`ox-mobile` is built as a Rust `cdylib` and packaged in a Flutter FFI plugin.
Initial targets are ARM64 devices and an x86_64 emulator build. Flutter's
supported native-code mechanisms are documented at
[Bind to native code using FFI](https://docs.flutter.dev/platform-integration/bind-native-code)
and
[Android C interop](https://docs.flutter.dev/platform-integration/android/c-interop).

The Android Wasm-engine spike must measure:

- build and link success for ARM64;
- build and link success for the x86_64 emulator target;
- application and split-APK size;
- cold engine initialization and module compilation time;
- resident memory with one, two, and eight Block instances;
- repeated blocking read/suspend/resume behavior;
- executable-memory behavior on representative Android versions;
- app background/foreground and process-recreation behavior.

The engine is hidden behind an `EngineBackend` boundary so Wasmtime can be
replaced without changing Assembly, store, or Flutter APIs.

### C ABI

The ABI is handle-based, versioned, and panic-contained. It is a carrier for
the same `StructFsRequestFrame` and `StructFsResponseFrame` defined by
`ox-structfs-transport`, bound to the root Assembly's one public store. A
minimal surface is:

```c
uint32_t ox_mobile_abi_version(void);

OxStatus ox_mobile_create(
    const uint8_t *config_json,
    size_t config_len,
    OxRuntimeHandle *out_runtime);

OxStatus ox_mobile_start(OxRuntimeHandle runtime);

OxStatus ox_mobile_submit(
    OxRuntimeHandle runtime,
    const uint8_t *request_frame,
    size_t request_len);

OxStatus ox_mobile_next_response(
    OxRuntimeHandle runtime,
    uint32_t timeout_ms,
    OxOwnedBuffer *out_response_frame);

OxStatus ox_mobile_stop(OxRuntimeHandle runtime, uint32_t timeout_ms);
void ox_mobile_buffer_free(OxOwnedBuffer buffer);
void ox_mobile_destroy(OxRuntimeHandle runtime);
```

The first ABI uses the transport's fixed versioned encoding, preferably CBOR,
with its JSON codec available for diagnostics and golden fixtures. A request
contains `request_id`, operation, relative path, optional write Record, and an
optional bounded read deadline. A response contains the correlated read
Record, returned write path, or store error. The FFI does not define domain
commands or event variants.

`config_json` is bootstrap-only: storage location, imported host-Store IDs,
artifact digest, and runtime limits. It cannot create threads, mutate settings,
or carry application intent. After `start`, all semantic traffic uses framed
Store operations.

All exported functions catch Rust panics and return an error status. Dart never
owns Rust pointers beyond an `OxOwnedBuffer`, and every successful buffer must
be released exactly once.

`ox_mobile_submit` only enqueues a Store operation. It cannot run an agent turn
or network request on the Flutter UI isolate. A dedicated Dart isolate may
block in `ox_mobile_next_response`; it correlates responses and forwards
decoded Records to the UI isolate.

## Public application store

The public store is the entire Flutter contract. Representative paths are:

```text
read  health
read  inbox
read  changes/from/<seq>

write threads <CreateThread> -> threads/<thread-id>
read  threads/<thread-id>
read  threads/<thread-id>/events/from/<seq>
write threads/<thread-id>/messages <UserMessage>
    -> threads/<thread-id>/messages/<message-id>

read  approvals
read  approvals/<approval-id>
write approvals/<approval-id> <ApprovalResponse>

write subtasks/spawn <RemoteTaskSpec>
    -> subtasks/outstanding/<task-id>
read  subtasks/outstanding/<task-id>
read  subtasks/outstanding/<task-id>/events/from/<seq>
write subtasks/outstanding/<task-id>/cancel {}

read  settings
write settings <SettingsPatch>
read  runtime
write runtime/reconcile {}
```

Writes return stable handles. Flutter may read a returned handle immediately
for acceptance state, and later read it again for progress. Mutation request
IDs are durably deduplicated so resubmitting an FFI frame cannot repeat the
operation.

Cursor paths are the only streaming contract. Event sequence is monotonic for
one owning object. After process restart Flutter first reads the relevant
projection and resumes from its committed cursor; it never relies on an
ephemeral native event stream.

`changes/from/<seq>` returns lightweight projection revision changes, fatal
runtime state, and notification-worthy object references. It does not duplicate
thread logs or remote event bodies; Flutter follows the referenced projection
or object path when it needs the new data.

## Projection records

Flutter renders projection Records owned by Rust and returned from public-store
reads:

- `InboxProjection` — thread summaries, child counts, remote task summaries;
- `ThreadProjection` — ordered visible log entries, turn state, token/cost data;
- `TaskProjection` — VM state, event cursor, elapsed time, cleanup state;
- `ApprovalProjection` — pending approval with allowed responses;
- `SettingsProjection` — provider/account metadata with secret presence only;
- `RuntimeProjection` — version, health, active Blocks, recovery status.

Dart does not reconstruct them by interpreting raw ledgers. Pagination and
incremental invalidation are explicit. A projection includes a revision; a
delta whose base revision does not match causes a full refresh.

## Threading model

- Flutter UI work remains on the UI isolate.
- A Dart bridge isolate owns blocking Store reads and frame decoding.
- Rust owns one Tokio runtime and a bounded blocking pool.
- Each Wasm Block remains single-threaded for an invocation.
- Blocking StructFS reads park only the relevant Block execution, following the
  existing gateway backing pattern.
- SQLite and ledger writes are never performed on the UI isolate.
- Runtime shutdown stops intake, persists state, requests graceful Block
  shutdown, and then applies a bounded forced-stop timeout.

## Android host capabilities

### Storage

- Inbox database, ledgers, context snapshots, remote event replicas, and Wasm
  artifacts live in application-private storage.
- SQLite uses WAL where the existing inbox schema does.
- Ledger append durability remains in Rust; Flutter never writes ledger files.
- User-selected files enter through Android content URIs and are copied or
  exposed through a narrowly scoped host store.
- No broad storage permission is required for the MVP.

### Secrets

- Provider and exe.dev tokens are represented inside the runtime by opaque
  handles.
- Long-lived material is encrypted by an Android Keystore-backed key.
- Projection DTOs expose configured/missing state, never secret values.
- Secrets are excluded from thread snapshots and remote task envelopes.
- Authentication failure can invalidate a handle and ask Flutter to present a
  reauthentication surface.

The Keystore implementation may be a small Kotlin/JNI service under the native
plugin. It implements the manifest-bound `secret` Store through the generic
host-Store bridge; it is not Dart application state or a separate secrets API.

### Network transports

HTTP provider calls and exe.dev control calls use separate clients and
credential scopes. The runtime receives both as stores returning handles with
bounded reads, cancellation, and response-size limits.

Remote worker calls use a `RemoteStore` carrying generic StructFS frames. The
desktop-first implementation uses a RuSSH client to execute
`ox-worker structfs-stdio` on the VM. The Android feasibility spike must test
RuSSH crypto packaging, host-key persistence, reconnect behavior, and signing
with Android-protected key material. If that carrier is unsuitable on Android,
the app uses the companion spec's HTTPS carrier without changing any Block,
coordinator, or Flutter path contract.

### Local tools

The app does not expose the current process-based `fs` and `os` executors.
Mobile-safe initial tools are:

- read/write within an explicit app workspace;
- HTTP fetch subject to policy;
- selected document import/export through content URIs;
- remote delegation and task inspection;
- completion/model access;
- time and user-visible notifications.

Shell, git, compiler, package-manager, and arbitrary process tools are remote
capabilities in the first release.

## Android lifecycle and background behavior

The runtime cannot assume its process remains alive.

### Foreground

While the app is visible, the embedded runtime stays active and Flutter keeps
bounded cursor reads in flight. Local turns and remote reconciliation run
normally.

### App backgrounded with no active local turn

Persist current state, release UI subscriptions, and allow the process to be
suspended. Remote workers continue independently. WorkManager schedules
bounded reconciliation when network and system policy permit.

### App backgrounded during a local turn

If the user explicitly requests continued local work, promote it to an
appropriate foreground service with a persistent notification and cancellation
action. Otherwise checkpoint at a safe boundary and resume later.

Android recommends WorkManager for reliable work that survives app exits and
reboots, including long-running workers that run through a foreground service.
See
[Android persistent work](https://developer.android.com/develop/background-work/background-tasks/persistent)
and
[long-running workers](https://developer.android.com/develop/background-work/background-tasks/persistent/how-to/long-running).

### Process death or reboot

On startup or a WorkManager reconciliation run:

1. Open and reconcile the inbox database.
2. Verify/recover ledger tails using current durability rules.
3. Restore Assembly definitions and host bindings.
4. Mark interrupted local executions appropriately.
5. Reconcile every nonterminal remote task from its stored VM identity and
   remote event sequence.
6. Post a notification for newly observed approval, completion, or failure.

The Kotlin Worker may load `ox-mobile` without starting Flutter, submit a
StructFS write to `runtime/reconcile`, and read its returned handle through the
same C ABI. It must not invoke a domain-specific native entry point or maintain
a second implementation of remote task state transitions.

## Flutter product surface

### Required screens

1. **Onboarding** — provider account, model, exe.dev token, remote capability
   test.
2. **Inbox** — local and remote threads, state, unread/attention indication,
   child task count.
3. **Thread** — streaming conversation, tool calls, approvals, child task cards,
   prompt composer.
4. **Remote task detail** — provisioning/run state, VM identity, ordered events,
   elapsed time, cancel/retain/delete controls, final result.
5. **Approvals** — blocking request detail and allow/deny/edit response.
6. **Settings** — accounts, defaults, remote resource policy, concurrency and
   cleanup policy, diagnostics export.

### UI rules

- A write displays pending state when its returned handle reports acceptance,
  not only after completion.
- Remote disconnection displays “reconnecting” rather than “failed.”
- A task failure and a cleanup failure are shown independently.
- Opening a notification navigates to the owning thread/task/approval.
- Every destructive action names the VM or task and requires confirmation when
  work or artifacts would be lost.
- Flutter retains only ephemeral view state such as scroll offset and draft
  text. Authoritative domain state comes from Rust projections.

## Remote-task integration

The app consumes the public stores and framed transport defined by the companion
remote spec.
Important mobile-specific rules:

- exe.dev never opens a reverse connection to the phone; the phone reads by
  cursor;
- VM work continues through app suspension and network changes;
- opening the app performs immediate reconciliation;
- periodic background reconciliation is opportunistic, not a real-time SLA;
- live UI may keep bounded StructFS reads in flight, but cursor-based replay is
  canonical;
- an OS notification is emitted when reconciliation discovers a new terminal
  result or approval request;
- remote approvals can remain pending indefinitely without keeping the phone
  process alive.

## Security and privacy

- Use pinned SSH host keys and an ox-specific SSH identity when RuSSH is the
  carrier; otherwise default to private exe.dev proxies and VM-scoped HTTPS
  tokens.
- Require explicit user action before enabling remote delegation.
- Show the repository, prompt/context scope, integrations, resources, and
  cleanup policy before the first remote task.
- Apply concurrency and spend limits locally before provisioning.
- Redact tokens and provider authorization headers from logs and diagnostics.
- Verify Wasm and worker artifacts by digest before execution.
- Give each Block only manifest-wired capabilities.
- Treat rendered Markdown, remote event content, and downloaded artifacts as
  untrusted input.
- Do not allow remote repository writeback in the MVP.
- Provide “delete remote VM” and “forget local replica” as separate actions.

## Failure and recovery UX

Failures carry a stable code, retryability, scope, and human explanation.
Required distinctions:

- local runtime failed to start;
- local turn interrupted by Android;
- device offline;
- exe.dev control unavailable or rate-limited;
- VM provisioning failed;
- worker booting or unreachable;
- remote agent failed;
- remote task succeeded but VM cleanup failed;
- credential expired or revoked;
- local projection stale and being rebuilt.

Retryable failures retain task identity and resume reconciliation. Retrying must
not silently create a new task or VM attempt. A user-requested new attempt gets
a new `attempt_id` and an explicit UI record.

## Testing

### Rust

- Assembly validation and namespace isolation tests extracted from gateway.
- C ABI ownership, malformed StructFS frame, panic containment, stop/start, and
  version compatibility tests.
- Projection golden tests from ledger and task fixtures.
- process-death simulations at every durable transition.
- fake Android host stores for secret, storage, HTTP, and lifecycle behavior.
- remote Store, frame, and reconciliation fixtures shared with `ox-worker`.

### Flutter

- widget and accessibility tests for all required projections and states;
- bridge tests with a fake public StructFS store and out-of-order responses;
- navigation from notification payloads;
- rotation, background/foreground, and process-recreation integration tests;
- large thread and high-frequency streaming performance tests.

### Android device matrix

- current low-, middle-, and high-memory ARM64 devices;
- at least one supported older Android release and the current release;
- no-network, metered-network, Doze, battery-saver, background restriction, and
  reboot scenarios;
- install/upgrade with existing inbox and remote task state.

## Delivery slices

### Slice 0: feasibility gates

- Generalize enough of gateway's Block runner to execute a trivial Assembly in
  a library test.
- Build `ox-mobile.so` for ARM64 and run one trivial Wasm Block through FFI.
- Measure engine size, startup, memory, and suspend/resume behavior.
- Prove a Rust-owned SQLite database survives app process recreation.

No broad Flutter UI implementation should precede this gate.

### Slice 1: runtime shell

- Start the root runtime from Flutter.
- Render runtime health and an inbox projection.
- Create a thread and persist it.
- Exercise write handles, cursor reads, projection refresh, shutdown, and
  restart through the public Store.

### Slice 2: remote-first product MVP

- Configure an exe.dev token.
- Start, observe, cancel, and clean up a remote task using the already-proven
  desktop remote Store and carrier frames.
- Display remote child threads and terminal results.
- Reconcile through app background/process death and post notifications.

### Slice 3: local agent turns

- Run the extracted agent host inside the mobile Assembly.
- Configure provider accounts through Keystore-backed secrets.
- Stream local turns and expose only mobile-safe tools.
- Delegate shell/build work to remote tasks.

### Slice 4: approvals and hardening

- Local and remote approval UX.
- foreground-service continuation for explicit local work;
- artifact download/export;
- performance, accessibility, upgrade, policy, and release hardening.

## Mobile MVP acceptance criteria

- Flutter launches a Rust-owned Isotope runtime and renders its health without
  containing runtime-specific state transitions in Dart.
- Every Flutter/native semantic operation is a read or write on the root
  Assembly's single public StructFS store.
- A user can create and reopen a durable local thread.
- A user can start two remote tasks and immediately leave the app.
- After process death, reopening the app restores both tasks and resumes each
  event cursor without duplicate events.
- Terminal remote results appear in the parent thread and task detail screen.
- A user can cancel a running task and separately delete or retain its VM.
- No provider or exe.dev secret appears in a projection, ledger, or remote task
  envelope.
- The app contains no process-backed local shell/fs executor.
- Unwired Block paths fail at the runtime namespace boundary.
- Existing ox-gateway parity tests remain green against the generalized runtime.

## Deferred after mobile MVP

- arbitrary local Android plugins as Isotope stores;
- location-transparent routing of arbitrary individual Blocks;
- warm remote worker pools;
- repository writeback and PR creation;
- on-device model packaging;
- iOS and desktop Flutter shells;
- full Assembly deployment/version-management UI;
- multi-device synchronization.

## Acceptance criteria for an implementing plan

- The plan includes a Prerequisites verification manifest for every store,
  persistence, FFI, and cross-crate seam it changes.
- The gateway extraction is a separate, behavior-preserving task with gateway
  parity tests before mobile-specific changes.
- The Android engine spike is an explicit go/no-go task before the UI depends on
  a particular engine.
- Rust and Flutter tasks communicate only through StructFS frame and
  public-store fixtures committed before either side is implemented.
- Process death, replay, and reconnection are tested as normal paths.
- The implementation runs `./scripts/fmt.sh` and
  `./scripts/quality_gates.sh` before completion.
