"""Exports a trained checkpoint to ONNX for MediaAnonymizer.

The graph takes one head crop and returns the detected accessories:

* input  `image`:  float32 [3, INPUT_SIZE, INPUT_SIZE], RGB, 0..1
* output `boxes`:  float32 [N, 4], x0 y0 x1 y1 in input pixels
* output `scores`: float32 [N]
* output `labels`: int64   [N], 1-based index into the `classes` metadata

Model metadata (ONNX `metadata_props`) records the class names, the crop
geometry and, when `--metrics` is given, the per-class score thresholds and
measured precision/recall, so the app never has to hard-code them.

The exported model is checked against PyTorch on a few validation crops.

Usage: python export.py --checkpoint runs/v1/best.pt --out accessories.onnx \\
           [--metrics runs/v1/test-metrics.json] [--data data]
"""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path

import numpy as np
import onnx
import onnxruntime as ort
import torch

from common import GROUPS, INPUT_SIZE, crop_geometry, group_names, load_split
from evaluate import crop_tensor
from train import build_model


class Wrapper(torch.nn.Module):
    """Single-crop interface around the torchvision detector."""

    def __init__(self, model: torch.nn.Module):
        super().__init__()
        self.model = model

    def forward(self, image: torch.Tensor):
        out = self.model([image])[0]
        return out["boxes"], out["scores"], out["labels"]


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--checkpoint", type=Path, required=True)
    ap.add_argument("--out", type=Path, default=Path("accessories.onnx"))
    ap.add_argument("--metrics", type=Path, help="evaluate.py --json output (test split) to embed")
    ap.add_argument("--data", type=Path, default=Path("data"), help="for the PyTorch/ONNX comparison")
    ap.add_argument("--version", default="1")
    args = ap.parse_args()

    state = torch.load(args.checkpoint, map_location="cpu", weights_only=False)
    model = build_model(pretrained=False)
    model.load_state_dict(state["model"])
    model.eval()
    wrapper = Wrapper(model).eval()

    dummy = torch.rand(3, INPUT_SIZE, INPUT_SIZE)
    torch.onnx.export(
        wrapper,
        (dummy,),
        str(args.out),
        input_names=["image"],
        output_names=["boxes", "scores", "labels"],
        dynamic_axes={"boxes": {0: "n"}, "scores": {0: "n"}, "labels": {0: "n"}},
        opset_version=17,
        dynamo=False,
    )

    m = onnx.load(str(args.out))
    meta = {
        "mediaanon.model": "accessories",
        "mediaanon.version": args.version,
        "mediaanon.classes": json.dumps(group_names()),
        "mediaanon.crop": json.dumps(crop_geometry()),
        "mediaanon.groups": json.dumps(GROUPS),
        "mediaanon.trained_epochs": str(state.get("epoch", "")),
        "mediaanon.license": "Model: MIT OR Apache-2.0. Trained on Open Images V7 annotations (CC BY 4.0, Google LLC) "
        "and images (CC BY 2.0, individual authors).",
    }
    if args.metrics:
        metrics = json.loads(args.metrics.read_text())
        meta["mediaanon.thresholds"] = json.dumps({k: g["threshold"] for k, g in metrics["groups"].items()})
        meta["mediaanon.metrics"] = json.dumps(
            {
                k: {
                    "ap50": g["ap50"],
                    "precision": g["at_threshold"]["precision"],
                    "recall": g["at_threshold"]["recall"],
                    "faces_with": g["faces_with"],
                }
                for k, g in metrics["groups"].items()
            }
            | {"crops": metrics["crops"], "target_precision": metrics["target_precision"]}
        )
    for k, v in meta.items():
        p = m.metadata_props.add()
        p.key, p.value = k, v
    onnx.checker.check_model(m)
    onnx.save(m, str(args.out))

    # PyTorch vs ONNX Runtime on real crops (or random inputs without data).
    session = ort.InferenceSession(str(args.out), providers=["CPUExecutionProvider"])
    try:
        root, samples = load_split(args.data, "validation")
        inputs = [crop_tensor(root / s.file)[0] for s in samples[:8]]
    except FileNotFoundError:
        inputs = [np.random.rand(3, INPUT_SIZE, INPUT_SIZE).astype(np.float32) for _ in range(4)]
    worst, checked = 0.0, 0
    for x in inputs:
        with torch.no_grad():
            tb, ts, tl = (t.numpy() for t in wrapper(torch.from_numpy(x)))
        ob, os_, ol = session.run(None, {"image": x})
        # Only detections clearly above the cut-off are comparable: among
        # equal scores (e.g. an untrained model) the order is arbitrary.
        cutoff = ts.min() + 1e-3 if len(ts) >= model.detections_per_img else 0.0
        for b, sc, lb in zip(tb, ts, tl, strict=True):
            if sc < max(0.1, cutoff):
                continue
            same = (ol == lb) & (np.abs(os_ - sc) < 1e-3)
            diff = np.abs(ob[same] - b).max(axis=1) if same.any() else np.array([np.inf])
            worst = max(worst, float(diff.min()))
            checked += 1
    if worst > 0.5:
        raise SystemExit(f"ONNX output differs from PyTorch (max box difference {worst:.3f} px)")

    digest = hashlib.sha256(args.out.read_bytes()).hexdigest()
    print(f"{args.out}: {args.out.stat().st_size / 1e6:.1f} MB, sha256 {digest}")
    print(f"PyTorch / ONNX Runtime: {checked} detections on {len(inputs)} crops, max box difference {worst:.2e} px")


if __name__ == "__main__":
    main()
