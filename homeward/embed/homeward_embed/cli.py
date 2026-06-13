"""homeward-embed CLI — warmup and smoke commands.

Commands:
  homeward-embed warmup   — prefetch the configured DINOv2 variant into a pinned
                            cache dir; subsequent runs with HF_HUB_OFFLINE=1 also pass.
  homeward-embed smoke    — end-to-end proof: enroll + query fixture images, assert
                            rank-1 self-match and discriminative ordering, record latency.

Usage:
  uv run homeward-embed warmup [--variant small|base]
  uv run homeward-embed smoke  [--variant small|base] [--base-url http://...]
  uv run python -m homeward_embed.cli warmup
"""

from __future__ import annotations

import argparse
import base64
import io
import json
import logging
import os
import sys
import tempfile
import time
from pathlib import Path
from typing import Optional

import numpy as np
from PIL import Image

logger = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Defaults
# ---------------------------------------------------------------------------

_DEFAULT_VARIANT = os.environ.get("HOMEWARD_EMBED_VARIANT", "small")
_DEFAULT_BASE_URL = os.environ.get("HW_EMBED_BASE_URL", "http://127.0.0.1:8741")

# Pinned cache dir for model weights.  Mirrors the env var checked by
# transformers / huggingface_hub.
_DEFAULT_HF_HOME = os.environ.get(
    "HF_HOME",
    str(Path.home() / ".cache" / "homeward" / "hf"),
)


# ---------------------------------------------------------------------------
# Fixture helpers
# ---------------------------------------------------------------------------

def _fixture_dir() -> Path:
    """Return path to the bundled fixture directory."""
    return Path(__file__).parent.parent / "fixtures"


def _generate_synthetic_fixtures(out_dir: Path) -> list[Path]:
    """Generate a small set of synthetic fixture images (solid-colour squares).

    Each "individual" gets a distinct colour so the embedding model (which
    encodes texture/colour) produces distinct vectors — enough for the rank-
    ordering smoke assertion.  These are used as a fallback when the bundled
    JPEG fixtures are absent (e.g. fresh clone without LFS).

    Colours chosen to be perceptually far apart in RGB space so DINOv2 CLS
    tokens will differ significantly.
    """
    out_dir.mkdir(parents=True, exist_ok=True)
    palette = [
        ("dog_retriever", (210, 140, 60)),   # golden-tan
        ("dog_dalmatian", (240, 240, 240)),   # white (dalmatian ground)
        ("cat_tabby", (100, 80, 50)),         # dark brown-grey
    ]
    paths: list[Path] = []
    for name, colour in palette:
        arr = np.full((224, 224, 3), colour, dtype=np.uint8)
        img = Image.fromarray(arr, mode="RGB")
        p = out_dir / f"{name}.png"
        img.save(p)
        paths.append(p)
    return paths


def _load_fixtures() -> list[Path]:
    """Return fixture image paths.

    Prefers real JPEG fixtures from ``homeward/embed/fixtures/``.
    Falls back to generating synthetic PNG fixtures in a temp dir.
    """
    fixture_dir = _fixture_dir()
    if fixture_dir.exists():
        images = sorted(
            p for p in fixture_dir.iterdir()
            if p.suffix.lower() in {".jpg", ".jpeg", ".png"}
        )
        if len(images) >= 2:
            return images

    logger.warning(
        "Bundled fixtures not found at %s; using synthetic colour squares.", fixture_dir
    )
    tmp = Path(tempfile.mkdtemp(prefix="hw-fixtures-"))
    return _generate_synthetic_fixtures(tmp)


def _image_to_b64(path: Path) -> str:
    """Read an image file and return a base64-encoded string."""
    return base64.b64encode(path.read_bytes()).decode()


# ---------------------------------------------------------------------------
# warmup command
# ---------------------------------------------------------------------------

def cmd_warmup(args: argparse.Namespace) -> int:
    """Load the DINOv2 variant into the pinned cache dir and exit 0."""
    variant = args.variant
    hf_home = args.hf_home

    # Point transformers at our pinned cache dir.
    os.environ["HF_HOME"] = hf_home
    os.environ["TRANSFORMERS_CACHE"] = str(Path(hf_home) / "hub")
    os.environ["HF_HUB_CACHE"] = str(Path(hf_home) / "hub")

    offline = os.environ.get("HF_HUB_OFFLINE", "0") == "1"

    print(f"homeward-embed warmup: variant={variant} hf_home={hf_home} offline={offline}")

    try:
        from homeward_embed.embedder import PhotoEmbedder  # noqa: PLC0415

        t0 = time.perf_counter()
        embedder = PhotoEmbedder(variant=variant)  # type: ignore[arg-type]
        elapsed = time.perf_counter() - t0

        print(
            f"OK  DINOv2-{variant} loaded in {elapsed:.1f}s "
            f"(embed_dim={embedder.embed_dim})"
        )

        # Quick sanity: embed a blank image to confirm forward pass works.
        blank = Image.fromarray(np.zeros((224, 224, 3), dtype=np.uint8), mode="RGB")
        vec = embedder.embed(blank)
        norm = float(np.linalg.norm(vec))
        print(f"OK  Forward pass: shape={vec.shape} norm={norm:.4f}")

        return 0

    except Exception as exc:
        print(f"FAIL  warmup error: {exc}", file=sys.stderr)
        logger.exception("warmup failed")
        return 1


# ---------------------------------------------------------------------------
# smoke command (standalone — no live sidecar required)
# ---------------------------------------------------------------------------

def cmd_smoke(args: argparse.Namespace) -> int:
    """End-to-end smoke: enroll fixture images, query, assert rank-1 + ordering."""
    variant = args.variant
    hf_home = args.hf_home

    os.environ["HF_HOME"] = hf_home
    os.environ["TRANSFORMERS_CACHE"] = str(Path(hf_home) / "hub")
    os.environ["HF_HUB_CACHE"] = str(Path(hf_home) / "hub")

    print(f"homeward-embed smoke: variant={variant}")

    try:
        from homeward_embed.embedder import PhotoEmbedder  # noqa: PLC0415
        from homeward_embed.index import EmbedIndex  # noqa: PLC0415
    except ImportError as exc:
        print(f"FAIL  import error: {exc}", file=sys.stderr)
        return 1

    # -----------------------------------------------------------------------
    # Load model
    # -----------------------------------------------------------------------
    try:
        t_load = time.perf_counter()
        embedder = PhotoEmbedder(variant=variant)  # type: ignore[arg-type]
        load_elapsed = time.perf_counter() - t_load
        print(f"  model load:  {load_elapsed:.2f}s")
    except Exception as exc:
        print(f"FAIL  model load error: {exc}", file=sys.stderr)
        return 1

    # -----------------------------------------------------------------------
    # Load fixture images
    # -----------------------------------------------------------------------
    fixture_paths = _load_fixtures()
    if len(fixture_paths) < 2:
        print("FAIL  need at least 2 fixture images for rank-ordering assertion", file=sys.stderr)
        return 1

    print(f"  fixtures:    {[p.name for p in fixture_paths]}")

    # -----------------------------------------------------------------------
    # Build in-memory index and enroll all fixtures
    # -----------------------------------------------------------------------
    with tempfile.TemporaryDirectory(prefix="hw-smoke-index-") as tmpdir:
        index = EmbedIndex(
            index_dir=tmpdir,
            embed_dim=embedder.embed_dim,
            max_elements=1000,
        )

        enrolled_ids: list[str] = []
        for fp in fixture_paths:
            img = Image.open(fp).convert("RGB")
            vec = embedder.embed(img)
            canonical_id = fp.stem  # use filename without extension as ID
            index.enroll(canonical_id, vec)
            enrolled_ids.append(canonical_id)
            print(f"  enrolled:    {canonical_id}")

        # -----------------------------------------------------------------------
        # Query with fixture[0] — assert rank-1 is itself
        # -----------------------------------------------------------------------
        query_img = Image.open(fixture_paths[0]).convert("RGB")

        t_query = time.perf_counter()
        query_vec = embedder.embed(query_img)
        results = index.query(query_vec, k=len(fixture_paths))
        query_elapsed = time.perf_counter() - t_query

        print(f"\n  query latency (embed+kNN): {query_elapsed * 1000:.1f}ms")
        print("  results:")
        for rank, (cid, score) in enumerate(results):
            marker = " <-- rank-1" if rank == 0 else ""
            print(f"    [{rank+1}] {cid:<30s}  score={score:.4f}{marker}")

        # Assertion 1: rank-1 is the enrolled fixture[0]
        if not results:
            print("FAIL  no results returned", file=sys.stderr)
            return 1

        rank1_id, rank1_score = results[0]
        expected_id = fixture_paths[0].stem
        if rank1_id != expected_id:
            print(
                f"FAIL  rank-1 is {rank1_id!r}, expected {expected_id!r}",
                file=sys.stderr,
            )
            return 1
        print(f"\n  PASS  rank-1 self-match: {rank1_id}  score={rank1_score:.4f}")

        # Assertion 2: a different fixture scores strictly lower than the self-match
        if len(results) >= 2:
            rank2_id, rank2_score = results[1]
            if rank2_score >= rank1_score:
                print(
                    f"FAIL  discriminative assertion: {rank2_id} score={rank2_score:.4f} "
                    f">= self score={rank1_score:.4f} — vectors may be constant",
                    file=sys.stderr,
                )
                return 1
            print(
                f"  PASS  discriminative: self={rank1_score:.4f} > other={rank2_score:.4f}"
            )
        else:
            print("  SKIP  only 1 result — cannot assert discriminative ordering")

        print(f"\nSMOKE PASS  query_latency_ms={query_elapsed * 1000:.1f}")
        return 0


# ---------------------------------------------------------------------------
# eval command
# ---------------------------------------------------------------------------

def cmd_eval(args: argparse.Namespace) -> int:
    """Run held-out retrieval evaluation and write results to dataset_dir/eval-results.json."""
    import json as _json

    dataset_dir: Path = args.dataset_dir
    ks = tuple(int(k.strip()) for k in args.ks.split(","))
    embed_variant: str = args.embed_variant

    # Set up HF cache env so offline mode works if weights already cached.
    hf_home = _DEFAULT_HF_HOME
    os.environ["HF_HOME"] = hf_home
    os.environ["TRANSFORMERS_CACHE"] = str(Path(hf_home) / "hub")
    os.environ["HF_HUB_CACHE"] = str(Path(hf_home) / "hub")

    training_ids: Optional[set[str]] = None
    if args.training_ids_file:
        training_ids = set(_json.loads(args.training_ids_file.read_text()))

    # Import eval harness
    try:
        from homeward_embed.eval import _load_samples_from_dir, run_eval  # noqa: PLC0415
    except ImportError as exc:
        print(f"FAIL  import error: {exc}", file=sys.stderr)
        return 1

    samples = _load_samples_from_dir(dataset_dir)
    if not samples:
        print(f"FAIL  No samples found in {dataset_dir}", file=sys.stderr)
        return 1
    print(f"Loaded {len(samples)} samples from {dataset_dir}")

    try:
        result = run_eval(
            samples=samples,
            embed_variant=embed_variant,
            ks=ks,
            training_individual_ids=training_ids,
            species_filter=args.species,
        )
    except ValueError as exc:
        print(f"FAIL  Evaluation aborted: {exc}", file=sys.stderr)
        return 2

    # Human summary
    print(f"\nEvaluation results ({result.n_queries} queries, {result.n_gallery} gallery):")
    if 1 in ks:
        print(f"  Rank-1  : {result.rank1:.4f}")
    if 5 in ks:
        print(f"  Rank-5  : {result.rank5:.4f}")
    if 20 in ks:
        print(f"  Rank-20 : {result.rank20:.4f}")
    print(f"  mAP     : {result.mAP:.4f}")
    print(f"  variant : {result.embed_variant}")

    # Write JSON result
    out_path = dataset_dir / "eval-results.json"
    payload = {
        "rank1": result.rank1,
        "rank5": result.rank5,
        "rank20": result.rank20,
        "mAP": result.mAP,
        "n_queries": result.n_queries,
        "n_gallery": result.n_gallery,
        "embed_variant": result.embed_variant,
        "ks": list(ks),
    }
    out_path.write_text(_json.dumps(payload, indent=2) + "\n")
    print(f"\nWrote {out_path}")
    return 0


# ---------------------------------------------------------------------------
# CLI entry point
# ---------------------------------------------------------------------------

def main(argv: Optional[list[str]] = None) -> None:
    logging.basicConfig(
        level=logging.WARNING,
        format="%(levelname)s %(name)s: %(message)s",
    )

    parser = argparse.ArgumentParser(
        prog="homeward-embed",
        description="homeward-embed CLI: warmup and smoke commands.",
    )
    sub = parser.add_subparsers(dest="command", required=True)

    # -- warmup ---------------------------------------------------------------
    p_warmup = sub.add_parser(
        "warmup",
        help="Prefetch DINOv2 model weights into pinned cache dir.",
    )
    p_warmup.add_argument(
        "--variant",
        default=_DEFAULT_VARIANT,
        choices=["small", "base"],
        help="DINOv2 variant (default: %(default)s; override with HOMEWARD_EMBED_VARIANT).",
    )
    p_warmup.add_argument(
        "--hf-home",
        default=_DEFAULT_HF_HOME,
        dest="hf_home",
        help="Pinned HuggingFace cache dir (default: %(default)s).",
    )

    # -- smoke ----------------------------------------------------------------
    p_smoke = sub.add_parser(
        "smoke",
        help="End-to-end smoke test: enroll fixture images, assert rank-1 self-match.",
    )
    p_smoke.add_argument(
        "--variant",
        default=_DEFAULT_VARIANT,
        choices=["small", "base"],
        help="DINOv2 variant (default: %(default)s).",
    )
    p_smoke.add_argument(
        "--hf-home",
        default=_DEFAULT_HF_HOME,
        dest="hf_home",
        help="Pinned HuggingFace cache dir (default: %(default)s).",
    )

    # -- eval -----------------------------------------------------------------
    p_eval = sub.add_parser(
        "eval",
        help="Run held-out retrieval evaluation on a labeled dataset directory.",
    )
    p_eval.add_argument(
        "--dataset-dir",
        required=True,
        type=Path,
        dest="dataset_dir",
        help=(
            "Path to dataset directory.  Supports labels.jsonl or hierarchical "
            "<species>/<individual_id>/<image> layout."
        ),
    )
    p_eval.add_argument(
        "--ks",
        default="1,5,20",
        help="Comma-separated Rank-k values to compute (default: %(default)s).",
    )
    p_eval.add_argument(
        "--embed-variant",
        default=_DEFAULT_VARIANT,
        choices=["small", "base"],
        dest="embed_variant",
        help="DINOv2 variant (default: %(default)s).",
    )
    p_eval.add_argument(
        "--species",
        choices=["dog", "cat"],
        default=None,
        help="Restrict evaluation to this species (default: all).",
    )
    p_eval.add_argument(
        "--training-ids-file",
        type=Path,
        default=None,
        dest="training_ids_file",
        help="JSON list of individual-IDs seen during embedder training.",
    )

    args = parser.parse_args(argv)

    if args.command == "warmup":
        sys.exit(cmd_warmup(args))
    elif args.command == "smoke":
        sys.exit(cmd_smoke(args))
    elif args.command == "eval":
        sys.exit(cmd_eval(args))
    else:
        parser.print_help()
        sys.exit(1)


if __name__ == "__main__":
    main()
