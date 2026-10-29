# Desktop application

`mediaanon-gui` is a desktop front-end for the same engine as the CLI
(`mediaanon-core`). It is a single native binary built with
[egui](https://github.com/emilk/egui) and runs on Linux (X11 and Wayland), Windows and macOS.

![MediaAnonymizer GUI](images/gui.png)

## Features

**Settings (left panel)**, saved between runs:

- **Metadata**: *Keep*, *Remove sensitive (GPS & personal)* or *Remove all*. These are the
  same policies as the CLI's `--strip-sensitive` / `--strip-metadata` (see [PRIVACY.md](PRIVACY.md)).
- **Faces**: turn face redaction on or off, then pick the mode (*Eyes only* / *Full face*),
  the style (*Black band* / *Blur* / *Pixelate*), the shape of the full-face area, and the
  strength, padding, detection threshold and an optional detector input size.
  Double-click a slider to reset it to its default.
- **Timestamp**: overlay the capture date, at the bottom left or bottom right, with a
  configurable format (`%Y-%m-%d %H:%M:%S` by default) and text size. The preview shows it.
- **Compute**: the inference device. Devices that this build or the installed ONNX Runtime
  cannot use are greyed out, and hovering one tells you which Cargo feature enables it.
  You can also set the GPU index and a thread limit.
- **Video**: hardware decoding (the menu only enables back-ends your FFmpeg supports), the
  encoder family, the quality (*Lossless* by default), how often to run detection and how
  many frames a detection persists.
- **Output**: write next to the originals with a suffix (default `.anon`), or into a folder.
  **Overwrite existing files** (shown in red) replaces outputs that already exist; when
  it would, **Start** first opens a dialog listing the files and asks for confirmation.
  The same confirmation appears when an input is its own output (output folder = source
  folder).

If a setting combination is invalid (for example nothing to do, or no output folder chosen),
the reason shows next to the **Start** button and the button stays disabled.

**Queue (centre)**:

- Add files with **Add files…**, add folders (scanned recursively) with **Add folder…**,
  or drag and drop files and folders onto the window.
- Each row shows the file's type and status: pending, a progress bar (videos report
  per-frame progress), a summary when done (hover it to see the output path, what was
  removed, and how the file was encoded), or the error when it failed (hover to read it).
- A failed or skipped file has a **Retry** button that processes it again right away (for
  example after switching the inference device, or enabling *Overwrite* for a skipped
  file). **Retry failed (N)** in the toolbar re-runs every failed file. The start of the
  error is shown in the row; hover it for the full message.
- **Start** processes the queue in the background: pending files, and again the failed
  and skipped ones, so after fixing a setting you can simply press **Start** to re-run. **Cancel** stops after the current
  frame and removes partial outputs. **Clear finished** empties the list of completed rows.

**Preview (right)**: select a photo while face redaction or the timestamp is on to see the
result and the number of faces found. You can switch between *Redacted* and *Original*. The
preview updates a moment after you change a setting. Scroll (or pinch) to zoom around the
cursor, drag to pan, and double-click or press **Fit** to see the whole picture again; the
zoom is kept when switching between *Redacted* and *Original*, and pixels are shown sharp
when zoomed in, so pixelation looks like the output.

When nothing is selected, a bundled sample photo is shown, so you can still tune the face and
timestamp settings on a real face.

**Leaving faces visible**: detected faces are outlined and numbered in the preview (turn the
outlines off with **Faces**). Every face is covered by default; click a face to leave it
visible (green outline), click it again to cover it again. Photos show how many faces are
marked, with **Cover all again**. In a video, a click marks the face *from that frame on*: it
is followed until it leaves the picture or can no longer be found, then covered again. The
timeline panel lists the marks (**Go to**, **Remove**), shows a green strip where each face
stays visible, and the follow-up runs in the background whenever the marks or settings
change. Scrub through the video to check, and click a face again where it was lost. See
[PRIVACY.md](PRIVACY.md#faces-left-visible) for why this is never automatic.

**Video timeline**: selecting a video shows the frame under the playhead and, below it, a
timeline:

- Click or drag on the timeline to pick a frame; the preview shows that frame with the
  settings of its section (the detection runs on that frame alone; the real output also
  smooths detections over neighbouring frames). The buttons step one frame, jump to the
  previous / next keyframe or to the start / end. With the keyboard: **←** / **→** step
  one frame, **Shift+←** / **Shift+→** one second, **Home** / **End**.
- **Add keyframe** (the diamond-plus button, or **K**) at the playhead starts a new
  *section* that lasts until the next keyframe or the end of the video, with the settings
  in effect there. Before the first keyframe the global settings apply.
- While the playhead is in a section, the left panel shows **This section**: choose
  **Process** or **Cut from the output** (those frames and the matching audio are
  removed), the face settings (mode, style, strength…, or **Detect and cover faces**
  turned off to leave faces visible in that part) and whether the timestamp is shown.
  **Reset to global** copies the global settings into the section; the **Global** tab
  edits the settings shared by every file.
- Diamonds above the timeline mark keyframes: click one to jump to it, drag it to move
  it, right-click it to delete it (the previous section then extends over it). On a
  keyframe, the same button (or **K** / **Delete**) removes it.
- Sections are coloured: global settings, section settings, faces not covered, and cut
  (red, hatched). The queue row shows how many keyframes a video has.

Cutting re-encodes the audio (FLAC in MKV, ALAC in MOV, PCM in AVI; AAC 320 kb/s in MP4 and
Opus 256 kb/s in WebM) and drops subtitles, data tracks and chapters, which would no longer
line up. Everything else stays lossless.

The footer shows the version, the device used for detection, and a reminder: the black band
is irreversible, while blur and pixelate at low strength can be weaker protection.

## Building and running

```sh
# Default build: downloads a prebuilt ONNX Runtime at build time.
cargo run --release -p mediaanon-gui

# Bring your own ONNX Runtime (for example a GPU-enabled build), loaded at runtime:
cargo build --release -p mediaanon-gui --no-default-features --features ort-dynamic
ORT_DYLIB_PATH=/path/to/libonnxruntime.so target/release/mediaanon-gui

# GPU inference providers use the same features as the CLI:
cargo build --release -p mediaanon-gui --features cuda
```

The GUI isn't part of the workspace's default members, so a plain `cargo build` doesn't
build it. Pass `-p mediaanon-gui`.

Files and folders given on the command line are added to the queue:

```sh
mediaanon-gui ~/Pictures/holiday ~/Videos/clip.mp4
```

Video processing requires `ffmpeg` and `ffprobe` (5.1 or newer) on `PATH`, or set through
`MEDIAANON_FFMPEG` / `MEDIAANON_FFPROBE`. Photos work without FFmpeg.

### Linux runtime libraries

The window is opened through winit and OpenGL. On minimal systems install `libxkbcommon-x11-0`
(X11), `libwayland-client0` (Wayland) and an OpenGL driver (Mesa). The file dialogs use the
XDG desktop portal, so no GTK development packages are needed to build.

### Environment variables

| Variable | Effect |
| --- | --- |
| `MEDIAANON_LOG` | Log filter, for example `info` or `debug` (logs go to stderr). |
| `MEDIAANON_GUI_AUTOSTART=1` | Start processing the queue as soon as the window opens. |
| `MEDIAANON_GUI_SCREENSHOT=out.png` | Save a screenshot once the preview (and any auto-started run) is ready, then exit. It doesn't read or write your saved settings. It's used to produce the image above. |

The documentation screenshot was produced headless with:

```sh
Xvfb :99 -screen 0 1400x900x24 &
DISPLAY=:99 LIBGL_ALWAYS_SOFTWARE=1 MEDIAANON_GUI_AUTOSTART=1 \
  MEDIAANON_GUI_SCREENSHOT=docs/images/gui.png mediaanon-gui demo-folder/
```

## Code layout

| File | Responsibility |
| --- | --- |
| `crates/gui/src/main.rs` | Command-line arguments, window options, start-up. |
| `crates/gui/src/app.rs` | Main window layout, queue table, worker events, screenshot mode. |
| `crates/gui/src/settings.rs` | Persisted settings, validation, output naming, settings panel. |
| `crates/gui/src/queue.rs` | Queue items, status, folder scanning. |
| `crates/gui/src/worker.rs` | Background batch processing with throttled progress and cancellation. |
| `crates/gui/src/preview.rs` | Debounced background preview rendering (photos and video frames), zoom and pan, sample photo. |
| `crates/gui/src/timeline.rs` | Video timeline: scrubber, keyframes (add, move, delete), faces left visible. |
| `crates/gui/assets/sample.jpg` | Sample photo for the preview (NASA, public domain; see `assets/README.md`). |
