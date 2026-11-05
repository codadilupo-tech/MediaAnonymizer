"""Trains the attribute heads on frozen CLIP image embeddings.

1. Every crop of `prepare.py` is embedded once with the CLIP image tower
   (cached in `<data>/embeddings-<split>.pt`). Training crops are embedded a
   second time after a random downscale to 32–96 pixels and back, which
   imitates small faces in video frames.
2. A linear layer maps the embedding to one logit per attribute. It starts
   from CLIP's zero-shot classifier (the text embeddings of the prompts of
   `common.ATTRIBUTES`; for gear, "wearing X" minus "a person") and is
   trained with a softmax over the roles and a sigmoid per gear item, with
   weight decay *towards* that start, so attributes with few or noisy
   examples stay close to CLIP's own judgement.
3. On the validation split, two thresholds are chosen per attribute:
   `present` (lowest score with a precision of at least `--precision`) and
   `absent` (highest score below which at most `1 - --recall` of the
   positives fall). The application treats anything in between as unsure.

Usage: python train.py --data data --out runs/v1
"""

from __future__ import annotations

import argparse
import json
import random
from pathlib import Path

import numpy as np
import open_clip
import torch
import torch.nn.functional as F
from PIL import Image

from common import ATTRIBUTES, CLIP_MODEL, CLIP_PRETRAINED, MEAN, STD

ROLE = torch.tensor([a.group == "role" for a in ATTRIBUTES])


def device() -> torch.device:
    if torch.cuda.is_available():
        return torch.device("cuda")
    if torch.backends.mps.is_available():
        return torch.device("mps")
    return torch.device("cpu")


def load_clip(dev: torch.device):
    model, _, _ = open_clip.create_model_and_transforms(CLIP_MODEL, pretrained=CLIP_PRETRAINED)
    return model.eval().to(dev), open_clip.get_tokenizer(CLIP_MODEL)


def to_tensor(img: Image.Image) -> torch.Tensor:
    x = torch.from_numpy(np.asarray(img, dtype=np.float32) / 255.0).permute(2, 0, 1)
    return (x - torch.tensor(MEAN)[:, None, None]) / torch.tensor(STD)[:, None, None]


def degrade(img: Image.Image, rng: random.Random) -> Image.Image:
    s = rng.randint(32, 96)
    small = img.resize((s, s), Image.Resampling.BOX)
    return small.resize(img.size, Image.Resampling.BICUBIC)


@torch.no_grad()
def embed(model, crops: Path, samples: list[dict], dev, degraded: bool, seed: int = 0) -> torch.Tensor:
    rng = random.Random(seed)
    out = []
    for i in range(0, len(samples), 128):
        batch = []
        for s in samples[i : i + 128]:
            img = Image.open(crops / s["file"]).convert("RGB")
            batch.append(to_tensor(degrade(img, rng) if degraded else img))
        e = model.encode_image(torch.stack(batch).to(dev))
        out.append(F.normalize(e.float(), dim=-1).cpu())
        if i % 2560 == 0:
            print(f"    {i}/{len(samples)}", flush=True)
    return torch.cat(out)


def cached(model, data: Path, split: str, samples, dev, degraded: bool) -> torch.Tensor:
    path = data / f"embeddings-{split}{'-degraded' if degraded else ''}.pt"
    if path.exists():
        e = torch.load(path)
        if len(e) == len(samples):
            return e
    print(f"  embedding {split}{' (degraded)' if degraded else ''}: {len(samples)} crops", flush=True)
    e = embed(model, data / "crops", samples, dev, degraded)
    torch.save(e, path)
    return e


@torch.no_grad()
def zero_shot(model, tokenizer, dev) -> tuple[torch.Tensor, torch.Tensor]:
    """Initial weights and biases: CLIP's zero-shot classifier."""

    def text(prompts: list[str]) -> torch.Tensor:
        t = F.normalize(model.encode_text(tokenizer(prompts).to(dev)).float(), dim=-1).mean(0)
        return F.normalize(t, dim=0).cpu()

    scale = model.logit_scale.exp().item()
    person = text(["a photo of a person", "a photo of a man", "a photo of a woman"])
    w = torch.stack([text(a.prompts) - (person if a.group == "gear" else 0) for a in ATTRIBUTES]) * scale
    return w, torch.zeros(len(ATTRIBUTES))


def predict(w: torch.Tensor, b: torch.Tensor, e: torch.Tensor) -> torch.Tensor:
    """Scores in 0..1: softmax over the roles, sigmoid for gear."""
    z = e @ w.T + b
    p = torch.sigmoid(z)
    p[:, ROLE] = torch.softmax(z[:, ROLE], dim=1)
    return p


def loss_fn(w, b, e, y, wt) -> torch.Tensor:
    z = e @ w.T + b
    gear = ~ROLE
    known = (y >= 0).float()
    lg = F.binary_cross_entropy_with_logits(z[:, gear], y[:, gear].clamp(min=0).float(), reduction="none")
    lg = (lg * wt[:, gear] * known[:, gear]).sum() / (wt[:, gear] * known[:, gear]).sum().clamp(min=1)
    # Roles: one target per crop, the role labelled 1, weighted by its label's weight.
    has_role = (y[:, ROLE] == 1).any(1)
    target = y[:, ROLE].clamp(min=0).argmax(1)
    lr = F.cross_entropy(z[has_role][:, ROLE], target[has_role], reduction="none")
    rw = (wt[:, ROLE] * (y[:, ROLE] == 1)).sum(1)[has_role]
    lr = (lr * rw).sum() / rw.sum().clamp(min=1)
    return lg + lr


def thresholds(p: np.ndarray, y: np.ndarray, precision: float, recall: float) -> dict:
    keep = y >= 0
    p, y = p[keep], y[keep]
    pos = int(y.sum())
    if pos == 0 or pos == len(y):
        return {"present": 0.95, "absent": 0.05, "positives": pos, "negatives": len(y) - pos}
    grid = np.linspace(0.02, 0.98, 49)
    rows = []
    for t in grid:
        tp = int(((p >= t) & (y == 1)).sum())
        fp = int(((p >= t) & (y == 0)).sum())
        rows.append((t, tp / max(1, tp + fp), tp / pos))
    present = next((t for t, prec, _ in rows if prec >= precision and tp_ok(p, y, t)), 0.98)
    # Highest threshold under which at most (1 - recall) of the positives fall.
    absent = max((t for t in grid if (p[y == 1] < t).mean() <= 1 - recall), default=0.02)
    absent = min(absent, present)
    at = lambda t: {  # noqa: E731
        "precision": float(((p >= t) & (y == 1)).sum() / max(1, (p >= t).sum())),
        "recall": float(((p >= t) & (y == 1)).sum() / pos),
    }
    neg = y == 0
    return {
        "present": float(round(present, 2)),
        "absent": float(round(absent, 2)),
        "positives": pos,
        "negatives": int(neg.sum()),
        "at_present": at(present),
        # Share of negatives confidently recognised as such, and positives wrongly called absent.
        "absent_specificity": float((p[neg] < absent).mean()) if neg.any() else None,
        "absent_miss_rate": float((p[y == 1] < absent).mean()),
        "ap": float(average_precision(p, y)),
    }


def tp_ok(p, y, t) -> bool:
    return ((p >= t) & (y == 1)).sum() >= 3


def average_precision(p: np.ndarray, y: np.ndarray) -> float:
    order = np.argsort(-p)
    y = y[order]
    tp = np.cumsum(y)
    prec = tp / np.arange(1, len(y) + 1)
    return float((prec * y).sum() / max(1, y.sum()))


def evaluate(w, b, e, y) -> dict:
    p = predict(w, b, e).numpy()
    y = y.numpy()
    out = {}
    for i, a in enumerate(ATTRIBUTES):
        k = y[:, i] >= 0
        out[a.name] = {"ap": average_precision(p[k, i], y[k, i]) if k.any() and y[k, i].sum() else None}
    return out


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--data", type=Path, default=Path("data"))
    ap.add_argument("--out", type=Path, default=Path("runs/v1"))
    ap.add_argument("--epochs", type=int, default=60)
    ap.add_argument("--lr", type=float, default=1e-3)
    ap.add_argument("--decay", type=float, default=1e-4, help="pull towards the zero-shot weights")
    ap.add_argument("--precision", type=float, default=0.9)
    ap.add_argument("--recall", type=float, default=0.95)
    args = ap.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    torch.manual_seed(0)

    dev = device()
    model, tokenizer = load_clip(dev)
    splits = {s: json.loads((args.data / f"{s}.json").read_text())["samples"] for s in ("train", "validation", "test")}
    emb = {s: cached(model, args.data, s, splits[s], dev, False) for s in splits}
    emb_deg = {s: cached(model, args.data, s, splits[s], dev, True) for s in splits}
    lab = {s: torch.tensor([x["labels"] for x in splits[s]]) for s in splits}
    wts = {s: torch.tensor([x["weights"] for x in splits[s]]) for s in splits}

    w0, b0 = zero_shot(model, tokenizer, dev)
    w, b = w0.clone().requires_grad_(), b0.clone().requires_grad_()
    opt = torch.optim.Adam([w, b], lr=args.lr)
    e_tr = torch.cat([emb["train"], emb_deg["train"]])
    y_tr = torch.cat([lab["train"], lab["train"]])
    wt_tr = torch.cat([wts["train"], wts["train"]])
    log = []
    zs = evaluate(w0, b0, emb["validation"], lab["validation"])
    print("zero-shot validation AP: " + ", ".join(f"{k}={v['ap']:.2f}" for k, v in zs.items() if v["ap"] is not None))
    best, best_state = -1.0, None
    for epoch in range(1, args.epochs + 1):
        perm = torch.randperm(len(e_tr))
        for i in range(0, len(perm), 256):
            j = perm[i : i + 256]
            loss = loss_fn(w, b, e_tr[j], y_tr[j], wt_tr[j]) + args.decay * ((w - w0) ** 2).sum()
            opt.zero_grad()
            loss.backward()
            opt.step()
        with torch.no_grad():
            val = evaluate(w, b, torch.cat([emb["validation"], emb_deg["validation"]]),
                           torch.cat([lab["validation"], lab["validation"]]))
        aps = [v["ap"] for v in val.values() if v["ap"] is not None]
        mean_ap = float(np.mean(aps))
        log.append({"epoch": epoch, "loss": float(loss), "val_map": mean_ap, "val": val})
        if mean_ap > best:
            best, best_state = mean_ap, (w.detach().clone(), b.detach().clone())
        if epoch % 10 == 0:
            print(f"epoch {epoch}: loss {float(loss):.3f}, validation mAP {mean_ap:.3f}", flush=True)

    w, b = best_state
    # Thresholds on clean and degraded validation crops together.
    pv = predict(w, b, torch.cat([emb["validation"], emb_deg["validation"]])).numpy()
    yv = torch.cat([lab["validation"], lab["validation"]]).numpy()
    thr = {a.name: thresholds(pv[:, i], yv[:, i], args.precision, args.recall) for i, a in enumerate(ATTRIBUTES)}
    torch.save({"w": w, "b": b, "w0": w0, "thresholds": thr, "attributes": [a.name for a in ATTRIBUTES]},
               args.out / "head.pt")
    (args.out / "log.jsonl").write_text("\n".join(json.dumps(r) for r in log))
    (args.out / "thresholds.json").write_text(json.dumps(thr, indent=1))
    print(f"best validation mAP {best:.3f}; head saved to {args.out / 'head.pt'}")
    for name, t in thr.items():
        print(f"  {name:12s} present≥{t['present']:.2f} absent<{t['absent']:.2f} "
              f"(+{t['positives']}/-{t['negatives']}, AP {t.get('ap', 0):.2f})")


if __name__ == "__main__":
    main()
