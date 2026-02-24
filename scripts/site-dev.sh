#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SITE="$ROOT/site"

# ---------------------------------------------------------------------------
# Preflight
# ---------------------------------------------------------------------------

for cmd in bun cargo-watch; do
    if ! command -v "$cmd" &>/dev/null; then
        echo "error: $cmd not found in PATH" >&2
        [[ "$cmd" == "cargo-watch" ]] && echo "       install with: cargo install cargo-watch" >&2
        exit 1
    fi
done

# ---------------------------------------------------------------------------
# Initial build
# ---------------------------------------------------------------------------

echo "==> bun install (site)"
(cd "$SITE" && bun install)

echo "==> initial build"
(cd "$SITE" && bun run build)

# ---------------------------------------------------------------------------
# Serve + watch
# ---------------------------------------------------------------------------

cleanup() {
    local pids
    pids=$(jobs -p 2>/dev/null) || true
    if [[ -n "$pids" ]]; then
        kill $pids 2>/dev/null || true
        sleep 0.3
        kill -9 $pids 2>/dev/null || true
    fi
    wait 2>/dev/null || true
}
trap cleanup EXIT

# Static file server — port 0 lets the OS pick a free port.
# serve.ts prints the URL to stdout on startup.
bun run "$SITE/serve.ts" &

echo ""
echo "==> watching for changes (Ctrl-C to stop)"
echo "    [site] site/{src,styles,index.html}        → bun run build"
echo "    [ui]   crates/ox-web/ui/{src,styles}       → bun run build"
echo "    [brand] BRAND_BOOK.md                      → bun run build"
echo ""

# Watcher — rebuilds on any source change
cargo watch -q \
    -w "$SITE/src" \
    -w "$SITE/styles" \
    -w "$SITE/index.html" \
    -w "$ROOT/BRAND_BOOK.md" \
    -w "$ROOT/crates/ox-web/ui/src" \
    -w "$ROOT/crates/ox-web/ui/styles" \
    -s "echo '[site] rebuilding...' && cd '$SITE' && bun run build 2>&1 && echo '[site] rebuilt ok' || echo '[site] BUILD FAILED'" &

wait
