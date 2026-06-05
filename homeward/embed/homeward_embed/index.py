"""HNSW vector index — disk-persisted, append-only gallery.

Maps float32 embedding vectors to canonical_id strings.
Uses hnswlib (Apache-2.0) for sub-second cosine kNN.

The index directory stores:
  {path}/index.bin          — hnswlib serialized HNSW graph
  {path}/id_map.json        — list of canonical_ids (position == hnswlib internal id)
  {path}/meta.json          — embed_dim, space, max_elements, species per entry

Species filtering is implemented as a post-filter on the id_map (HNSW does not
natively support metadata predicates).  For large galleries (>1M) a two-level
per-species index is the upgrade path; at 100k entries this is fast enough.
"""

from __future__ import annotations

import json
import logging
import threading
from pathlib import Path
from typing import Optional

import hnswlib  # type: ignore[import-untyped]
import numpy as np

logger = logging.getLogger(__name__)

_INDEX_FILE = "index.bin"
_ID_MAP_FILE = "id_map.json"
_META_FILE = "meta.json"

_Species = Optional[str]  # "dog" | "cat" | None


class EmbedIndex:
    """Thread-safe HNSW-backed vector index persisted to *index_dir*.

    Args:
        index_dir: Directory to store index files (created if absent).
        embed_dim: Embedding dimensionality (must match all enrolled vectors).
        max_elements: Pre-allocated capacity. Doubled automatically when full.
        ef_construction: HNSW build parameter (higher = better recall, slower build).
        M: HNSW connectivity parameter.
    """

    def __init__(
        self,
        index_dir: str | Path,
        embed_dim: int = 768,
        max_elements: int = 200_000,
        ef_construction: int = 200,
        M: int = 16,
    ) -> None:
        self._dir = Path(index_dir)
        self._dir.mkdir(parents=True, exist_ok=True)
        self._embed_dim = embed_dim
        self._ef_construction = ef_construction
        self._M = M
        self._lock = threading.Lock()

        # id_map[internal_id] = (canonical_id, species)
        self._id_map: list[tuple[str, _Species]] = []

        self._index = hnswlib.Index(space="cosine", dim=embed_dim)

        meta_path = self._dir / _META_FILE
        index_path = self._dir / _INDEX_FILE
        id_map_path = self._dir / _ID_MAP_FILE

        if index_path.exists() and id_map_path.exists() and meta_path.exists():
            meta = json.loads(meta_path.read_text())
            stored_dim = meta.get("embed_dim")
            if stored_dim != embed_dim:
                msg = (
                    f"Index at {index_dir} has embed_dim={stored_dim}, "
                    f"but embedder uses {embed_dim}. Delete the index dir to rebuild."
                )
                raise ValueError(msg)
            self._index.load_index(str(index_path), max_elements=max_elements)
            raw_map = json.loads(id_map_path.read_text())
            self._id_map = [tuple(entry) for entry in raw_map]  # type: ignore[misc]
            logger.info(
                "Loaded HNSW index from %s (%d vectors, dim=%d)",
                index_dir,
                len(self._id_map),
                embed_dim,
            )
        else:
            self._index.init_index(
                max_elements=max_elements,
                ef_construction=ef_construction,
                M=M,
            )
            logger.info("Initialized new HNSW index (dim=%d, cap=%d)", embed_dim, max_elements)

        self._index.set_ef(50)

    # ------------------------------------------------------------------
    # Public API
    # ------------------------------------------------------------------

    @property
    def size(self) -> int:
        """Number of vectors currently in the index."""
        with self._lock:
            return len(self._id_map)

    def enroll(self, canonical_id: str, vector: np.ndarray, species: _Species = None) -> int:
        """Add *vector* → *canonical_id* to the index and persist.

        Args:
            canonical_id: Stable pet identifier (e.g. ULID from homeward-schema).
            vector: L2-normalized float32 array of shape (embed_dim,).
            species: Optional "dog" or "cat" for filtered queries.

        Returns:
            Internal HNSW id assigned to this entry.
        """
        if vector.shape != (self._embed_dim,):
            msg = f"Expected shape ({self._embed_dim},), got {vector.shape}"
            raise ValueError(msg)

        with self._lock:
            internal_id = len(self._id_map)
            # Grow index if at capacity
            if internal_id >= self._index.get_max_elements():
                new_cap = self._index.get_max_elements() * 2
                logger.info("HNSW index capacity reached; resizing to %d", new_cap)
                self._index.resize_index(new_cap)

            self._index.add_items(vector.reshape(1, -1), [internal_id])
            self._id_map.append((canonical_id, species))
            self._persist_unlocked()
            return internal_id

    def query(
        self,
        vector: np.ndarray,
        k: int = 10,
        species_filter: _Species = None,
    ) -> list[tuple[str, float]]:
        """Return top-k nearest *canonical_id*s with cosine similarity scores.

        Because hnswlib returns cosine *distance* (1 − similarity), we convert
        to similarity = 1 − distance.

        Args:
            vector: Query vector, shape (embed_dim,).
            k: Number of results requested (may return fewer if index is small).
            species_filter: If "dog" or "cat", only returns matching species.

        Returns:
            List of (canonical_id, cosine_similarity) sorted by similarity desc.
        """
        with self._lock:
            n = len(self._id_map)
            if n == 0:
                return []
            # Over-fetch when filtering so we have enough after species post-filter
            fetch_k = min(k * 5, n) if species_filter else min(k, n)
            labels, distances = self._index.knn_query(vector.reshape(1, -1), k=fetch_k)

        results: list[tuple[str, float]] = []
        for label, dist in zip(labels[0], distances[0]):
            canonical_id, species = self._id_map[int(label)]
            if species_filter and species != species_filter:
                continue
            sim = max(0.0, 1.0 - float(dist))
            results.append((canonical_id, sim))
            if len(results) >= k:
                break

        return results

    def reembed_all(
        self,
        embed_fn: "Callable[[str], np.ndarray]",  # noqa: F821
        id_species_iter: "Iterable[tuple[str, _Species]]",  # noqa: F821
    ) -> None:
        """Rebuild the index from scratch using *embed_fn*.

        Args:
            embed_fn: callable(canonical_id) -> float32 vector.
            id_species_iter: iterable of (canonical_id, species) in enroll order.
        """
        with self._lock:
            cap = max(self._index.get_max_elements(), 200_000)
            self._index.init_index(
                max_elements=cap,
                ef_construction=self._ef_construction,
                M=self._M,
            )
            self._id_map = []
            for canonical_id, species in id_species_iter:
                vec = embed_fn(canonical_id)
                internal_id = len(self._id_map)
                self._index.add_items(vec.reshape(1, -1), [internal_id])
                self._id_map.append((canonical_id, species))
            self._persist_unlocked()
        logger.info("reembed_all complete: %d vectors", len(self._id_map))

    # ------------------------------------------------------------------
    # Internal helpers
    # ------------------------------------------------------------------

    def _persist_unlocked(self) -> None:
        """Persist index and id_map to disk. Must hold self._lock."""
        self._index.save_index(str(self._dir / _INDEX_FILE))
        (self._dir / _ID_MAP_FILE).write_text(json.dumps(self._id_map))
        (self._dir / _META_FILE).write_text(
            json.dumps({"embed_dim": self._embed_dim, "space": "cosine"})
        )
