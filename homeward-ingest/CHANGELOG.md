# Changelog

## v0.2.3 — 2026-09-13

Symlinks `homeward-ingest/Cargo.lock` to the workspace-root lock file.
`supply-audit`/`license-audit`/`sbom` were erroring `read .../Cargo.lock:
No such file or directory` because a workspace member has no lock file of
its own — this is the actual reason v0.2.1's onboarding (proof-lanes.toml +
run-metrics.sh) did not close the gate gap it claimed to close. Verified
directly: all three producers now write `verdict: "pass"` receipts for
homeward-ingest (380 deps scanned, 0 advisories, 0 license violations).

## v0.2.2 — 2026-09-13

Fixes homeward-ingest/agent/proof-lanes.toml to the [[lane]] schema the
installed autobuilder's vti-plan actually parses (homeward-schema's own
file, used as the original model, predates this schema and fails the
same way) — verified vti-plan now reports verdict=pass.

## v0.2.1 — 2026-09-13

Onboards homeward-ingest to autobuilder's full producer set: adds
agent/proof-lanes.toml (every src/*.rs and tests/*.rs path mapped to a
verification lane) and scripts/run-metrics.sh (copied from
homeward-schema), closing the gap that made homeward-ingest-backfill's
gate block on supply-audit/license-audit/sbom no-receipt three times in a
row with no stable baseline possible.

## v0.2.0 — 2026-09-12

homeward's DB holds ~9.7k animals while RescueGroups reports ~62k available: the ingest cursor started 2026-06-13 and the historical population was never backfilled. This ships `homeward-ingestd backfill` — an idempotent, resumable command that walks the full RG population (page-based pagination, honoring the known offset-pagination breakage), reconciles counts per source, and enrolls backfilled animals in the embed index.
