"""Shared definitions of the person-attribute model pipeline.

The model classifies the *person crop* around a face found by YuNet: a
square centred on the face, four face-heights wide, starting a little above
the head, so that it shows the headwear, the face and the upper body
(uniform, shield, clothing). The geometry below is part of the model's contract:
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
    applies), `gear` for things a person wears on the head or face or
    carries, and `clothing` for upper-body garments (any number of gear and
    clothing items apply).
    """

    name: str
    label: str
    group: str
    prompts: list[str]
    # Wikimedia Commons categories whose pictures show this attribute.
    commons: list[str] = field(default_factory=list)
    # Accessory group of the Open Images crops of `models/accessories`.
    openimages: str | None = None
    # Open Images boxable classes whose boxes, on the neck and chest of a
    # face, show this clothing item (see `download_openimages.py`). Gear uses
    # the head-crop boxes of `models/accessories` instead (`openimages`).
    openimages_classes: list[str] = field(default_factory=list)
    # Open Images classes whose verified absence on a person shows that this
    # attribute is absent (default: `openimages_classes`). Open Images'
    # "Shirt" covers T-shirts and button-up shirts alike: no shirt on a
    # person means neither, but a shirt box does not tell which.
    openimages_negative_by: list[str] | None = None
    # Attributes whose (reliable) presence on a person shows that this one is
    # absent: someone in a suit is not wearing a hoodie. They provide the
    # reliable negatives of clothing that Open Images does not annotate.
    excluded_by: list[str] = field(default_factory=list)
    # Region of `REGIONS` where the application measures the colour of this
    # attribute, or None when its colour cannot be measured.
    region: str | None = None


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
        ["a photo of a person wearing a helmet", "a photo of a person wearing a riot helmet with a visor",
         "a photo of a person wearing a bicycle helmet", "a photo of a person wearing a hard hat"],
        ["Police helmets", "Riot helmets", "Combat helmets", "Men wearing helmets", "Women wearing helmets",
         "People wearing motorcycle helmets", "Men wearing hard hats", "Women wearing hard hats", "People with hard hats"],
        openimages="helmet",
        region="head",
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
        region="lower_face",
    ),
    Attribute(
        "face_mask",
        "Face mask",
        "gear",
        ["a photo of a person wearing a surgical face mask", "a photo of a person wearing a medical mask"],
        ["Face masks", "Face masks during the COVID-19 pandemic", "People wearing face masks during the COVID-19 pandemic"],
        region="lower_face",
    ),
    Attribute(
        "hat",
        "Hat or cap",
        "gear",
        ["a photo of a person wearing a hat", "a photo of a person wearing a cap"],
        ["Men wearing caps", "Men wearing hats", "Women wearing hats", "People wearing baseball caps"],
        openimages="hat",
        excluded_by=["helmet"],
        region="head",
    ),
    Attribute(
        "glasses",
        "Glasses",
        "gear",
        ["a photo of a person wearing glasses", "a photo of a person wearing eyeglasses"],
        ["People with glasses", "Men wearing eyeglasses", "Women wearing eyeglasses"],
        openimages="glasses",
        region="eyes",
    ),
    Attribute(
        "sunglasses",
        "Sunglasses",
        "gear",
        ["a photo of a person wearing sunglasses", "a photo of a person wearing dark sunglasses"],
        ["Women wearing sunglasses", "Men wearing sunglasses"],
        openimages="sunglasses",
        region="eyes",
    ),
    # Upper-body clothing: what the person crop shows below the face (it ends
    # about two face heights below the chin, so trousers, skirts and shoes
    # are out of reach).
    Attribute(
        "t_shirt",
        "T-shirt",
        "clothing",
        ["a photo of a person wearing a t-shirt", "a photo of a person in a short-sleeved t-shirt"],
        ["Men wearing T-shirts", "Women wearing T-shirts", "People wearing T-shirts"],
        openimages_negative_by=["Shirt"],
        excluded_by=["suit", "tie", "dress", "coat"],
        region="torso",
    ),
    Attribute(
        "shirt",
        "Shirt",
        "clothing",
        ["a photo of a person wearing a button-up shirt", "a photo of a person wearing a collared shirt"],
        ["Men wearing shirts with buttons", "Dress shirts", "Men wearing checkered shirts", "Men wearing striped shirts",
         "Women wearing shirts"],
        openimages_negative_by=["Shirt"],
        excluded_by=["t_shirt", "hoodie", "dress"],
        region="torso",
    ),
    Attribute(
        "suit",
        "Suit or blazer",
        "clothing",
        ["a photo of a person wearing a suit", "a photo of a person wearing a blazer", "a photo of a businessman in a suit"],
        ["Men wearing suits", "Women wearing suits", "Blazers"],
        openimages_classes=["Suit"],
        excluded_by=["t_shirt", "hoodie", "hi_vis_vest", "dress"],
        region="torso",
    ),
    Attribute(
        "jacket",
        "Jacket",
        "clothing",
        ["a photo of a person wearing a jacket", "a photo of a person wearing a leather jacket",
         "a photo of a person wearing a zip-up jacket"],
        ["Men wearing jackets", "Women wearing jackets", "Leather jackets", "Denim jackets", "Puffer jackets"],
        openimages_classes=["Jacket"],
        region="torso",
    ),
    Attribute(
        "coat",
        "Coat",
        "clothing",
        ["a photo of a person wearing a coat", "a photo of a person wearing a long winter coat"],
        ["Men wearing coats", "Women wearing coats"],
        openimages_classes=["Coat"],
        excluded_by=["t_shirt"],
        region="torso",
    ),
    Attribute(
        "hoodie",
        "Hoodie",
        "clothing",
        ["a photo of a person wearing a hoodie", "a photo of a person wearing a hooded sweatshirt"],
        ["Men wearing hoodies", "Women wearing hoodies", "People wearing hoodies"],
        excluded_by=["suit", "tie", "dress"],
        region="torso",
    ),
    Attribute(
        "sweater",
        "Sweater",
        "clothing",
        ["a photo of a person wearing a knitted sweater", "a photo of a person wearing a jumper or cardigan"],
        ["Men wearing sweaters", "Women wearing sweaters", "Cardigans"],
        excluded_by=["t_shirt"],
        region="torso",
    ),
    Attribute(
        "dress",
        "Dress",
        "clothing",
        ["a photo of a woman wearing a dress", "a photo of a person wearing a dress"],
        ["Women wearing dresses"],
        openimages_classes=["Dress"],
        excluded_by=["suit", "tie", "t_shirt", "hoodie"],
        region="torso",
    ),
    Attribute(
        "hi_vis_vest",
        "Hi-vis vest",
        "clothing",
        ["a photo of a person wearing a high-visibility vest", "a photo of a worker in a fluorescent safety vest"],
        ["High-visibility vests", "High-visibility work clothing"],
        excluded_by=["suit", "dress", "tie"],
        region="torso",
    ),
    Attribute(
        "tie",
        "Tie",
        "clothing",
        ["a photo of a person wearing a necktie", "a photo of a man wearing a tie"],
        ["Men wearing neckties"],
        openimages_classes=["Tie"],
        excluded_by=["t_shirt", "hoodie", "dress"],
        region="neck",
    ),
    Attribute(
        "scarf",
        "Scarf",
        "clothing",
        ["a photo of a person wearing a scarf", "a photo of a person with a scarf around the neck"],
        ["Men wearing scarves", "Women wearing scarves"],
        openimages_classes=["Scarf"],
        region="neck",
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


# Regions where the application measures colours, relative to the face box
# (x, y, w, h): (x0, y0, x1, y1) in face heights, x from the centre of the
# face, y from its top. They are stored in the exported model; the colour of
# an attribute is only read for people who have it.
REGIONS: dict[str, tuple[float, float, float, float]] = {
    # Above the forehead: the crown of a hat or a helmet.
    "head": (-0.45, -0.5, 0.45, -0.03),
    # Eye line: lenses and frames.
    "eyes": (-0.4, 0.28, 0.4, 0.48),
    # Mouth and chin: masks and balaclavas.
    "lower_face": (-0.3, 0.62, 0.3, 0.95),
    # Below the chin: tie, scarf, collar.
    "neck": (-0.25, 1.1, 0.25, 1.8),
    # Chest: the outermost upper-body garment.
    "torso": (-0.7, 1.45, 0.7, 2.6),
}

# Groups scored with a sigmoid (any number apply); roles use a softmax.
MULTI_LABEL = ("gear", "clothing")


def openimages_negative_by(a: Attribute) -> list[str]:
    return a.openimages_classes if a.openimages_negative_by is None else a.openimages_negative_by


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


def region_geometry() -> dict:
    """The colour regions, stored in the exported model."""
    return {name: list(box) for name, box in REGIONS.items()}


def data_dir(root: Path, source: str) -> Path:
    return root / source
