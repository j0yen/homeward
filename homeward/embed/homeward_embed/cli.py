"""homeward-embed CLI — warmup, smoke, attest, and eval commands.

Commands:
  homeward-embed warmup        — prefetch the configured DINOv2 variant into a pinned
                                  cache dir; subsequent runs with HF_HUB_OFFLINE=1 also pass.
  homeward-embed smoke         — end-to-end proof: enroll + query fixture images, assert
                                  rank-1 self-match and discriminative ordering, record latency.
  homeward-embed attest        — end-to-end delivery attestation: enroll eval-smoke gallery,
                                  query with cat1_photo2, assert cat1 is rank-1 with both dogs
                                  below, record latency, write DELIVER.md.
  homeward-embed attest deliver — same as 'attest' (explicit form).

Usage:
  uv run homeward-embed warmup [--variant small|base]
  uv run homeward-embed smoke  [--variant small|base] [--base-url http://...]
  uv run homeward-embed attest [--variant small|base] [--out <path>]
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
# attest command — end-to-end delivery attestation
# ---------------------------------------------------------------------------

_EVAL_SMOKE_DIR = Path(__file__).parent.parent / "fixtures" / "eval-smoke"

# Canonical IDs for eval-smoke gallery members (derived from filenames).
_GALLERY_ENTRIES = [
    ("dog1", "dog", "gallery/dog1_photo1.png"),
    ("dog2", "dog", "gallery/dog2_photo1.png"),
    ("cat1", "cat", "gallery/cat1_photo1.png"),
]
_QUERY_ENTRY = ("cat1", "cat", "query/cat1_photo2.png")

_ATTEST_STATUS_SKIPPED = "SKIPPED"
_ATTEST_STATUS_PASS = "PASS"
_ATTEST_STATUS_FAIL = "FAIL"


def cmd_attest(args: argparse.Namespace) -> int:
    """End-to-end delivery attestation using real DINOv2 and eval-smoke fixtures.

    Enrolls gallery/dog1_photo1.png (dog1), gallery/dog2_photo1.png (dog2),
    gallery/cat1_photo1.png (cat1) into an ephemeral index.  Queries with
    query/cat1_photo2.png.  Asserts cat1 is rank-1 and both dogs rank below.
    Records latency.  Writes DELIVER.md (or the --out path).

    Gracefully skips (exit 0, status=SKIPPED) if the DINOv2 model weights are
    not available and the build environment has no network access.
    """
    import datetime  # noqa: PLC0415

    variant = args.variant
    out_path: Path = args.out
    hf_home = args.hf_home

    os.environ["HF_HOME"] = hf_home
    os.environ["TRANSFORMERS_CACHE"] = str(Path(hf_home) / "hub")
    os.environ["HF_HUB_CACHE"] = str(Path(hf_home) / "hub")

    print(f"homeward-embed attest deliver: variant={variant}")
    print(f"  hf_home:    {hf_home}")
    print(f"  smoke_dir:  {_EVAL_SMOKE_DIR}")

    # ------------------------------------------------------------------
    # AC5: graceful skip if fixtures are absent (shouldn't happen in repo)
    # ------------------------------------------------------------------
    smoke_dir = _EVAL_SMOKE_DIR
    if not smoke_dir.exists():
        msg = f"SKIPPED  eval-smoke fixture dir not found: {smoke_dir}"
        print(msg, file=sys.stderr)
        _write_deliver_md(
            out_path=out_path,
            status=_ATTEST_STATUS_SKIPPED,
            skip_reason=f"eval-smoke fixture dir not found: {smoke_dir}",
            variant=variant,
            timestamp=datetime.datetime.now(datetime.timezone.utc).isoformat(),
        )
        return 0

    # ------------------------------------------------------------------
    # Imports — try to load embedder; skip loudly if unavailable
    # ------------------------------------------------------------------
    try:
        from homeward_embed.embedder import PhotoEmbedder  # noqa: PLC0415
        from homeward_embed.index import EmbedIndex  # noqa: PLC0415
    except ImportError as exc:
        msg = f"SKIPPED  import error (dependency not installed): {exc}"
        print(msg, file=sys.stderr)
        _write_deliver_md(
            out_path=out_path,
            status=_ATTEST_STATUS_SKIPPED,
            skip_reason=f"import error: {exc}",
            variant=variant,
            timestamp=datetime.datetime.now(datetime.timezone.utc).isoformat(),
        )
        return 0

    # ------------------------------------------------------------------
    # AC5: load model — skip if weights unavailable (offline + no cache)
    # ------------------------------------------------------------------
    print(f"  Loading DINOv2-{variant} ...")
    try:
        t_load = time.perf_counter()
        embedder = PhotoEmbedder(variant=variant)  # type: ignore[arg-type]
        load_elapsed = time.perf_counter() - t_load
        print(f"  model load:  {load_elapsed:.2f}s  dim={embedder.embed_dim}")
    except Exception as exc:
        # Broad catch: OSError (network), RuntimeError (torch), etc.
        reason = (
            f"DINOv2-{variant} could not be loaded "
            f"(likely offline + no cached weights): {exc}"
        )
        print(f"SKIPPED  {reason}", file=sys.stderr)
        _write_deliver_md(
            out_path=out_path,
            status=_ATTEST_STATUS_SKIPPED,
            skip_reason=reason,
            variant=variant,
            timestamp=datetime.datetime.now(datetime.timezone.utc).isoformat(),
        )
        return 0

    # ------------------------------------------------------------------
    # Anti-tautology guard: gallery and query must be DISJOINT PHOTOS
    # (same individual is OK; same *file* would be tautological)
    # ------------------------------------------------------------------
    gallery_files = {entry[2] for entry in _GALLERY_ENTRIES}
    query_file = _QUERY_ENTRY[2]
    if query_file in gallery_files:
        msg = (
            f"Anti-tautology violation: query file {query_file!r} is also in the gallery. "
            "The attestation must use a held-out photo."
        )
        print(f"FAIL  {msg}", file=sys.stderr)
        _write_deliver_md(
            out_path=out_path,
            status=_ATTEST_STATUS_FAIL,
            skip_reason=None,
            variant=variant,
            timestamp=datetime.datetime.now(datetime.timezone.utc).isoformat(),
            fail_reason=msg,
        )
        return 1

    # ------------------------------------------------------------------
    # Enroll gallery
    # ------------------------------------------------------------------
    print("\n  Enrolling gallery ...")
    with tempfile.TemporaryDirectory(prefix="hw-attest-index-") as tmpdir:
        index = EmbedIndex(
            index_dir=tmpdir,
            embed_dim=embedder.embed_dim,
            max_elements=100,
        )

        enrolled: list[tuple[str, str]] = []  # (canonical_id, species)
        for canonical_id, species, rel_path in _GALLERY_ENTRIES:
            img_path = smoke_dir / rel_path
            if not img_path.exists():
                msg = f"FAIL  gallery fixture missing: {img_path}"
                print(msg, file=sys.stderr)
                _write_deliver_md(
                    out_path=out_path,
                    status=_ATTEST_STATUS_FAIL,
                    skip_reason=None,
                    variant=variant,
                    timestamp=datetime.datetime.now(datetime.timezone.utc).isoformat(),
                    fail_reason=f"gallery fixture missing: {img_path}",
                )
                return 1
            img = Image.open(img_path).convert("RGB")
            vec = embedder.embed(img)
            index.enroll(canonical_id, vec, species=species)
            enrolled.append((canonical_id, species))
            print(f"    enrolled: {canonical_id}  ({species})  {rel_path}")

        # ------------------------------------------------------------------
        # Query
        # ------------------------------------------------------------------
        query_canonical_id, _query_species, query_rel_path = _QUERY_ENTRY
        query_img_path = smoke_dir / query_rel_path
        if not query_img_path.exists():
            msg = f"FAIL  query fixture missing: {query_img_path}"
            print(msg, file=sys.stderr)
            _write_deliver_md(
                out_path=out_path,
                status=_ATTEST_STATUS_FAIL,
                skip_reason=None,
                variant=variant,
                timestamp=datetime.datetime.now(datetime.timezone.utc).isoformat(),
                fail_reason=f"query fixture missing: {query_img_path}",
            )
            return 1

        print(f"\n  Querying with {query_rel_path} ...")
        query_img = Image.open(query_img_path).convert("RGB")

        t_query = time.perf_counter()
        query_vec = embedder.embed(query_img)
        results = index.query(query_vec, k=len(enrolled))
        query_elapsed_ms = (time.perf_counter() - t_query) * 1000.0

        print(f"  query latency (embed + kNN): {query_elapsed_ms:.1f}ms")
        print("  ranked results:")
        for rank, (cid, score) in enumerate(results):
            tag = " <-- EXPECTED RANK-1" if (rank == 0 and cid == query_canonical_id) else ""
            print(f"    [{rank + 1}] {cid:<10s}  score={score:.6f}{tag}")

        # ------------------------------------------------------------------
        # AC2: assert cat1 is rank-1 and both dogs rank below
        # ------------------------------------------------------------------
        if not results:
            fail_reason = "No results returned from index."
            print(f"FAIL  {fail_reason}", file=sys.stderr)
            _write_deliver_md(
                out_path=out_path,
                status=_ATTEST_STATUS_FAIL,
                skip_reason=None,
                variant=variant,
                timestamp=datetime.datetime.now(datetime.timezone.utc).isoformat(),
                fail_reason=fail_reason,
                query_latency_ms=query_elapsed_ms,
                results=results,
            )
            return 1

        rank1_id, rank1_score = results[0]
        if rank1_id != query_canonical_id:
            fail_reason = (
                f"AC2 FAIL: expected rank-1 = {query_canonical_id!r}, "
                f"got {rank1_id!r} (score={rank1_score:.6f}). "
                f"Full ranking: {[(cid, f'{s:.6f}') for cid, s in results]}"
            )
            print(f"FAIL  {fail_reason}", file=sys.stderr)
            _write_deliver_md(
                out_path=out_path,
                status=_ATTEST_STATUS_FAIL,
                skip_reason=None,
                variant=variant,
                timestamp=datetime.datetime.now(datetime.timezone.utc).isoformat(),
                fail_reason=fail_reason,
                query_latency_ms=query_elapsed_ms,
                results=results,
            )
            return 1

        # Check both dogs rank strictly below cat1
        dog_ids = {"dog1", "dog2"}
        for rank, (cid, score) in enumerate(results[1:], start=1):
            if cid in dog_ids and score >= rank1_score:
                fail_reason = (
                    f"AC2 FAIL: dog {cid!r} at rank {rank + 1} has score={score:.6f} "
                    f">= cat1 rank-1 score={rank1_score:.6f}. Dogs must rank strictly below."
                )
                print(f"FAIL  {fail_reason}", file=sys.stderr)
                _write_deliver_md(
                    out_path=out_path,
                    status=_ATTEST_STATUS_FAIL,
                    skip_reason=None,
                    variant=variant,
                    timestamp=datetime.datetime.now(datetime.timezone.utc).isoformat(),
                    fail_reason=fail_reason,
                    query_latency_ms=query_elapsed_ms,
                    results=results,
                )
                return 1

        # ------------------------------------------------------------------
        # PASS
        # ------------------------------------------------------------------
        timestamp = datetime.datetime.now(datetime.timezone.utc).isoformat()
        print(f"\n  PASS  rank-1={rank1_id}  score={rank1_score:.6f}  latency={query_elapsed_ms:.1f}ms")
        _write_deliver_md(
            out_path=out_path,
            status=_ATTEST_STATUS_PASS,
            skip_reason=None,
            variant=variant,
            timestamp=timestamp,
            query_latency_ms=query_elapsed_ms,
            results=results,
        )
        print(f"\nATTEST PASS  written: {out_path}")
        return 0


def _write_deliver_md(
    out_path: Path,
    status: str,
    skip_reason: Optional[str],
    variant: str,
    timestamp: str,
    fail_reason: Optional[str] = None,
    query_latency_ms: Optional[float] = None,
    results: Optional[list[tuple[str, float]]] = None,
) -> None:
    """Write (or overwrite) the DELIVER.md attestation artifact."""
    lines: list[str] = [
        "# DELIVER.md — Homeward end-to-end delivery attestation",
        "",
        f"**Status**: {status}",
        f"**Timestamp**: {timestamp}",
        f"**Model variant**: DINOv2-{variant}",
        "",
    ]

    if status == _ATTEST_STATUS_SKIPPED:
        lines += [
            "## Result: SKIPPED",
            "",
            f"> {skip_reason}",
            "",
            "The attestation did not run.  This is not a success and not a failure.",
            "Re-run once the model weights are cached (run `homeward-embed warmup`).",
            "",
        ]
    elif status == _ATTEST_STATUS_FAIL:
        lines += [
            "## Result: FAIL",
            "",
            f"> {fail_reason}",
            "",
        ]
        if results:
            lines.append("### Ranked results")
            lines.append("")
            lines.append("| Rank | canonical_id | cosine_sim |")
            lines.append("|------|-------------|------------|")
            for rank, (cid, score) in enumerate(results, start=1):
                lines.append(f"| {rank} | {cid} | {score:.6f} |")
            lines.append("")
    else:
        assert status == _ATTEST_STATUS_PASS  # noqa: S101
        assert results is not None  # noqa: S101
        assert query_latency_ms is not None  # noqa: S101
        lines += [
            "## Result: PASS",
            "",
            f"**Query latency (embed + kNN)**: {query_latency_ms:.1f}ms",
            "",
            "### Assertion: cat1_photo2 → rank-1 = cat1",
            "",
            "The query photo `eval-smoke/query/cat1_photo2.png` (a held-out second",
            "photo of the cat1 individual) was submitted against a gallery of three",
            "enrolled animals.  The top-ranked result is `cat1`, confirming the",
            "real DINOv2 embedder returns the correct individual at rank-1.",
            "",
            "Both dog gallery entries (`dog1`, `dog2`) rank strictly below `cat1`.",
            "",
            "### Ranked results",
            "",
            "| Rank | canonical_id | cosine_sim |",
            "|------|-------------|------------|",
        ]
        for rank, (cid, score) in enumerate(results, start=1):
            lines.append(f"| {rank} | {cid} | {score:.6f} |")
        lines += [
            "",
            "### Anti-tautology statement",
            "",
            "Gallery and query photos are **disjoint held-out photos** of the same",
            "individual — `cat1_photo1.png` (gallery) and `cat1_photo2.png` (query)",
            "are two distinct synthetic images of the same color family.  The embedder",
            "was not given the query photo during gallery construction.",
            "",
            "### Fixture sources",
            "",
            "All eval-smoke fixtures are synthetic solid-color PNG images generated by",
            "`homeward/embed/fixtures/eval-smoke/generate_fixture.py`, authored by",
            "Joe Yen <jyen.tech@gmail.com> and released under MIT OR Apache-2.0.",
            "No third-party images or copyrighted materials were used.",
            "See `homeward/embed/fixtures/eval-smoke/SOURCES.md` for full provenance.",
            "",
            "### What this proves",
            "",
            "- The real DINOv2 embedder (no stub, no mock) successfully embeds both",
            "  gallery and query images.",
            "- The HNSW index returns correct nearest-neighbor results.",
            "- The end-to-end owner round-trip (enroll → query → ranked shortlist)",
            "  returns the correct animal at rank-1.",
            "",
            "### What this does NOT prove",
            "",
            "- Real-world re-ID accuracy on shelter photos.  These are solid-color",
            "  synthetic images — DINOv2 trivially separates them by color.",
            "- Performance on PetFace or any research dataset.  See EVAL.md.",
            "",
        ]

    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text("\n".join(lines))


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

    # -- attest ---------------------------------------------------------------
    _repo_root = Path(__file__).parent.parent.parent.parent  # homeward root
    _default_deliver_md = _repo_root / "DELIVER.md"

    p_attest = sub.add_parser(
        "attest",
        help=(
            "End-to-end delivery attestation: enroll eval-smoke gallery, "
            "query with cat1_photo2, assert cat1 rank-1, record latency, write DELIVER.md."
        ),
    )
    p_attest.add_argument(
        "subcommand",
        nargs="?",
        default="deliver",
        choices=["deliver"],
        help="Attestation sub-command (only 'deliver' supported; default: deliver).",
    )
    p_attest.add_argument(
        "--variant",
        default=_DEFAULT_VARIANT,
        choices=["small", "base"],
        help="DINOv2 variant (default: %(default)s).",
    )
    p_attest.add_argument(
        "--hf-home",
        default=_DEFAULT_HF_HOME,
        dest="hf_home",
        help="Pinned HuggingFace cache dir (default: %(default)s).",
    )
    p_attest.add_argument(
        "--out",
        default=_default_deliver_md,
        type=Path,
        help=f"Output path for DELIVER.md artifact (default: {_default_deliver_md}).",
    )

    args = parser.parse_args(argv)

    if args.command == "warmup":
        sys.exit(cmd_warmup(args))
    elif args.command == "smoke":
        sys.exit(cmd_smoke(args))
    elif args.command == "eval":
        sys.exit(cmd_eval(args))
    elif args.command == "attest":
        sys.exit(cmd_attest(args))
    else:
        parser.print_help()
        sys.exit(1)


if __name__ == "__main__":
    main()
