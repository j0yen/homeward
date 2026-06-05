# homeward-embed

Photo-embedding pipeline for homeward shelter-pet similarity search.

## Architecture

1. **Detector** — YOLO (COCO weights) crops the largest dog/cat detection.
   Falls back to whole-image on no detection (logged, not dropped).
2. **Embedder** — DINOv2 ViT-B/14 (Apache-2.0) → 768-d L2-normalized vector.
   Use `HW_EMBED_MODEL=small` for ViT-S/14 (384-d, faster on CPU-only boxes).
3. **Index** — HNSW (hnswlib, Apache-2.0) disk-persisted gallery; append-only.
4. **Service** — FastAPI localhost HTTP sidecar; Rust callers use it via HTTP.

## License

All model weights used by this service are permissively licensed:
- DINOv2 (Apache-2.0) — `facebook/dinov2-base`, `facebook/dinov2-small`
- YOLO / ultralytics (AGPL-3.0 + commercial option) — COCO weights

**NOT bundled, NOT supported:**
- MegaDescriptor weights — CC-BY-NC (non-commercial only)
- PetFace fine-tuned weights — research-gated

## Usage

```bash
uv run homeward-embed-svc   # starts on 127.0.0.1:8741

# Enroll an intake photo
curl -X POST http://127.0.0.1:8741/enroll \
  -H 'Content-Type: application/json' \
  -d '{"canonical_id":"01HZ...","image_url":"https://...","species":"dog"}'

# Query with a lost-pet photo
curl -X POST http://127.0.0.1:8741/query \
  -H 'Content-Type: application/json' \
  -d '{"image_url":"https://...","k":10,"species_filter":"dog"}'
```

## Eval harness

```bash
uv run python -m homeward_embed.eval \
  --dataset-dir /path/to/petface-holdout \
  --embed-variant base \
  --k 1 5 20
```

The harness **errors out** if any query individual-ID is present in the gallery
(anti-tautology guard). Datasets must be downloaded separately — PetFace and
Flickr-Dog are research-gated and not bundled.

## Environment variables

| Variable | Default | Description |
|---|---|---|
| `HW_EMBED_MODEL` | `base` | DINOv2 variant: `small` (384-d) or `base` (768-d) |
| `HW_EMBED_INDEX_DIR` | `~/.local/share/homeward/embed-index` | Index storage path |
| `HW_EMBED_PORT` | `8741` | Service port |
| `HW_EMBED_HOST` | `127.0.0.1` | Service bind address |

## Memory / performance notes

- 100k × 768-d HNSW index ≈ 300 MB RAM (ViT-B)
- 100k × 384-d HNSW index ≈ 150 MB RAM (ViT-S)
- Query latency dominated by one DINOv2 forward pass (~0.5–2s CPU)
- Gallery enrollment is amortized: embed each intake once at intake time
