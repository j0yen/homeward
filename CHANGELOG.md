# Changelog

## v0.8.0 — 2026-06-05

homeward-connectors: source-connector framework + RescueGroups (JSON:API v5) and municipal Socrata (Austin/Dallas/Sonoma/Long Beach) connectors. Polite conditional-request HTTP core (304 no-op, identifying UA, per-host backoff), PhotoRef stores source URLs only (never raw image bytes), normalizes dogs and cats into PetRecord. CLI `homeward connectors poll <name>`. All 7 ACs green (29 tests).

## v0.7.0 — 2026-06-05

Freshness engine: sqlite-backed canonical store, AIMD-cadence orchestrator, entity-resolution dedup, two-strikes departure detection, TTL expiry, and delete-org ToS hook — all clippy gates green, 21 tests passing.

## v0.6.0 — 2026-06-05

Fix all clippy pedantic+nursery warnings in homeward-report; all 28 tests pass

## v0.5.0 — 2026-06-05

homeward-match: fusion matching crate — prefilter + visual kNN + calibrated scoring (AC1-7 green, 28 tests)

## v0.4.0 — 2026-06-05

Adds homeward-ingest: sqlite-backed canonical store, AIMD-cadence multi-connector
orchestrator, source_animal_id dedup, two-strikes + TTL departure detection,
IngestEvent bus interface, and homeward-ingestd CLI (run/stats/get). All 7 ACs
green (21 unit tests).

## v0.3.0 — 2026-06-05

Add `homeward-report` crate: owner-side lost-pet reports with EXIF stripping,
brokered contact tokens, 90-day TTL/CCPA delete, continuous match alerts with
dedup + candidate-framing, stray-hold reclaim deadlines, and the open read API
serving shelter intakes (rate-limited, zero LostReport PII). 28 tests green.
