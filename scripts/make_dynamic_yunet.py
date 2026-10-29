#!/usr/bin/env python3
"""Turn the upstream YuNet model (fixed 640x640 input) into a dynamic-shape model.

YuNet is fully convolutional, so the only thing preventing arbitrary input
sizes is the static shape declared on the graph input/outputs. Relaxing it lets
MediaAnonymizer run detection at the native aspect ratio of each frame instead
of letterboxing everything into 640x640.

Usage:
    pip install onnx
    python3 scripts/make_dynamic_yunet.py face_detection_yunet_2023mar.onnx models/yunet_2023mar_dynamic.onnx
"""
import sys

import onnx


def main(src: str, dst: str) -> None:
    model = onnx.load(src)
    dims = model.graph.input[0].type.tensor_type.shape.dim
    dims[2].dim_param = "height"
    dims[3].dim_param = "width"
    for out in model.graph.output:
        out.type.tensor_type.shape.dim[1].dim_param = "anchors"
    onnx.checker.check_model(model)
    onnx.save(model, dst)


if __name__ == "__main__":
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    main(sys.argv[1], sys.argv[2])
