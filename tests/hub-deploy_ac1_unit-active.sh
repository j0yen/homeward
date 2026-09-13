#!/usr/bin/env bash
# AC1 (PRD-homeward-mcp-hub-deploy): Given the applied unit, When
# `systemctl --user status homeward-mcp` runs on the hub, Then it reports
# active/running.
#
# Real-environment check -- SSHes to the live constellation hub (Tailscale
# name `hub`) and asks systemd directly. No fixtures: a hub that is
# unreachable or a unit that isn't active/running is a real failure here.
set -uo pipefail

HUB="${HUB_HOST:-hub}"

status=$(ssh -o BatchMode=yes -o ConnectTimeout=8 "$HUB" \
  'systemctl --user is-active homeward-mcp' 2>&1)
rc=$?

if [[ $rc -ne 0 || "$status" != "active" ]]; then
  echo "FAIL: homeward-mcp is not active on $HUB (is-active said: $status)" >&2
  exit 1
fi

echo "OK: homeward-mcp.service is active on $HUB"
