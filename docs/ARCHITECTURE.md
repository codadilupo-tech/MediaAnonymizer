# Architecture

This document explains how MediaAnonymizer is organised and why the main
design decisions were taken. It is aimed at contributors.

## Crates

```text
mediaanon-core  (library)
├── options      user-facing configuration (serde, string conversions)
├── media        format sniffing (magic bytes first, extension second)
├── metadata     lossless container editing
│   ├── jpeg       marker segments, scan/trailer split, rebuild
│   ├── exif       in-place TIFF/EXIF editor (+ inspection)
│   ├── png        chunk filter / rebuild (CRC)
│   ├── webp       RIFF chunk filter / rebuild (VP8X flags)
│   └── irb        Photoshop image resources (IPTC, thumbnails)
├── detect       face detection
│   ├── yunet      ONNX session, pre-processing, output decoding, NMS
│   ├── runtime    ONNX Runtime environment, execution providers
│   └── geometry   Face, IoU, coordinate mapping
├── redact       regions (eye band / face ellipse) and pixel kernels
├── overlay      capture-date parsing/formatting and text stamp rendering
├── photo        still-image pipeline
│   ├── jpeg_dct   DCT-domain JPEG editing through libjpeg
│   └── orientation EXIF orientation mapping
├── video        FFmpeg-based pipeline
│   ├── probe      ffprobe model
│   ├── metadata   stream selection + tag policy → ffmpeg arguments
│   ├── encode     encoder selection and quality mapping
│   ├── pixfmt     raw frame layouts
│   └── temporal   detection smoothing over time
├── timeline     per-section video settings (keyframes, cuts)
├── edits        per-file decisions: timeline + faces left visible, face tracker
└── lib.rs       Processor, Report, Progress, inspect()

mediaanon-cli   clap front-end, batch planning, progress bars, JSON output
mediaanon-gui   egui front-end (see docs/GUI.md)
mediaanon-mobile C ABI (JSON in/out) for the Flutter app in mobile/ (see docs/MOBILE.md)
```

The front-ends contain no media logic: everything goes through
`Processor::process_file`, `Processor::preview` and `inspect`.

## Principles

1. **Never degrade what we do not need to touch.** Metadata is removed at the
   container level; pixels are re-encoded only when faces are redacted, and
   then losslessly or, for JPEG, only where the faces are.
2. **Privacy over convenience.** When in doubt a block is removed (unknown
   vendor segments, unparseable EXIF); previews are dropped whenever faces are
   redacted; video detections are smoothed over time.
3. **Atomic outputs.** Results are written to a temporary sibling file and
   renamed, so a crash or cancellation never leaves a partial file.
4. **Explicit hardware choices.** An explicitly requested accelerator that is
   unavailable is an error, never a silent fallback; `auto` is the only mode
   that falls back.

## Photo pipeline

```text
bytes ─┬─ metadata only ──▶ jpeg/png/webp::strip ──▶ output (pixels untouched)
       │
       └─ faces ──▶ decode (image crate, EXIF orientation)
                    ──▶ detection frame: upright, box-downscaled RGB
                    ──▶ YuNet ──▶ faces mapped back to stored coordinates
                    ──▶ regions ──▶ paint (fill / mosaic / blur)
                    ──▶ encode:
                         JPEG  → jpeg_dct::redact (blocks ∩ regions only)
                         PNG   → lossless PNG  + original chunks (filtered)
                         WebP  → lossless WebP + ICCP/EXIF/XMP (filtered)
                         TIFF/BMP → lossless re-encode
```

### DCT-domain JPEG redaction

`jpeg_read_coefficients` gives access to the quantized DCT coefficients of
every 8×8 block of every component. For each block whose pixel footprint
(16×16 for subsampled chroma) intersects a redaction region:

1. the redacted pixels of the footprint are converted to Y/Cb/Cr with the
   JFIF matrix and averaged over the subsampling footprint;
2. a forward DCT (JPEG normalisation) is applied and the result is quantized
   with the file's own quantization table;
3. the block is replaced.

All other blocks keep their exact coefficients, and `jpeg_write_coefficients`
entropy-codes them again losslessly (optimised Huffman tables, progressive
mode preserved). The output therefore decodes to exactly the same pixels
outside the redacted blocks. Unsupported color spaces (CMYK, YCCK, RGB-coded)
fall back to a full re-encode at quality 100 with a warning.

### EXIF editing

EXIF values reference each other through absolute offsets (MakerNotes even
use private offsets), so rebuilding the structure risks corrupting the tags
we keep. The editor works in place: removed entries are taken out of their
IFD table (remaining entries compacted, count and next-IFD pointer moved) and
all their bytes — inline values, out-of-line data, whole GPS/thumbnail
directories — are zeroed.

## Video pipeline

Metadata-only processing is one `ffmpeg -c copy` command whose `-map` and
`-metadata` arguments are computed from `ffprobe` output by
`video::metadata::mux_args`.

Face redaction runs two FFmpeg processes and five kinds of threads:

```text
            ┌──────────┐ raw frames ┌────────┐   ┌──────────────┐   ┌─────────────┐   ┌────────┐ raw  ┌──────────┐
 input ───▶ │ ffmpeg   │──(pipe)──▶ │ reader │──▶│ detector ×N  │──▶│ coordinator │──▶│ writer │─────▶│ ffmpeg   │──▶ output
            │ decoder  │            └────────┘   │ (ONNX pool)  │   │ reorder,    │   └────────┘      │ encoder  │
            │ (hwaccel)│                ▲        └──────────────┘   │ temporal,   │       │           │ + mux of │
            └──────────┘                │                           │ paint       │       │           │ audio/subs│
                                        └──────── free buffers ◀────┴─────────────┴───────┘           └──────────┘
```

* Frames travel through bounded channels; buffers are recycled, so memory is
  bounded (`≈ (6·N + persistence + 4)` frames).
* Frames are decoded in the **source pixel format** (or a layout-only
  equivalent: NV12 → yuv420p, RGB24 → gbrp, P010 → yuv420p10le). Painting
  works directly on the Y/U/V (or G/B/R) planes with per-plane scaling for
  chroma subsampling, so untouched pixels are bit-identical to the decoder
  output. Detection input is converted to RGB on the fly at reduced size.
* `temporal::Temporal` delays frames by `persistence` and merges the
  detections of neighbouring frames (closest first, IoU de-duplication).
* The encoder process reads the original file as a second input and stream
  copies audio, subtitles and the other kept streams. Display rotation is
  applied to the pixels by the decoder (detection needs upright faces), so the
  output carries no rotation flag.
* Timestamps: frames are re-timed at the source's average frame rate.
  Variable-frame-rate sources produce a warning.
* A `timeline::Timeline` (keyframes) maps each frame index to its section
  settings (`FrameSections`): detector workers use the section's face options,
  the coordinator paints with them (detections smoothed in from a
  neighbouring section are painted with this frame's settings, or not at
  all) and drops the frames of cut sections. Cuts also add an
  `aselect`/`asetpts` filter on the audio, re-encoded with a codec that suits
  the container, and drop subtitles, data streams and chapters.
* `video::preview_frame` decodes a single frame (accurate seek, RGB) and
  applies its section's settings, for the GUI timeline.
* Faces marked visible (`edits::VisibleFace`) are followed by
  `edits::Tracker` in the coordinator, in frame order and *before* temporal
  smoothing, so neighbouring frames do not cover them either: a mark picks
  the face under its point, then each frame continues the track with the
  best-overlapping detection (IoU ≥ 0.3) until the face is missing for longer
  than the tolerance. `video::track_visible` runs the same tracker on a
  detector-resolution decode, from the first mark until every track ended, so
  the GUI can show where faces stay visible before processing.

## Face detection

YuNet (`models/yunet_2023mar_dynamic.onnx`) is the 2023 YuNet model with its
input made dynamic (`scripts/make_dynamic_yunet.py`), so frames are processed
at their own aspect ratio (padded to a multiple of 32) instead of being
letterboxed into 640×640. Outputs are decoded as in OpenCV's
`FaceDetectorYN` (score = √(cls·obj)), followed by NMS (IoU 0.3).

`DetectorPool` owns several sessions behind a mutex/condvar; CPU builds use
one session per two cores for inter-frame parallelism, GPU builds two
sessions.

## Redaction kernels

`redact::Plane<T>` is a strided view (`stride`, `step`, `channels`,
sub-sampling scale) over 8- or 16-bit samples, so the same three kernels serve
interleaved RGB(A) photos (alpha untouched) and planar video:

* **fill** – constant per channel (black in that plane's color space:
  Y = 16·2^(bits-8) in limited range, chroma = mid-grey);
* **pixelate** – block means on a grid anchored at the plane origin (luma and
  chroma blocks line up exactly);
* **blur** – three box-filter passes per axis (≈ Gaussian), prefix sums in
  f64, edge clamping.

Coverage is decided per pixel by mapping the pixel center back to image
coordinates and testing the (rotated) rectangle or ellipse.

## Timestamp overlay

`overlay::Stamp` rasterises the formatted date once per file with
[`ab_glyph`](https://crates.io/crates/ab_glyph) and the embedded Hack font,
then dilates the glyph coverage with a disc to obtain a legibility outline.
The stamp is placed in the corner of the *displayed* image and, for photos
with an EXIF orientation, re-projected onto the stored pixel grid.
`Plane::blend_stamp` alpha-blends it (outline in black, glyphs in white, in
each plane's own color space), so the same code serves RGB photos and native
YUV video frames. For JPEG the stamp's coverage is part of the "touched
blocks" test, so only the blocks under the text are re-encoded. A timestamp
alone sends videos through the re-encoding pipeline without face detection.

## Testing

* Unit tests next to the code (parsers, kernels, encoder planning, CLI args).
* `crates/core/tests/photo.rs` – real portrait: bit-exactness outside faces,
  metadata removal, orientation, lossless PNG/WebP, atomic writes.
* `crates/core/tests/video.rs` – generated clips: bit-exact remux, lossless
  redaction frame by frame, 10-bit HEVC (skipped without FFmpeg).
* `crates/cli/tests/cli.rs` – end-to-end CLI behaviour.
