//! "People filter": scan the selected file for people, classify them
//! (civilian, police, helmet, shield...) with the attribute model, and turn
//! a rule ("cover only…" / "cover everyone except…") into visible-face marks.
//!
//! The marks it adds are ordinary marks: the timeline shows where each one
//! is followed, and they can be removed one by one. Applying the rule again
//! replaces the marks it added before, never the hand-made ones.

use crate::queue::Item;
use crate::settings::Settings;
use eframe::egui;
use mediaanon_core::{
    AttributeGroup, AttributeModel, CancelToken, FilterMode, Options, PeopleScan, Presence, Processor, Timeline,
    VisibleFace,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

/// File name of the attribute model looked for next to the application.
const MODEL_FILE: &str = "attributes.onnx";

/// Scan state of one file.
#[derive(Debug, Clone)]
pub struct FileScan {
    pub result: Result<PeopleScan, String>,
    /// What the scan was computed for.
    pub key: ScanKey,
    /// Marks added by the last "Apply", replaced by the next one.
    pub applied: Vec<VisibleFace>,
}

/// What a scan depends on: the timeline (skipped sections) and the options
/// (detection settings).
pub type ScanKey = (Timeline, Options);

type ScanResult = (PathBuf, ScanKey, Result<PeopleScan, String>);
/// The loaded model and its path, kept between scans.
type ModelCache = Arc<Mutex<Option<(PathBuf, Arc<AttributeModel>)>>>;

struct Job {
    path: PathBuf,
    cancel: CancelToken,
    /// Fraction done, as `f32` bits.
    progress: Arc<AtomicU32>,
}

/// App-wide state of the people filter.
pub struct People {
    job: Option<Job>,
    tx: Sender<ScanResult>,
    rx: Receiver<ScanResult>,
    model: ModelCache,
}

impl Default for People {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self { job: None, tx, rx, model: Arc::default() }
    }
}

/// The attribute model to use: the one chosen in the settings, else
/// `MEDIAANON_ATTRIBUTE_MODEL`, else `attributes.onnx` next to the
/// executable (or in the app bundle's resources, or in a `models/attributes`
/// folder above it, for development builds).
pub fn model_path(settings: &Settings) -> Option<PathBuf> {
    if let Some(p) = &settings.attribute_model {
        return Some(p.clone());
    }
    if let Some(p) = std::env::var_os("MEDIAANON_ATTRIBUTE_MODEL").filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let mut candidates = vec![dir.join(MODEL_FILE), dir.join("../Resources").join(MODEL_FILE)];
    candidates.extend(dir.ancestors().map(|d| d.join("models/attributes").join(MODEL_FILE)));
    candidates.into_iter().find(|p| p.is_file())
}

impl People {
    /// Collects finished scans into the queue items.
    pub fn poll(&mut self, items: &mut [Item]) {
        while let Ok((path, key, result)) = self.rx.try_recv() {
            if self.job.as_ref().is_some_and(|j| j.path == path) {
                self.job = None;
            }
            if let Some(item) = items.iter_mut().find(|i| i.path == path) {
                let applied = item.people.take().map(|p| p.applied).unwrap_or_default();
                item.people = Some(FileScan { result, key, applied });
            }
        }
    }

    /// Whether a scan is running.
    pub fn is_running(&self) -> bool {
        self.job.is_some()
    }

    /// Starts scanning `item` with the model of `settings`, if there is one.
    pub fn scan(&mut self, item: &Item, settings: &Settings, ctx: &egui::Context) {
        if let Some(model) = model_path(settings) {
            self.start(item, model, settings.preview_options(), ctx);
        }
    }

    fn running_for(&self, path: &Path) -> Option<f32> {
        self.job.as_ref().filter(|j| j.path == path).map(|j| f32::from_bits(j.progress.load(Ordering::Relaxed)))
    }

    fn start(&mut self, item: &Item, model: PathBuf, options: Options, ctx: &egui::Context) {
        if let Some(job) = self.job.take() {
            job.cancel.cancel();
        }
        let job = Job { path: item.path.clone(), cancel: CancelToken::new(), progress: Arc::default() };
        let key: ScanKey = (item.edits.timeline.clone(), options);
        let (path, cancel, progress, tx, ctx, cache) = (
            job.path.clone(),
            job.cancel.clone(),
            Arc::clone(&job.progress),
            self.tx.clone(),
            ctx.clone(),
            Arc::clone(&self.model),
        );
        self.job = Some(job);
        std::thread::spawn(move || {
            let on_progress = |p: mediaanon_core::Progress| {
                if let Some(f) = p.fraction() {
                    progress.store(f.to_bits(), Ordering::Relaxed);
                    ctx.request_repaint();
                }
            };
            let result = (|| {
                let model = {
                    let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
                    match cache.as_ref().filter(|(p, _)| *p == model) {
                        Some((_, m)) => Arc::clone(m),
                        None => {
                            let m = Arc::new(AttributeModel::load(&model, &key.1.compute)?);
                            *cache = Some((model, Arc::clone(&m)));
                            m
                        }
                    }
                };
                Processor::new(key.1.clone())?.scan_people(&path, &key.0, &model, &on_progress, &cancel)
            })();
            if !cancel.is_cancelled() {
                let _ = tx.send((path, key, result.map_err(|e| e.to_string())));
                ctx.request_repaint();
            }
        });
    }

    /// The "People filter" section of the settings panel, for the selected file.
    pub fn show(&mut self, ui: &mut egui::Ui, settings: &mut Settings, item: Option<&mut Item>) {
        ui.heading("People filter");
        let Some(item) = item.filter(|i| i.is_image() || i.is_video()) else {
            ui.weak("Select a photo or video in the queue to find the people in it and cover them by kind.");
            return;
        };
        let model = model_path(settings);
        ui.horizontal(|ui| {
            match &model {
                Some(p) => {
                    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    ui.weak(format!("Model: {name}")).on_hover_text(p.display().to_string());
                }
                None => {
                    ui.colored_label(ui.visuals().warn_fg_color, "No attribute model")
                        .on_hover_text("Train one with models/attributes (see its README), then choose the .onnx file");
                }
            }
            if ui.small_button("Choose…").on_hover_text("Choose the attribute model (.onnx)").clicked() {
                if let Some(p) = rfd::FileDialog::new().add_filter("ONNX model", &["onnx"]).pick_file() {
                    settings.attribute_model = Some(p);
                }
            }
        });
        let options = settings.preview_options();
        let key: ScanKey = (item.edits.timeline.clone(), options.clone());
        ui.horizontal(|ui| match self.running_for(&item.path) {
            Some(f) => {
                ui.add(egui::ProgressBar::new(f).desired_width(160.0).show_percentage());
                if ui.small_button("Cancel").clicked() {
                    if let Some(job) = self.job.take() {
                        job.cancel.cancel();
                    }
                }
            }
            None => {
                let label = if item.people.is_some() { "Scan again" } else { "Scan people" };
                let scan = ui
                    .add_enabled(model.is_some(), egui::Button::new(label))
                    .on_hover_text("Find every person in this file and recognise their role and gear")
                    .on_disabled_hover_text("Choose an attribute model first");
                if scan.clicked() {
                    if let Some(m) = model.clone() {
                        self.start(item, m, options.clone(), ui.ctx());
                    }
                }
                if item.people.as_ref().is_some_and(|p| p.key != key) {
                    ui.colored_label(ui.visuals().warn_fg_color, "outdated")
                        .on_hover_text("The settings or the timeline changed since the scan");
                }
            }
        });
        let Some(state) = item.people.as_mut() else { return };
        let scan = match &state.result {
            Ok(scan) => scan.clone(),
            Err(e) => {
                ui.colored_label(ui.visuals().error_fg_color, format!("Scan failed: {e}"));
                return;
            }
        };
        let n = scan.people.len();
        ui.label(format!("{n} {} found", if n == 1 { "person" } else { "people" }));
        if n == 0 {
            return;
        }

        let filter = &mut settings.people_filter;
        ui.radio_value(&mut filter.mode, FilterMode::CoverOnly, "Include only: cover only people who are…")
            .on_hover_text(
                "Everyone else is left visible, if the model is sure they have none of the ticked attributes",
            );
        ui.radio_value(&mut filter.mode, FilterMode::CoverAllExcept, "Exclude: cover everyone except people who are…")
            .on_hover_text(
                "Those people are left visible, if the model is sure they have one of the ticked attributes",
            );
        let counts = scan.counts();
        for (group, title) in [(AttributeGroup::Role, "Role"), (AttributeGroup::Gear, "Wearing or carrying")] {
            ui.label(egui::RichText::new(title).small().strong());
            ui.horizontal_wrapped(|ui| {
                for (i, a) in scan.attributes.iter().enumerate().filter(|(_, a)| a.group == group) {
                    let (yes, unsure) = counts[i];
                    let mut on = filter.attributes.contains(&a.name);
                    let text = if unsure > 0 {
                        format!("{} ({yes}+{unsure}?)", a.label)
                    } else {
                        format!("{} ({yes})", a.label)
                    };
                    let resp = ui
                        .checkbox(&mut on, text)
                        .on_hover_text(format!("{yes} recognised, {unsure} unsure (unsure people are always covered)"));
                    if resp.changed() {
                        if on {
                            filter.attributes.push(a.name.clone());
                        } else {
                            filter.attributes.retain(|n| n != &a.name);
                        }
                    }
                }
            });
        }
        let visible = scan.people.iter().filter(|p| scan.is_visible(p, filter)).count();
        ui.label(format!("Result: {visible} left visible, {} covered", n - visible));
        ui.horizontal(|ui| {
            let apply = ui
                .add_enabled(!filter.attributes.is_empty(), egui::Button::new("Apply to this file"))
                .on_hover_text("Replace the marks added by the people filter with the people this rule leaves visible")
                .on_disabled_hover_text("Tick at least one attribute");
            if apply.clicked() {
                let marks = scan.marks(filter);
                item.edits.visible.retain(|m| !state.applied.contains(m));
                item.edits.visible.extend(marks.iter().copied());
                state.applied = marks;
            }
            let applied = state.applied.iter().filter(|m| item.edits.visible.contains(m)).count();
            if applied > 0 && ui.small_button(format!("Remove its {applied} marks")).clicked() {
                item.edits.visible.retain(|m| !state.applied.contains(m));
                state.applied.clear();
            }
        });
        ui.weak("Review the result: the model can be wrong, and people it is unsure about stay covered.");

        let video = item.is_video();
        egui::CollapsingHeader::new(format!("People ({n})")).id_salt("people-list").show(ui, |ui| {
            for p in &scan.people {
                let role = scan.role(p).map(|a| a.label.as_str()).unwrap_or("?");
                let gear: Vec<&str> = (0..scan.attributes.len())
                    .filter(|&i| {
                        scan.attributes[i].group == AttributeGroup::Gear && scan.presence(p, i) == Presence::Yes
                    })
                    .map(|i| scan.attributes[i].label.as_str())
                    .collect();
                let mut text = format!("#{} {role}", p.id);
                if !gear.is_empty() {
                    text += &format!(" · {}", gear.join(", "));
                }
                if p.samples == 0 {
                    text += " · too small to classify";
                }
                let visible = scan.is_visible(p, filter);
                ui.horizontal(|ui| {
                    if video && ui.small_button(crate::timeline::format_time(p.start)).on_hover_text("Go to").clicked()
                    {
                        item.position = p.start;
                    }
                    let color = if visible { egui::Color32::from_rgb(60, 200, 90) } else { ui.visuals().text_color() };
                    let scores: Vec<String> = scan
                        .attributes
                        .iter()
                        .zip(&p.scores)
                        .map(|(a, s)| format!("{} {:.0}%", a.label, s * 100.0))
                        .collect();
                    ui.colored_label(color, text).on_hover_text(if scores.is_empty() {
                        "No score".to_string()
                    } else {
                        scores.join("\n")
                    });
                });
            }
        });
    }
}
