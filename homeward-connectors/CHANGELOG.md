# Changelog

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
