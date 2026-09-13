#!/usr/bin/env bash
# AC2 (PRD-homeward-mcp-hub-deploy): Given the running unit, When a client
# outside the hub's private network calls `tools/list` against its
# streamable-HTTP endpoint, Then `search_pets`/`get_pet` appear with
# schemas.
#
# Real-environment check -- run this FROM a box other than the hub itself
# (RedBaron/carbon over Tailscale satisfies "outside the hub's private
# network" per the PRD's own minimum bar). Speaks raw MCP streamable-HTTP
# JSON-RPC: initialize -> notifications/initialized -> tools/list. No
# fixtures, no mock server.
set -uo pipefail

HUB_ADDR="${HOMEWARD_MCP_ADDR:-100.66.158.49:8095}"
URL="http://${HUB_ADDR}/mcp"
PROTO="2025-06-18"

headers=$(mktemp)
trap 'rm -f "$headers"' EXIT

init_body='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"'"$PROTO"'","capabilities":{},"clientInfo":{"name":"hub-deploy-ac2","version":"0.0.0"}}}'

curl -sS -m 8 -D "$headers" -X POST "$URL" \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -d "$init_body" >/dev/null

session=$(grep -i '^mcp-session-id:' "$headers" | sed 's/.*: //' | tr -d '\r')
if [[ -z "$session" ]]; then
  echo "FAIL: no mcp-session-id from $URL (endpoint unreachable or handshake failed)" >&2
  exit 1
fi

curl -sS -m 8 -X POST "$URL" \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -H "Mcp-Session-Id: $session" \
  -H "Mcp-Protocol-Version: $PROTO" \
  -d '{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}' -o /dev/null

list=$(curl -sS -m 8 -X POST "$URL" \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -H "Mcp-Session-Id: $session" \
  -H "Mcp-Protocol-Version: $PROTO" \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}')

if ! grep -q '"name":"search_pets"' <<<"$list"; then
  echo "FAIL: search_pets missing from tools/list: $list" >&2
  exit 1
fi
if ! grep -q '"name":"get_pet"' <<<"$list"; then
  echo "FAIL: get_pet missing from tools/list: $list" >&2
  exit 1
fi
if ! grep -q '"inputSchema"' <<<"$list"; then
  echo "FAIL: tools/list entries have no inputSchema: $list" >&2
  exit 1
fi

echo "OK: tools/list from an external host returned search_pets+get_pet with schemas"
