"""Shared definitions of the accessory model pipeline.

The model runs on a *head crop*: a region around a detected face, large
enough to contain a hat or a helmet above it and glasses on it. The exact
geometry below is part of the model's contract: MediaAnonymizer builds the
same crop around every face found by YuNet, so any change here requires
retraining and is recorded in the exported model's metadata.
"""

from __future__ import annotations

import json
from dataclasses import dataclass
from pathlib import Path

# Accessory groups predicted by the model, in label order (label 0 is the
# background). Each group merges Open Images boxable classes; `negative_by`
# lists the classes whose human-verified absence proves the group is absent
# (Open Images classes form a hierarchy: no "Hat" means no "Sun hat").
#
# To add an accessory, add a group here, then re-run every step.
GROUPS: list[dict] = [
    {"name": "hat", "classes": ["Hat", "Cowboy hat", "Sun hat"], "negative_by": ["Hat"]},
    {"name": "helmet", "classes": ["Helmet", "Bicycle helmet", "Football helmet"], "negative_by": ["Helmet"]},
    {"name": "glasses", "classes": ["Glasses", "Goggles"], "negative_by": ["Glasses"]},
    {"name": "sunglasses", "classes": ["Sunglasses"], "negative_by": ["Sunglasses", "Glasses"]},
]
FACE_CLASS = "Human face"

# Head crop around a face box (x, y, w, h), as multiples of the face size.
CROP_LEFT = 0.5
CROP_RIGHT = 0.5
CROP_TOP = 0.9
CROP_BOTTOM = 0.2

# Model input: the crop resized (without keeping the aspect ratio) to a
# square of this size, RGB, float32 in 0..1, CHW.
INPUT_SIZE = 320

# An accessory box belongs to a crop when at least this fraction of its area
# lies inside the crop.
MIN_INSIDE = 0.6
# Faces smaller than this (pixels, shortest side) are ignored when building
# the dataset: YuNet rarely finds them and their accessories are unreadable.
MIN_FACE = 24


def group_names() -> list[str]:
    return [g["name"] for g in GROUPS]


def head_crop(fx: float, fy: float, fw: float, fh: float, width: int, height: int) -> tuple[int, int, int, int]:
    """Pixel box `(x0, y0, x1, y1)` of the head crop, clipped to the image."""
    x0 = max(0, int(round(fx - CROP_LEFT * fw)))
    y0 = max(0, int(round(fy - CROP_TOP * fh)))
    x1 = min(width, int(round(fx + fw + CROP_RIGHT * fw)))
    y1 = min(height, int(round(fy + fh + CROP_BOTTOM * fh)))
    return x0, y0, x1, y1


def crop_geometry() -> dict:
    """The crop contract, stored in the exported model."""
    return {
        "left": CROP_LEFT,
        "right": CROP_RIGHT,
        "top": CROP_TOP,
        "bottom": CROP_BOTTOM,
        "input_size": INPUT_SIZE,
        "input": "RGB float32 0..1, NCHW, crop resized to input_size x input_size",
    }


@dataclass
class Sample:
    """One head crop and its accessory boxes (crop pixel coordinates)."""

    file: str
    width: int
    height: int
    boxes: list[list[float]]  # x0, y0, x1, y1
    labels: list[int]  # 1-based group index
    face: list[float]  # face box in crop coordinates, x0, y0, x1, y1

    @staticmethod
    def load_all(path: Path) -> list["Sample"]:
        return [Sample(**s) for s in json.loads(path.read_text())]


def load_split(data: Path, split: str) -> tuple[Path, list[Sample]]:
    """Crops directory and samples of a prepared split."""
    root = data / "crops" / split
    return root, Sample.load_all(root / "annotations.json")
