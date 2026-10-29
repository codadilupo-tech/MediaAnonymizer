//! # mediaanon-core
//!
//! The engine behind MediaAnonymizer: removes metadata and redacts faces in
//! photos and videos while preserving the original quality.
//!
//! ```no_run
//! use mediaanon_core::{CancelToken, FaceOptions, MetadataPolicy, Options, Processor, RedactionStyle};
//!
//! let options = Options {
//!     metadata: MetadataPolicy::All,
//!     faces: Some(FaceOptions { style: RedactionStyle::Blur, ..Default::default() }),
//!     ..Default::default()
//! };
//! let processor = Processor::new(options)?;
//! let report = processor.process_file(
//!     "holiday.jpg".as_ref(),
//!     "holiday.anon.jpg".as_ref(),
//!     &|_progress| {},
//!     &CancelToken::new(),
//! )?;
//! println!("{} face(s) redacted", report.faces);
//! # Ok::<(), mediaanon_core::Error>(())
//! ```
//!
//! ## Guarantees
//!
//! * Metadata removal never re-encodes: JPEG/PNG/WebP are edited at the
//!   container level and videos are remuxed with stream copy.
//! * JPEG face redaction rewrites only the 8×8 DCT blocks that touch a face;
//!   every other block is bit-identical to the source.
//! * Other formats and videos are re-encoded losslessly by default.
//! * Outputs are written atomically: a failed or cancelled run never leaves a
//!   half-written file behind.

#![warn(missing_debug_implementations, rust_2018_idioms)]

mod cancel;
pub mod detect;
pub mod edits;
mod error;
pub mod media;
pub mod metadata;
mod options;
pub mod overlay;
pub mod photo;
pub mod redact;
pub mod timeline;
pub mod video;

pub use cancel::CancelToken;
pub use detect::{available_devices, DetectorPool, DeviceInfo, Face};
pub use edits::{Edits, FaceBox, PreviewFace, VisibleFace};
pub use error::{Error, Result};
pub use media::{ImageFormat, MediaKind};
pub use metadata::Finding;
pub use options::*;
pub use photo::PhotoPreview;
pub use timeline::{Keyframe, Section, Timeline};
pub use video::{FaceTrack, FramePreview, VideoInfo};

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Progress of the file being processed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub done: u64,
    pub total: Option<u64>,
    /// Unit of `done`/`total` (`"frames"`, `"µs"`...).
    pub unit: &'static str,
}

impl Progress {
    /// Completion in `0..=1`, if the total is known.
    pub fn fraction(&self) -> Option<f32> {
        self.total.filter(|&t| t > 0).map(|t| (self.done as f32 / t as f32).min(1.0))
    }
}

/// Summary of one processed file.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub input: PathBuf,
    pub output: PathBuf,
    pub kind: MediaKind,
    /// Faces redacted (photos) or face detections (videos).
    pub faces: u64,
    /// Videos: number of frames, and frames where at least one face was covered.
    pub frames: Option<u64>,
    pub frames_with_faces: Option<u64>,
    /// Text drawn by the timestamp overlay and its source.
    pub timestamp: Option<String>,
    /// Metadata that was removed.
    pub removed: Vec<String>,
    /// Things the user should know about (quality trade-offs, dropped streams...).
    pub warnings: Vec<String>,
    /// How the output was produced.
    pub notes: Vec<String>,
    pub input_bytes: u64,
    pub output_bytes: u64,
    #[serde(serialize_with = "ser_duration")]
    pub elapsed: Duration,
}

fn ser_duration<S: serde::Serializer>(d: &Duration, s: S) -> std::result::Result<S::Ok, S::Error> {
    s.serialize_f64(d.as_secs_f64())
}

/// Reusable processing engine. Create it once and process many files: the
/// face detector is loaded a single time and shared across threads.
pub struct Processor {
    options: Options,
    /// Loaded up front when `options.faces` is set, otherwise on first use
    /// (a video timeline can enable faces in some sections only).
    detector: OnceLock<DetectorPool>,
    detector_init: Mutex<()>,
}

impl std::fmt::Debug for Processor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Processor").field("options", &self.options).field("device", &self.inference_device()).finish()
    }
}

impl Processor {
    /// Validates the options and loads the face detector if needed.
    pub fn new(options: Options) -> Result<Self> {
        options.validate()?;
        let processor = Self { options, detector: OnceLock::new(), detector_init: Mutex::new(()) };
        if processor.options.faces.is_some() {
            processor.detector()?;
        }
        Ok(processor)
    }

    /// The face detector, loaded on first use.
    fn detector(&self) -> Result<&DetectorPool> {
        if let Some(d) = self.detector.get() {
            return Ok(d);
        }
        let _guard = self.detector_init.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(d) = self.detector.get() {
            return Ok(d);
        }
        let faces = self.options.faces.clone().unwrap_or_default();
        let pool = DetectorPool::new(&faces, &self.options.compute)?;
        Ok(self.detector.get_or_init(|| pool))
    }

    /// The detector if `needed`.
    fn detector_if(&self, needed: bool) -> Result<Option<&DetectorPool>> {
        if needed {
            self.detector().map(Some)
        } else {
            Ok(None)
        }
    }

    pub fn options(&self) -> &Options {
        &self.options
    }

    /// Device running face detection, e.g. `cuda:0` (`None` until faces are
    /// detected).
    pub fn inference_device(&self) -> Option<&str> {
        self.detector.get().map(|d| d.device())
    }

    /// Processes `input` and writes the result to `output`.
    ///
    /// `output` may equal `input` to process in place. The result is first
    /// written to a temporary file next to `output` and renamed at the end.
    pub fn process_file(
        &self,
        input: &Path,
        output: &Path,
        progress: &(dyn Fn(Progress) + Sync),
        cancel: &CancelToken,
    ) -> Result<Report> {
        self.process_file_with(input, output, &Edits::default(), progress, cancel)
    }

    /// Like [`Processor::process_file`], with per-file decisions: per-section
    /// settings (videos) and faces to leave visible.
    pub fn process_file_with(
        &self,
        input: &Path,
        output: &Path,
        edits: &Edits,
        progress: &(dyn Fn(Progress) + Sync),
        cancel: &CancelToken,
    ) -> Result<Report> {
        edits.validate()?;
        let started = Instant::now();
        cancel.check()?;
        let kind = media::detect(input)?;
        let input_bytes = std::fs::metadata(input).map_err(|e| Error::io(input, e))?.len();
        let tmp = temp_path(output);
        let result = self.process_to(input, &tmp, kind, edits, progress, cancel);
        let mut report = match result {
            Ok(r) => r,
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
        };
        if let Err(e) = std::fs::rename(&tmp, output) {
            let _ = std::fs::remove_file(&tmp);
            return Err(Error::io(output, e));
        }
        report.input = input.to_path_buf();
        report.output = output.to_path_buf();
        report.input_bytes = input_bytes;
        report.output_bytes = std::fs::metadata(output).map(|m| m.len()).unwrap_or(0);
        report.elapsed = started.elapsed();
        Ok(report)
    }

    fn process_to(
        &self,
        input: &Path,
        tmp: &Path,
        kind: MediaKind,
        edits: &Edits,
        progress: &(dyn Fn(Progress) + Sync),
        cancel: &CancelToken,
    ) -> Result<Report> {
        let mut report = Report {
            input: PathBuf::new(),
            output: PathBuf::new(),
            kind,
            faces: 0,
            frames: None,
            frames_with_faces: None,
            timestamp: None,
            removed: Vec::new(),
            warnings: Vec::new(),
            notes: Vec::new(),
            input_bytes: 0,
            output_bytes: 0,
            elapsed: Duration::ZERO,
        };
        match kind {
            MediaKind::Image(format) => {
                progress(Progress { done: 0, total: Some(1), unit: "files" });
                let data = std::fs::read(input).map_err(|e| Error::io(input, e))?;
                let detector = self.detector_if(self.options.faces.is_some())?;
                let out =
                    photo::process(&data, format, &self.options, detector, &edits.visible, file_time(input), cancel)?;
                std::fs::write(tmp, &out.data).map_err(|e| Error::io(tmp, e))?;
                report.faces = out.faces.len() as u64;
                report.timestamp = out.timestamp;
                report.removed = out.changes.removed;
                report.warnings = out.changes.warnings;
                report.notes = out.notes;
                progress(Progress { done: 1, total: Some(1), unit: "files" });
            }
            MediaKind::Video => {
                let faces = self.options.faces.is_some() || edits.timeline.uses_faces();
                let detector = self.detector_if(faces)?;
                let out = video::process(input, tmp, &self.options, edits, detector, progress, cancel)?;
                report.faces = out.detections;
                report.timestamp = out.timestamp;
                if faces {
                    report.frames = Some(out.frames);
                    report.frames_with_faces = Some(out.frames_with_faces);
                }
                report.removed = out.changes.removed;
                report.warnings = out.changes.warnings;
                report.notes = out.notes;
            }
        }
        Ok(report)
    }

    /// Renders an upright preview of an image (longest side `max_side`) with
    /// faces redacted and the timestamp drawn, and returns it with the number
    /// of faces found.
    pub fn preview(&self, input: &Path, max_side: u32) -> Result<(image::DynamicImage, usize)> {
        if self.options.faces.is_none() && self.options.timestamp.is_none() {
            return Err(Error::InvalidOptions("neither face redaction nor the timestamp is enabled".into()));
        }
        let data = std::fs::read(input).map_err(|e| Error::io(input, e))?;
        let p = self.preview_data(&data, file_time(input), &[], max_side)?;
        Ok((p.image, p.faces.len()))
    }

    /// Like [`Processor::preview`], for an image already in memory;
    /// `file_time` stands in for the modification time when the timestamp
    /// overlay finds no capture date.
    /// Faces designated by `visible` are left uncovered; every detected face
    /// is returned with its box, for display.
    pub fn preview_data(
        &self,
        data: &[u8],
        file_time: Option<std::time::SystemTime>,
        visible: &[VisibleFace],
        max_side: u32,
    ) -> Result<PhotoPreview> {
        if self.options.faces.is_none() && self.options.timestamp.is_none() {
            return Err(Error::InvalidOptions("neither face redaction nor the timestamp is enabled".into()));
        }
        let detector = self.detector_if(self.options.faces.is_some())?;
        photo::preview(data, &self.options, detector, visible, file_time, max_side)
    }

    /// Renders the video frame shown at `time` (seconds from the first frame)
    /// with the settings of its section in `timeline`. Faces overlapping the
    /// boxes in `keep` (from [`Processor::track_visible_faces`] for this
    /// frame) are left uncovered.
    pub fn preview_video_frame(
        &self,
        input: &Path,
        time: f64,
        timeline: &Timeline,
        keep: &[FaceBox],
        max_side: u32,
    ) -> Result<FramePreview> {
        let faces = self.options.faces.is_some() || timeline.uses_faces();
        let detector = self.detector_if(faces)?;
        video::preview_frame(input, time, &self.options, timeline, keep, detector, max_side)
    }

    /// Follows the faces marked visible in `edits` through a video, exactly
    /// as processing would, to show where they stay uncovered.
    pub fn track_visible_faces(
        &self,
        input: &Path,
        edits: &Edits,
        progress: &(dyn Fn(Progress) + Sync),
        cancel: &CancelToken,
    ) -> Result<Vec<FaceTrack>> {
        edits.validate()?;
        video::track_visible(input, &self.options, edits, self.detector()?, progress, cancel)
    }
}

fn file_time(path: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Temporary sibling of `output`, keeping the extension (FFmpeg picks the
/// container from it).
fn temp_path(output: &Path) -> PathBuf {
    let name = output.file_name().and_then(|n| n.to_str()).unwrap_or("output");
    let ext = output.extension().and_then(|e| e.to_str()).map(|e| format!(".{e}")).unwrap_or_default();
    output.with_file_name(format!(".{name}.{}.partial{ext}", std::process::id()))
}

/// Lists the metadata found in a file, flagging what the `sensitive`
/// policy would remove.
pub fn inspect(path: &Path) -> Result<Vec<Finding>> {
    let mut out = Vec::new();
    match media::detect(path)? {
        MediaKind::Image(fmt) => {
            let data = std::fs::read(path).map_err(|e| Error::io(path, e))?;
            match fmt {
                ImageFormat::Jpeg => metadata::jpeg::inspect(&data, &mut out)?,
                ImageFormat::Png => metadata::png::inspect(&data, &mut out)?,
                ImageFormat::Webp => metadata::webp::inspect(&data, &mut out)?,
                ImageFormat::Tiff => out.push(Finding {
                    block: "TIFF".into(),
                    detail: "tag inspection not implemented; `--strip-metadata` rewrites all tags".into(),
                    sensitive: true,
                }),
                ImageFormat::Bmp => {}
            }
        }
        MediaKind::Video => out = video::inspect(path)?,
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_path_keeps_extension() {
        let t = temp_path(Path::new("/a/b/clip.mp4"));
        assert_eq!(t.parent(), Some(Path::new("/a/b")));
        assert!(t.to_str().unwrap().ends_with(".partial.mp4"));
        assert!(t.file_name().unwrap().to_str().unwrap().starts_with(".clip.mp4."));
    }
}
