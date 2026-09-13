#!/usr/bin/env bash
# AC2 (PRD-homeward-mcp-hub-deploy-public-reachability-test-gap): Given
# homeward's deploy documentation, When read, Then it names the public
# address (not just the Tailscale one) as the one mcphost tenant tools
# should target.
#
# Real-environment check on the doc file itself (no fixtures): fails
# loudly if the public address is missing, or if the doc still reads as
# if the Tailscale address were the supported one for external/mcphost
# callers.
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DOC="$REPO_ROOT/deploy/hub/README.md"
PUBLIC_ADDR="${HOMEWARD_MCP_PUBLIC_ADDR:-178.105.64.66:8095}"

if [[ ! -f "$DOC" ]]; then
  echo "FAIL: $DOC does not exist" >&2
  exit 1
fi

if ! grep -q "$PUBLIC_ADDR" "$DOC"; then
  echo "FAIL: $DOC does not mention the public address $PUBLIC_ADDR" >&2
  exit 1
fi

if ! grep -qi "public" "$DOC"; then
  echo "FAIL: $DOC mentions $PUBLIC_ADDR but never says 'public' -- doesn't clearly name it as the supported external/mcphost address" >&2
  exit 1
fi

echo "OK: $DOC names $PUBLIC_ADDR as the supported public address for external/mcphost callers"
