//! `ffprobe` output model.

use super::ffmpeg::{command, ffprobe_path, run};
use crate::{Error, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Probe {
    #[serde(default)]
    pub streams: Vec<Stream>,
    #[serde(default)]
    pub format: Format,
    #[serde(default)]
    pub chapters: Vec<Chapter>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Format {
    #[serde(default)]
    pub format_name: String,
    pub duration: Option<String>,
    #[serde(default)]
    pub tags: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Chapter {
    #[serde(default)]
    pub tags: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Disposition {
    #[serde(default)]
    pub attached_pic: u8,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SideData {
    #[serde(default)]
    pub side_data_type: String,
    pub rotation: Option<f64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Stream {
    pub index: usize,
    #[serde(default)]
    pub codec_type: String,
    #[serde(default)]
    pub codec_name: String,
    #[serde(default)]
    pub codec_tag_string: String,
    pub profile: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub pix_fmt: Option<String>,
    pub color_range: Option<String>,
    pub color_space: Option<String>,
    pub color_transfer: Option<String>,
    pub color_primaries: Option<String>,
    pub avg_frame_rate: Option<String>,
    pub r_frame_rate: Option<String>,
    pub nb_frames: Option<String>,
    pub start_time: Option<String>,
    pub duration: Option<String>,
    #[serde(default)]
    pub disposition: Disposition,
    #[serde(default)]
    pub tags: BTreeMap<String, String>,
    #[serde(default)]
    pub side_data_list: Vec<SideData>,
}

impl Stream {
    pub fn is_video(&self) -> bool {
        self.codec_type == "video" && self.disposition.attached_pic == 0
    }

    /// Display rotation in degrees (multiple of 90), from the display
    /// matrix or the legacy `rotate` tag.
    pub fn rotation(&self) -> i32 {
        let deg = self
            .side_data_list
            .iter()
            .find(|s| s.side_data_type == "Display Matrix")
            .and_then(|s| s.rotation)
            .or_else(|| self.tags.get("rotate").and_then(|r| r.parse().ok()))
            .unwrap_or(0.0);
        ((deg.round() as i32 % 360) + 360) % 360
    }

    /// Average frame rate as `(num, den)`, falling back to `r_frame_rate`.
    pub fn frame_rate(&self) -> Option<(u32, u32)> {
        [self.avg_frame_rate.as_deref(), self.r_frame_rate.as_deref()].into_iter().flatten().find_map(parse_ratio)
    }

    /// Exact frame count when known, otherwise an estimate from the duration.
    pub fn frame_count(&self, container_duration: Option<f64>) -> Option<u64> {
        if let Some(n) = self.nb_frames.as_deref().and_then(|n| n.parse::<u64>().ok()).filter(|&n| n > 0) {
            return Some(n);
        }
        let (num, den) = self.frame_rate()?;
        let d = self.duration.as_deref().and_then(|d| d.parse::<f64>().ok()).or(container_duration)?;
        Some((d * num as f64 / den as f64).round() as u64)
    }

    pub fn start_seconds(&self) -> f64 {
        self.start_time.as_deref().and_then(|s| s.parse().ok()).unwrap_or(0.0)
    }
}

fn parse_ratio(s: &str) -> Option<(u32, u32)> {
    let (n, d) = s.split_once('/')?;
    let (n, d) = (n.parse().ok()?, d.parse().ok()?);
    (n > 0 && d > 0).then_some((n, d))
}

impl Probe {
    pub fn duration(&self) -> Option<f64> {
        self.format.duration.as_deref().and_then(|d| d.parse().ok())
    }

    /// The main video stream (first non-cover-art video stream).
    pub fn video(&self) -> Option<&Stream> {
        self.streams.iter().find(|s| s.is_video())
    }
}

/// Runs `ffprobe` on a file.
pub fn probe(path: &Path) -> Result<Probe> {
    let mut c = command(ffprobe_path());
    c.args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams", "-show_chapters"]).arg(path);
    let out = run(c)?;
    serde_json::from_slice(&out).map_err(|e| Error::Ffmpeg(format!("cannot parse ffprobe output: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ffprobe_json() {
        let json = r#"{
          "streams": [
            {"index": 0, "codec_type": "video", "codec_name": "h264", "width": 1920, "height": 1080,
             "pix_fmt": "yuv420p", "avg_frame_rate": "30000/1001", "r_frame_rate": "30000/1001", "nb_frames": "300",
             "side_data_list": [{"side_data_type": "Display Matrix", "rotation": -90}],
             "tags": {"language": "und"}},
            {"index": 1, "codec_type": "audio", "codec_name": "aac"},
            {"index": 2, "codec_type": "video", "codec_name": "mjpeg", "disposition": {"attached_pic": 1}}
          ],
          "format": {"format_name": "mov,mp4", "duration": "10.01",
                     "tags": {"com.apple.quicktime.location.ISO6709": "+45.5-073.5/"}}
        }"#;
        let p: Probe = serde_json::from_str(json).unwrap();
        let v = p.video().unwrap();
        assert_eq!(v.rotation(), 270);
        assert_eq!(v.frame_rate(), Some((30000, 1001)));
        assert_eq!(v.frame_count(p.duration()), Some(300));
        assert_eq!(p.streams.iter().filter(|s| s.is_video()).count(), 1);
    }
}
