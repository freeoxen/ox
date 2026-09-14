#!/usr/bin/env bash
# Generate ignored registry-package Wasm with the pinned toolchain.
# --check verifies existing generated outputs without updating them.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
CHECK=false
case "${1:-}" in
    '') ;;
    --check) CHECK=true; shift ;;
    *) echo "usage: $0 [--check]" >&2; exit 2 ;;
esac
[ "$#" -eq 0 ] || { echo "unexpected arguments" >&2; exit 2; }
STAGING="$ROOT/local/wasm-artifacts"
mkdir -p "$STAGING"

# Scrub both llvm-cov instrumentation channels before running the guest build.
COVERAGE=false
while IFS= read -r key; do
    case "$key" in *LLVM_COV*) COVERAGE=true ;; esac
done < <(compgen -e)
while IFS= read -r key; do
    case "$key" in *LLVM_COV*|*RUSTFLAGS|LLVM_PROFILE_FILE) unset "$key" ;; esac
done < <(compgen -e)
if "$COVERAGE"; then unset RUSTC_WRAPPER RUSTC_WORKSPACE_WRAPPER; fi
TASK_CARGO_HOME="$(cd "${CARGO_HOME:-$HOME/.cargo}" && pwd)"
export CARGO_ENCODED_RUSTFLAGS="--remap-path-prefix=$ROOT=/ox"$'\x1f'"--remap-path-prefix=$TASK_CARGO_HOME=/cargo"
export CARGO_TARGET_DIR="$STAGING/target"
RUSTC_VERSION="$(rustc --version)"
cargo metadata --no-deps --format-version 1 --locked > "$STAGING/metadata.json"
if ! cargo build --release --locked --target wasm32-unknown-unknown \
    -p ox-wasm -p ox-gateway-wasm > "$STAGING/build.log" 2>&1; then
    echo "Wasm build failed; see $STAGING/build.log" >&2
    exit 1
fi
sha256() { shasum -a 256 "$1" | awk '{print $1}'; }
STALE=false
for guest in ox-wasm ox-gateway-wasm; do
    case "$guest" in
        ox-wasm) package=ox-executor; filename=agent.wasm ;;
        ox-gateway-wasm) package=ox-gateway; filename=codec_block.wasm ;;
    esac
    # Metadata includes dependency target conditions. Conservatively include all
    # normal/build path edges rather than excluding another target's inputs.
    jq -r --arg guest "$guest" '
        .packages as $packages |
        def closure($p): $p,
            ($p.dependencies[] | select(.path != null and (.kind == null or .kind == "build")) |
             .path as $path | $packages[] |
             select(.manifest_path == ($path + "/Cargo.toml")) | closure(.));
        [$packages[] | select(.name == $guest) | closure(.) | .manifest_path] |
        unique[]
    ' "$STAGING/metadata.json" > "$STAGING/$guest-manifests.txt"
    {
        printf '%s\n' Cargo.toml Cargo.lock scripts/build-wasm-artifacts.sh
        for config in rust-toolchain.toml .cargo/config.toml; do
            if [ -f "$config" ]; then printf '%s\n' "$config"; fi
        done
        while IFS= read -r manifest; do
            relative="${manifest#"$ROOT/"}"
            directory="${relative%/Cargo.toml}"
            printf '%s\n' "$relative"
            if [ -d "$directory/src" ]; then find "$directory/src" -type f; fi
            if [ -f "$directory/build.rs" ]; then printf '%s\n' "$directory/build.rs"; fi
        done < "$STAGING/$guest-manifests.txt"
    } | LC_ALL=C sort -u > "$STAGING/$guest-inputs.txt"
    while IFS= read -r input; do
        jq -cn --arg path "$input" --arg hash "$(sha256 "$input")" '{key:$path,value:$hash}'
    done < "$STAGING/$guest-inputs.txt" |
        jq -csS 'from_entries' > "$STAGING/$guest-inputs.json"
    built="$CARGO_TARGET_DIR/wasm32-unknown-unknown/release/${guest//-/_}.wasm"
    artifact_hash="$(sha256 "$built")"
    jq -nS --arg guest "$guest" --arg rustc "$RUSTC_VERSION" \
        --arg artifact_hash "$artifact_hash" \
        --arg source_hash "$(sha256 "$STAGING/$guest-inputs.json")" \
        --slurpfile inputs "$STAGING/$guest-inputs.json" '
        {schema_version:1, guest_package:$guest, target:"wasm32-unknown-unknown",
         profile:"release", rustc:$rustc, artifact_sha256:$artifact_hash,
         source_sha256:$source_hash, inputs:$inputs[0]}
    ' > "$STAGING/$filename.provenance.json"
    destination="$ROOT/crates/$package/artifacts"
    if "$CHECK"; then
        if ! cmp -s "$built" "$destination/$filename" ||
            ! cmp -s "$STAGING/$filename.provenance.json" "$destination/$filename.provenance.json"; then
            echo "Stale packaged Wasm: crates/$package/artifacts/$filename" >&2
            STALE=true
        fi
    else
        mkdir -p "$destination"
        cp "$built" "$destination/$filename"
        cp "$STAGING/$filename.provenance.json" "$destination/$filename.provenance.json"
    fi
    echo "$guest: SHA256 $artifact_hash"
done
if "$STALE"; then
    echo "Run scripts/build-wasm-artifacts.sh to regenerate artifacts." >&2
    exit 1
fi
if "$CHECK"; then echo "Packaged Wasm artifacts verified."; else echo "Packaged Wasm artifacts updated."; fi
