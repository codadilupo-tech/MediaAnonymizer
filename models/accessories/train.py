"""Trains the accessory detector on the prepared head crops.

Model: torchvision SSDlite320 with a MobileNetV3-Large backbone (BSD-3-Clause),
initialised from ImageNet weights unless `--no-pretrained` (small, fast on a
CPU at inference, exportable to ONNX). After each epoch the model is scored
on the validation split and the best checkpoint is kept.

Usage: python train.py --data data --out runs/v1 [--epochs 40] [--batch 32]
"""

from __future__ import annotations

import argparse
import json
import math
import random
import time
from pathlib import Path

import torch
from PIL import Image
from torch.utils.data import DataLoader, Dataset
from torchvision import tv_tensors
from torchvision.models import MobileNet_V3_Large_Weights
from torchvision.models.detection import ssdlite320_mobilenet_v3_large
from torchvision.transforms import v2 as T

import evaluate
from common import GROUPS, INPUT_SIZE, Sample, load_split


def build_model(pretrained: bool) -> torch.nn.Module:
    weights = MobileNet_V3_Large_Weights.IMAGENET1K_V1 if pretrained else None
    model = ssdlite320_mobilenet_v3_large(
        weights=None, weights_backbone=weights, num_classes=len(GROUPS) + 1, score_thresh=0.01, detections_per_img=20
    )
    return model


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
    ap.add_argument("--no-pretrained", action="store_true", help="random backbone (no ImageNet download)")
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

    model = build_model(pretrained=not args.no_pretrained).to(dev)
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

    best = -1.0
    log = (args.out / "log.jsonl").open("a")
    for epoch in range(1, args.epochs + 1):
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
            "epoch": epoch,
            "metrics": metrics,
            "args": vars(args) | {"data": str(args.data), "out": str(args.out)},
        }
        torch.save(state, args.out / "last.pt")
        if score > best:
            best = score
            torch.save(state, args.out / "best.pt")
    print(f"best validation mAP@0.5: {best:.3f} -> {args.out / 'best.pt'}")


if __name__ == "__main__":
    main()
