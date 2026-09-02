#!/bin/sh
set -eu

if [ "${OX_REMOTE_LIVE:-0}" != 1 ]; then
  echo "refusing live exe.dev mutation; set OX_REMOTE_LIVE=1" >&2
  exit 2
fi
: "${OX_REMOTE__EXE__WORKER_IMAGE:?set a digest-pinned worker image}"
case "$OX_REMOTE__EXE__WORKER_IMAGE" in
  *@sha256:*) ;;
  *) echo "worker image must be digest-pinned" >&2; exit 2 ;;
esac
command -v jq >/dev/null 2>&1 || { echo "jq is required" >&2; exit 2; }

run_id=${OX_REMOTE_LIVE_RUN_ID:-live-$(date -u +%Y%m%dT%H%M%SZ)-$$}
node_id=
cleanup_status=not-attempted

cleanup() {
  status=$?
  cleanup_failed=0
  trap - EXIT HUP INT TERM
  if [ -n "$node_id" ]; then
    if cargo run -q -p ox-cli --bin ox -- remote --json node delete \
      "$node_id" --yes --force \
      --request-id "cleanup-$run_id" --delete-id "cleanup-$run_id"
    then
      cleanup_status=deleted
    else
      cleanup_status="FAILED: node $node_id may be leaked"
      cleanup_failed=1
    fi
  fi
  echo "live cleanup: $cleanup_status" >&2
  if [ "$cleanup_failed" -ne 0 ]; then
    exit 1
  fi
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' HUP INT TERM

node_json=$(cargo run -q -p ox-cli --bin ox -- remote --json node new \
  --request-id "node-$run_id")
node_id=$(printf '%s\n' "$node_json" | jq -er '.node_id')
cargo run -q -p ox-cli --bin ox -- remote --json node doctor "$node_id" >/dev/null

conversation_json=$(cargo run -q -p ox-cli --bin ox -- remote --json conversation new \
  --node "$node_id" --request-id "conversation-$run_id" \
  --title "remote live smoke $run_id" --prompt "reply with the token $run_id")
conversation_id=$(printf '%s\n' "$conversation_json" | jq -er '.conversation_id')

cargo run -q -p ox-cli --bin ox -- remote --json conversation send \
  "$conversation_id" --message-id "message-$run_id" \
  --prompt "finish this smoke test without using tools" >/dev/null
cargo run -q -p ox-cli --bin ox -- remote --json conversation reconcile \
  "$conversation_id" >/dev/null
cargo run -q -p ox-cli --bin ox -- remote conversation logs \
  "$conversation_id" --jsonl >/dev/null
cargo run -q -p ox-cli --bin ox -- remote --json conversation cancel \
  "$conversation_id" --wait --timeout 30s \
  --request-id "cancel-request-$run_id" --cancel-id "cancel-$run_id" >/dev/null

echo "live smoke passed: node=$node_id conversation=$conversation_id" >&2
