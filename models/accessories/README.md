# Accessory model (work in progress)

A small detector that finds **hats, helmets, glasses and sunglasses** on the
head of each detected face. MediaAnonymizer will use it to *suggest* faces
(for example "faces wearing a blue helmet") that you then confirm one by one
before they are left visible. It never decides on its own: a wrong guess
must not expose anyone (see [docs/PRIVACY.md](../../docs/PRIVACY.md#faces-left-visible)).

This folder contains everything needed to build it from public data. The
trained model is not part of the repository yet.

## What the model does

* **Input**: a *head crop* around a face found by YuNet (the face box
  extended by 50 % of its width left and right, 90 % of its height above and
  20 % below), resized to 320×320, RGB in 0–1. The geometry is defined in
  [`common.py`](common.py) and stored in the exported model, so the app
  builds exactly the same crop.
* **Output**: boxes, scores and labels of the accessories in the crop.
* **Not learnt**: colours. The app measures the dominant colour inside each
  accessory box afterwards, which is simpler and more reliable than teaching
  a model colour names.

| Group | Open Images classes |
|---|---|
| `hat` | Hat, Cowboy hat, Sun hat |
| `helmet` | Helmet, Bicycle helmet, Football helmet |
| `glasses` | Glasses, Goggles |
| `sunglasses` | Sunglasses |

Groups are defined in `common.GROUPS`. To add one (Open Images also boxes
Scarf, Crown, Tiara…), edit that list and run every step again.

## Requirements

* Python 3.10+ and the packages of [`requirements.txt`](requirements.txt).
* An NVIDIA GPU is strongly recommended (any recent card with 8 GB; Apple
  silicon works through MPS, more slowly). A CPU works for testing only.
* Disk: the default dataset (about 30 000 training images plus validation
  and test) takes roughly 10–30 GB, depending on image sizes; reduce
  `--per-group` / `--negatives` for a smaller run.
* Internet access to `storage.googleapis.com` (annotations),
  `open-images-dataset.s3.amazonaws.com` (images) and `download.pytorch.org`
  (ImageNet weights of the backbone).

## Steps

```sh
cd models/accessories
python -m venv .venv && . .venv/bin/activate
pip install -r requirements.txt

# 0. Check that everything runs (synthetic data, CPU, about 4 minutes, no download)
python smoke_test.py

# 1. Annotations + images from Open Images (resumable; the train
#    annotation file is 2.3 GB and is streamed, so this step takes a while)
python download.py --data data

# 2. Head crops + their accessory boxes
python prepare.py --data data

# 3. Training (best checkpoint by validation mAP is kept)
python train.py --data data --out runs/v1

# 4. Measurements on the test split, never seen during training
python evaluate.py --data data --checkpoint runs/v1/best.pt --split test \
    --json runs/v1/test.json --report runs/v1/METRICS.md

# 5. ONNX export (thresholds and metrics embedded) and check of the exported file
python export.py --checkpoint runs/v1/best.pt --metrics runs/v1/test.json --out accessories.onnx
python evaluate.py --data data --onnx accessories.onnx --split test
```

Each script has `--help`. Useful options: `train.py --epochs`, `--batch`,
`--lr`; `evaluate.py --target-precision` (default 0.9: the suggested score
threshold per accessory is the lowest one reaching that precision).

The result to hand over for integration is `accessories.onnx` together with
`runs/v1/METRICS.md` and `runs/v1/log.jsonl`.

## How the dataset is built

1. `download.py` streams the official box annotations and keeps faces and
   accessory boxes, plus the human-verified *absences* of the accessory
   classes. It selects, per group, images that show a face and that
   accessory, and images that show a face where every group was verified
   absent (reliable negatives), then downloads them.
2. `prepare.py` cuts the head crop of every usable face (not a group of
   faces, not a drawing, at least 24 px) and keeps the accessory boxes that
   lie at least 60 % inside it.

## Known limitations

* **Label noise.** Open Images boxes every instance of a class only in
  images where that class was verified present. A crop chosen for its hat
  may contain unlabelled glasses, which teaches the model to miss some.
* **Domain.** Open Images are mostly photographs; small, blurred or
  side-on faces in video will be harder. The test split gives a measure on
  photos only; check videos by hand.
* **Bias.** The data's distribution of people, places and headwear styles is
  not controlled. Per-group recall should be checked on your own material
  before relying on the suggestions.
* None of this matters for safety as long as the model only *suggests*:
  every face stays covered until you confirm it.

## Licences and attribution

* Code in this folder: MIT OR Apache-2.0, like the rest of the project.
* Model architecture: torchvision SSDlite / MobileNetV3 (BSD-3-Clause),
  initialised from torchvision's ImageNet weights.
* Training data: [Open Images V7](https://storage.googleapis.com/openimages/web/index.html),
  annotations by Google LLC under CC BY 4.0; images listed as CC BY 2.0 by
  their individual authors. Images are downloaded for training only and are
  not redistributed; the exported model carries this attribution in its
  metadata.
* Ultralytics YOLO is deliberately not used: its AGPL licence would extend to
  the application.
