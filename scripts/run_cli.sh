#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# ---------------------------------------------------------------------------
# Build all artifacts the CLI needs, then run it.
#
# 1. Packaged Wasm artifacts consumed by the host build scripts
# 2. ox-tool-exec                 (sandboxed tool executor, sibling binary)
# 3. ox (the CLI itself)
# ---------------------------------------------------------------------------

echo "==> building packaged Wasm artifacts"
"$ROOT/scripts/build-wasm-artifacts.sh"

echo "==> building ox-tool-exec"
cargo build --locked -p ox-tools --bin ox-tool-exec

echo "==> building + running ox-cli"
exec cargo run --locked -p ox-cli --bin ox -- "$@"
