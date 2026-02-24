#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

WATCH=true
PORT=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --no-watch) WATCH=false; shift ;;
        --port)
            if [[ -z "${2:-}" ]]; then
                echo "error: --port requires a value" >&2
                exit 1
            fi
            PORT="$2"; shift 2 ;;
        *)
            echo "usage: $0 [--no-watch] [--port PORT]" >&2
            exit 1
            ;;
    esac
done

SERVER_ARGS=()
if [[ -n "$PORT" ]]; then
    SERVER_ARGS+=(-- --port "$PORT")
fi

# ---------------------------------------------------------------------------
# Preflight checks
# ---------------------------------------------------------------------------

if [ -z "${ANTHROPIC_API_KEY:-}" ]; then
    echo "error: ANTHROPIC_API_KEY is not set" >&2
    echo "usage: ANTHROPIC_API_KEY=sk-ant-... $0" >&2
    exit 1
fi

required_cmds=(cargo wasm-pack bun)
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
# Initial builds
# ---------------------------------------------------------------------------

echo "==> wasm-pack build (ox-web)"
wasm-pack build crates/ox-web --target web --out-dir ../../target/wasm-pkg

echo "==> bun build (UI)"
(cd crates/ox-web/ui && bun run build)

# ---------------------------------------------------------------------------
# Run
# ---------------------------------------------------------------------------

if "$WATCH"; then
    # On any exit (Ctrl-C, error, normal), kill all background jobs and their
    # process trees, then wait for them to finish.
    cleanup() {
        local pids
        pids=$(jobs -p 2>/dev/null) || true
        if [[ -n "$pids" ]]; then
            # SIGTERM the direct children (cargo-watch instances).
            # cargo-watch/watchexec forwards signals to its spawned commands.
            kill $pids 2>/dev/null || true
            # Give them a moment, then force-kill stragglers.
            sleep 0.3
            kill -9 $pids 2>/dev/null || true
        fi
        wait 2>/dev/null || true
    }
    trap cleanup EXIT

    echo ""
    echo "==> watching for changes (Ctrl-C to stop)"
    echo "    [ui]     crates/ox-web/ui/{src,styles,fonts} → bun build"
    echo "    [wasm]   crates/ox-web/src/                  → wasm-pack"
    echo "    [server] Rust crates                         → cargo run"
    echo ""

    # UI watcher — quiet mode, tagged output
    cargo watch -q \
        -w crates/ox-web/ui/src \
        -w crates/ox-web/ui/styles \
        -w crates/ox-web/ui/fonts \
        -s 'echo "[ui] rebuilding..." && cd crates/ox-web/ui && bun run build 2>&1 && echo "[ui] rebuilt ok" || echo "[ui] BUILD FAILED"' &

    # Wasm watcher — quiet mode, tagged output
    cargo watch -q \
        -w crates/ox-web/src \
        -s 'echo "[wasm] rebuilding..." && wasm-pack build crates/ox-web --target web --out-dir ../../target/wasm-pkg 2>&1 && echo "[wasm] rebuilt ok" || echo "[wasm] BUILD FAILED"' &

    # Server watcher — shows full cargo/server output (the primary stream)
    cargo watch \
        -w crates/ox-dev-server \
        -w crates/ox-core \
        -w crates/ox-kernel \
        -w crates/ox-context \
        -w crates/ox-history \
        -x "run -p ox-dev-server ${SERVER_ARGS[*]:-}" &

    wait
else
    echo ""
    echo "==> starting ox-dev-server"
    echo "    the server will print its URL when ready"
    echo ""

    exec cargo run -p ox-dev-server "${SERVER_ARGS[@]}"
fi
