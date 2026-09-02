#!/bin/sh
set -eu

if [ "${OX_REMOTE_SOAK:-0}" != 1 ]; then
  echo "refusing live soak; set OX_REMOTE_SOAK=1" >&2
  exit 2
fi
: "${OX_REMOTE_SOAK_NODE:?set the verified disposable node id}"
command -v jq >/dev/null 2>&1 || { echo "jq is required" >&2; exit 2; }

duration=${OX_REMOTE_SOAK_SECONDS:-86400}
threads=${OX_REMOTE_SOAK_THREADS:-50}
case "$duration:$threads" in
  *[!0-9:]*|:*|*:) echo "duration and thread count must be positive integers" >&2; exit 2 ;;
esac
[ "$duration" -gt 0 ] && [ "$threads" -ge 50 ] || {
  echo "the rollout soak requires positive duration and at least 50 threads" >&2
  exit 2
}

run_id=${OX_REMOTE_SOAK_RUN_ID:-soak-$(date -u +%Y%m%dT%H%M%SZ)-$$}
report_dir=${OX_REMOTE_SOAK_REPORT_DIR:-target/remote-soak/$run_id}
mkdir -p "$report_dir"
started_at=$(date +%s)
deadline=$((started_at + duration))
i=1

while [ "$i" -le "$threads" ]; do
  request_id="$run_id-create-$i"
  conversation_json=$(cargo run -q -p ox-cli --bin ox -- remote --json conversation new \
    --node "$OX_REMOTE_SOAK_NODE" --request-id "$request_id" \
    --title "$run_id thread $i" --prompt "soak canary $run_id/$i")
  printf '%s\n' "$conversation_json" >>"$report_dir/conversations.jsonl"
  conversation_id=$(printf '%s\n' "$conversation_json" | jq -er '.conversation_id')
  printf '%s\t%s\n' "$conversation_id" "$i" >>"$report_dir/manifest.tsv"
  i=$((i + 1))
done

created=$(cut -f1 "$report_dir/manifest.tsv" | sort -u | wc -l | tr -d ' ')
[ "$created" -eq "$threads" ] || {
  echo "expected $threads distinct conversations, got $created" >&2
  exit 1
}

while [ "$(date +%s)" -lt "$deadline" ]; do
  cargo run -q -p ox-cli --bin ox -- remote --json conversation reconcile \
    >>"$report_dir/reconcile.jsonl"
  cargo run -q -p ox-cli --bin ox -- remote --json node doctor \
    "$OX_REMOTE_SOAK_NODE" >>"$report_dir/doctor.jsonl"
  sleep 30
done

cargo run -q -p ox-cli --bin ox -- remote --json conversation reconcile \
  >>"$report_dir/reconcile.jsonl"
cargo run -q -p ox-cli --bin ox -- remote --json node show \
  "$OX_REMOTE_SOAK_NODE" >"$report_dir/final-node.json"

while IFS="	" read -r conversation_id index; do
  ledger="$report_dir/ledger-$conversation_id.jsonl"
  cargo run -q -p ox-cli --bin ox -- remote conversation logs \
    "$conversation_id" --jsonl >"$ledger"
  jq -e -s '
    length > 0 and
    ([.[].seq] | unique | length) == length and
    ([.[].hash] | unique | length) == length and
    (. as $rows | [range(1; length) as $i |
      $rows[$i].parent == $rows[$i - 1].hash] | all)
  ' "$ledger" >/dev/null
  own="soak canary $run_id/$index"
  users=$(jq -s '[.[] | select(.msg.type == "user")] | length' "$ledger")
  [ "$users" -eq 1 ] || {
    echo "conversation $conversation_id has $users user entries; expected one" >&2
    exit 1
  }
  jq -e -s --arg prefix "soak canary $run_id/" --arg own "$own" '
    all(.[]; ((.msg | tostring | contains($prefix)) | not) or
      (.msg | tostring | contains($own)))
  ' "$ledger" >/dev/null
done <"$report_dir/manifest.tsv"

echo "soak completed; inspect $report_dir before deleting the disposable node" >&2
