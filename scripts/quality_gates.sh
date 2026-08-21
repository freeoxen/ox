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

    # Print the gate name immediately so the operator sees what's
    # currently running. The result (PASS/FAIL + elapsed) is appended
    # to the same line on completion.
    printf "  %-40s " "$name"

    local tmpfile
    tmpfile="$(mktemp)"

    local start
    start="$(date +%s)"

    if "$@" >"$tmpfile" 2>&1; then
        local elapsed=$(( $(date +%s) - start ))
        PASSED=$((PASSED + 1))
        printf "PASS  (%ds)\n" "$elapsed"
    else
        local elapsed=$(( $(date +%s) - start ))
        FAILED=$((FAILED + 1))
        printf "FAIL  (%ds)\n" "$elapsed"
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

# 2. Format (prettier — UI)
if "$FIX"; then
    gate "prettier (ui)"          bash -c "cd crates/ox-web/ui && \"$BUN\" x prettier --write 'src/**/*.{ts,js,svelte}'"
else
    gate "prettier --check (ui)"  bash -c "cd crates/ox-web/ui && \"$BUN\" x prettier --check 'src/**/*.{ts,js,svelte}'"
fi

# 2b. Format (prettier — site)
if "$FIX"; then
    gate "prettier (site)"        "$BUN" x prettier --write 'site/**/*.{ts,js,css,html}'
else
    gate "prettier --check (site)" "$BUN" x prettier --check 'site/**/*.{ts,js,css,html}'
fi

# 3. Lint (native)
gate "clippy (native)"            cargo clippy --workspace -- -D warnings

# 3b. Lint: no silent parse fallback.
#
# Flag `<parse|extract|from_value|from_str>(...).unwrap_or(_default|_else)(...)`
# patterns that silently substitute defaults for parse failures — the
# class of bug `legacy_account_endpoint_is_migrated_to_provider` taught
# us about. Each existing site must justify itself with a one-line
# `// allow(silent_parse_fallback): <reason>` comment immediately before
# (or on the same line as) the call. New violations fail the gate.
#
# Greps only — single-line patterns. Multi-line shapes (a chained
# `.unwrap_or_default()` on its own line) are not caught; see
# docs/superpowers/specs/2026-05-22-silent-unwrap-audit.md for the
# audit pass.
gate "no silent parse fallback"   bash -c '
    set -e
    violations=$(grep -rnE "(extract\(|from_value\(|from_str\(|\.parse\().*\.unwrap_or(_default|_else)?\(" crates/ --include="*.rs" \
        | grep -vE "tests/|::tests::|allow\(silent_parse_fallback" \
        || true)
    if [ -n "$violations" ]; then
        echo "Silent parse-fallback violations. Add a // allow(silent_parse_fallback): <reason>"
        echo "comment justifying each site, or fix by surfacing the error:"
        echo ""
        echo "$violations"
        exit 1
    fi
'

# 4. Lint (wasm, ox-web)
#
# Clippy subsumes `cargo check` for both targets — a full type-check
# with lints on top — so there are no separate check gates.
gate "clippy (wasm)"              cargo clippy --target wasm32-unknown-unknown -p ox-web -- -D warnings

# 5. Tests + coverage (Rust + TypeScript, thresholds from coverage.toml).
#
# This is the canonical Rust test run: cargo-llvm-cov executes the whole
# workspace suite (instrumented) and coverage.sh fails on any test
# failure, so a separate `cargo test --workspace` pass would run every
# test a second time for no additional signal. Doctests are the one
# thing llvm-cov skips, and the workspace has none that run (all fences
# are `text` or `ignore`).
gate "test + coverage"            "$ROOT/scripts/coverage.sh" --gate

# 6. wasm-pack build
gate "wasm-pack build"            wasm-pack build crates/ox-web --target web --out-dir ../../target/wasm-pkg

# 7. Install UI dependencies
gate "bun install (ui)"           "$BUN" install --cwd crates/ox-web/ui

# 8. SvelteKit sync + check
gate "svelte-kit sync (ui)"       bash -c "cd crates/ox-web/ui && \"$BUN\" run node_modules/@sveltejs/kit/svelte-kit.js sync"
gate "svelte-check (ui)"          bash -c "cd crates/ox-web/ui && \"$BUN\" run check"

# 9. TypeScript tests (ui)
gate "bun test (ui)"              bash -c "cd crates/ox-web/ui && \"$BUN\" test"

# 10. SvelteKit build
gate "vite build (ui)"            bash -c "cd crates/ox-web/ui && \"$BUN\" run build"

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
