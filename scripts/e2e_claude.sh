#!/usr/bin/env bash
# End-to-end test against a Dockerized bridge.
# Verifies, in order:
#   1. container builds & boots
#   2. push agent (one per instance) succeeds
#   3. simple Q&A streams a content_delta + turn_completed
#   4. tool-call request triggers tool_call_started + tool_call_completed
#   5. approval flow: prompt that needs permission triggers
#      tool_approval_required, we approve via /approvals, tool runs
#
# Usage: scripts/e2e_claude.sh [--no-build] [--keep]

set -euo pipefail

NO_BUILD=0
KEEP=0
while [[ $# -gt 0 ]]; do
    case "$1" in
        --no-build) NO_BUILD=1 ;;
        --keep) KEEP=1 ;;
        *) echo "unknown arg $1" >&2; exit 2 ;;
    esac
    shift
done

: "${ANTHROPIC_BASE_URL:=https://token-plan-sgp.xiaomimimo.com/anthropic}"
: "${ANTHROPIC_AUTH_TOKEN:=***REMOVED***}"
: "${ANTHROPIC_MODEL:=mimo-v2.5-pro}"
: "${BRIDGE_BASE_URL:=http://127.0.0.1:8080}"

CTRL_KEY="test-control-plane-key"
IMAGE_TAG="bridge-e2e:latest"
CONTAINER_NAME="bridge-e2e"

cleanup() {
    if [[ $KEEP -eq 1 ]]; then
        echo "→ keeping container ${CONTAINER_NAME} for inspection"
        return
    fi
    echo "→ tearing down container ${CONTAINER_NAME}"
    docker rm -f "${CONTAINER_NAME}" >/dev/null 2>&1 || true
    rm -f /tmp/bridge_events.*
}
trap cleanup EXIT

build_image() {
    echo "→ building docker image ${IMAGE_TAG}"
    DOCKER_BUILDKIT=1 docker build -f docker/Dockerfile -t "${IMAGE_TAG}" .
}

start_container() {
    local permission_mode="$1"
    echo "→ removing any stale container"
    docker rm -f "${CONTAINER_NAME}" >/dev/null 2>&1 || true

    echo "→ starting container (permission_mode=${permission_mode})"
    docker run -d --name "${CONTAINER_NAME}" \
        -p 8080:8080 \
        -e ANTHROPIC_BASE_URL="${ANTHROPIC_BASE_URL}" \
        -e ANTHROPIC_AUTH_TOKEN="${ANTHROPIC_AUTH_TOKEN}" \
        -e ANTHROPIC_MODEL="${ANTHROPIC_MODEL}" \
        -e ANTHROPIC_DEFAULT_SONNET_MODEL="${ANTHROPIC_MODEL}" \
        -e ANTHROPIC_DEFAULT_OPUS_MODEL="${ANTHROPIC_MODEL}" \
        -e ANTHROPIC_DEFAULT_HAIKU_MODEL="${ANTHROPIC_MODEL}" \
        "${IMAGE_TAG}" >/dev/null

    echo "→ waiting for /health"
    for i in {1..30}; do
        if curl -fsS "${BRIDGE_BASE_URL}/health" >/dev/null 2>&1; then
            echo "  bridge healthy after ${i}s"
            return
        fi
        sleep 1
    done
    echo "✗ bridge failed to start" >&2
    docker logs "${CONTAINER_NAME}" >&2 || true
    exit 1
}

push_agent() {
    local permission_mode="$1"
    local mcp_servers_json="${2:-[]}"
    AGENT_ID="agent_test"

    echo "→ pushing agent (permission_mode=${permission_mode})"
    PUSH_BODY=$(cat <<JSON
{
  "agents": [
    {
      "id": "${AGENT_ID}",
      "name": "Test Claude",
      "harness": "claude",
      "system_prompt": "You are a helpful, terse assistant. Always answer in under 50 words. When the user asks you to remember or recall something, use the available memory tools (mcp__hiveloop__memory_retain, mcp__hiveloop__memory_recall, mcp__hiveloop__memory_retrieve) instead of relying on your own context.",
      "provider": {
        "provider_type": "anthropic",
        "model": "${ANTHROPIC_MODEL}",
        "api_key": "unused",
        "base_url": "${ANTHROPIC_BASE_URL}"
      },
      "mcp_servers": ${mcp_servers_json},
      "config": {
        "permission_mode": "${permission_mode}"
      }
    }
  ]
}
JSON
)

    if [[ -n "${BRIDGE_E2E_DEBUG:-}" ]]; then
        echo "── PUSH BODY ──"
        echo "${PUSH_BODY}"
        echo "── /PUSH BODY ──"
    fi
    PUSH_RESP=$(curl -sS -w "\n%{http_code}" \
        -X POST "${BRIDGE_BASE_URL}/push/agents" \
        -H "content-type: application/json" \
        -H "authorization: Bearer ${CTRL_KEY}" \
        -d "${PUSH_BODY}")
    local code=$(echo "${PUSH_RESP}" | tail -n1)
    local body=$(echo "${PUSH_RESP}" | sed '$d')
    if [[ "${code}" != "200" ]]; then
        echo "✗ push/agents returned ${code}: ${body}" >&2
        docker logs "${CONTAINER_NAME}" >&2
        exit 1
    fi
    echo "  pushed: ${body}"
}

create_conversation() {
    CONV_RESP=$(curl -sS -X POST "${BRIDGE_BASE_URL}/agents/${AGENT_ID}/conversations" \
        -H "content-type: application/json" \
        -H "authorization: Bearer ${CTRL_KEY}" \
        -d '{}')
    CONV_ID=$(echo "${CONV_RESP}" | python3 -c "import sys,json;print(json.load(sys.stdin)['conversation_id'])")
    echo "  conversation_id=${CONV_ID}"
}

start_sse_subscriber() {
    EVENTS_FILE=$(mktemp /tmp/bridge_events.XXXXXX)
    echo "  events → ${EVENTS_FILE}"
    curl -sN "${BRIDGE_BASE_URL}/conversations/${CONV_ID}/stream" > "${EVENTS_FILE}" &
    SSE_PID=$!
    sleep 1
}

send_message() {
    local prompt="$1"
    local body=$(printf '{"content": %s}' "$(printf '%s' "${prompt}" | python3 -c 'import sys,json;print(json.dumps(sys.stdin.read()))')")
    curl -fsS -X POST "${BRIDGE_BASE_URL}/conversations/${CONV_ID}/messages" \
        -H "content-type: application/json" \
        -d "${body}" >/dev/null
}

wait_for_terminal_event() {
    local timeout="${1:-90}"
    local deadline=$((SECONDS + timeout))
    while (( SECONDS < deadline )); do
        if grep -q "event: turn_completed\|event: agent_error" "${EVENTS_FILE}" 2>/dev/null; then
            return 0
        fi
        sleep 1
    done
    echo "✗ timed out waiting for turn_completed" >&2
    docker logs "${CONTAINER_NAME}" 2>&1 | tail -100 >&2
    exit 1
}

stop_subscriber() {
    kill "${SSE_PID}" >/dev/null 2>&1 || true
    wait "${SSE_PID}" >/dev/null 2>&1 || true
}

dump_events() {
    echo "──── EVENTS (${1}) ────"
    cat "${EVENTS_FILE}"
    echo "──── /END ────"
}

assert_event() {
    local pattern="$1"
    local description="$2"
    if grep -q "${pattern}" "${EVENTS_FILE}"; then
        echo "  ✓ ${description}"
    else
        echo "  ✗ MISSING: ${description}" >&2
        dump_events "fail"
        docker logs "${CONTAINER_NAME}" 2>&1 | tail -100 >&2
        exit 1
    fi
}

# ──────────────────────────────────────────
# Phase 1: build + boot + simple Q&A (bypassPermissions for the tool phase)
# ──────────────────────────────────────────
if [[ $NO_BUILD -eq 0 ]]; then
    build_image
fi
start_container "bypassPermissions"
push_agent "bypassPermissions"

echo
echo "═══ Phase 1: simple Q&A ═══"
create_conversation
start_sse_subscriber
send_message "What is 2+2? Reply with just the number."
wait_for_terminal_event 30
stop_subscriber
echo
assert_event "event: content_delta" "Phase 1: got content_delta (response_chunk)"
assert_event "event: turn_completed" "Phase 1: got turn_completed"

# ──────────────────────────────────────────
# Phase 2: tool call (forced Bash echo) — bypass perms so it just runs
# ──────────────────────────────────────────
echo
echo "═══ Phase 2: tool call ═══"
create_conversation
start_sse_subscriber
send_message "Use the Bash tool right now. Execute exactly this command: echo HELLO_FROM_BRIDGE. After running it, tell me the exact output."
wait_for_terminal_event 45
stop_subscriber
echo
assert_event "event: tool_call_start" "Phase 2: got tool_call_start"
assert_event "event: tool_call_result" "Phase 2: got tool_call_result"
assert_event "event: turn_completed" "Phase 2: got turn_completed"

# ──────────────────────────────────────────
# Phase 3: approval flow — restart with permission_mode=default,
# fire a Bash request, observe tool_approval_required, approve via API.
# ──────────────────────────────────────────
echo
echo "═══ Phase 3: approval flow ═══"
docker rm -f "${CONTAINER_NAME}" >/dev/null 2>&1 || true
start_container "default"
push_agent "default"

create_conversation
start_sse_subscriber
send_message "Use the Write tool to create a new file at /workspace/approved.txt with the contents: APPROVED_AND_WRITTEN. Then confirm the path you wrote."

echo "→ waiting for tool_approval_required (up to 30s)"
APPROVAL_DEADLINE=$((SECONDS + 30))
APPROVAL_REQ_ID=""
while (( SECONDS < APPROVAL_DEADLINE )); do
    APPROVAL_LINE=$(grep "event: tool_approval_required" "${EVENTS_FILE}" -A1 2>/dev/null | tail -1 || true)
    if [[ -n "${APPROVAL_LINE}" ]]; then
        # Pull it from the /approvals API instead — robust against SSE timing.
        REQS=$(curl -sS "${BRIDGE_BASE_URL}/agents/${AGENT_ID}/conversations/${CONV_ID}/approvals")
        APPROVAL_REQ_ID=$(echo "${REQS}" | python3 -c "import sys,json;j=json.load(sys.stdin);print(j[0]['id'] if j else '')")
        if [[ -n "${APPROVAL_REQ_ID}" ]]; then
            break
        fi
    fi
    sleep 1
done

if [[ -z "${APPROVAL_REQ_ID}" ]]; then
    echo "✗ no approval request appeared" >&2
    dump_events "phase3-fail"
    docker logs "${CONTAINER_NAME}" 2>&1 | tail -100 >&2
    exit 1
fi
echo "  approval id: ${APPROVAL_REQ_ID}"

echo "→ approving via API"
curl -fsS -X POST "${BRIDGE_BASE_URL}/agents/${AGENT_ID}/conversations/${CONV_ID}/approvals/${APPROVAL_REQ_ID}" \
    -H "content-type: application/json" \
    -d '{"decision": "approve"}' >/dev/null

wait_for_terminal_event 45
stop_subscriber
echo
assert_event "event: tool_approval_required" "Phase 3: got tool_approval_required"
assert_event "event: tool_approval_resolved" "Phase 3: got tool_approval_resolved"
assert_event "event: tool_call_result" "Phase 3: got tool_call_result"
assert_event "event: turn_completed" "Phase 3: got turn_completed"


# ──────────────────────────────────────────
# Phase 4: custom MCP server (hiveloop memory tools)
# Restart container with bypassPermissions so MCP tools just run; assert
# the agent invokes the named MCP tools across two separate conversations.
# ──────────────────────────────────────────
echo
echo "═══ Phase 4: custom MCP (hiveloop memory) ═══"
docker rm -f "${CONTAINER_NAME}" >/dev/null 2>&1 || true
start_container "bypassPermissions"

HIVELOOP_MCP=$(cat <<'JSON'
[
  {
    "name": "hiveloop",
    "transport": {
      "type": "streamable_http",
      "url": "https://mcp.usehiveloop.com/292b4d0a-0f4f-47fe-b7a4-cb479b465c92",
      "headers": {
        "Authorization": "Bearer ***REMOVED***"
      }
    }
  }
]
JSON
)
push_agent "bypassPermissions" "${HIVELOOP_MCP}"

# Phase 4a — retain
echo "── 4a: retain a fact ──"
create_conversation
start_sse_subscriber
send_message "Use the memory_retain tool to save this fact: my favorite color is purple."
wait_for_terminal_event 45
stop_subscriber
echo
assert_event "memory_retain" "Phase 4a: tool_call mentions memory_retain"
assert_event "event: tool_call_result" "Phase 4a: got tool_call_result"
assert_event "event: turn_completed" "Phase 4a: got turn_completed"

# Phase 4b — recall (in a fresh conversation so the model can't cheat from context)
echo "── 4b: recall the fact ──"
create_conversation
start_sse_subscriber
send_message "What is my favorite color? Use the memory_recall or memory_retrieve tool to look it up."
wait_for_terminal_event 45
stop_subscriber
echo
if grep -q "memory_recall\|memory_retrieve" "${EVENTS_FILE}"; then
    echo "  ✓ Phase 4b: tool_call mentions memory_recall or memory_retrieve"
else
    echo "  ✗ MISSING: Phase 4b: memory_recall/retrieve" >&2
    dump_events "phase4b-fail"
    docker logs "${CONTAINER_NAME}" 2>&1 | tail -50 >&2
    exit 1
fi
assert_event "event: tool_call_result" "Phase 4b: got tool_call_result"
assert_event "event: turn_completed" "Phase 4b: got turn_completed"


# ──────────────────────────────────────────
# Phase 5: stop/start container — restore conversation
# Daytona-style sleep simulation: docker stop preserves the writable
# layer (claude session backup + bridge.db). After docker start, bridge
# should call ACP session/load for every persisted conversation. We then
# send a follow-up referencing the prior turn and assert the response
# shows context awareness.
# ──────────────────────────────────────────
echo
echo "═══ Phase 5: stop/start, restore conversation ═══"

# Phase 5a — establish a conversation with a memorable seed turn.
docker rm -f "${CONTAINER_NAME}" >/dev/null 2>&1 || true
start_container "bypassPermissions"
push_agent "bypassPermissions"
create_conversation
SAVED_CONV_ID="${CONV_ID}"
start_sse_subscriber
send_message "Remember this token: PURPLE_LLAMA_42. Reply with 'noted'."
wait_for_terminal_event 30
stop_subscriber
echo
assert_event "event: turn_completed" "Phase 5a: pre-restart turn_completed"

# Phase 5b — stop + start, then send follow-up that references the seed.
echo "→ stopping container (preserves writable layer)"
docker stop "${CONTAINER_NAME}" >/dev/null
echo "→ restarting container"
docker start "${CONTAINER_NAME}" >/dev/null
echo "→ waiting for /health"
for i in {1..30}; do
    if curl -fsS "${BRIDGE_BASE_URL}/health" >/dev/null 2>&1; then
        echo "  bridge healthy after ${i}s"
        break
    fi
    sleep 1
done

# Confirm the agent and conversation survived.
RESTORED_AGENTS=$(curl -fsS "${BRIDGE_BASE_URL}/agents" | python3 -c "import sys,json;print(len(json.load(sys.stdin)))")
if [[ "${RESTORED_AGENTS}" != "1" ]]; then
    echo "✗ expected 1 restored agent, got ${RESTORED_AGENTS}" >&2
    docker logs "${CONTAINER_NAME}" 2>&1 | tail -50 >&2
    exit 1
fi
echo "  ✓ Phase 5b: agent restored from storage"

CONV_ID="${SAVED_CONV_ID}"
EVENTS_FILE=$(mktemp /tmp/bridge_events.XXXXXX)
curl -sN "${BRIDGE_BASE_URL}/conversations/${CONV_ID}/stream" > "${EVENTS_FILE}" &
SSE_PID=$!
sleep 1
send_message "What was the token I asked you to remember? Reply with just the token."
wait_for_terminal_event 45
stop_subscriber
echo
assert_event "event: content_delta" "Phase 5b: post-restart content_delta"
assert_event "event: turn_completed" "Phase 5b: post-restart turn_completed"
if grep -q "PURPLE_LLAMA_42" "${EVENTS_FILE}"; then
    echo "  ✓ Phase 5b: response references the pre-restart token (PURPLE_LLAMA_42)"
else
    echo "  ✗ MISSING: Phase 5b: response should mention PURPLE_LLAMA_42" >&2
    dump_events "phase5b-fail"
    docker logs "${CONTAINER_NAME}" 2>&1 | tail -50 >&2
    exit 1
fi

echo
echo "✓✓✓ E2E PASSED (Phases 1, 2, 3, 4, 5) ✓✓✓"
