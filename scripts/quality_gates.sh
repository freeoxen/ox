#!/usr/bin/env bash
#
# Quality gates for the ox workspace.
# Run this before accepting changes. Every gate must pass.
#
# Usage:
#   ./scripts/quality_gates.sh          # run all gates
#   ./scripts/quality_gates.sh --fix    # auto-fix what can be fixed (fmt)
#
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

FIX=false
for arg in "$@"; do
    case "$arg" in
        --fix) FIX=true ;;
        *)
            echo "usage: $0 [--fix]" >&2
            exit 1
            ;;
    esac
done

FAILED=0
PASSED=0
TOTAL=0
FAILURES=""

gate() {
    TOTAL=$((TOTAL + 1))
    local name="$1"
    shift

    local tmpfile
    tmpfile="$(mktemp)"

    local start
    start="$(date +%s)"

    if "$@" >"$tmpfile" 2>&1; then
        local elapsed=$(( $(date +%s) - start ))
        PASSED=$((PASSED + 1))
        printf "  PASS  %-40s (%ds)\n" "$name" "$elapsed"
    else
        local elapsed=$(( $(date +%s) - start ))
        FAILED=$((FAILED + 1))
        printf "  FAIL  %-40s (%ds)\n" "$name" "$elapsed"
        FAILURES="${FAILURES}\n--- $name ---\n$(cat "$tmpfile")\n"
    fi

    rm -f "$tmpfile"
}

# Resolve bun binary
BUN="$(command -v bun 2>/dev/null || echo "${HOME}/.bun/bin/bun")"

echo "running quality gates..."
echo ""

# 1. Format (Rust)
if "$FIX"; then
    gate "fmt"                    cargo fmt --all
else
    gate "fmt --check"            cargo fmt --all -- --check
fi

# 2. Format (prettier)
if "$FIX"; then
    gate "prettier"               "$BUN" x prettier --write 'crates/ox-web/ui/src/**/*.{ts,js}' 'site/**/*.{ts,js,css,html}'
else
    gate "prettier --check"       "$BUN" x prettier --check 'crates/ox-web/ui/src/**/*.{ts,js}' 'site/**/*.{ts,js,css,html}'
fi

# 3. Lint (native)
gate "clippy (native)"            cargo clippy --workspace -- -D warnings

# 4. Lint (wasm, ox-web)
gate "clippy (wasm)"              cargo clippy --target wasm32-unknown-unknown -p ox-web -- -D warnings

# 5. Check (native)
gate "check (native)"            cargo check --workspace

# 6. Check (wasm)
gate "check (wasm)"              cargo check --target wasm32-unknown-unknown -p ox-web

# 7. Tests
gate "test"                       cargo test --workspace

# 8. wasm-pack build
gate "wasm-pack build"            wasm-pack build crates/ox-web --target web --out-dir ../../target/wasm-pkg

# 9. Install UI dependencies
gate "bun install (ui)"           "$BUN" install --cwd crates/ox-web/ui

# 10. TypeScript type check
gate "tsc check (ui)"             bash -c "cd crates/ox-web/ui && \"$BUN\" x tsc --noEmit"

# 11. Bundle UI
gate "bun build (ui)"             "$BUN" build crates/ox-web/ui/src/main.ts --outdir target/js-pkg --format esm --sourcemap=external --external '/pkg/*'

# 12. Copy CSS
gate "copy css (ui)"              cp crates/ox-web/ui/styles/main.css target/js-pkg/main.css

# Summary
echo ""
if [ "$FAILED" -ne 0 ]; then
    echo "$PASSED/$TOTAL passed, $FAILED failed"
    echo ""
    echo "=== failure details ==="
    printf "%b" "$FAILURES"
    exit 1
else
    echo "$PASSED/$TOTAL passed"
fi
