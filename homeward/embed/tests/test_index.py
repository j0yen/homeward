"""Tests for the HNSW EmbedIndex.

Tests run without downloading any ML models (pure index logic).
"""

from __future__ import annotations

import json
import tempfile
from pathlib import Path

import numpy as np
import pytest


def _rand_vec(dim: int = 768, seed: int = 0) -> np.ndarray:
    rng = np.random.default_rng(seed)
    v = rng.standard_normal(dim).astype(np.float32)
    return v / float(np.linalg.norm(v))


class TestEmbedIndex:
    """Tests for EmbedIndex (no ML models needed)."""

    def test_enroll_and_persist(self) -> None:
        """AC3: enroll adds an entry that survives process restart (persist + reload)."""
        from homeward_embed.index import EmbedIndex

        dim = 64  # small dim for speed
        with tempfile.TemporaryDirectory() as tmpdir:
            idx = EmbedIndex(index_dir=tmpdir, embed_dim=dim, max_elements=100)
            vec = _rand_vec(dim, seed=1)
            internal_id = idx.enroll("canonical-001", vec, species="dog")
            assert internal_id == 0

            # Verify files exist on disk
            assert (Path(tmpdir) / "index.bin").exists()
            assert (Path(tmpdir) / "id_map.json").exists()

            # Reload from disk simulates process restart
            idx2 = EmbedIndex(index_dir=tmpdir, embed_dim=dim, max_elements=100)
            assert idx2.size == 1

    def test_query_returns_correct_id(self) -> None:
        """AC3: query returns the k nearest canonical_ids with cosine scores."""
        from homeward_embed.index import EmbedIndex

        dim = 64
        with tempfile.TemporaryDirectory() as tmpdir:
            idx = EmbedIndex(index_dir=tmpdir, embed_dim=dim, max_elements=100)
            vec_a = _rand_vec(dim, seed=10)
            vec_b = _rand_vec(dim, seed=20)
            idx.enroll("pet-A", vec_a, species="dog")
            idx.enroll("pet-B", vec_b, species="cat")

            # Query with vec_a — pet-A must be top-1
            results = idx.query(vec_a, k=2)
            assert len(results) >= 1
            top_id, top_score = results[0]
            assert top_id == "pet-A", f"Expected pet-A, got {top_id}"
            assert top_score > 0.99, f"Expected near-1.0 cosine for identical vector, got {top_score:.4f}"

    def test_species_filter(self) -> None:
        """AC3: query honors optional species_filter."""
        from homeward_embed.index import EmbedIndex

        dim = 64
        with tempfile.TemporaryDirectory() as tmpdir:
            idx = EmbedIndex(index_dir=tmpdir, embed_dim=dim, max_elements=100)
            vec_dog = _rand_vec(dim, seed=1)
            vec_cat = _rand_vec(dim, seed=2)
            idx.enroll("dog-1", vec_dog, species="dog")
            idx.enroll("cat-1", vec_cat, species="cat")

            # Query with dog filter — only dog-1 returned
            results = idx.query(vec_dog, k=5, species_filter="dog")
            ids = [r[0] for r in results]
            assert "dog-1" in ids, f"dog-1 not in results: {ids}"
            assert "cat-1" not in ids, f"cat-1 leaked through dog filter: {ids}"

    def test_persist_survives_restart_with_1k_vectors(self) -> None:
        """AC3: index with 1k vectors persists and reloads correctly."""
        from homeward_embed.index import EmbedIndex

        dim = 128
        n = 1000
        with tempfile.TemporaryDirectory() as tmpdir:
            idx = EmbedIndex(index_dir=tmpdir, embed_dim=dim, max_elements=n + 10)
            vecs = [_rand_vec(dim, seed=i) for i in range(n)]
            for i, v in enumerate(vecs):
                idx.enroll(f"pet-{i:04d}", v, species="dog" if i % 2 == 0 else "cat")

            # Reload
            idx2 = EmbedIndex(index_dir=tmpdir, embed_dim=dim, max_elements=n + 10)
            assert idx2.size == n

            # Query the last vector — should find itself at top
            results = idx2.query(vecs[-1], k=1)
            assert results[0][0] == f"pet-{n - 1:04d}"

    def test_wrong_dim_raises(self) -> None:
        """EmbedIndex raises ValueError if reloaded with wrong embed_dim."""
        from homeward_embed.index import EmbedIndex

        with tempfile.TemporaryDirectory() as tmpdir:
            idx = EmbedIndex(index_dir=tmpdir, embed_dim=64, max_elements=10)
            idx.enroll("x", _rand_vec(64), species="dog")
            del idx

            with pytest.raises(ValueError, match="embed_dim"):
                EmbedIndex(index_dir=tmpdir, embed_dim=128, max_elements=10)
