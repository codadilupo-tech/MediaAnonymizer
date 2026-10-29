"""Measures the accessory detector on a prepared split.

Two kinds of numbers are reported per accessory group:

* **AP@0.5**: box detection quality (Pascal VOC style average precision).
* **Presence** at several score thresholds: does the crop of a face contain
  the accessory? This is what the app relies on, so precision and recall are
  reported, and the suggested threshold is the lowest one that reaches the
  target precision (default 0.9), or the best F1 when none does.

Works on a PyTorch checkpoint or on the exported ONNX model (`--onnx`), so the
artifact that ships can be measured directly.

Usage:
  python evaluate.py --data data --checkpoint runs/v1/best.pt [--split test]
  python evaluate.py --data data --onnx accessories.onnx --report METRICS.md
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

import numpy as np
from PIL import Image

from common import GROUPS, INPUT_SIZE, Sample, group_names, load_split

THRESHOLDS = [round(t, 2) for t in np.arange(0.1, 0.96, 0.05)]


def crop_tensor(path: Path) -> tuple[np.ndarray, tuple[int, int]]:
    """A crop as the model expects it: RGB, 0..1, CHW, INPUT_SIZE square."""
    img = Image.open(path).convert("RGB")
    size = img.size
    arr = np.asarray(img.resize((INPUT_SIZE, INPUT_SIZE), Image.Resampling.BILINEAR), dtype=np.float32) / 255.0
    return arr.transpose(2, 0, 1).copy(), size


class TorchPredictor:
    def __init__(self, model, device):
        self.model, self.device = model, device

    def __call__(self, x: np.ndarray):
        import torch

        self.model.eval()
        with torch.no_grad():
            out = self.model([torch.from_numpy(x).to(self.device)])[0]
        return out["boxes"].cpu().numpy(), out["scores"].cpu().numpy(), out["labels"].cpu().numpy()


class OnnxPredictor:
    def __init__(self, path: Path):
        import onnxruntime as ort

        self.session = ort.InferenceSession(str(path), providers=["CPUExecutionProvider"])

    def __call__(self, x: np.ndarray):
        boxes, scores, labels = self.session.run(None, {"image": x})
        return boxes, scores, labels


def average_precision(scores: list[float], hits: list[bool], positives: int) -> float:
    if positives == 0:
        return float("nan")
    order = np.argsort(-np.asarray(scores))
    tp = np.cumsum(np.asarray(hits, dtype=float)[order])
    fp = np.cumsum(1 - np.asarray(hits, dtype=float)[order])
    recall = tp / positives
    precision = tp / np.maximum(tp + fp, 1e-9)
    # All-point interpolation.
    mrec = np.concatenate([[0.0], recall, [1.0]])
    mpre = np.concatenate([[0.0], precision, [0.0]])
    for i in range(len(mpre) - 2, -1, -1):
        mpre[i] = max(mpre[i], mpre[i + 1])
    idx = np.where(mrec[1:] != mrec[:-1])[0]
    return float(np.sum((mrec[idx + 1] - mrec[idx]) * mpre[idx + 1]))


def iou(a, b) -> float:
    ix = max(0.0, min(a[2], b[2]) - max(a[0], b[0]))
    iy = max(0.0, min(a[3], b[3]) - max(a[1], b[1]))
    inter = ix * iy
    union = (a[2] - a[0]) * (a[3] - a[1]) + (b[2] - b[0]) * (b[3] - b[1]) - inter
    return inter / union if union > 0 else 0.0


def evaluate(predict, root: Path, samples: list[Sample], target_precision: float = 0.9) -> dict:
    n = len(GROUPS)
    det_scores = [[] for _ in range(n)]
    det_hits = [[] for _ in range(n)]
    positives = [0] * n
    # Presence: best score of the group in the crop, and whether it is there.
    presence = [[] for _ in range(n)]
    for s in samples:
        x, (w, h) = crop_tensor(root / s.file)
        boxes, scores, labels = predict(x)
        # Back to crop pixels.
        sx, sy = w / INPUT_SIZE, h / INPUT_SIZE
        boxes = [[b[0] * sx, b[1] * sy, b[2] * sx, b[3] * sy] for b in boxes]
        for g in range(n):
            label = g + 1
            gt = [b for b, lab in zip(s.boxes, s.labels, strict=True) if lab == label]
            positives[g] += len(gt)
            used = [False] * len(gt)
            mine = sorted(
                ((sc, b) for b, sc, lab in zip(boxes, scores, labels, strict=True) if lab == label), key=lambda t: -t[0]
            )
            for sc, b in mine:
                best, j = 0.0, -1
                for k, gb in enumerate(gt):
                    o = iou(b, gb)
                    if o > best and not used[k]:
                        best, j = o, k
                hit = best >= 0.5
                if hit:
                    used[j] = True
                det_scores[g].append(float(sc))
                det_hits[g].append(hit)
            presence[g].append((max((sc for sc, _ in mine), default=0.0), bool(gt)))

    result = {"crops": len(samples), "groups": {}}
    aps = []
    for g, name in enumerate(group_names()):
        ap = average_precision(det_scores[g], det_hits[g], positives[g])
        rows = []
        truth = np.array([p for _, p in presence[g]])
        score = np.array([s for s, _ in presence[g]])
        for t in THRESHOLDS:
            pred = score >= t
            tp = int(np.sum(pred & truth))
            fp = int(np.sum(pred & ~truth))
            fn = int(np.sum(~pred & truth))
            p = tp / (tp + fp) if tp + fp else float("nan")
            r = tp / (tp + fn) if tp + fn else float("nan")
            f1 = 2 * p * r / (p + r) if p == p and r == r and p + r else 0.0
            rows.append({"threshold": t, "precision": p, "recall": r, "f1": f1})
        ok = [row for row in rows if row["precision"] == row["precision"] and row["precision"] >= target_precision]
        chosen = ok[0] if ok else max(rows, key=lambda row: row["f1"])
        result["groups"][name] = {
            "boxes": positives[g],
            "faces_with": int(truth.sum()),
            "ap50": ap,
            "presence": rows,
            "threshold": chosen["threshold"],
            "at_threshold": chosen,
            "meets_target_precision": bool(ok),
        }
        if ap == ap:
            aps.append(ap)
    result["mean_ap50"] = float(np.mean(aps)) if aps else 0.0
    result["target_precision"] = target_precision
    return result


def report(m: dict, title: str) -> str:
    lines = [
        f"# {title}",
        "",
        f"{m['crops']:,} head crops. Presence = the face's crop contains the accessory; "
        f"threshold = lowest score reaching precision {m['target_precision']:.0%} (else best F1).",
        "",
        "| Accessory | Boxes | Faces with it | AP@0.5 | Threshold | Precision | Recall |",
        "|---|---:|---:|---:|---:|---:|---:|",
    ]
    for name, g in m["groups"].items():
        a = g["at_threshold"]
        flag = "" if g["meets_target_precision"] else " (below target)"
        lines.append(
            f"| {name} | {g['boxes']:,} | {g['faces_with']:,} | {g['ap50']:.3f} | {g['threshold']:.2f} | "
            f"{a['precision']:.3f}{flag} | {a['recall']:.3f} |"
        )
    lines += ["", f"Mean AP@0.5: **{m['mean_ap50']:.3f}**", ""]
    return "\n".join(lines)


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--data", type=Path, default=Path("data"))
    ap.add_argument("--split", default="test")
    src = ap.add_mutually_exclusive_group(required=True)
    src.add_argument("--checkpoint", type=Path)
    src.add_argument("--onnx", type=Path)
    ap.add_argument("--target-precision", type=float, default=0.9)
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--json", type=Path, help="write the metrics as JSON")
    ap.add_argument("--report", type=Path, help="write a Markdown summary")
    args = ap.parse_args()

    root, samples = load_split(args.data, args.split)
    if args.limit:
        samples = samples[: args.limit]
    if args.onnx:
        predict = OnnxPredictor(args.onnx)
        title = f"{args.onnx.name} on the {args.split} split"
    else:
        import torch

        from train import build_model

        state = torch.load(args.checkpoint, map_location="cpu", weights_only=False)
        model = build_model(pretrained=False)
        model.load_state_dict(state["model"])
        predict = TorchPredictor(model, torch.device("cpu"))
        title = f"{args.checkpoint} on the {args.split} split"
    m = evaluate(predict, root, samples, args.target_precision)
    text = report(m, title)
    print(text)
    if args.json:
        args.json.write_text(json.dumps(m, indent=2))
    if args.report:
        args.report.write_text(text)


if __name__ == "__main__":
    main()
