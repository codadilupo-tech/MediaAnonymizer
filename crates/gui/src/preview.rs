//! Original vs. redacted preview, rendered in a background thread.
//!
//! Requests are debounced: the preview is only recomputed once the selection
//! and the face options have been stable for a short while, so dragging a
//! slider does not queue dozens of detections.
//!
//! The image can be zoomed (scroll wheel, pinch) and panned (drag); the view
//! is shared by the "Redacted" and "Original" tabs so that both show the same
//! region, and kept while scrubbing through a video.
//!
//! Videos are previewed one frame at a time: the frame under the timeline's
//! playhead, with the settings of its section.

use eframe::egui;
use image::DynamicImage;
use mediaanon_core::{FaceBox, Options, PreviewFace, Processor, Timeline, VisibleFace};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant, SystemTime};

const DEBOUNCE: Duration = Duration::from_millis(350);
/// Shorter wait for video frames, so that scrubbing feels live (the render
/// thread always skips to the latest request anyway).
const DEBOUNCE_VIDEO: Duration = Duration::from_millis(120);
/// Longest side of preview images: large enough to zoom in on a face.
pub const MAX_SIDE: u32 = 2048;
/// Maximum zoom, relative to "fit in the panel".
const MAX_ZOOM: f32 = 32.0;

/// Photo shown when nothing is selected, so the settings can still be tuned
/// on a real face.
static SAMPLE: &[u8] = include_bytes!("../assets/sample.jpg");

/// What is previewed.
#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    /// A photo, with the faces marked visible.
    File { path: PathBuf, visible: Vec<VisibleFace> },
    /// The bundled sample photo.
    Sample { visible: Vec<VisibleFace> },
    /// The frame of a video at `time` seconds; `keep` are the positions of
    /// the tracked visible faces on that frame.
    Video { path: PathBuf, time: f64, timeline: Timeline, keep: Vec<FaceBox> },
}

impl Source {
    /// Whether two sources show the same picture (frames of one video do).
    fn same_picture(&self, other: &Source) -> bool {
        match (self, other) {
            (Source::File { path: a, .. }, Source::File { path: b, .. }) => a == b,
            (Source::Video { path: a, .. }, Source::Video { path: b, .. }) => a == b,
            (Source::Sample { .. }, Source::Sample { .. }) => true,
            _ => false,
        }
    }
}

/// What a preview depends on.
#[derive(Debug, Clone, PartialEq)]
struct Key {
    source: Source,
    options: Options,
}

/// A rendered preview.
struct Images {
    original: DynamicImage,
    redacted: DynamicImage,
    /// Faces found, `None` when face redaction does not apply.
    faces: Option<Vec<PreviewFace>>,
    /// The video frame is in a cut section.
    cut: bool,
}

struct Rendered {
    key: Key,
    result: Result<Images, String>,
}

/// Zoom and pan of the preview.
#[derive(Debug, Clone, Copy)]
struct View {
    /// 1 = the whole image fits the panel.
    zoom: f32,
    /// Offset of the image centre from the panel centre, in points.
    offset: egui::Vec2,
}

impl Default for View {
    fn default() -> Self {
        Self { zoom: 1.0, offset: egui::Vec2::ZERO }
    }
}

/// Preview state owned by the UI thread.
pub struct Preview {
    requests: Sender<Key>,
    results: Receiver<Rendered>,
    /// Latest wanted key and when it last changed.
    wanted: Option<(Key, Instant)>,
    sent: Option<Key>,
    shown: Option<Key>,
    original: Option<egui::TextureHandle>,
    redacted: Option<egui::TextureHandle>,
    faces: Option<Vec<PreviewFace>>,
    cut: bool,
    error: Option<String>,
    view: View,
    pub show_original: bool,
    /// Draw the outlines of the detected faces.
    pub show_faces: bool,
}

impl Preview {
    pub fn new(ctx: egui::Context) -> Self {
        let (requests, rx) = channel::<Key>();
        let (tx, results) = channel();
        std::thread::Builder::new()
            .name("mediaanon-preview".into())
            .spawn(move || render_loop(rx, tx, ctx))
            .expect("cannot spawn preview thread");
        Self {
            requests,
            results,
            wanted: None,
            sent: None,
            shown: None,
            original: None,
            redacted: None,
            faces: None,
            cut: false,
            error: None,
            view: View::default(),
            show_original: false,
            show_faces: true,
        }
    }

    /// Declares what should be previewed (`None` clears the preview).
    pub fn want(&mut self, source: Option<Source>, options: Options) {
        let key = source.map(|source| Key { source, options });
        match (&key, &self.wanted) {
            (Some(k), Some((w, _))) if k == w => {}
            (Some(k), _) => self.wanted = Some((k.clone(), Instant::now())),
            (None, _) => {
                self.wanted = None;
                self.shown = None;
                self.original = None;
                self.redacted = None;
                self.error = None;
            }
        }
    }

    /// Whether a rendered preview for the current selection is on screen.
    pub fn is_ready(&self) -> bool {
        self.wanted.as_ref().is_some_and(|(k, _)| self.shown.as_ref() == Some(k))
    }

    /// Sends pending requests and collects results; call every frame.
    pub fn update(&mut self, ctx: &egui::Context) {
        if let Some((key, since)) = &self.wanted {
            if self.sent.as_ref() != Some(key) {
                let debounce = if matches!(key.source, Source::Video { .. }) { DEBOUNCE_VIDEO } else { DEBOUNCE };
                let wait = debounce.saturating_sub(since.elapsed());
                if wait.is_zero() {
                    if self.requests.send(key.clone()).is_ok() {
                        self.sent = Some(key.clone());
                    }
                } else {
                    ctx.request_repaint_after(wait);
                }
            }
        }
        while let Ok(r) = self.results.try_recv() {
            if self.wanted.as_ref().map(|(k, _)| k) != Some(&r.key) {
                continue; // stale
            }
            match r.result {
                Ok(images) => {
                    self.original = Some(texture(ctx, "preview-original", &images.original));
                    self.redacted = Some(texture(ctx, "preview-redacted", &images.redacted));
                    self.faces = images.faces;
                    self.cut = images.cut;
                    self.error = None;
                }
                Err(e) => {
                    self.original = None;
                    self.redacted = None;
                    self.error = Some(e);
                }
            }
            // A different picture starts fitted; new settings keep the view.
            if !self.shown.as_ref().is_some_and(|k| k.source.same_picture(&r.key.source)) {
                self.view = View::default();
            }
            self.shown = Some(r.key);
        }
    }

    /// Draws the preview; returns the face the user clicked, if any.
    pub fn show(&mut self, ui: &mut egui::Ui) -> Option<PreviewFace> {
        let Some((wanted, _)) = &self.wanted else {
            ui.weak("Enable face redaction or the timestamp to see a preview.");
            return None;
        };
        if matches!(wanted.source, Source::Sample { .. }) {
            ui.weak("Sample photo: select a photo or a video in the queue to preview it.");
        }
        if let Some(e) = &self.error {
            ui.colored_label(ui.visuals().error_fg_color, e);
            return None;
        }
        let busy = !self.is_ready();
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.show_original, false, "Redacted");
            ui.selectable_value(&mut self.show_original, true, "Original");
            if busy {
                ui.spinner();
            } else if self.cut {
                ui.colored_label(ui.visuals().error_fg_color, "Cut: not in the output");
            } else if let Some(faces) = &self.faces {
                let n = faces.len();
                let visible = faces.iter().filter(|f| f.visible).count();
                let mut text = format!("{n} face{} found", if n == 1 { "" } else { "s" });
                if visible > 0 {
                    text += &format!(", {visible} left visible");
                }
                ui.label(text);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add_enabled(self.view.zoom > 1.0, egui::Button::new("Fit").small()).clicked() {
                    self.view = View::default();
                }
                ui.weak(format!("{:.0}%", self.view.zoom * 100.0))
                    .on_hover_text("Scroll or pinch to zoom, drag to pan, double-click to fit");
                ui.checkbox(&mut self.show_faces, "Faces")
                    .on_hover_text("Outline the detected faces; click a face to leave it visible or cover it again");
            });
        });
        let tex = if self.show_original { &self.original } else { &self.redacted };
        let t = tex.clone()?;
        let cut = self.cut && !self.show_original;
        self.canvas(ui, &t, cut)
    }

    /// Draws the zoomable, pannable image in the remaining space.
    fn canvas(&mut self, ui: &mut egui::Ui, tex: &egui::TextureHandle, cut: bool) -> Option<PreviewFace> {
        let (rect, resp) = ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());
        let size = tex.size_vec2();
        if rect.width() <= 0.0 || rect.height() <= 0.0 || size.x <= 0.0 || size.y <= 0.0 {
            return None;
        }
        let fit = (rect.width() / size.x).min(rect.height() / size.y);
        // Face under the pointer, from the previous layout (good enough for
        // hit testing; the image moves only while dragging or zooming).
        let faces: Vec<PreviewFace> =
            if self.show_faces && !cut { self.faces.clone().unwrap_or_default() } else { Vec::new() };
        let view = &mut self.view;
        let image_rect_of = |view: &View| {
            let shown = size * fit * view.zoom;
            egui::Rect::from_center_size(rect.center() + view.offset, shown)
        };
        let face_rect = |r: egui::Rect, b: &FaceBox| {
            egui::Rect::from_min_size(
                r.min + egui::vec2(b.x * r.width(), b.y * r.height()),
                egui::vec2(b.width * r.width(), b.height * r.height()),
            )
        };
        let hovered = resp.hover_pos().and_then(|p| {
            let r = image_rect_of(view);
            faces
                .iter()
                .enumerate()
                .filter(|(_, f)| face_rect(r, &f.bbox).contains(p))
                .min_by(|a, b| (a.1.bbox.width * a.1.bbox.height).total_cmp(&(b.1.bbox.width * b.1.bbox.height)))
                .map(|(i, _)| i)
        });

        if resp.double_clicked() && hovered.is_none() {
            *view = View::default();
        }
        if resp.dragged() {
            view.offset += resp.drag_delta();
        }
        if let Some(pointer) = resp.hover_pos() {
            let (scroll, pinch) = ui.input(|i| (i.smooth_scroll_delta.y, i.zoom_delta()));
            let factor = (scroll / 400.0).exp() * pinch;
            if factor != 1.0 {
                let zoom = (view.zoom * factor).clamp(1.0, MAX_ZOOM);
                // Keep the image point under the cursor where it is.
                let centre = rect.center() + view.offset;
                let under = (pointer - centre) / view.zoom;
                view.offset = pointer - under * zoom - rect.center();
                view.zoom = zoom;
            }
        }
        // Do not let the image leave the panel.
        let shown = size * fit * view.zoom;
        let max = ((shown - rect.size()) / 2.0).max(egui::Vec2::ZERO);
        view.offset = view.offset.clamp(-max, max);

        let image_rect = image_rect_of(view);
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        let painter = ui.painter_at(rect);
        // Cut frames are dimmed: they will not be in the output.
        let tint = if cut { egui::Color32::from_gray(70) } else { egui::Color32::WHITE };
        painter.image(tex.id(), image_rect, uv, tint);
        if cut {
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "CUT",
                egui::FontId::proportional(32.0),
                egui::Color32::from_rgb(230, 80, 80),
            );
        }

        // Face outlines: numbered, green when left visible.
        for (i, f) in faces.iter().enumerate() {
            let r = face_rect(image_rect, &f.bbox);
            let (color, width) = if f.visible {
                (egui::Color32::from_rgb(60, 200, 90), 2.5)
            } else {
                (egui::Color32::from_white_alpha(200), 1.5)
            };
            let width = if hovered == Some(i) { width + 1.5 } else { width };
            painter.rect_stroke(r, 3.0, egui::Stroke::new(width, color), egui::StrokeKind::Outside);
            let label = if f.visible { format!("{} visible", i + 1) } else { format!("{}", i + 1) };
            let galley = painter.layout_no_wrap(label, egui::FontId::proportional(12.0), egui::Color32::BLACK);
            let tag = egui::Rect::from_min_size(
                r.left_top() - egui::vec2(0.0, galley.size().y + 4.0),
                galley.size() + egui::vec2(6.0, 4.0),
            );
            painter.rect_filled(tag, 2.0, color);
            painter.galley(tag.min + egui::vec2(3.0, 2.0), galley, egui::Color32::BLACK);
        }

        let clicked = hovered.filter(|_| resp.clicked()).map(|i| faces[i]);
        match hovered.map(|i| faces[i].visible) {
            Some(visible) => {
                let hint = if visible { "Click to cover this face again" } else { "Click to leave this face visible" };
                resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text_at_pointer(hint);
            }
            None if view.zoom > 1.0 => {
                let icon = if resp.dragged() { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Grab };
                resp.on_hover_cursor(icon);
            }
            None => {}
        }
        clicked
    }
}

fn texture(ctx: &egui::Context, name: &str, img: &DynamicImage) -> egui::TextureHandle {
    let rgba = img.to_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    // Nearest-neighbour when zoomed in, so pixelation looks like the output.
    let options = egui::TextureOptions {
        magnification: egui::TextureFilter::Nearest,
        minification: egui::TextureFilter::Linear,
        ..Default::default()
    };
    ctx.load_texture(name, egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()), options)
}

/// Decodes an image upright and downscaled.
fn load_original(data: &[u8]) -> Result<DynamicImage, String> {
    use image::ImageDecoder;
    let reader = image::ImageReader::new(std::io::Cursor::new(data));
    let reader = reader.with_guessed_format().map_err(|e| e.to_string())?;
    let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
    let orientation = decoder.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    img.apply_orientation(orientation);
    Ok(img.thumbnail(MAX_SIDE, MAX_SIDE))
}

/// Reads a photo source; also returns the time used when the picture has no
/// capture date.
fn read(path: Option<&Path>) -> Result<(std::borrow::Cow<'static, [u8]>, Option<SystemTime>), String> {
    match path {
        None => Ok((SAMPLE.into(), Some(SystemTime::now()))),
        Some(path) => {
            let data = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            Ok((data.into(), modified(path)))
        }
    }
}

/// Renders one request with a ready processor.
fn render(processor: &Processor, key: &Key) -> Result<Images, String> {
    let photo = |path: Option<&Path>, visible: &[VisibleFace]| {
        let (data, time) = read(path)?;
        let original = load_original(&data)?;
        let p = processor.preview_data(&data, time, visible, MAX_SIDE).map_err(|e| e.to_string())?;
        Ok(Images { original, redacted: p.image, faces: key.options.faces.as_ref().map(|_| p.faces), cut: false })
    };
    match &key.source {
        Source::Sample { visible } => photo(None, visible),
        Source::File { path, visible } => photo(Some(path), visible),
        Source::Video { path, time, timeline, keep } => {
            let frame =
                processor.preview_video_frame(path, *time, timeline, keep, MAX_SIDE).map_err(|e| e.to_string())?;
            let section = timeline.section_at(*time, &key.options);
            Ok(Images {
                original: frame.original,
                redacted: frame.redacted,
                faces: section.faces.map(|_| frame.faces),
                cut: frame.cut,
            })
        }
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

fn render_loop(rx: Receiver<Key>, tx: Sender<Rendered>, ctx: egui::Context) {
    // The processor (and its detector) is reused while the options are unchanged.
    let mut cached: Option<(Options, Processor)> = None;
    while let Ok(mut key) = rx.recv() {
        // Only the most recent request matters.
        while let Ok(newer) = rx.try_recv() {
            key = newer;
        }
        if cached.as_ref().is_none_or(|(o, _)| *o != key.options) {
            cached = None;
            match Processor::new(key.options.clone()) {
                Ok(p) => cached = Some((key.options.clone(), p)),
                Err(e) => {
                    let _ = tx.send(Rendered { key, result: Err(e.to_string()) });
                    ctx.request_repaint();
                    continue;
                }
            }
        }
        let processor = &cached.as_ref().expect("processor was just created").1;
        let result = render(processor, &key);
        if tx.send(Rendered { key, result }).is_err() {
            break;
        }
        ctx.request_repaint();
    }
}
