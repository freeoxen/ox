#!/usr/bin/env bash
#
# Smoke-test a running ox-gateway instance.
#
# Assumes the gateway is already running (e.g. `cargo run -p ox-gateway`)
# on $OX_GATEWAY_BIND (default 127.0.0.1:11343). Drives each route with
# curl, asserts on the wire shape, and confirms the usage ledger appended.
#
# Required: curl, jq. The Anthropic + OpenAI sections need API keys in
# ~/.ox/keys.json (or env-var override in the gateway's config); skipped
# automatically if the account isn't configured.
#
# Usage:
#   ./scripts/smoke-gateway.sh
#
# Env overrides:
#   OX_GATEWAY_BIND               where the gateway is listening (default 127.0.0.1:11343)
#   OX_SMOKE_ANTHROPIC_MODEL      slash-form model id (default anthropic/claude-haiku-4-5-20251001)
#   OX_SMOKE_OPENAI_MODEL         slash-form model id (default openai/gpt-4o-mini)
#   OX_SMOKE_USAGE_FILE           usage ledger path (default $HOME/.ox/usage.jsonl)

set -uo pipefail

ADDR="${OX_GATEWAY_BIND:-127.0.0.1:11343}"
BASE="http://${ADDR}"
ANTHROPIC_MODEL="${OX_SMOKE_ANTHROPIC_MODEL:-anthropic/claude-haiku-4-5-20251001}"
OPENAI_MODEL="${OX_SMOKE_OPENAI_MODEL:-openai/gpt-4o-mini}"
USAGE_FILE="${OX_SMOKE_USAGE_FILE:-$HOME/.ox/usage.jsonl}"

PASS=0
FAIL=0
SKIP=0
FAILURES=()

c_green() { printf "\033[32m%s\033[0m" "$1"; }
c_red()   { printf "\033[31m%s\033[0m" "$1"; }
c_yellow(){ printf "\033[33m%s\033[0m" "$1"; }
c_dim()   { printf "\033[2m%s\033[0m" "$1"; }

ok()   { printf "  $(c_green ok)   %s\n" "$1"; PASS=$((PASS+1)); }
fail() { printf "  $(c_red FAIL) %s\n%s\n" "$1" "$(echo "$2" | sed 's/^/        /')"; FAIL=$((FAIL+1)); FAILURES+=("$1"); }
skip() { printf "  $(c_yellow skip) %s — %s\n" "$1" "$2"; SKIP=$((SKIP+1)); }

require() {
    if ! command -v "$1" >/dev/null 2>&1; then
        echo "missing required tool: $1" >&2
        exit 2
    fi
}

require curl
require jq

echo
echo "== ox-gateway smoke at $BASE =="
echo

# ---------------------------------------------------------------------------
# /v1/models — always available, no upstream call
# ---------------------------------------------------------------------------

models_resp=$(curl -fsS --max-time 5 "$BASE/v1/models" 2>&1)
models_rc=$?
if [[ $models_rc -ne 0 ]]; then
    fail "GET /v1/models reaches the gateway" "$models_resp"
    echo
    echo "Cannot continue — the gateway didn't respond. Is it running on $BASE?"
    exit 1
fi
ok "GET /v1/models returns 200"

models_count=$(echo "$models_resp" | jq -r '.data | length')
if [[ "$models_count" =~ ^[0-9]+$ ]] && [[ "$models_count" -gt 0 ]]; then
    ok "GET /v1/models has $models_count entries"
else
    fail "GET /v1/models has at least one entry" "got: $models_resp"
fi

has_anthropic=$(echo "$models_resp" | jq -r '[.data[] | select(.id | startswith("anthropic/"))] | length > 0')
has_openai=$(echo "$models_resp" | jq -r '[.data[] | select(.id | startswith("openai/"))] | length > 0')

# ---------------------------------------------------------------------------
# Pre-flight: snapshot usage.jsonl length so we can verify it grew
# ---------------------------------------------------------------------------

usage_before=0
if [[ -f "$USAGE_FILE" ]]; then
    usage_before=$(wc -l < "$USAGE_FILE" | tr -d ' ')
fi

# ---------------------------------------------------------------------------
# POST /v1/messages — Anthropic streaming
# ---------------------------------------------------------------------------

if [[ "$has_anthropic" == "true" ]]; then
    anthropic_body=$(curl -fsS --max-time 30 -N \
        "$BASE/v1/messages" \
        -H 'Content-Type: application/json' \
        -d "$(jq -n --arg model "$ANTHROPIC_MODEL" \
            '{
                model: $model,
                max_tokens: 32,
                messages: [{role: "user", content: "say hi in one word"}],
                stream: true
            }')" 2>&1)
    anthropic_rc=$?

    if [[ $anthropic_rc -ne 0 ]]; then
        fail "POST /v1/messages streaming" "curl rc=$anthropic_rc; body: $anthropic_body"
    elif ! echo "$anthropic_body" | grep -q "event: message_start"; then
        fail "POST /v1/messages emits message_start" "body: $(echo "$anthropic_body" | head -10)"
    elif ! echo "$anthropic_body" | grep -q "event: content_block_delta"; then
        fail "POST /v1/messages emits content_block_delta" "body: $(echo "$anthropic_body" | head -10)"
    elif ! echo "$anthropic_body" | grep -q "event: message_stop"; then
        fail "POST /v1/messages emits message_stop" "body: $(echo "$anthropic_body" | tail -10)"
    else
        ok "POST /v1/messages streams Anthropic SSE"
    fi
else
    skip "POST /v1/messages streaming" "no Anthropic account in /v1/models"
fi

# ---------------------------------------------------------------------------
# POST /v1/chat/completions — OpenAI streaming
# ---------------------------------------------------------------------------

if [[ "$has_openai" == "true" ]]; then
    openai_body=$(curl -fsS --max-time 30 -N \
        "$BASE/v1/chat/completions" \
        -H 'Content-Type: application/json' \
        -d "$(jq -n --arg model "$OPENAI_MODEL" \
            '{
                model: $model,
                messages: [{role: "user", content: "say hi in one word"}],
                stream: true
            }')" 2>&1)
    openai_rc=$?

    if [[ $openai_rc -ne 0 ]]; then
        fail "POST /v1/chat/completions streaming" "curl rc=$openai_rc; body: $openai_body"
    elif ! echo "$openai_body" | grep -q '"role":"assistant"'; then
        fail "POST /v1/chat/completions emits assistant role" "body: $(echo "$openai_body" | head -10)"
    elif ! echo "$openai_body" | grep -q "data: \[DONE\]"; then
        fail "POST /v1/chat/completions terminates with [DONE]" "body: $(echo "$openai_body" | tail -10)"
    else
        ok "POST /v1/chat/completions streams OpenAI SSE"
    fi
else
    skip "POST /v1/chat/completions streaming" "no OpenAI account in /v1/models"
fi

# ---------------------------------------------------------------------------
# POST /completions — ox-native shape (uses whichever account we have)
# ---------------------------------------------------------------------------

if [[ "$has_anthropic" == "true" ]]; then
    native_model="$ANTHROPIC_MODEL"
elif [[ "$has_openai" == "true" ]]; then
    native_model="$OPENAI_MODEL"
else
    native_model=""
fi

if [[ -n "$native_model" ]]; then
    native_body=$(curl -fsS --max-time 30 -N \
        "$BASE/completions" \
        -H 'Content-Type: application/json' \
        -d "$(jq -n --arg model "$native_model" \
            '{
                model: $model,
                max_tokens: 32,
                system: "",
                messages: [{role: "user", content: "say hi in one word"}],
                tools: [],
                stream: true
            }')" 2>&1)
    native_rc=$?

    if [[ $native_rc -ne 0 ]]; then
        fail "POST /completions (ox-native)" "curl rc=$native_rc; body: $native_body"
    elif ! echo "$native_body" | grep -q '"type":"text_delta"'; then
        fail "POST /completions emits text_delta events" "body: $(echo "$native_body" | head -10)"
    elif ! echo "$native_body" | grep -q '"type":"message_stop"'; then
        fail "POST /completions emits message_stop" "body: $(echo "$native_body" | tail -10)"
    else
        ok "POST /completions streams ox-native StreamEvents"
    fi
else
    skip "POST /completions (ox-native)" "no accounts available"
fi

# ---------------------------------------------------------------------------
# Error path: unknown role returns an error frame
# ---------------------------------------------------------------------------

err_body=$(curl -fsS --max-time 10 -N \
    "$BASE/v1/messages" \
    -H 'Content-Type: application/json' \
    -d '{
        "model": "nopesuchrole",
        "max_tokens": 32,
        "messages": [{"role": "user", "content": "hi"}],
        "stream": true
    }' 2>&1)
err_rc=$?

if [[ $err_rc -ne 0 ]]; then
    fail "POST /v1/messages with unknown role" "curl rc=$err_rc; body: $err_body"
elif ! echo "$err_body" | grep -qE "event: error|no role named"; then
    fail "POST /v1/messages with unknown role emits error frame" "body: $(echo "$err_body" | head -10)"
else
    ok "POST /v1/messages with unknown role emits error frame"
fi

# ---------------------------------------------------------------------------
# Usage ledger: at least one record appended
# ---------------------------------------------------------------------------

usage_after=0
if [[ -f "$USAGE_FILE" ]]; then
    usage_after=$(wc -l < "$USAGE_FILE" | tr -d ' ')
fi

grew=$((usage_after - usage_before))
if [[ $grew -gt 0 ]]; then
    ok "$USAGE_FILE grew by $grew record(s)"
    last_line=$(tail -1 "$USAGE_FILE" 2>/dev/null || true)
    if [[ -n "$last_line" ]]; then
        echo
        echo "  $(c_dim "last record:")"
        echo "$last_line" | jq -C . 2>/dev/null | sed 's/^/    /' || echo "    $last_line"
    fi
else
    skip "$USAGE_FILE grew" "no new lines (file: $USAGE_FILE, before=$usage_before after=$usage_after)"
fi

# ---------------------------------------------------------------------------
# Summary
# ---------------------------------------------------------------------------

echo
echo "$(c_green "$PASS passed"), $(c_red "$FAIL failed"), $(c_yellow "$SKIP skipped")"

if [[ $FAIL -gt 0 ]]; then
    echo
    echo "Failed checks:"
    for f in "${FAILURES[@]}"; do
        echo "  - $f"
    done
    exit 1
fi
exit 0
