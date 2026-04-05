#!/usr/bin/env bash
set -euo pipefail
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(dirname "$SCRIPT_DIR")"

echo "Building ox-wasm agent module..."
cargo build --target wasm32-unknown-unknown --release -p ox-wasm
cp "$ROOT/target/wasm32-unknown-unknown/release/ox_wasm.wasm" "$ROOT/target/agent.wasm"
echo "Built: target/agent.wasm ($(du -h "$ROOT/target/agent.wasm" | cut -f1))"
