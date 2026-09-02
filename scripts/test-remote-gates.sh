#!/bin/sh
set -eu

# Deterministic, credential-free remote execution gates. Live provider and
# long-running soak gates are intentionally separate scripts.
./scripts/test-remote-script-contracts.sh
cargo test -p ox-worker
cargo test -p ox-executor --test worker_ingress
cargo test -p ox-structfs-transport --test conformance
cargo test -p ox-structfs-transport --test carriers
cargo test -p ox-remote --test exe_control
cargo test -p ox-remote --test manager_reconcile
cargo test -p ox-tools --test sandbox_limits

if [ "$(uname -s)" != Linux ]; then
  echo "PARTIAL: Linux-only Landlock/seccomp cases still require a Linux worker image" >&2
fi
