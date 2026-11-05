"""Shared definitions of the person-attribute model pipeline.

The model classifies the *person crop* around a face found by YuNet: a
square centred on the face, four face-heights wide, starting a little above
the head, so that it shows the headwear, the face and the upper body
(uniform, shield). The geometry below is part of the model's contract:
MediaAnonymizer builds the same crop around every face, so any change here
requires retraining. It is stored in the exported model's metadata.
"""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path

from PIL import Image

# Backbone: OpenCLIP ViT-B/32 trained on LAION-2B (MIT licence). Its image
# tower is frozen; only a linear head per attribute is trained.
CLIP_MODEL = "ViT-B-32"
CLIP_PRETRAINED = "laion2b_s34b_b79k"
INPUT_SIZE = 224
MEAN = (0.48145466, 0.4578275, 0.40821073)
STD = (0.26862954, 0.26130258, 0.27577711)
# Fill colour of the parts of a crop outside the picture (the mean colour).
FILL = tuple(round(m * 255) for m in MEAN)

# Person crop around a face box (x, y, w, h), as multiples of the face height.
CROP_SIDE = 4.0
CROP_ABOVE = 0.75


@dataclass
class Attribute:
    """One attribute predicted by the model.

    `group` is `role` for the mutually exclusive kinds of person (exactly one
    applies) and `gear` for things a person wears or carries (any number).
    """

    name: str
    label: str
    group: str
    prompts: list[str]
    # Wikimedia Commons categories whose pictures show this attribute.
    commons: list[str] = field(default_factory=list)
    # Accessory group of the Open Images crops of `models/accessories`.
    openimages: str | None = None


ATTRIBUTES: list[Attribute] = [
    Attribute(
        "civilian",
        "Civilian",
        "role",
        [
            "a photo of a person in everyday clothes",
            "a photo of a protester",
            "a photo of a pedestrian in the street",
            "a photo of a civilian",
        ],
        ["Pedestrians", "Demonstrations and protests", "Street photography", "Portrait photographs of men",
         "Portrait photographs of women", "People in the street"],
    ),
    Attribute(
        "police",
        "Police",
        "role",
        ["a photo of a police officer", "a photo of a police officer in uniform", "a photo of riot police"],
        ["Police officers", "Police officers of Italy", "Polizia di Stato", "Police officers of France",
         "Police officers of Germany", "Police officers of the United Kingdom", "Riot control", "Police at protests"],
    ),
    Attribute(
        "carabinieri",
        "Carabinieri",
        "role",
        ["a photo of an Italian carabiniere", "a photo of a carabiniere in uniform with a red stripe"],
        ["Carabinieri", "Arma dei Carabinieri", "Carabinieri uniforms", "Carabinieri in Rome"],
    ),
    Attribute(
        "military",
        "Military",
        "role",
        ["a photo of a soldier", "a photo of a soldier in camouflage uniform", "a photo of military personnel"],
        ["Soldiers", "Soldiers of Italy", "Esercito Italiano", "Soldiers of the United States", "Military personnel of France",
         "Soldiers of Germany"],
    ),
    Attribute(
        "helmet",
        "Helmet",
        "gear",
        ["a photo of a person wearing a helmet", "a photo of a person wearing a riot helmet with a visor"],
        ["Police helmets", "Riot helmets", "Combat helmets"],
        openimages="helmet",
    ),
    Attribute(
        "shield",
        "Shield",
        "gear",
        ["a photo of a person holding a riot shield", "a photo of police with shields"],
        ["Riot shields"],
    ),
    Attribute(
        "balaclava",
        "Balaclava",
        "gear",
        ["a photo of a person wearing a balaclava", "a photo of a masked person with only the eyes visible"],
        ["Balaclavas", "Black bloc"],
    ),
    Attribute(
        "face_mask",
        "Face mask",
        "gear",
        ["a photo of a person wearing a surgical face mask", "a photo of a person wearing a medical mask"],
        ["Face masks", "Face masks during the COVID-19 pandemic", "People wearing face masks during the COVID-19 pandemic"],
    ),
    Attribute(
        "hat",
        "Hat or cap",
        "gear",
        ["a photo of a person wearing a hat", "a photo of a person wearing a cap"],
        ["Men wearing caps", "Men wearing hats", "Women wearing hats"],
        openimages="hat",
    ),
    Attribute(
        "glasses",
        "Glasses",
        "gear",
        ["a photo of a person wearing glasses", "a photo of a person wearing eyeglasses"],
        ["People with glasses", "Men wearing eyeglasses", "Women wearing eyeglasses"],
        openimages="glasses",
    ),
    Attribute(
        "sunglasses",
        "Sunglasses",
        "gear",
        ["a photo of a person wearing sunglasses", "a photo of a person wearing dark sunglasses"],
        ["Women wearing sunglasses", "Men wearing sunglasses"],
        openimages="sunglasses",
    ),
]

# Commons subcategories whose names contain one of these words rarely show
# people (vehicles, buildings, insignia...): they are not explored.
SKIP_SUBCATEGORY = [
    "vehicle", "car ", "cars", "boat", "helicopter", "aircraft", "station", "building", "barrack", "headquarter",
    "logo", "insignia", "badge", "emblem", "flag", "coat of arms", "medal", "stamp", "museum", "monument",
    "memorial", "grave", "cemetery", "map", "drawing", "painting", "illustration", "cartoon", "toy", "figurine",
    "statue", "sculpture", "postcard", "document", "video", "audio", "horse", "dog", "band", "music", "parade ground",
    "weapon", "gun", "rifle", "tank", "ship", "motorcycle", "bicycle", "equipment", "uniform buttons",
]


def names() -> list[str]:
    return [a.name for a in ATTRIBUTES]


def person_crop(img: Image.Image, fx: float, fy: float, fw: float, fh: float) -> Image.Image:
    """The person crop of a face box, resized to `INPUT_SIZE`; parts outside
    the picture are filled with `FILL`."""
    side = CROP_SIDE * fh
    x0 = fx + fw / 2 - side / 2
    y0 = fy - CROP_ABOVE * fh
    box = (round(x0), round(y0), round(x0 + side), round(y0 + side))
    W, H = img.size
    inner = (max(0, box[0]), max(0, box[1]), min(W, box[2]), min(H, box[3]))
    canvas = Image.new("RGB", (box[2] - box[0], box[3] - box[1]), FILL)
    if inner[2] > inner[0] and inner[3] > inner[1]:
        canvas.paste(img.crop(inner), (inner[0] - box[0], inner[1] - box[1]))
    return canvas.resize((INPUT_SIZE, INPUT_SIZE), Image.Resampling.BICUBIC)


def crop_geometry() -> dict:
    """The crop contract, stored in the exported model."""
    return {
        "side": CROP_SIDE,
        "above": CROP_ABOVE,
        "input_size": INPUT_SIZE,
        "mean": list(MEAN),
        "std": list(STD),
        "fill": list(FILL),
        "input": "RGB float32 NCHW, (pixel/255 - mean) / std, square crop resized to input_size (bicubic)",
    }


def data_dir(root: Path, source: str) -> Path:
    return root / source
