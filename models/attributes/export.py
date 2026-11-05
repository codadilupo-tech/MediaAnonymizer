"""Exports the CLIP image tower and the trained heads as one ONNX model.

Input `crops`: float32 `[N, 3, 224, 224]`, the person crops normalised as
described by `common.crop_geometry()`. Output `scores`: float32 `[N, K]` in
0..1, one column per attribute (softmax over the roles, sigmoid for gear).

The attribute list, thresholds, crop geometry and attribution are stored in
the model's metadata (`mediaanon.attributes`, JSON), which is all the
application needs. The export is checked against PyTorch, then measured on
the test split, never seen while training.

Usage: python export.py --data data --head runs/v1/head.pt --out attributes.onnx [--int8]
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path

import numpy as np
import onnx
import onnxruntime as ort
import torch
import torch.nn.functional as F

from common import ATTRIBUTES, CLIP_MODEL, CLIP_PRETRAINED, INPUT_SIZE, crop_geometry
from train import ROLE, average_precision, load_clip, to_tensor

ATTRIBUTION = (
    "Image tower: OpenCLIP ViT-B/32 trained on LAION-2B (laion2b_s34b_b79k), MIT licence. "
    "Heads trained by MediaAnonymizer on person crops from Wikimedia Commons (individual free licences, "
    "listed in the training manifest) and Open Images V7 (annotations CC BY 4.0 Google LLC, images CC BY 2.0)."
)


class Model(torch.nn.Module):
    def __init__(self, visual, w, b):
        super().__init__()
        self.visual = visual
        self.register_buffer("w", w)
        self.register_buffer("b", b)
        self.register_buffer("role", ROLE.clone())

    def forward(self, crops):
        e = F.normalize(self.visual(crops), dim=-1)
        z = e @ self.w.T + self.b
        roles = torch.softmax(z.masked_fill(~self.role, -1e9), dim=1)
        return torch.where(self.role, roles, torch.sigmoid(z))


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--data", type=Path, default=Path("data"))
    ap.add_argument("--head", type=Path, default=Path("runs/v1/head.pt"))
    ap.add_argument("--out", type=Path, default=Path("attributes.onnx"))
    ap.add_argument("--int8", action="store_true", help="quantize the weights to 8 bits (4x smaller)")
    args = ap.parse_args()

    head = torch.load(args.head)
    clip, _ = load_clip(torch.device("cpu"))
    model = Model(clip.visual.float(), head["w"], head["b"]).eval()
    dummy = torch.zeros(2, 3, INPUT_SIZE, INPUT_SIZE)
    tmp = args.out.with_suffix(".fp32.onnx") if args.int8 else args.out
    torch.onnx.export(
        model, (dummy,), str(tmp), input_names=["crops"], output_names=["scores"],
        dynamic_axes={"crops": {0: "n"}, "scores": {0: "n"}}, opset_version=17, dynamo=False,
    )
    if args.int8:
        from onnxruntime.quantization import QuantType, quantize_dynamic

        quantize_dynamic(str(tmp), str(args.out), weight_type=QuantType.QInt8)
        tmp.unlink()

    meta = {
        "version": 1,
        "attributes": [
            {"name": a.name, "label": a.label, "group": a.group,
             "present": head["thresholds"][a.name]["present"], "absent": head["thresholds"][a.name]["absent"]}
            for a in ATTRIBUTES
        ],
        "crop": crop_geometry(),
        "backbone": f"{CLIP_MODEL} {CLIP_PRETRAINED}",
        "attribution": ATTRIBUTION,
    }
    m = onnx.load(str(args.out))
    entry = m.metadata_props.add()
    entry.key, entry.value = "mediaanon.attributes", json.dumps(meta)
    m.producer_name = "mediaanon-attributes"
    onnx.save(m, str(args.out))
    print(f"wrote {args.out} ({args.out.stat().st_size / 1e6:.0f} MB)")

    # Check against PyTorch, then measure on the test split.
    sess = ort.InferenceSession(str(args.out), providers=["CPUExecutionProvider"])
    split = json.loads((args.data / "test.json").read_text())["samples"]
    from PIL import Image

    scores = []
    for i in range(0, len(split), 64):
        x = torch.stack([to_tensor(Image.open(args.data / "crops" / s["file"]).convert("RGB")) for s in split[i : i + 64]])
        scores.append(sess.run(None, {"crops": x.numpy()})[0])
        if i == 0:
            with torch.no_grad():
                ref = model(x).numpy()
            print(f"max |onnx - torch| on the first batch: {np.abs(scores[0] - ref).max():.4f}")
    p = np.concatenate(scores)
    # Only labels people gave (weight 1) count; weak ones are guesses.
    y = np.where(np.array([s["weights"] for s in split]) >= 1.0, np.array([s["labels"] for s in split]), -1)
    report = {}
    for i, a in enumerate(meta["attributes"]):
        k = y[:, i] >= 0
        pi, yi = p[k, i], y[k, i]
        if yi.sum() == 0:
            continue
        sel = pi >= a["present"]
        report[a["name"]] = {
            "positives": int(yi.sum()), "negatives": int((yi == 0).sum()),
            "ap": average_precision(pi, yi),
            "precision_at_present": float((sel & (yi == 1)).sum() / max(1, sel.sum())),
            "recall_at_present": float((sel & (yi == 1)).sum() / yi.sum()),
            "positives_called_absent": float((pi[yi == 1] < a["absent"]).mean()),
        }
    path = args.out.with_suffix(".metrics.json")
    path.write_text(json.dumps(report, indent=1))
    print("test split:")
    for name, r in report.items():
        print(f"  {name:12s} AP {r['ap']:.2f}  P {r['precision_at_present']:.2f}  R {r['recall_at_present']:.2f}  "
              f"missed as absent {r['positives_called_absent']:.2f}  (+{r['positives']}/-{r['negatives']})")


if __name__ == "__main__":
    main()
