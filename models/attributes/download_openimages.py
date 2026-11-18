"""Downloads the Open Images clothing annotations of the accessory pictures.

The pictures themselves are those already downloaded by
`models/accessories/download.py` (faces, hats, helmets, glasses). For those
pictures only, this script keeps, from the official annotation files
(streamed, never stored whole):

* the boxes of the clothing classes of `common.ATTRIBUTES`
  (`openimages_classes` and `openimages_negative_by`: Suit, Jacket, Coat,
  Shirt, Dress, Tie, Scarf);
* the human-verified image-level labels of those classes: present
  (confidence 1) or verified absent (confidence 0).

`prepare.py` turns them into reliable labels: a face whose neck and chest
overlap a box of a class wears it; a face in a picture where the class was
verified absent, or where it is boxed on other people only, does not.

The train files are large (2.3 and 2.7 GB): this step takes a while but
stores only a few megabytes.

Usage: python download_openimages.py [--accessories ../accessories/data] [--data data]

Open Images annotations: CC BY 4.0, Google LLC.
"""

from __future__ import annotations

import argparse
import csv
import io
import json
import sys
import urllib.request
from pathlib import Path

from common import ATTRIBUTES, openimages_negative_by

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


def stream_csv(url: str):
    """Yields the rows of a remote CSV as dicts without storing the file."""
    with urllib.request.urlopen(url, timeout=120) as resp:
        yield from csv.DictReader(io.TextIOWrapper(resp, encoding="utf-8", newline=""))


def clothing_classes() -> dict[str, str]:
    """Open Images MID -> display name of every class used by an attribute."""
    wanted = {c for a in ATTRIBUTES for c in a.openimages_classes + openimages_negative_by(a)}
    names = {row["DisplayName"]: row["LabelName"] for row in stream_csv(CLASSES_CSV)}
    missing = wanted - names.keys()
    if missing:
        sys.exit(f"classes not found in Open Images: {sorted(missing)}")
    return {names[n]: n for n in wanted}


def filtered(url: str, images: set[str], mids: dict[str, str], out: Path, fields: list[str]) -> int:
    """Writes the rows of `url` about `images` and `mids` to `out` (cached)."""
    if out.exists():
        return sum(1 for _ in out.open()) - 1
    tmp = out.with_suffix(".partial")
    kept = 0
    with tmp.open("w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["ImageID", "Class", *fields])
        for n, row in enumerate(stream_csv(url)):
            if row["LabelName"] in mids and row["ImageID"] in images:
                w.writerow([row["ImageID"], mids[row["LabelName"]], *(row[k] for k in fields)])
                kept += 1
            if n % 5_000_000 == 0 and n:
                print(f"    {n // 1_000_000}M rows, {kept} kept", flush=True)
    tmp.rename(out)
    return kept


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--accessories", type=Path, default=Path("../accessories/data"))
    ap.add_argument("--data", type=Path, default=Path("data"))
    ap.add_argument("--splits", nargs="*", default=["validation", "test", "train"])
    args = ap.parse_args()

    mids = clothing_classes()
    print(f"classes: {sorted(mids.values())}")
    for split in args.splits:
        sel_path = args.accessories / "openimages" / split / "selection.json"
        if not sel_path.exists():
            print(f"{split}: no accessory pictures ({sel_path}); run models/accessories/download.py first")
            continue
        images = set(json.loads(sel_path.read_text())["images"])
        out = args.data / "openimages" / split
        out.mkdir(parents=True, exist_ok=True)
        print(f"{split}: {len(images)} pictures", flush=True)
        n = filtered(BOXES_CSV[split], images, mids, out / "clothing_boxes.csv",
                     ["XMin", "XMax", "YMin", "YMax", "IsGroupOf", "IsDepiction"])
        print(f"  {n} clothing boxes", flush=True)
        n = filtered(LABELS_CSV[split], images, mids, out / "clothing_labels.csv", ["Confidence"])
        print(f"  {n} verified image labels", flush=True)


if __name__ == "__main__":
    main()
