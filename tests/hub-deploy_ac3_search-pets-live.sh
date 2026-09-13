#!/usr/bin/env bash
# AC3 (PRD-homeward-mcp-hub-deploy): Given the running unit, When
# `search_pets` is called against the live DB, Then results return with
# hotlinked photo URLs and brokered contact (same legal-ethics contract as
# the server PRD's AC2).
#
# KNOWN GAP (filed as a finding PRD, see homeward/tests/ sibling scripts'
# header convention and build-queue/PRD-homeward-ingest-location-backfill-
# missing.md): as of 2026-09-13 zero of ~185k live canonical_records carry
# ANY location data (homeward-ingest never populated `location`), so
# `city_county`/`state` are legitimately null on every live result -- a
# pre-existing homeward-ingest gap, not a hub-deploy defect. This check
# therefore asserts the two properties the live data CAN prove (hotlinked
# photo, brokered contact, no raw PII) rather than asserting non-null
# location, which would be a false claim against real data today.
set -uo pipefail

HUB_ADDR="${HOMEWARD_MCP_ADDR:-100.66.158.49:8095}"
URL="http://${HUB_ADDR}/mcp"
PROTO="2025-06-18"

headers=$(mktemp)
trap 'rm -f "$headers"' EXIT

init_body='{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"'"$PROTO"'","capabilities":{},"clientInfo":{"name":"hub-deploy-ac3","version":"0.0.0"}}}'

curl -sS -m 8 -D "$headers" -X POST "$URL" \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -d "$init_body" >/dev/null

session=$(grep -i '^mcp-session-id:' "$headers" | sed 's/.*: //' | tr -d '\r')
if [[ -z "$session" ]]; then
  echo "FAIL: no mcp-session-id from $URL" >&2
  exit 1
fi

curl -sS -m 8 -X POST "$URL" \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -H "Mcp-Session-Id: $session" \
  -H "Mcp-Protocol-Version: $PROTO" \
  -d '{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}' -o /dev/null

result=$(curl -sS -m 15 -X POST "$URL" \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -H "Mcp-Session-Id: $session" \
  -H "Mcp-Protocol-Version: $PROTO" \
  -d '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"search_pets","arguments":{"species":"dog","limit":3}}}')

if grep -qi '"iserror":true' <<<"${result,,}"; then
  echo "FAIL: search_pets call errored: $result" >&2
  exit 1
fi
if ! grep -q 'cdn.rescuegroups.org' <<<"$result"; then
  echo "FAIL: no hotlinked photo URL in live search_pets result: $result" >&2
  exit 1
fi
if ! grep -q 'brokered-via:' <<<"$result"; then
  echo "FAIL: no brokered shelter_contact in live search_pets result: $result" >&2
  exit 1
fi
if grep -qE '"phone"|"email"|@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}' <<<"$result"; then
  echo "FAIL: possible raw PII leaked in search_pets result: $result" >&2
  exit 1
fi

echo "OK: live search_pets returns hotlinked photos + brokered contact, no raw PII"
echo "NOTE: city_county/state are null in live data today (0/185374 records carry location -- homeward-ingest gap, filed separately, not this PRD's scope)"
