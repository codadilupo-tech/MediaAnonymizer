"""Downloads reference pictures for each attribute from Wikimedia Commons.

For every attribute of `common.ATTRIBUTES`, the Commons categories it lists
are explored breadth first (subcategories up to `--depth` levels, skipping
those whose names suggest objects rather than people, see
`common.SKIP_SUBCATEGORY`), and up to `--per-attribute` photographs are
kept. A file found under several attributes is downloaded once and carries
all of them.

Only a thumbnail (`--width` pixels wide) is downloaded. Author, licence and
source page of every file are recorded in `manifest.json`: the pictures are
used for training only and are not redistributed.

Usage: python download_commons.py --data data [--per-attribute 1500] [--depth 2]
"""

from __future__ import annotations

import argparse
import json
import re
import time
import urllib.error
import urllib.parse
import urllib.request
from collections import deque
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from common import ATTRIBUTES, SKIP_SUBCATEGORY

API = "https://commons.wikimedia.org/w/api.php"
UA = "MediaAnonymizer-attribute-model/0.1 (training data collection; https://www.mediawiki.org/wiki/API:Etiquette)"
PHOTO = re.compile(r"\.(jpe?g|png|webp)$", re.IGNORECASE)


def api(params: dict) -> dict:
    # POST: lists of 50 long file titles do not fit in a URL.
    body = urllib.parse.urlencode({"action": "query", "format": "json", "formatversion": 2, **params}).encode()
    for attempt in range(10):
        time.sleep(0.3)  # stay well under the API rate limits
        try:
            req = urllib.request.Request(API, data=body, headers={"User-Agent": UA})
            with urllib.request.urlopen(req, timeout=60) as resp:
                return json.load(resp)
        except Exception as e:  # noqa: BLE001 - retried, then raised
            if attempt == 9:
                raise
            wait = 10 * (attempt + 1)
            if isinstance(e, urllib.error.HTTPError) and e.headers.get("Retry-After", "").isdigit():
                wait = max(wait, int(e.headers["Retry-After"]))
            print(f"  retry in {wait}s ({e})", flush=True)
            time.sleep(wait)
    raise AssertionError


def members(category: str, kind: str) -> list[str]:
    out, cont = [], {}
    while True:
        r = api({"list": "categorymembers", "cmtitle": f"Category:{category}", "cmtype": kind, "cmlimit": 500, **cont})
        out += [m["title"] for m in r.get("query", {}).get("categorymembers", [])]
        if "continue" not in r:
            return out
        cont = r["continue"]


def explore(roots: list[str], depth: int, limit: int) -> list[str]:
    """File titles under `roots`, breadth first, at most `limit`."""
    files: list[str] = []
    seen_files: set[str] = set()
    seen = set(roots)
    queue = deque((c, 0) for c in roots)
    while queue and len(files) < limit:
        cat, d = queue.popleft()
        try:
            found = members(cat, "file")
        except Exception as e:  # noqa: BLE001 - a missing category is not fatal
            print(f"  cannot list {cat}: {e}")
            continue
        new = [f for f in found if PHOTO.search(f) and f not in seen_files]
        if d == 0 and not found:
            print(f"  warning: category '{cat}' is empty or missing")
        seen_files.update(new)
        files += new
        if d < depth:
            for sub in members(cat, "subcat"):
                name = sub.removeprefix("Category:")
                low = name.lower() + " "
                if name not in seen and not any(w in low for w in SKIP_SUBCATEGORY):
                    seen.add(name)
                    queue.append((name, d + 1))
    return files[:limit]


def image_info(titles: list[str], width: int) -> dict[str, dict]:
    """Thumbnail URL and attribution of each file."""
    info = {}
    for i in range(0, len(titles), 50):
        r = api(
            {
                "titles": "|".join(titles[i : i + 50]),
                "prop": "imageinfo",
                "iiprop": "url|extmetadata|mime",
                "iiurlwidth": width,
                "iiextmetadatafilter": "LicenseShortName|Artist|UsageTerms",
            }
        )
        for p in r.get("query", {}).get("pages", []):
            ii = (p.get("imageinfo") or [None])[0]
            if not ii or not ii.get("mime", "").startswith("image/"):
                continue
            meta = ii.get("extmetadata", {})
            artist = re.sub(r"<[^>]+>", "", meta.get("Artist", {}).get("value", "")).strip()
            info[p["title"]] = {
                "thumb": ii.get("thumburl") or ii["url"],
                "page": ii.get("descriptionurl", ""),
                "license": meta.get("LicenseShortName", {}).get("value", ""),
                "artist": artist[:200],
            }
    return info


def fetch(url: str, dest: Path) -> bool:
    if dest.exists():
        return True
    for attempt in range(4):
        try:
            req = urllib.request.Request(url, headers={"User-Agent": UA})
            with urllib.request.urlopen(req, timeout=60) as resp:
                data = resp.read()
            tmp = dest.with_suffix(".part")
            tmp.write_bytes(data)
            tmp.rename(dest)
            return True
        except Exception as e:  # noqa: BLE001 - retried, then skipped
            if "404" in str(e):
                return False
            time.sleep(10 * (attempt + 1))
    return False


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--data", type=Path, default=Path("data"))
    ap.add_argument("--per-attribute", type=int, default=1500)
    ap.add_argument("--depth", type=int, default=2)
    ap.add_argument("--width", type=int, default=1024)
    ap.add_argument("--threads", type=int, default=2)
    ap.add_argument("--only", nargs="*", help="attribute names (default: all with Commons categories)")
    args = ap.parse_args()

    root = args.data / "commons"
    (root / "images").mkdir(parents=True, exist_ok=True)
    manifest_path = root / "manifest.json"
    manifest: dict[str, dict] = json.loads(manifest_path.read_text()) if manifest_path.exists() else {}

    for attr in ATTRIBUTES:
        if not attr.commons or (args.only and attr.name not in args.only):
            continue
        print(f"{attr.name}: exploring {len(attr.commons)} categories", flush=True)
        titles = explore(attr.commons, args.depth, args.per_attribute)
        todo = [t for t in titles if t not in manifest]
        info = image_info(todo, args.width)
        for t in titles:
            if t in info:
                manifest[t] = {**info[t], "attributes": [attr.name]}
            elif t in manifest and attr.name not in manifest[t]["attributes"]:
                manifest[t]["attributes"].append(attr.name)
        print(f"  {len(titles)} files ({len(info)} new)", flush=True)
        manifest_path.write_text(json.dumps(manifest, indent=1))

    def download(item):
        title, m = item
        name = re.sub(r"[^A-Za-z0-9._-]", "_", title.removeprefix("File:"))[:150]
        name = Path(name).with_suffix(".jpg").name
        m["file"] = name
        return fetch(m["thumb"], root / "images" / name)

    items = list(manifest.items())
    done = 0
    with ThreadPoolExecutor(args.threads) as pool:
        for n, ok in enumerate(pool.map(download, items), 1):
            done += ok
            if n % 200 == 0:
                print(f"  downloaded {done}/{n} of {len(items)}", flush=True)
    manifest_path.write_text(json.dumps(manifest, indent=1))
    print(f"{done} images in {root / 'images'}")


if __name__ == "__main__":
    main()
