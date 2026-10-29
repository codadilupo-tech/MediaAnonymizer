//! Main window: settings, queue and preview.

use crate::preview::{Preview, Source};
use crate::queue::{Queue, Status};
use crate::settings::{self, Environment, Settings};
use crate::worker::{Event, Task, Worker};
use eframe::egui;
use mediaanon_core::{CancelToken, Edits, FaceTrack, Options, PreviewFace, VideoInfo, VisibleFace};
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Instant;

const SETTINGS_KEY: &str = "settings";

/// Automatic screenshot for documentation (`MEDIAANON_GUI_SCREENSHOT`).
struct Screenshot {
    path: PathBuf,
    started: Instant,
    requested: bool,
    done: Arc<Mutex<bool>>,
}

pub struct App {
    settings: Settings,
    env: Environment,
    queue: Queue,
    selected: Option<usize>,
    worker: Option<Worker>,
    preview: Preview,
    /// Error that prevented the last run from starting.
    run_error: Option<String>,
    device: Option<String>,
    screenshot: Option<Screenshot>,
    /// Start processing on the first frame (`MEDIAANON_GUI_AUTOSTART=1`).
    autostart: bool,
    /// Files that would be replaced, while the confirmation dialog is open.
    confirm_overwrite: Option<Vec<PathBuf>>,
    /// Video information (duration, frame rate) read in the background.
    info_tx: Sender<(PathBuf, Result<VideoInfo, String>)>,
    info_rx: Receiver<(PathBuf, Result<VideoInfo, String>)>,
    info_pending: HashSet<PathBuf>,
    /// Faces marked visible on the sample photo (to try the feature).
    sample_visible: Vec<VisibleFace>,
    /// Background tracking of the faces marked visible in the selected video.
    tracking: Option<TrackingJob>,
    track_tx: Sender<TrackResult>,
    track_rx: Receiver<TrackResult>,
    /// Debounce: tracking key wanted and since when.
    track_wanted: Option<(TrackKey, Instant)>,
    /// The left panel shows the global settings although the playhead of
    /// the selected video is in a keyframe section.
    section_global: bool,
    /// Section under the playhead (video, keyframe index, keyframe count):
    /// entering another section shows its settings again.
    section_seen: Option<(PathBuf, usize, usize)>,
}

/// What a tracking pass depends on.
type TrackKey = (Edits, Options);
type TrackResult = (PathBuf, TrackKey, Result<Vec<FaceTrack>, String>);

/// Wait before (re)starting a tracking pass after the marks or settings change.
const TRACK_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(500);

struct TrackingJob {
    path: PathBuf,
    key: TrackKey,
    cancel: CancelToken,
    /// Fraction done, as `f32` bits.
    progress: Arc<AtomicU32>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, files: Vec<PathBuf>, screenshot: Option<PathBuf>) -> Self {
        let settings = cc.storage.and_then(|s| eframe::get_value(s, SETTINGS_KEY)).unwrap_or_default();
        let mut queue = Queue::default();
        queue.add_paths(files);
        let selected = queue.items.iter().position(|i| i.is_image() || i.is_video());
        let (info_tx, info_rx) = channel();
        let (track_tx, track_rx) = channel();
        Self {
            settings,
            env: Environment::detect(),
            queue,
            selected,
            worker: None,
            preview: Preview::new(cc.egui_ctx.clone()),
            run_error: None,
            device: None,
            screenshot: screenshot.map(|path| Screenshot {
                path,
                started: Instant::now(),
                requested: false,
                done: Arc::new(Mutex::new(false)),
            }),
            autostart: std::env::var_os("MEDIAANON_GUI_AUTOSTART").is_some_and(|v| v == "1"),
            confirm_overwrite: None,
            info_tx,
            info_rx,
            info_pending: HashSet::new(),
            sample_visible: Vec::new(),
            tracking: None,
            track_tx,
            track_rx,
            track_wanted: None,
            section_global: false,
            section_seen: None,
        }
    }

    fn running(&self) -> bool {
        self.worker.is_some()
    }

    fn start(&mut self, ctx: &egui::Context) {
        let tasks: Vec<Task> = self
            .queue
            .items
            .iter_mut()
            .filter(|i| Queue::will_run(i))
            .map(|i| {
                i.status = Status::Pending;
                Task { input: i.path.clone(), output: self.settings.output_for(&i.path), edits: i.edits.clone() }
            })
            .collect();
        if tasks.is_empty() {
            return;
        }
        self.run_error = None;
        let opts = self.settings.effective_options();
        self.worker = Some(Worker::start(tasks, opts, self.settings.overwrite, ctx.clone()));
    }

    /// Starts processing unless the settings are invalid (the reason is then
    /// shown next to the Start button). If existing files would be replaced,
    /// asks for confirmation first.
    fn request_start(&mut self, ctx: &egui::Context) {
        if self.running() || self.settings.validation_error().is_some() {
            return;
        }
        let replaced = self.replaced_files();
        if replaced.is_empty() {
            self.start(ctx);
        } else {
            self.confirm_overwrite = Some(replaced);
        }
    }

    /// Existing files the next run would overwrite: outputs that already
    /// exist when overwriting is enabled, and inputs that are their own output.
    fn replaced_files(&self) -> Vec<PathBuf> {
        self.queue
            .items
            .iter()
            .filter(|i| Queue::will_run(i))
            .map(|i| (i.path.as_path(), self.settings.output_for(&i.path)))
            .filter(|(input, out)| out.exists() && (self.settings.overwrite || out == input))
            .map(|(_, out)| out)
            .collect()
    }

    fn confirm_overwrite_ui(&mut self, ctx: &egui::Context) {
        let Some(files) = &self.confirm_overwrite else { return };
        let mut decision = None;
        let modal = egui::Modal::new(egui::Id::unique("confirm-overwrite")).show(ctx, |ui| {
            ui.set_max_width(460.0);
            ui.heading("Overwrite existing files?");
            let n = files.len();
            ui.label(format!(
                "{n} existing file{} will be replaced. This cannot be undone.",
                if n == 1 { "" } else { "s" }
            ));
            egui::ScrollArea::vertical().max_height(160.0).show(ui, |ui| {
                for f in files.iter().take(50) {
                    ui.small(f.display().to_string());
                }
                if n > 50 {
                    ui.small(format!("… and {} more", n - 50));
                }
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let overwrite = egui::RichText::new("Overwrite").color(ui.visuals().error_fg_color);
                if ui.button(overwrite).clicked() {
                    decision = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    decision = Some(false);
                }
            });
        });
        if modal.should_close() {
            decision.get_or_insert(false);
        }
        match decision {
            Some(true) => {
                self.confirm_overwrite = None;
                self.start(ctx);
            }
            Some(false) => self.confirm_overwrite = None,
            None => {}
        }
    }

    fn poll_worker(&mut self) {
        let Some(worker) = &self.worker else { return };
        let mut finished = false;
        while let Ok(ev) = worker.events.try_recv() {
            let mut set = |path: &PathBuf, status: Status| {
                if let Some(i) = self.queue.index_of(path) {
                    self.queue.items[i].status = status;
                }
            };
            match ev {
                Event::Ready(dev) => self.device = dev,
                Event::Started(p) => set(&p, Status::Running(None)),
                Event::Progress(p, f) => set(&p, Status::Running(f)),
                Event::Done(p, Ok(r)) => set(&p, Status::Done(r)),
                Event::Done(p, Err(e)) => set(&p, Status::Failed(e)),
                Event::Skipped(p, why) => set(&p, Status::Skipped(why)),
                Event::Fatal(e) => self.run_error = Some(e),
                Event::Finished => finished = true,
            }
        }
        if finished {
            self.worker = None;
            // Anything still marked running was interrupted by a cancel.
            for i in &mut self.queue.items {
                if matches!(i.status, Status::Running(_)) {
                    i.status = Status::Failed("cancelled".into());
                }
            }
        }
    }

    /// Collects video information and starts reading it for the selected
    /// video when missing.
    fn update_video_info(&mut self, ctx: &egui::Context) {
        while let Ok((path, info)) = self.info_rx.try_recv() {
            self.info_pending.remove(&path);
            if let Some(i) = self.queue.index_of(&path) {
                self.queue.items[i].info = Some(info);
            }
        }
        let Some(item) = self.selected.and_then(|i| self.queue.items.get(i)) else { return };
        if !item.is_video() || item.info.is_some() || !self.info_pending.insert(item.path.clone()) {
            return;
        }
        let (path, tx, ctx) = (item.path.clone(), self.info_tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let info = mediaanon_core::video::info(&path).map_err(|e| e.to_string());
            let _ = tx.send((path, info));
            ctx.request_repaint();
        });
    }

    /// Tracking key of a video for the current settings.
    fn track_key(&self, item: &crate::queue::Item) -> TrackKey {
        (item.edits.clone(), self.settings.preview_options())
    }

    /// Collects tracking results and (re)starts the pass of the selected
    /// video when its marks or the settings changed.
    fn update_tracking(&mut self, ctx: &egui::Context) {
        while let Ok((path, key, result)) = self.track_rx.try_recv() {
            if self.tracking.as_ref().is_some_and(|j| j.path == path && j.key == key) {
                self.tracking = None;
            }
            if let Some(i) = self.queue.index_of(&path) {
                self.queue.items[i].tracks = Some(crate::queue::Tracks { key, result });
            }
        }
        let Some(item) = self.selected.and_then(|i| self.queue.items.get(i)).filter(|i| i.is_video()) else {
            return;
        };
        if item.edits.visible.is_empty() || !matches!(item.info, Some(Ok(_))) {
            return;
        }
        let key = self.track_key(item);
        let fresh = item.tracks.as_ref().is_some_and(|t| t.key == key);
        let running = self.tracking.as_ref().is_some_and(|j| j.path == item.path && j.key == key);
        if fresh || running {
            return;
        }
        match &self.track_wanted {
            Some((k, since)) if *k == key && since.elapsed() >= TRACK_DEBOUNCE => {}
            Some((k, since)) if *k == key => {
                ctx.request_repaint_after(TRACK_DEBOUNCE.saturating_sub(since.elapsed()));
                return;
            }
            _ => {
                self.track_wanted = Some((key, Instant::now()));
                ctx.request_repaint_after(TRACK_DEBOUNCE);
                return;
            }
        }
        if let Some(job) = self.tracking.take() {
            job.cancel.cancel();
        }
        let job = TrackingJob {
            path: item.path.clone(),
            key: key.clone(),
            cancel: CancelToken::new(),
            progress: Arc::new(AtomicU32::new(0f32.to_bits())),
        };
        let (path, cancel, progress, tx, ctx) =
            (job.path.clone(), job.cancel.clone(), Arc::clone(&job.progress), self.track_tx.clone(), ctx.clone());
        self.tracking = Some(job);
        std::thread::spawn(move || {
            let (edits, options) = &key;
            let on_progress = |p: mediaanon_core::Progress| {
                if let Some(f) = p.fraction() {
                    progress.store(f.to_bits(), Ordering::Relaxed);
                    ctx.request_repaint();
                }
            };
            let result = mediaanon_core::Processor::new(options.clone())
                .and_then(|p| p.track_visible_faces(&path, edits, &on_progress, &cancel));
            if !cancel.is_cancelled() {
                let _ = tx.send((path, key, result.map_err(|e| e.to_string())));
                ctx.request_repaint();
            }
        });
    }

    /// Progress of the tracking pass of `path`, if one is running.
    fn tracking_progress(&self, path: &std::path::Path) -> Option<f32> {
        self.tracking.as_ref().filter(|j| j.path == path).map(|j| f32::from_bits(j.progress.load(Ordering::Relaxed)))
    }

    /// Clicking a face in the preview toggles whether it stays visible.
    fn toggle_face(&mut self, face: PreviewFace) {
        let toggle_photo = |marks: &mut Vec<VisibleFace>| {
            if face.visible {
                marks.retain(|m| !face.bbox.contains(m.x, m.y));
            } else {
                let (x, y) = face.bbox.center();
                marks.push(VisibleFace { time: 0.0, x, y });
            }
        };
        let Some(i) = self.selected.filter(|&i| i < self.queue.items.len()) else {
            toggle_photo(&mut self.sample_visible);
            return;
        };
        let options = self.settings.preview_options();
        let item = &mut self.queue.items[i];
        if item.is_image() {
            toggle_photo(&mut item.edits.visible);
            return;
        }
        if !face.visible {
            let (x, y) = face.bbox.center();
            item.edits.visible.push(VisibleFace { time: item.position, x, y });
            return;
        }
        // A visible face of a video belongs to a track: remove its mark.
        let frame = match &item.info {
            Some(Ok(info)) => (item.position * info.frame_rate).round() as u64,
            _ => return,
        };
        let key = (item.edits.clone(), options);
        let tracks = item.tracks.as_ref().filter(|t| t.key == key).and_then(|t| t.result.as_ref().ok());
        let mark = tracks.and_then(|tracks| {
            tracks
                .iter()
                .filter_map(|t| t.frames.get(&frame).map(|b| (t.mark, b.iou(&face.bbox))))
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(m, _)| m)
        });
        if let Some(m) = mark.filter(|&m| m < item.edits.visible.len()) {
            item.edits.visible.remove(m);
        }
    }

    /// Positions of the tracked visible faces on the current frame of a video
    /// (empty while tracking is not up to date).
    fn keep_boxes(&self, item: &crate::queue::Item) -> Vec<mediaanon_core::FaceBox> {
        let Some(Ok(info)) = &item.info else { return Vec::new() };
        let frame = (item.position * info.frame_rate).round() as u64;
        let key = self.track_key(item);
        match item.tracks.as_ref().filter(|t| t.key == key).map(|t| &t.result) {
            Some(Ok(tracks)) => tracks.iter().filter_map(|t| t.frames.get(&frame).copied()).collect(),
            _ => Vec::new(),
        }
    }

    /// Photos: how many faces are left visible, with a way to cover them all again.
    fn photo_marks_ui(&mut self, ui: &mut egui::Ui) {
        let marks = match self.selected.and_then(|i| self.queue.items.get_mut(i)) {
            Some(item) if item.is_image() => &mut item.edits.visible,
            Some(_) => return,
            None => &mut self.sample_visible,
        };
        if marks.is_empty() {
            return;
        }
        ui.horizontal(|ui| {
            let n = marks.len();
            ui.colored_label(
                egui::Color32::from_rgb(60, 200, 90),
                format!("{n} face{} marked to stay visible", if n == 1 { "" } else { "s" }),
            );
            if ui.small_button("Cover all again").clicked() {
                marks.clear();
            }
        });
    }

    fn handle_drops(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect()
        });
        if !dropped.is_empty() && !self.running() {
            self.queue.add_paths(dropped);
        }
    }

    fn queue_ui(&mut self, ui: &mut egui::Ui) {
        let running = self.running();
        ui.horizontal(|ui| {
            ui.add_enabled_ui(!running, |ui| {
                if ui.button("Add files…").clicked() {
                    if let Some(files) = rfd::FileDialog::new()
                        .add_filter(
                            "Photos and videos",
                            &[mediaanon_core::media::IMAGE_EXTENSIONS, mediaanon_core::media::VIDEO_EXTENSIONS]
                                .concat(),
                        )
                        .pick_files()
                    {
                        self.queue.add_paths(files);
                    }
                }
                if ui.button("Add folder…").clicked() {
                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                        self.queue.add_paths([dir]);
                    }
                }
                let failed = self.queue.items.iter().filter(|i| matches!(i.status, Status::Failed(_))).count();
                if ui
                    .add_enabled(!running && failed > 0, egui::Button::new(format!("Retry failed ({failed})")))
                    .on_hover_text("Process the failed files again")
                    .clicked()
                {
                    self.queue.retry_failed();
                    self.request_start(ui.ctx());
                }
                if ui.button("Clear finished").clicked() {
                    self.queue.clear_finished();
                    self.selected = None;
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if running {
                    if ui.button("Cancel").clicked() {
                        if let Some(w) = &self.worker {
                            w.cancel();
                        }
                    }
                    ui.spinner();
                } else {
                    let problem = self.settings.validation_error();
                    let runnable = self.queue.items.iter().any(Queue::will_run);
                    let start = ui
                        .add_enabled(problem.is_none() && runnable, egui::Button::new("Start"))
                        .on_hover_text("Process the pending files and retry the failed or skipped ones")
                        .on_disabled_hover_text(problem.clone().unwrap_or_else(|| "Add files to the queue".into()));
                    if start.clicked() {
                        self.request_start(ui.ctx());
                    }
                    if let Some(p) = problem {
                        ui.colored_label(ui.visuals().warn_fg_color, p);
                    }
                }
            });
        });
        if let Some(e) = &self.run_error {
            ui.colored_label(ui.visuals().error_fg_color, format!("Cannot start: {e}"));
        }
        ui.separator();

        if self.queue.items.is_empty() {
            ui.centered_and_justified(|ui| ui.weak("Drop photos, videos or folders here"));
            return;
        }
        let mut remove = None;
        let mut retry = None;
        egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
            egui::Grid::new("queue").num_columns(4).striped(true).spacing([12.0, 6.0]).show(ui, |ui| {
                for (idx, item) in self.queue.items.iter().enumerate() {
                    let selected = self.selected == Some(idx);
                    if ui
                        .selectable_label(selected, item.name())
                        .on_hover_text(item.path.display().to_string())
                        .clicked()
                    {
                        self.selected = Some(idx);
                    }
                    let mut label = item.kind_label().to_string();
                    let keys = item.edits.timeline.keyframes().len();
                    if keys > 0 {
                        label += &format!(" · {keys} keyframe{}", if keys == 1 { "" } else { "s" });
                    }
                    let marks = item.edits.visible.len();
                    if marks > 0 {
                        label += &format!(" · {marks} visible face{}", if marks == 1 { "" } else { "s" });
                    }
                    let resp = ui.label(label);
                    if keys + marks > 0 {
                        resp.on_hover_text("Edits for this file: select it to review them in the preview");
                    }
                    match &item.status {
                        Status::Pending => {
                            ui.weak("Pending");
                        }
                        Status::Running(f) => {
                            let bar = egui::ProgressBar::new(f.unwrap_or(0.0)).desired_width(180.0);
                            ui.add(if f.is_some() { bar.show_percentage() } else { bar.animate(true) });
                        }
                        Status::Done(r) => {
                            let text = summary(r);
                            let resp = ui.label(format!("Done: {text}"));
                            let mut details: Vec<String> = r.notes.clone();
                            details.extend(r.warnings.iter().map(|w| format!("warning: {w}")));
                            details.extend(r.removed.iter().map(|m| format!("removed: {m}")));
                            details.insert(0, format!("output: {}", r.output.display()));
                            resp.on_hover_text(details.join("\n"));
                        }
                        Status::Skipped(why) => {
                            ui.weak(format!("Skipped: {why}"));
                        }
                        Status::Failed(e) => {
                            let first = e.lines().next().unwrap_or_default();
                            let short: String = first.chars().take(42).collect();
                            let ellipsis = if short.len() < e.len() { "…" } else { "" };
                            ui.colored_label(ui.visuals().error_fg_color, format!("Failed: {short}{ellipsis}"))
                                .on_hover_text(e);
                        }
                    }
                    ui.horizontal(|ui| {
                        if Queue::can_retry(item)
                            && ui
                                .add_enabled(!running, egui::Button::new("Retry").small())
                                .on_hover_text("Process this file again")
                                .clicked()
                        {
                            retry = Some(idx);
                        }
                        if ui.add_enabled(!running, egui::Button::new("Remove").small()).clicked() {
                            remove = Some(idx);
                        }
                    });
                    ui.end_row();
                }
            });
        });
        if let Some(idx) = retry {
            self.queue.retry(idx);
            self.request_start(ui.ctx());
        }
        if let Some(idx) = remove {
            self.queue.items.remove(idx);
            self.selected = match self.selected {
                Some(s) if s == idx => None,
                Some(s) if s > idx => Some(s - 1),
                s => s,
            };
        }
    }

    fn take_screenshot_if_ready(&mut self, ctx: &egui::Context) {
        let ready = self.preview.is_ready() && !self.running() && !self.autostart;
        let Some(shot) = &mut self.screenshot else { return };
        if *shot.done.lock().unwrap_or_else(|e| e.into_inner()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        let timeout = shot.started.elapsed().as_secs() >= 60;
        // Give the layout a moment to settle before capturing.
        if !shot.requested && shot.started.elapsed().as_millis() > 800 && (ready || timeout) {
            shot.requested = true;
            let (path, done) = (shot.path.clone(), Arc::clone(&shot.done));
            ctx.request_screenshot(move |img| {
                let [w, h] = img.size;
                let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
                match image::RgbaImage::from_raw(w as u32, h as u32, bytes).map(|i| i.save(&path)) {
                    Some(Ok(())) => tracing::info!("screenshot saved to {}", path.display()),
                    Some(Err(e)) => tracing::error!("cannot save screenshot: {e}"),
                    None => tracing::error!("invalid screenshot buffer"),
                }
                *done.lock().unwrap_or_else(|e| e.into_inner()) = true;
            });
        }
    }
}

fn summary(r: &mediaanon_core::Report) -> String {
    let faces = match (r.frames, r.frames_with_faces) {
        (Some(f), Some(w)) => format!("{w}/{f} frames with faces"),
        _ => format!("{} face{}", r.faces, if r.faces == 1 { "" } else { "s" }),
    };
    format!("{faces} · {} metadata removed · {:.1}s", r.removed.len(), r.elapsed.as_secs_f64())
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_worker();
        self.handle_drops(&ctx);
        if self.autostart {
            self.autostart = false;
            if self.settings.validation_error().is_none() {
                self.start(&ctx);
            }
        }

        // Preview the selected photo with the current settings; videos (and
        // no selection) show a sample photo instead.
        self.update_video_info(&ctx);
        self.update_tracking(&ctx);
        let selected = self.selected.and_then(|i| self.queue.items.get(i));
        let target = match selected {
            // Video frames are always shown: the timeline needs them, even
            // when only cutting.
            Some(item) if item.is_video() => Some(Source::Video {
                path: item.path.clone(),
                time: item.position,
                timeline: item.edits.timeline.clone(),
                keep: self.keep_boxes(item),
            }),
            Some(item) if item.is_image() => self
                .settings
                .changes_pixels()
                .then(|| Source::File { path: item.path.clone(), visible: item.edits.visible.clone() }),
            _ => self.settings.changes_pixels().then(|| Source::Sample { visible: self.sample_visible.clone() }),
        };
        self.preview.want(target, self.settings.preview_options());
        self.preview.update(&ctx);

        egui::Panel::bottom("footer").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.small(format!("MediaAnonymizer {}", env!("CARGO_PKG_VERSION")));
                if let Some(d) = &self.device {
                    ui.small(format!("· faces detected on {d}"));
                }
                ui.small("· Black band is irreversible; blur and pixelate can be weaker at low strength.");
            });
        });
        // Inside a keyframe section of the selected video, the left panel
        // edits that section.
        let section = self
            .selected
            .filter(|&i| self.queue.items.get(i).is_some_and(|i| i.is_video()))
            .and_then(|i| crate::timeline::current_section(&self.queue.items[i]).map(|s| (i, s)));
        let seen = section.as_ref().map(|(i, (k, _))| {
            let item = &self.queue.items[*i];
            (item.path.clone(), *k, item.edits.timeline.keyframes().len())
        });
        if seen != self.section_seen {
            self.section_seen = seen;
            self.section_global = false;
        }
        let before_keyframes = section.is_none()
            && self
                .selected
                .and_then(|i| self.queue.items.get(i))
                .is_some_and(|i| i.is_video() && matches!(i.info, Some(Ok(_))) && !i.edits.timeline.is_empty());
        let running = self.running();
        let edit = section.and_then(|(i, (k, range))| {
            let keyframe = self.queue.items[i].edits.timeline.get_mut(k)?;
            Some(settings::SectionEdit { keyframe, range, global: &mut self.section_global })
        });
        egui::Panel::left("settings").resizable(true).default_size(310.0).show(ui, |ui| {
            ui.add_enabled_ui(!running, |ui| settings::show(ui, &mut self.settings, &self.env, edit, before_keyframes));
        });
        let video = self.selected.filter(|&i| self.queue.items.get(i).is_some_and(|i| i.is_video()));
        let tracking = video.and_then(|i| self.tracking_progress(&self.queue.items[i].path));
        let clicked = egui::Panel::right("preview")
            .resizable(true)
            .default_size(520.0)
            .min_size(360.0)
            .show(ui, |ui| {
                ui.heading("Preview");
                if let Some(i) = video {
                    egui::Panel::bottom("timeline").resizable(true).default_size(330.0).min_size(120.0).show(
                        ui,
                        |ui| {
                            ui.add_space(4.0);
                            crate::timeline::show(ui, &mut self.queue.items[i], &self.settings, tracking);
                        },
                    );
                } else {
                    self.photo_marks_ui(ui);
                }
                self.preview.show(ui)
            })
            .inner;
        if let Some(face) = clicked {
            self.toggle_face(face);
        }
        egui::CentralPanel::default().show(ui, |ui| {
            ui.heading("Queue");
            self.queue_ui(ui);
        });

        self.confirm_overwrite_ui(&ctx);
        self.take_screenshot_if_ready(&ctx);
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, SETTINGS_KEY, &self.settings);
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(w) = &self.worker {
            w.cancel();
        }
    }
}
