"""Downloads the Open Images data needed to train the accessory model.

For each split it keeps, from the official annotation files (streamed, never
stored whole):

* the boxes of faces and of every accessory class of `common.GROUPS`;
* the human-verified *absences* of those classes (image-level labels with
  confidence 0), used to pick reliable negative examples;

then selects images that show at least one face and either an accessory, or
no accessory box and at least `--min-verified` accessory groups verified
absent (preferring images where more groups were verified), and downloads
those images.

Verified absences are recorded per class and are rare: in the validation
split only a few dozen images with a face have two or more groups verified
absent, and none have all four. Requiring a complete verification would
leave no negatives, hence the relaxed rule; faces without boxes in the images
chosen for an accessory are further negatives.

Usage: python download.py --data data [--per-group 6000] [--negatives 6000]

Open Images annotations: CC BY 4.0, Google LLC. Images: listed by Google as
CC BY 2.0, each by its own author (see the dataset's image metadata). See
README.md for attribution.
"""

from __future__ import annotations

import argparse
import csv
import io
import json
import random
import sys
import urllib.request
from collections import defaultdict
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from common import FACE_CLASS, GROUPS

BASE = "https://storage.googleapis.com/openimages"
CLASSES_CSV = f"{BASE}/v7/oidv7-class-descriptions-boxable.csv"
BOXES_CSV = {
    "train": f"{BASE}/v6/oidv6-train-annotations-bbox.csv",
    "validation": f"{BASE}/v5/validation-annotations-bbox.csv",
    "test": f"{BASE}/v5/test-annotations-bbox.csv",
}
LABELS_CSV = {
    "train": f"{BASE}/v7/oidv7-train-annotations-human-imagelabels.csv",
    "validation": f"{BASE}/v7/oidv7-val-annotations-human-imagelabels.csv",
    "test": f"{BASE}/v7/oidv7-test-annotations-human-imagelabels.csv",
}
IMAGE_URL = "https://open-images-dataset.s3.amazonaws.com/{split}/{image}.jpg"


def stream_csv(url: str):
    """Yields the rows of a remote CSV as dicts without storing the file."""
    with urllib.request.urlopen(url, timeout=120) as resp:
        yield from csv.DictReader(io.TextIOWrapper(resp, encoding="utf-8", newline=""))


def class_ids() -> dict[str, str]:
    """Display name -> Open Images label id (MID)."""
    names = {row["DisplayName"]: row["LabelName"] for row in stream_csv(CLASSES_CSV)}
    wanted = {FACE_CLASS} | {c for g in GROUPS for c in g["classes"] + g["negative_by"]}
    missing = wanted - names.keys()
    if missing:
        sys.exit(f"classes not found in Open Images: {sorted(missing)}")
    return {n: names[n] for n in wanted}


def fetch_boxes(split: str, mids: set[str], out: Path) -> Path:
    """Filtered box annotations of a split (cached)."""
    path = out / "boxes.csv"
    if path.exists():
        return path
    tmp = path.with_suffix(".partial")
    kept = 0
    with tmp.open("w", newline="") as f:
        writer = None
        for i, row in enumerate(stream_csv(BOXES_CSV[split])):
            if writer is None:
                writer = csv.DictWriter(f, fieldnames=list(row.keys()))
                writer.writeheader()
            if row["LabelName"] in mids:
                writer.writerow(row)
                kept += 1
            if i % 2_000_000 == 0:
                print(f"  {split} boxes: {i:,} rows read, {kept:,} kept", flush=True)
    tmp.rename(path)
    return path


def fetch_negatives(split: str, mids: set[str], out: Path) -> Path:
    """Images where accessory classes were verified absent (cached)."""
    path = out / "negatives.json"
    if path.exists():
        return path
    neg: dict[str, list[str]] = defaultdict(list)
    for i, row in enumerate(stream_csv(LABELS_CSV[split])):
        if row["LabelName"] in mids and row["Confidence"] in ("0", "0.0"):
            neg[row["ImageID"]].append(row["LabelName"])
        if i % 5_000_000 == 0:
            print(f"  {split} labels: {i:,} rows read", flush=True)
    path.write_text(json.dumps(neg))
    return path


def select_images(
    boxes: Path, negatives: Path, ids: dict[str, str], per_group: int, n_neg: int, min_verified: int, seed: int
):
    """Images with a usable face and an accessory, plus negatives."""
    face = ids[FACE_CLASS]
    group_of = {ids[c]: g["name"] for g in GROUPS for c in g["classes"]}
    with_face: set[str] = set()
    by_group: dict[str, set[str]] = defaultdict(set)
    with boxes.open(newline="") as f:
        for row in csv.DictReader(f):
            if row["LabelName"] == face and row["IsGroupOf"] == "0" and row["IsDepiction"] != "1":
                with_face.add(row["ImageID"])
            elif row["LabelName"] in group_of:
                by_group[group_of[row["LabelName"]]].add(row["ImageID"])
    rng = random.Random(seed)
    chosen: set[str] = set()
    for g in GROUPS:
        pool = sorted(by_group[g["name"]] & with_face)
        rng.shuffle(pool)
        chosen.update(pool[:per_group])
        print(f"  {g['name']}: {len(pool):,} images with a face, {min(per_group, len(pool)):,} taken")
    # Negatives: no accessory box, and as many groups verified absent as possible.
    neg = json.loads(negatives.read_text())
    any_accessory = set().union(*by_group.values()) if by_group else set()
    verified = {
        img: sum(any(ids[c] in labels for c in g["negative_by"]) for g in GROUPS)
        for img, labels in neg.items()
        if img in with_face and img not in any_accessory
    }
    absent = sorted(img for img, n in verified.items() if n >= min_verified)
    rng.shuffle(absent)
    absent.sort(key=lambda img: -verified[img])  # stable: random within equal counts
    negatives_chosen = set(absent[:n_neg])
    full = sum(1 for img in negatives_chosen if verified[img] == len(GROUPS))
    print(
        f"  negatives: {len(absent):,} images with a face, no accessory box and >= {min_verified} group(s) "
        f"verified absent; {len(negatives_chosen):,} taken ({full:,} with every group verified absent)"
    )
    return sorted(chosen | negatives_chosen), sorted(negatives_chosen)


def download(split: str, images: list[str], out: Path, workers: int) -> None:
    folder = out / "images"
    folder.mkdir(parents=True, exist_ok=True)

    def get(image: str) -> bool:
        path = folder / f"{image}.jpg"
        if path.exists():
            return True
        try:
            with urllib.request.urlopen(IMAGE_URL.format(split=split, image=image), timeout=60) as r:
                data = r.read()
        except Exception as e:  # noqa: BLE001 - report and continue
            print(f"  cannot download {image}: {e}", file=sys.stderr)
            return False
        tmp = path.with_suffix(".partial")
        tmp.write_bytes(data)
        tmp.rename(path)
        return True

    done = 0
    with ThreadPoolExecutor(workers) as pool:
        for ok in pool.map(get, images):
            done += ok
            if done % 1000 == 0:
                print(f"  {split}: {done:,}/{len(images):,} images", flush=True)
    print(f"  {split}: {done:,}/{len(images):,} images downloaded")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--data", type=Path, default=Path("data"))
    ap.add_argument("--splits", nargs="+", default=["train", "validation", "test"])
    ap.add_argument("--per-group", type=int, default=6000, help="train images per accessory group")
    ap.add_argument("--negatives", type=int, default=6000, help="train images without accessories")
    ap.add_argument("--min-verified", type=int, default=1, help="accessory groups verified absent in a negative image")
    ap.add_argument("--eval-fraction", type=float, default=0.25, help="validation/test size relative to train")
    ap.add_argument("--workers", type=int, default=32)
    ap.add_argument("--seed", type=int, default=0)
    args = ap.parse_args()

    ids = class_ids()
    mids = set(ids.values())
    for split in args.splits:
        out = args.data / "openimages" / split
        out.mkdir(parents=True, exist_ok=True)
        print(f"{split}: reading annotations")
        boxes = fetch_boxes(split, mids, out)
        negatives = fetch_negatives(split, mids, out)
        scale = 1.0 if split == "train" else args.eval_fraction
        images, neg = select_images(
            boxes,
            negatives,
            ids,
            int(args.per_group * scale),
            int(args.negatives * scale),
            args.min_verified,
            args.seed,
        )
        (out / "selection.json").write_text(json.dumps({"images": images, "negatives": neg, "ids": ids}))
        download(split, images, out, args.workers)


if __name__ == "__main__":
    main()
