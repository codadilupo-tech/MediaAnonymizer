#!/usr/bin/env bash
# Regenerates the GUI screenshots and the animated demo used by README.md and
# docs/GUI.md:
#
#   docs/images/gui.png           queue after a run, photo preview
#   docs/images/gui-timeline.png  video with keyframes, left panel on "This section"
#   docs/images/gui-demo.gif      the video timeline in action (Linux only, see
#                                 scripts/gui_demo.py)
#
# Usage: scripts/gui_screenshots.sh
#   MEDIAANON_GUI_BIN=path/to/mediaanon-gui  use this binary instead of building one
#                                            (use a release build: the GIF needs a
#                                            responsive preview)
#   MEDIAANON_GUI_NO_GIF=1                   skip the GIF
#
# Needs ffmpeg (to make the demo clip). On Linux without a display it runs under
# xvfb-run (software OpenGL). The GIF also needs ffmpeg with x11grab and
# python-xlib (pip install python-xlib). The demo media come from the
# repository only: the NASA astronaut portrait (public domain), see
# crates/gui/assets/README.md.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
out="$root/docs/images"
mkdir -p "$out"

bin="${MEDIAANON_GUI_BIN:-}"
if [[ -z "$bin" ]]; then
  cargo build --release -p mediaanon-gui --manifest-path "$root/Cargo.toml"
  bin="$root/target/release/mediaanon-gui"
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# Demo media: a photo with GPS metadata, and a short clip panning over the portrait.
mkdir -p "$work/demo"
cp "$root/crates/core/tests/fixtures/astronaut_gps.jpg" "$work/demo/astronaut.jpg"
ffmpeg -loglevel error -y -loop 1 -i "$root/crates/gui/assets/sample.jpg" \
  -vf "scale=1024:1024,zoompan=z='1.0+0.002*on':x='iw/2-(iw/zoom/2)':y='ih/3-(ih/zoom/3)':d=1:s=640x640:fps=25" \
  -t 6 -c:v libx264 -pix_fmt yuv420p "$work/demo/clip.mp4"

# Keyframes for the timeline shot: blur from 1.5 s, a cut from 3.5 s, faces left
# uncovered from 4.5 s. The playhead sits in the blur section.
cat > "$work/edits.json" <<'JSON'
{
  "keyframes": [
    { "time": 1.5, "faces": { "style": "blur", "strength": 2.0 } },
    { "time": 3.5, "cut": true },
    { "time": 4.5, "faces": null }
  ]
}
JSON

run() {
  # Screenshot runs keep their settings in this file: start from the defaults.
  rm -f "${TMPDIR:-/tmp}/mediaanon-gui-screenshot.ron"
  if [[ "$(uname)" == "Linux" && -z "${DISPLAY:-}" && -z "${WAYLAND_DISPLAY:-}" ]]; then
    LIBGL_ALWAYS_SOFTWARE=1 xvfb-run -a -s "-screen 0 1500x900x24" "$@"
  else
    "$@"
  fi
}

echo "gui.png"
cp -R "$work/demo" "$work/run1"
run env MEDIAANON_GUI_AUTOSTART=1 MEDIAANON_GUI_SCREENSHOT="$out/gui.png" "$bin" "$work/run1"

echo "gui-timeline.png"
run env MEDIAANON_GUI_EDITS="$work/edits.json" MEDIAANON_GUI_POSITION=2.2 \
  MEDIAANON_GUI_SCREENSHOT="$out/gui-timeline.png" "$bin" "$work/demo/clip.mp4"

if [[ -n "${MEDIAANON_GUI_NO_GIF:-}" ]]; then
  echo "gui-demo.gif skipped (MEDIAANON_GUI_NO_GIF)"
elif [[ "$(uname)" != "Linux" ]]; then
  echo "gui-demo.gif skipped: it is recorded on Linux (Xvfb); run this script in a Linux machine or container"
else
  echo "gui-demo.gif"
  run python3 "$root/scripts/gui_demo.py" "$bin" "$work/demo/clip.mp4" "$out/gui-demo.gif"
fi

echo "Screenshots written to $out"
