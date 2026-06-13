"""Tests for the eval harness — especially the anti-tautology guard.

These tests focus on the correctness of the evaluation logic without requiring
actual ML model downloads.  The anti-tautology guard is the highest-priority
correctness property (see [[feedback_agent_written_fixtures_tautology]]).
"""

from __future__ import annotations

from pathlib import Path

import numpy as np
import pytest

from homeward_embed.eval import (
    EvalSample,
    EvalResult,
    assert_disjoint_individuals,
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


class TestAssertDisjointIndividuals:
    """AC1: assert_disjoint_individuals raises on gallery/query overlap."""

    def test_raises_when_id_in_both(self) -> None:
        """If dog1 appears in both gallery and query, raise ValueError."""
        with pytest.raises(ValueError, match="Anti-tautology"):
            assert_disjoint_individuals(
                gallery_ids=["dog1", "cat1"],
                query_ids=["dog1", "cat2"],
            )

    def test_no_raise_for_disjoint(self) -> None:
        """Disjoint gallery/query — no error."""
        # Should not raise
        assert_disjoint_individuals(
            gallery_ids=["dog1", "cat1"],
            query_ids=["dog2", "cat2"],
        )

    def test_error_message_names_offender(self) -> None:
        """The error message includes the overlapping individual ID."""
        with pytest.raises(ValueError) as exc_info:
            assert_disjoint_individuals(["overlap_id"], ["overlap_id"])
        assert "overlap_id" in str(exc_info.value)

    def test_accepts_sets_as_input(self) -> None:
        """Function accepts set inputs as well as lists."""
        assert_disjoint_individuals(
            gallery_ids={"g1", "g2"},
            query_ids={"q1", "q2"},
        )


class TestFixtureCorrectness:
    """AC3: correctness fixture validates harness arithmetic without real model.

    Uses a mock embedder that maps solid-color images to known vectors, so we
    can assert Rank-1 self-match and discriminative ordering deterministically.
    """

    def _fixture_dir(self) -> Path:
        """Return path to the eval-smoke fixture directory."""
        return Path(__file__).parent.parent / "fixtures" / "eval-smoke"

    def test_fixture_images_exist(self) -> None:
        """Fixture images are committed and accessible."""
        fixture_dir = self._fixture_dir()
        assert fixture_dir.exists(), f"Fixture dir missing: {fixture_dir}"
        gallery_imgs = list((fixture_dir / "gallery").glob("*.png"))
        query_imgs = list((fixture_dir / "query").glob("*.png"))
        assert len(gallery_imgs) >= 2, "Need ≥2 gallery images"
        assert len(query_imgs) >= 2, "Need ≥2 query images"

    def test_fixture_sources_md_exists(self) -> None:
        """SOURCES.md documents fixture provenance."""
        sources = self._fixture_dir() / "SOURCES.md"
        assert sources.exists(), "SOURCES.md must document fixture image licenses"

    def test_rank1_self_match_with_mock_embedder(self) -> None:
        """AC3: an identical gallery/query image yields Rank-1 = 1.0.

        Uses a mock embedder that returns the mean pixel RGB as a 3-d vector,
        so similar-colored images get nearby embeddings.  dog1_photo1 (gallery)
        and dog1_photo2 (query, similar color) should Rank-1 match each other.
        """
        import unittest.mock as mock
        from pathlib import Path
        from PIL import Image as PILImage

        fixture_dir = self._fixture_dir()
        if not fixture_dir.exists():
            pytest.skip("eval-smoke fixture dir not present")

        # Build samples from gallery + query directories.
        # Filename format: <species><n>_photo<m>.png, e.g. dog1_photo1.png
        # individual_id = first underscore-delimited token, e.g. "dog1"
        samples: list[EvalSample] = []
        for split in ("gallery", "query"):
            for img_path in sorted((fixture_dir / split).glob("*.png")):
                stem = img_path.stem  # e.g. dog1_photo1
                individual_id = stem.split("_")[0]  # "dog1", "cat1", "dog2"
                # species is the alpha prefix of the individual_id
                species = "".join(c for c in individual_id if c.isalpha())  # "dog" or "cat"
                samples.append(EvalSample(
                    individual_id=individual_id,
                    image_path=img_path,
                    species=species,
                ))

        # Mock embedder: embed = mean RGB of the image as a float32 vector (768-d padded)
        def mock_embed(img: PILImage.Image) -> np.ndarray:
            arr = np.array(img.convert("RGB"), dtype=np.float32)
            mean_rgb = arr.reshape(-1, 3).mean(axis=0)  # shape (3,)
            # Pad to 768-d to match embedder interface
            vec = np.zeros(768, dtype=np.float32)
            vec[:3] = mean_rgb
            norm = np.linalg.norm(vec)
            return vec / norm if norm > 0 else vec

        # Import index for a real-similarity lookup
        import tempfile
        from homeward_embed.index import EmbedIndex

        gallery, queries = build_gallery_and_queries(samples)
        assert queries, "Need ≥2 images per individual for queries"

        with tempfile.TemporaryDirectory() as tmpdir:
            index = EmbedIndex(index_dir=tmpdir, embed_dim=768)
            for g in gallery:
                img = PILImage.open(g.image_path).convert("RGB")
                vec = mock_embed(img)
                index.enroll(g.individual_id, vec, species=g.species)

            rank1_correct = 0
            for q in queries:
                img = PILImage.open(q.image_path).convert("RGB")
                vec = mock_embed(img)
                results = index.query(vec, k=len(gallery))
                if results and results[0][0] == q.individual_id:
                    rank1_correct += 1

        rank1_accuracy = rank1_correct / len(queries)
        assert rank1_accuracy == pytest.approx(1.0), (
            f"Expected Rank-1 = 1.0 on eval-smoke fixture with mock embedder, "
            f"got {rank1_accuracy:.2f} ({rank1_correct}/{len(queries)} correct)"
        )

    def test_discriminative_ordering_with_mock_embedder(self) -> None:
        """AC3: dog1's query is ranked closer to dog1's gallery than to dog2's.

        With the solid-color fixture and mean-RGB mock embedder, dog1_photo2
        (orange-ish) should be closer to dog1_photo1 (gallery, orange) than to
        dog2_photo1 (gallery, lime-green).
        """
        import tempfile
        from PIL import Image as PILImage

        fixture_dir = self._fixture_dir()
        if not fixture_dir.exists():
            pytest.skip("eval-smoke fixture dir not present")

        dog1_gallery = fixture_dir / "gallery" / "dog1_photo1.png"
        dog2_gallery = fixture_dir / "gallery" / "dog2_photo1.png"
        dog1_query = fixture_dir / "query" / "dog1_photo2.png"

        if not all(p.exists() for p in [dog1_gallery, dog2_gallery, dog1_query]):
            pytest.skip("Expected dog1/dog2 fixture images not present")

        def mock_embed(img: PILImage.Image) -> np.ndarray:
            arr = np.array(img.convert("RGB"), dtype=np.float32)
            mean_rgb = arr.reshape(-1, 3).mean(axis=0)
            vec = np.zeros(768, dtype=np.float32)
            vec[:3] = mean_rgb
            norm = np.linalg.norm(vec)
            return vec / norm if norm > 0 else vec

        from homeward_embed.index import EmbedIndex

        with tempfile.TemporaryDirectory() as tmpdir:
            index = EmbedIndex(index_dir=tmpdir, embed_dim=768)
            index.enroll("dog1", mock_embed(PILImage.open(dog1_gallery).convert("RGB")))
            index.enroll("dog2", mock_embed(PILImage.open(dog2_gallery).convert("RGB")))

            results = index.query(mock_embed(PILImage.open(dog1_query).convert("RGB")), k=2)

        assert results, "No results from index"
        top_id, top_score = results[0]
        assert top_id == "dog1", (
            f"dog1_photo2 should Rank-1 match dog1's gallery, but got {top_id}"
        )


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
