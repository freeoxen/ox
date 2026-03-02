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
    (cd crates/ox-web/ui && NO_COLOR=1 "$BUN" x prettier --check 'src/**/*.{ts,js,svelte}')
    NO_COLOR=1 "$BUN" x prettier --check 'site/**/*.{ts,js,css,html}'
else
    cargo fmt --all
    (cd crates/ox-web/ui && "$BUN" x prettier --write --log-level warn 'src/**/*.{ts,js,svelte}')
    "$BUN" x prettier --write --log-level warn 'site/**/*.{ts,js,css,html}'
fi
