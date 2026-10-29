"""End-to-end check of the pipeline on a tiny synthetic dataset.

Draws fake heads (an ellipse) with a coloured "hat" or "helmet" box above
and dark "glasses" across, writes them in the layout produced by
prepare.py, then trains briefly (from scratch), evaluates, exports and
evaluates the exported model. The task is trivial, so the model learns it
and the PyTorch/ONNX comparison has real detections to check. About four
minutes on a 4-core CPU, without any download.

Usage: python smoke_test.py
"""

from __future__ import annotations

import json
import random
import subprocess
import sys
import tempfile
from pathlib import Path

from PIL import Image, ImageDraw

from common import GROUPS

HERE = Path(__file__).resolve().parent


def make_split(root: Path, split: str, n: int, rng: random.Random) -> None:
    out = root / "crops" / split
    out.mkdir(parents=True)
    samples = []
    for i in range(n):
        w, h = rng.randint(120, 320), rng.randint(120, 320)
        img = Image.new("RGB", (w, h), tuple(rng.randint(0, 255) for _ in range(3)))
        d = ImageDraw.Draw(img)
        fx0, fy0, fx1, fy1 = w * 0.25, h * 0.45, w * 0.75, h * 0.9
        d.ellipse((fx0, fy0, fx1, fy1), fill=(224, 172, 140))
        boxes, labels = [], []
        kind = rng.randrange(len(GROUPS) + 1)
        if kind in (1, 2):  # hat / helmet: a box above the face
            b = [w * 0.2, h * 0.2, w * 0.8, h * 0.5]
            d.rectangle(b, fill=(30, 60, 200) if kind == 2 else (150, 40, 40))
            boxes.append(b)
            labels.append(kind)
        elif kind in (3, 4):  # glasses / sunglasses: a band across the eyes
            b = [w * 0.3, h * 0.55, w * 0.7, h * 0.65]
            d.rectangle(b, outline=(0, 0, 0), width=3, fill=(10, 10, 10) if kind == 4 else None)
            boxes.append(b)
            labels.append(kind)
        name = f"{split}_{i}.jpg"
        img.save(out / name)
        samples.append(
            {"file": name, "width": w, "height": h, "boxes": boxes, "labels": labels, "face": [fx0, fy0, fx1, fy1]}
        )
    (out / "annotations.json").write_text(json.dumps(samples))


def run(*args: str) -> None:
    print("$", " ".join(args), flush=True)
    subprocess.run([sys.executable, *args], cwd=HERE, check=True)


def main() -> None:
    rng = random.Random(0)
    with tempfile.TemporaryDirectory() as tmp:
        data = Path(tmp) / "data"
        for split, n in [("train", 160), ("validation", 32), ("test", 32)]:
            make_split(data, split, n, rng)
        runs = Path(tmp) / "run"
        run(
            "train.py",
            "--data",
            str(data),
            "--out",
            str(runs),
            "--epochs",
            "25",
            "--batch",
            "16",
            "--lr",
            "3e-3",
            "--workers",
            "0",
            "--no-pretrained",
        )
        metrics = runs / "metrics.json"
        run("evaluate.py", "--data", str(data), "--checkpoint", str(runs / "best.pt"), "--json", str(metrics))
        onnx_path = Path(tmp) / "accessories.onnx"
        run(
            "export.py",
            "--checkpoint",
            str(runs / "best.pt"),
            "--out",
            str(onnx_path),
            "--metrics",
            str(metrics),
            "--data",
            str(data),
        )
        run("evaluate.py", "--data", str(data), "--onnx", str(onnx_path))
    print("smoke test passed")


if __name__ == "__main__":
    main()
