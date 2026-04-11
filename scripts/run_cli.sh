#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# ---------------------------------------------------------------------------
# Build all artifacts the CLI needs, then run it.
#
# 1. ox-wasm → target/agent.wasm  (embedded in the CLI via include_bytes!)
# 2. ox-tool-exec                 (sandboxed tool executor, sibling binary)
# 3. ox (the CLI itself)
# ---------------------------------------------------------------------------

echo "==> building agent.wasm"
cargo build --target wasm32-unknown-unknown --release -p ox-wasm
cp target/wasm32-unknown-unknown/release/ox_wasm.wasm target/agent.wasm

echo "==> building ox-tool-exec"
cargo build -p ox-tools --bin ox-tool-exec

echo "==> building + running ox-cli"
exec cargo run -p ox-cli -- "$@"
