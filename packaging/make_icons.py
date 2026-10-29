#!/usr/bin/env python3
"""Generate the application icons in packaging/icons.

    pip install pillow
    python3 packaging/make_icons.py

Produces icon.png (1024x1024, used for the macOS .icns built in CI and the
Linux package) and icon.ico (Windows executable and installer).
"""
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter

S = 1024
OUT = Path(__file__).resolve().parent / "icons"


def gradient(size, top, bottom):
    img = Image.new("RGB", (size, size))
    px = img.load()
    for y in range(size):
        t = y / (size - 1)
        c = tuple(int(a + (b - a) * t) for a, b in zip(top, bottom))
        for x in range(size):
            px[x, y] = c
    return img


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    # Background: rounded square (macOS-style margins), indigo -> teal.
    margin, radius = 100, 185
    bg = gradient(S, (58, 48, 150), (28, 150, 160)).convert("RGBA")
    mask = Image.new("L", (S, S), 0)
    ImageDraw.Draw(mask).rounded_rectangle((margin, margin, S - margin, S - margin), radius, fill=255)
    icon = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    shadow = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    ImageDraw.Draw(shadow).rounded_rectangle((margin, margin + 18, S - margin, S - margin + 18), radius, fill=(0, 0, 0, 90))
    icon.alpha_composite(shadow.filter(ImageFilter.GaussianBlur(22)))
    icon.paste(bg, (0, 0), mask)

    # Portrait: head and shoulders, clipped to the rounded square.
    fig = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    d = ImageDraw.Draw(fig)
    white = (250, 250, 255, 255)
    d.ellipse((322, 250, 702, 650), fill=white)                     # head
    d.rounded_rectangle((232, 690, 792, 1100), 230, fill=white)     # shoulders
    clip = Image.new("L", (S, S), 0)
    ImageDraw.Draw(clip).rounded_rectangle((margin, margin, S - margin, S - margin), radius, fill=255)
    fig.putalpha(Image.composite(fig.getchannel("A"), Image.new("L", (S, S), 0), clip))
    icon.alpha_composite(fig)

    # The censor band across the eyes.
    band = Image.new("RGBA", (S, S), (0, 0, 0, 0))
    ImageDraw.Draw(band).rounded_rectangle((262, 392, 762, 482), 22, fill=(18, 18, 24, 255))
    icon.alpha_composite(band)

    icon.save(OUT / "icon.png")
    icon.save(OUT / "icon.ico", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)])


if __name__ == "__main__":
    main()
