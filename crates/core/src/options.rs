//! User-facing configuration.
//!
//! Every feature of the tool is opt-in: an [`Options`] value with everything
//! disabled is valid but [`Options::validate`] rejects it because it would not
//! do anything.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

/// Complete configuration of a processing run.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Options {
    /// What to do with embedded metadata.
    pub metadata: MetadataPolicy,
    /// Face redaction; `None` disables face detection entirely.
    pub faces: Option<FaceOptions>,
    /// Burn the capture date into the pixels; `None` disables it.
    pub timestamp: Option<TimestampOptions>,
    /// Which device runs face detection.
    pub compute: ComputeOptions,
    /// Video-specific settings (ignored for photos).
    pub video: VideoOptions,
}

impl Options {
    /// Returns an error if the options would be a no-op or are inconsistent.
    pub fn validate(&self) -> crate::Result<()> {
        if self.metadata == MetadataPolicy::Keep && self.faces.is_none() && self.timestamp.is_none() {
            return Err(crate::Error::InvalidOptions(
                "nothing to do: enable metadata removal, face redaction and/or the timestamp overlay".into(),
            ));
        }
        if let Some(t) = &self.timestamp {
            if !(t.size > 0.0 && t.size <= 0.5) {
                return Err(crate::Error::InvalidOptions("timestamp size must be in (0, 0.5]".into()));
            }
            if t.format.trim().is_empty() {
                return Err(crate::Error::InvalidOptions("timestamp format must not be empty".into()));
            }
        }
        if let Some(f) = &self.faces {
            f.validate()?;
        }
        if self.video.detect_every == 0 {
            return Err(crate::Error::InvalidOptions("detect-every must be >= 1".into()));
        }
        if self.video.persistence < self.video.detect_every.saturating_sub(1) {
            return Err(crate::Error::InvalidOptions(
                "persistence must be >= detect-every - 1, otherwise frames between detections stay uncovered".into(),
            ));
        }
        Ok(())
    }
}

/// How embedded metadata is treated.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MetadataPolicy {
    /// Leave metadata untouched (except embedded previews when faces are
    /// redacted, see `docs/PRIVACY.md`).
    #[default]
    Keep,
    /// Remove location (GPS) and other personally identifying fields — owner,
    /// author, serial numbers, maker notes, XMP, IPTC, comments — while keeping
    /// technical data such as camera model, exposure and capture date.
    Sensitive,
    /// Remove every metadata block that is not required to render the media
    /// correctly (color profile and orientation-independent structural data
    /// are kept).
    All,
}

/// Which part of a face gets covered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FaceMode {
    /// A band across both eyes, aligned with the eye line.
    EyesOnly,
    /// The whole face.
    #[default]
    FullFace,
}

/// How the covered region is rendered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RedactionStyle {
    /// Solid black fill. Irreversible; the safest choice.
    #[default]
    BlackBand,
    /// Strong Gaussian-like blur.
    Blur,
    /// Mosaic of large flat blocks.
    Pixelate,
}

/// Outline of the full-face region.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FaceShape {
    /// Ellipse inscribed in the (padded) face box.
    #[default]
    Ellipse,
    /// The (padded) face box itself.
    Rectangle,
}

/// Face redaction settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FaceOptions {
    pub mode: FaceMode,
    pub style: RedactionStyle,
    pub shape: FaceShape,
    /// Extra margin around each face, as a fraction of the face size.
    pub padding: f32,
    /// Detector score threshold in `0..=1`. Lower values catch more faces at
    /// the cost of more false positives (privacy favours lower values).
    pub min_confidence: f32,
    /// Multiplier for blur radius / pixel block size (1.0 = default).
    pub strength: f32,
    /// Longest side, in pixels, of the frame fed to the detector. `None`
    /// picks a sensible default per media type. Larger finds smaller faces
    /// but is slower.
    pub detection_size: Option<u32>,
    /// Optional path to a custom YuNet-compatible ONNX model; the bundled
    /// model is used otherwise.
    pub model_path: Option<std::path::PathBuf>,
}

impl Default for FaceOptions {
    fn default() -> Self {
        Self {
            mode: FaceMode::FullFace,
            style: RedactionStyle::BlackBand,
            shape: FaceShape::Ellipse,
            padding: 0.15,
            min_confidence: 0.6,
            strength: 1.0,
            detection_size: None,
            model_path: None,
        }
    }
}

impl FaceOptions {
    /// Checks that the values are in range.
    pub fn validate(&self) -> crate::Result<()> {
        if !(0.0..=1.0).contains(&self.min_confidence) {
            return Err(crate::Error::InvalidOptions("min confidence must be in 0..=1".into()));
        }
        if !(0.0..=4.0).contains(&self.padding) {
            return Err(crate::Error::InvalidOptions("padding must be in 0..=4".into()));
        }
        if !(self.strength > 0.0 && self.strength <= 10.0) {
            return Err(crate::Error::InvalidOptions("strength must be in (0, 10]".into()));
        }
        Ok(())
    }
}

/// Corner of the (upright) picture where the timestamp is drawn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Corner {
    BottomLeft,
    #[default]
    BottomRight,
}

/// Capture-date overlay settings.
///
/// The date is read from the file *before* metadata is removed (EXIF
/// `DateTimeOriginal`, video `creation_time`...), falling back to the file
/// modification time, and drawn as white text with a dark outline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TimestampOptions {
    pub position: Corner,
    /// `strftime`-like pattern supporting `%Y %m %d %H %M %S %y %b %%`.
    pub format: String,
    /// Text height as a fraction of the picture height.
    pub size: f32,
}

impl Default for TimestampOptions {
    fn default() -> Self {
        Self { position: Corner::BottomRight, format: "%Y-%m-%d %H:%M:%S".into(), size: 0.035 }
    }
}

/// Where face detection runs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum InferenceDevice {
    /// Try GPU providers compiled into this build in order of preference and
    /// fall back to the CPU.
    #[default]
    Auto,
    Cpu,
    /// NVIDIA CUDA.
    Cuda,
    /// NVIDIA TensorRT (falls back to CUDA kernels for unsupported ops).
    TensorRt,
    /// DirectML (any DirectX 12 GPU on Windows).
    DirectMl,
    /// Apple Core ML (Neural Engine / GPU).
    CoreMl,
    /// Intel OpenVINO (CPU, iGPU, NPU).
    OpenVino,
    /// AMD ROCm.
    Rocm,
}

/// Compute settings for face detection.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ComputeOptions {
    pub device: InferenceDevice,
    /// GPU ordinal for multi-GPU machines. Applies to inference and, for
    /// CUDA/NVENC, to video decoding/encoding.
    pub gpu_index: u32,
    /// Worker threads. `None` uses all logical cores.
    pub threads: Option<usize>,
}

/// FFmpeg hardware-accelerated decoding backend.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HwAccel {
    /// Software decoding.
    #[default]
    None,
    /// Let FFmpeg pick the best available backend.
    Auto,
    Cuda,
    Vaapi,
    Qsv,
    VideoToolbox,
    D3d11va,
    Dxva2,
    Vulkan,
}

/// Video encoder family. `Auto` keeps the source codec and uses the hardware
/// encoder matching [`VideoOptions::hwaccel`] when one exists.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EncoderBackend {
    #[default]
    Auto,
    /// CPU encoders (x264, x265, libvpx, SVT-AV1, FFV1...).
    Software,
    /// NVIDIA NVENC.
    Nvenc,
    /// Intel Quick Sync.
    Qsv,
    /// VA-API (Linux, Intel/AMD).
    Vaapi,
    /// Apple VideoToolbox.
    VideoToolbox,
    /// AMD AMF (Windows).
    Amf,
}

/// Quality target used when a video has to be re-encoded (face redaction).
///
/// Removing metadata alone never re-encodes: streams are copied bit-exactly.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum VideoQuality {
    /// Mathematically lossless: every untouched pixel is bit-identical to the
    /// decoded source. Files are large.
    #[default]
    Lossless,
    /// Visually indistinguishable from the source at a fraction of the size.
    VisuallyLossless,
    /// Good quality, comparable to the source bitrate.
    High,
}

/// Video processing settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct VideoOptions {
    pub hwaccel: HwAccel,
    pub encoder: EncoderBackend,
    pub quality: VideoQuality,
    /// Explicit FFmpeg encoder name (e.g. `libx265`, `hevc_nvenc`); overrides
    /// [`Self::encoder`].
    pub encoder_name: Option<String>,
    /// Encoder speed preset (e.g. `medium` for x264/x265, `p5` for NVENC).
    /// `None` uses a balanced default per encoder.
    pub preset: Option<String>,
    /// Run the detector every N frames (1 = every frame, the safest).
    pub detect_every: u32,
    /// Keep a detection alive for this many frames before and after it was
    /// seen, so a face that the detector misses for a few frames stays
    /// covered.
    pub persistence: u32,
    /// VA-API render node (Linux), e.g. `/dev/dri/renderD128`.
    pub vaapi_device: Option<String>,
}

impl Default for VideoOptions {
    fn default() -> Self {
        Self {
            hwaccel: HwAccel::None,
            encoder: EncoderBackend::Auto,
            quality: VideoQuality::Lossless,
            encoder_name: None,
            preset: None,
            detect_every: 1,
            persistence: 3,
            vaapi_device: None,
        }
    }
}

// --- string conversions shared by the CLI, the GUI and config files ---------

macro_rules! string_enum {
    ($ty:ty { $($variant:ident => [$($name:literal),+]),+ $(,)? }) => {
        impl $ty {
            /// All variants, in declaration order.
            pub const ALL: &'static [$ty] = &[$(<$ty>::$variant),+];

            /// Canonical kebab-case name.
            pub fn as_str(self) -> &'static str {
                match self { $(<$ty>::$variant => string_enum!(@first $($name),+)),+ }
            }
        }
        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.as_str()) }
        }
        impl FromStr for $ty {
            type Err = String;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                let lower = s.to_ascii_lowercase().replace('_', "-");
                $( if [$($name),+].contains(&lower.as_str()) { return Ok(<$ty>::$variant); } )+
                let valid: Vec<&str> = Self::ALL.iter().map(|v| v.as_str()).collect();
                Err(format!("invalid value `{s}`, expected one of: {}", valid.join(", ")))
            }
        }
    };
    (@first $first:literal $(, $rest:literal)*) => { $first };
}

string_enum!(MetadataPolicy { Keep => ["keep"], Sensitive => ["sensitive", "geo"], All => ["all"] });
string_enum!(Corner { BottomLeft => ["bottom-left", "left", "bl"], BottomRight => ["bottom-right", "right", "br"] });
string_enum!(FaceMode { EyesOnly => ["eyes-only", "eyes"], FullFace => ["full-face", "full", "face"] });
string_enum!(RedactionStyle {
    BlackBand => ["black-band", "black", "band"],
    Blur => ["blur"],
    Pixelate => ["pixelate", "pixel", "mosaic"],
});
string_enum!(FaceShape { Ellipse => ["ellipse", "oval"], Rectangle => ["rectangle", "rect", "box"] });
string_enum!(InferenceDevice {
    Auto => ["auto"],
    Cpu => ["cpu"],
    Cuda => ["cuda"],
    TensorRt => ["tensorrt", "trt"],
    DirectMl => ["directml", "dml"],
    CoreMl => ["coreml"],
    OpenVino => ["openvino"],
    Rocm => ["rocm"],
});
string_enum!(HwAccel {
    None => ["none", "off", "cpu"],
    Auto => ["auto"],
    Cuda => ["cuda", "nvdec"],
    Vaapi => ["vaapi"],
    Qsv => ["qsv"],
    VideoToolbox => ["videotoolbox"],
    D3d11va => ["d3d11va"],
    Dxva2 => ["dxva2"],
    Vulkan => ["vulkan"],
});
string_enum!(EncoderBackend {
    Auto => ["auto"],
    Software => ["software", "cpu"],
    Nvenc => ["nvenc"],
    Qsv => ["qsv"],
    Vaapi => ["vaapi"],
    VideoToolbox => ["videotoolbox"],
    Amf => ["amf"],
});
string_enum!(VideoQuality {
    Lossless => ["lossless"],
    VisuallyLossless => ["visually-lossless", "visual"],
    High => ["high"],
});

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enum_round_trip() {
        for v in RedactionStyle::ALL {
            assert_eq!(v.as_str().parse::<RedactionStyle>().unwrap(), *v);
        }
        assert_eq!("Pixel".parse::<RedactionStyle>().unwrap(), RedactionStyle::Pixelate);
        assert_eq!("eyes_only".parse::<FaceMode>().unwrap(), FaceMode::EyesOnly);
        assert!("nope".parse::<HwAccel>().is_err());
    }

    #[test]
    fn noop_is_rejected() {
        assert!(Options::default().validate().is_err());
        let o = Options { metadata: MetadataPolicy::All, ..Default::default() };
        assert!(o.validate().is_ok());
        let o = Options { timestamp: Some(TimestampOptions::default()), ..Default::default() };
        assert!(o.validate().is_ok(), "a timestamp alone is a valid job");
    }

    #[test]
    fn persistence_must_cover_skipped_frames() {
        let mut o = Options { faces: Some(FaceOptions::default()), ..Default::default() };
        o.video.detect_every = 5;
        o.video.persistence = 2;
        assert!(o.validate().is_err());
        o.video.persistence = 4;
        assert!(o.validate().is_ok());
    }
}
