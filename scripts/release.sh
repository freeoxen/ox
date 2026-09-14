#!/usr/bin/env bash
# Assemble and independently verify registry archives; upload only with --execute.
# Requires Bash 3.2+, jq, curl, shasum, tar and the pinned Rust toolchain.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
STAGE="$ROOT/local/release"
mkdir -p "$STAGE/scratch"
export TMPDIR="$STAGE/scratch"
fail() { echo "$*" >&2; exit 1; }
usage() { echo "Usage: $0 {order|registry|package|check|publish} [--offline] [--allow-dirty] [--execute]"; }
command_name="${1:-}"
case "$command_name" in
    order|registry|package|check|publish) shift ;;
    --help|-h) usage; exit 0 ;;
    *) usage >&2; exit 1 ;;
esac
offline=false
allow_dirty=false
execute=false
for arg in "$@"; do
    case "$arg" in
        --offline) offline=true ;;
        --allow-dirty) allow_dirty=true ;;
        --execute) execute=true ;;
        *) fail "Unknown option: $arg" ;;
    esac
done
for tool in cargo rustc jq curl shasum tar; do
    command -v "$tool" >/dev/null || fail "Required command unavailable: $tool"
done
if [[ "$command_name" == publish ]] && ! "$execute"; then
    fail "No upload performed. Review docs/releasing.md; publish requires --execute."
fi
run_log() {
    local log="$1"
    shift
    printf '+ %s\n' "$*"
    "$@" >"$STAGE/$log" 2>&1 || fail "Command failed; see $STAGE/$log"
}
digest() { shasum -a 256 "$1" | cut -d ' ' -f 1; }

# Cargo parses manifests. jq operates on Cargo's metadata, never on TOML text.
cargo metadata --no-deps --format-version 1 --locked --offline >"$STAGE/metadata.json"
jq -er --slurpfile config "$ROOT/release.json" '
    .packages as $packages | $config[0] as $config |
    ($packages | map({key: .name, value: .}) | from_entries) as $by_name |
    def visit($name):
        if (.visited | index($name)) != null then .
        elif (.visiting | index($name)) != null then error("Release dependency cycle: " + $name)
        else
            $by_name[$name] as $p |
            if $p.version != $config.version then error("Version mismatch: " + $name) else . end |
            .visiting += [$name] |
            reduce ($p.dependencies[] | select(.kind != "dev" and .path != null)) as $dep (.;
                if ($config.packages | index($dep.name)) == null or $dep.req == "*"
                then error("Unpublishable dependency: " + $name + " -> " + $dep.name)
                else visit($dep.name) end) |
            .visiting -= [$name] | .visited += [$name] | .order += [$name]
        end;
    if ($config.packages | unique | length) != ($config.packages | length)
       or ($config.install - $config.packages | length) != 0
    then error("Invalid release allowlist") else . end |
    if ($packages | map(select(.publish != []) | .name) | sort) != ($config.packages | sort)
    then error("release.json and package publish flags differ") else . end |
    reduce $config.packages[] as $name ({visited: [], visiting: [], order: []}; visit($name)) |
    .order[]
' "$STAGE/metadata.json" >"$STAGE/order.txt"
version="$(jq -er '.version' release.json)"
ordered=()
while IFS= read -r name; do ordered+=("$name"); done <"$STAGE/order.txt"

registry_check() {
    local name prefix code status
    : >"$STAGE/registry-check.jsonl"
    for name in "${ordered[@]}"; do
        case "${#name}" in
            1) prefix=1 ;;
            2) prefix=2 ;;
            3) prefix="3/${name:0:1}" ;;
            *) prefix="${name:0:2}/${name:2:2}" ;;
        esac
        code="$(curl --silent --show-error --connect-timeout 15 --max-time 60 \
            --output "$STAGE/registry-$name.json" --write-out '%{http_code}' \
            "https://index.crates.io/$prefix/$name")" || fail "Registry check failed: $name"
        case "$code" in
            404) status='not found' ;;
            200) status='exists; verify ownership and unused version' ;;
            *) fail "Registry check inconclusive for $name: HTTP $code" ;;
        esac
        echo "$name: $status"
        jq -cn --arg name "$name" --arg status "$status" '{key: $name, value: $status}' >>"$STAGE/registry-check.jsonl"
    done
    jq -s 'from_entries' "$STAGE/registry-check.jsonl" >"$STAGE/registry-check.json"
}
case "$command_name" in
    order) cat "$STAGE/order.txt"; exit 0 ;;
    registry) registry_check; exit 0 ;;
esac
head="$(git rev-parse HEAD)"
dirty=false
if [[ -n "$(git status --porcelain)" ]]; then
    dirty=true
    "$allow_dirty" || fail "Commit release inputs first (or use --allow-dirty for development checks)."
fi
receipt="$STAGE/verified.json"
if [[ "$command_name" == publish ]]; then
    "$dirty" && fail "Cannot publish a dirty checkout."
    jq -e --arg head "$head" --arg version "$version" \
        '.head == $head and .dirty == false and .version == $version' "$receipt" >/dev/null \
        || fail "Run check on this clean commit before publication."
    # Ignored generated package inputs must still match the verified source.
    run_log wasm.log bash "$ROOT/scripts/build-wasm-artifacts.sh" --check
    for name in "${ordered[@]}"; do
        archive="$name-$version.crate"
        expected="$(jq -er --arg archive "$archive" '.archives[$archive]' "$receipt")"
        [[ "$(digest "$STAGE/archives/$archive")" == "$expected" ]] || fail "Verified archive changed: $archive"
    done
    registry_check
    for name in "${ordered[@]}"; do cargo publish -p "$name" --locked; done
    exit 0
fi
# Invalidate old evidence even if the following check fails.
if [[ -f "$receipt" ]]; then mv -f "$receipt" "$STAGE/previous-verified.json"; fi
run_log wasm.log bash "$ROOT/scripts/build-wasm-artifacts.sh"
# Cargo 1.96 batch verification fails for unpublished dependencies ("no hash
# listed"). Every actual archive is independently built below instead.
package_args=(package --locked --no-verify --target-dir "$ROOT/target")
if "$offline"; then package_args+=(--offline); fi
if "$allow_dirty"; then package_args+=(--allow-dirty); fi
for name in "${ordered[@]}"; do package_args+=(-p "$name"); done
run_log package.log cargo "${package_args[@]}"
if [[ "$command_name" == package ]]; then
    echo 'Archives assembled; installation has not been verified.'
    exit 0
fi
vendor="$STAGE/vendor"
vendor_args=(vendor --locked --versioned-dirs "$vendor")
if "$offline"; then vendor_args+=(--offline); fi
run_log vendor.log cargo "${vendor_args[@]}"
mkdir -p "$STAGE/archives" "$STAGE/isolated"
: >"$STAGE/archive-checksums.jsonl"
for name in "${ordered[@]}"; do
    folder="$name-$version"
    archive="$STAGE/archives/$folder.crate"
    cp "$ROOT/target/package/$folder.crate" "$archive"
    [[ "$(wc -c <"$archive")" -le 10485760 ]] || fail "Archive exceeds crates.io default size limit: $name"
    checksum="$(digest "$archive")"
    jq -cn --arg name "$folder.crate" --arg hash "$checksum" '{key: $name, value: $hash}' >>"$STAGE/archive-checksums.jsonl"
    # Replace only this invocation's generated staging package, avoiding stale files.
    if [[ -d "$vendor/$folder" ]]; then rm -rf "${vendor:?}/${folder:?}"; fi
    tar -xzf "$archive" -C "$vendor"
    : >"$STAGE/file-checksums.jsonl"
    while IFS= read -r -d '' file; do
        relative="${file#"$vendor/$folder/"}"
        jq -cn --arg name "$relative" --arg hash "$(digest "$file")" \
            '{key: $name, value: $hash}' >>"$STAGE/file-checksums.jsonl"
    done < <(find "$vendor/$folder" -type f ! -name .cargo-checksum.json -print0)
    jq -s --arg checksum "$checksum" '{files: from_entries, package: $checksum}' \
        "$STAGE/file-checksums.jsonl" >"$vendor/$folder/.cargo-checksum.json"
    package_root="$STAGE/isolated/$name"
    mkdir -p "$package_root/.cargo"
    if [[ -d "$package_root/$folder" ]]; then rm -rf "${package_root:?}/${folder:?}"; fi
    tar -xzf "$archive" -C "$package_root"
    printf '[workspace]\nresolver = "2"\nmembers = ["%s"]\n' "$folder" >"$package_root/Cargo.toml"
    cp "$package_root/$folder/Cargo.lock" "$package_root/Cargo.lock"
    # JSON quoted strings also encode these absolute paths as valid TOML strings.
    printf '[source.crates-io]\nreplace-with = "release-vendor"\n[source.release-vendor]\ndirectory = %s\n[net]\noffline = true\n' \
        "$(jq -Rn --arg path "$vendor" '$path')" >"$package_root/.cargo/config.toml"
    # Ask Cargo to inspect the normalized archive, including target/dev tables.
    cargo read-manifest --manifest-path "$package_root/$folder/Cargo.toml" >"$STAGE/archive-manifest.json"
    jq -e '[.dependencies[] | select(.path != null or
        ((.source // "") | startswith("git+")))] | length == 0' \
        "$STAGE/archive-manifest.json" >/dev/null || fail "Non-registry dependency in archive: $name"
done
# Exclude coverage/developer compiler flags from independent consumer builds.
while IFS= read -r key; do
    case "$key" in
        *LLVM_COV*|*RUSTFLAGS|LLVM_PROFILE_FILE|RUSTC_WRAPPER|RUSTC_WORKSPACE_WRAPPER) unset "$key" ;;
    esac
done < <(compgen -e)
export CARGO_TARGET_DIR="$STAGE/target"
for name in "${ordered[@]}"; do
    (cd "$STAGE/isolated/$name"; run_log "build-$name.log" \
        cargo build --locked --offline --manifest-path "$name-$version/Cargo.toml")
done
(cd "$STAGE/isolated/ox-cli"; run_log archive-tests.log \
    cargo test --locked --offline --manifest-path "ox-cli-$version/Cargo.toml" \
    --test remount --test policy_refusal --test tool_exec_install)
while IFS= read -r name; do
    (cd "$STAGE/isolated/$name"; run_log "install-$name.log" \
        cargo install --path "$name-$version" --locked --offline --root "$STAGE/install" --force)
done < <(jq -r '.install[]' "$ROOT/release.json")
run_log smoke.log bash "$ROOT/scripts/release-smoke.sh" --bin-dir "$STAGE/install/bin" --work-dir "$STAGE/smoke"
[[ "$(git rev-parse HEAD)" == "$head" ]] || fail 'HEAD changed during verification; rerun check.'
if ! "$dirty" && [[ -n "$(git status --porcelain)" ]]; then fail 'Checkout changed during verification; rerun check.'; fi
jq -s --arg head "$head" --argjson dirty "$dirty" --arg version "$version" \
    --arg platform "$(uname -s)-$(uname -m)" --arg rustc "$(rustc --version)" \
    --rawfile order "$STAGE/order.txt" \
    '{head: $head, dirty: $dirty, version: $version, platform: $platform, rustc: $rustc,
      independently_built: ($order | split("\n") | map(select(length > 0))), archives: from_entries}' \
    "$STAGE/archive-checksums.jsonl" >"$receipt"
echo "Release verified: ${#ordered[@]} archives; receipt $receipt"
