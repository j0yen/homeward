# homeward

Aggregates shelter and lost-pet listings from many incompatible sources into one canonical record, then matches an owner's lost pet against current intakes by structure and by photo.

## The problem

A dozen sources describe the same dog or cat in a dozen shapes — RescueGroups JSON:API, municipal Socrata columns, ArcGIS feeds, vendor APIs — and none of them agree on a schema, a freshness model, or what "available" means. An owner whose pet is missing can't watch them all. By the time a listing surfaces in the one place they happened to check, the hold window may have closed.

homeward closes that gap. It normalizes every source into one record type, keeps that view fresh as shelters intake and adopt out, and lets an owner file a single lost report that is matched continuously against everything coming in — by breed, age, color, and location, and by visual similarity of the photos themselves.

## How it works

A record flows through the workspace's crates in order:

| Crate | Role |
| --- | --- |
| `homeward-schema` | The canonical types every source normalizes into — `PetRecord`, `LostReport`, `Provenance` — plus their validation and serde. The vocabulary the rest of the fleet speaks. |
| `homeward-connectors` | Source connectors (RescueGroups, Socrata, OpenDataSoft, ArcGIS) and the `homeward` CLI that polls them, reports coverage, and discovers new feeds. |
| `homeward-geocode` | Resolves a record's free-text found-location to a coarse location offline, against a bundled US Census gazetteer. No network. |
| `homeward-ingest` | The freshness engine: orchestrates the connectors on an adaptive cadence, deduplicates records, and detects departures via a sqlite-backed store. |
| `homeward-match` | Fusion matching: a structured prefilter, then score fusion across structured fields and photo-embedding similarity, with stray-hold flagging. |
| `homeward-embed-client` | Async HTTP client for the photo-embedding sidecar (`/enroll`, `/query`, `/health`). |
| `homeward-report` | Owner-side lost reports, continuous matching, owner alerts, and the open read API plus a single-page web UI. |
| `homeward/embed` | The embedding sidecar (Python): YOLO body-crop → DINOv2 ViT-B/14 → HNSW index, served over localhost HTTP. |

The design encodes a deliberate privacy and copyright posture in the types themselves: photos are stored as source URLs with attribution and *cannot* hold raw image bytes; an owner's contact is an opaque brokered token with no raw-string accessor; locations are coarsened on construction with no street-address field. The constraints live at the type level, so a careless caller can't bypass them.

## Build

```sh
cargo build --release
```

The workspace is Rust 2024, MSRV 1.86. The embedding sidecar is a separate Python service under `homeward/embed` (run with `uv`); see its README for model weights and licensing.

## Run it locally

The CLI polls a source and prints normalized records:

```sh
homeward connectors list
homeward connectors poll --since 2026-06-01T00:00:00Z --limit 20
homeward connectors coverage --geo
```

Run the whole fleet as three systemd user services — the embedding sidecar, the ingest freshness engine, and the report API:

```sh
bash deploy/install.sh    # link units, install binaries (idempotent)
homeward up               # start homeward.target
homeward status           # per-unit state + embed /health
homeward down             # stop the fleet
```

Configuration lives in `~/.config/homeward/homeward.env` (see `deploy/homeward.env.sample`) and `sources.toml`.

The live pawsandpetals.org instance on the constellation hub is captured verbatim in [`deploy/hub/`](deploy/hub/README.md).

## Status

Working multi-crate system, actively developed (v0.36.0). The matching path runs real photo-embedding fusion end to end. Accuracy is recorded honestly in `EVAL.md`: the bundled `eval-smoke` fixture proves the harness arithmetic, not a real-world accuracy number — measured accuracy against held-out datasets is reported separately there. The model weights it depends on (DINOv2, YOLO COCO) are documented in `homeward/embed/README.md`; non-commercial and research-gated weights are deliberately not bundled.

## Live match demo

[Finding Coconut](https://claude.ai/code/artifact/cf3fc099-3138-4080-84d6-672741d83770) — two real queries against the production embed index: a doctored photo of an enrolled dog (rank 1 at 0.978 with a 0.22 margin), and a never-seen Labrador that surfaces five Labrador lookalikes without a false match. Real photos, scores, and ranked candidates from the live system.

## License

MIT OR Apache-2.0 © Joe Yen
