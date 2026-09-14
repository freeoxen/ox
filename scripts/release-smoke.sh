#!/usr/bin/env bash
# Smoke-test installed executables without changing HOME or reading user accounts.
set -euo pipefail
BIN_DIR=
WORK_DIR=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --bin-dir) BIN_DIR="$2"; shift 2 ;;
        --work-dir) WORK_DIR="$2"; shift 2 ;;
        *) echo "usage: $0 --bin-dir PATH --work-dir PATH" >&2; exit 2 ;;
    esac
done
[ -n "$BIN_DIR" ] && [ -n "$WORK_DIR" ] || { echo "Both directories are required" >&2; exit 2; }
BIN_DIR="$(cd "$BIN_DIR" && pwd)"
mkdir -p "$WORK_DIR"
WORK_DIR="$(cd "$WORK_DIR" && pwd)"
cd "$WORK_DIR"
ACTIVE_PID=
GATEWAY_PID=
fail() { echo "$*" >&2; exit 1; }

# macOS has no timeout(1). Poll direct children and reap them; recursively stop
# descendants on abnormal exit so a helper's shell cannot outlive this script.
kill_tree() {
    local pid="$1" child
    for child in $(ps -axo pid=,ppid= | awk -v parent="$pid" '$2 == parent {print $1}'); do
        kill_tree "$child"
    done
    kill -KILL "$pid" 2>/dev/null || true
}
cleanup() {
    local pid
    for pid in "$ACTIVE_PID" "$GATEWAY_PID"; do
        if [ -n "$pid" ]; then
            kill_tree "$pid"
            wait "$pid" 2>/dev/null || true
        fi
    done
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
wait_bounded() {
    local pid="$1" limit="$2" deadline=$((SECONDS + $2))
    while kill -0 "$pid" 2>/dev/null; do
        if [ "$SECONDS" -ge "$deadline" ]; then
            fail "Process $pid exceeded ${limit}s"
        fi
        sleep 0.1
    done
    wait "$pid"
}
run_bounded() {
    "$@" <&0 &
    ACTIVE_PID=$!
    wait_bounded "$ACTIVE_PID" 15
    ACTIVE_PID=
}
helper() {
    run_bounded "$BIN_DIR/ox-tool-exec" --tool-exec < request.json > response.json
    jq -e '.ok == true' response.json > /dev/null || fail "Helper failed: $(cat response.json)"
}
for name in ox ox-worker; do
    run_bounded "$BIN_DIR/$name" --help > "$name-help.log" 2>&1
    grep -q 'Usage:' "$name-help.log" || fail "$name did not print help"
    echo "$name: installed CLI entry point works"
done
FILE="$WORK_DIR/tool-file.txt"
jq -n --arg path "$FILE" '{op:"fs/write",args:{path:$path,content:"before\n"}}' > request.json
helper
jq -n --arg path "$FILE" '{op:"fs/read",args:{path:$path}}' > request.json
helper
jq -e '.value == "before\n"' response.json > /dev/null
jq -n --arg path "$FILE" '{op:"fs/edit",args:{path:$path,old_string:"before",new_string:"after"}}' > request.json
helper
printf 'after\n' > expected.txt
cmp "$FILE" expected.txt
jq -n --arg path "$FILE" '{op:"fs/read",args:{path:$path},_ox_max_output_bytes:3}' > request.json
helper
jq -e '.value == "aft\n[... file truncated at byte limit]"' response.json > /dev/null
jq -n --arg workspace "$WORK_DIR" '{op:"os/shell",args:{command:"printf abcdefgh",workspace:$workspace},_ox_max_output_bytes:4}' > request.json
helper
jq -e '.value.exit_code == 0 and .value.stdout == "abcd\n[... output truncated at byte limit]"' response.json > /dev/null
echo "ox-tool-exec: installed helper write/read/edit/shell and output limits work"

mkdir -p gateway-state
printf '[gate.accounts]\n[gate.providers]\n' > gateway-state/config.toml
(
    while IFS= read -r key; do
        case "$key" in OX_*) unset "$key" ;; esac
    done < <(compgen -e)
    export OX_DIR="$WORK_DIR/gateway-state" OX_GATEWAY_BIND=127.0.0.1:0 RUST_LOG=info
    exec "$BIN_DIR/ox-gateway"
) > gateway.log 2>&1 &
GATEWAY_PID=$!
DEADLINE=$((SECONDS + 45))
PORT=
while [ "$SECONDS" -lt "$DEADLINE" ]; do
    kill -0 "$GATEWAY_PID" 2>/dev/null || fail "Gateway exited during startup; see $WORK_DIR/gateway.log"
    PORT="$(sed -n '/ox-gateway listening/s/.*127\.0\.0\.1:\([0-9][0-9]*\).*/\1/p' gateway.log | head -1)"
    if [ -n "$PORT" ]; then break; fi
    sleep 0.1
done
[ -n "$PORT" ] || fail "Gateway did not start within 45s; see $WORK_DIR/gateway.log"
BASE="http://127.0.0.1:$PORT"
STATUS="$(curl --silent --show-error --noproxy '*' --max-time 20 -o models.json -w '%{http_code}' "$BASE/v1/models")"
if [ "$STATUS" != 200 ] || ! jq -e '.data == []' models.json > /dev/null; then
    fail "Unexpected catalog response"
fi
STATUS="$(curl --silent --show-error --noproxy '*' --max-time 20 -o stats.json -w '%{http_code}' "$BASE/stats")"
if [ "$STATUS" != 200 ] || ! jq -e 'type == "object" and (has("error") | not)' stats.json > /dev/null; then
    fail "Embedded telemetry failed"
fi
# This reaches the embedded wire codec and fails before contacting any provider.
STATUS="$(curl --silent --show-error --noproxy '*' --max-time 20 -o wire.json -w '%{http_code}' -H 'Content-Type: application/json' --data '{}' "$BASE/v1/chat/completions")"
if [ "$STATUS" != 400 ] || ! jq -e 'has("error")' wire.json > /dev/null; then
    fail "Embedded wire codec failed"
fi
kill -TERM "$GATEWAY_PID"
wait_bounded "$GATEWAY_PID" 15
GATEWAY_PID=
echo "ox-gateway: isolated startup, catalog, embedded telemetry/wire Wasm, graceful shutdown work"
