# Remote execution threat model

**Scope:** the `ox remote` coordinator, exe.dev control adapter, RuSSH
carriers, `ox-worker`, its public StructFS Store, the shared `ExecutionCore`,
Wasm turns, sandboxed tools, and durable local/worker state.

**Security status:** architecture and deterministic regressions are present.
Production approval remains blocked on a scanned digest-pinned image, Linux
sandbox execution, live provider smoke, and the 24-hour shared-node soak.

## Assets

- provider and model credentials;
- SSH private keys and known-host databases;
- user prompts, assistant output, tool inputs/results, approvals, and ledgers;
- coordinator operation records and worker ingress receipts;
- conversation workspaces and artifacts;
- node/attempt identity and release image identity;
- availability of control, cancellation, and unrelated conversations.

The worker inbox database contains ingress payloads for multiple conversations
on that node and is therefore sensitive even though each conversation has its
own ledger and workspace.

## Trust boundaries

```text
operator/CLI
  -> coordinator InboxStore (durable intent and placement)
  -> exe.dev SSH control (provider mutation)
  -> worker SSH carrier (host identity)
  -> StructFS wire decoder and public export root
  -> PublicStore (allowlisted operations and bounded admission)
  -> shared ExecutionCore
       -> per-thread namespace, worker, ledger, approval, workspace
       -> Wasm Store/instance per turn
       -> fail-closed sandboxed tool subprocess
```

SSH and StructFS frames authenticate/carry requests; neither grants access to
arbitrary broker paths. The public export root is the worker authority
boundary. Per-thread workers are concurrency boundaries, not tenant-grade host
security boundaries. Stronger tenant isolation requires node placement, not a
parallel executor implementation.

## Threats and controls

| Threat | Required control | Current deterministic evidence |
|---|---|---|
| Duplicate VM, conversation, prompt, decision, or cancel after timeout/crash | Durable semantic IDs, canonical request hashes, exact replay/reconciliation | `ox-remote/tests/{exe_control,manager_reconcile}.rs`, `ox-executor/tests/worker_ingress.rs` |
| Wrong VM adoption or deletion | Exact provider name plus node/attempt identity echoed by worker health | `ox-remote/tests/exe_control.rs` |
| Unknown/changed SSH host | Persistent known-hosts, explicit enrollment for unknown only, changed key fail-closed | RuSSH host-key unit coverage; live rotation drill still pending |
| Wire confusion, oversized frame, or path escape | Versioned bounded codec, request correlation, export root, returned-path confinement | `ox-structfs-transport/tests/{conformance,carriers}.rs` |
| Head-of-line blocking or unbounded requests | Async Store serving plus bounded in-flight/send/cursor/control admission | carrier overload tests and `ox-worker/tests/public_store.rs` |
| Cross-conversation state exposure | Existing per-thread namespace/ledger/approval/workspace; public path allowlist | `ox-worker/tests/semantic_parity.rs`; long soak pending |
| Remote path reaches config, secrets, tools, or arbitrary broker paths | PublicStore exact route match and empty-root export of only that Store | `ox-worker/tests/public_store.rs` and transport export-root tests |
| Tool escapes workspace or reads worker environment/network | Remote-enforced Clash launcher, cleared environment, Landlock/seccomp/Seatbelt, timeout and process-group cancellation | platform sandbox tests; Linux release-image run pending |
| Wasm consumes unbounded memory/CPU or cancellation affects another turn | Per-Store memory/fuel/epoch limits and per-thread cancellation token | executor unit tests; image/load chaos pending |
| Approval replay authorizes a later request | Derived approval identity revalidated immediately before durable decision ingress | worker public-store approval test |
| Secret appears in health/capacity/listing output | Typed projections and no raw environment/provider response | semantic parity canary test; release artifact/process audit pending |
| DB/disk failure creates false visible history | Commit-before-visibility; missing/degraded ledger fails non-writable | kernel/inbox durability tests; fault-injected disk-full run pending |
| Worker/client disconnect cancels or duplicates admitted mutation | Server detaches carrier; semantic retry owned above transport | carrier disconnect tests |
| Resource exhaustion starves cancel/status/approval | Turn admission is separate from public control | approval saturation and simultaneous four-role worker tests |

## Abuse cases

### Malicious prompt or model output

Prompt text and model output are untrusted. They may request tools, embed escape
sequences, or attempt instruction injection. They must never select arbitrary
StructFS paths or provider commands. CLI display sanitizes control characters;
JSON output remains structured data and consumers must encode it for their
terminal/UI.

### Malicious or compromised worker node

A compromised node can read all conversations hosted on that node and forge
its local results. SSH and health identity protect against accidental/wrong
node adoption, not a malicious correctly keyed host. Minimize node lifetime,
scope provider credentials away from worker environments, use tenant-separated
placement where required, and treat worker image/host integrity as part of the
trusted computing base.

### Malicious coordinator state

The local coordinator DB controls placement and semantic IDs. Protect it like
conversation data. Canonical hashes detect same-ID payload substitution during
normal operation; they do not protect against an attacker who can rewrite the
DB and all local evidence.

### Carrier replay or loss

Reads may be retried. Writes are never blindly retried by the byte carrier.
After an ambiguous result, the caller reconciles the original durable semantic
ID. Generating a new ID for the same user intent defeats this control.

## Secrets policy

- SSH keys must be regular user-only files and are read by the SSH adapter, not
  sent through StructFS.
- Worker tool subprocesses receive an allowlisted environment only.
- Provider/model credentials must not be inherited by remote shell tools.
- Health/capacity/capability/list records contain typed operational fields, not
  prompts, raw environment, private-key data, or raw provider responses.
- Ledgers, coordinator DBs, ingress tables, snapshots, and soak reports may
  contain user data and must be encrypted/retained/accessed accordingly.
- Stable IDs and hashes are correlation identifiers, not secrets.

The canary regression checks public records and per-thread ledgers. It does not
claim that a node-wide database contains no prompts; that database is an
authoritative durability owner and is expected to contain ingress payloads.

## Residual risk and release blockers

The following cannot be established by credential-free tests and remain
explicit rollout gates:

- actual exe.dev identity, provisioning, reconnect, and cleanup behavior;
- changed-host-key drill against real endpoints;
- SBOM/vulnerability/provenance results for the final digest;
- Linux Landlock/seccomp escape tests inside that final image;
- disk-full/DB-busy behavior under the deployed filesystem;
- 24-hour, 50-conversation contention, memory/thread bounds, and leakage audit;
- operator review of exe.dev data retention, host hardening, and incident paths.

Any failure must preserve its report and leaked-node identity. A release must
not convert a missing external gate into a pass.
