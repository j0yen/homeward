# Changelog

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
