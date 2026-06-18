# Changelog

## v0.33.0 — 2026-06-18

Adds `homeward-geocode`: offline found-location geocoder. Resolves
`PetRecord.found_location_text` (ZIP, "City, ST", "City ST", prefix forms) to a
coarse `ShelterLocation` using a bundled public-domain US Census Gazetteer — no
network access. Enrichment fills `location` only when absent, is provenance-tagged
as `LocationSource::FoundText`, and is idempotent. All 10 acceptance criteria pass.

## v0.32.0 — 2026-06-18

Adds `discover --from-holes` mode: reads a JSON list of CoverageHole objects
(from homeward-catchment-geo or a fixture), maps each hole to catalog query
terms via its label, reuses the existing Socrata/ODS catalog crawl, and emits
HoleCandidateRow results ranked by estimated_missed_population then score.
Strictly propose-only — never writes sources.toml. Also fixes 3 pre-existing
probe.rs/opendatasoft.rs compilation and test bugs found during build.

## v0.31.0 — 2026-06-18

Stray-aware AIMD cadence in homeward-ingest orchestrator.

Changed adapt() from flat count to PollOutcome{total,stray}; added is_stray_class()
classifying Stray+FoundReport as lost-pet-relevant. New rules: stray burst triggers
hot window (fast polling at stray_floor); adoptable-only churn no longer pins cadence
at floor. Hot window decays over configurable tick count. CadenceConfig with env
overrides (HW_STRAY_FLOOR_THRESHOLD/SECS/HOT_WINDOW_TICKS), hard 10s lower bound on
stray_floor. SourceStatus surface exposes stray_seen + hot_ticks_remaining per source.
All 9 AC tests pass. Also fixed two pre-existing baseline bugs (probe.rs missing brace,
opendatasoft.rs Json→Http error mapping).

## v0.30.0 — 2026-06-18

Adds geo coverage rollup to homeward-connectors: bins PetRecord.location
into 0.5° equal-angle grid cells, computes per-cell record/stray/source counts,
and detects coverage holes (populated regions with no feed) ranked by estimated
missed population. Seeded from documented-gaps list. Purely additive to
CoverageReport (cells/geo_holes/ungeocoded fields, skip_serializing_if=None when
--geo not requested). CLI: `homeward connectors coverage --geo [--min-count N]`.
Also fixes 2 pre-existing compile errors: probe.rs missing closing brace for
run_arcgis_probe, opendatasoft.rs wrong error type in map_err.

## v0.29.0 — 2026-06-15

OpenDataSoft Explore API v2.1 connector: `OpenDataSoftConnector` implementing
the `Connector` trait with ODSQL where-clause filtering, timestamp-cursor
incremental polling, and stray-only normalization. Probe extended with
`--family opendatasoft` emitting `[[opendatasoft]]` TOML blocks. All 93 tests
pass. Reuses `PoliteClient`, `OpenDataSoftConfig` from catalog, and shared
`STRAY_VALUES` set.

## v0.28.0 — 2026-06-15

discover subcommand crawls Socrata+ODS federated catalogs for animal-intake dataset candidates

## v0.27.0 — 2026-06-15

ArcGisConnector implementing Connector trait for ArcGIS REST Feature Service with paging, probe extension

## v0.26.0 — 2026-06-15

multi-family catalog: SourceCatalog deserializes [[socrata]]/[[opendatasoft]]/[[arcgis]] entries; OpenDataSoftConfig+ArcGisConfig defined; load_catalog dispatches per family; ConnectorError::FamilyNotBuilt for unbuilt families

## v0.25.0 — 2026-06-14

homeward-report-upload: POST /uploads endpoint — owners can upload a pet photo via multipart/form-data instead of supplying an external hotlink URL. EXIF metadata (including GPS) is stripped via the existing JPEG marker-level stripper before storage. Files saved as `<ULID>.jpg` under `HW_UPLOAD_DIR` (default `~/.local/share/homeward/uploads/`, created on startup). Response: `{ "url": "/uploads/<filename>" }`. GET /uploads/<filename> serves stored photos via tower-http ServeDir. AC3: 413 on body > HW_UPLOAD_MAX_BYTES (default 10 MiB). AC4: 415 on non-image content-type. AC5: POST /reports accepts photo_url from uploads, stored in photos[0].url on LostReport. AC6: HW_UPLOAD_DIR env var controls upload directory. 6 new AC tests; 84 total tests green.

## v0.24.0 — 2026-06-15

Adds webhook-based owner notification to homeward-reportd (v0.23.0→v0.24.0).

Changes:
- `LostReport` gains `notify_url: Option<String>` in homeward-schema
- `POST /reports` validates notify_url (http/https only), returns 400 on bad URL
- New `WebhookSink` in `homeward-report/src/webhook.rs` uses reqwest with rustls-tls
- `WebhookSink` wired into `AppState` and `MatchWatcher`
- `MatchWatcher::process_report` calls `webhook.fire()` after each new `MatchAlert`
  for reports with notify_url — best-effort fire-and-forget, no retry, non-2xx logged at WARN
- 9 new tests: 5 webhook unit tests + AC1 server tests (valid/invalid/ftp/absent notify_url + stored)
- All 78 homeward-report lib tests + full workspace tests pass

## v0.23.0 — 2026-06-14

homeward-matches-endpoint: `GET /reports/:id/matches` HTTP endpoint — owners can query match alerts logged for their lost-pet report; returns list of {candidate_id, score, shelter_area, source_url, reclaimable_until, alerted_at}; 404 for unknown report; empty list when no alerts yet; AlertLog wired into AppState and MatchWatcher

## v0.22.0 — 2026-06-14

homeward-report-submit: `POST /reports` and `GET /reports/:id` HTTP endpoints — owners can submit lost-pet reports (JSON body → ULID report_id, species parse, BrokeredContactToken mint, CoarseLocation) and retrieve them by ID; 409 on duplicate, 404 on missing; 4 new handler tests green

## v0.21.0 — 2026-06-14

homeward-match-watch: background match-watch task — polls active lost reports every HOMEWARD_MATCH_INTERVAL seconds, delivers MatchAlerts for new intake matches via DeliveryLedger with dedup

## v0.20.0 — 2026-06-14

Wire reportd to ingest SQLite DB. IngestDbReader loads 174K+ shelter animals on startup; GET /intake returns live data; GET /coverage is DB-backed.

## v0.19.0 — 2026-06-14

homeward-web-ui v0.19.0 — single-page web UI served by reportd; drag-and-drop pet photo search with ranked shelter cards; updated POST /search response schema with embed_available flag

## v0.18.1 — 2026-06-14

build and install homeward-ingestd binary; homeward-ingest.service now starts cleanly

## v0.18.0 — 2026-06-14

wired POST /search to embed sidecar with EXIF strip, graceful degradation, and --no-embed flag

## v0.17.0 — 2026-06-14

`homeward-reportd serve [--port PORT] [--bind ADDR]`: axum HTTP server exposing the existing query API over the network. Four endpoints: `GET /health`, `GET /coverage`, `GET /intake` (shelter query with species/zip/state filters, capped at 50 results), `POST /search` (photo upload → ranked candidates). No LostReport PII exposed. homeward-report v0.3.0.

## v0.16.0 — 2026-06-14

`homeward-connectors probe <domain> <dataset_id>`: hits SODA metadata + a one-row
sample, decides whether the dataset is a usable STRAY-bearing animal-intake feed,
and emits either a draft `sources.toml` entry (best-guess column mapping) or an
honest red verdict explaining why it isn't usable. Onboarding becomes probe →
review → commit instead of manual archaeology.

## v0.15.0 — 2026-06-13

homeward-coverage-report: iter-1 scaffold — coverage subcommand with LIVE/STALE/SILENT/UNKNOWN statuses, --json output, fixture tests green

## v0.14.0 — 2026-06-13

homeward-source-catalog: deploy/sources.toml (6 cities), CATCHMENT.md, catalog_load_test, homeward.env.sample, wrapper wired

## v0.13.0 — 2026-06-13

homeward-source-registry: make Socrata sources loadable from HOMEWARD_SOURCES toml; four built-ins unchanged as fallback

## v0.12.0 — 2026-06-13

Real DINOv2 round-trip attest — prove owner round-trip by enrolling fixture photos and verifying rank-1 self-match with real embedding.

## v0.11.0 — 2026-06-13

Wire ingest daemon to call EmbedClient.enroll() for each intake photo via delta events, populating the vector gallery that the matcher searches.

## v0.10.0 — 2026-06-13

Wire stored lost-report photo through embed sidecar and homeward-match fusion into a real ranked shortlist, replacing hardcoded stubs.

## v0.10.0 — homeward-alert-delivery — 2026-06-13

Adds the last-mile delivery pipeline to `homeward-report`: `Deliverer` trait with `DryRunDeliverer` default (renders the would-send message, records a `DryRun` ledger entry, transmits nothing), `RelayEmailDeliverer` that degrades to dry-run when `HOMEWARD_RELAY_ENDPOINT`/credential are unset (never errors, never leaks a raw address), and an append-only delivery ledger (`DeliveryLedger`) with per-attempt records `{alert_id, report_id, deliverer, outcome, ts}`. Dedup on `alert_id` ensures at-most-once delivery. Two new `reportd` subcommands: `deliver --report <id> [--dry-run]` (full generate→deliver→ledger path) and `alerts-log` (prints ledger). All rendered messages preserve candidate-not-confirmation framing and contain no raw phone/email. 43 tests green.

## embed-provision — 2026-06-13

Adds `homeward-embed warmup` and `homeward-embed smoke` CLI subcommands. `warmup` prefetches DINOv2-small weights into a pinned `HF_HOME` cache dir and exits 0; a second run with `HF_HUB_OFFLINE=1` confirms no network dependency after provisioning. `smoke` enrolls bundled fixture images, asserts rank-1 self-match and discriminative ordering, and records/prints query wall-clock latency. Committed synthetic PNG fixtures (solid-colour squares) for offline CI use; `SOURCES.md` documents CC-licensed real photo URLs. `service.py` now calls `_run_warmup()` at startup so first `/enroll` never blocks on a model download. 25 unit tests + 1 integration test (skip-guarded) pass.

## v0.9.3 — 2026-06-12

Adds PetFbiConnector pulling lost/found reports from Pet FBI + Helping Lost Pets (HeLP) network via the report widget feed. Maps type=lost → LostReport, type=found/sighting → PetRecord, with honest HeLP vs PetFbi source attribution. Connector registers only when HOMEWARD_PETFBI_DATA_FILE is set.

## v0.9.2 — 2026-06-12

Extends homeward-ingest dedup.rs with federated_merge: reconciles Pet FBI / HeLP found/stray records against shelter intakes using species + geo + date-window + perceptual-hash guards. Conservative thresholds bias toward false-split over false-merge. Merged records carry both provenances. Lost-report pairs never merge.

## v0.9.1 — 2026-06-12

Adds homeward-report export module: LostReportExport JSON-LD serializer (schema.org + homeward namespace), deterministic text flyer renderer, and Syndicator trait. LocalArtifactSyndicator writes both artifacts. Gated channels (PawBoost/Nextdoor/Facebook/Petco) return ManualOnly with documented reason — no fictional transports. PetFbiPartnerSyndicator behind feature flag, dry-run by default.

## v0.9.0 — 2026-06-05

Photo-embedding pipeline: YOLO body-crop → DINOv2 ViT-B/14 embedding (Apache-2.0) → L2-normalized 768-d vector → HNSW vector index for sub-second kNN pet matching. Includes a localhost HTTP sidecar (enroll/query/reembed_all), a Rust embed_client in homeward-ingest, and an honest eval harness with anti-tautology guard. 21 tests green; permissive-license only (MegaDescriptor/PetFace explicitly excluded).

## Recent

- homeward-embed (Rust): `homeward_ingest::embed_client` — typed async HTTP client for the Python embed sidecar (enroll/query/health_check); 10 unit tests green.

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
