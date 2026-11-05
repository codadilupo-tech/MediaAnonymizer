# Person attribute model

A classifier that tells, for each face found by YuNet, **what kind of
person** it belongs to (civilian, police, carabinieri, military) and **what
they wear or carry** (helmet, shield, balaclava, face mask, hat or cap,
glasses, sunglasses). It powers the *people filter* of the desktop app and
the `--cover-only` / `--cover-except` options of the CLI: "cover only the
civilians", "cover everyone except police and carabinieri", "cover only
people wearing a balaclava"…

The model is trained locally from public pictures and is **not bundled**:
it is about 350 MB, and you should check its measurements before relying on
it. The application looks for `attributes.onnx` next to its executable (or
in this folder for development builds), or uses the file you choose.

## How it works

* **Input**: the *person crop* of a face: a square four face-heights wide,
  centred on the face, starting three quarters of a face above it, so it
  shows the headwear, the face and the upper body (uniform, shield). Parts
  outside the picture are filled with a neutral grey. The geometry is
  defined in [`common.py`](common.py) and stored in the exported model.
* **Backbone**: the image tower of [OpenCLIP](https://github.com/mlfoundations/open_clip)
  ViT-B/32 trained on LAION-2B, frozen. CLIP already knows what a police
  officer or a riot shield looks like; training only adjusts that knowledge
  to these crops.
* **Heads**: one linear layer, started from CLIP's zero-shot classifier
  (the text prompts of `common.ATTRIBUTES`) and trained with weight decay
  *towards* it, so attributes with few or noisy examples stay close to
  CLIP's judgement. Roles are exclusive (softmax); gear items are
  independent (sigmoid).
* **Two thresholds per attribute**, chosen on validation data: `present`
  (precision ≥ 90 %) and `absent` (at most 5 % of the people who have the
  attribute score lower). In between, the model is *unsure*, and the
  application covers unsure people whatever the rule says.

| Attribute | Group | Reference pictures |
|---|---|---|
| civilian | role | Commons: pedestrians, demonstrations, street photography, portraits; Open Images faces (weak label) |
| police | role | Commons: police officers (Italy, France, Germany, UK…), Polizia di Stato, riot control |
| carabinieri | role | Commons: Carabinieri, Arma dei Carabinieri |
| military | role | Commons: soldiers (Italy, US, France, Germany), Esercito Italiano |
| helmet | gear | Commons: police helmets; Open Images helmet boxes |
| shield | gear | Commons: riot shields |
| balaclava | gear | Commons: balaclavas, black bloc |
| face_mask | gear | Commons: face masks (COVID-19) |
| hat | gear | Commons: men / women wearing hats or caps; Open Images hat boxes |
| glasses, sunglasses | gear | Commons: people wearing (sun)glasses; Open Images boxes |

To add an attribute, add it to `common.ATTRIBUTES` (prompts and Commons
categories) and run every step again; the application reads the attribute
list from the model.

## Steps

```sh
cd models/attributes
python -m venv .venv && . .venv/bin/activate
pip install -r requirements.txt

# 1. Reference pictures from Wikimedia Commons (thumbnails, resumable, polite rate)
python download_commons.py --data data

# 2. Person crops (YuNet) and labels; reuses the Open Images data of
#    ../accessories/data if you downloaded it (optional)
python prepare.py --data data

# 3. CLIP embeddings (cached) and heads; prints validation thresholds
python train.py --data data --out runs/v1

# 4. ONNX export with metadata, checked against PyTorch and measured on the test split
python export.py --data data --head runs/v1/head.pt --out attributes.onnx
```

`export.py` writes `attributes.metrics.json` next to the model: per
attribute, average precision, precision and recall at the `present`
threshold, and how many people who have it fall below `absent`. Read it
before using the model. `--int8` produces a 4× smaller model (weights
quantized to 8 bits); check its metrics too.

A run on an Apple M4 Pro takes about 30 minutes for the download, 5 for
preparation, 10 for the embeddings and seconds for the heads.

## Known limitations

* **Label noise.** Commons categories describe a picture, not each person
  in it: a picture of carabinieri at a demonstration also shows civilians.
  Only the (up to three) largest faces of a picture are used, but some
  labels are still wrong. Gear not listed in a picture's categories is
  treated as a *weak* negative.
* **Domain.** Reference pictures are mostly photographs of good quality;
  crops are also degraded to 32–96 px during training to imitate video, but
  small, blurred or back-turned people remain hard. Measure on your own
  material.
* **Look-alikes.** Uniforms differ by country and unit, and private
  security, firefighters or re-enactors can look like police or soldiers.
  The model recognises *appearance*, not *status*.
* **Bias.** The distribution of people, places and uniforms in the
  reference pictures is not controlled.

The application therefore only ever *uncovers* people the model is
confident about, writes its decisions as ordinary visible-face marks that
you review, and covers every face it did not see during the scan. See
[docs/PRIVACY.md](../../docs/PRIVACY.md#people-filter).

## Licences and attribution

* Code: MIT OR Apache-2.0, like the rest of the project.
* Backbone: OpenCLIP ViT-B/32 `laion2b_s34b_b79k`, MIT licence.
* Training pictures: Wikimedia Commons, each under its own free licence
  (author, licence and source page recorded in `data/commons/manifest.json`);
  Open Images V7, annotations CC BY 4.0 by Google LLC, images CC BY 2.0.
  Pictures are used for training only and are not redistributed; the
  exported model carries this attribution in its metadata.
