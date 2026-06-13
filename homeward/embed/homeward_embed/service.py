"""FastAPI localhost HTTP sidecar for homeward-embed.

Endpoints:
  POST /enroll           — embed + index one intake image
  POST /query            — embed query image + kNN search
  POST /reembed_all      — rebuild index from scratch (admin)
  GET  /health           — liveness probe

The sidecar is called by Rust (homeward-ingest / homeward-match) via HTTP.
No Python types leak into the Rust crates.

EXIF: stripped by the embedder before any processing or persistence.

Usage:
  uv run homeward-embed-svc
  # or:
  uv run python -m homeward_embed.service
"""

from __future__ import annotations

import io
import logging
import os
from pathlib import Path
from typing import Optional

import httpx
import numpy as np
import uvicorn
from fastapi import FastAPI, HTTPException, Response
from PIL import Image
from pydantic import BaseModel, Field

from homeward_embed.detector import AnimalDetector
from homeward_embed.embedder import ModelVariant, PhotoEmbedder
from homeward_embed.index import EmbedIndex

logger = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Request / response models
# ---------------------------------------------------------------------------


class EnrollRequest(BaseModel):
    """Enroll a new shelter animal photo into the gallery."""

    canonical_id: str = Field(..., description="Stable pet identifier (ULID).")
    image_url: str = Field(..., description="URL of the photo to enroll.")
    species: Optional[str] = Field(None, description="'dog' or 'cat' if known.")


class EnrollResponse(BaseModel):
    canonical_id: str
    internal_id: int
    detected: bool  # True if an animal was cropped, False if whole-image fallback


class QueryRequest(BaseModel):
    """Search the gallery for pets similar to a query photo."""

    image_url: Optional[str] = Field(None, description="URL of the query photo.")
    image_b64: Optional[str] = Field(None, description="Base64-encoded JPEG/PNG.")
    k: int = Field(10, ge=1, le=200)
    species_filter: Optional[str] = Field(None, description="'dog' or 'cat' or None.")


class QueryMatch(BaseModel):
    canonical_id: str
    score: float  # cosine similarity [0, 1]


class QueryResponse(BaseModel):
    matches: list[QueryMatch]


# ---------------------------------------------------------------------------
# App state
# ---------------------------------------------------------------------------

_INDEX_DIR = Path(os.environ.get("HW_EMBED_INDEX_DIR", "~/.local/share/homeward/embed-index")).expanduser()
_MODEL_VARIANT: ModelVariant = os.environ.get("HW_EMBED_MODEL", "base")  # type: ignore[assignment]

app = FastAPI(title="homeward-embed", version="0.1.0")

_detector: AnimalDetector | None = None
_embedder: PhotoEmbedder | None = None
_index: EmbedIndex | None = None


def _run_warmup(variant: str) -> None:
    """Prefetch model weights into the pinned cache dir before serving.

    Sets HF_HOME / TRANSFORMERS_CACHE so the subsequent PhotoEmbedder()
    instantiation hits the local cache rather than the network.

    If the model is already cached this is a no-op (weights load from disk).
    If HF_HUB_OFFLINE=1 is set and weights are absent, this raises — which is
    the correct behaviour: fail loudly at startup, not silently at first request.
    """
    import time  # noqa: PLC0415

    hf_home = os.environ.get("HF_HOME", str(Path.home() / ".cache" / "homeward" / "hf"))
    os.environ["HF_HOME"] = hf_home
    os.environ["TRANSFORMERS_CACHE"] = str(Path(hf_home) / "hub")
    os.environ["HF_HUB_CACHE"] = str(Path(hf_home) / "hub")

    logger.info("homeward-embed warmup: variant=%s hf_home=%s", variant, hf_home)
    t0 = time.perf_counter()
    # Import here to avoid circular import at module level.
    from homeward_embed.embedder import PhotoEmbedder as _PE  # noqa: PLC0415

    tmp = _PE(variant=variant)  # type: ignore[arg-type]
    elapsed = time.perf_counter() - t0
    logger.info("homeward-embed warmup complete: variant=%s embed_dim=%d elapsed=%.1fs",
                variant, tmp.embed_dim, elapsed)


@app.on_event("startup")  # type: ignore[misc]
async def _startup() -> None:
    global _detector, _embedder, _index
    logger.info("homeward-embed startup: variant=%s index_dir=%s", _MODEL_VARIANT, _INDEX_DIR)
    # Warmup: prefetch model weights before serving so the first /enroll
    # never blocks on a network download or fails offline.
    _run_warmup(_MODEL_VARIANT)
    _embedder = PhotoEmbedder(variant=_MODEL_VARIANT)
    try:
        _detector = AnimalDetector()
    except ImportError:
        logger.warning("ultralytics not available — detector disabled, using whole-image fallback")
        _detector = None


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------


def _load_image_from_url(url: str) -> Image.Image:
    """Fetch an image from *url* and return a PIL Image."""
    resp = httpx.get(url, timeout=30, follow_redirects=True)
    resp.raise_for_status()
    return Image.open(io.BytesIO(resp.content))


def _embed_with_crop(image: Image.Image, species: Optional[str] = None) -> tuple[np.ndarray, bool]:
    """Detect + crop + embed. Returns (vector, was_detected)."""
    assert _embedder is not None  # set at startup  # noqa: S101
    if _detector is not None:
        crop, detected = _detector.crop(image, species_hint=species)
    else:
        crop, detected = image, False
        logger.warning("Detector not available; using whole-image fallback.")
    vec = _embedder.embed(crop)
    return vec, detected


# ---------------------------------------------------------------------------
# Endpoints
# ---------------------------------------------------------------------------


@app.get("/health")
async def health() -> Response:
    return Response(content='{"status":"ok"}', media_type="application/json")


@app.post("/enroll", response_model=EnrollResponse)
async def enroll(req: EnrollRequest) -> EnrollResponse:
    """Embed + index one intake photo."""
    assert _index is not None  # noqa: S101
    try:
        image = _load_image_from_url(req.image_url)
    except Exception as exc:
        raise HTTPException(status_code=422, detail=f"Could not fetch image: {exc}") from exc

    vec, detected = _embed_with_crop(image, species=req.species)
    internal_id = _index.enroll(req.canonical_id, vec, species=req.species)
    return EnrollResponse(canonical_id=req.canonical_id, internal_id=internal_id, detected=detected)


@app.post("/query", response_model=QueryResponse)
async def query(req: QueryRequest) -> QueryResponse:
    """Search gallery for pets similar to the query photo."""
    assert _index is not None  # noqa: S101
    if req.image_url is None and req.image_b64 is None:
        raise HTTPException(status_code=422, detail="Provide image_url or image_b64.")

    if req.image_url is not None:
        try:
            image = _load_image_from_url(req.image_url)
        except Exception as exc:
            raise HTTPException(status_code=422, detail=f"Could not fetch image: {exc}") from exc
    else:
        import base64

        raw = base64.b64decode(req.image_b64)  # type: ignore[arg-type]
        image = Image.open(io.BytesIO(raw))

    vec, _ = _embed_with_crop(image, species=req.species_filter)
    results = _index.query(vec, k=req.k, species_filter=req.species_filter)
    matches = [QueryMatch(canonical_id=cid, score=score) for cid, score in results]
    return QueryResponse(matches=matches)


@app.post("/reembed_all")
async def reembed_all() -> dict[str, str]:
    """Admin endpoint — rebuild the entire index. Long-running on large galleries."""
    return {"status": "not_implemented", "detail": "reembed_all requires a canonical-id → URL mapping source; wire via homeward-ingest."}


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def main() -> None:
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(name)s: %(message)s")
    port = int(os.environ.get("HW_EMBED_PORT", "8741"))
    host = os.environ.get("HW_EMBED_HOST", "127.0.0.1")
    uvicorn.run("homeward_embed.service:app", host=host, port=port, reload=False)


if __name__ == "__main__":
    main()
