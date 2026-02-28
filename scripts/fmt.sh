#!/usr/bin/env bash
#
# Format all code in the ox workspace.
#
# Usage:
#   ./scripts/fmt.sh           # format everything
#   ./scripts/fmt.sh --check   # check only, exit non-zero if unformatted
#
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

CHECK=false
for arg in "$@"; do
    case "$arg" in
        --check) CHECK=true ;;
        *)
            echo "usage: $0 [--check]" >&2
            exit 1
            ;;
    esac
done

BUN="$(command -v bun 2>/dev/null || echo "${HOME}/.bun/bin/bun")"

if "$CHECK"; then
    cargo fmt --all -- --check
    "$BUN" x prettier --check 'crates/ox-web/ui/src/**/*.{ts,js}' 'site/**/*.{ts,js,css,html}'
else
    cargo fmt --all
    "$BUN" x prettier --write 'crates/ox-web/ui/src/**/*.{ts,js}' 'site/**/*.{ts,js,css,html}'
fi
