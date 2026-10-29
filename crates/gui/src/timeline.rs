//! Video timeline: a scrubber to pick the previewed frame, keyframes that
//! start sections with their own settings, and the editor of the section
//! under the playhead.

use crate::queue::Item;
use crate::settings::{self, Settings};
use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Vec2};
use mediaanon_core::{Keyframe, Options, Timeline, VideoInfo};

const TRACK_HEIGHT: f32 = 22.0;
const MARKER_ROW: f32 = 12.0;

/// Section colours (readable on light and dark themes).
const GLOBAL: Color32 = Color32::from_rgb(96, 110, 130);
const CUSTOM: Color32 = Color32::from_rgb(52, 120, 210);
const NO_FACES: Color32 = Color32::from_rgb(150, 150, 150);
const CUT: Color32 = Color32::from_rgb(205, 64, 64);
/// Where a face marked visible stays uncovered.
const VISIBLE: Color32 = Color32::from_rgb(60, 200, 90);
const STRIP: f32 = 6.0;

/// Outcome of one visible-face mark: `None` while tracking is not up to
/// date, else the span where the face stays visible and whether it is lost.
type MarkSpan = Option<(Option<(f64, f64)>, bool)>;

/// Formats seconds as `m:ss.cc` (or `h:mm:ss.cc`).
pub fn format_time(t: f64) -> String {
    let cs = (t.max(0.0) * 100.0).round() as u64;
    let (h, m, s, c) = (cs / 360_000, cs / 6000 % 60, cs / 100 % 60, cs % 100);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}.{c:02}")
    } else {
        format!("{m}:{s:02}.{c:02}")
    }
}

/// Frame geometry of a video.
#[derive(Debug, Clone, Copy)]
struct Clock {
    fps: f64,
    /// Time of the last frame.
    last: f64,
}

impl Clock {
    fn new(info: &VideoInfo) -> Self {
        let fps = if info.frame_rate > 0.0 { info.frame_rate } else { 30.0 };
        let frames = info.frames.unwrap_or_else(|| (info.duration * fps).round() as u64).max(1);
        Self { fps, last: (frames - 1) as f64 / fps }
    }

    fn frame(&self, t: f64) -> u64 {
        (t.max(0.0) * self.fps).round() as u64
    }

    /// Snaps `t` to the nearest frame inside the video.
    fn snap(&self, t: f64) -> f64 {
        (self.frame(t) as f64 / self.fps).min(self.last)
    }

    /// Track length: the end of the last frame.
    fn length(&self) -> f64 {
        self.last + 1.0 / self.fps
    }
}

fn section_color(k: Option<&Keyframe>) -> Color32 {
    match k {
        None => GLOBAL,
        Some(k) if k.cut => CUT,
        Some(k) if k.faces.is_none() => NO_FACES,
        Some(_) => CUSTOM,
    }
}

/// Draws the timeline of a video item. `tracking` is the progress of a
/// running tracking pass of its visible faces.
pub fn show(ui: &mut egui::Ui, item: &mut Item, settings: &Settings, tracking: Option<f32>) {
    let info = match &item.info {
        None => {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.weak("Reading the video…");
            });
            return;
        }
        Some(Err(e)) => {
            ui.colored_label(ui.visuals().error_fg_color, format!("Cannot read the video: {e}"));
            return;
        }
        Some(Ok(info)) => *info,
    };
    let clock = Clock::new(&info);
    let global = settings.effective_options();
    item.position = clock.snap(item.position);

    // Tracks of the visible faces, if computed for the current edits.
    let key = (item.edits.clone(), settings.preview_options());
    let tracks = item.tracks.as_ref().filter(|t| t.key == key).map(|t| &t.result);
    let error = tracks.and_then(|r| r.as_ref().err()).cloned();
    let spans: Vec<MarkSpan> = (0..item.edits.visible.len())
        .map(|m| {
            let t = tracks?.as_ref().ok()?.iter().find(|t| t.mark == m)?;
            Some((t.span, t.lost))
        })
        .collect();

    transport(ui, item, &clock);
    track(ui, item, &clock, &spans);
    legend(ui);
    ui.separator();
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        visible_faces(ui, item, &spans, tracking, error.as_deref());
        ui.separator();
        section_editor(ui, item, &clock, settings, &global);
    });
}

/// The faces marked to stay visible, with where they are followed.
fn visible_faces(ui: &mut egui::Ui, item: &mut Item, spans: &[MarkSpan], tracking: Option<f32>, error: Option<&str>) {
    ui.horizontal(|ui| {
        ui.strong("Faces left visible");
        if let Some(p) = tracking {
            ui.spinner();
            ui.weak(format!("following the marked faces… {:.0}%", p * 100.0));
        }
    });
    if let Some(e) = error {
        ui.colored_label(ui.visuals().error_fg_color, format!("Tracking failed: {e}"));
    }
    if item.edits.visible.is_empty() {
        ui.weak(
            "Every face is covered. Click a face in the preview to leave it visible from this frame on: it is \
             followed until it leaves the picture, then covered again.",
        );
        return;
    }
    let mut remove = None;
    for (i, m) in item.edits.visible.iter().enumerate() {
        ui.horizontal_wrapped(|ui| {
            ui.label(format!("Face {} from {}", i + 1, format_time(m.time)));
            match spans.get(i).copied().flatten() {
                None => {
                    ui.weak("…");
                }
                Some((None, _)) => {
                    ui.colored_label(ui.visuals().error_fg_color, "no face found at the mark: stays covered");
                }
                Some((Some((a, b)), lost)) => {
                    let mut text = format!("visible {} – {}", format_time(a), format_time(b));
                    if lost {
                        text += ", then lost and covered again";
                    }
                    ui.colored_label(VISIBLE, text);
                }
            }
            if ui.small_button("Go to").clicked() {
                item.position = m.time;
            }
            if ui.small_button("Remove").on_hover_text("Cover this face again").clicked() {
                remove = Some(i);
            }
        });
    }
    if let Some(i) = remove {
        item.edits.visible.remove(i);
    }
    ui.weak("Scrub through the video to check: a lost face is covered again; click it to add a new mark.");
}

/// Frame stepping, keyframe navigation and the time readout.
fn transport(ui: &mut egui::Ui, item: &mut Item, clock: &Clock) {
    let step = 1.0 / clock.fps;
    let mut target = None;
    ui.horizontal(|ui| {
        let keys: Vec<f64> = item.edits.timeline.keyframes().iter().map(|k| k.time).collect();
        let pos = item.position;
        let prev_key = keys.iter().rev().find(|&&t| t < pos - step / 2.0).copied();
        let next_key = keys.iter().find(|&&t| t > pos + step / 2.0).copied();
        if icon_button(ui, Icon::Start, true, "Start (Home)") {
            target = Some(0.0);
        }
        if icon_button(ui, Icon::PrevKey, prev_key.is_some(), "Previous keyframe") {
            target = prev_key;
        }
        if icon_button(ui, Icon::PrevFrame, true, "Previous frame (←, Shift+← one second)") {
            target = Some(pos - step);
        }
        ui.monospace(format!("{} / {}", format_time(pos), format_time(clock.length())));
        ui.weak(format!("frame {}", clock.frame(pos)));
        if icon_button(ui, Icon::NextFrame, true, "Next frame (→, Shift+→ one second)") {
            target = Some(pos + step);
        }
        if icon_button(ui, Icon::NextKey, next_key.is_some(), "Next keyframe") {
            target = next_key;
        }
        if icon_button(ui, Icon::End, true, "End (End)") {
            target = Some(clock.last);
        }
    });

    // Keyboard, unless a text field has the focus.
    if !ui.ctx().egui_wants_keyboard_input() {
        ui.input(|i| {
            let big = if i.modifiers.shift { 1.0 } else { step };
            if i.key_pressed(egui::Key::ArrowLeft) {
                target = Some(item.position - big);
            }
            if i.key_pressed(egui::Key::ArrowRight) {
                target = Some(item.position + big);
            }
            if i.key_pressed(egui::Key::Home) {
                target = Some(0.0);
            }
            if i.key_pressed(egui::Key::End) {
                target = Some(clock.last);
            }
        });
    }
    if let Some(t) = target {
        item.position = clock.snap(t.clamp(0.0, clock.last));
    }
}

/// Transport icons, drawn with shapes (the default fonts lack media glyphs).
#[derive(Clone, Copy, PartialEq)]
enum Icon {
    Start,
    PrevKey,
    PrevFrame,
    NextFrame,
    NextKey,
    End,
}

/// A small button with a drawn icon; returns whether it was clicked.
fn icon_button(ui: &mut egui::Ui, icon: Icon, enabled: bool, hint: &str) -> bool {
    let resp = ui.add_enabled(enabled, egui::Button::new("").min_size(Vec2::new(26.0, 20.0))).on_hover_text(hint);
    let color = ui.style().interact(&resp).fg_stroke.color;
    let color = if enabled { color } else { color.gamma_multiply(0.4) };
    let painter = ui.painter();
    let c = resp.rect.center();
    // Triangle pointing left (`dir` = -1) or right (1), centred on `x`.
    let triangle = |x: f32, dir: f32| {
        let pts = vec![
            Pos2::new(x + 4.0 * dir, c.y),
            Pos2::new(x - 3.0 * dir, c.y - 5.0),
            Pos2::new(x - 3.0 * dir, c.y + 5.0),
        ];
        egui::Shape::convex_polygon(pts, color, Stroke::NONE)
    };
    let bar =
        |x: f32| egui::Shape::line_segment([Pos2::new(x, c.y - 5.0), Pos2::new(x, c.y + 5.0)], Stroke::new(2.0, color));
    let diamond = |x: f32| {
        let r = 4.0;
        let pts = vec![Pos2::new(x, c.y - r), Pos2::new(x + r, c.y), Pos2::new(x, c.y + r), Pos2::new(x - r, c.y)];
        egui::Shape::convex_polygon(pts, color, Stroke::NONE)
    };
    let shapes = match icon {
        Icon::Start => vec![bar(c.x - 5.0), triangle(c.x + 1.0, -1.0)],
        Icon::PrevKey => vec![diamond(c.x - 5.0), triangle(c.x + 4.0, -1.0)],
        Icon::PrevFrame => vec![triangle(c.x, -1.0)],
        Icon::NextFrame => vec![triangle(c.x, 1.0)],
        Icon::NextKey => vec![triangle(c.x - 4.0, 1.0), diamond(c.x + 5.0)],
        Icon::End => vec![triangle(c.x - 1.0, 1.0), bar(c.x + 5.0)],
    };
    painter.extend(shapes);
    resp.clicked()
}

/// The scrubber: sections, keyframe markers and the playhead.
fn track(ui: &mut egui::Ui, item: &mut Item, clock: &Clock, spans: &[MarkSpan]) {
    let width = ui.available_width();
    let strip = if item.edits.visible.is_empty() { 0.0 } else { STRIP + 2.0 };
    let (rect, resp) =
        ui.allocate_exact_size(Vec2::new(width, MARKER_ROW + TRACK_HEIGHT + 4.0 + strip), Sense::click_and_drag());
    let bar = Rect::from_min_size(rect.min + Vec2::new(0.0, MARKER_ROW), Vec2::new(rect.width(), TRACK_HEIGHT));
    let length = clock.length();
    let x_of = |t: f64| bar.left() + (t / length).clamp(0.0, 1.0) as f32 * bar.width();
    let t_of = |x: f32| ((x - bar.left()) / bar.width()).clamp(0.0, 1.0) as f64 * length;
    let painter = ui.painter_at(rect.expand(2.0));
    let visuals = ui.visuals();

    // Sections.
    let keys = item.edits.timeline.keyframes();
    let first = keys.first().map_or(length, |k| k.time);
    painter.rect_filled(Rect::from_x_y_ranges(bar.left()..=x_of(first), bar.y_range()), 2.0, GLOBAL);
    for (i, k) in keys.iter().enumerate() {
        let end = keys.get(i + 1).map_or(length, |n| n.time);
        let r = Rect::from_x_y_ranges(x_of(k.time)..=x_of(end), bar.y_range());
        painter.rect_filled(r, 0.0, section_color(Some(k)));
        if k.cut {
            // Hatching, so cuts do not rely on colour alone.
            let hatch = painter.with_clip_rect(r);
            let mut x = r.left() - r.height();
            while x < r.right() {
                let line = [Pos2::new(x, r.bottom()), Pos2::new(x + r.height(), r.top())];
                hatch.line_segment(line, Stroke::new(1.0, Color32::from_black_alpha(90)));
                x += 6.0;
            }
        }
    }
    painter.rect_stroke(bar, 2.0, visuals.widgets.noninteractive.bg_stroke, egui::StrokeKind::Inside);

    // Keyframe markers; the one starting the current section is highlighted.
    let current = item.edits.timeline.index_at(item.position + 0.5 / clock.fps);
    let mut clicked_key = None;
    let pointer = resp.interact_pointer_pos().or(resp.hover_pos());
    for (i, k) in keys.iter().enumerate() {
        let x = x_of(k.time);
        let c = Pos2::new(x, rect.top() + MARKER_ROW / 2.0);
        let r = 5.0;
        let diamond =
            vec![c + Vec2::new(0.0, -r), c + Vec2::new(r, 0.0), c + Vec2::new(0.0, r), c + Vec2::new(-r, 0.0)];
        let fill =
            if Some(i) == current { visuals.selection.bg_fill } else { visuals.widgets.inactive.fg_stroke.color };
        painter.add(egui::Shape::convex_polygon(diamond, fill, Stroke::new(1.0, visuals.extreme_bg_color)));
        painter.line_segment([Pos2::new(x, bar.top()), Pos2::new(x, bar.bottom())], Stroke::new(1.0, fill));
        if resp.clicked() && pointer.is_some_and(|p| p.y < bar.top() && (p.x - x).abs() <= r + 2.0) {
            clicked_key = Some(k.time);
        }
    }

    // Where the faces marked visible stay uncovered, and the marks.
    if strip > 0.0 {
        let y = bar.bottom() + 3.0;
        for (m, span) in item.edits.visible.iter().zip(spans) {
            if let Some((Some((a, b)), _)) = span {
                let r = Rect::from_x_y_ranges(x_of(*a)..=x_of(b + 1.0 / clock.fps).max(x_of(*a) + 2.0), y..=y + STRIP);
                painter.rect_filled(r, 1.0, VISIBLE);
            }
            painter.circle_filled(Pos2::new(x_of(m.time), y + STRIP / 2.0), 3.0, VISIBLE);
        }
    }

    // Playhead.
    let x = x_of(item.position);
    let head = visuals.strong_text_color();
    painter.line_segment([Pos2::new(x, rect.top()), Pos2::new(x, bar.bottom() + 3.0)], Stroke::new(2.0, head));

    if let Some(t) = clicked_key {
        item.position = t;
    } else if resp.dragged() || resp.clicked() {
        if let Some(p) = pointer {
            item.position = clock.snap(t_of(p.x));
        }
    }
    if let Some(p) = resp.hover_pos() {
        let t = clock.snap(t_of(p.x));
        resp.on_hover_text_at_pointer(format!("{} · frame {}", format_time(t), clock.frame(t)));
    }
}

fn legend(ui: &mut egui::Ui) {
    ui.horizontal_wrapped(|ui| {
        for (color, label) in [
            (GLOBAL, "Global settings"),
            (CUSTOM, "Section settings"),
            (NO_FACES, "Faces not covered"),
            (CUT, "Cut"),
            (VISIBLE, "Face left visible"),
        ] {
            let (r, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
            ui.painter().rect_filled(r, 2.0, color);
            ui.small(label);
            ui.add_space(6.0);
        }
    });
}

/// Settings of the section under the playhead, and keyframe actions.
fn section_editor(ui: &mut egui::Ui, item: &mut Item, clock: &Clock, settings: &Settings, global: &Options) {
    let epsilon = 0.5 / clock.fps;
    let pos = item.position;
    let current = item.edits.timeline.index_at(pos + epsilon);
    let keys = item.edits.timeline.keyframes();
    let start = current.map_or(0.0, |i| keys[i].time);
    let end = current.map_or_else(|| keys.first().map(|k| k.time), |i| keys.get(i + 1).map(|k| k.time));
    let on_key = current.is_some_and(|i| (keys[i].time - pos).abs() <= epsilon);
    let range = format!("{} – {}", format_time(start), end.map_or("end".into(), format_time));

    ui.horizontal_wrapped(|ui| {
        ui.strong(match current {
            None => format!("Section {range}: global settings"),
            Some(_) => format!("Section {range}"),
        });
        if !on_key {
            let add = ui
                .button(format!("Add keyframe at {}", format_time(pos)))
                .on_hover_text("Start a new section here, with the settings currently in effect; change them below");
            if add.clicked() {
                // Same settings as the current section (including the face
                // options of a cut section, ready if it is switched back).
                let keyframe = match current {
                    Some(i) => Keyframe { time: pos, ..item.edits.timeline.keyframes()[i].clone() },
                    None => Keyframe::from_options(pos, global),
                };
                item.edits.timeline.insert(keyframe, epsilon);
            }
        }
    });

    let Some(index) = item.edits.timeline.index_at(pos + epsilon) else {
        ui.weak("Before the first keyframe the settings of the left panel apply. Add a keyframe to change them from a point of the video, or to cut a part out.");
        clear_all(ui, &mut item.edits.timeline);
        return;
    };

    let mut delete = false;
    let mut move_here = false;
    ui.horizontal_wrapped(|ui| {
        if ui.button("Delete keyframe").on_hover_text("The previous section extends over this one").clicked() {
            delete = true;
        }
        if !on_key && ui.button("Move keyframe here").clicked() {
            move_here = true;
        }
    });

    let k = item.edits.timeline.get_mut(index).expect("keyframe index");
    ui.horizontal(|ui| {
        ui.radio_value(&mut k.cut, false, "Process");
        ui.radio_value(&mut k.cut, true, "Cut from the output")
            .on_hover_text("Removes these frames and the matching audio from the output");
    });
    if !k.cut {
        let mut cover = k.faces.is_some();
        ui.checkbox(&mut cover, "Cover faces");
        if cover != k.faces.is_some() {
            k.faces = cover.then(|| settings.face.clone());
        }
        if let Some(f) = &mut k.faces {
            ui.indent("keyframe-faces", |ui| settings::face_controls(ui, f, "keyframe"));
        }
        ui.add_enabled_ui(settings.timestamp_enabled, |ui| {
            ui.checkbox(&mut k.timestamp, "Show the timestamp")
                .on_disabled_hover_text("Enable the timestamp in the left panel first");
        });
    }

    if delete {
        item.edits.timeline.remove(index);
    } else if move_here {
        if let Some(mut k) = item.edits.timeline.remove(index) {
            k.time = pos;
            item.edits.timeline.insert(k, epsilon);
        }
    }
    clear_all(ui, &mut item.edits.timeline);
}

fn clear_all(ui: &mut egui::Ui, timeline: &mut Timeline) {
    if timeline.is_empty() {
        return;
    }
    ui.add_space(6.0);
    if ui.small_button(format!("Remove all {} keyframes", timeline.keyframes().len())).clicked() {
        *timeline = Timeline::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_are_formatted() {
        assert_eq!(format_time(0.0), "0:00.00");
        assert_eq!(format_time(75.456), "1:15.46");
        assert_eq!(format_time(3725.0), "1:02:05.00");
    }

    #[test]
    fn clock_snaps_to_frames() {
        let c = Clock::new(&VideoInfo { duration: 1.0, frame_rate: 25.0, frames: Some(25), width: 1, height: 1 });
        assert_eq!(c.snap(0.43), 0.44);
        assert_eq!(c.snap(5.0), 0.96, "clamped to the last frame");
        assert_eq!(c.frame(0.96), 24);
        assert!((c.length() - 1.0).abs() < 1e-9);
    }
}
