# Changelog

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
