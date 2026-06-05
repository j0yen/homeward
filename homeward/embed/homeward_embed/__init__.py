"""homeward-embed — photo-embedding pipeline for shelter-pet similarity search.

Architecture:
  1. Detector: YOLO (COCO weights) crops the largest dog/cat detection.
     Falls back to whole-image on no detection (logged, not dropped).
  2. Embedder: DINOv2 ViT-B/14 (Apache-2.0) → 768-d L2-normalized vector.
     ViT-S config available for CPU-only boxes (faster, ~384-d).
  3. Index: HNSW disk-persisted gallery, append-only; maps vector → canonical_id.
  4. Service: FastAPI localhost HTTP sidecar with enroll/query/reembed_all.

License discipline: only permissively-licensed weights are used.
  DINOv2 (Apache-2.0), OpenCLIP (MIT), YOLO/ultralytics (AGPL/commercial).
  MegaDescriptor (CC-BY-NC) and PetFace weights are NOT bundled.
"""

__version__ = "0.1.0"
