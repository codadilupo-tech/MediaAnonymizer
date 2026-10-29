//! Command-line definition.

use clap::{Args, Parser, Subcommand, ValueEnum};
use mediaanon_core as core;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(
    name = "mediaanon",
    version,
    about = "Remove metadata and redact faces in photos and videos, without losing quality.",
    long_about = None,
    after_help = "Examples:\n  \
        mediaanon process --strip-metadata photo.jpg\n  \
        mediaanon process --strip-sensitive --faces --face-style blur -r ~/Pictures -o ~/Anonymized\n  \
        mediaanon process --faces --face-mode eyes-only --hwaccel cuda --device cuda clip.mp4\n  \
        mediaanon process --strip-metadata --timestamp-position bottom-left photo.jpg\n  \
        mediaanon inspect photo.jpg\n  \
        mediaanon devices"
)]
pub struct Cli {
    /// Increase log verbosity (-v, -vv).
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    /// Only print errors.
    #[arg(short, long, global = true)]
    pub quiet: bool,

    /// Print machine-readable JSON instead of human-readable output.
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Anonymize files and directories.
    #[command(visible_alias = "run")]
    Process(Box<ProcessArgs>),
    /// Show the metadata stored in files and what `--strip-sensitive` would remove.
    Inspect {
        /// Files to inspect.
        #[arg(required = true)]
        files: Vec<PathBuf>,
    },
    /// List inference devices, hardware decoders and encoders available on this machine.
    Devices,
}

#[derive(Debug, Args)]
pub struct ProcessArgs {
    /// Files or directories to process.
    #[arg(required = true)]
    pub inputs: Vec<PathBuf>,

    /// Output file (single input) or directory. Defaults to writing next to
    /// each input with `--suffix` added to the name.
    #[arg(short, long)]
    pub output: Option<PathBuf>,

    /// Suffix added to output file names when `--output` is not a file.
    #[arg(long, default_value = ".anon")]
    pub suffix: String,

    /// Replace the input files with the processed ones.
    #[arg(long, conflicts_with_all = ["output"])]
    pub in_place: bool,

    /// Recurse into sub-directories.
    #[arg(short, long)]
    pub recursive: bool,

    /// Overwrite existing output files.
    #[arg(long)]
    pub overwrite: bool,

    /// Show what would be processed without writing anything.
    #[arg(long)]
    pub dry_run: bool,

    /// Number of photos processed in parallel (videos are processed one at a
    /// time, each using all cores). Defaults to the number of CPU cores.
    #[arg(short = 'j', long)]
    pub jobs: Option<usize>,

    #[command(flatten)]
    pub metadata: MetadataArgs,

    #[command(flatten)]
    pub faces: FaceArgs,

    #[command(flatten)]
    pub timestamp: TimestampArgs,

    #[command(flatten)]
    pub compute: ComputeArgs,

    #[command(flatten)]
    pub video: VideoArgs,
}

#[derive(Debug, Args)]
#[command(next_help_heading = "Metadata")]
pub struct MetadataArgs {
    /// Remove all metadata (EXIF, XMP, IPTC, comments, thumbnails, container
    /// tags, chapters, GPS tracks...). Color profiles are kept.
    #[arg(long, short = 'm')]
    pub strip_metadata: bool,

    /// Remove only location and personal data (GPS, owner, serial numbers,
    /// maker notes, XMP, thumbnails...), keeping camera settings and dates.
    #[arg(long, short = 's', conflicts_with = "strip_metadata")]
    pub strip_sensitive: bool,
}

#[derive(Debug, Args)]
#[command(next_help_heading = "Face redaction")]
pub struct FaceArgs {
    /// Detect and cover faces. Any other `--face-*` option implies it.
    #[arg(long, short = 'f')]
    pub faces: bool,

    /// Part of the face to cover.
    #[arg(long, value_enum)]
    pub face_mode: Option<FaceModeArg>,

    /// How the covered area is rendered.
    #[arg(long, value_enum)]
    pub face_style: Option<FaceStyleArg>,

    /// Outline of the full-face area.
    #[arg(long, value_enum)]
    pub face_shape: Option<FaceShapeArg>,

    /// Margin around each face, as a fraction of its size.
    #[arg(long, value_name = "FRACTION")]
    pub padding: Option<f32>,

    /// Detection threshold (0-1). Lower catches more faces but also more false positives.
    #[arg(long, value_name = "SCORE")]
    pub min_confidence: Option<f32>,

    /// Blur radius / mosaic block size multiplier.
    #[arg(long, value_name = "FACTOR")]
    pub strength: Option<f32>,

    /// Longest side (px) of the image given to the detector. Larger finds
    /// smaller faces but is slower. Default: 1920 for photos, 960 for videos.
    #[arg(long, value_name = "PX")]
    pub detection_size: Option<u32>,

    /// Custom YuNet-compatible ONNX face detection model.
    #[arg(long, value_name = "FILE")]
    pub model: Option<PathBuf>,
}

impl FaceArgs {
    pub fn enabled(&self) -> bool {
        self.faces
            || self.face_mode.is_some()
            || self.face_style.is_some()
            || self.face_shape.is_some()
            || self.padding.is_some()
            || self.min_confidence.is_some()
            || self.strength.is_some()
            || self.detection_size.is_some()
            || self.model.is_some()
    }
}

#[derive(Debug, Args)]
#[command(next_help_heading = "Timestamp overlay")]
pub struct TimestampArgs {
    /// Draw the capture date on the picture (read from EXIF / video tags
    /// before metadata removal, else the file modification time). Any other
    /// `--timestamp-*` option implies it.
    #[arg(long, short = 't')]
    pub timestamp: bool,

    /// Corner where the date is drawn.
    #[arg(long, value_enum, value_name = "CORNER")]
    pub timestamp_position: Option<CornerArg>,

    /// Date pattern: %Y %y %m %b %d %H %M %S %%.
    #[arg(long, value_name = "PATTERN")]
    pub timestamp_format: Option<String>,

    /// Text height as a fraction of the picture height.
    #[arg(long, value_name = "FRACTION")]
    pub timestamp_size: Option<f32>,
}

impl TimestampArgs {
    pub fn enabled(&self) -> bool {
        self.timestamp
            || self.timestamp_position.is_some()
            || self.timestamp_format.is_some()
            || self.timestamp_size.is_some()
    }
}

#[derive(Debug, Args)]
#[command(next_help_heading = "Compute")]
pub struct ComputeArgs {
    /// Device running face detection.
    #[arg(long, value_enum, default_value_t = DeviceArg::Auto)]
    pub device: DeviceArg,

    /// GPU index (inference and, with CUDA/NVENC/D3D11, video decoding/encoding).
    #[arg(long, default_value_t = 0)]
    pub gpu: u32,

    /// Maximum worker threads (default: all cores).
    #[arg(long)]
    pub threads: Option<usize>,
}

#[derive(Debug, Args)]
#[command(next_help_heading = "Video")]
pub struct VideoArgs {
    /// Hardware-accelerated decoding.
    #[arg(long, value_enum, default_value_t = HwAccelArg::None)]
    pub hwaccel: HwAccelArg,

    /// Encoder family used when faces are redacted. `auto` keeps the source
    /// codec and matches `--hwaccel` when possible.
    #[arg(long, value_enum, default_value_t = EncoderArg::Auto)]
    pub encoder: EncoderArg,

    /// Exact FFmpeg encoder name (e.g. libx265, hevc_nvenc). Overrides `--encoder`.
    #[arg(long, value_name = "NAME")]
    pub encoder_name: Option<String>,

    /// Re-encoding quality.
    #[arg(long, value_enum, default_value_t = QualityArg::Lossless)]
    pub quality: QualityArg,

    /// Encoder speed preset (e.g. `slow` for x264, `p5` for NVENC).
    #[arg(long)]
    pub preset: Option<String>,

    /// Run face detection every N frames (1 = every frame, safest).
    #[arg(long, default_value_t = 1, value_name = "N")]
    pub detect_every: u32,

    /// Keep each detection for N frames before and after it was seen, to
    /// cover frames where the detector misses a face. Default: max(3, detect-every - 1).
    #[arg(long, value_name = "N")]
    pub persistence: Option<u32>,

    /// VA-API render node.
    #[arg(long, value_name = "PATH")]
    pub vaapi_device: Option<String>,

    /// JSON file with per-file decisions applied to every input: keyframes
    /// that change the face redaction or the timestamp from a given time or
    /// cut sections out (videos), and faces to leave visible (see docs/CLI.md).
    #[arg(long, value_name = "FILE")]
    pub timeline: Option<std::path::PathBuf>,
}

macro_rules! value_enum {
    ($name:ident => $target:ty { $($variant:ident => $core:ident),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
        pub enum $name { $($variant),+ }
        impl From<$name> for $target {
            fn from(v: $name) -> Self {
                match v { $($name::$variant => <$target>::$core),+ }
            }
        }
    };
}

value_enum!(FaceModeArg => core::FaceMode { EyesOnly => EyesOnly, FullFace => FullFace });
value_enum!(FaceStyleArg => core::RedactionStyle { BlackBand => BlackBand, Blur => Blur, Pixelate => Pixelate });
value_enum!(CornerArg => core::Corner { BottomLeft => BottomLeft, BottomRight => BottomRight });
value_enum!(FaceShapeArg => core::FaceShape { Ellipse => Ellipse, Rectangle => Rectangle });
value_enum!(DeviceArg => core::InferenceDevice {
    Auto => Auto, Cpu => Cpu, Cuda => Cuda, Tensorrt => TensorRt, Directml => DirectMl,
    Coreml => CoreMl, Openvino => OpenVino, Rocm => Rocm,
});
value_enum!(HwAccelArg => core::HwAccel {
    None => None, Auto => Auto, Cuda => Cuda, Vaapi => Vaapi, Qsv => Qsv,
    Videotoolbox => VideoToolbox, D3d11va => D3d11va, Dxva2 => Dxva2, Vulkan => Vulkan,
});
value_enum!(EncoderArg => core::EncoderBackend {
    Auto => Auto, Software => Software, Nvenc => Nvenc, Qsv => Qsv, Vaapi => Vaapi,
    Videotoolbox => VideoToolbox, Amf => Amf,
});
value_enum!(QualityArg => core::VideoQuality {
    Lossless => Lossless, VisuallyLossless => VisuallyLossless, High => High,
});

impl ProcessArgs {
    /// Translates the flags into library options.
    pub fn options(&self) -> core::Options {
        let metadata = if self.metadata.strip_metadata {
            core::MetadataPolicy::All
        } else if self.metadata.strip_sensitive {
            core::MetadataPolicy::Sensitive
        } else {
            core::MetadataPolicy::Keep
        };
        let f = &self.faces;
        let faces = f.enabled().then(|| {
            let d = core::FaceOptions::default();
            core::FaceOptions {
                mode: f.face_mode.map(Into::into).unwrap_or(d.mode),
                style: f.face_style.map(Into::into).unwrap_or(d.style),
                shape: f.face_shape.map(Into::into).unwrap_or(d.shape),
                padding: f.padding.unwrap_or(d.padding),
                min_confidence: f.min_confidence.unwrap_or(d.min_confidence),
                strength: f.strength.unwrap_or(d.strength),
                detection_size: f.detection_size,
                model_path: f.model.clone(),
            }
        });
        let t = &self.timestamp;
        let timestamp = t.enabled().then(|| {
            let d = core::TimestampOptions::default();
            core::TimestampOptions {
                position: t.timestamp_position.map(Into::into).unwrap_or(d.position),
                format: t.timestamp_format.clone().unwrap_or(d.format),
                size: t.timestamp_size.unwrap_or(d.size),
            }
        });
        let v = &self.video;
        core::Options {
            metadata,
            faces,
            timestamp,
            compute: core::ComputeOptions {
                device: self.compute.device.into(),
                gpu_index: self.compute.gpu,
                threads: self.compute.threads,
            },
            video: core::VideoOptions {
                hwaccel: v.hwaccel.into(),
                encoder: v.encoder.into(),
                quality: v.quality.into(),
                encoder_name: v.encoder_name.clone(),
                preset: v.preset.clone(),
                detect_every: v.detect_every,
                persistence: v.persistence.unwrap_or_else(|| v.detect_every.saturating_sub(1).max(3)),
                vaapi_device: v.vaapi_device.clone(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn face_options_imply_faces() {
        let cli = Cli::parse_from(["mediaanon", "process", "--face-style", "pixelate", "a.jpg"]);
        let Command::Process(p) = cli.command else { panic!() };
        let o = p.options();
        assert_eq!(o.faces.unwrap().style, core::RedactionStyle::Pixelate);
        assert_eq!(o.metadata, core::MetadataPolicy::Keep);
    }

    #[test]
    fn timestamp_options_imply_timestamp() {
        let cli = Cli::parse_from(["mediaanon", "process", "--timestamp-position", "bottom-left", "a.jpg"]);
        let Command::Process(p) = cli.command else { panic!() };
        let o = p.options();
        assert_eq!(o.timestamp.unwrap().position, core::Corner::BottomLeft);
        assert!(o.faces.is_none());
        let cli = Cli::parse_from(["mediaanon", "process", "-t", "a.jpg"]);
        let Command::Process(p) = cli.command else { panic!() };
        assert_eq!(p.options().timestamp.unwrap().position, core::Corner::BottomRight);
    }

    #[test]
    fn persistence_follows_detect_every() {
        let cli = Cli::parse_from(["mediaanon", "process", "-m", "--detect-every", "10", "a.mp4"]);
        let Command::Process(p) = cli.command else { panic!() };
        assert_eq!(p.options().video.persistence, 9);
        assert!(p.options().validate().is_ok());
    }
}
