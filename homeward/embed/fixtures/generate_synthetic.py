"""Generate synthetic fixture images for homeward-embed smoke tests.

Run this script to regenerate the synthetic PNG fixtures in this directory:
  python fixtures/generate_synthetic.py

These are solid-colour 224×224 squares — perceptually distinct enough for
DINOv2's CLS token to produce meaningfully different embeddings, which lets
the smoke test assert rank-ordering without downloading real model weights.

For proper end-to-end validation with real photos, see SOURCES.md.
"""

from __future__ import annotations

from pathlib import Path

import numpy as np
from PIL import Image


FIXTURES = [
    ("dog_retriever", (210, 140, 60)),    # golden-tan
    ("dog_dalmatian", (240, 240, 240)),   # near-white
    ("cat_tabby",     (100,  80, 50)),    # dark brown-grey
]


def main() -> None:
    out_dir = Path(__file__).parent
    for name, colour in FIXTURES:
        arr = np.full((224, 224, 3), colour, dtype=np.uint8)
        img = Image.fromarray(arr, mode="RGB")
        p = out_dir / f"{name}.png"
        img.save(p)
        print(f"  wrote {p.name}  colour=RGB{colour}")
    print("Done.")


if __name__ == "__main__":
    main()
