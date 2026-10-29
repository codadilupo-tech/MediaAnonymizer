# Hardware acceleration

MediaAnonymizer has two independent accelerated stages:

| Stage | Engine | Selected with |
|---|---|---|
| Face detection | [ONNX Runtime](https://onnxruntime.ai) execution providers | `--device`, `--gpu` |
| Video decoding / encoding | FFmpeg hardware back-ends | `--hwaccel`, `--encoder`, `--encoder-name`, `--gpu`, `--vaapi-device` |

They can be mixed freely, e.g. decode with VA-API, detect with OpenVINO and
encode on the CPU. `mediaanon devices` shows what the current build and
machine support.

```text
$ mediaanon devices
Face detection (ONNX Runtime):
  cpu        available
  cuda       available
  tensorrt   unavailable (build with --features tensorrt and a matching ONNX Runtime)
  ...
Video (FFmpeg):
  version    6.1.1
  hwaccel    vdpau, cuda, vaapi, qsv, drm, opencl, vulkan
  encoders   libx264, libx264rgb, h264_nvenc, h264_qsv, h264_vaapi, libx265, hevc_nvenc, ...
```

## Face detection devices

| `--device` | Hardware | Cargo feature | ONNX Runtime |
|---|---|---|---|
| `cpu` | any CPU | — | default build |
| `cuda` | NVIDIA GPU (CUDA 12 + cuDNN 9) | `cuda` | CUDA build |
| `tensorrt` | NVIDIA GPU with TensorRT | `tensorrt` | CUDA/TensorRT build |
| `directml` | any DirectX 12 GPU on Windows (NVIDIA, AMD, Intel) | `directml` | DirectML build |
| `coreml` | Apple Neural Engine / GPU | `coreml` | macOS build |
| `openvino` | Intel CPU, iGPU, Arc, NPU | `openvino` | OpenVINO build |
| `rocm` | AMD GPU (Linux) | `rocm` | ROCm build |
| `auto` *(default)* | best of TensorRT, CUDA, DirectML, ROCm, OpenVINO compiled in, else CPU; falls back to the CPU if that provider fails to initialise | | |

Core ML is never chosen by `auto`: on some macOS versions it rejects the
detector's variable input sizes at run time. When selected explicitly and it
fails, the detector logs one ONNX Runtime error per session, switches to the
CPU and continues.

`--gpu N` selects the GPU on multi-GPU machines (CUDA, TensorRT, DirectML,
ROCm).

An execution provider is usable only if it is **compiled into MediaAnonymizer**
(Cargo feature) **and present in the ONNX Runtime library** that is loaded.
Asking explicitly for an unavailable device is an error; `auto` silently falls
back to the CPU.

### Building with a GPU provider

The default build (`ort-download` feature) downloads a prebuilt ONNX Runtime
at build time and copies it next to the binaries. Prebuilt binaries exist for
the common providers; enabling the matching feature downloads the right
variant:

```sh
cargo build --release --features cuda        # Linux/Windows x64, needs CUDA 12 + cuDNN 9 at runtime
cargo build --release --features tensorrt    # also needs TensorRT at runtime
cargo build --release --features directml    # Windows
cargo build --release --features coreml      # macOS
# GUI: same features with -p mediaanon-gui
```

### Bring your own ONNX Runtime

For providers without a prebuilt package (OpenVINO, ROCm, custom builds) or to
share a system-wide installation, build with dynamic loading:

```sh
cargo build --release --no-default-features --features ort-dynamic,openvino
export ORT_DYLIB_PATH=/opt/onnxruntime/lib/libonnxruntime.so   # .dll / .dylib on Windows / macOS
mediaanon devices
```

With `ort-dynamic` the library is searched in `ORT_DYLIB_PATH`, then next to
the executable, then on the system library path. ONNX Runtime **1.22 or
newer** is required (the `api-22` feature of `ort` in the workspace
`Cargo.toml`).

Python wheels are a convenient source of libraries: e.g. `pip download
onnxruntime-openvino` contains `onnxruntime/capi/libonnxruntime.so.*`.

### Performance tips

* The model is tiny (230 KB); on CPU, throughput comes from running several
  frames at once. MediaAnonymizer creates one ONNX session per two cores
  automatically; `--threads` caps the total.
* `--detection-size` has the largest impact on detection cost (quadratic). The
  defaults (1920 px photos, 960 px videos) find faces down to ~1–2 % of the
  frame width.
* `--detect-every 2` or `3` halves/thirds detection cost for videos; keep
  `--persistence` ≥ `detect-every - 1` (enforced).

## Video decoding

`--hwaccel` maps to FFmpeg's `-hwaccel` option. Decoded frames are copied back
to system memory, where faces are painted, so every back-end works with every
encoder.

| `--hwaccel` | Platform | Notes |
|---|---|---|
| `none` *(default)* | all | Software decoding, multi-threaded. Often fast enough. |
| `auto` | all | Let FFmpeg pick; falls back to software. |
| `cuda` | NVIDIA | NVDEC; `--gpu` selects the device. |
| `vaapi` | Linux, Intel/AMD | `--vaapi-device` (default `/dev/dri/renderD128`). |
| `qsv` | Intel | Quick Sync (oneVPL / Media SDK). |
| `videotoolbox` | macOS | |
| `d3d11va` | Windows | `--gpu` selects the adapter. |
| `dxva2` | Windows | Legacy. |
| `vulkan` | Linux/Windows | Requires an FFmpeg built with Vulkan video. |

If a hardware decoder cannot handle a stream (unsupported codec/profile),
FFmpeg falls back to software decoding.

## Video encoding

Encoding happens only when faces are redacted. `--encoder auto` keeps the
source codec and, when `--hwaccel` is set, uses the matching hardware encoder:

| `--encoder` | Encoders | Lossless mode |
|---|---|---|
| `software` | `libx264`, `libx265`, `libvpx-vp9`, `libaom-av1`/`libsvtav1`, `ffv1`, `prores_ks`, … | yes (x264, x265, VP9, AV1 via libaom, FFV1) |
| `nvenc` | `h264_nvenc`, `hevc_nvenc`, `av1_nvenc` | yes for H.264/HEVC (`-tune lossless`) |
| `qsv` | `h264_qsv`, `hevc_qsv`, `av1_qsv`, `vp9_qsv` | no |
| `vaapi` | `h264_vaapi`, `hevc_vaapi`, `av1_vaapi`, `vp9_vaapi` | no |
| `videotoolbox` | `h264_videotoolbox`, `hevc_videotoolbox`, `prores_videotoolbox` | no |
| `amf` | `h264_amf`, `hevc_amf`, `av1_amf` | no |

With the default `--quality lossless`, `auto` switches to a software encoder
when the hardware one has no lossless mode (a note is printed). Choosing such
an encoder explicitly encodes at its highest quality instead and reports that
the result is not lossless.

Quality mapping:

| `--quality` | x264 | x265 | NVENC | QSV | VA-API | VideoToolbox |
|---|---|---|---|---|---|---|
| `lossless` | `-qp 0` | `lossless=1` | `-tune lossless` | `global_quality 1`* | `qp 1`* | `q:v 100`* |
| `visually-lossless` | `-crf 16` | `-crf 18` | `-cq 16` | `global_quality 18` | `qp 18` | `q:v 80` |
| `high` | `-crf 20` | `-crf 22` | `-cq 22` | `global_quality 23` | `qp 23` | `q:v 65` |

\* not mathematically lossless.

Use `--encoder-name` for any other FFmpeg encoder and `--preset` to trade
speed for size.

## Examples

```sh
# NVIDIA: decode, detect and encode on GPU 1
mediaanon process --faces --hwaccel cuda --device cuda --gpu 1 --quality visually-lossless clip.mp4

# Intel on Linux: VA-API decode/encode, OpenVINO detection
mediaanon process --faces --hwaccel vaapi --encoder vaapi --quality high --device openvino clip.mp4

# Apple silicon
mediaanon process --faces --hwaccel videotoolbox --encoder videotoolbox --quality high --device coreml clip.mov

# Windows, any GPU
mediaanon process --faces --hwaccel d3d11va --device directml clip.mp4
```
