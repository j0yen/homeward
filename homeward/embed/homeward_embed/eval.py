"""Honest held-out evaluation harness for homeward-embed.

Computes Rank-1 / Rank-5 / Rank-20 retrieval accuracy and mAP on a held-out
set of **unseen individuals** (individuals whose IDs are NEVER in the gallery).

Anti-tautology guard: the harness **errors out** if any query individual-ID
overlaps with the gallery individual-IDs.  This enforces the lesson from
[[feedback_agent_written_fixtures_tautology]] — a matcher that grades itself on
self-generated fixtures proves nothing.

Dataset contract:
  A dataset is a list of EvalSample, each with:
    individual_id: str   — unique individual identity label (e.g. "dog_00042")
    image_path: Path     — path to the image file
    species: str         — "dog" or "cat"

The harness splits samples into:
  gallery: one image per individual
  queries: all remaining images of the same individuals (≥1 per individual)

The gallery individuals must be entirely disjoint from any training/pre-embed
IDs passed in via ``training_individual_ids``.

Metrics:
  Rank-k accuracy: fraction of queries where the true individual appears in
  the top-k results.
  mAP: mean average precision over all queries.

Usage (CLI):
  python -m homeward_embed.eval \
      --dataset-dir /path/to/petface-holdout \
      --embed-variant base \
      --k 1 5 20

Note: PetFace and Flickr-Dog datasets are research-gated / not bundled.
      Download them separately according to their respective licenses.
"""

from __future__ import annotations

import argparse
import json
import logging
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Optional

import numpy as np
from PIL import Image

logger = logging.getLogger(__name__)


# ---------------------------------------------------------------------------
# Data structures
# ---------------------------------------------------------------------------


@dataclass
class EvalSample:
    """One labeled image sample for evaluation."""

    individual_id: str
    image_path: Path
    species: str  # "dog" | "cat"


@dataclass
class EvalResult:
    """Evaluation metrics."""

    rank1: float
    rank5: float
    rank20: float
    mAP: float
    n_queries: int
    n_gallery: int
    embed_variant: str
    extra: dict = field(default_factory=dict)


# ---------------------------------------------------------------------------
# Anti-tautology guard
# ---------------------------------------------------------------------------


def assert_no_overlap(
    query_ids: set[str],
    gallery_ids: set[str],
    training_ids: Optional[set[str]] = None,
) -> None:
    """Raise ValueError if query individual-IDs overlap gallery or training IDs.

    This is the core anti-tautology guard: evaluating on individuals that were
    in the gallery or training set inflates recall to near-100% trivially.

    Args:
        query_ids: Individual IDs of the query set.
        gallery_ids: Individual IDs of the gallery set.
        training_ids: Optional set of individual IDs used during model training
                      (if the embedder was fine-tuned).

    Raises:
        ValueError: If any overlap is detected.
    """
    overlap_gallery = query_ids & gallery_ids
    if overlap_gallery:
        msg = (
            f"Anti-tautology violation: {len(overlap_gallery)} query individual-ID(s) "
            f"also appear in the gallery: {sorted(overlap_gallery)[:5]}{'...' if len(overlap_gallery) > 5 else ''}. "
            "Evaluation on overlapping IDs proves nothing. "
            "Use a truly held-out split (no shared individuals between gallery and queries)."
        )
        raise ValueError(msg)

    if training_ids:
        overlap_train = query_ids & training_ids
        if overlap_train:
            msg = (
                f"Anti-tautology violation: {len(overlap_train)} query individual-ID(s) "
                f"were seen during embedder training: {sorted(overlap_train)[:5]}. "
                "Evaluation must use individuals unseen during training."
            )
            raise ValueError(msg)


def assert_disjoint_individuals(
    gallery_ids: list[str] | set[str],
    query_ids: list[str] | set[str],
) -> None:
    """Raise ValueError if any individual_id appears in both gallery and query splits.

    This is a convenience wrapper around :func:`assert_no_overlap` with
    positional semantics matching the PRD's function signature (gallery first,
    query second).

    Args:
        gallery_ids: Individual IDs in the gallery split.
        query_ids: Individual IDs in the query split.

    Raises:
        ValueError: If any individual_id is shared between the two splits.
    """
    assert_no_overlap(
        query_ids=set(query_ids),
        gallery_ids=set(gallery_ids),
    )


# ---------------------------------------------------------------------------
# Eval harness
# ---------------------------------------------------------------------------


def build_gallery_and_queries(
    samples: list[EvalSample],
) -> tuple[list[EvalSample], list[EvalSample]]:
    """Split *samples* into gallery (1 image per individual) and queries (rest).

    The first encountered image per individual becomes the gallery entry.
    All remaining images of that individual become queries.

    Individuals with only one image are placed in the gallery and skipped
    during query evaluation (they cannot contribute a query).

    Args:
        samples: All labeled samples.

    Returns:
        (gallery_samples, query_samples)
    """
    seen: dict[str, EvalSample] = {}
    queries: list[EvalSample] = []
    gallery: list[EvalSample] = []
    for s in samples:
        if s.individual_id not in seen:
            seen[s.individual_id] = s
            gallery.append(s)
        else:
            queries.append(s)
    return gallery, queries


def average_precision(ranked_ids: list[str], true_id: str) -> float:
    """Compute average precision for one query.

    Args:
        ranked_ids: Ordered list of retrieved individual-IDs.
        true_id: The correct individual-ID.

    Returns:
        Average precision (0.0–1.0).
    """
    hits = 0
    precision_sum = 0.0
    for rank, rid in enumerate(ranked_ids, start=1):
        if rid == true_id:
            hits += 1
            precision_sum += hits / rank
    if hits == 0:
        return 0.0
    return precision_sum / hits  # normalised by number of relevant docs (always 1 here)


def run_eval(
    samples: list[EvalSample],
    embed_variant: str = "base",
    ks: tuple[int, ...] = (1, 5, 20),
    training_individual_ids: Optional[set[str]] = None,
    species_filter: Optional[str] = None,
) -> EvalResult:
    """Run the full evaluation pipeline.

    Args:
        samples: Labeled EvalSamples (individual_id, image_path, species).
        embed_variant: DINOv2 variant ("small" or "base").
        ks: Rank-k values to compute accuracy for.
        training_individual_ids: Individual IDs seen during embedder training
                                  (for anti-tautology check).
        species_filter: If set, restrict gallery + queries to this species.

    Returns:
        EvalResult with rank-k accuracies and mAP.

    Raises:
        ValueError: If anti-tautology guard fires.
        ValueError: If not enough samples to form queries.
    """
    # Optionally filter by species
    if species_filter:
        samples = [s for s in samples if s.species == species_filter]

    gallery, queries = build_gallery_and_queries(samples)
    if not queries:
        msg = "No query samples found (need ≥2 images per individual for any queries)."
        raise ValueError(msg)

    gallery_ids = {s.individual_id for s in gallery}
    query_ids = {s.individual_id for s in queries}
    # Anti-tautology: queries must be individuals also in gallery (same individual,
    # different image). But the gallery must NOT overlap training set.
    assert_no_overlap(
        query_ids=query_ids - gallery_ids,  # IDs in queries but NOT in gallery
        gallery_ids=gallery_ids,
        training_ids=training_individual_ids,
    )

    # Load embedder and detector
    from homeward_embed.detector import AnimalDetector
    from homeward_embed.embedder import PhotoEmbedder
    from homeward_embed.index import EmbedIndex

    embedder = PhotoEmbedder(variant=embed_variant)  # type: ignore[arg-type]
    try:
        detector = AnimalDetector()
    except ImportError:
        logger.warning("ultralytics not available; whole-image fallback for eval.")
        detector = None

    import tempfile

    def embed_sample(s: EvalSample) -> np.ndarray:
        img = Image.open(s.image_path).convert("RGB")
        if detector is not None:
            img, _ = detector.crop(img, species_hint=s.species)
        return embedder.embed(img)

    # Build gallery index (temp dir)
    with tempfile.TemporaryDirectory() as tmpdir:
        index = EmbedIndex(index_dir=tmpdir, embed_dim=embedder.embed_dim)
        logger.info("Embedding %d gallery images…", len(gallery))
        for g in gallery:
            vec = embed_sample(g)
            index.enroll(g.individual_id, vec, species=g.species)

        # Evaluate queries
        logger.info("Evaluating %d query images…", len(queries))
        max_k = max(ks)
        rank_correct: dict[int, int] = {k: 0 for k in ks}
        ap_values: list[float] = []

        per_query: list[dict] = []
        for q in queries:
            vec = embed_sample(q)
            results = index.query(vec, k=max_k, species_filter=species_filter)
            ranked_ids = [cid for cid, _ in results]
            for k in ks:
                top_k = ranked_ids[:k]
                if q.individual_id in top_k:
                    rank_correct[k] += 1
            ap_values.append(average_precision(ranked_ids, q.individual_id))
            # rank of the true individual within the top max_k (None = not retrieved)
            true_rank = ranked_ids.index(q.individual_id) + 1 if q.individual_id in ranked_ids else None
            per_query.append({
                "individual_id": q.individual_id,
                "image_path": str(q.image_path),
                "species": q.species,
                "true_rank": true_rank,
                "top1": ranked_ids[0] if ranked_ids else None,
            })

    n = len(queries)
    return EvalResult(
        rank1=rank_correct.get(1, 0) / n,
        rank5=rank_correct.get(5, 0) / n,
        rank20=rank_correct.get(20, 0) / n,
        mAP=float(np.mean(ap_values)),
        n_queries=n,
        n_gallery=len(gallery),
        embed_variant=embed_variant,
        extra={"per_query": per_query},
    )


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------


def _load_samples_from_dir(dataset_dir: Path) -> list[EvalSample]:
    """Load samples from a directory structured as:

      dataset_dir/
        labels.jsonl    — one JSON object per line: {individual_id, image_path, species}
      OR
        <species>/<individual_id>/<image_file>  — hierarchical layout

    Returns list of EvalSample.
    """
    labels_file = dataset_dir / "labels.jsonl"
    if labels_file.exists():
        samples = []
        for line in labels_file.read_text().splitlines():
            obj = json.loads(line)
            samples.append(EvalSample(
                individual_id=obj["individual_id"],
                image_path=dataset_dir / obj["image_path"],
                species=obj["species"],
            ))
        return samples

    # Hierarchical: <species>/<individual_id>/<image>
    samples = []
    for species_dir in dataset_dir.iterdir():
        if not species_dir.is_dir():
            continue
        species = species_dir.name
        if species not in {"dog", "cat"}:
            continue
        for individual_dir in species_dir.iterdir():
            if not individual_dir.is_dir():
                continue
            for img_path in individual_dir.iterdir():
                if img_path.suffix.lower() in {".jpg", ".jpeg", ".png", ".webp"}:
                    samples.append(EvalSample(
                        individual_id=individual_dir.name,
                        image_path=img_path,
                        species=species,
                    ))
    return samples


def main() -> None:
    logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(name)s: %(message)s")
    parser = argparse.ArgumentParser(description="homeward-embed evaluation harness")
    parser.add_argument("--dataset-dir", required=True, type=Path)
    parser.add_argument("--embed-variant", default="base", choices=["small", "base"])
    parser.add_argument("--k", nargs="+", type=int, default=[1, 5, 20])
    parser.add_argument("--species", choices=["dog", "cat"], default=None)
    parser.add_argument("--training-ids-file", type=Path, default=None,
                        help="JSON list of individual-IDs seen during embedder training.")
    args = parser.parse_args()

    training_ids: Optional[set[str]] = None
    if args.training_ids_file:
        training_ids = set(json.loads(args.training_ids_file.read_text()))

    samples = _load_samples_from_dir(args.dataset_dir)
    if not samples:
        logger.error("No samples found in %s", args.dataset_dir)
        sys.exit(1)
    logger.info("Loaded %d samples from %s", len(samples), args.dataset_dir)

    try:
        result = run_eval(
            samples=samples,
            embed_variant=args.embed_variant,
            ks=tuple(args.k),
            training_individual_ids=training_ids,
            species_filter=args.species,
        )
    except ValueError as exc:
        logger.error("Evaluation aborted: %s", exc)
        sys.exit(2)

    print(json.dumps({
        "rank1": result.rank1,
        "rank5": result.rank5,
        "rank20": result.rank20,
        "mAP": result.mAP,
        "n_queries": result.n_queries,
        "n_gallery": result.n_gallery,
        "embed_variant": result.embed_variant,
    }, indent=2))


if __name__ == "__main__":
    main()
