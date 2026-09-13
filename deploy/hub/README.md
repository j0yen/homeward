# deploy/hub — the live constellation hub instance

Verbatim copies of what runs pawsandpetals.org on the constellation hub
(Hetzner cpx42, Ubuntu, user `jsy`). The generic `deploy/` set one level up
is the portable fleet layout; this directory is the one specific deployment,
captured so the site is reproducible from git.

## Layout

| path | lands at (on hub) |
|---|---|
| `systemd/homeward-{embed,ingest,report,wall}.service` | `~/.config/systemd/user/` (user units, `WantedBy=default.target`) |
| `systemd/homeward-mcp.service` | same; applied 2026-09-13 (PRD-homeward-mcp-hub-deploy) — streamable-HTTP MCP server, `--http :8095`, binds `0.0.0.0` |
| `systemd/homeward-backfill-legacy.service` | same; one-shot, exited SUCCESS 2026-08-17, kept for re-runs |
| `caddy/Caddyfile` | `/etc/caddy/Caddyfile` (system Caddy, TLS for apex/www + `stream.` subdomain) |
| `scripts/backfill_legacy_enroll.py` | `~/.local/bin/` |
| `homeward.env.sample` | `~/.config/homeward/homeward.env` (600; holds the RescueGroups key) |
| `placement.toml.fragment` | lines merged into `~/.config/wintermute/placement.toml` |

Binaries: `homeward-ingestd`, `homeward-reportd`, `homeward-walld`,
`homeward-mcp` in `~/.local/bin/`. Embed sidecar: `~/homeward-embed/` venv
(CPU torch, `HW_EMBED_MODEL=large`, 1024-d) with `yolov8n.pt` in the working
directory — `WorkingDirectory` must be that dir or YOLO silently re-downloads.

## Differences from the generic `deploy/` units

- `ExecCondition=wm-node should-run <name>` on ingest/report/mcp (fleet placement guard)
- `WantedBy=default.target` instead of `homeward.target`
- embed runs from a venv, not `uv run`; `MemoryHigh=4G`
- report listens on 8080 (unit flag overrides the env 8081) and reads an optional `messaging.env` for relay/SMTP
- embed sidecar on 127.0.0.1:8741
- wall service (port 8090) exists only here
- mcp service (port 8095, streamable-HTTP `/mcp`, `GET /healthz`) exists only here; reachable both over Tailscale and on the hub's public IP (no Caddy route yet — see "MCP verification" and "Supported address for external/mcphost callers" below)

## MCP verification (homeward-mcp, applied 2026-09-13)

`tests/hub-deploy_ac{1,2,3}_*.sh` at the repo root re-run the real checks
used to verify this deploy: `systemctl --user is-active` on the hub,
`tools/list` from an external host (RedBaron/carbon over Tailscale,
`100.66.158.49:8095`), and a live `search_pets` call. All three were green
against the real hub at ship time. Known gap surfaced by that verification:
every live result's `city_county`/`state` is null because
`homeward-connectors`' RescueGroups mapper never populates `location` at
all (0/185374 rows) — tracked in `PRD-homeward-ingest-location-backfill-missing.md`,
not a defect in this deploy.

`tests/hub-deploy_ac4_public-reachability.sh` closes a gap those three left:
Tailscale-peer reachability (`100.66.158.49:8095`) is a real but *weaker*
bar than what mcphost.dev's tenant tool-execution sandbox (2.28.40.4)
actually needs, and mcphost.dev is not a Tailscale peer. That sandbox
timed out against the Tailscale address entirely; the hub's **public**
address, `178.105.64.66:8095`, is the one that actually works from
mcphost.dev and is what every mcphost-tool wrapping of homeward-mcp must
target (see PRD-homeward-mcp-hub-deploy-public-reachability-test-gap.md).
AC4 pins its own egress route away from `tailscale0` before trusting a
green result, so it fails loudly — not silently — if a future firewall
change ever closes the public port.

## Supported address for external/mcphost callers

**Use `178.105.64.66:8095` (public IP), not `100.66.158.49:8095`
(Tailscale), when wrapping homeward-mcp as a tool for a non-Tailscale
caller such as an mcphost.dev tenant.** The Tailscale address only works
for callers inside the fleet's Tailscale mesh (RedBaron/carbon/hub/ryzen7).
There is no Caddy/TLS route yet for either address — both are plain HTTP
directly to port 8095.

## Deploy recipe (binaries)

1. Build on a box whose glibc ≤ 2.39 symbols (hub is Ubuntu 24.04);
   check `objdump -T <bin> | grep GLIBC`.
2. `rsync -z --partial` to the hub, `sha256sum` both sides.
3. `install -o jsy -g jsy -m 755 <bin> /home/jsy/.local/bin/`
4. `systemctl --user -M jsy@ restart homeward-<svc>`
5. Verify a fresh `polled source=rescuegroups count=N` line from the NEW pid:
   `journalctl _UID=$(id -u jsy) --since "5 min ago"`.

## Caddy / Cloudflare

DNS is proxied (orange-cloud) for apex+www; `stream.pawsandpetals.org` is
DNS-only because SSE stalls through the Cloudflare proxy on HTTP/2. Cache
rules bypass `/api/stream` and `/health`. The `header_down` directive uses the
single Set form — Add+Delete pairs run Add first and wipe the header.
