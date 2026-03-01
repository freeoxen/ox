#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SITE="$ROOT/site"
UI="$ROOT/crates/ox-web/ui"

# ---------------------------------------------------------------------------
# Mutual exclusion with dev.sh (shared build outputs)
# ---------------------------------------------------------------------------
LOCKFILE="$ROOT/target/.dev-lock"
mkdir -p "$ROOT/target"
if [ -f "$LOCKFILE" ]; then
    other=$(cat "$LOCKFILE")
    lock_pid=$(echo "$other" | grep -o '[0-9]*')
    if [ -n "$lock_pid" ] && kill -0 "$lock_pid" 2>/dev/null; then
        echo "error: $other is already running." >&2
        echo "       These scripts share build outputs and cannot run concurrently." >&2
        exit 1
    fi
    echo "warning: removing stale lock from $other" >&2
fi
echo "site-dev.sh (pid $$)" > "$LOCKFILE"
remove_lock() { rm -f "$LOCKFILE"; }
trap remove_lock EXIT

# ---------------------------------------------------------------------------
# Preflight
# ---------------------------------------------------------------------------

for cmd in bun cargo-watch wasm-pack; do
    if ! command -v "$cmd" &>/dev/null; then
        echo "error: $cmd not found in PATH" >&2
        [[ "$cmd" == "cargo-watch" ]] && echo "       install with: cargo install cargo-watch" >&2
        exit 1
    fi
done

# ---------------------------------------------------------------------------
# Initial builds (wasm → svelte → site)
# ---------------------------------------------------------------------------

echo "==> wasm-pack build (ox-web)"
wasm-pack build "$ROOT/crates/ox-web" --target web --out-dir ../../target/wasm-pkg

echo "==> bun install (ui)"
(cd "$UI" && bun install)

echo "==> svelte build (ui)"
(cd "$UI" && bun run build)

echo "==> bun install (site)"
(cd "$SITE" && bun install)

echo "==> site build"
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
    remove_lock
}
trap cleanup EXIT

# Static file server — port 0 lets the OS pick a free port.
# serve.ts prints the URL to stdout on startup; capture it for reuse.
URLFILE=$(mktemp)
bun run "$SITE/serve.ts" > >(tee "$URLFILE") &
# Wait for the server to print its URL
while [ ! -s "$URLFILE" ]; do sleep 0.05; done
SITE_URL=$(cat "$URLFILE")
rm -f "$URLFILE"

echo ""
echo "  site → $SITE_URL"
echo ""
echo "==> watching for changes (Ctrl-C to stop)"
echo "    [site]  site/{src,styles,index.html}       → site build"
echo "    [brand] BRAND_BOOK.md                      → site build"
echo "    [ui]    crates/ox-web/ui/src                → svelte build → site build"
echo "    [wasm]  crates/ox-web/src                   → wasm-pack → site build"
echo ""

# Site source watcher — site build only
cargo watch -q \
    -w "$SITE/src" \
    -w "$SITE/styles" \
    -w "$SITE/index.html" \
    -w "$ROOT/BRAND_BOOK.md" \
    -s "echo '[site] rebuilding...' && cd '$SITE' && bun run build 2>&1 && echo '[site] rebuilt ok → $SITE_URL' || echo '[site] BUILD FAILED'" &

# UI source watcher — SvelteKit build, then site build
cargo watch -q \
    -w "$UI/src" \
    -s "echo '[ui] rebuilding...' && cd '$UI' && bun run build 2>&1 && echo '[ui] svelte ok' && cd '$SITE' && bun run build 2>&1 && echo '[ui] site ok → $SITE_URL' || echo '[ui] BUILD FAILED'" &

# Wasm source watcher — wasm-pack build, then site build
cargo watch -q \
    -w "$ROOT/crates/ox-web/src" \
    -s "echo '[wasm] rebuilding...' && wasm-pack build '$ROOT/crates/ox-web' --target web --out-dir ../../target/wasm-pkg 2>&1 && echo '[wasm] wasm ok' && cd '$SITE' && bun run build 2>&1 && echo '[wasm] site ok → $SITE_URL' || echo '[wasm] BUILD FAILED'" &

wait
