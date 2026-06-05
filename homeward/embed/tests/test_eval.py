"""Tests for the eval harness — especially the anti-tautology guard.

These tests focus on the correctness of the evaluation logic without requiring
actual ML model downloads.  The anti-tautology guard is the highest-priority
correctness property (see [[feedback_agent_written_fixtures_tautology]]).
"""

from __future__ import annotations

import pytest

from homeward_embed.eval import (
    EvalSample,
    assert_no_overlap,
    average_precision,
    build_gallery_and_queries,
)


class TestAntiTautologyGuard:
    """AC5: assert_no_overlap must raise when IDs overlap."""

    def test_raises_on_gallery_query_overlap(self) -> None:
        """Evaluation errors out if query individual-ID is in the gallery."""
        query_ids = {"dog_001", "dog_002"}
        gallery_ids = {"dog_001", "dog_003"}  # dog_001 in both

        # Passing query_ids directly (not subtracted) — dog_001 overlaps gallery
        with pytest.raises(ValueError, match="Anti-tautology violation"):
            assert_no_overlap(query_ids, gallery_ids)

    def test_no_raise_on_disjoint_ids(self) -> None:
        """No error for truly disjoint query and gallery individual-IDs."""
        query_ids = {"dog_100", "dog_101"}
        gallery_ids = {"dog_001", "dog_002"}
        # Should not raise
        assert_no_overlap(query_ids, gallery_ids)

    def test_raises_on_training_overlap(self) -> None:
        """Evaluation errors out if query individual-ID was in training set."""
        query_ids = {"dog_100"}
        gallery_ids = {"dog_200"}
        training_ids = {"dog_100", "dog_999"}  # dog_100 in training

        with pytest.raises(ValueError, match="training"):
            assert_no_overlap(query_ids, gallery_ids, training_ids=training_ids)

    def test_no_raise_disjoint_all_three(self) -> None:
        """Clean case: all three sets disjoint."""
        assert_no_overlap(
            query_ids={"q1", "q2"},
            gallery_ids={"g1", "g2"},
            training_ids={"t1", "t2"},
        )

    def test_overlap_message_names_offenders(self) -> None:
        """Error message includes the offending individual-IDs."""
        query_ids = {"bad_dog"}
        gallery_ids = {"bad_dog"}

        with pytest.raises(ValueError) as exc_info:
            assert_no_overlap(query_ids, gallery_ids)
        assert "bad_dog" in str(exc_info.value)


class TestBuildGalleryAndQueries:
    """Tests for gallery/query splitting logic."""

    def _sample(self, iid: str, idx: int = 0) -> EvalSample:
        from pathlib import Path
        return EvalSample(individual_id=iid, image_path=Path(f"/fake/{iid}_{idx}.jpg"), species="dog")

    def test_first_image_per_individual_is_gallery(self) -> None:
        samples = [
            self._sample("dog_001", 0),
            self._sample("dog_001", 1),
            self._sample("dog_002", 0),
            self._sample("dog_002", 1),
        ]
        gallery, queries = build_gallery_and_queries(samples)
        gallery_ids = {s.individual_id for s in gallery}
        query_ids = {s.individual_id for s in queries}
        assert gallery_ids == {"dog_001", "dog_002"}
        assert query_ids == {"dog_001", "dog_002"}
        assert len(gallery) == 2
        assert len(queries) == 2

    def test_single_image_individual_goes_to_gallery_only(self) -> None:
        samples = [
            self._sample("lone_dog", 0),
            self._sample("multi_dog", 0),
            self._sample("multi_dog", 1),
        ]
        gallery, queries = build_gallery_and_queries(samples)
        gallery_ids = {s.individual_id for s in gallery}
        assert "lone_dog" in gallery_ids
        query_ids = {s.individual_id for s in queries}
        assert "lone_dog" not in query_ids  # no second image → no query contribution


class TestAveragePrecision:
    """Tests for the AP calculation helper."""

    def test_correct_at_rank1(self) -> None:
        assert average_precision(["dog_001", "dog_002", "dog_003"], "dog_001") == pytest.approx(1.0)

    def test_correct_at_rank2(self) -> None:
        ap = average_precision(["dog_999", "dog_001", "dog_002"], "dog_001")
        # Hit at rank 2: precision = 1/2
        assert ap == pytest.approx(0.5)

    def test_not_found(self) -> None:
        assert average_precision(["dog_002", "dog_003"], "dog_001") == pytest.approx(0.0)

    def test_empty_ranked_list(self) -> None:
        assert average_precision([], "dog_001") == pytest.approx(0.0)
