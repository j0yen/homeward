"""DINOv2 photo embedder — produces L2-normalized 1024-d (ViT-L), 768-d (ViT-B),
or 384-d (ViT-S) vectors.

Model choices:
  "facebook/dinov2-small" → 384-d  (ViT-S/14, faster on CPU)
  "facebook/dinov2-base"  → 768-d  (ViT-B/14, default, better accuracy)
  "facebook/dinov2-large" → 1024-d (ViT-L/14, slower, higher accuracy)

All three are Apache-2.0 licensed and commercially usable.

EXIF: any EXIF metadata present on a PIL image is stripped before processing
via image.tobytes() round-trip — raw pixel data carries no EXIF.
"""

from __future__ import annotations

import io
import logging
from typing import Literal

import numpy as np
import torch
from PIL import Image
from transformers import AutoImageProcessor, AutoModel  # type: ignore[import-untyped]

logger = logging.getLogger(__name__)

ModelVariant = Literal["small", "base", "large"]
_MODEL_IDS: dict[ModelVariant, str] = {
    "small": "facebook/dinov2-small",
    "base": "facebook/dinov2-base",
    "large": "facebook/dinov2-large",
}
_EMBED_DIMS: dict[ModelVariant, int] = {
    "small": 384,
    "base": 768,
    "large": 1024,
}


def _strip_exif(image: Image.Image) -> Image.Image:
    """Return a pixel-only copy of *image* with no EXIF metadata.

    We round-trip through raw bytes so no EXIF tag survives.
    """
    buf = io.BytesIO()
    # Save as PNG (no EXIF support) to a buffer, then reload.
    image.save(buf, format="PNG")
    buf.seek(0)
    return Image.open(buf).copy()


class PhotoEmbedder:
    """Embed a PIL image to an L2-normalized vector using DINOv2.

    Args:
        variant: "small" (384-d, ViT-S/14), "base" (768-d, ViT-B/14), or
            "large" (1024-d, ViT-L/14).
        device: torch device string. Defaults to "cuda" if available, else "cpu".
    """

    def __init__(
        self,
        variant: ModelVariant = "base",
        device: str | None = None,
    ) -> None:
        if variant not in _MODEL_IDS:
            msg = f"Unknown variant {variant!r}; choose one of {sorted(_MODEL_IDS)}."
            raise ValueError(msg)
        self._variant = variant
        self._embed_dim = _EMBED_DIMS[variant]
        model_id = _MODEL_IDS[variant]

        if device is None:
            device = "cuda" if torch.cuda.is_available() else "cpu"
        self._device = device

        logger.info("Loading DINOv2 %s (%s) on %s …", variant, model_id, device)
        self._processor = AutoImageProcessor.from_pretrained(model_id)
        self._model = AutoModel.from_pretrained(model_id).to(device)
        self._model.eval()
        logger.info("DINOv2 %s ready (embed_dim=%d)", variant, self._embed_dim)

    @property
    def embed_dim(self) -> int:
        """Dimensionality of the output vector."""
        return self._embed_dim

    @torch.no_grad()
    def embed(self, image: Image.Image) -> np.ndarray:
        """Embed *image* to an L2-normalized float32 vector.

        EXIF metadata is stripped before processing.

        Args:
            image: PIL Image (any mode; converted to RGB internally).

        Returns:
            float32 numpy array of shape (embed_dim,) with L2 norm ≈ 1.0.
        """
        # Strip EXIF — raw pixels only.
        clean = _strip_exif(image).convert("RGB")

        inputs = self._processor(images=clean, return_tensors="pt")
        inputs = {k: v.to(self._device) for k, v in inputs.items()}

        outputs = self._model(**inputs)
        # CLS token from last hidden state → (1, embed_dim)
        cls_vec: torch.Tensor = outputs.last_hidden_state[:, 0, :]
        vec = cls_vec.squeeze(0).float().cpu().numpy()

        # L2-normalize
        norm = float(np.linalg.norm(vec))
        if norm < 1e-9:
            logger.warning("Near-zero embedding norm (%.2e) — returning zero vector.", norm)
            return vec.astype(np.float32)
        return (vec / norm).astype(np.float32)
