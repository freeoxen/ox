# Remote execution runbook

**Status:** pre-production. The credential-free gates are reproducible; the
live exe.dev smoke, worker-image scan, Linux sandbox gate, and 24-hour soak are
release blockers until their reports exist.

## Ownership and invariants

The remote worker is a carrier around the existing executor, not a second
runtime. One `ox-worker serve` process owns one `ExecutionCore`; that core owns
the existing per-thread workers, namespaces, approval state, ledger writers,
and snapshots. Coordinator-to-worker traffic is limited to the public StructFS
surface. SSH and the Unix socket carry frames but do not define semantics.

Operational invariants:

1. Never run two worker services against the same inbox root.
2. Never copy or edit a live `ledger.jsonl`; stop the owning worker first.
3. Never adopt or delete a VM unless worker health echoes the expected node and
   node-attempt IDs.
4. Never start production service mode without a digest-pinned
   `OX_WORKER_IMAGE_DIGEST` and a passing sandbox preflight.
5. Treat an unknown or changed SSH host key as an incident. `--accept-new-host-key`
   enrolls an unknown key only; it does not authorize a changed key.
6. A transport timeout is ambiguous. Reconcile the durable semantic ID; do not
   issue a new ID to “retry” the same action.

## Release gates

Run the deterministic suite from the repository root:

```sh
./scripts/test-remote-gates.sh
./scripts/fmt.sh --check
./scripts/quality_gates.sh
```

Build and scan the exact release image with pinned bases. Push builds require
both `syft` and `trivy` and write provenance, SBOM, vulnerability, image-ID, and
registry-digest artifacts under `target/ox-worker-image/`:

```sh
OX_WORKER_BUILD_IMAGE='rust@sha256:…' \
OX_WORKER_RUNTIME_IMAGE='debian@sha256:…' \
OX_WORKER_IMAGE='registry.example/ox-worker:candidate' \
OX_WORKER_PUSH=1 OX_WORKER_REQUIRE_SCANNERS=1 \
./scripts/build-worker-image.sh
```

On the resulting Linux image, run `scripts/test-remote-gates.sh`. The Linux-only
Landlock/seccomp escape suite must pass inside the image; a macOS pass is not a
substitute.

The live test is deliberately mutation-gated. Run it only from a dedicated
runner account whose `~/.ox` is disposable and whose remote config and SSH key
are scoped to test nodes:

```sh
OX_REMOTE_LIVE=1 \
OX_REMOTE__EXE__WORKER_IMAGE='registry.example/ox-worker@sha256:…' \
OX_REMOTE_CONFIG=/secure/test-runner/remote.toml \
./scripts/test-remote-live.sh
```

The script uses stable request IDs and a trap to delete its node. Cleanup
failure is printed as a leaked-node error and must be resolved manually; it is
never hidden behind a successful test result.

The rollout soak requires an already-created disposable node and defaults to
24 hours and 50 conversations:

```sh
OX_REMOTE_SOAK=1 OX_REMOTE_SOAK_NODE='n_…' \
OX_REMOTE_SOAK_SECONDS=86400 OX_REMOTE_SOAK_THREADS=50 \
./scripts/soak-remote-worker.sh
```

It records node health and reconciliation results, downloads every final
ledger, and fails on duplicate sequence/hash, a broken parent chain, duplicate
initial user input, or cross-thread canary exposure. The script does not delete
the supplied node; inspect the report first, then delete it explicitly.

## Provision and inspect

Use JSON for automation and retain the returned durable IDs:

```sh
ox remote --json node new --request-id node-<stable-id>
ox remote --json node doctor <node-id>
ox remote --json conversation new --node <node-id> \
  --request-id conversation-<stable-id> --prompt '…'
ox remote --json conversation show <conversation-id>
ox remote conversation logs <conversation-id> --jsonl
```

`fresh-node` is the default placement. Use `--node` to deliberately share a
verified node; use `--placement prefer-existing` only when scheduler choice is
acceptable.

## Failure response

### Worker unreachable or SSH lost

1. Do not resend with a new semantic ID.
2. Run `ox remote --json conversation reconcile <conversation-id>`.
3. Run `ox remote --json node doctor <node-id>`.
4. If provider identity is intact but worker health is unavailable, inspect the
   node service and disk. Restarting the worker replays accepted, unapplied
   ingress against durable ledger evidence.
5. If the node is truly lost, preserve the local coordinator database and mark
   the incident lost; do not claim completion from cached state.

### Ambiguous provider create/delete

The provider adapter performs an exact-name lookup after an ambiguous result.
Repeat the original CLI request with the same request/delete IDs. Never create
a second VM name or force-delete a VM whose node-attempt identity cannot be
verified.

### Overload

`overloaded` is an admission result, not a transport failure. Inspect
`capacity`; wait or place new work on another node. Status, health, approval,
cancel, and bounded ledger reads are deliberately outside turn admission.
Repeated overload with stable/declining capacity is an incident; do not raise
limits before checking resident threads, cursor admission, and provider
latency.

### Approval parked

A parked approval consumes an active-turn permit by design, but must not block
public control. Read the exact pending approval ID, then approve or deny it
once. A stale approval conflict means the pending durable evidence changed;
re-read rather than overriding it.

### Disk full, DB busy, or degraded ledger

Stop accepting new work and drain the node. Preserve the inbox root. A failed
ledger commit must not become visible; a degraded or unrecoverable ledger is
not writable. Free capacity without deleting thread directories, restart one
worker owner, run doctor/reconcile, and verify the ledger chain before
returning the node to placement.

### Host-key change

Quarantine the node/provider endpoint. Compare the expected fingerprint using
an independent control channel. Remove or rotate the known-host entry only as
an explicit incident action after validating node-attempt identity. Never use
accept-new as a changed-key bypass.

### Sandbox refusal, Wasm trap/fuel, or tool timeout

Sandbox preflight refusal prevents readiness and is a release/image failure.
A per-turn Wasm limit or tool timeout should terminate that turn and release
its permit without taking down unrelated conversations. Preserve the ledger,
worker health record, image digest, and structured error; reproduce with the
same pinned image before changing limits.

## Drain and delete

```sh
ox remote --json node drain <node-id>
ox remote --json node show <node-id>
ox remote --json node delete <node-id> --yes \
  --request-id delete-request-<stable-id> --delete-id delete-<stable-id>
```

Normal deletion refuses active conversations. `--force` is for a reviewed
incident or disposable test node; it does not relax identity checks. After any
cleanup failure, list provider VMs and local node records and report the exact
leaked names/IDs.

## Evidence retention

For a release or incident retain:

- commit and worker image digest;
- deterministic-gate output and Linux sandbox output;
- SBOM, vulnerability report, provenance, and registry digest;
- live-test/soak report and leaked-node cleanup result;
- node/attempt IDs, semantic request IDs, and sanitized doctor output;
- affected ledger chain and coordinator DB copy, handled as sensitive data.

Do not put credentials, full prompts, SSH private-key paths, or raw provider
responses into issue titles, process arguments, or shared logs.
