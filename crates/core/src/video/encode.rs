//! Encoder selection and quality settings.
//!
//! Re-encoding is only needed when faces are redacted. By default the source
//! codec is kept and the stream is encoded **mathematically lossless**, so
//! that every pixel outside the faces is identical to the decoded source.

use super::ffmpeg::Capabilities;
use super::pixfmt::{Family, PixFmt};
use crate::options::{EncoderBackend, HwAccel, VideoOptions, VideoQuality};
use crate::{Error, Result};

/// Encoder command-line fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncoderPlan {
    pub name: String,
    /// Arguments placed before the inputs (hardware device initialisation).
    pub global_args: Vec<String>,
    /// Arguments placed after `-c:v <name>`.
    pub args: Vec<String>,
    /// Whether the selected configuration is mathematically lossless.
    pub lossless: bool,
    pub notes: Vec<String>,
}

/// Intra/lossless codecs that we can always re-encode with the same encoder.
const LOSSLESS_CODECS: &[&str] = &["ffv1", "huffyuv", "ffvhuff", "utvideo", "png", "qtrle", "magicyuv", "rawvideo"];

fn backend(opts: &VideoOptions) -> EncoderBackend {
    match (opts.encoder, opts.hwaccel) {
        (EncoderBackend::Auto, HwAccel::Cuda) => EncoderBackend::Nvenc,
        (EncoderBackend::Auto, HwAccel::Qsv) => EncoderBackend::Qsv,
        (EncoderBackend::Auto, HwAccel::Vaapi) => EncoderBackend::Vaapi,
        (EncoderBackend::Auto, HwAccel::VideoToolbox) => EncoderBackend::VideoToolbox,
        (EncoderBackend::Auto, _) => EncoderBackend::Software,
        (b, _) => b,
    }
}

/// Whether the hardware back-end has a true lossless mode.
fn hw_supports_lossless(b: EncoderBackend) -> bool {
    matches!(b, EncoderBackend::Software | EncoderBackend::Nvenc)
}

fn software_candidates(codec: &str, container: &str, pix: &PixFmt, quality: VideoQuality) -> Vec<&'static str> {
    let rgb = pix.family == Family::Gbr;
    if let Some(c) = LOSSLESS_CODECS.iter().find(|c| **c == codec) {
        return vec![c];
    }
    let lossless = quality == VideoQuality::Lossless;
    let by_codec: Vec<&'static str> = match (codec, lossless) {
        ("h264", _) if rgb && lossless => vec!["libx264rgb"],
        ("h264", _) => vec!["libx264"],
        ("hevc", _) => vec!["libx265"],
        ("vp9", _) => vec!["libvpx-vp9"],
        ("av1", true) => vec!["libaom-av1"],
        ("av1", false) => vec!["libsvtav1", "libaom-av1", "librav1e"],
        ("vp8", false) => vec!["libvpx"],
        ("prores", false) => vec!["prores_ks", "prores"],
        ("mjpeg", false) => vec!["mjpeg"],
        ("mpeg2video", false) => vec!["mpeg2video"],
        ("mpeg4", false) => vec!["mpeg4"],
        _ => vec![],
    };
    if !by_codec.is_empty() {
        return by_codec;
    }
    // The source codec cannot be (losslessly) re-encoded: pick a codec the
    // container accepts.
    match container {
        "webm" => vec!["libvpx-vp9"],
        "avi" | "mxf" if lossless => vec!["ffv1"],
        _ if rgb && lossless => vec!["libx264rgb", "ffv1"],
        _ => vec!["libx264"],
    }
}

fn hardware_name(b: EncoderBackend, codec: &str) -> Option<String> {
    let suffix = match b {
        EncoderBackend::Nvenc => "nvenc",
        EncoderBackend::Qsv => "qsv",
        EncoderBackend::Vaapi => "vaapi",
        EncoderBackend::VideoToolbox => "videotoolbox",
        EncoderBackend::Amf => "amf",
        EncoderBackend::Software | EncoderBackend::Auto => return None,
    };
    let family = match codec {
        "h264" | "hevc" | "av1" => codec,
        "vp9" if matches!(b, EncoderBackend::Qsv | EncoderBackend::Vaapi) => codec,
        "prores" if b == EncoderBackend::VideoToolbox => codec,
        // Anything else: HEVC is the best widely supported hardware codec.
        _ => "hevc",
    };
    Some(format!("{family}_{suffix}"))
}

/// Picks the encoder for a stream.
pub fn plan(
    source_codec: &str,
    container: &str,
    pix: &PixFmt,
    opts: &VideoOptions,
    gpu: u32,
    caps: &Capabilities,
) -> Result<EncoderPlan> {
    let mut notes = Vec::new();
    let quality = opts.quality;
    let name = if let Some(n) = &opts.encoder_name {
        n.clone()
    } else {
        let mut b = backend(opts);
        if quality == VideoQuality::Lossless && !hw_supports_lossless(b) {
            if opts.encoder == EncoderBackend::Auto {
                notes.push(format!("{b} has no lossless mode: using a software encoder"));
                b = EncoderBackend::Software;
            } else {
                notes.push(format!("{b} has no lossless mode: encoding at its highest quality instead"));
            }
        }
        match hardware_name(b, source_codec) {
            Some(n) => n,
            None => {
                let candidates = software_candidates(source_codec, container, pix, quality);
                candidates.iter().find(|c| caps.has_encoder(c)).copied().unwrap_or(candidates[0]).to_string()
            }
        }
    };
    if !caps.has_encoder(&name) {
        return Err(Error::Hardware(format!(
            "encoder `{name}` is not available in this FFmpeg build ({}); run `mediaanon devices` to list \
             the available encoders",
            if caps.version.is_empty() { "unknown version" } else { &caps.version }
        )));
    }
    let (args, lossless) = quality_args(&name, quality, pix, opts.preset.as_deref(), gpu);
    if quality == VideoQuality::Lossless && !lossless {
        notes.push(format!("`{name}` is not lossless; encoded at its highest quality"));
    }
    let mut global_args = Vec::new();
    let mut args = args;
    if name.ends_with("_vaapi") {
        let dev = opts.vaapi_device.clone().unwrap_or_else(|| "/dev/dri/renderD128".into());
        global_args.extend(["-vaapi_device".into(), dev]);
        let upload = if pix.bits > 8 { "format=p010,hwupload" } else { "format=nv12,hwupload" };
        args.extend(["-vf".into(), upload.into()]);
    }
    Ok(EncoderPlan { name, global_args, args, lossless, notes })
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

/// Encoder arguments for a quality target; returns `(args, is_lossless)`.
fn quality_args(name: &str, q: VideoQuality, pix: &PixFmt, preset: Option<&str>, gpu: u32) -> (Vec<String>, bool) {
    use VideoQuality::*;
    let preset_or = |d: &str| preset.unwrap_or(d).to_string();
    let lossless = q == Lossless;
    match name {
        "libx264" | "libx264rgb" => {
            let rc = match q {
                Lossless => s(&["-qp", "0"]),
                VisuallyLossless => s(&["-crf", "16"]),
                High => s(&["-crf", "20"]),
            };
            ([rc, s(&["-preset"]), vec![preset_or("medium")]].concat(), lossless)
        }
        "libx265" => {
            let (rc, params) = match q {
                Lossless => (vec![], "lossless=1:log-level=error"),
                VisuallyLossless => (s(&["-crf", "18"]), "log-level=error"),
                High => (s(&["-crf", "22"]), "log-level=error"),
            };
            ([rc, s(&["-x265-params", params, "-preset"]), vec![preset_or("medium")]].concat(), lossless)
        }
        "libvpx-vp9" => {
            let rc = match q {
                Lossless => s(&["-lossless", "1"]),
                VisuallyLossless => s(&["-crf", "15", "-b:v", "0"]),
                High => s(&["-crf", "24", "-b:v", "0"]),
            };
            ([rc, s(&["-row-mt", "1", "-deadline", "good", "-cpu-used"]), vec![preset_or("2")]].concat(), lossless)
        }
        "libvpx" => {
            let crf = if q == High { "10" } else { "4" };
            (s(&["-crf", crf, "-b:v", "100M", "-qmin", "0", "-deadline", "good", "-cpu-used", "2"]), false)
        }
        "libaom-av1" => {
            let rc = match q {
                Lossless => s(&["-aom-params", "lossless=1"]),
                VisuallyLossless => s(&["-crf", "18", "-b:v", "0"]),
                High => s(&["-crf", "26", "-b:v", "0"]),
            };
            ([rc, s(&["-row-mt", "1", "-cpu-used"]), vec![preset_or("6")]].concat(), lossless)
        }
        "libsvtav1" => {
            let crf = if q == High { "28" } else { "18" };
            (vec!["-crf".into(), crf.into(), "-preset".into(), preset_or("6")], false)
        }
        "librav1e" => (s(&["-qp", if q == High { "80" } else { "40" }]), false),
        "prores_ks" | "prores" => {
            let profile = if pix.alpha || pix.chroma_shift == (0, 0) { "4" } else { "3" };
            (s(&["-profile:v", profile, "-vendor", "apl0"]), false)
        }
        "mjpeg" | "mpeg2video" | "mpeg4" => (s(&["-q:v", "1"]), false),
        "ffv1" => (s(&["-level", "3", "-slicecrc", "1", "-slices", "16", "-g", "1"]), true),
        n if LOSSLESS_CODECS.contains(&n) => (vec![], true),
        n if n.ends_with("_nvenc") => {
            let mut a = vec!["-preset".into(), preset_or("p7"), "-gpu".into(), gpu.to_string()];
            a.extend(match q {
                Lossless => s(&["-tune", "lossless"]),
                VisuallyLossless => s(&["-tune", "hq", "-rc", "vbr", "-cq", "16", "-b:v", "0"]),
                High => s(&["-tune", "hq", "-rc", "vbr", "-cq", "22", "-b:v", "0"]),
            });
            (a, lossless && !n.starts_with("av1"))
        }
        n if n.ends_with("_qsv") => {
            let gq = match q {
                Lossless => "1",
                VisuallyLossless => "18",
                High => "23",
            };
            (vec!["-global_quality".into(), gq.into(), "-preset".into(), preset_or("veryslow")], false)
        }
        n if n.ends_with("_vaapi") => {
            let qp = match q {
                Lossless => "1",
                VisuallyLossless => "18",
                High => "23",
            };
            (s(&["-rc_mode", "CQP", "-qp", qp]), false)
        }
        n if n.ends_with("_videotoolbox") => {
            let qv = match q {
                Lossless => "100",
                VisuallyLossless => "80",
                High => "65",
            };
            (s(&["-q:v", qv]), false)
        }
        n if n.ends_with("_amf") => {
            let qp = match q {
                Lossless => "0",
                VisuallyLossless => "16",
                High => "22",
            };
            (s(&["-rc", "cqp", "-qp_i", qp, "-qp_p", qp, "-quality", "quality"]), false)
        }
        // Unknown encoder chosen explicitly: let FFmpeg use its defaults.
        _ => (vec![], false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(encoders: &[&str]) -> Capabilities {
        Capabilities {
            version: "test".into(),
            hwaccels: vec![],
            encoders: encoders.iter().map(|e| e.to_string()).collect(),
        }
    }

    fn yuv420() -> PixFmt {
        PixFmt::for_source("yuv420p").0
    }

    #[test]
    fn lossless_h264_by_default() {
        let p = plan("h264", "mp4", &yuv420(), &VideoOptions::default(), 0, &caps(&["libx264"])).unwrap();
        assert_eq!(p.name, "libx264");
        assert!(p.lossless);
        assert!(p.args.windows(2).any(|w| w == ["-qp", "0"]));
    }

    #[test]
    fn cuda_uses_nvenc_lossless() {
        let o = VideoOptions { hwaccel: HwAccel::Cuda, ..Default::default() };
        let p = plan("hevc", "mov", &yuv420(), &o, 1, &caps(&["hevc_nvenc"])).unwrap();
        assert_eq!(p.name, "hevc_nvenc");
        assert!(p.lossless);
        assert!(p.args.windows(2).any(|w| w == ["-gpu", "1"]));
    }

    #[test]
    fn qsv_lossless_falls_back_to_software() {
        let o = VideoOptions { hwaccel: HwAccel::Qsv, ..Default::default() };
        let p = plan("h264", "mp4", &yuv420(), &o, 0, &caps(&["libx264", "h264_qsv"])).unwrap();
        assert_eq!(p.name, "libx264");
        assert!(!p.notes.is_empty());
    }

    #[test]
    fn non_lossless_codec_switches_for_lossless() {
        let p = plan("mpeg4", "avi", &yuv420(), &VideoOptions::default(), 0, &caps(&["ffv1", "mpeg4"])).unwrap();
        assert_eq!(p.name, "ffv1");
        let p = plan("vp8", "webm", &yuv420(), &VideoOptions::default(), 0, &caps(&["libvpx-vp9"])).unwrap();
        assert_eq!(p.name, "libvpx-vp9");
    }

    #[test]
    fn missing_encoder_is_an_error() {
        let o = VideoOptions { encoder: EncoderBackend::Nvenc, ..Default::default() };
        assert!(plan("h264", "mp4", &yuv420(), &o, 0, &caps(&["libx264"])).is_err());
    }

    #[test]
    fn vaapi_uploads_frames() {
        let o = VideoOptions {
            encoder: EncoderBackend::Vaapi,
            quality: VideoQuality::High,
            vaapi_device: Some("/dev/dri/renderD129".into()),
            ..Default::default()
        };
        let p = plan("h264", "mp4", &yuv420(), &o, 0, &caps(&["h264_vaapi"])).unwrap();
        assert_eq!(p.global_args, ["-vaapi_device", "/dev/dri/renderD129"]);
        assert!(p.args.ends_with(&["-vf".to_string(), "format=nv12,hwupload".to_string()]));
    }
}
