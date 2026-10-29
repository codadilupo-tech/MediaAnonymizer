# Changelog

All notable changes are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the project uses
[Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added

- `mediaanon-core` library, `mediaanon` CLI and `mediaanon-gui` desktop app.
- Metadata policies `keep`, `sensitive` (GPS and personal data) and `all`
  for JPEG, PNG, WebP, TIFF and every FFmpeg-readable video container;
  lossless container-level editing and in-place EXIF editing.
- Removal of GPS telemetry data streams, cover art, chapters and container
  tags in videos (stream copy, no re-encoding).
- Face redaction with the YuNet detector: `eyes-only` / `full-face` modes,
  `black-band` / `blur` / `pixelate` styles, ellipse or rectangle shapes,
  head-tilt alignment, temporal smoothing for videos.
- DCT-domain JPEG redaction (untouched blocks bit-identical), lossless
  re-encoding for other photo formats and lossless video encoding by default.
- Inference on CPU, CUDA, TensorRT, DirectML, Core ML, OpenVINO and ROCm;
  FFmpeg hardware decoding (CUDA, VA-API, QSV, VideoToolbox, D3D11VA, DXVA2,
  Vulkan) and encoding (NVENC, QSV, VA-API, VideoToolbox, AMF).
- Timestamp overlay (`--timestamp`, `--timestamp-position bottom-left|bottom-right`,
  `--timestamp-format`, `--timestamp-size`, and a GUI section): the capture
  date from EXIF / video tags (falling back to the file time) is drawn on
  photos and videos.
- Training pipeline for an accessory model (hats, helmets, glasses,
  sunglasses) in `models/accessories/`: Open Images download, head crops,
  SSDlite/MobileNetV3 training, evaluation and ONNX export, with a CPU smoke
  test. The model is not used by the app yet.
- Faces left visible: click a face in the preview to leave it uncovered
  (photos), or to leave it uncovered from that frame on (videos: the face is
  tracked until it is lost, then covered again; the timeline shows where).
  Library: `Edits`, `VisibleFace`, `Processor::track_visible_faces`; the CLI
  `--timeline` file accepts `visible` marks. There is deliberately no
  automatic rule (such as by uniform): see docs/PRIVACY.md.
- Video timeline: keyframes start sections with their own face settings
  (or none), timestamp on/off, or cut them out of the output (video and
  audio). GUI: frame-accurate scrubbing with a live preview of the frame
  under the playhead, keyframe markers and a section editor. CLI:
  `--timeline FILE` (JSON). Library: `Timeline`, `Keyframe`,
  `Processor::process_file_with`, `Processor::preview_video_frame`,
  `video::info`.
- GUI: double-click a slider to reset it; zoomable, pannable preview; a
  sample photo is previewed when a video is selected; **Start** also re-runs
  failed and skipped files; *Overwrite existing files* is shown in red and
  asks for confirmation before replacing files.
- `--device auto` falls back to the CPU when the selected GPU provider cannot
  be initialised (e.g. DirectML without a usable DirectX 12 GPU).
- GUI: per-file **Retry** and **Retry failed** buttons; failure reasons shown inline.
- Mobile app for Android and iOS (`mobile/`, Flutter): pick or take photos,
  remove metadata, redact faces and add the timestamp on the device, with a
  live preview, the metadata list and saving to the photo library. Built on the
  new `mediaanon-mobile` C ABI crate; CI builds the APK and an unsigned iOS app,
  and releases attach `MediaAnonymizer-<v>-android.apk` and
  `MediaAnonymizer-<v>-ios-unsigned.ipa` (optional Android release signing).
- Release workflow: publishing a GitHub Release builds an Apple-silicon macOS
  `.app` in a `.dmg` (optionally signed and notarized), a Windows installer
  and portable zip, and Linux archives, and attaches them with checksums.
- App icon; FFmpeg is also looked up in Homebrew/MacPorts/WinGet/Chocolatey/Scoop
  locations so desktop launches find it without a shell `PATH`.
- `inspect` and `devices` commands, JSON output, atomic output files.
