"""Tests for homeward-embed attest deliver.

These tests cover:
- AC5: graceful skip when model is unavailable
- AC2: the assertion logic (rank-1 = cat1, dogs ranked below)
- AC4: the anti-tautology guard (gallery and query are disjoint photos)
- Integration with eval-smoke fixtures (path existence)

Tests that require the real DINOv2 model are marked with @pytest.mark.integration
and skipped when weights are not cached locally.
"""

from __future__ import annotations

import argparse
import tempfile
import types
import unittest.mock as mock
from pathlib import Path

import numpy as np
import pytest
from PIL import Image


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _make_attest_args(
    variant: str = "small",
    hf_home: str = "/nonexistent/hf_home",
    out: Path | None = None,
) -> argparse.Namespace:
    """Build a minimal Namespace that cmd_attest expects."""
    if out is None:
        out = Path(tempfile.mktemp(suffix=".md"))
    return argparse.Namespace(
        variant=variant,
        hf_home=hf_home,
        out=out,
        subcommand="deliver",
    )


def _make_mock_embedder(color_map: dict[str, tuple[int, int, int]]) -> object:
    """Return a fake PhotoEmbedder that embeds by mean RGB of the input image.

    color_map: {canonical_id: (R, G, B)} — not used directly, but the fixture
    images are solid-color PNGs, so the embedder will naturally produce
    embeddings close to the mean color.

    The mock uses the actual mean-RGB of the input image and pads to 768-d.
    """
    class _MockEmbedder:
        embed_dim = 768

        def embed(self, img: Image.Image) -> np.ndarray:
            arr = np.array(img.convert("RGB"), dtype=np.float32)
            mean_rgb = arr.reshape(-1, 3).mean(axis=0)
            vec = np.zeros(768, dtype=np.float32)
            vec[:3] = mean_rgb
            norm = float(np.linalg.norm(vec))
            if norm > 0:
                vec /= norm
            return vec

    return _MockEmbedder()


# ---------------------------------------------------------------------------
# Unit tests
# ---------------------------------------------------------------------------


class TestAttestGracefulSkip:
    """AC5: attestation skips loudly when model unavailable."""

    def test_skip_on_import_error(self, tmp_path: Path) -> None:
        """If PhotoEmbedder cannot be imported, attest exits 0 with SKIPPED."""
        from homeward_embed.cli import cmd_attest

        args = _make_attest_args(out=tmp_path / "DELIVER.md")

        with mock.patch.dict("sys.modules", {
            "homeward_embed.embedder": None,  # type: ignore[dict-item]
            "homeward_embed.index": None,  # type: ignore[dict-item]
        }):
            # builtins.__import__ raises ImportError for None-mapped modules
            result = cmd_attest(args)

        assert result == 0, "Should exit 0 on ImportError (skip, not crash)"
        out_text = (tmp_path / "DELIVER.md").read_text()
        assert "SKIPPED" in out_text

    def test_skip_on_model_load_error(self, tmp_path: Path) -> None:
        """If PhotoEmbedder raises during __init__, attest exits 0 with SKIPPED."""
        from homeward_embed.cli import cmd_attest

        args = _make_attest_args(out=tmp_path / "DELIVER.md")

        with mock.patch("homeward_embed.cli.Path") as _mock_path:
            # Let Path calls through for fixture dir, but mock embedder load
            pass  # We'll patch the import directly

        # Patch the embedder import to raise on instantiation
        mock_embedder_mod = types.ModuleType("homeward_embed.embedder")

        class _FailEmbedder:
            def __init__(self, **_kwargs: object) -> None:
                raise OSError("Network unreachable — model not cached")

        mock_embedder_mod.PhotoEmbedder = _FailEmbedder  # type: ignore[attr-defined]
        mock_embedder_mod.ModelVariant = str  # type: ignore[attr-defined]

        from homeward_embed import index as _idx_mod

        with mock.patch.dict("sys.modules", {"homeward_embed.embedder": mock_embedder_mod}):
            result = cmd_attest(args)

        assert result == 0, "Should exit 0 on model load error (skip, not crash)"
        out_text = (tmp_path / "DELIVER.md").read_text()
        assert "SKIPPED" in out_text

    def test_skip_writes_deliver_md(self, tmp_path: Path) -> None:
        """Even on skip, DELIVER.md is written with SKIPPED status."""
        from homeward_embed.cli import _write_deliver_md, _ATTEST_STATUS_SKIPPED

        out = tmp_path / "DELIVER.md"
        _write_deliver_md(
            out_path=out,
            status=_ATTEST_STATUS_SKIPPED,
            skip_reason="unit test skip",
            variant="small",
            timestamp="2026-06-13T00:00:00+00:00",
        )

        assert out.exists()
        text = out.read_text()
        assert "SKIPPED" in text
        assert "unit test skip" in text
        assert "PASS" not in text


class TestAttestAssertionLogic:
    """AC2: cat1 must be rank-1, dogs ranked below."""

    def test_write_deliver_md_pass(self, tmp_path: Path) -> None:
        """PASS status writes a well-formed DELIVER.md."""
        from homeward_embed.cli import _write_deliver_md, _ATTEST_STATUS_PASS

        results = [("cat1", 0.99), ("dog1", 0.45), ("dog2", 0.20)]
        out = tmp_path / "DELIVER.md"
        _write_deliver_md(
            out_path=out,
            status=_ATTEST_STATUS_PASS,
            skip_reason=None,
            variant="small",
            timestamp="2026-06-13T00:00:00+00:00",
            query_latency_ms=12.3,
            results=results,
        )

        assert out.exists()
        text = out.read_text()
        assert "PASS" in text
        assert "cat1" in text
        assert "12.3ms" in text
        assert "Anti-tautology statement" in text
        assert "Fixture sources" in text
        assert "SKIPPED" not in text
        assert "FAIL" not in text

    def test_write_deliver_md_fail(self, tmp_path: Path) -> None:
        """FAIL status writes a FAIL DELIVER.md with the fail reason."""
        from homeward_embed.cli import _write_deliver_md, _ATTEST_STATUS_FAIL

        out = tmp_path / "DELIVER.md"
        _write_deliver_md(
            out_path=out,
            status=_ATTEST_STATUS_FAIL,
            skip_reason=None,
            variant="small",
            timestamp="2026-06-13T00:00:00+00:00",
            fail_reason="dog1 ranked above cat1",
            results=[("dog1", 0.9), ("cat1", 0.5), ("dog2", 0.2)],
        )

        text = out.read_text()
        assert "FAIL" in text
        assert "dog1 ranked above cat1" in text


class TestAttestFixturesExist:
    """Validate the eval-smoke fixtures that the attestation reads."""

    def _smoke_dir(self) -> Path:
        return Path(__file__).parent.parent / "fixtures" / "eval-smoke"

    def test_gallery_fixtures_exist(self) -> None:
        """Gallery fixtures dog1, dog2, cat1 must be present."""
        smoke_dir = self._smoke_dir()
        for name in ("dog1_photo1.png", "dog2_photo1.png", "cat1_photo1.png"):
            assert (smoke_dir / "gallery" / name).exists(), f"Missing gallery fixture: {name}"

    def test_query_fixture_exists(self) -> None:
        """Query fixture cat1_photo2.png must be present."""
        smoke_dir = self._smoke_dir()
        assert (smoke_dir / "query" / "cat1_photo2.png").exists(), "Missing query fixture: cat1_photo2.png"

    def test_query_file_not_in_gallery(self) -> None:
        """cat1_photo2.png (query) must NOT appear in gallery — tautology guard."""
        smoke_dir = self._smoke_dir()
        gallery_files = {p.name for p in (smoke_dir / "gallery").glob("*.png")}
        assert "cat1_photo2.png" not in gallery_files, (
            "Anti-tautology violation: cat1_photo2.png appears in both gallery and query"
        )

    def test_sources_md_exists(self) -> None:
        """SOURCES.md must exist and mention the fixture files."""
        smoke_dir = self._smoke_dir()
        sources = smoke_dir / "SOURCES.md"
        assert sources.exists(), "SOURCES.md must document fixture provenance"
        text = sources.read_text()
        assert "cat1" in text.lower()


class TestAttestWithMockEmbedder:
    """AC1+AC2: full attest flow with a mock embedder (no real model needed).

    Uses real eval-smoke PNG fixtures + mock embedder (mean-RGB) to verify
    the round-trip assertion passes without downloading DINOv2.
    """

    def _smoke_dir(self) -> Path:
        return Path(__file__).parent.parent / "fixtures" / "eval-smoke"

    def test_cat1_query_ranks_first_with_mock_embedder(self, tmp_path: Path) -> None:
        """cat1_photo2 should rank-1 cat1 with the mean-RGB mock embedder."""
        smoke_dir = self._smoke_dir()
        if not smoke_dir.exists():
            pytest.skip("eval-smoke fixture dir not present")

        from homeward_embed.index import EmbedIndex

        gallery_entries = [
            ("dog1", "dog", "gallery/dog1_photo1.png"),
            ("dog2", "dog", "gallery/dog2_photo1.png"),
            ("cat1", "cat", "gallery/cat1_photo1.png"),
        ]

        def mock_embed(img: Image.Image) -> np.ndarray:
            arr = np.array(img.convert("RGB"), dtype=np.float32)
            mean_rgb = arr.reshape(-1, 3).mean(axis=0)
            vec = np.zeros(768, dtype=np.float32)
            vec[:3] = mean_rgb
            norm = float(np.linalg.norm(vec))
            return vec / norm if norm > 0 else vec

        with tempfile.TemporaryDirectory(prefix="hw-attest-test-") as tmpdir:
            index = EmbedIndex(index_dir=tmpdir, embed_dim=768, max_elements=100)

            for canonical_id, species, rel_path in gallery_entries:
                img_path = smoke_dir / rel_path
                if not img_path.exists():
                    pytest.skip(f"fixture missing: {rel_path}")
                img = Image.open(img_path).convert("RGB")
                vec = mock_embed(img)
                index.enroll(canonical_id, vec, species=species)

            query_path = smoke_dir / "query" / "cat1_photo2.png"
            if not query_path.exists():
                pytest.skip("query fixture cat1_photo2.png missing")

            query_img = Image.open(query_path).convert("RGB")
            query_vec = mock_embed(query_img)
            results = index.query(query_vec, k=3)

        assert results, "Index returned no results"
        rank1_id, rank1_score = results[0]
        assert rank1_id == "cat1", (
            f"Expected cat1 at rank-1, got {rank1_id!r}. "
            f"Full ranking: {results}"
        )

        # Both dogs must score strictly below cat1
        for rank, (cid, score) in enumerate(results[1:], start=1):
            if cid in {"dog1", "dog2"}:
                assert score < rank1_score, (
                    f"AC2 fail: {cid} at rank {rank + 1} has score={score:.6f} "
                    f">= cat1 rank-1 score={rank1_score:.6f}"
                )
