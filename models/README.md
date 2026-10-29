# Bundled models

## `yunet_2023mar_dynamic.onnx`

[YuNet](https://github.com/ShiqiYu/libfacedetection) face detector, 2023
release (`face_detection_yunet_2023mar.onnx` from the
[OpenCV model zoo](https://github.com/opencv/opencv_zoo/tree/main/models/face_detection_yunet)),
by Shiqi Yu et al. Licensed under the MIT License, see [LICENSE-YuNet](LICENSE-YuNet).

Modification: the input height/width and the anchor dimension of the outputs
were changed from fixed (640×640) to symbolic so that frames can be processed
at their native aspect ratio. Weights and graph are otherwise unchanged. The
transformation is reproducible with
[`scripts/make_dynamic_yunet.py`](../scripts/make_dynamic_yunet.py).

| | |
|---|---|
| Upstream SHA-256 | `8f2383e4dd3cfbb4553ea8718107fc0423210dc964f9f4280604804ed2552fa4` |
| Bundled SHA-256 | `c52098bae6f202cfdd09f637005d86c017c0a9f60ca3fcbc914f7af6fc640ad5` |
| Input | `input`: float32 `[1, 3, H, W]`, BGR, 0–255, H and W multiples of 32 |
| Outputs | `cls_{8,16,32}`, `obj_{8,16,32}`, `bbox_{8,16,32}`, `kps_{8,16,32}` |

The model is embedded in the binary at compile time. A different
YuNet-compatible model can be supplied at runtime with `--model`.

> Reference: W. Wu, H. Peng, S. Yu, *YuNet: A Tiny Millisecond-level Face
> Detector*, Machine Intelligence Research, 2023.

## `accessories/` (in preparation)

Training pipeline for a small detector of hats, helmets, glasses and
sunglasses on each face, built from Open Images (CC BY 4.0). It will power
an assisted "select faces by accessory" helper whose suggestions you confirm
one by one. The model is not trained or bundled yet; see
[accessories/README.md](accessories/README.md).
