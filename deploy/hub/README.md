# deploy/hub — the live constellation hub instance

Verbatim copies of what runs pawsandpetals.org on the constellation hub
(Hetzner cpx42, Ubuntu, user `jsy`). The generic `deploy/` set one level up
is the portable fleet layout; this directory is the one specific deployment,
captured so the site is reproducible from git.

## Layout

| path | lands at (on hub) |
|---|---|
| `systemd/homeward-{embed,ingest,report,wall}.service` | `~/.config/systemd/user/` (user units, `WantedBy=default.target`) |
| `systemd/homeward-backfill-legacy.service` | same; one-shot, exited SUCCESS 2026-08-17, kept for re-runs |
| `caddy/Caddyfile` | `/etc/caddy/Caddyfile` (system Caddy, TLS for apex/www + `stream.` subdomain) |
| `scripts/backfill_legacy_enroll.py` | `~/.local/bin/` |
| `homeward.env.sample` | `~/.config/homeward/homeward.env` (600; holds the RescueGroups key) |
| `placement.toml.fragment` | lines merged into `~/.config/wintermute/placement.toml` |

Binaries: `homeward-ingestd`, `homeward-reportd`, `homeward-walld` in
`~/.local/bin/`. Embed sidecar: `~/homeward-embed/` venv (CPU torch,
`HW_EMBED_MODEL=large`, 1024-d) with `yolov8n.pt` in the working directory —
`WorkingDirectory` must be that dir or YOLO silently re-downloads.

## Differences from the generic `deploy/` units

- `ExecCondition=wm-node should-run <name>` on ingest/report (fleet placement guard)
- `WantedBy=default.target` instead of `homeward.target`
- embed runs from a venv, not `uv run`; `MemoryHigh=4G`
- report listens on 8080 (unit flag overrides the env 8081) and reads an optional `messaging.env` for relay/SMTP
- embed sidecar on 127.0.0.1:8741
- wall service (port 8090) exists only here

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
