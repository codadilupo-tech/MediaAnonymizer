#!/usr/bin/env python3
"""Records docs/images/gui-demo.gif: the video timeline in action.

Drives the real GUI on an X display (Xvfb) with synthetic mouse and keyboard
input (XTest) while ffmpeg records the window, then turns the recording into
a GIF. Called by scripts/gui_screenshots.sh; Linux only.

Usage: gui_demo.py GUI_BINARY CLIP.mp4 OUT.gif

Needs: python-xlib (pip install python-xlib), ffmpeg with x11grab, a running X
server in $DISPLAY (Xvfb), and the demo clip made by gui_screenshots.sh
(6 s, 25 fps). Coordinates are relative to the window, which has a fixed
1360x820 size and the default panel sizes; if the layout changes, check the
frames of the GIF and update the constants below.
"""

import os
import shutil
import subprocess
import sys
import tempfile
import time

from Xlib import X, XK, display
from Xlib.ext import xtest

W, H = 1360, 820
CLIP_LENGTH = 6.0  # seconds, see gui_screenshots.sh

# Section colours of the timeline bar (crates/gui/src/timeline.rs), used to
# find it on screen: its height depends on the panel layout.
SECTION_COLOURS = [(96, 110, 130), (52, 120, 210), (150, 150, 150), (205, 64, 64)]

# Left panel while the playhead is in a keyframe section ("This section").
CUT_RADIO = (85, 101)
STYLE_COMBO = (95, 221)
STYLE_BLUR = (75, 271)  # "Blur" in the open style list
# "Delete keyframe" in a keyframe's right-click menu, relative to the marker.
CONTEXT_DELETE = (55, 58)
STRENGTH_SLIDER = (8, 108, 267)  # left, right, y; range 0.25..4.0


class Driver:
    def __init__(self, d, win):
        self.d = d
        self.win = win
        pos = win.translate_coords(d.screen().root, 0, 0)
        self.ox, self.oy = -pos.x, -pos.y
        self.x, self.y = W // 2, H // 2
        # Without a window manager nothing gives the window the keyboard.
        win.set_input_focus(X.RevertToParent, X.CurrentTime)
        d.sync()

    def pixel_rows(self, x, y0, y1):
        img = self.d.screen().root.get_image(self.ox + x, self.oy + y0, 1, y1 - y0, X.ZPixmap, 0xFFFFFFFF)
        data = img.data
        return [(data[i * 4 + 2], data[i * 4 + 1], data[i * 4]) for i in range(y1 - y0)]

    def pixel_cols(self, y, x0, x1):
        img = self.d.screen().root.get_image(self.ox + x0, self.oy + y, x1 - x0, 1, X.ZPixmap, 0xFFFFFFFF)
        data = img.data
        return [(data[i * 4 + 2], data[i * 4 + 1], data[i * 4]) for i in range(x1 - x0)]

    def locate_track(self):
        """Finds the timeline bar: (left, right, middle y, keyframe marker y)."""

        def section(c):
            return any(sum(abs(a - b) for a, b in zip(c, s)) < 30 for s in SECTION_COLOURS)

        # From the bottom up, the first run of bar colour as tall as the bar
        # (the video frame above may contain the same colours).
        x = W - 30  # inside the bar, away from the playhead and the legend
        y0 = H // 3
        rows = self.pixel_rows(x, y0, H - 20)
        run = 0
        for i in range(len(rows) - 1, -1, -1):
            run = run + 1 if section(rows[i]) else 0
            if run >= 15 and (i == 0 or not section(rows[i - 1])):
                top, bottom = y0 + i, y0 + i + run - 1
                break
        else:
            raise SystemExit("timeline bar not found")
        y = (top + bottom) // 2
        cols = self.pixel_cols(y, W // 2, W)
        xs = [W // 2 + i for i, c in enumerate(cols) if section(c)]
        self.track = (xs[0], xs[-1], y, top - 6)
        return self.track

    def tx(self, t):
        left, right, _, _ = self.track
        return round(left + t / CLIP_LENGTH * (right - left))

    def _move(self, x, y):
        xtest.fake_input(self.d, X.MotionNotify, x=self.ox + round(x), y=self.oy + round(y))
        self.d.sync()

    def move(self, x, y, duration=0.4):
        """Glides the pointer, so the movement is visible in the recording."""
        steps = max(1, int(duration * 40))
        x0, y0 = self.x, self.y
        for i in range(1, steps + 1):
            f = i / steps
            f = f * f * (3 - 2 * f)  # ease in/out
            self._move(x0 + (x - x0) * f, y0 + (y - y0) * f)
            time.sleep(duration / steps)
        self.x, self.y = x, y

    def button(self, down, which=1):
        xtest.fake_input(self.d, X.ButtonPress if down else X.ButtonRelease, which)
        self.d.sync()

    def click(self, x, y, which=1, pause=0.5):
        self.move(x, y)
        time.sleep(0.15)
        self.button(True, which)
        time.sleep(0.08)
        self.button(False, which)
        time.sleep(pause)

    def drag(self, x0, y0, x1, y1, duration=1.2, pause=0.5):
        self.move(x0, y0)
        time.sleep(0.15)
        self.button(True)
        time.sleep(0.1)
        self.move(x1, y1, duration)
        time.sleep(0.1)
        self.button(False)
        time.sleep(pause)

    def key(self, name, pause=0.6):
        code = self.d.keysym_to_keycode(XK.string_to_keysym(name))
        xtest.fake_input(self.d, X.KeyPress, code)
        self.d.sync()
        time.sleep(0.05)
        xtest.fake_input(self.d, X.KeyRelease, code)
        self.d.sync()
        time.sleep(pause)


def find_window(d, title, timeout=60):
    root = d.screen().root
    end = time.time() + timeout
    while time.time() < end:
        stack = [root]
        while stack:
            w = stack.pop()
            try:
                name = w.get_wm_name()
                children = w.query_tree().children
            except Exception:
                continue
            if name == title and w.get_geometry().width >= W:
                return w
            stack.extend(children)
        time.sleep(0.5)
    raise SystemExit(f"window {title!r} not found")


def storyboard(p):
    """The demo: scrub, add keyframes, change a section, cut, move, delete."""
    t = p.tx
    _, _, ty, my = p.locate_track()
    time.sleep(1.0)
    # Scrub through the clip.
    p.drag(t(0.2), ty, t(4.8), ty, duration=1.8)
    p.drag(t(4.8), ty, t(1.5), ty, duration=1.2, pause=0.8)
    # K: a keyframe at 1.5 s; the left panel now edits that section.
    p.key("k", pause=1.2)
    _, _, ty, my = p.locate_track()
    # Blur instead of the black band, stronger.
    p.click(*STYLE_COMBO, pause=0.8)
    p.click(*STYLE_BLUR, pause=1.0)
    left, right, y = STRENGTH_SLIDER
    p.drag(left + (right - left) * (1.0 - 0.25) / 3.75, y, left + (right - left) * (2.5 - 0.25) / 3.75, y,
           duration=0.8, pause=1.2)
    # A second keyframe at 3.5 s, cut from the output.
    p.click(t(3.5), ty, pause=0.6)
    p.key("k", pause=0.8)
    p.click(*CUT_RADIO, pause=1.5)
    # Drag the cut's keyframe to 3.0 s.
    p.drag(t(3.5), my, t(3.0), my, duration=1.0, pause=1.2)
    # A third keyframe at 4.2 s, then delete it from its right-click menu (far
    # enough from the window edge for the menu to open to the right).
    p.click(t(4.2), ty, pause=0.6)
    p.key("k", pause=1.0)
    p.click(t(4.2), my, which=3, pause=1.0)
    p.click(t(4.2) + CONTEXT_DELETE[0], my + CONTEXT_DELETE[1], pause=1.2)
    # Step through the sections: global, blur, cut.
    p.click(t(0.6), ty, pause=1.5)
    p.click(t(2.2), ty, pause=1.8)
    p.click(t(4.0), ty, pause=1.8)


def main():
    gui, clip, out = sys.argv[1:4]
    work = tempfile.mkdtemp()
    env = dict(os.environ)
    # Fresh settings: a private home and data dirs for this run.
    for var in ("HOME", "XDG_DATA_HOME", "XDG_CONFIG_HOME", "XDG_CACHE_HOME"):
        env[var] = os.path.join(work, var.lower())
        os.makedirs(env[var], exist_ok=True)
    app = subprocess.Popen([gui, clip], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    rec = None
    try:
        d = display.Display()
        win = find_window(d, "MediaAnonymizer")
        time.sleep(4.0)  # video information and the first frame
        p = Driver(d, win)
        p._move(W // 2, H // 2)
        raw = os.path.join(work, "demo.mkv")
        rec = subprocess.Popen(
            ["ffmpeg", "-loglevel", "error", "-y", "-f", "x11grab", "-draw_mouse", "1", "-framerate", "15",
             "-video_size", f"{W}x{H}", "-i", f"{os.environ['DISPLAY']}+{p.ox},{p.oy}",
             "-c:v", "libx264", "-preset", "veryfast", "-qp", "0", raw],
            stdin=subprocess.PIPE)
        storyboard(p)
        rec.communicate(b"q")
        rec = None
        # Half size, 12 fps, one palette for the whole clip.
        vf = "fps=12,scale=960:-1:flags=lanczos,split[a][b];[a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=4:diff_mode=rectangle"
        subprocess.run(["ffmpeg", "-loglevel", "error", "-y", "-i", raw, "-vf", vf, "-loop", "0", out], check=True)
    finally:
        if rec:
            rec.kill()
        app.terminate()
        app.wait(10)
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
