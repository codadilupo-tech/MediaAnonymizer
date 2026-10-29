# Command-line reference

```text
mediaanon [-v|-vv] [-q] [--json] <COMMAND>

Commands:
  process (run)   Anonymize files and directories
  inspect         Show the metadata stored in files and what --strip-sensitive would remove
  devices         List inference devices, hardware decoders and encoders
```

Global options: `-v`/`-vv` more logging, `-q` errors only, `--json`
machine-readable output (see [JSON output](#json-output)).

## `mediaanon process`

```text
mediaanon process [OPTIONS] <INPUTS>...
```

At least one action is required: `--strip-metadata`, `--strip-sensitive`,
`--faces` (or any `--face-*` option) and/or `--timestamp` (or any
`--timestamp-*` option).

### Inputs and outputs

| Option | Description |
|---|---|
| `<INPUTS>...` | Files and/or directories. Directories contribute every file with a supported extension (hidden files are skipped). |
| `-r, --recursive` | Recurse into sub-directories. |
| `-o, --output <PATH>` | With a single input file and a path with an extension: the output file. Otherwise an output **directory**; the directory structure of the inputs is mirrored inside it and file names are kept. |
| `--suffix <S>` | Without `--output`, outputs are written next to their input with this suffix before the extension (`photo.jpg` → `photo.anon.jpg`). Default `.anon`. Files already carrying the suffix are skipped when scanning directories. |
| `--in-place` | Replace the inputs. The new file is written to a temporary file first and atomically renamed over the original. |
| `--overwrite` | Overwrite existing outputs (otherwise they are skipped). |
| `--dry-run` | Print `input -> output` pairs without processing. |
| `-j, --jobs <N>` | Photos processed in parallel (default: CPU cores). Videos are processed one after the other, each using every core. |

Outputs are always written atomically: an interrupted or failed run never
leaves a truncated file behind. `Ctrl-C` cancels cleanly; press it twice to
abort immediately.

### Metadata

| Option | Description |
|---|---|
| `-m, --strip-metadata` | Remove all metadata not needed to render the media. |
| `-s, --strip-sensitive` | Remove location and personal data, keep camera settings and dates. |

See [PRIVACY.md](PRIVACY.md) for the exact rules.

### Face redaction

| Option | Default | Description |
|---|---|---|
| `-f, --faces` | off | Enable face detection and redaction. Implied by every option below. |
| `--face-mode <eyes-only\|full-face>` | `full-face` | Area to cover: a band across the eyes (aligned with the eye line) or the whole face. |
| `--face-style <black-band\|blur\|pixelate>` | `black-band` | Rendering of the covered area. |
| `--face-shape <ellipse\|rectangle>` | `ellipse` | Outline of the full-face area. |
| `--padding <F>` | `0.15` | Margin around the face, as a fraction of its size (0–4). |
| `--min-confidence <S>` | `0.6` | Detector threshold (0–1). Lower = more faces found, more false positives. |
| `--strength <F>` | `1.0` | Multiplier of the blur radius / mosaic block size (0–10]. |
| `--detection-size <PX>` | 1920 photos, 960 videos | Longest side of the image given to the detector. Increase for small faces in high-resolution media. |
| `--model <FILE>` | bundled | Custom YuNet-compatible ONNX model. |

### Timestamp overlay

Draws the capture date on the picture as white text with a dark outline, in
the corner of the image *as displayed* (EXIF orientation and video rotation
are honoured).

| Option | Default | Description |
|---|---|---|
| `-t, --timestamp` | off | Enable the overlay. Implied by every option below. |
| `--timestamp-position <bottom-left\|bottom-right>` | `bottom-right` | Corner where the date is drawn. |
| `--timestamp-format <PATTERN>` | `%Y-%m-%d %H:%M:%S` | `%Y` year, `%y` 2-digit year, `%m` month, `%b` month name (Jan…), `%d` day, `%H` `%M` `%S` time, `%%` a percent sign. |
| `--timestamp-size <F>` | `0.035` | Text height as a fraction of the picture height (min. 12 px; long text is shrunk to fit the width). |

Where the date comes from, first match wins:

| Media | Sources |
|---|---|
| Photos | EXIF `DateTimeOriginal`, `DateTimeDigitized`, `DateTime` (JPEG, PNG `eXIf`, WebP, TIFF); PNG `tIME`; file modification time |
| Videos | `com.apple.quicktime.creationdate` (local time), container `creation_time`, video stream `creation_time` (both UTC); file modification time |

EXIF and QuickTime dates are shown in the local time recorded by the camera;
`creation_time` and file times are UTC. The date is read **before** metadata
removal, so `--strip-metadata --timestamp` keeps the date visible while
removing it from the metadata. The overlay changes pixels: JPEG keeps every
block not touched by the text bit-identical, videos are re-encoded (losslessly
by default) even without face redaction.

### Compute

| Option | Default | Description |
|---|---|---|
| `--device <D>` | `auto` | Face detection device: `auto`, `cpu`, `cuda`, `tensorrt`, `directml`, `coreml`, `openvino`, `rocm`. `auto` picks the best GPU provider available in this build and falls back to the CPU. An explicitly requested device that is unavailable is an error. |
| `--gpu <N>` | `0` | GPU index for inference and, with CUDA / D3D11VA / NVENC, for video decoding and encoding. |
| `--threads <N>` | all cores | Upper bound of worker threads used for detection. |

### Video

These options matter only when faces are redacted in videos: metadata-only
processing is always a lossless stream copy.

| Option | Default | Description |
|---|---|---|
| `--hwaccel <A>` | `none` | Hardware decoding: `none`, `auto`, `cuda`, `vaapi`, `qsv`, `videotoolbox`, `d3d11va`, `dxva2`, `vulkan`. |
| `--encoder <E>` | `auto` | Encoder family: `auto`, `software`, `nvenc`, `qsv`, `vaapi`, `videotoolbox`, `amf`. `auto` uses the hardware encoder matching `--hwaccel` (CUDA→NVENC, QSV→QSV, VA-API→VA-API, VideoToolbox→VideoToolbox) unless it cannot honour `--quality lossless`, in which case a software encoder is used. |
| `--encoder-name <NAME>` | — | Exact FFmpeg encoder (`libx264`, `libx265`, `hevc_nvenc`, `ffv1`, …). Overrides `--encoder`. |
| `--quality <Q>` | `lossless` | `lossless` (bit-exact pixels, large files), `visually-lossless` (CRF ≈ 16–18), `high` (CRF ≈ 20–23). |
| `--preset <P>` | per encoder | Encoder speed preset, e.g. `ultrafast`…`veryslow` for x264/x265, `p1`…`p7` for NVENC. |
| `--detect-every <N>` | `1` | Run detection every N frames; intermediate frames reuse the neighbouring detections. Faster, slightly less safe. |
| `--persistence <N>` | `max(3, N-1)` | Each detection also covers the N frames before and after it. Must be ≥ `detect-every - 1`. |
| `--vaapi-device <PATH>` | `/dev/dri/renderD128` | VA-API render node. |
| `--timeline <FILE>` | — | Per-section settings for every video, see below. |

#### Timeline

`--timeline edits.json` changes the settings from given points of the videos, or cuts
parts out. Each keyframe starts a section that lasts until the next keyframe; before the
first one the command-line settings apply. Photos ignore the keyframes.

The same file can list faces to leave visible (`visible`), meant for a single input: a
point on the face as a fraction of the upright picture's width and height, and for videos
the time of the frame the point refers to. The face is followed from there until it is
lost (see [PRIVACY.md](PRIVACY.md#faces-left-visible)).

```json
{
  "keyframes": [
    { "time": 12.0, "faces": { "style": "blur", "strength": 2.0 } },
    { "time": 30.5, "cut": true },
    { "time": 41.0, "faces": null, "timestamp": false }
  ],
  "visible": [
    { "time": 3.2, "x": 0.41, "y": 0.27 }
  ]
}
```

| Field | Default | Meaning |
|---|---|---|
| `time` | `0` | Start of the section, in seconds from the first frame (rounded to a frame). |
| `faces` | default face settings | Face options of the section (same names as in the library: `mode`, `style`, `shape`, `padding`, `min_confidence`, `strength`, `detection_size`); `null` leaves faces uncovered. |
| `timestamp` | `true` | Draw the timestamp in this section (when `--timestamp` is on). |
| `cut` | `false` | Remove the section, video and audio, from the output. |

Cuts re-encode the audio: FLAC in MKV, ALAC in MOV, PCM in AVI (lossless), AAC 320 kb/s
in MP4 and other containers, Opus 256 kb/s in WebM. Subtitles, data tracks and chapters
are dropped, since they would no longer line up with the cut video.

The source codec is kept when possible. In lossless mode codecs without a
lossless mode are replaced by one the container accepts (H.264 for MP4/MOV/MKV,
VP9 for WebM, FFV1 for AVI/MXF). Lossless H.264 uses the *High 4:4:4
Predictive* profile, which some hardware players do not support; use
`--quality visually-lossless` for maximum compatibility.

## `mediaanon inspect`

```text
mediaanon inspect <FILES>...
```

Lists metadata blocks and noteworthy tags. Lines marked `!` are removed by
`--strip-sensitive`; everything except blocks marked "needed for rendering" is
removed by `--strip-metadata`.

## `mediaanon devices`

Lists the inference devices compiled into this build and whether ONNX Runtime
can use them, the FFmpeg version, its hardware decoders and the relevant
encoders. A listed FFmpeg back-end still needs a matching GPU and driver.

## JSON output

With `--json`, `process` prints one JSON object per file on stdout:

```json
{"input":"photo.jpg","output":"photo.anon.jpg","kind":{"kind":"image","format":"jpeg"},
 "faces":1,"frames":null,"frames_with_faces":null,
 "removed":["EXIF Artist","EXIF GPS location (4 tags)","COM comment (12 B)"],
 "warnings":[],"notes":["JPEG edited in the DCT domain: 472 of 6144 blocks re-encoded (7.68%), the rest are bit-identical"],
 "input_bytes":68268,"output_bytes":62811,"elapsed":0.05}
```

Failures are reported as `{"input": "...", "error": "..."}` and skipped files
as `{"input": "...", "skipped": "..."}`. `inspect --json` prints
`{"input": "...", "metadata": [{"block", "detail", "sensitive"}, ...]}` per
file and `devices --json` a single object.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Every file was processed (or skipped). |
| 1 | At least one file failed, the run was cancelled, or a fatal error occurred. |
| 2 | Invalid command line. |

## Environment variables

| Variable | Description |
|---|---|
| `MEDIAANON_FFMPEG`, `MEDIAANON_FFPROBE` | Paths of the FFmpeg tools (default: next to `mediaanon`, then `PATH`). |
| `ORT_DYLIB_PATH` | ONNX Runtime library to load (builds with the `ort-dynamic` feature). |
| `MEDIAANON_LOG` | Log filter (`tracing` syntax), e.g. `MEDIAANON_LOG=debug`. |

## Recipes

```sh
# Share holiday photos: no location, no names, no faces, nothing else changed.
mediaanon process -s --faces -r ~/Holiday -o ~/Holiday-share

# Journalism: maximum safety.
mediaanon process -m --faces --face-style black-band --min-confidence 0.4 --detection-size 3000 source.jpg

# Large video archive on an NVIDIA GPU, smaller files.
mediaanon process -m --faces --face-style blur --hwaccel cuda --device cuda \
    --quality visually-lossless -r ./videos -o ./videos-anon

# Intel iGPU on Linux.
mediaanon process --faces --hwaccel vaapi --encoder vaapi --quality high --device openvino clip.mp4

# Strip all metadata but keep the date visible, bottom-left, European format.
mediaanon process -m --timestamp-position bottom-left --timestamp-format "%d/%m/%Y %H:%M" -r ./photos -o ./out

# Faster video at the cost of some safety: detect every 3rd frame.
mediaanon process --faces --detect-every 3 --persistence 4 clip.mp4
```
