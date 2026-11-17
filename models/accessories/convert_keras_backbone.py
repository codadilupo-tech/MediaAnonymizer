"""Converts Keras' ImageNet MobileNetV3-Large weights to torchvision's layout.

The backbone normally starts from torchvision's ImageNet weights, downloaded
from download.pytorch.org. Where that host is unreachable, the same
architecture's ImageNet weights published with Keras Applications (Apache
2.0, on storage.googleapis.com) can be used instead:

    curl -O https://storage.googleapis.com/tensorflow/keras-applications/mobilenet_v3/weights_mobilenet_v3_large_224_1.0_float_no_top_v2.h5
    python convert_keras_backbone.py weights_mobilenet_v3_large_224_1.0_float_no_top_v2.h5 backbone.pt [--verify]
    python train.py --backbone backbone.pt ...

Both implement the paper's MobileNetV3-Large and expect inputs in [-1, 1]
(what SSDlite's normalisation produces), so the conversion is a renaming and
transposition of tensors. `--verify` (needs TensorFlow) checks that the
converted network computes the same features as Keras on random inputs.
The two frameworks pad stride-2 convolutions differently on even input sizes
(by one pixel); the check uses an input size where they agree, and the
difference is otherwise absorbed by fine-tuning.
"""

from __future__ import annotations

import argparse
from functools import partial
from pathlib import Path

import h5py
import numpy as np
import torch
from torch import nn
from torchvision.models import mobilenet_v3_large

# Blocks of torchvision's `features[1..15]` with a squeeze-and-excitation layer.
SE_BLOCKS = {4, 5, 6, 11, 12, 13, 14, 15}


def keras_tensors(path: Path) -> dict[str, np.ndarray]:
    """Weights by short name, e.g. `expanded_conv_3/expand/kernel`."""
    out: dict[str, np.ndarray] = {}
    with h5py.File(path, "r") as f:

        def visit(name: str, obj) -> None:
            if isinstance(obj, h5py.Dataset):
                parts = name.split("/")
                # h5 names repeat the layer path: "<layer>/<layer>/<var>:0".
                layer = "/".join(parts[: len(parts) // 2])
                out[f"{layer}/{parts[-1].split(':')[0]}"] = np.asarray(obj)

        f.visititems(visit)
    return out


def conv(k: np.ndarray, depthwise: bool = False) -> torch.Tensor:
    # Keras: (kh, kw, in, out) or depthwise (kh, kw, channels, 1);
    # torch: (out, in, kh, kw) or depthwise (channels, 1, kh, kw).
    return torch.from_numpy(k.transpose(2, 3, 0, 1) if depthwise else k.transpose(3, 2, 0, 1)).contiguous()


def convert(path: Path) -> dict[str, torch.Tensor]:
    k = keras_tensors(path)
    used: set[str] = set()

    def get(name: str) -> np.ndarray:
        used.add(name)
        return k[name]

    sd: dict[str, torch.Tensor] = {}

    def bn(prefix: str, layer: str) -> None:
        sd[f"{prefix}.weight"] = torch.from_numpy(get(f"{layer}/BatchNorm/gamma"))
        sd[f"{prefix}.bias"] = torch.from_numpy(get(f"{layer}/BatchNorm/beta"))
        sd[f"{prefix}.running_mean"] = torch.from_numpy(get(f"{layer}/BatchNorm/moving_mean"))
        sd[f"{prefix}.running_var"] = torch.from_numpy(get(f"{layer}/BatchNorm/moving_variance"))
        sd[f"{prefix}.num_batches_tracked"] = torch.tensor(0)

    def conv_bn(prefix: str, layer: str, depthwise: bool = False) -> None:
        kernel = f"{layer}/depthwise_kernel" if depthwise else f"{layer}/kernel"
        sd[f"{prefix}.0.weight"] = conv(get(kernel), depthwise)
        bn(f"{prefix}.1", layer)

    conv_bn("features.0", "Conv")
    for i in range(1, 16):
        name = "expanded_conv" if i == 1 else f"expanded_conv_{i - 1}"
        b = f"features.{i}.block"
        j = 0
        if i > 1:
            conv_bn(f"{b}.0", f"{name}/expand")
            j = 1
        conv_bn(f"{b}.{j}", f"{name}/depthwise", depthwise=True)
        j += 1
        if i in SE_BLOCKS:
            for fc, layer in (("fc1", "Conv"), ("fc2", "Conv_1")):
                sd[f"{b}.{j}.{fc}.weight"] = conv(get(f"{name}/squeeze_excite/{layer}/kernel"))
                sd[f"{b}.{j}.{fc}.bias"] = torch.from_numpy(get(f"{name}/squeeze_excite/{layer}/bias"))
            j += 1
        conv_bn(f"{b}.{j}", f"{name}/project")
    conv_bn("features.16", "Conv_1")

    unused = set(k) - used
    if unused:
        raise SystemExit(f"unmapped Keras tensors: {sorted(unused)[:5]}…")
    return sd


def torch_backbone(state: dict[str, torch.Tensor]) -> nn.Module:
    norm = partial(nn.BatchNorm2d, eps=0.001, momentum=0.03)
    model = mobilenet_v3_large(weights=None, norm_layer=norm, reduced_tail=False)
    missing, unexpected = model.load_state_dict(state, strict=False)
    # The classifier is not part of the "no top" weights.
    if unexpected or any(not m.startswith("classifier.") for m in missing):
        raise SystemExit(f"state dict mismatch: missing {missing}, unexpected {unexpected}")
    return model.eval()


def verify(h5: Path, state: dict[str, torch.Tensor]) -> float:
    import tensorflow as tf

    size = 225  # odd at every stride-2 layer: both frameworks pad symmetrically
    keras = tf.keras.applications.MobileNetV3Large(
        input_shape=(size, size, 3), include_top=False, weights=None, include_preprocessing=False
    )
    keras.load_weights(str(h5))
    model = torch_backbone(state)
    rng = np.random.default_rng(0)
    worst = 0.0
    for _ in range(3):
        x = rng.uniform(-1, 1, (1, size, size, 3)).astype(np.float32)
        a = keras(x, training=False).numpy()
        with torch.no_grad():
            b = model.features(torch.from_numpy(x.transpose(0, 3, 1, 2))).numpy().transpose(0, 2, 3, 1)
        worst = max(worst, float(np.abs(a - b).max() / (np.abs(a).max() + 1e-9)))
    return worst


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("h5", type=Path, help="Keras no-top weights (.h5)")
    ap.add_argument("out", type=Path, help="torchvision state dict (.pt)")
    ap.add_argument("--verify", action="store_true", help="compare with Keras (needs TensorFlow)")
    args = ap.parse_args()

    state = convert(args.h5)
    torch_backbone(state)  # checks every tensor has its place
    if args.verify:
        diff = verify(args.h5, state)
        print(f"max relative difference with Keras: {diff:.2e}")
        if diff > 1e-3:
            raise SystemExit("conversion does not reproduce Keras")
    torch.save({k: v for k, v in state.items()}, args.out)
    print(f"{args.out}: {len(state)} tensors")


if __name__ == "__main__":
    main()
