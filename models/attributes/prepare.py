"""Builds the person-crop dataset from the downloaded pictures.

Sources:

* **Wikimedia Commons** (`download_commons.py`): faces are found with the
  same YuNet model as the application. A picture from a category is
  evidence about its main subjects only, so at most `--max-faces` faces are
  kept per picture, those at least half the size of the largest one. They
  get the attributes of the picture's categories; the other roles are
  negatives (roles are exclusive); other gear is a weak negative, other
  clothing unknown.
* **Open Images** (the data of `models/accessories`, if present): faces and
  boxes of hats, helmets, glasses and sunglasses annotated by people. A face
  whose head region holds no box of an accessory is a negative for it. These
  are everyday pictures: their role is weakly labelled civilian. Clothing
  comes from the boxes and verified labels of `download_openimages.py`: a
  face whose neck and chest overlap a box wears that item; a face without
  such a box, in a picture where the item is boxed on someone else or was
  verified absent, does not.

Finally, `excluded_by` (see `common.Attribute`) turns some unknown labels
into negatives: a person known to wear a suit is not wearing a hoodie.

Every crop is stored at the model's input size with one label per
attribute: 1 (present), 0 (absent), -1 (unknown) and a weight.

Usage: python prepare.py --data data [--openimages ../accessories/data]
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import importlib.util
import sys
from collections import defaultdict
from pathlib import Path

import cv2
import numpy as np
from PIL import Image, ImageOps

from common import ATTRIBUTES, names, openimages_negative_by, person_crop

YUNET = Path(__file__).resolve().parent.parent / "yunet_2023mar_dynamic.onnx"
DETECT_SIDE = 1280
MIN_FACE = 20
# Weight of a role guessed from the source (Open Images is mostly everyday
# life) and of an unknown gear label treated as absent.
WEAK = 0.25
RARE_IN_OPENIMAGES = {"shield", "balaclava", "face_mask"}
# Neck and chest of a face (x0, y0, x1, y1 in face heights from the centre
# and the top of the face box), where an Open Images clothing box must lie.
CHEST = (-0.6, 1.0, 0.6, 2.5)
# Overlap between a clothing box and the chest, as a fraction of the smaller.
MIN_CHEST_OVERLAP = 0.4
# Fraction of the chest that must lie inside the picture for its clothing
# to be known.
MIN_CHEST_INSIDE = 0.5


def apply_exclusions(labels: list[int], weights: list[float]) -> None:
    """Unknown or weak labels become reliable negatives when an attribute
    that excludes them is reliably present."""
    present = {a.name for i, a in enumerate(ATTRIBUTES) if labels[i] == 1 and weights[i] >= 1.0}
    for i, a in enumerate(ATTRIBUTES):
        if weights[i] < 1.0 and present & set(a.excluded_by):
            labels[i], weights[i] = 0, 1.0


def split_of(key: str) -> str:
    h = int(hashlib.sha1(key.encode()).hexdigest()[:8], 16) % 100
    return "train" if h < 80 else "validation" if h < 90 else "test"


class Faces:
    def __init__(self, score: float):
        self.det = cv2.FaceDetectorYN.create(str(YUNET), "", (320, 320), score, 0.3, 5000)

    def __call__(self, img: Image.Image) -> list[tuple[float, float, float, float]]:
        W, H = img.size
        s = min(1.0, DETECT_SIDE / max(W, H))
        small = img.resize((max(1, round(W * s)), max(1, round(H * s))), Image.Resampling.BOX) if s < 1 else img
        bgr = cv2.cvtColor(np.asarray(small), cv2.COLOR_RGB2BGR)
        self.det.setInputSize((bgr.shape[1], bgr.shape[0]))
        _, found = self.det.detect(bgr)
        if found is None:
            return []
        return [(f[0] / s, f[1] / s, f[2] / s, f[3] / s) for f in found]


def commons(data: Path, out: Path, max_faces: int, detector: Faces, samples: dict[str, list]) -> None:
    root = data / "commons"
    manifest = json.loads((root / "manifest.json").read_text())
    idx = {a.name: i for i, a in enumerate(ATTRIBUTES)}
    roles = [a.name for a in ATTRIBUTES if a.group == "role"]
    counts = defaultdict(int)
    for n, (title, m) in enumerate(manifest.items()):
        path = root / "images" / m.get("file", "-")
        if not path.exists():
            continue
        try:
            img = ImageOps.exif_transpose(Image.open(path)).convert("RGB")
        except Exception:  # noqa: BLE001 - unreadable thumbnails are skipped
            continue
        faces = [f for f in detector(img) if f[3] >= MIN_FACE]
        if not faces:
            continue
        faces.sort(key=lambda f: -f[3])
        faces = [f for f in faces if f[3] >= 0.5 * faces[0][3]][:max_faces]
        attrs = set(m["attributes"])
        labels = [-1] * len(ATTRIBUTES)
        weights = [WEAK] * len(ATTRIBUTES)
        present_roles = attrs & set(roles)
        if len(present_roles) == 1:
            for r in roles:
                labels[idx[r]] = int(r in attrs)
                weights[idx[r]] = 1.0
        for a in ATTRIBUTES:
            if a.group == "gear":
                labels[idx[a.name]] = 1 if a.name in attrs else 0
                weights[idx[a.name]] = 1.0 if a.name in attrs else WEAK
            elif a.group == "clothing":
                # A picture of men wearing ties says nothing about jackets.
                labels[idx[a.name]] = 1 if a.name in attrs else -1
                weights[idx[a.name]] = 1.0 if a.name in attrs else 0.0
        apply_exclusions(labels, weights)
        split = split_of(title)
        for k, (x, y, w, h) in enumerate(faces):
            name = f"c_{Path(m['file']).stem}_{k}.jpg"
            person_crop(img, x, y, w, h).save(out / name, quality=93)
            samples[split].append({"file": name, "labels": labels, "weights": weights, "source": "commons"})
            for a in attrs:
                counts[a] += 1
        if n % 500 == 0:
            print(f"  commons: {n}/{len(manifest)} pictures", flush=True)
    print(f"commons crops per attribute: {dict(counts)}")


class Clothing:
    """Open Images clothing boxes and verified labels of one split."""

    def __init__(self, root: Path):
        self.boxes: dict[str, list[tuple[str, tuple[float, ...], bool]]] = defaultdict(list)
        self.labels: dict[str, dict[str, int]] = defaultdict(dict)
        self.available = (root / "clothing_boxes.csv").exists() and (root / "clothing_labels.csv").exists()
        if not self.available:
            return
        with (root / "clothing_boxes.csv").open(newline="") as f:
            for r in csv.DictReader(f):
                if r["IsDepiction"] == "1":
                    continue
                box = tuple(float(r[k]) for k in ("XMin", "YMin", "XMax", "YMax"))
                self.boxes[r["ImageID"]].append((r["Class"], box, r["IsGroupOf"] == "1"))
        with (root / "clothing_labels.csv").open(newline="") as f:
            for r in csv.DictReader(f):
                self.labels[r["ImageID"]][r["Class"]] = int(float(r["Confidence"]) >= 0.5)

    def label(self, image: str, face: tuple[float, float, float, float], W: int, H: int) -> dict[str, tuple[int, float]]:
        """`(label, weight)` of every clothing attribute for a face (pixels)."""
        fx, fy, fw, fh = face
        cx = fx + fw / 2
        r = (cx + CHEST[0] * fh, fy + CHEST[1] * fh, cx + CHEST[2] * fh, fy + CHEST[3] * fh)
        r_area = (r[2] - r[0]) * (r[3] - r[1])
        inside = max(0.0, min(r[2], W) - max(r[0], 0)) * max(0.0, min(r[3], H) - max(r[1], 0)) / r_area
        out = {a.name: (-1, 0.0) for a in ATTRIBUTES if a.group == "clothing"}
        if not self.available or inside < MIN_CHEST_INSIDE:
            return out
        on_face: dict[str, bool] = {}  # class -> boxed on this face (True: by a group box only)
        boxed = set()
        for cls, (x0, y0, x1, y1), group in self.boxes.get(image, []):
            boxed.add(cls)
            b = (x0 * W, y0 * H, x1 * W, y1 * H)
            iw = max(0.0, min(b[2], r[2]) - max(b[0], r[0]))
            ih = max(0.0, min(b[3], r[3]) - max(b[1], r[1]))
            b_area = max(1e-6, (b[2] - b[0]) * (b[3] - b[1]))
            if iw * ih / min(r_area, b_area) >= MIN_CHEST_OVERLAP:
                on_face[cls] = on_face.get(cls, True) and group
        verified = self.labels.get(image, {})
        # A class is known to be absent from this face when it is boxed in
        # the picture (boxes are exhaustive) but not here, or verified absent.
        absent = lambda c: c not in on_face and (c in boxed or verified.get(c) == 0)  # noqa: E731
        for a in ATTRIBUTES:
            if a.group != "clothing":
                continue
            if any(on_face.get(c) is False for c in a.openimages_classes):
                out[a.name] = (1, 1.0)
            elif any(c in on_face for c in a.openimages_classes):
                continue  # only a box around a group of people: unknown
            elif (neg := openimages_negative_by(a)) and all(absent(c) for c in neg):
                out[a.name] = (0, 1.0)
        return out


def openimages(data: Path, out: Path, per_split: dict[str, int], samples: dict[str, list], clothing_root: Path) -> None:
    # The accessory pipeline's `common` module, loaded under another name.
    spec = importlib.util.spec_from_file_location(
        "accessories_common", Path(__file__).resolve().parent.parent / "accessories" / "common.py"
    )
    acc_common = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = acc_common  # dataclasses look the module up there
    spec.loader.exec_module(acc_common)
    FACE_CLASS, GROUPS, MIN_INSIDE, head_crop = (
        acc_common.FACE_CLASS, acc_common.GROUPS, acc_common.MIN_INSIDE, acc_common.head_crop
    )
    idx = {a.name: i for i, a in enumerate(ATTRIBUTES)}
    group_attr = {i + 1: a.name for i, g in enumerate(GROUPS) for a in ATTRIBUTES if a.openimages == g["name"]}
    covered = set(group_attr.values())
    for split, limit in per_split.items():
        src = data / "openimages" / split
        if not (src / "selection.json").exists():
            print(f"  no Open Images {split} data in {src}")
            continue
        sel = json.loads((src / "selection.json").read_text())
        clothing = Clothing(clothing_root / "openimages" / split)
        if not clothing.available:
            print(f"  no Open Images clothing labels for {split}: run download_openimages.py (clothing stays unknown)")
        ids, chosen = sel["ids"], set(sel["images"])
        face_mid = ids[FACE_CLASS]
        label_of = {ids[c]: i + 1 for i, g in enumerate(GROUPS) for c in g["classes"]}
        faces, acc = defaultdict(list), defaultdict(list)
        with (src / "boxes.csv").open(newline="") as f:
            for row in csv.DictReader(f):
                if row["ImageID"] not in chosen:
                    continue
                box = tuple(float(row[k]) for k in ("XMin", "YMin", "XMax", "YMax"))
                if row["LabelName"] == face_mid:
                    if row["IsGroupOf"] == "0" and row["IsDepiction"] != "1":
                        faces[row["ImageID"]].append(box)
                elif row["LabelName"] in label_of:
                    acc[row["ImageID"]].append((label_of[row["LabelName"]], box))
        images = sorted(i for i in chosen if i in faces)
        # Deterministic subset, balanced between pictures with and without accessories.
        images.sort(key=lambda i: hashlib.sha1(i.encode()).hexdigest())
        with_acc = [i for i in images if acc.get(i)][: limit // 2]
        without = [i for i in images if not acc.get(i)][: limit - len(with_acc)]
        kept = 0
        for image in with_acc + without:
            path = src / "images" / f"{image}.jpg"
            if not path.exists():
                continue
            try:
                img = ImageOps.exif_transpose(Image.open(path)).convert("RGB")
            except Exception:  # noqa: BLE001
                continue
            W, H = img.size
            for k, (x0, y0, x1, y1) in enumerate(faces[image][:3]):
                fx, fy, fw, fh = x0 * W, y0 * H, (x1 - x0) * W, (y1 - y0) * H
                if min(fw, fh) < MIN_FACE:
                    continue
                cx0, cy0, cx1, cy1 = head_crop(fx, fy, fw, fh, W, H)
                present = set()
                for label, (ax0, ay0, ax1, ay1) in acc.get(image, []):
                    ax0, ay0, ax1, ay1 = ax0 * W, ay0 * H, ax1 * W, ay1 * H
                    area = max(1e-6, (ax1 - ax0) * (ay1 - ay0))
                    iw = max(0.0, min(ax1, cx1) - max(ax0, cx0))
                    ih = max(0.0, min(ay1, cy1) - max(ay0, cy0))
                    if iw * ih / area >= MIN_INSIDE:
                        present.add(group_attr[label])
                labels = [-1] * len(ATTRIBUTES)
                weights = [0.0] * len(ATTRIBUTES)
                for a in covered:
                    labels[idx[a]] = int(a in present)
                    weights[idx[a]] = 1.0
                for name, (label, weight) in clothing.label(image, (fx, fy, fw, fh), W, H).items():
                    labels[idx[name]], weights[idx[name]] = label, weight
                for a in ATTRIBUTES:
                    if a.group == "role":
                        labels[idx[a.name]] = int(a.name == "civilian")
                        weights[idx[a.name]] = WEAK
                    elif a.group == "gear" and a.name not in covered:
                        # Riot shields, balaclavas and medical masks are
                        # practically absent from Open Images (mostly
                        # everyday photos taken before 2020): reliable
                        # negatives. Other gear stays weak.
                        labels[idx[a.name]] = 0
                        weights[idx[a.name]] = 1.0 if a.name in RARE_IN_OPENIMAGES else WEAK
                apply_exclusions(labels, weights)
                name = f"o_{image}_{k}.jpg"
                person_crop(img, fx, fy, fw, fh).save(out / name, quality=93)
                samples[split].append({"file": name, "labels": labels, "weights": weights, "source": "openimages"})
                kept += 1
        print(f"  openimages {split}: {kept} crops")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--data", type=Path, default=Path("data"))
    ap.add_argument("--openimages", type=Path, default=Path("../accessories/data"))
    ap.add_argument("--max-faces", type=int, default=3)
    ap.add_argument("--openimages-train", type=int, default=6000)
    ap.add_argument("--openimages-eval", type=int, default=1000)
    ap.add_argument("--score", type=float, default=0.7)
    args = ap.parse_args()

    out = args.data / "crops"
    out.mkdir(parents=True, exist_ok=True)
    samples: dict[str, list] = {"train": [], "validation": [], "test": []}
    commons(args.data, out, args.max_faces, Faces(args.score), samples)
    if args.openimages.exists():
        n = {"train": args.openimages_train, "validation": args.openimages_eval, "test": args.openimages_eval}
        openimages(args.openimages, out, n, samples, args.data)
    for split, s in samples.items():
        (args.data / f"{split}.json").write_text(json.dumps({"attributes": names(), "samples": s}))
        pos = np.array([x["labels"] for x in s]) if s else np.zeros((0, len(ATTRIBUTES)))
        print(f"{split}: {len(s)} crops; positives: " + ", ".join(f"{a}={int((pos[:, i] == 1).sum())}" for i, a in enumerate(names())))


if __name__ == "__main__":
    main()
