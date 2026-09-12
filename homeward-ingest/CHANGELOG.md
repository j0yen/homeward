# Changelog

## v0.2.0 — 2026-09-12

homeward's DB holds ~9.7k animals while RescueGroups reports ~62k available: the ingest cursor started 2026-06-13 and the historical population was never backfilled. This ships `homeward-ingestd backfill` — an idempotent, resumable command that walks the full RG population (page-based pagination, honoring the known offset-pagination breakage), reconciles counts per source, and enrolls backfilled animals in the embed index.
