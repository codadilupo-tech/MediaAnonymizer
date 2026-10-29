#!/usr/bin/env python3
"""Generate the mobile app icons from the desktop design (packaging/make_icons.py).

    pip install pillow
    python3 mobile/scripts/make_icons.py

iOS wants an opaque full-bleed square (the system rounds the corners);
Android gets legacy launcher icons (rounded square) for old launchers and an
adaptive icon whose layer is the full-bleed artwork.
"""
import json
from pathlib import Path

from PIL import Image, ImageDraw

S = 1024
MOBILE = Path(__file__).resolve().parent.parent
IOS = MOBILE / "ios" / "Runner" / "Assets.xcassets" / "AppIcon.appiconset"
RES = MOBILE / "android" / "app" / "src" / "main" / "res"

# Desktop artwork coordinates live in a rounded square spanning 100..924.
D0, DS = 100, 824


def gradient(size, top, bottom):
    img = Image.new("RGB", (size, size))
    d = ImageDraw.Draw(img)
    for y in range(size):
        t = y / (size - 1)
        d.line([(0, y), (size, y)], fill=tuple(int(a + (b - a) * t) for a, b in zip(top, bottom)))
    return img


def artwork():
    """The icon without margins, shadow or rounding, as an opaque square."""
    k = S / DS

    def box(x0, y0, x1, y1):
        return tuple(round((v - D0) * k) for v in (x0, y0, x1, y1))

    img = gradient(S, (58, 48, 150), (28, 150, 160))
    d = ImageDraw.Draw(img)
    white = (250, 250, 255)
    d.ellipse(box(322, 250, 702, 650), fill=white)
    d.rounded_rectangle(box(232, 690, 792, 1100), round(230 * k), fill=white)
    d.rounded_rectangle(box(262, 392, 762, 482), round(22 * k), fill=(18, 18, 24))
    return img


def rounded(img, size, margin, radius):
    """Legacy Android launcher icon: rounded square with a small margin."""
    out = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    inner = size - 2 * margin
    mask = Image.new("L", (inner, inner), 0)
    ImageDraw.Draw(mask).rounded_rectangle((0, 0, inner - 1, inner - 1), radius, fill=255)
    out.paste(img.resize((inner, inner), Image.LANCZOS), (margin, margin), mask)
    return out


def main():
    art = artwork()

    contents = json.loads((IOS / "Contents.json").read_text())
    for image in contents["images"]:
        points = float(image["size"].split("x")[0])
        px = round(points * int(image["scale"].rstrip("x")))
        art.resize((px, px), Image.LANCZOS).save(IOS / image["filename"])

    densities = {"mdpi": 1, "hdpi": 1.5, "xhdpi": 2, "xxhdpi": 3, "xxxhdpi": 4}
    for name, scale in densities.items():
        folder = RES / f"mipmap-{name}"
        folder.mkdir(parents=True, exist_ok=True)
        legacy = round(48 * scale)
        rounded(art, legacy * 4, round(legacy * 4 * 0.06), round(legacy * 4 * 0.2)).resize(
            (legacy, legacy), Image.LANCZOS
        ).save(folder / "ic_launcher.png")
        layer = round(108 * scale)
        art.resize((layer, layer), Image.LANCZOS).save(folder / "ic_launcher_background.png")

    adaptive = RES / "mipmap-anydpi-v26"
    adaptive.mkdir(parents=True, exist_ok=True)
    (adaptive / "ic_launcher.xml").write_text(
        '<?xml version="1.0" encoding="utf-8"?>\n'
        '<adaptive-icon xmlns:android="http://schemas.android.com/apk/res/android">\n'
        '    <background android:drawable="@mipmap/ic_launcher_background" />\n'
        '    <foreground android:drawable="@android:color/transparent" />\n'
        "</adaptive-icon>\n"
    )


if __name__ == "__main__":
    main()
