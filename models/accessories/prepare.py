"""Builds the head-crop dataset from the downloaded Open Images data.

For every usable face (not a group, not a drawing, at least `MIN_FACE`
pixels) of a selected image, the head crop of `common.head_crop` is cut out,
resized so that its longest side is at most `INPUT_SIZE`, and stored with
the accessory boxes that lie mostly inside it.

Crops of images chosen for their verified absences have no boxes and are
reliable negatives. Crops of images chosen for an accessory may contain
unlabelled accessories of *other* groups (Open Images only boxes a class
exhaustively when it was verified present), a known source of noise.

Usage: python prepare.py --data data
"""

from __future__ import annotations

import argparse
import csv
import json
from collections import defaultdict
from pathlib import Path

from PIL import Image, ImageOps

from common import FACE_CLASS, GROUPS, INPUT_SIZE, MIN_FACE, MIN_INSIDE, head_crop


def prepare_split(data: Path, split: str) -> None:
    src = data / "openimages" / split
    sel = json.loads((src / "selection.json").read_text())
    ids: dict[str, str] = sel["ids"]
    chosen = set(sel["images"])
    face_mid = ids[FACE_CLASS]
    label_of = {ids[c]: i + 1 for i, g in enumerate(GROUPS) for c in g["classes"]}

    faces: dict[str, list[tuple[float, ...]]] = defaultdict(list)
    accessories: dict[str, list[tuple[int, tuple[float, ...]]]] = defaultdict(list)
    with (src / "boxes.csv").open(newline="") as f:
        for row in csv.DictReader(f):
            if row["ImageID"] not in chosen:
                continue
            box = tuple(float(row[k]) for k in ("XMin", "YMin", "XMax", "YMax"))
            if row["LabelName"] == face_mid:
                if row["IsGroupOf"] == "0" and row["IsDepiction"] != "1":
                    faces[row["ImageID"]].append(box)
            elif row["LabelName"] in label_of:
                accessories[row["ImageID"]].append((label_of[row["LabelName"]], box))

    out = data / "crops" / split
    out.mkdir(parents=True, exist_ok=True)
    samples = []
    counts = defaultdict(int)
    for n, image in enumerate(sorted(chosen)):
        path = src / "images" / f"{image}.jpg"
        if not path.exists() or image not in faces:
            continue
        try:
            img = Image.open(path)
            # Open Images boxes refer to the image as displayed.
            img = ImageOps.exif_transpose(img).convert("RGB")
        except Exception as e:  # noqa: BLE001 - skip unreadable images
            print(f"  skipping {image}: {e}")
            continue
        W, H = img.size
        for k, (x0, y0, x1, y1) in enumerate(faces[image]):
            fx, fy, fw, fh = x0 * W, y0 * H, (x1 - x0) * W, (y1 - y0) * H
            if min(fw, fh) < MIN_FACE:
                continue
            cx0, cy0, cx1, cy1 = head_crop(fx, fy, fw, fh, W, H)
            cw, ch = cx1 - cx0, cy1 - cy0
            scale = min(1.0, INPUT_SIZE / max(cw, ch))
            boxes, labels = [], []
            for label, (ax0, ay0, ax1, ay1) in accessories.get(image, []):
                ax0, ay0, ax1, ay1 = ax0 * W, ay0 * H, ax1 * W, ay1 * H
                area = max(1e-6, (ax1 - ax0) * (ay1 - ay0))
                ix0, iy0, ix1, iy1 = max(ax0, cx0), max(ay0, cy0), min(ax1, cx1), min(ay1, cy1)
                inside = max(0.0, ix1 - ix0) * max(0.0, iy1 - iy0) / area
                if inside < MIN_INSIDE:
                    continue
                boxes.append([(ix0 - cx0) * scale, (iy0 - cy0) * scale, (ix1 - cx0) * scale, (iy1 - cy0) * scale])
                labels.append(label)
                counts[GROUPS[label - 1]["name"]] += 1
            if not boxes:
                counts["(no accessory)"] += 1
            crop = img.crop((cx0, cy0, cx1, cy1))
            if scale < 1.0:
                crop = crop.resize((max(1, round(cw * scale)), max(1, round(ch * scale))), Image.Resampling.BILINEAR)
            name = f"{image}_{k}.jpg"
            crop.save(out / name, quality=92)
            face = [(fx - cx0) * scale, (fy - cy0) * scale, (fx + fw - cx0) * scale, (fy + fh - cy0) * scale]
            samples.append(
                {
                    "file": name,
                    "width": crop.width,
                    "height": crop.height,
                    "boxes": boxes,
                    "labels": labels,
                    "face": face,
                }
            )
        if n % 2000 == 0:
            print(f"  {split}: {n:,}/{len(chosen):,} images, {len(samples):,} crops", flush=True)
    (out / "annotations.json").write_text(json.dumps(samples))
    print(f"{split}: {len(samples):,} crops; accessories: {dict(counts)}")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--data", type=Path, default=Path("data"))
    ap.add_argument("--splits", nargs="+", default=["train", "validation", "test"])
    args = ap.parse_args()
    for split in args.splits:
        prepare_split(args.data, split)


if __name__ == "__main__":
    main()
