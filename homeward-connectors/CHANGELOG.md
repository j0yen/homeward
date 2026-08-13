# Changelog

## v0.2.1 — 2026-08-12

Fixes the RescueGroups connector's 404 against the live v5 API. The
`search/available` endpoint lives under `/public` and only accepts one
species segment per request — the old `dogs,cats` comma-joined GET 404'd.
`fetch_page` now issues one POST per species (`dogs`, then `cats`) to
`/public/animals/search/available/{species}`, with filters in a JSON:API
body (`Content-Type: application/vnd.api+json`) instead of query-string
brackets, matching the endpoint's actual contract. `poll` merges both
species' pages and dedups by animal ID. Added `build_search_url` unit
tests plus an integration test asserting exactly one POST per species with
no comma-joined path.

## v0.2.0 — 2026-06-05

Integration tests covering all 7 ACs using wiremock mock HTTP server:
- AC1: 304 Not Modified returns empty result without error
- AC2: RescueGroups JSON:API v5 normalizes species/breeds/photo URLs/last_seen; provenance=api
- AC3: Socrata SODA normalizes STRAY intake_type, found_location, chip_status; provenance=open-data
- AC4: Mixed dog+cat fixtures yield both species from both connectors
- AC5: Polite HTTP sends User-Agent, If-None-Match, If-Modified-Since; per-host rate limit engaged
- AC6: PhotoRef carries only source URLs (no raw bytes) — type-enforced
- AC7: Unknown connector name returns clear error; registry poll outputs valid JSON

# Changelog — homeward-connectors

## v0.1.0 (2026-06-04)

Added `homeward-connectors` crate to the homeward workspace. Implements the
source-connector framework (polite HTTP core, `Connector` trait, `ConnectorRegistry`)
plus two working connectors: `RescueGroupsConnector` (JSON:API v5, `IntakeType::Adoptable`)
and `SocrataConnector` (generic SODA client pre-configured for Austin, Dallas, Sonoma,
and Long Beach municipal shelters). All records normalize into `homeward-schema::PetRecord`.
14 unit tests pass against fixture JSON. Live network calls are not made in tests.
