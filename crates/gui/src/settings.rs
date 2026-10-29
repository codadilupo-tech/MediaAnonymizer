//! Persisted user settings and the "Settings" side panel.

use eframe::egui;
use mediaanon_core::{
    Corner, DeviceInfo, EncoderBackend, FaceMode, FaceOptions, FaceShape, HwAccel, InferenceDevice, Keyframe,
    MetadataPolicy, Options, RedactionStyle, TimestampOptions, VideoQuality,
};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Where processed files are written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutputMode {
    /// Next to the original, with a suffix before the extension.
    Suffix,
    /// Into a chosen folder, keeping the file name.
    Folder,
}

/// Everything the user can configure, saved between runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Processing options; `options.faces` / `options.timestamp` are ignored
    /// in favour of the `*_enabled` flags + their settings, so that toggling a
    /// feature off does not lose its settings.
    pub options: Options,
    pub faces_enabled: bool,
    pub face: FaceOptions,
    pub timestamp_enabled: bool,
    pub timestamp: TimestampOptions,
    pub output_mode: OutputMode,
    pub suffix: String,
    pub folder: Option<PathBuf>,
    pub overwrite: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            options: Options { metadata: MetadataPolicy::All, ..Default::default() },
            faces_enabled: true,
            face: FaceOptions::default(),
            timestamp_enabled: false,
            timestamp: TimestampOptions::default(),
            output_mode: OutputMode::Suffix,
            suffix: ".anon".into(),
            folder: None,
            overwrite: false,
        }
    }
}

impl Settings {
    /// Options handed to the processor.
    pub fn effective_options(&self) -> Options {
        Options {
            faces: self.faces_enabled.then(|| self.face.clone()),
            timestamp: self.timestamp_enabled.then(|| self.timestamp.clone()),
            ..self.options.clone()
        }
    }

    /// Options for previews: same pixel changes. Previews never touch
    /// metadata; the policy is only set so that the options are valid even
    /// when nothing changes the pixels (a video frame is still shown).
    pub fn preview_options(&self) -> Options {
        Options { metadata: MetadataPolicy::All, ..self.effective_options() }
    }

    /// Whether the selected photo changes visually (worth a preview).
    pub fn changes_pixels(&self) -> bool {
        self.faces_enabled || self.timestamp_enabled
    }

    /// Returns a user-facing message if the current settings cannot run.
    pub fn validation_error(&self) -> Option<String> {
        if let Err(e) = self.effective_options().validate() {
            return Some(e.to_string());
        }
        match self.output_mode {
            OutputMode::Suffix if self.suffix.is_empty() => Some("the suffix must not be empty".into()),
            OutputMode::Folder if self.folder.is_none() => Some("choose an output folder".into()),
            _ => None,
        }
    }

    /// Output path for an input file.
    pub fn output_for(&self, input: &Path) -> PathBuf {
        match (&self.output_mode, &self.folder) {
            (OutputMode::Folder, Some(dir)) => dir.join(input.file_name().unwrap_or_default()),
            _ => {
                let stem = input.file_stem().unwrap_or_default().to_string_lossy();
                let name = match input.extension() {
                    Some(ext) => format!("{stem}{}.{}", self.suffix, ext.to_string_lossy()),
                    None => format!("{stem}{}", self.suffix),
                };
                input.with_file_name(name)
            }
        }
    }
}

/// Machine capabilities shown in the settings panel.
#[derive(Debug, Default)]
pub struct Environment {
    pub devices: Vec<DeviceInfo>,
    pub hwaccels: Vec<String>,
    /// Problems detecting ONNX Runtime / FFmpeg.
    pub errors: Vec<String>,
}

impl Environment {
    pub fn detect() -> Self {
        let mut env = Environment::default();
        match mediaanon_core::available_devices() {
            Ok(d) => env.devices = d,
            Err(e) => env.errors.push(e.to_string()),
        }
        match mediaanon_core::video::capabilities() {
            Ok(c) => env.hwaccels = c.hwaccels.clone(),
            Err(e) => env.errors.push(format!("{e} (videos cannot be processed)")),
        }
        env
    }

    /// `None` if available, otherwise why not.
    fn device_problem(&self, d: InferenceDevice) -> Option<String> {
        if d == InferenceDevice::Auto {
            return None;
        }
        match self.devices.iter().find(|i| i.device == d) {
            Some(i) if i.available => None,
            Some(i) if !i.feature.is_empty() => {
                Some(format!("Not available: build with `--features {}` and a matching ONNX Runtime", i.feature))
            }
            _ => Some("Not available on this machine".into()),
        }
    }

    fn hwaccel_available(&self, h: HwAccel) -> bool {
        match h {
            HwAccel::None | HwAccel::Auto => true,
            other => {
                let name = match other {
                    HwAccel::Cuda => "cuda",
                    HwAccel::Vaapi => "vaapi",
                    HwAccel::Qsv => "qsv",
                    HwAccel::VideoToolbox => "videotoolbox",
                    HwAccel::D3d11va => "d3d11va",
                    HwAccel::Dxva2 => "dxva2",
                    HwAccel::Vulkan => "vulkan",
                    HwAccel::None | HwAccel::Auto => unreachable!(),
                };
                self.hwaccels.iter().any(|a| a == name)
            }
        }
    }
}

fn metadata_label(p: MetadataPolicy) -> &'static str {
    match p {
        MetadataPolicy::Keep => "Keep",
        MetadataPolicy::Sensitive => "Remove sensitive (GPS & personal)",
        MetadataPolicy::All => "Remove all",
    }
}

fn mode_label(m: FaceMode) -> &'static str {
    match m {
        FaceMode::EyesOnly => "Eyes only",
        FaceMode::FullFace => "Full face",
    }
}

fn style_label(s: RedactionStyle) -> &'static str {
    match s {
        RedactionStyle::BlackBand => "Black band",
        RedactionStyle::Blur => "Blur",
        RedactionStyle::Pixelate => "Pixelate",
    }
}

fn shape_label(s: FaceShape) -> &'static str {
    match s {
        FaceShape::Ellipse => "Ellipse",
        FaceShape::Rectangle => "Rectangle",
    }
}

fn quality_label(q: VideoQuality) -> &'static str {
    match q {
        VideoQuality::Lossless => "Lossless",
        VideoQuality::VisuallyLossless => "Visually lossless",
        VideoQuality::High => "High",
    }
}

/// A combo box over an enum's values.
fn combo<T: Copy + PartialEq>(
    ui: &mut egui::Ui,
    id: &str,
    label: &str,
    value: &mut T,
    all: &[T],
    name: impl Fn(T) -> String,
    problem: impl Fn(T) -> Option<String>,
) {
    ui.horizontal(|ui| {
        ui.label(label);
        egui::ComboBox::from_id_salt(id).selected_text(name(*value)).show_ui(ui, |ui| {
            for &v in all {
                let issue = problem(v);
                let resp = ui.add_enabled(issue.is_none(), egui::Button::selectable(*value == v, name(v)));
                if resp.clicked() {
                    *value = v;
                }
                if let Some(why) = issue {
                    resp.on_disabled_hover_text(why);
                }
            }
        });
    });
}

/// A slider that goes back to `default` when double-clicked.
fn slider(
    ui: &mut egui::Ui,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    text: &str,
    default: f32,
    hint: Option<&str>,
) {
    let mut resp = ui.add(egui::Slider::new(value, range).text(text));
    // Sliders only sense drags, so `Response::double_clicked` never fires.
    let double_click = ui.input(|i| i.pointer.button_double_clicked(egui::PointerButton::Primary));
    if double_click && resp.contains_pointer() {
        *value = default;
        resp.mark_changed();
    }
    let reset = "Double-click to reset";
    resp.on_hover_text(match hint {
        Some(h) => format!("{h}\n{reset}"),
        None => reset.to_string(),
    });
}

fn corner_label(c: Corner) -> &'static str {
    match c {
        Corner::BottomLeft => "Bottom left",
        Corner::BottomRight => "Bottom right",
    }
}

/// Controls for one set of face options; `salt` keeps the widget ids of
/// several instances apart.
fn face_controls(ui: &mut egui::Ui, f: &mut FaceOptions, salt: &str) {
    let id = |name: &str| format!("{salt}-{name}");
    combo(ui, &id("mode"), "Mode", &mut f.mode, FaceMode::ALL, |v| mode_label(v).into(), |_| None);
    combo(ui, &id("style"), "Style", &mut f.style, RedactionStyle::ALL, |v| style_label(v).into(), |_| None);
    ui.add_enabled_ui(f.mode == FaceMode::FullFace, |ui| {
        combo(ui, &id("shape"), "Shape", &mut f.shape, FaceShape::ALL, |v| shape_label(v).into(), |_| None);
    });
    let defaults = FaceOptions::default();
    ui.add_enabled_ui(f.style != RedactionStyle::BlackBand, |ui| {
        slider(ui, &mut f.strength, 0.25..=4.0, "Strength", defaults.strength, None);
    });
    slider(ui, &mut f.padding, 0.0..=1.0, "Padding", defaults.padding, None);
    slider(
        ui,
        &mut f.min_confidence,
        0.1..=0.95,
        "Min confidence",
        defaults.min_confidence,
        Some("Lower values catch more faces, but also more false positives."),
    );
    let mut custom = f.detection_size.is_some();
    ui.horizontal(|ui| {
        ui.checkbox(&mut custom, "Detection size")
            .on_hover_text("Longest side given to the detector. Larger finds smaller faces but is slower.");
        if custom {
            let mut v = f.detection_size.unwrap_or(1920);
            ui.add(egui::DragValue::new(&mut v).range(160..=8192).suffix(" px"));
            f.detection_size = Some(v);
        } else {
            f.detection_size = None;
        }
    });
}

/// The section of a video under the playhead: while it is shown, the panel
/// edits its keyframe instead of the global face and timestamp settings.
pub struct SectionEdit<'a> {
    pub keyframe: &'a mut Keyframe,
    /// Time range of the section, for display.
    pub range: String,
    /// Whether the user switched to the global settings.
    pub global: &'a mut bool,
}

/// Draws the settings panel content: the settings of the video section under
/// the playhead if there is one (with a tab back to the global settings),
/// else the global settings. `before_keyframes`: the playhead of the selected
/// video is before its first keyframe.
pub fn show(
    ui: &mut egui::Ui,
    s: &mut Settings,
    env: &Environment,
    section: Option<SectionEdit<'_>>,
    before_keyframes: bool,
) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        if before_keyframes {
            ui.weak("Global settings: they apply to every file, and to this video before its first keyframe.");
            ui.separator();
        }
        if let Some(sec) = section {
            ui.horizontal(|ui| {
                ui.selectable_value(sec.global, false, "This section")
                    .on_hover_text("Settings of the video section under the playhead");
                ui.selectable_value(sec.global, true, "Global")
                    .on_hover_text("Settings of every file, and of this video before its first keyframe");
            });
            ui.separator();
            if !*sec.global {
                section_settings(ui, s, sec.keyframe, &sec.range);
                return;
            }
            ui.weak("These apply to every file, and to this video before its first keyframe.");
            ui.add_space(4.0);
        }
        global_settings(ui, s, env);
    });
}

/// Settings of one video section (the keyframe starting it).
fn section_settings(ui: &mut egui::Ui, s: &Settings, k: &mut Keyframe, range: &str) {
    ui.heading("Section");
    ui.weak(format!("{range} · from the keyframe to the next one"));
    ui.horizontal(|ui| {
        ui.radio_value(&mut k.cut, false, "Process");
        ui.radio_value(&mut k.cut, true, "Cut from the output")
            .on_hover_text("Removes these frames and the matching audio from the output");
    });
    if k.cut {
        ui.weak("These frames and the matching audio are removed from the output.");
        return;
    }
    if ui
        .small_button("Reset to global")
        .on_hover_text("Use the global face and timestamp settings in this section")
        .clicked()
    {
        k.faces = s.faces_enabled.then(|| s.face.clone());
        k.timestamp = true;
    }

    ui.separator();
    ui.heading("Faces");
    let mut cover = k.faces.is_some();
    ui.checkbox(&mut cover, "Detect and cover faces");
    if cover != k.faces.is_some() {
        k.faces = cover.then(|| s.face.clone());
    }
    match &mut k.faces {
        Some(f) => face_controls(ui, f, "section"),
        None => {
            ui.weak("Faces are left visible in this section.");
        }
    }

    ui.separator();
    ui.heading("Timestamp");
    if s.timestamp_enabled {
        ui.checkbox(&mut k.timestamp, "Overlay capture date");
    } else {
        ui.add_enabled(false, egui::Checkbox::new(&mut false, "Overlay capture date"))
            .on_disabled_hover_text("Turn the timestamp on in the Global tab first");
    }
    ui.weak(if s.timestamp_enabled {
        "Position, format and size are shared by the whole video (Global tab)."
    } else {
        "The timestamp is off in the Global tab."
    });
}

/// The settings shared by every file.
fn global_settings(ui: &mut egui::Ui, s: &mut Settings, env: &Environment) {
    ui.heading("Metadata");
    for p in MetadataPolicy::ALL {
        ui.radio_value(&mut s.options.metadata, *p, metadata_label(*p));
    }

    ui.separator();
    ui.heading("Faces");
    ui.checkbox(&mut s.faces_enabled, "Detect and cover faces");
    ui.add_enabled_ui(s.faces_enabled, |ui| {
        face_controls(ui, &mut s.face, "global");
    });

    ui.separator();
    ui.heading("Timestamp");
    ui.checkbox(&mut s.timestamp_enabled, "Overlay capture date")
        .on_hover_text("Read from EXIF / video tags before metadata removal, else the file modification time.");
    ui.add_enabled_ui(s.timestamp_enabled, |ui| {
        let t = &mut s.timestamp;
        combo(ui, "corner", "Position", &mut t.position, Corner::ALL, |v| corner_label(v).into(), |_| None);
        ui.horizontal(|ui| {
            ui.label("Format");
            ui.add(egui::TextEdit::singleline(&mut t.format).desired_width(150.0))
                .on_hover_text("%Y year, %m month, %b month name, %d day, %H:%M:%S time, %y 2-digit year");
        });
        slider(
            ui,
            &mut t.size,
            0.015..=0.1,
            "Text size",
            TimestampOptions::default().size,
            Some("Height of the text as a fraction of the picture height."),
        );
    });

    ui.separator();
    ui.heading("Compute");
    let c = &mut s.options.compute;
    combo(ui, "device", "Inference", &mut c.device, InferenceDevice::ALL, |v| v.to_string(), |v| env.device_problem(v));
    ui.horizontal(|ui| {
        ui.label("GPU index");
        ui.add(egui::DragValue::new(&mut c.gpu_index).range(0..=15));
    });
    ui.horizontal(|ui| {
        let mut limit = c.threads.is_some();
        ui.checkbox(&mut limit, "Limit threads");
        if limit {
            let mut t = c.threads.unwrap_or(4);
            ui.add(egui::DragValue::new(&mut t).range(1..=256));
            c.threads = Some(t);
        } else {
            c.threads = None;
        }
    });

    ui.separator();
    ui.heading("Video");
    let v = &mut s.options.video;
    combo(
        ui,
        "hwaccel",
        "HW decode",
        &mut v.hwaccel,
        HwAccel::ALL,
        |h| h.to_string(),
        |h| (!env.hwaccel_available(h)).then(|| "Not supported by the installed FFmpeg".to_string()),
    );
    combo(ui, "encoder", "Encoder", &mut v.encoder, EncoderBackend::ALL, |e| e.to_string(), |_| None);
    combo(ui, "quality", "Quality", &mut v.quality, VideoQuality::ALL, |q| quality_label(q).into(), |_| None);
    ui.horizontal(|ui| {
        ui.label("Detect every");
        ui.add(egui::DragValue::new(&mut v.detect_every).range(1..=30).suffix(" frames"));
    });
    ui.horizontal(|ui| {
        ui.label("Persistence");
        ui.add(egui::DragValue::new(&mut v.persistence).range(0..=60).suffix(" frames"))
            .on_hover_text("Keep each detection this many frames before and after, to cover missed frames.");
    });

    ui.separator();
    ui.heading("Output");
    ui.horizontal(|ui| {
        ui.radio_value(&mut s.output_mode, OutputMode::Suffix, "Next to originals, suffix");
        ui.add_enabled(
            s.output_mode == OutputMode::Suffix,
            egui::TextEdit::singleline(&mut s.suffix).desired_width(60.0),
        );
    });
    ui.horizontal(|ui| {
        ui.radio_value(&mut s.output_mode, OutputMode::Folder, "To folder");
        if ui.button("Choose…").clicked() {
            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                s.folder = Some(dir);
                s.output_mode = OutputMode::Folder;
            }
        }
    });
    if let Some(dir) = &s.folder {
        ui.small(dir.display().to_string());
    }
    let label = egui::RichText::new("Overwrite existing files").color(ui.visuals().error_fg_color);
    ui.checkbox(&mut s.overwrite, label)
        .on_hover_text("Files already at the output path are replaced; you are asked to confirm before starting.");

    for e in &env.errors {
        ui.separator();
        ui.colored_label(ui.visuals().warn_fg_color, e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_paths() {
        let mut s = Settings::default();
        assert_eq!(s.output_for(Path::new("/a/b.jpg")), PathBuf::from("/a/b.anon.jpg"));
        s.output_mode = OutputMode::Folder;
        s.folder = Some("/out".into());
        assert_eq!(s.output_for(Path::new("/a/b.jpg")), PathBuf::from("/out/b.jpg"));
    }

    #[test]
    fn defaults_are_valid() {
        assert!(Settings::default().validation_error().is_none());
        let s = Settings { faces_enabled: false, options: Options::default(), ..Default::default() };
        assert!(s.validation_error().is_some());
    }
}
