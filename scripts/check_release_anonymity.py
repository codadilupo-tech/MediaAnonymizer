#!/usr/bin/env python3
"""Fail if a release file contains a string that identifies the maintainer.

Releases must not reveal who builds them: no account name, real name or e-mail
in binaries, installers, metadata or bundled documents. This scans every file
given (directories recursively), looks inside .zip/.apk/.ipa/.jar/.aar and
.tar.gz/.tgz archives (nested ones too), and searches the raw bytes for each
forbidden string, case-insensitively, in UTF-8 and UTF-16LE (Windows version
resources, iOS/Android string tables).

The forbidden strings come from the MEDIAANON_FORBIDDEN environment variable,
one per line; in CI it holds the repository owner plus the optional
RELEASE_FORBIDDEN_STRINGS secret, so the names themselves are never committed.
Strings shorter than 4 characters are ignored (too many false positives).

Disk images (.dmg) are compressed and not scanned here: the release workflow
scans the .app before it is put into the image.

Usage:
    MEDIAANON_FORBIDDEN=$'name\\nemail@example.com' \\
        python3 scripts/check_release_anonymity.py FILE_OR_DIR...
"""
import io
import os
import sys
import tarfile
import zipfile

ZIP_SUFFIXES = (".zip", ".apk", ".ipa", ".jar", ".aar")
TAR_SUFFIXES = (".tar.gz", ".tgz", ".tar")
SKIPPED_SUFFIXES = (".dmg",)


def needles(words):
    return [w.lower().encode(enc) for w in words for enc in ("utf-8", "utf-16-le")]


def scan_bytes(name, data, patterns, hits):
    low = data.lower()
    if any(needle in low for needle in patterns):
        hits.append(name)
    lname = name.lower()
    if lname.endswith(ZIP_SUFFIXES):
        with zipfile.ZipFile(io.BytesIO(data)) as z:
            for info in z.infolist():
                if not info.is_dir():
                    scan_bytes(f"{name}!{info.filename}", z.read(info), patterns, hits)
    elif lname.endswith(TAR_SUFFIXES):
        with tarfile.open(fileobj=io.BytesIO(data)) as t:
            for member in t.getmembers():
                if member.isfile():
                    scan_bytes(f"{name}!{member.name}", t.extractfile(member).read(), patterns, hits)
    # Entry and member names are checked too: a path can identify as well.
    if any(needle in lname.encode("utf-8", "replace") for needle in patterns):
        hits.append(f"{name} (file name)")


def files(paths):
    for p in paths:
        if os.path.isdir(p):
            for root, _, names in os.walk(p):
                for n in sorted(names):
                    yield os.path.join(root, n)
        else:
            yield p


def main(paths):
    words = [w.strip() for w in os.environ.get("MEDIAANON_FORBIDDEN", "").splitlines()]
    words = sorted({w for w in words if len(w) >= 4})
    if not words:
        print("error: MEDIAANON_FORBIDDEN is empty, nothing to check against", file=sys.stderr)
        return 2
    patterns = needles(words)
    hits, count = [], 0
    for f in files(paths):
        if f.lower().endswith(SKIPPED_SUFFIXES):
            print(f"skipped (disk image): {f}")
            continue
        with open(f, "rb") as fh:
            scan_bytes(f, fh.read(), patterns, hits)
        count += 1
    # Name the file only: the log must not repeat the strings.
    for h in sorted(set(hits)):
        print(f"::error::identifying string found in {h}")
    print(f"checked {count} file(s) against {len(words)} forbidden string(s): "
          f"{'FAILED' if hits else 'clean'}")
    return 1 if hits else 0


if __name__ == "__main__":
    if len(sys.argv) < 2:
        print(__doc__, file=sys.stderr)
        sys.exit(2)
    sys.exit(main(sys.argv[1:]))
