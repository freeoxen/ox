#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

WATCH=true
for arg in "$@"; do
    case "$arg" in
        --no-watch) WATCH=false ;;
        *)
            echo "usage: $0 [--no-watch]" >&2
            exit 1
            ;;
    esac
done

# ---------------------------------------------------------------------------
# Preflight checks
# ---------------------------------------------------------------------------

if [ -z "${ANTHROPIC_API_KEY:-}" ]; then
    echo "error: ANTHROPIC_API_KEY is not set" >&2
    echo "usage: ANTHROPIC_API_KEY=sk-ant-... $0" >&2
    exit 1
fi

required_cmds=(cargo wasm-pack)
if "$WATCH"; then
    required_cmds+=(cargo-watch)
fi
for cmd in "${required_cmds[@]}"; do
    if ! command -v "$cmd" &>/dev/null; then
        echo "error: $cmd not found in PATH" >&2
        if [ "$cmd" = "cargo-watch" ]; then
            echo "       install with: cargo install cargo-watch" >&2
            echo "       or run with --no-watch to skip auto-reload" >&2
        fi
        exit 1
    fi
done

# ---------------------------------------------------------------------------
# Build wasm (must happen before the server starts serving /pkg/)
# ---------------------------------------------------------------------------

echo "==> wasm-pack build (ox-web)"
wasm-pack build crates/ox-web --target web --out-dir ../../target/wasm-pkg

# ---------------------------------------------------------------------------
# Run
# ---------------------------------------------------------------------------

if "$WATCH"; then
    echo ""
    echo "==> starting ox-dev-server on http://localhost:3000 (watching for changes)"
    echo "    the server will rebuild and restart when source files change"
    echo "    watching: crates/ox-dev-server/, crates/ox-core/, crates/ox-kernel/,"
    echo "              crates/ox-context/, crates/ox-history/"
    echo ""
    echo "    note: changes to crates/ox-web/ require a manual wasm-pack rebuild"
    echo "          (the watcher does not re-run wasm-pack automatically)"
    echo ""
    echo "    open http://localhost:3000 and try: \"reverse the word hello\""
    echo "    press Ctrl-C to stop"
    echo ""

    exec cargo watch \
        -w crates/ox-dev-server \
        -w crates/ox-core \
        -w crates/ox-kernel \
        -w crates/ox-context \
        -w crates/ox-history \
        -x "run -p ox-dev-server"
else
    echo ""
    echo "==> starting ox-dev-server on http://localhost:3000"
    echo "    open that URL in a browser and try: \"reverse the word hello\""
    echo ""

    exec cargo run -p ox-dev-server
fi
