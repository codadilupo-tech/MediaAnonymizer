//! Video pipeline built on the FFmpeg command-line tools.
//!
//! * **Metadata only**: a single `ffmpeg -c copy` remux. Audio and video
//!   packets are copied bit-exactly, only tags/streams are dropped.
//! * **Face redaction / timestamp overlay**: two FFmpeg processes connected through pipes:
//!
//! ```text
//!  decoder ffmpeg ──raw frames──▶ reader ─▶ detector workers (N) ─▶ reorder +
//!  (optional HW decode)                                              temporal smoothing
//!                                                                    + painting
//!  encoder ffmpeg ◀──raw frames── writer ◀───────────────────────────┘
//!  (optional HW encode, muxes original audio/subtitles by stream copy)
//! ```
//!
//! Frames stay in the source pixel format end to end (see [`pixfmt`]), every
//! stage runs concurrently, and buffers are recycled.
//!
//! A [`Timeline`] can change the settings per section of the video, or cut
//! sections out: cut frames are dropped before the encoder, and the audio is
//! trimmed to match (which requires re-encoding it).

pub mod encode;
pub mod ffmpeg;
pub mod metadata;
pub mod pixfmt;
pub mod probe;
mod temporal;

pub use ffmpeg::{capabilities, Capabilities};

use crate::detect::{detection_dims, DetectorPool, Face, Point, RgbFrame};
use crate::edits::{Edits, FaceBox, PreviewFace, Tracker};
use crate::metadata::{Changes, Finding};
use crate::options::{FaceOptions, HwAccel, Options};
use crate::overlay::{DateSource, DateTime, Stamp};
use crate::redact::{self, Plane};
use crate::timeline::{FrameSections, Timeline};
use crate::{CancelToken, Error, Progress, Result};
use crossbeam_channel::{bounded, Receiver, Sender};
use pixfmt::{Family, PixFmt};
use serde::Serialize;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use temporal::Temporal;

/// Longest side of the detector input for videos when not configured.
pub const DEFAULT_DETECTION_SIZE: u32 = 960;

/// Result of processing one video.
#[derive(Debug, Default)]
pub struct Processed {
    pub changes: Changes,
    pub notes: Vec<String>,
    pub frames: u64,
    pub frames_with_faces: u64,
    pub detections: u64,
    /// Frames removed by cut sections of the timeline.
    pub cut_frames: u64,
    /// Text of the timestamp overlay and where the date came from.
    pub timestamp: Option<String>,
}

/// Lists the metadata of a video file.
pub fn inspect(path: &Path) -> Result<Vec<Finding>> {
    let p = probe::probe(path)?;
    let mut out = Vec::new();
    metadata::inspect(&p, &mut out);
    Ok(out)
}

/// Duration, rate and size of a video, e.g. to draw a timeline.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct VideoInfo {
    /// Seconds.
    pub duration: f64,
    /// Frames per second (the rate frames are processed at).
    pub frame_rate: f64,
    /// Number of frames, when known or estimable.
    pub frames: Option<u64>,
    /// Displayed size (after rotation).
    pub width: u32,
    pub height: u32,
}

/// Reads the [`VideoInfo`] of a file.
pub fn info(path: &Path) -> Result<VideoInfo> {
    let probe = probe::probe(path)?;
    let vs = probe.video().ok_or_else(|| Error::Unsupported("no video stream found".into()))?;
    let (num, den) = vs.frame_rate().unwrap_or((30, 1));
    let frame_rate = num as f64 / den as f64;
    let frames = vs.frame_count(probe.duration());
    let duration = probe
        .duration()
        .or_else(|| vs.duration.as_deref().and_then(|d| d.parse().ok()))
        .or_else(|| frames.map(|n| n as f64 / frame_rate))
        .unwrap_or(0.0);
    let (w, h) = (vs.width.unwrap_or(0), vs.height.unwrap_or(0));
    let (width, height) = if matches!(vs.rotation(), 90 | 270) { (h, w) } else { (w, h) };
    Ok(VideoInfo { duration, frame_rate, frames, width, height })
}

/// One frame rendered with the settings of its section.
#[derive(Debug, Clone)]
pub struct FramePreview {
    /// The decoded frame, upright, longest side at most `max_side`.
    pub original: image::DynamicImage,
    /// The same frame as it will appear in the output.
    pub redacted: image::DynamicImage,
    /// Faces detected on this frame (empty when faces are not covered in
    /// its section).
    pub faces: Vec<PreviewFace>,
    /// The frame belongs to a cut section (it will not be in the output).
    pub cut: bool,
}

/// Decodes the frame shown at `time` (seconds from the first frame) and
/// applies the face redaction and timestamp of its section. Detected faces
/// overlapping a box of `keep` (positions of tracked visible faces on this
/// frame, see [`track_visible`]) are left uncovered.
///
/// The detection runs on this frame alone: in the real output, temporal
/// smoothing can also cover faces missed on a single frame.
pub fn preview_frame(
    input: &Path,
    time: f64,
    opts: &Options,
    timeline: &Timeline,
    keep: &[FaceBox],
    detector: Option<&DetectorPool>,
    max_side: u32,
) -> Result<FramePreview> {
    let probe = probe::probe(input)?;
    let vs = probe.video().ok_or_else(|| Error::Unsupported("no video stream found".into()))?;
    let (num, den) = vs.frame_rate().unwrap_or((30, 1));
    let fps = num as f64 / den as f64;
    let (mut width, mut height) = (vs.width.unwrap_or(0) as usize, vs.height.unwrap_or(0) as usize);
    if matches!(vs.rotation(), 90 | 270) {
        std::mem::swap(&mut width, &mut height);
    }
    let idx = (time.max(0.0) * fps).round() as u64;
    // Accurate input seeking returns the first frame at or after the target:
    // aim half a frame early so rounding cannot skip to the next one.
    let seek = (idx as f64 - 0.5).max(0.0) / fps;
    let mut cmd = base_command();
    cmd.args(["-ss", &format!("{seek:.6}")]).arg("-i").arg(input);
    cmd.args(["-map", &format!("0:{}", vs.index), "-frames:v", "1", "-an", "-sn", "-dn"]);
    cmd.args(["-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"]);
    let data = ffmpeg::run(cmd)?;
    let len = width * height * 3;
    if width == 0 || data.len() < len {
        return Err(Error::Ffmpeg(format!("no frame at {time:.3}s")));
    }
    let frame = image::RgbImage::from_raw(width as u32, height as u32, data[..len].to_vec())
        .ok_or_else(|| Error::Ffmpeg("invalid frame size".into()))?;
    let original = image::DynamicImage::ImageRgb8(frame);

    let sections = FrameSections::new(opts, timeline, fps);
    let section = sections.at(idx);
    let mut redacted = original.clone();
    let mut faces = Vec::new();
    if let (Some(f), Some(pool)) = (section.faces, detector) {
        let found = crate::photo::detect(&redacted, image::metadata::Orientation::NoTransforms, f, pool)?;
        let (w, h) = (width as f32, height as f32);
        let mut visible = vec![false; found.len()];
        for k in keep.iter().map(|k| k.to_face(w, h)) {
            let best = found
                .iter()
                .enumerate()
                .filter(|(i, _)| !visible[*i])
                .map(|(i, face)| (i, face.iou(&k)))
                .filter(|&(_, iou)| iou >= KEEP_IOU)
                .max_by(|a, b| a.1.total_cmp(&b.1));
            if let Some((i, _)) = best {
                visible[i] = true;
            }
        }
        faces = found
            .iter()
            .zip(&visible)
            .map(|(face, &v)| PreviewFace { bbox: FaceBox::of(face, w, h), visible: v })
            .collect();
        let covered: Vec<Face> = found.into_iter().zip(visible).filter(|(_, v)| !v).map(|(f, _)| f).collect();
        crate::photo::paint(&mut redacted, &redact::regions(&covered, f), f)?;
    }
    if let Some(t) = section.timestamp {
        if let Some((date, _)) = capture_date(input, &probe, vs) {
            if let Some(stamp) = Stamp::render(&date.format(&t.format), width as u32, height as u32, t) {
                crate::photo::blend_stamp(&mut redacted, &stamp)?;
            }
        }
    }
    Ok(FramePreview {
        original: original.thumbnail(max_side, max_side),
        redacted: redacted.thumbnail(max_side, max_side),
        faces,
        cut: section.cut,
    })
}

/// Overlap between a detection and a tracked box for the preview to treat
/// them as the same face.
const KEEP_IOU: f32 = 0.3;

/// Frames a tracked visible face may go undetected before its track ends.
fn track_tolerance(opts: &Options) -> u64 {
    (2 * opts.video.detect_every as u64).max(12)
}

/// Where a face marked visible was followed.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FaceTrack {
    /// Index of the mark in [`Edits::visible`].
    pub mark: usize,
    /// Position on each frame where the face is left visible, by frame index.
    pub frames: std::collections::BTreeMap<u64, FaceBox>,
    /// First and last time (seconds) the face is visible; `None` if no face
    /// was found at the mark.
    pub span: Option<(f64, f64)>,
    /// The face was lost before the end of the video: it is covered again
    /// from there on.
    pub lost: bool,
}

/// Follows the faces marked visible in `edits`, the same way processing
/// does, and returns where each one stays uncovered. Only the part of the
/// video from the first mark until every track has ended is decoded, at the
/// detector's resolution.
pub fn track_visible(
    input: &Path,
    opts: &Options,
    edits: &Edits,
    detector: &DetectorPool,
    progress: &(dyn Fn(Progress) + Sync),
    cancel: &CancelToken,
) -> Result<Vec<FaceTrack>> {
    let probe = probe::probe(input)?;
    let vs = probe.video().ok_or_else(|| Error::Unsupported("no video stream found".into()))?;
    let (num, den) = vs.frame_rate().unwrap_or((30, 1));
    let fps = num as f64 / den as f64;
    let (mut width, mut height) = (vs.width.unwrap_or(0), vs.height.unwrap_or(0));
    if matches!(vs.rotation(), 90 | 270) {
        std::mem::swap(&mut width, &mut height);
    }
    if width == 0 || height == 0 {
        return Err(Error::Ffmpeg("cannot determine the video dimensions".into()));
    }
    let max_side = opts.faces.as_ref().and_then(|f| f.detection_size).unwrap_or(DEFAULT_DETECTION_SIZE);
    let (dw, dh) = detection_dims(width, height, max_side);
    let mut tracker = Tracker::new(&edits.visible, fps, dw as f32, dh as f32, track_tolerance(opts));
    let mut tracks: Vec<FaceTrack> = (0..edits.visible.len())
        .map(|mark| FaceTrack { mark, frames: Default::default(), span: None, lost: false })
        .collect();
    let Some(first) = tracker.first_frame() else { return Ok(tracks) };
    let total = vs.frame_count(probe.duration());
    let sections = FrameSections::new(opts, &edits.timeline, fps);
    let detect_every = opts.video.detect_every.max(1) as u64;

    let mut cmd = base_command();
    cmd.args(hwaccel_args(opts, ffmpeg::capabilities()?)?);
    cmd.args(["-ss", &format!("{:.6}", (first as f64 - 0.5).max(0.0) / fps)]).arg("-i").arg(input);
    cmd.args(["-map", &format!("0:{}", vs.index), "-an", "-sn", "-dn", "-fps_mode", "passthrough"]);
    cmd.args(["-vf", &format!("scale={dw}:{dh}:flags=area"), "-f", "rawvideo", "-pix_fmt", "rgb24", "pipe:1"]);
    cmd.stdout(Stdio::piped());
    let mut proc = ffmpeg::Process::spawn(cmd, "ffmpeg (tracking)")?;
    let mut out = proc.child.stdout.take().expect("stdout is piped");
    let mut buf = vec![0u8; dw as usize * dh as usize * 3];
    let mut idx = first;
    let mut last = first;
    loop {
        if cancel.is_cancelled() {
            proc.kill();
            return Err(Error::Cancelled);
        }
        match read_frame(&mut out, &mut buf) {
            Ok(true) => {}
            Ok(false) => break,
            Err(e) => {
                proc.kill();
                return Err(e);
            }
        }
        let section = sections.at(idx);
        let faces = match section.faces {
            Some(f) if !section.cut && idx % detect_every == 0 => {
                let frame = RgbFrame { width: dw, height: dh, data: std::mem::take(&mut buf) };
                let found = detector.detect(&frame, f.min_confidence);
                buf = frame.data;
                Some(found?)
            }
            _ => None,
        };
        for (face, owner) in faces.iter().flatten().zip(tracker.step(idx, faces.as_deref())) {
            if let Some(m) = owner {
                tracks[m].frames.insert(idx, FaceBox::of(face, dw as f32, dh as f32));
            }
        }
        progress(Progress { done: idx, total, unit: "frames" });
        last = idx;
        idx += 1;
        if tracker.finished() {
            break;
        }
    }
    proc.kill();
    for (t, span) in tracks.iter_mut().zip(tracker.spans()) {
        t.span = span.frames.map(|(a, b)| (a as f64 / fps, b as f64 / fps));
        t.lost = span.lost || span.frames.is_none_or(|(_, b)| b < last);
    }
    Ok(tracks)
}

/// Processes `input` into `output` (whose extension selects the container),
/// applying the per-section settings and visible faces of `edits`.
pub fn process(
    input: &Path,
    output: &Path,
    opts: &Options,
    edits: &Edits,
    detector: Option<&DetectorPool>,
    progress: &(dyn Fn(Progress) + Sync),
    cancel: &CancelToken,
) -> Result<Processed> {
    let probe = probe::probe(input)?;
    let container = output.extension().and_then(|e| e.to_str()).unwrap_or_default().to_ascii_lowercase();
    let timeline = &edits.timeline;
    let needs_faces = opts.faces.is_some() || timeline.uses_faces();
    if needs_faces && detector.is_none() {
        return Err(Error::InvalidOptions("face redaction requested without a detector".into()));
    }
    if !needs_faces && opts.timestamp.is_none() && !timeline.has_cuts() {
        return remux(input, output, &probe, &container, opts, progress, cancel);
    }
    let detector = detector.filter(|_| needs_faces);
    reencode(input, output, &probe, &container, opts, edits, detector, progress, cancel)
}

/// Capture date of a video: QuickTime `creationdate` (local time), then the
/// container / stream `creation_time` (UTC), then the file modification time.
fn capture_date(input: &Path, probe: &probe::Probe, vs: &probe::Stream) -> Option<(DateTime, String)> {
    let tags = [
        (&probe.format.tags, "com.apple.quicktime.creationdate", "QuickTime creation date"),
        (&probe.format.tags, "creation_time", "container creation_time (UTC)"),
        (&vs.tags, "creation_time", "stream creation_time (UTC)"),
    ];
    tags.into_iter().find_map(|(t, key, label)| Some((DateTime::parse(t.get(key)?)?, label.to_string()))).or_else(
        || {
            let mtime = std::fs::metadata(input).and_then(|m| m.modified()).ok()?;
            Some((DateTime::from_system(mtime)?, DateSource::FileModified.to_string()))
        },
    )
}

fn is_mov_like(container: &str) -> bool {
    matches!(container, "mp4" | "m4v" | "mov" | "qt" | "3gp" | "3g2")
}

fn base_command() -> std::process::Command {
    let mut c = ffmpeg::command(ffmpeg::ffmpeg_path());
    c.args(["-hide_banner", "-nostdin", "-loglevel", "error", "-y"]);
    c
}

/// Metadata-only processing: stream copy, no re-encoding.
fn remux(
    input: &Path,
    output: &Path,
    probe: &probe::Probe,
    container: &str,
    opts: &Options,
    progress: &(dyn Fn(Progress) + Sync),
    cancel: &CancelToken,
) -> Result<Processed> {
    let mut changes = Changes::default();
    let mux = metadata::MuxInputs {
        probe,
        source: 0,
        replaced_video: None,
        policy: opts.metadata,
        faces: false,
        cut: false,
        mov_like: is_mov_like(container),
    };
    let mut cmd = base_command();
    cmd.arg("-i").arg(input);
    cmd.args(metadata::mux_args(&mux, &mut changes));
    cmd.args(["-c", "copy", "-progress", "pipe:1", "-nostats"]).arg(output);
    cmd.stdout(Stdio::piped());
    let mut proc = ffmpeg::Process::spawn(cmd, "ffmpeg (remux)")?;

    // `-progress` prints key=value lines; `out_time_us` tells how far we are.
    let total_us = probe.duration().map(|d| (d * 1e6) as u64);
    let stdout = proc.child.stdout.take().expect("stdout is piped");
    for line in BufReader::new(stdout).lines().map_while(|l| l.ok()) {
        if cancel.is_cancelled() {
            proc.kill();
            return Err(Error::Cancelled);
        }
        if let Some(us) = line.strip_prefix("out_time_us=").and_then(|v| v.trim().parse::<u64>().ok()) {
            progress(Progress { done: us.min(total_us.unwrap_or(u64::MAX)), total: total_us, unit: "µs" });
        }
    }
    proc.finish()?;
    Ok(Processed {
        changes,
        notes: vec!["audio and video streams copied bit-exactly (no re-encoding)".into()],
        ..Default::default()
    })
}

fn hwaccel_args(opts: &Options, caps: &Capabilities) -> Result<Vec<String>> {
    let v = &opts.video;
    let gpu = opts.compute.gpu_index.to_string();
    let (name, device): (&str, Option<String>) = match v.hwaccel {
        HwAccel::None => return Ok(vec![]),
        HwAccel::Auto => return Ok(vec!["-hwaccel".into(), "auto".into()]),
        HwAccel::Cuda => ("cuda", Some(gpu)),
        HwAccel::Vaapi => ("vaapi", Some(v.vaapi_device.clone().unwrap_or_else(|| "/dev/dri/renderD128".into()))),
        HwAccel::Qsv => ("qsv", None),
        HwAccel::VideoToolbox => ("videotoolbox", None),
        HwAccel::D3d11va => ("d3d11va", Some(gpu)),
        HwAccel::Dxva2 => ("dxva2", None),
        HwAccel::Vulkan => ("vulkan", None),
    };
    if !caps.hwaccels.iter().any(|h| h == name) {
        return Err(Error::Hardware(format!(
            "hardware decoder `{name}` is not supported by this FFmpeg build (available: {})",
            if caps.hwaccels.is_empty() { "none".into() } else { caps.hwaccels.join(", ") }
        )));
    }
    let mut a = vec!["-hwaccel".into(), name.into()];
    if let Some(d) = device {
        a.extend(["-hwaccel_device".into(), d]);
    }
    Ok(a)
}

/// Frame geometry and format shared by all stages.
#[derive(Clone, Copy)]
struct Layout {
    pix: PixFmt,
    width: u32,
    height: u32,
    full_range: bool,
    frame_bytes: usize,
}

impl Layout {
    /// Byte ranges of each plane inside a frame buffer.
    fn planes(&self) -> Vec<(usize, u32, u32)> {
        let bps = self.pix.bytes_per_sample();
        let mut off = 0;
        (0..self.pix.plane_count())
            .map(|i| {
                let (pw, ph) = self.pix.plane_dims(i, self.width, self.height);
                let start = off;
                off += pw as usize * ph as usize * bps;
                (start, pw, ph)
            })
            .collect()
    }
}

/// A raw frame. Storage is `u16` so that 16-bit planes are always aligned.
struct Frame {
    idx: u64,
    buf: Vec<u16>,
}

impl Frame {
    fn bytes(&self, len: usize) -> &[u8] {
        &bytemuck::cast_slice(&self.buf)[..len]
    }
    fn bytes_mut(&mut self, len: usize) -> &mut [u8] {
        &mut bytemuck::cast_slice_mut(&mut self.buf)[..len]
    }
}

/// First error raised by any stage; also stops the other stages.
#[derive(Default)]
struct Failure {
    flag: AtomicBool,
    error: Mutex<Option<Error>>,
}

impl Failure {
    fn set(&self, e: Error) {
        let mut slot = self.error.lock().unwrap_or_else(|p| p.into_inner());
        if slot.is_none() {
            *slot = Some(e);
        }
        self.flag.store(true, Ordering::SeqCst);
    }
    fn failed(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
    fn take(&self) -> Option<Error> {
        self.error.lock().unwrap_or_else(|p| p.into_inner()).take()
    }
}

/// Audio encoder used when cuts force the audio to be re-encoded: lossless
/// where the container supports it well, otherwise a high-bitrate lossy codec.
fn cut_audio_codec(container: &str) -> (&'static str, &'static [&'static str], bool) {
    match container {
        "mkv" | "mka" => ("flac", &[], true),
        "mov" | "qt" => ("alac", &[], true),
        "avi" | "wav" => ("pcm_s16le", &[], true),
        "webm" => ("libopus", &["-b:a", "256k"], false),
        _ => ("aac", &["-b:a", "320k"], false),
    }
}

/// `aselect` expression keeping the audio outside the cut ranges (seconds of
/// the source timeline).
fn keep_audio_expr(cuts: &[(f64, f64)], offset: f64) -> String {
    let terms: Vec<String> = cuts
        .iter()
        .map(|&(a, b)| {
            if b.is_finite() {
                format!("between(t,{:.6},{:.6})", a + offset, b + offset)
            } else {
                format!("gte(t,{:.6})", a + offset)
            }
        })
        .collect();
    format!("not({})", terms.join("+"))
}

/// Decode → (detect + paint faces) / (draw timestamp) / (drop cut frames) → encode.
#[allow(clippy::too_many_arguments)]
fn reencode(
    input: &Path,
    output: &Path,
    probe: &probe::Probe,
    container: &str,
    opts: &Options,
    edits: &Edits,
    detector: Option<&DetectorPool>,
    progress: &(dyn Fn(Progress) + Sync),
    cancel: &CancelToken,
) -> Result<Processed> {
    let timeline = &edits.timeline;
    let vs = probe.video().ok_or_else(|| Error::Unsupported("no video stream found".into()))?;
    let caps = ffmpeg::capabilities()?;
    let mut changes = Changes::default();
    let mut notes = Vec::new();

    let src_pix = vs.pix_fmt.clone().unwrap_or_else(|| "yuv420p".into());
    let (pix, exact) = PixFmt::for_source(&src_pix);
    if !exact {
        changes.warn(format!("pixel format `{src_pix}` is processed as `{}`", pix.name));
    }
    let (mut width, mut height) = (vs.width.unwrap_or(0), vs.height.unwrap_or(0));
    if width == 0 || height == 0 {
        return Err(Error::Ffmpeg("cannot determine the video dimensions".into()));
    }
    let rotation = vs.rotation();
    if rotation == 90 || rotation == 270 {
        std::mem::swap(&mut width, &mut height);
    }
    if rotation != 0 {
        notes.push(format!("{rotation}° display rotation applied to the pixels"));
    }
    let (num, den) = vs.frame_rate().unwrap_or((30, 1));
    if vs.avg_frame_rate.is_some() && vs.r_frame_rate.is_some() && vs.avg_frame_rate != vs.r_frame_rate {
        changes.warn(format!("variable frame rate source: frames are re-timed at a constant {num}/{den} fps"));
    }
    let layout = Layout {
        pix,
        width,
        height,
        full_range: pix.full_range || vs.color_range.as_deref() == Some("pc"),
        frame_bytes: pix.frame_size(width, height),
    };
    let total = vs.frame_count(probe.duration());
    let sections = FrameSections::new(opts, timeline, num as f64 / den as f64);
    let cuts = timeline.cut_ranges();

    // Timestamp: rendered once, frames are already upright.
    let mut timestamp = None;
    let stamp = match &opts.timestamp {
        Some(t) => match capture_date(input, probe, vs) {
            Some((date, source)) => {
                let text = date.format(&t.format);
                timestamp = Some(format!("{text} ({source})"));
                Stamp::render(&text, width, height, t)
            }
            None => {
                changes.warn("no capture date found: timestamp not drawn");
                None
            }
        },
        None => None,
    };

    let enc = encode::plan(&vs.codec_name, container, &pix, &opts.video, opts.compute.gpu_index, caps)?;
    notes.push(format!(
        "video re-encoded with {} ({})",
        enc.name,
        if enc.lossless { "mathematically lossless" } else { "lossy" }
    ));
    notes.extend(enc.notes.iter().cloned());

    // --- decoder -----------------------------------------------------------
    let mut dec = base_command();
    dec.args(hwaccel_args(opts, caps)?);
    dec.arg("-i").arg(input);
    dec.args(["-map", &format!("0:{}", vs.index), "-an", "-sn", "-dn", "-fps_mode", "passthrough"]);
    dec.args(["-f", "rawvideo", "-pix_fmt", pix.name, "pipe:1"]);
    dec.stdout(Stdio::piped());

    // --- encoder -----------------------------------------------------------
    let color_args: Vec<String> = [
        ("-color_range", vs.color_range.as_deref()),
        ("-colorspace", vs.color_space.as_deref()),
        ("-color_primaries", vs.color_primaries.as_deref()),
        ("-color_trc", vs.color_transfer.as_deref()),
    ]
    .into_iter()
    .filter_map(|(k, v)| v.filter(|v| *v != "unknown" && *v != "reserved").map(|v| [k.to_string(), v.to_string()]))
    .flatten()
    .collect();
    let mut encc = base_command();
    encc.args(&enc.global_args);
    encc.args(["-f", "rawvideo", "-pix_fmt", pix.name, "-video_size", &format!("{width}x{height}")]);
    encc.args(["-framerate", &format!("{num}/{den}")]);
    encc.args(&color_args);
    let start = vs.start_seconds();
    // With cuts, the trimmed audio is re-timed from zero: so is the video.
    if start > 0.0 && cuts.is_empty() {
        encc.args(["-itsoffset", &format!("{start:.6}")]);
    }
    encc.args(["-i", "pipe:0"]).arg("-i").arg(input);
    let mux = metadata::MuxInputs {
        probe,
        source: 1,
        replaced_video: Some(vs.index),
        policy: opts.metadata,
        faces: detector.is_some(),
        cut: !cuts.is_empty(),
        mov_like: is_mov_like(container),
    };
    encc.args(metadata::mux_args(&mux, &mut changes));
    encc.args(["-c", "copy", "-c:v", &enc.name]).args(&enc.args).args(&color_args);
    if !cuts.is_empty() && probe.streams.iter().any(|s| s.codec_type == "audio") {
        let (codec, args, lossless) = cut_audio_codec(container);
        encc.args(["-af", &format!("aselect='{}',asetpts=N/SR/TB", keep_audio_expr(&cuts, start))]);
        encc.args(["-c:a", codec]).args(args);
        notes.push(format!(
            "audio re-encoded with {codec} ({}) to apply the cuts",
            if lossless { "lossless" } else { "high bitrate" }
        ));
    }
    encc.arg(output);
    encc.stdin(Stdio::piped());

    let mut decoder = ffmpeg::Process::spawn(dec, "ffmpeg decoder")?;
    let mut encoder = match ffmpeg::Process::spawn(encc, "ffmpeg encoder") {
        Ok(e) => e,
        Err(e) => {
            decoder.kill();
            return Err(e);
        }
    };
    let dec_out = decoder.child.stdout.take().expect("stdout is piped");
    let enc_in = encoder.child.stdin.take().expect("stdin is piped");

    let mut tracker = (!edits.visible.is_empty()).then(|| {
        Tracker::new(&edits.visible, num as f64 / den as f64, width as f32, height as f32, track_tolerance(opts))
    });
    let failure = Failure::default();
    let stats = run_pipeline(
        dec_out,
        enc_in,
        layout,
        opts,
        &sections,
        detector,
        tracker.as_mut(),
        stamp.as_ref(),
        total,
        progress,
        cancel,
        &failure,
    );

    if cancel.is_cancelled() || failure.failed() {
        decoder.kill();
        encoder.kill();
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        // Prefer FFmpeg's own explanation when one of the processes died.
        let err = failure.take().unwrap_or_else(|| Error::Ffmpeg("pipeline failed".into()));
        return Err(match err {
            Error::Io { .. } => encoder.error("stopped unexpectedly"),
            e => e,
        });
    }
    decoder.finish()?;
    if stats.frames > 0 && stats.frames == stats.cut {
        encoder.kill();
        return Err(Error::InvalidOptions("every frame of the video is cut".into()));
    }
    encoder.finish()?;
    let Stats { frames, with_faces: frames_with_faces, detections, cut: cut_frames, visible } = stats;
    if let Some(t) = &tracker {
        let spans = t.spans();
        let found = spans.iter().filter(|s| s.frames.is_some()).count();
        notes
            .push(format!("{visible} face detections left visible as marked ({found} of {} marks found)", spans.len()));
        if found < spans.len() {
            changes.warn("some faces marked visible were not found: they stay covered");
        }
    }
    if frames == 0 {
        return Err(Error::Ffmpeg("the decoder produced no frames".into()));
    }
    if cut_frames > 0 {
        notes.push(format!("{cut_frames} frames cut ({:.2}s)", cut_frames as f64 * den as f64 / num as f64));
    }
    Ok(Processed { changes, notes, frames, frames_with_faces, detections, cut_frames, timestamp })
}

/// Frame counters of a pipeline run.
#[derive(Debug, Clone, Copy, Default)]
struct Stats {
    /// Decoded frames.
    frames: u64,
    with_faces: u64,
    detections: u64,
    /// Frames dropped by cut sections.
    cut: u64,
    /// Detections left uncovered because of a visible-face mark.
    visible: u64,
}

/// Runs the threaded frame pipeline.
#[allow(clippy::too_many_arguments)]
fn run_pipeline(
    mut source: impl Read + Send,
    mut sink: impl Write + Send,
    layout: Layout,
    opts: &Options,
    sections: &FrameSections<'_>,
    detector: Option<&DetectorPool>,
    mut tracker: Option<&mut Tracker>,
    stamp: Option<&Stamp>,
    total: Option<u64>,
    progress: &(dyn Fn(Progress) + Sync),
    cancel: &CancelToken,
    failure: &Failure,
) -> Stats {
    let workers = detector.map_or(1, |pool| pool.size().max(1) + 1);
    let detect_every = opts.video.detect_every.max(1) as u64;
    let depth = workers * 2;
    // Recycled buffers: enough for every stage plus the temporal window.
    let max_buffers = depth * 3 + opts.video.persistence as usize + 4;
    let (free_tx, free_rx): (Sender<Vec<u16>>, Receiver<Vec<u16>>) = bounded(max_buffers);
    let (raw_tx, raw_rx) = bounded::<Frame>(depth);
    let (det_tx, det_rx) = bounded::<(Frame, Option<Vec<Face>>)>(depth);
    let (out_tx, out_rx) = bounded::<Frame>(depth);
    let words = layout.frame_bytes.div_ceil(2);
    let stop = || cancel.is_cancelled() || failure.failed();

    std::thread::scope(|s| {
        // Reader: decoder stdout -> raw frames.
        s.spawn(|| {
            let mut allocated = 0usize;
            for idx in 0.. {
                if stop() {
                    break;
                }
                let buf = match free_rx.try_recv() {
                    Ok(b) => b,
                    Err(_) if allocated < max_buffers => {
                        allocated += 1;
                        vec![0u16; words]
                    }
                    Err(_) => match free_rx.recv() {
                        Ok(b) => b,
                        Err(_) => break,
                    },
                };
                let mut frame = Frame { idx, buf };
                match read_frame(&mut source, frame.bytes_mut(layout.frame_bytes)) {
                    Ok(true) => {
                        if raw_tx.send(frame).is_err() {
                            break;
                        }
                    }
                    Ok(false) => break,
                    Err(e) => {
                        failure.set(e);
                        break;
                    }
                }
            }
            drop(raw_tx);
        });

        // Detector workers.
        for _ in 0..workers {
            let (raw_rx, det_tx) = (raw_rx.clone(), det_tx.clone());
            s.spawn(move || {
                for frame in raw_rx.iter() {
                    if stop() {
                        break;
                    }
                    let section = sections.at(frame.idx);
                    let found = match (section.faces, detector) {
                        (Some(fopts), Some(pool)) if frame.idx % detect_every == 0 => {
                            match detect_frame(&frame, &layout, fopts, pool) {
                                Ok(f) => Some(f),
                                Err(e) => {
                                    failure.set(e);
                                    break;
                                }
                            }
                        }
                        _ => None,
                    };
                    if det_tx.send((frame, found)).is_err() {
                        break;
                    }
                }
            });
        }
        drop(raw_rx);
        drop(det_tx);

        // Writer: painted frames -> encoder stdin. Owns the receiving end so
        // that the coordinator unblocks if the writer stops.
        let recycle = free_tx.clone();
        let free_tx = free_tx;
        s.spawn(move || {
            for frame in out_rx.iter() {
                if failure.failed() {
                    break;
                }
                if let Err(e) = sink.write_all(frame.bytes(layout.frame_bytes)) {
                    failure.set(Error::io("encoder pipe", e));
                    break;
                }
                let _ = free_tx.try_send(frame.buf);
            }
            // Closing stdin tells the encoder the stream is over.
            drop(sink);
        });

        // Coordinator (this thread): reorder, smooth, paint.
        let mut reorder = std::collections::BTreeMap::new();
        let mut next = 0u64;
        let mut temporal = Temporal::new(opts.video.persistence);
        let mut stats = Stats::default();
        let emit = |ready: Vec<(u64, Frame, Vec<Face>)>, stats: &mut Stats| {
            for (idx, mut frame, found) in ready {
                let section = sections.at(idx);
                if section.cut {
                    stats.cut += 1;
                    let _ = recycle.try_send(frame.buf);
                    continue;
                }
                // Detections carried over from a neighbouring section are
                // painted with this section's settings, or not at all.
                let found = if section.faces.is_some() { found } else { Vec::new() };
                let stamp = stamp.filter(|_| section.timestamp.is_some());
                if !found.is_empty() {
                    stats.with_faces += 1;
                }
                if !found.is_empty() || stamp.is_some() {
                    paint_frame(&mut frame, &layout, &found, section.faces, stamp);
                }
                if out_tx.send(frame).is_err() {
                    break;
                }
            }
        };
        for (frame, faces) in det_rx.iter() {
            if stop() {
                break;
            }
            reorder.insert(frame.idx, (frame, faces));
            while let Some((frame, faces)) = reorder.remove(&next) {
                // Faces marked visible are removed before smoothing, so that
                // neighbouring frames do not cover them either.
                let faces = match tracker.as_deref_mut() {
                    Some(t) => {
                        let owners = t.step(frame.idx, faces.as_deref());
                        let faces = faces.unwrap_or_default();
                        stats.visible += owners.iter().filter(|o| o.is_some()).count() as u64;
                        faces.into_iter().zip(owners).filter(|(_, o)| o.is_none()).map(|(f, _)| f).collect()
                    }
                    None => faces.unwrap_or_default(),
                };
                stats.detections += faces.len() as u64;
                stats.frames += 1;
                next += 1;
                emit(temporal.push(frame.idx, frame, faces), &mut stats);
                progress(Progress { done: stats.frames, total, unit: "frames" });
            }
        }
        drop(det_rx);
        if !stop() {
            emit(temporal.flush(), &mut stats);
        }
        drop(out_tx);
        stats
    })
}

/// Reads exactly one frame. `Ok(false)` on a clean end of stream.
fn read_frame(src: &mut impl Read, buf: &mut [u8]) -> Result<bool> {
    let mut filled = 0;
    while filled < buf.len() {
        match src.read(&mut buf[filled..]) {
            Ok(0) if filled == 0 => return Ok(false),
            Ok(0) => return Err(Error::Ffmpeg("decoder output ended in the middle of a frame".into())),
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(Error::io("decoder pipe", e)),
        }
    }
    Ok(true)
}

/// Builds the downscaled RGB detector input straight from the native planes.
fn detect_frame(frame: &Frame, l: &Layout, fopts: &FaceOptions, pool: &DetectorPool) -> Result<Vec<Face>> {
    let max = fopts.detection_size.unwrap_or(DEFAULT_DETECTION_SIZE);
    let (dw, dh) = detection_dims(l.width, l.height, max);
    let planes = l.planes();
    let bytes = frame.bytes(l.frame_bytes);
    let words: &[u16] = &frame.buf;
    let deep = l.pix.bytes_per_sample() == 2;
    let shift = l.pix.bits.saturating_sub(8);
    // Sample of plane `p` at plane coordinates, normalised to 8 bits.
    let get = |p: usize, x: u32, y: u32| -> f32 {
        let (off, pw, _) = planes[p];
        let i = y as usize * pw as usize + x as usize;
        if deep {
            (words[off / 2 + i] >> shift) as f32
        } else {
            bytes[off + i] as f32
        }
    };
    let (sx, sy) = l.pix.chroma_shift;
    let (full, family) = (l.full_range, l.pix.family);
    let rgb = RgbFrame::downscale(l.width, l.height, dw, dh, |x, y| match family {
        Family::Gray => {
            let v = get(0, x, y) as u8;
            [v, v, v]
        }
        Family::Gbr => [get(2, x, y) as u8, get(0, x, y) as u8, get(1, x, y) as u8],
        Family::Yuv => {
            let (mut yv, mut u, mut v) =
                (get(0, x, y), get(1, x >> sx, y >> sy) - 128.0, get(2, x >> sx, y >> sy) - 128.0);
            if !full {
                yv = (yv - 16.0) * (255.0 / 219.0);
                u *= 255.0 / 224.0;
                v *= 255.0 / 224.0;
            }
            // BT.601; the exact matrix is irrelevant for detection.
            let r = yv + 1.402 * v;
            let g = yv - 0.344_136 * u - 0.714_136 * v;
            let b = yv + 1.772 * u;
            [r.clamp(0.0, 255.0) as u8, g.clamp(0.0, 255.0) as u8, b.clamp(0.0, 255.0) as u8]
        }
    });
    let (kx, ky) = (l.width as f32 / dw as f32, l.height as f32 / dh as f32);
    let faces = pool.detect(&rgb, fopts.min_confidence)?;
    Ok(faces.into_iter().map(|f| f.map(|p| Point::new(p.x * kx, p.y * ky))).collect())
}

/// Paints the face regions and the timestamp on every color plane (alpha is
/// left untouched).
fn paint_frame(frame: &mut Frame, l: &Layout, faces: &[Face], fopts: Option<&FaceOptions>, stamp: Option<&Stamp>) {
    let regions = fopts.map(|f| redact::regions(faces, f)).unwrap_or_default();
    let (style, k) = fopts.map_or((Default::default(), 1.0), |f| (f.style, f.strength));
    let planes = l.planes();
    let color_planes = if l.pix.family == Family::Gray { 1 } else { 3 };
    let (w, h) = (l.width as f32, l.height as f32);
    let deep = l.pix.bytes_per_sample() == 2;

    macro_rules! paint_planes {
        ($data:expr, $ty:ty, $div:expr) => {{
            let mut rest: &mut [$ty] = $data;
            let mut slices = Vec::with_capacity(color_planes);
            let mut consumed = 0;
            for &(off, pw, ph) in planes.iter().take(color_planes) {
                let len = pw as usize * ph as usize;
                let (_, tail) = rest.split_at_mut(off / $div - consumed);
                let (plane, tail) = tail.split_at_mut(len);
                consumed = off / $div + len;
                rest = tail;
                slices.push((plane, pw, ph));
            }
            rayon::scope(|s| {
                for (i, (plane, pw, ph)) in slices.into_iter().enumerate() {
                    let regions = &regions;
                    let black = [l.pix.black(i, l.full_range) as $ty];
                    let white = [l.pix.white(i, l.full_range) as $ty];
                    s.spawn(move |_| {
                        let mut p = Plane::new(plane, pw as usize, ph as usize, pw as usize, 1, 1)
                            .with_scale(pw as f32 / w, ph as f32 / h);
                        redact::paint(&mut p, regions, style, k, &black);
                        if let Some(stamp) = stamp {
                            p.blend_stamp(stamp, &white, &black);
                        }
                    });
                }
            });
        }};
    }
    if deep {
        let len = l.frame_bytes / 2;
        paint_planes!(&mut frame.buf[..len], u16, 2)
    } else {
        let len = l.frame_bytes;
        paint_planes!(frame.bytes_mut(len), u8, 1)
    }
}
