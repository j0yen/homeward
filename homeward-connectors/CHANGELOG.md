# Changelog

## v0.2.4 — 2026-08-13

Fixes a production HTTP 500 (`{"errors":[{"status":500,"title":"System
error","detail":"We encountered a system error and couldn't continue."}]}`)
on delta polls, the last link in the rescuegroups fix chain. Verified live:
a space-separated, non-RFC3339 criteria timestamp (e.g.
`"2026-08-13 02:44:31"`) 500s; RFC3339 with a trailing `Z` (with or without
fractional seconds) returns 200. `build_filters` now formats the cursor
timestamp with `to_rfc3339_opts(SecondsFormat::Secs, true)` instead of a
bespoke `%Y-%m-%dT%H:%M:%S` strftime pattern, producing e.g.
`"2026-08-13T02:44:31Z"` — matching the verified-good form exactly. Added a
dedicated unit test asserting the criteria is never space-separated, always
contains 'T', ends with 'Z', and parses as RFC3339; extended the delta-poll
integration test to assert the same against the actual POST body sent.

## v0.2.3 — 2026-08-13

Fixes a production HTTP 400 on every delta (second-and-later) rescuegroups
poll: `{"errors":[{"source":{"pointer":"/data/filters/0/fieldName/updatedDate"},
"title":"Invalid field","detail":"updatedDate is not a valid filter
field"}]}`. Verified live: the real API namespaces filterable animal fields
under the resource type — `animals.updatedDate` returns 200, bare
`updatedDate` 400s. `fetch_page`'s filter construction is now factored into
`build_filters`, which sends `animals.updatedDate`. This is currently the
only filter fieldName the connector sends, so it's the only one that needed
the prefix. Added unit tests for `build_filters` (full-poll = empty,
delta-poll = namespaced field) and an integration test asserting the actual
POST body sent on a delta poll.

## v0.2.2 — 2026-08-12

Fixes the RescueGroups connector's "error decoding response body" that
appeared in production after v0.2.1 fixed the 404. The `/public` path was
right, but the deserialization structs still assumed a synthetic shape
that doesn't match the real v5 JSON:API payload:

- `meta.count`, not `meta.totalRecords`.
- `id` is a JSON string, not a number (already correct, kept string).
- `breedPrimary`/`breedSecondary`/`sizeGroup`/`descriptionText`/`createdDate`
  replace the old (never-real) `primaryBreed`/`secondaryBreed`/
  `sizeDescription`/`description`/`pubDate` field names.
- `species` is not an attribute at all — the connector already knows which
  species it queried (one request per species), so that's passed straight
  through instead of parsed from the payload.
- `colors` and `pictures` are JSON:API relationships (reference `type`+`id`
  only); the full resource data lives in the top-level `included[]` array
  and is now resolved via a `(type, id)` lookup built per page.

Added two live-captured sample payloads (`rg-sample-dogs.json`,
`rg-sample-cats.json`, 3 records each, public API data) as regression
fixtures, plus a deserialization test running them end-to-end through
`poll()` and asserting populated PetRecords (species, breed, photos,
colors) per species.

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
