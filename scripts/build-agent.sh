#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(dirname "$SCRIPT_DIR")"

cd "$ROOT"

"$SCRIPT_DIR/build-wasm-artifacts.sh"
mkdir -p "$ROOT/target"
cp "$ROOT/crates/ox-executor/artifacts/agent.wasm" "$ROOT/target/agent.wasm"
echo "Built: target/agent.wasm ($(du -h "$ROOT/target/agent.wasm" | cut -f1))"
