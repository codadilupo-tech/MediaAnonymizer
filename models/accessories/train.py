"""Trains the accessory detector on the prepared head crops.

Model: torchvision SSDlite320 with a MobileNetV3-Large backbone (BSD-3-Clause),
small, fast on a CPU at inference and exportable to ONNX. The backbone starts
from ImageNet weights: torchvision's by default, or a file given with
`--backbone` (e.g. made by convert_keras_backbone.py when download.pytorch.org
is unreachable); `--backbone none` starts from random weights. After each
epoch the model is scored on the validation split and the best checkpoint is
kept.

Usage: python train.py --data data --out runs/v1 [--epochs 40] [--batch 32]
"""

from __future__ import annotations

import argparse
import json
import math
import random
import time
from functools import partial
from pathlib import Path

import torch
from PIL import Image
from torch import nn
from torch.utils.data import DataLoader, Dataset
from torchvision import tv_tensors
from torchvision.models import MobileNet_V3_Large_Weights, mobilenet_v3_large
from torchvision.models.detection import _utils as det_utils
from torchvision.models.detection.anchor_utils import DefaultBoxGenerator
from torchvision.models.detection.ssd import SSD
from torchvision.models.detection.ssdlite import SSDLiteHead, _mobilenet_extractor, _normal_init
from torchvision.transforms import v2 as T

import evaluate
from common import GROUPS, INPUT_SIZE, Sample, load_split


def build_model(backbone_state: dict | None = None, reduced_tail: bool | None = None) -> torch.nn.Module:
    """SSDlite320 / MobileNetV3-Large, as torchvision's
    `ssdlite320_mobilenet_v3_large`, with an explicit backbone.

    `backbone_state` is a state dict of torchvision's `mobilenet_v3_large`
    (ImageNet weights); `None` initialises it randomly. Like torchvision, a
    random backbone uses the "reduced tail" variant; checkpoints record which
    one they use, so that they can be rebuilt without any download.
    """
    if reduced_tail is None:
        reduced_tail = backbone_state is None
    norm = partial(nn.BatchNorm2d, eps=0.001, momentum=0.03)
    backbone = mobilenet_v3_large(weights=None, norm_layer=norm, reduced_tail=reduced_tail)
    if backbone_state is None:
        _normal_init(backbone)
    else:
        # The ImageNet classifier is not used by the detector and may be absent.
        missing, unexpected = backbone.load_state_dict(backbone_state, strict=False)
        if unexpected or any(not k.startswith("classifier.") for k in missing):
            raise ValueError(f"backbone weights do not fit: missing {missing}, unexpected {unexpected}")
    backbone = _mobilenet_extractor(backbone, 6, norm)
    size = (INPUT_SIZE, INPUT_SIZE)
    anchors = DefaultBoxGenerator([[2, 3] for _ in range(6)], min_ratio=0.2, max_ratio=0.95)
    channels = det_utils.retrieve_out_channels(backbone, size)
    classes = len(GROUPS) + 1
    return SSD(
        backbone,
        anchors,
        size,
        classes,
        head=SSDLiteHead(channels, anchors.num_anchors_per_location(), classes, norm),
        score_thresh=0.01,
        nms_thresh=0.55,
        detections_per_img=20,
        topk_candidates=300,
        # [0, 1] -> [-1, 1], what the backbone was trained with.
        image_mean=[0.5, 0.5, 0.5],
        image_std=[0.5, 0.5, 0.5],
    )


def backbone_weights(spec: str) -> dict | None:
    """`imagenet` (torchvision download), `none`, or a state dict file."""
    if spec == "none":
        return None
    if spec == "imagenet":
        return MobileNet_V3_Large_Weights.IMAGENET1K_V1.get_state_dict(progress=True)
    return torch.load(spec, map_location="cpu", weights_only=True)


def load_checkpoint(path: Path) -> tuple[torch.nn.Module, dict]:
    """A trained model (no download needed) and its checkpoint data."""
    state = torch.load(path, map_location="cpu", weights_only=False)
    model = build_model(reduced_tail=state.get("reduced_tail", True))
    model.load_state_dict(state["model"])
    model.eval()
    return model, state


class Crops(Dataset):
    """Head crops as tensors in 0..1 with their boxes."""

    def __init__(self, root: Path, samples: list[Sample], train: bool):
        self.root, self.samples, self.train = root, samples, train
        # Geometry jitter imitates the difference between Open Images face
        # boxes and YuNet's; photometric jitter imitates video conditions.
        # Colours are not learnt (they are measured afterwards), but hue is
        # left alone anyway so that "blue helmet" stays a blue helmet.
        self.augment = T.Compose(
            [
                T.RandomHorizontalFlip(),
                T.RandomResizedCrop(INPUT_SIZE, scale=(0.75, 1.0), ratio=(0.8, 1.25), antialias=True),
                T.ColorJitter(brightness=0.3, contrast=0.3, saturation=0.2),
                T.SanitizeBoundingBoxes(min_size=2),
            ]
        )

    def __len__(self) -> int:
        return len(self.samples)

    def __getitem__(self, i: int):
        s = self.samples[i]
        img = T.functional.to_image(Image.open(self.root / s.file).convert("RGB"))
        img = T.functional.to_dtype(img, torch.float32, scale=True)
        boxes = tv_tensors.BoundingBoxes(
            torch.tensor(s.boxes, dtype=torch.float32).reshape(-1, 4), format="XYXY", canvas_size=(s.height, s.width)
        )
        target = {"boxes": boxes, "labels": torch.tensor(s.labels, dtype=torch.int64)}
        if self.train:
            img, target = self.augment(img, target)
        return img, target


def collate(batch):
    return tuple(zip(*batch, strict=True))


def device() -> torch.device:
    if torch.cuda.is_available():
        return torch.device("cuda")
    if torch.backends.mps.is_available():
        return torch.device("mps")
    return torch.device("cpu")


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--data", type=Path, default=Path("data"))
    ap.add_argument("--out", type=Path, default=Path("runs/v1"))
    ap.add_argument("--epochs", type=int, default=40)
    ap.add_argument("--batch", type=int, default=32)
    ap.add_argument("--lr", type=float, default=1e-3)
    ap.add_argument("--workers", type=int, default=4)
    ap.add_argument(
        "--backbone", default="imagenet", help="imagenet (torchvision download), none, or a state dict file"
    )
    ap.add_argument("--no-pretrained", action="store_true", help="same as --backbone none")
    ap.add_argument("--resume", type=Path, help="continue from a checkpoint (e.g. runs/v1/last.pt)")
    ap.add_argument("--limit", type=int, default=0, help="use at most N crops per split (quick tests)")
    ap.add_argument("--seed", type=int, default=0)
    args = ap.parse_args()

    torch.manual_seed(args.seed)
    random.seed(args.seed)
    dev = device()
    args.out.mkdir(parents=True, exist_ok=True)

    train_root, train_samples = load_split(args.data, "train")
    val_root, val_samples = load_split(args.data, "validation")
    if args.limit:
        train_samples, val_samples = train_samples[: args.limit], val_samples[: args.limit]
    train_loader = DataLoader(
        Crops(train_root, train_samples, train=True),
        batch_size=args.batch,
        shuffle=True,
        num_workers=args.workers,
        collate_fn=collate,
        drop_last=len(train_samples) > args.batch,
    )
    print(f"device {dev}; {len(train_samples):,} train crops, {len(val_samples):,} validation crops")

    resumed = torch.load(args.resume, map_location="cpu", weights_only=False) if args.resume else None
    if resumed:
        # The backbone weights are in the checkpoint: nothing to download.
        reduced_tail = resumed.get("reduced_tail", True)
        model = build_model(reduced_tail=reduced_tail)
        model.load_state_dict(resumed["model"])
        model.to(dev)
    else:
        backbone = backbone_weights("none" if args.no_pretrained else args.backbone)
        reduced_tail = backbone is None
        model = build_model(backbone, reduced_tail).to(dev)
    params = [p for p in model.parameters() if p.requires_grad]
    opt = torch.optim.AdamW(params, lr=args.lr, weight_decay=1e-4)
    steps = max(1, args.epochs * len(train_loader))
    warmup = min(500, steps // 10)
    sched = torch.optim.lr_scheduler.LambdaLR(
        opt,
        lambda s: (s + 1) / warmup if s < warmup else 0.5 * (1 + math.cos(math.pi * (s - warmup) / (steps - warmup))),
    )
    use_amp = dev.type == "cuda"
    scaler = torch.amp.GradScaler(enabled=use_amp)

    best, first = -1.0, 1
    if resumed:
        first = resumed["epoch"] + 1
        best_file = args.out / "best.pt"
        best = (
            torch.load(best_file, map_location="cpu", weights_only=False)["metrics"]["mean_ap50"]
            if best_file.exists()
            else resumed["metrics"]["mean_ap50"]
        )
        if "optimizer" in resumed:
            opt.load_state_dict(resumed["optimizer"])
            sched.load_state_dict(resumed["scheduler"])
            scaler.load_state_dict(resumed["scaler"])
        else:
            # Older checkpoint: fresh optimizer moments, schedule moved on.
            for _ in range(resumed["epoch"] * len(train_loader)):
                sched.step()
        print(f"resuming after epoch {resumed['epoch']} (best validation mAP@0.5 so far {best:.3f})")
    log = (args.out / "log.jsonl").open("a")
    for epoch in range(first, args.epochs + 1):
        model.train()
        started, total, n = time.time(), 0.0, 0
        for images, targets in train_loader:
            images = [im.to(dev) for im in images]
            targets = [{k: v.to(dev) for k, v in t.items()} for t in targets]
            with torch.autocast(dev.type, enabled=use_amp):
                losses = model(images, targets)
                loss = sum(losses.values())
            opt.zero_grad(set_to_none=True)
            scaler.scale(loss).backward()
            scaler.step(opt)
            scaler.update()
            sched.step()
            total, n = total + loss.item(), n + 1
        metrics = evaluate.evaluate(evaluate.TorchPredictor(model, dev), val_root, val_samples)
        score = metrics["mean_ap50"]
        entry = {"epoch": epoch, "loss": total / max(n, 1), "seconds": round(time.time() - started), **metrics}
        log.write(json.dumps(entry) + "\n")
        log.flush()
        print(f"epoch {epoch}: loss {entry['loss']:.3f}, val mAP@0.5 {score:.3f} ({entry['seconds']}s)", flush=True)
        state = {
            "model": model.state_dict(),
            "reduced_tail": reduced_tail,
            "epoch": epoch,
            "metrics": metrics,
            "optimizer": opt.state_dict(),
            "scheduler": sched.state_dict(),
            "scaler": scaler.state_dict(),
            "args": vars(args) | {"data": str(args.data), "out": str(args.out)},
        }
        torch.save(state, args.out / "last.pt")
        if score > best:
            best = score
            torch.save(state, args.out / "best.pt")
    print(f"best validation mAP@0.5: {best:.3f} -> {args.out / 'best.pt'}")


if __name__ == "__main__":
    main()
