"""Tests for the homeward-embed FastAPI service.

Tests run without downloading any ML models or starting the app (pure
helper-function logic only).
"""

from __future__ import annotations


class TestDedupMatches:
    """Tests for _dedup_matches (per-animal collapse of raw kNN hits)."""

    def test_multiple_photos_collapse_to_best_score(self) -> None:
        """Same canonical_id at several scores collapses to one entry, best score kept."""
        from homeward_embed.service import _dedup_matches

        raw = [("pet-A", 0.6), ("pet-A", 0.9), ("pet-A", 0.75)]
        result = _dedup_matches(raw, k=5)
        assert result == [("pet-A", 0.9)]

    def test_sort_order_is_score_descending(self) -> None:
        """Distinct animals are returned sorted by score descending."""
        from homeward_embed.service import _dedup_matches

        raw = [("pet-A", 0.5), ("pet-B", 0.9), ("pet-C", 0.7)]
        result = _dedup_matches(raw, k=3)
        assert result == [("pet-B", 0.9), ("pet-C", 0.7), ("pet-A", 0.5)]

    def test_fewer_distinct_than_k_returns_all(self) -> None:
        """When fewer distinct animals exist than k, return however many there are."""
        from homeward_embed.service import _dedup_matches

        raw = [("pet-A", 0.9), ("pet-A", 0.8), ("pet-B", 0.7)]
        result = _dedup_matches(raw, k=5)
        assert result == [("pet-A", 0.9), ("pet-B", 0.7)]

    def test_repro_shape_five_hits_same_animal_collapse_to_one(self) -> None:
        """Exact repro: k=5 raw hits, all the same canonical_id, collapse to 1 result."""
        from homeward_embed.service import _dedup_matches

        raw = [
            ("01KZYASQEF6GHKZS1JKDJ7J6JH", 1.000),
            ("01KZYASQEF6GHKZS1JKDJ7J6JH", 0.894),
            ("01KZYASQEF6GHKZS1JKDJ7J6JH", 0.800),
            ("01KZYASQEF6GHKZS1JKDJ7J6JH", 0.713),
            ("01KZYASQEF6GHKZS1JKDJ7J6JH", 0.688),
        ]
        result = _dedup_matches(raw, k=5)
        assert result == [("01KZYASQEF6GHKZS1JKDJ7J6JH", 1.000)]

    def test_empty_raw_matches_returns_empty(self) -> None:
        """No raw hits (empty gallery / no neighbors) returns an empty list."""
        from homeward_embed.service import _dedup_matches

        assert _dedup_matches([], k=5) == []
