"""Animal detector/cropper using YOLO (COCO weights).

Crops the largest dog (class 16) or cat (class 15) detection.
Falls back to the whole image if no animal is detected — never drops the image.

YOLO COCO class IDs:
  15 = cat
  16 = dog
"""

from __future__ import annotations

import logging
from pathlib import Path
from typing import Optional

from PIL import Image

logger = logging.getLogger(__name__)

# COCO class IDs for animals we detect
_CAT_CLASS = 15
_DOG_CLASS = 16
_ANIMAL_CLASSES = frozenset({_CAT_CLASS, _DOG_CLASS})

_Species = str  # "dog" | "cat" | "unknown"


class AnimalDetector:
    """YOLO-based animal detector that produces a tight crop before embedding.

    Falls back to whole-image (with a logged warning) when no animal is found
    so that intake is never silently dropped.
    """

    def __init__(self, model_name: str = "yolov8n.pt", conf_threshold: float = 0.25) -> None:
        """Load YOLO model.

        Args:
            model_name: YOLO model name or path. Ultralytics downloads on first use.
            conf_threshold: Minimum detection confidence (0–1).
        """
        try:
            from ultralytics import YOLO  # type: ignore[import-untyped]

            self._model = YOLO(model_name)
            logger.info("YOLO model loaded: %s", model_name)
        except ImportError as exc:
            raise ImportError("ultralytics is required for animal detection") from exc
        self._conf = conf_threshold

    def crop(self, image: Image.Image, species_hint: Optional[_Species] = None) -> tuple[Image.Image, bool]:
        """Detect and crop the largest animal in *image*.

        Args:
            image: Input PIL image (RGB).
            species_hint: Optional "dog" or "cat" to filter detections.

        Returns:
            (cropped_image, was_detected): cropped_image is the best crop or
            the whole image; was_detected is True when a detection was used.
        """
        results = self._model.predict(image, conf=self._conf, verbose=False)
        best_box = None
        best_area = 0.0

        allowed_classes = _ANIMAL_CLASSES
        if species_hint == "dog":
            allowed_classes = frozenset({_DOG_CLASS})
        elif species_hint == "cat":
            allowed_classes = frozenset({_CAT_CLASS})

        for result in results:
            if result.boxes is None:
                continue
            for box in result.boxes:
                cls_id = int(box.cls.item())
                if cls_id not in allowed_classes:
                    continue
                x1, y1, x2, y2 = box.xyxy[0].tolist()
                area = (x2 - x1) * (y2 - y1)
                if area > best_area:
                    best_area = area
                    best_box = (int(x1), int(y1), int(x2), int(y2))

        if best_box is None:
            logger.warning(
                "No animal detected (species_hint=%s, image_size=%s); falling back to whole-image.",
                species_hint,
                image.size,
            )
            return image, False

        crop = image.crop(best_box)
        logger.debug("Animal crop: box=%s area=%.0f", best_box, best_area)
        return crop, True
