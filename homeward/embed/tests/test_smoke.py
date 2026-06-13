"""Smoke tests for homeward-embed CLI (warmup + smoke commands).

These tests exercise the CLI logic with a mocked PhotoEmbedder — they do NOT
download real DINOv2 weights, so they run in CI / offline sandboxes.

An integration test that uses real weights is marked ``@pytest.mark.integration``
and skipped unless the model cache is already populated (no network required).

Run with real weights:
  HF_HUB_OFFLINE=1 pytest tests/test_smoke.py -m integration -v
"""

from __future__ import annotations

import os
import tempfile
import unittest.mock as mock
from pathlib import Path
from typing import Generator

import numpy as np
import pytest
from PIL import Image


# ---------------------------------------------------------------------------
# Helpers shared across tests
# ---------------------------------------------------------------------------

def _make_image(colour: tuple[int, int, int] = (128, 64, 32)) -> Image.Image:
    arr = np.full((224, 224, 3), colour, dtype=np.uint8)
    return Image.fromarray(arr, mode="RGB")


def _make_mock_embedder(embed_dim: int = 384) -> mock.MagicMock:
    """Return a PhotoEmbedder mock that produces colour-dependent vectors."""
    m = mock.MagicMock()
    m.embed_dim = embed_dim

    def _embed(image: Image.Image) -> np.ndarray:
        # Produce a vector from the mean pixel colour — distinct images → distinct vecs.
        arr = np.array(image.convert("RGB"), dtype=np.float32)
        mean = arr.mean(axis=(0, 1))  # shape (3,)
        # Expand to embed_dim by tiling
        vec = np.tile(mean, embed_dim // 3 + 1)[:embed_dim].astype(np.float32)
        norm = float(np.linalg.norm(vec))
        return (vec / norm).astype(np.float32) if norm > 1e-9 else vec

    m.embed.side_effect = _embed
    return m


# ---------------------------------------------------------------------------
# CLI import sanity
# ---------------------------------------------------------------------------

def test_cli_importable() -> None:
    """homeward_embed.cli must be importable without side-effects."""
    from homeward_embed import cli  # noqa: PLC0415

    assert hasattr(cli, "main")
    assert hasattr(cli, "cmd_warmup")
    assert hasattr(cli, "cmd_smoke")


def test_cli_warmup_help(capsys: pytest.CaptureFixture) -> None:
    """``homeward-embed warmup --help`` must exit 0 without loading a model."""
    from homeward_embed.cli import main  # noqa: PLC0415

    with pytest.raises(SystemExit) as exc_info:
        main(["warmup", "--help"])
    assert exc_info.value.code == 0
    captured = capsys.readouterr()
    assert "warmup" in captured.out.lower() or "hf-home" in captured.out.lower()


def test_cli_smoke_help(capsys: pytest.CaptureFixture) -> None:
    """``homeward-embed smoke --help`` must exit 0 without loading a model."""
    from homeward_embed.cli import main  # noqa: PLC0415

    with pytest.raises(SystemExit) as exc_info:
        main(["smoke", "--help"])
    assert exc_info.value.code == 0


# ---------------------------------------------------------------------------
# Warmup command (mocked)
# ---------------------------------------------------------------------------

class TestWarmup:
    """Tests for cmd_warmup with a mocked PhotoEmbedder."""

    def _run_warmup(self, tmp_path: Path, variant: str = "small") -> tuple[int, mock.MagicMock]:
        mock_embedder = _make_mock_embedder(embed_dim=384 if variant == "small" else 768)
        # PhotoEmbedder is imported locally inside cmd_warmup via
        # "from homeward_embed.embedder import PhotoEmbedder", so patch it
        # at the embedder module level.
        with mock.patch(
            "homeward_embed.embedder.PhotoEmbedder",
            return_value=mock_embedder,
        ) as mock_cls:
            import argparse  # noqa: PLC0415
            from homeward_embed.cli import cmd_warmup  # noqa: PLC0415

            args = argparse.Namespace(variant=variant, hf_home=str(tmp_path))
            rc = cmd_warmup(args)
        return rc, mock_cls

    def test_warmup_exits_zero(self, tmp_path: Path) -> None:
        rc, _ = self._run_warmup(tmp_path)
        assert rc == 0

    def test_warmup_sets_hf_home(self, tmp_path: Path) -> None:
        self._run_warmup(tmp_path)
        assert os.environ.get("HF_HOME") == str(tmp_path)

    def test_warmup_loads_embedder(self, tmp_path: Path) -> None:
        _, mock_cls = self._run_warmup(tmp_path)
        mock_cls.assert_called_once()
        call_kwargs = mock_cls.call_args
        assert call_kwargs is not None

    def test_warmup_calls_embed_for_sanity_check(self, tmp_path: Path) -> None:
        mock_embedder = _make_mock_embedder()
        with mock.patch("homeward_embed.embedder.PhotoEmbedder", return_value=mock_embedder):
            import argparse  # noqa: PLC0415
            from homeward_embed.cli import cmd_warmup  # noqa: PLC0415

            args = argparse.Namespace(variant="small", hf_home=str(tmp_path))
            rc = cmd_warmup(args)

        assert rc == 0
        mock_embedder.embed.assert_called_once()  # blank-image forward pass


# ---------------------------------------------------------------------------
# Smoke command (mocked, no live sidecar)
# ---------------------------------------------------------------------------

class TestSmoke:
    """Tests for cmd_smoke using synthetic fixtures + mocked PhotoEmbedder."""

    def _run_smoke(
        self,
        tmp_path: Path,
        variant: str = "small",
        fixture_override: list[Path] | None = None,
    ) -> int:
        mock_embedder = _make_mock_embedder(embed_dim=384 if variant == "small" else 768)

        def _fake_load_fixtures() -> list[Path]:
            if fixture_override is not None:
                return fixture_override
            from homeward_embed.cli import _generate_synthetic_fixtures  # noqa: PLC0415
            return _generate_synthetic_fixtures(tmp_path / "fixtures")

        # PhotoEmbedder is imported locally in cmd_smoke; patch at embedder module level.
        with mock.patch("homeward_embed.embedder.PhotoEmbedder", return_value=mock_embedder), \
             mock.patch("homeward_embed.cli._load_fixtures", side_effect=_fake_load_fixtures):
            import argparse  # noqa: PLC0415
            from homeward_embed.cli import cmd_smoke  # noqa: PLC0415

            args = argparse.Namespace(variant=variant, hf_home=str(tmp_path / "hf"))
            rc = cmd_smoke(args)
        return rc

    def test_smoke_exits_zero(self, tmp_path: Path) -> None:
        rc = self._run_smoke(tmp_path)
        assert rc == 0

    def test_smoke_rank1_self_match(self, tmp_path: Path, capsys: pytest.CaptureFixture) -> None:
        """Smoke output must declare SMOKE PASS."""
        self._run_smoke(tmp_path)
        captured = capsys.readouterr()
        assert "SMOKE PASS" in captured.out

    def test_smoke_discriminative_assertion(self, tmp_path: Path, capsys: pytest.CaptureFixture) -> None:
        """Smoke must assert discriminative rank ordering (different images score lower)."""
        rc = self._run_smoke(tmp_path)
        captured = capsys.readouterr()
        assert rc == 0
        # Either "PASS  discriminative" or "SKIP  only 1 result"
        assert "discriminative" in captured.out.lower() or "SKIP" in captured.out

    def test_smoke_latency_reported(self, tmp_path: Path, capsys: pytest.CaptureFixture) -> None:
        """Smoke must print measured query latency in milliseconds."""
        self._run_smoke(tmp_path)
        captured = capsys.readouterr()
        assert "query_latency_ms=" in captured.out

    def test_smoke_fails_with_one_fixture(self, tmp_path: Path) -> None:
        """Smoke must exit non-zero if fewer than 2 fixtures are available."""
        # Create only one fixture image
        one_fixture = tmp_path / "fixtures" / "only_one.png"
        one_fixture.parent.mkdir(parents=True, exist_ok=True)
        arr = np.full((224, 224, 3), (128, 64, 32), dtype=np.uint8)
        Image.fromarray(arr).save(one_fixture)

        rc = self._run_smoke(tmp_path, fixture_override=[one_fixture])
        assert rc != 0


# ---------------------------------------------------------------------------
# Synthetic fixture generator
# ---------------------------------------------------------------------------

class TestSyntheticFixtures:
    def test_generates_three_images(self, tmp_path: Path) -> None:
        from homeward_embed.cli import _generate_synthetic_fixtures  # noqa: PLC0415

        paths = _generate_synthetic_fixtures(tmp_path / "fixtures")
        assert len(paths) == 3

    def test_images_are_valid_pngs(self, tmp_path: Path) -> None:
        from homeward_embed.cli import _generate_synthetic_fixtures  # noqa: PLC0415

        paths = _generate_synthetic_fixtures(tmp_path / "fixtures")
        for p in paths:
            assert p.exists()
            img = Image.open(p)
            assert img.mode in ("RGB", "RGBA", "L")
            assert img.size == (224, 224)

    def test_images_are_colour_distinct(self, tmp_path: Path) -> None:
        """Each synthetic fixture should have a distinct mean pixel colour."""
        from homeward_embed.cli import _generate_synthetic_fixtures  # noqa: PLC0415

        paths = _generate_synthetic_fixtures(tmp_path / "fixtures")
        means = []
        for p in paths:
            arr = np.array(Image.open(p).convert("RGB"), dtype=np.float32)
            means.append(arr.mean(axis=(0, 1)))

        # All mean colours must be distinct (no two identical)
        for i in range(len(means)):
            for j in range(i + 1, len(means)):
                assert not np.allclose(means[i], means[j], atol=1.0), \
                    f"Fixtures {i} and {j} have identical mean colour — smoke ordering may be trivial"


# ---------------------------------------------------------------------------
# Integration test (real model, skipped unless weights are cached)
# ---------------------------------------------------------------------------

@pytest.mark.integration
def test_smoke_with_real_model(tmp_path: Path) -> None:
    """Real end-to-end smoke using actual DINOv2-small weights.

    Skipped automatically if:
      1. The model isn't cached locally (avoids surprise downloads in CI).
      2. HF_HUB_OFFLINE is already set but weights are absent.

    To run manually after `homeward-embed warmup --variant small`:
      pytest tests/test_smoke.py::test_smoke_with_real_model -m integration -v
    """
    import argparse  # noqa: PLC0415
    from homeward_embed.cli import cmd_smoke  # noqa: PLC0415

    hf_home = os.environ.get("HF_HOME", str(Path.home() / ".cache" / "homeward" / "hf"))
    model_cache = Path(hf_home) / "hub"

    # Check if model files are cached to avoid triggering a download.
    cached = model_cache.exists() and any(
        p.name.startswith("models--facebook--dinov2") for p in model_cache.iterdir()
        if p.is_dir()
    )
    if not cached:
        pytest.skip("DINOv2-small weights not cached locally; run `homeward-embed warmup --variant small` first")

    args = argparse.Namespace(variant="small", hf_home=hf_home)
    rc = cmd_smoke(args)
    assert rc == 0, "Integration smoke failed — see stdout for details"
