# EVAL.md — Evaluation harness and accuracy record

## What this file records

1. **Harness correctness**: The bundled `eval-smoke` fixture validates that the
   harness *arithmetic* is correct (Rank-1 self-match, discriminative ordering).
   This is NOT a real-world accuracy claim.
2. **Measured accuracy**: Results from running the harness against held-out datasets.
3. **How to obtain PetFace** and run the headline accuracy figure.

---

## eval-smoke fixture — harness arithmetic only

The fixture at `homeward/embed/fixtures/eval-smoke/` contains synthetic
solid-color PNG images.  It proves two things:

- **Rank-1 self-match**: A query image similar to its gallery counterpart
  retrieves the correct individual at Rank-1.
- **Discriminative ordering**: A clearly-different individual (lime-green vs.
  orange) is ranked below the correct match.

**This fixture does NOT prove real-world re-ID accuracy.**  The solid-color
images bear no resemblance to actual shelter-pet photographs.

### Running the fixture

```bash
cd homeward/embed
uv run homeward-embed eval \
    --dataset-dir fixtures/eval-smoke-flat \
    --ks 1,5,20
```

> Note: `eval-smoke` uses an explicit gallery/query split layout.  For the CLI,
> convert to a flat `<species>/<individual_id>/<image>` layout or provide a
> `labels.jsonl` (see `_load_samples_from_dir` in `eval.py`).

### Automated test

```bash
cd homeward/embed
uv run pytest tests/test_eval.py::TestFixtureCorrectness -v
```

---

## Measured accuracy — this box (DINOv2 ViT-S/14, no fine-tune)

| Dataset | Rank-1 | Rank-5 | Rank-20 | mAP | Date | Notes |
|---------|--------|--------|---------|-----|------|-------|
| eval-smoke (synthetic, harness-only) | N/A | N/A | N/A | N/A | — | Not a real accuracy claim |
| PetFace held-out | _pending_ | _pending_ | _pending_ | _pending_ | — | See below |

---

## How to run against PetFace (headline figure)

PetFace is a research-gated dataset.  It requires a signed data-use agreement
with the authors (see the [PetFace paper](https://arxiv.org/abs/2407.05230)).
**It is not automatically downloaded by any part of this codebase.**

Once you have obtained and extracted the dataset, run:

```bash
# Assuming PetFace holdout split is at /data/petface/holdout/
# Expected layout: <species>/<individual_id>/<image_file>
cd homeward/embed
uv run homeward-embed eval \
    --dataset-dir /data/petface/holdout \
    --embed-variant base \
    --ks 1,5,20
# Results written to /data/petface/holdout/eval-results.json
```

Update the "Measured accuracy" table above with the printed results.

---

## Anti-tautology guarantee

`eval.py` contains an `assert_disjoint_individuals` guard that **raises** (not
warns) if any individual ID appears in both the gallery and the query split.
This makes it impossible to accidentally inflate accuracy by evaluating on
individuals the model has already indexed.

The guard is exercised by `tests/test_eval.py::TestAssertDisjointIndividuals`.

---

## Design rationale

This harness was built in response to [[feedback_agent_written_fixtures_tautology]],
where a wm-router safety claim of 100% collapsed to 73.5% on a truly held-out
set.  A matcher with an unrun eval is exactly that unproven claim.  This PRD
ensures the harness is runnable, enforces honesty in code, and distinguishes
clearly between "harness arithmetic is correct" and "real-world accuracy is X."
