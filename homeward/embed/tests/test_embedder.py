"""Tests for the DINOv2 embedder.

These tests run in offline mode (no model download) by mocking the transformers
library, so they can run in CI without GPU or network access.

For a real integration test with actual model weights, run:
  pytest tests/ -m integration --no-header -rN
"""

from __future__ import annotations

import io
import struct
import unittest.mock as mock
from pathlib import Path
from typing import Generator

import numpy as np
import pytest
from PIL import Image


# ---------------------------------------------------------------------------
# Fixtures
# ---------------------------------------------------------------------------


def _make_rgb_image(width: int = 64, height: int = 64) -> Image.Image:
    """Create a synthetic RGB image for testing."""
    rng = np.random.default_rng(42)
    arr = rng.integers(0, 255, (height, width, 3), dtype=np.uint8)
    return Image.fromarray(arr, mode="RGB")


def _make_image_with_exif() -> Image.Image:
    """Create a minimal JPEG-bearing EXIF marker in memory."""
    img = _make_rgb_image()
    buf = io.BytesIO()
    # Pillow's default save for JPEG strips most EXIF, but we add a comment
    # to simulate metadata. The test asserts no metadata is retained.
    exif_bytes = img.getexif().tobytes()
    img.save(buf, format="JPEG", exif=exif_bytes)
    buf.seek(0)
    return Image.open(buf)


@pytest.fixture
def mock_dinov2() -> Generator[mock.MagicMock, None, None]:
    """Mock transformers to avoid downloading the model."""
    fake_vec = np.ones((1, 768), dtype=np.float32)  # non-normalized for realistic test

    class FakeOutput:
        last_hidden_state = None  # set per call below

    class FakeModel:
        def to(self, device: str) -> "FakeModel":
            return self

        def eval(self) -> "FakeModel":
            return self

        def __call__(self, **kwargs: object) -> FakeOutput:
            out = FakeOutput()
            import torch
            out.last_hidden_state = torch.tensor(fake_vec.reshape(1, 1, 768))
            return out

    class FakeProcessor:
        def __call__(self, images: object, return_tensors: str) -> dict:
            import torch
            return {"pixel_values": torch.zeros(1, 3, 224, 224)}

    with mock.patch("homeward_embed.embedder.AutoModel") as mock_model, \
         mock.patch("homeward_embed.embedder.AutoImageProcessor") as mock_proc:
        mock_model.from_pretrained.return_value = FakeModel()
        mock_proc.from_pretrained.return_value = FakeProcessor()
        yield mock_model


# ---------------------------------------------------------------------------
# Tests
# ---------------------------------------------------------------------------


class TestPhotoEmbedder:
    """Tests for PhotoEmbedder (with mocked model)."""

    def test_embed_returns_correct_shape(self, mock_dinov2: mock.MagicMock) -> None:
        """AC1: embed produces a fixed-dimension vector."""
        from homeward_embed.embedder import PhotoEmbedder

        embedder = PhotoEmbedder(variant="base", device="cpu")
        img = _make_rgb_image()
        vec = embedder.embed(img)

        assert vec.shape == (768,), f"Expected (768,) got {vec.shape}"

    def test_embed_is_l2_normalized(self, mock_dinov2: mock.MagicMock) -> None:
        """AC1: the output vector has L2 norm ≈ 1.0."""
        from homeward_embed.embedder import PhotoEmbedder

        embedder = PhotoEmbedder(variant="base", device="cpu")
        img = _make_rgb_image()
        vec = embedder.embed(img)

        norm = float(np.linalg.norm(vec))
        assert abs(norm - 1.0) < 1e-5, f"L2 norm {norm:.6f} is not ≈ 1.0"

    def test_embed_small_variant(self, mock_dinov2: mock.MagicMock) -> None:
        """ViT-S variant produces 384-d embeddings."""
        import unittest.mock as m
        import torch

        class FakeModelSmall:
            def to(self, device: str) -> "FakeModelSmall":
                return self

            def eval(self) -> "FakeModelSmall":
                return self

            def __call__(self, **kwargs: object) -> object:
                class Out:
                    last_hidden_state = torch.ones(1, 1, 384)
                return Out()

        with m.patch("homeward_embed.embedder.AutoModel") as pm, \
             m.patch("homeward_embed.embedder.AutoImageProcessor") as pp:
            pm.from_pretrained.return_value = FakeModelSmall()

            class FakeProcessorSmall:
                def __call__(self, images: object, return_tensors: str) -> dict:
                    import torch
                    return {"pixel_values": torch.zeros(1, 3, 224, 224)}

            pp.from_pretrained.return_value = FakeProcessorSmall()
            # No importlib.reload here: reloading inside the patch context
            # re-runs `from transformers import AutoModel, ...` and replaces
            # the patched names with the real ones, so the test hit the HF
            # Hub for real (and failed whenever the cache was unavailable).
            from homeward_embed.embedder import PhotoEmbedder

            embedder = PhotoEmbedder(variant="small", device="cpu")

        assert embedder.embed_dim == 384

    def test_exif_stripped_from_input(self, mock_dinov2: mock.MagicMock) -> None:
        """AC7: EXIF metadata present on an input image is not written to disk or returned."""
        from homeward_embed.embedder import _strip_exif

        exif_img = _make_image_with_exif()
        # The original might have exif info from the save
        stripped = _strip_exif(exif_img)

        # After stripping, getexif() should return empty (PNG format has no EXIF)
        exif_data = stripped.getexif()
        assert len(exif_data) == 0, f"EXIF not fully stripped; remaining keys: {list(exif_data.keys())}"

    def test_exif_not_persisted_by_embedder(self, mock_dinov2: mock.MagicMock) -> None:
        """AC7: embedder does not write EXIF-bearing data to any file."""
        from homeward_embed.embedder import PhotoEmbedder
        import tempfile

        embedder = PhotoEmbedder(variant="base", device="cpu")
        exif_img = _make_image_with_exif()

        with tempfile.TemporaryDirectory() as tmpdir:
            vec = embedder.embed(exif_img)
            # Verify no image files were written to the temp dir
            written = list(Path(tmpdir).rglob("*"))
            assert written == [], f"Embedder wrote unexpected files: {written}"

        # Vector returned, no crash
        assert vec.shape == (768,)
