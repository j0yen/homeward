#!/usr/bin/env bash
# AC1 (PRD-homeward-mcp-hub-deploy-public-reachability-test-gap): Given the
# hub's homeward-mcp unit running, When a client queries it via the
# non-Tailscale/public path, Then it passes today and would fail loudly if
# that path were closed.
#
# Why this exists: hub-deploy's own AC2/AC3 tests (siblings in this dir)
# only prove reachability from a Tailscale peer (100.66.158.49:8095). The
# actual external caller this deploy exists for -- mcphost.dev's tenant
# tool-execution sandbox (2.28.40.4) -- is NOT a Tailscale peer and could
# not reach that address at all (confirmed: URLError: <urlopen error
# timed out>). The hub's public IP (178.105.64.66:8095) is the address
# that actually works from mcphost.dev; this test pins itself to a route
# that provably avoids tailscale0 before trusting a green result, so a
# future firewall change that closes the public port fails this test
# instead of silently breaking every mcphost tenant tool built on top of
# it.
#
# Real-environment check -- no fixtures, no mock server. Speaks raw MCP
# streamable-HTTP JSON-RPC: initialize -> notifications/initialized ->
# tools/list -> tools/call(search_pets), all against the public address.
set -uo pipefail

HUB_PUBLIC_ADDR="${HOMEWARD_MCP_PUBLIC_ADDR:-178.105.64.66:8095}"
HUB_PUBLIC_IP="${HUB_PUBLIC_ADDR%%:*}"
URL="http://${HUB_PUBLIC_ADDR}/mcp"
PROTO="2025-06-18"

# --- Step 1: prove this box's route to the public IP does not run over
# tailscale0. If it did, a "pass" here would be exactly the weak bar this
# PRD exists to close (see hub-deploy_ac2/ac3's Tailscale-only vantage).
route_line=$(ip route get "$HUB_PUBLIC_IP" 2>&1)
rc=$?
if [[ $rc -ne 0 ]]; then
  echo "FAIL: no route to $HUB_PUBLIC_IP at all (ip route get: $route_line)" >&2
  exit 1
fi
egress_dev=$(sed -n 's/.* dev \([^ ]*\).*/\1/p' <<<"$route_line" | head -1)
if [[ -z "$egress_dev" ]]; then
  echo "FAIL: could not determine egress interface for $HUB_PUBLIC_IP (route: $route_line)" >&2
  exit 1
fi
if [[ "$egress_dev" == tailscale0 ]]; then
  echo "FAIL: route to $HUB_PUBLIC_IP goes out tailscale0 ($route_line) -- this vantage point cannot prove non-Tailscale reachability; run from a host whose route to the public IP is NOT the tailscale mesh" >&2
  exit 1
fi

curl_pinned=(curl -sS -m 8 --interface "$egress_dev")

headers=$(mktemp)
trap 'rm -f "$headers"' EXIT

init_body='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"'"$PROTO"'","capabilities":{},"clientInfo":{"name":"hub-deploy-ac4-public","version":"0.0.0"}}}'

"${curl_pinned[@]}" -D "$headers" -X POST "$URL" \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -d "$init_body" >/dev/null
init_rc=$?
if [[ $init_rc -ne 0 ]]; then
  echo "FAIL: curl to $URL via dev $egress_dev failed (rc=$init_rc) -- public path is NOT reachable" >&2
  exit 1
fi

session=$(grep -i '^mcp-session-id:' "$headers" | sed 's/.*: //' | tr -d '\r')
if [[ -z "$session" ]]; then
  echo "FAIL: no mcp-session-id from $URL via dev $egress_dev (endpoint unreachable or handshake failed)" >&2
  exit 1
fi

"${curl_pinned[@]}" -X POST "$URL" \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -H "Mcp-Session-Id: $session" \
  -H "Mcp-Protocol-Version: $PROTO" \
  -d '{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}' -o /dev/null

list=$("${curl_pinned[@]}" -X POST "$URL" \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -H "Mcp-Session-Id: $session" \
  -H "Mcp-Protocol-Version: $PROTO" \
  -d '{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}')

if ! grep -q '"name":"search_pets"' <<<"$list"; then
  echo "FAIL: search_pets missing from tools/list via public path: $list" >&2
  exit 1
fi
if ! grep -q '"name":"get_pet"' <<<"$list"; then
  echo "FAIL: get_pet missing from tools/list via public path: $list" >&2
  exit 1
fi

result=$("${curl_pinned[@]}" -m 15 -X POST "$URL" \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -H "Mcp-Session-Id: $session" \
  -H "Mcp-Protocol-Version: $PROTO" \
  -d '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"search_pets","arguments":{"species":"dog","limit":3}}}')

if grep -qi '"iserror":true' <<<"${result,,}"; then
  echo "FAIL: search_pets call errored via public path: $result" >&2
  exit 1
fi
if ! grep -q 'cdn.rescuegroups.org' <<<"$result"; then
  echo "FAIL: no hotlinked photo URL in live search_pets result via public path: $result" >&2
  exit 1
fi

echo "OK: $HUB_PUBLIC_ADDR is reachable via a confirmed non-Tailscale route (dev=$egress_dev); tools/list + live search_pets both succeeded through it"
