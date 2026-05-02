#!/usr/bin/env bash
# End-to-end test: build bridge in docker, push one Claude-harness agent,
# create a conversation, send a prompt, and collect lifecycle events.
#
# Usage: scripts/e2e_claude.sh [--no-build] [--keep] [--prompt "text"]
#
# Env overrides (defaults are real working values for testing):
#   BRIDGE_BASE_URL          host base URL the test driver hits (default http://127.0.0.1:8080)
#   ANTHROPIC_BASE_URL       upstream proxy URL passed into the container
#   ANTHROPIC_AUTH_TOKEN     gateway token passed into the container
#   ANTHROPIC_MODEL          model id passed into the container

set -euo pipefail

NO_BUILD=0
KEEP=0
PROMPT="What is 2+2? Reply with just the number."

while [[ $# -gt 0 ]]; do
    case "$1" in
        --no-build) NO_BUILD=1 ;;
        --keep) KEEP=1 ;;
        --prompt) shift; PROMPT="$1" ;;
        *) echo "unknown arg $1" >&2; exit 2 ;;
    esac
    shift
done

# Real default values for the test runs.
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
}
trap cleanup EXIT

if [[ $NO_BUILD -eq 0 ]]; then
    echo "→ building docker image ${IMAGE_TAG}"
    docker build -f docker/Dockerfile -t "${IMAGE_TAG}" .
fi

echo "→ removing any stale container"
docker rm -f "${CONTAINER_NAME}" >/dev/null 2>&1 || true

echo "→ starting container"
docker run -d --rm --name "${CONTAINER_NAME}" \
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
        break
    fi
    sleep 1
    if [[ $i -eq 30 ]]; then
        echo "✗ bridge failed to start" >&2
        docker logs "${CONTAINER_NAME}" >&2 || true
        exit 1
    fi
done

AGENT_ID="agent_test"

echo "→ pushing agent (one per instance)"
PUSH_BODY=$(cat <<JSON
{
  "agents": [
    {
      "id": "${AGENT_ID}",
      "name": "Test Claude",
      "harness": "claude",
      "system_prompt": "You are a quiet, terse assistant. Always answer in under 50 words.",
      "provider": {
        "provider_type": "anthropic",
        "model": "${ANTHROPIC_MODEL}",
        "api_key": "unused",
        "base_url": "${ANTHROPIC_BASE_URL}"
      },
      "config": {
        "permission_mode": "bypassPermissions"
      }
    }
  ]
}
JSON
)

PUSH_RESP=$(curl -sS -w "\n%{http_code}" \
    -X POST "${BRIDGE_BASE_URL}/push/agents" \
    -H "content-type: application/json" \
    -H "authorization: Bearer ${CTRL_KEY}" \
    -d "${PUSH_BODY}")
PUSH_CODE=$(echo "${PUSH_RESP}" | tail -n1)
PUSH_BODY_RESP=$(echo "${PUSH_RESP}" | sed '$d')

if [[ "${PUSH_CODE}" != "200" ]]; then
    echo "✗ push/agents returned ${PUSH_CODE}: ${PUSH_BODY_RESP}" >&2
    docker logs "${CONTAINER_NAME}" >&2
    exit 1
fi
echo "  pushed: ${PUSH_BODY_RESP}"

echo "→ creating conversation"
CONV_RESP=$(curl -sS -X POST "${BRIDGE_BASE_URL}/agents/${AGENT_ID}/conversations" \
    -H "content-type: application/json" \
    -H "authorization: Bearer ${CTRL_KEY}" \
    -d '{}')
CONV_ID=$(echo "${CONV_RESP}" | python3 -c "import sys,json;print(json.load(sys.stdin)['conversation_id'])")
echo "  conversation_id=${CONV_ID}"

# Open the SSE stream in the background; capture every event line.
EVENTS_FILE=$(mktemp /tmp/bridge_events.XXXXXX)
echo "→ subscribing to SSE → ${EVENTS_FILE}"
curl -sN "${BRIDGE_BASE_URL}/conversations/${CONV_ID}/stream" > "${EVENTS_FILE}" &
SSE_PID=$!
sleep 1

echo "→ sending prompt: ${PROMPT}"
SEND_BODY=$(printf '{"content": %s}' "$(printf '%s' "${PROMPT}" | python3 -c "import sys,json;print(json.dumps(sys.stdin.read()))")")
curl -fsS -X POST "${BRIDGE_BASE_URL}/conversations/${CONV_ID}/messages" \
    -H "content-type: application/json" \
    -d "${SEND_BODY}" >/dev/null

echo "→ collecting events for up to 90s"
DEADLINE=$((SECONDS + 90))
while (( SECONDS < DEADLINE )); do
    if grep -q "turn_completed\|message_end" "${EVENTS_FILE}" 2>/dev/null; then
        break
    fi
    sleep 1
done

# Stop SSE subscriber.
kill "${SSE_PID}" >/dev/null 2>&1 || true
wait "${SSE_PID}" >/dev/null 2>&1 || true

echo
echo "──── EVENTS ────"
cat "${EVENTS_FILE}"
echo
echo "──── /END EVENTS ────"

EVENT_COUNT=$(grep -c '^event:' "${EVENTS_FILE}" || true)
echo "→ event count: ${EVENT_COUNT}"

if (( EVENT_COUNT == 0 )); then
    echo "✗ no events received"
    docker logs "${CONTAINER_NAME}" 2>&1 | tail -100
    exit 1
fi

if grep -q "event: content_delta\|event: message_end\|event: turn_completed" "${EVENTS_FILE}"; then
    echo "✓ E2E PASSED"
    rm -f "${EVENTS_FILE}"
    exit 0
fi

echo "✗ E2E FAILED — no terminal event"
docker logs "${CONTAINER_NAME}" 2>&1 | tail -100
exit 1
