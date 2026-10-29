//! Container metadata and stream selection for FFmpeg muxing.
//!
//! Video metadata lives in container tags (global and per stream), chapters,
//! cover art, and *data streams*: action cameras and phones record GPS tracks
//! as timed metadata (GoPro `gpmd`, Apple `mebx`, DJI subtitles...). Removing
//! the tags alone is therefore not enough.

use super::probe::{Probe, Stream};
use crate::metadata::{Changes, Finding};
use crate::options::MetadataPolicy;

/// Substrings of tag keys considered personal or location data.
const SENSITIVE_KEYS: &[&str] = &[
    "location",
    "gps",
    "xyz",
    "geo",
    "artist",
    "author",
    "composer",
    "performer",
    "copyright",
    "comment",
    "description",
    "synopsis",
    "keywords",
    "owner",
    "serial",
    "identifier",
    "uuid",
    "user",
    "email",
    "phone",
    "address",
    "lyrics",
    "encoded_by",
    "publisher",
    "director",
    "producer",
    "make.id",
    "content.id",
];

/// Tags rewritten by the muxer itself; never copied explicitly.
const MUXER_KEYS: &[&str] = &["major_brand", "minor_version", "compatible_brands", "encoder", "rotate", "duration"];

pub fn is_sensitive_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    SENSITIVE_KEYS.iter().any(|s| k.contains(s))
}

/// Inputs given to the muxing FFmpeg command.
#[derive(Debug)]
pub struct MuxInputs<'a> {
    pub probe: &'a Probe,
    /// Input index of the original file in the FFmpeg command.
    pub source: usize,
    /// When the main video is re-encoded: its stream index in the original
    /// file. The encoded stream is taken from input 0.
    pub replaced_video: Option<usize>,
    pub policy: MetadataPolicy,
    pub faces: bool,
    /// Sections of the video are cut: streams that cannot be trimmed with
    /// it (subtitles, data, other video tracks) and chapters are dropped.
    pub cut: bool,
    /// Output container is MP4/MOV (needs `use_metadata_tags` to keep vendor keys).
    pub mov_like: bool,
}

/// Builds the `-map` and metadata arguments.
pub fn mux_args(m: &MuxInputs<'_>, ch: &mut Changes) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    let mut out = 0usize;
    let src = m.source;
    let keep_tag = |k: &str| match m.policy {
        MetadataPolicy::Keep => true,
        MetadataPolicy::Sensitive => !is_sensitive_key(k) && !MUXER_KEYS.contains(&k),
        // Track language is needed to select audio/subtitle tracks.
        MetadataPolicy::All => k == "language",
    };

    for s in &m.probe.streams {
        let label = stream_label(s);
        let include = if Some(s.index) == m.replaced_video {
            args.extend(["-map".into(), "0:0".into()]);
            true
        } else {
            let keep = match s.codec_type.as_str() {
                "video" if s.disposition.attached_pic == 1 => !(m.faces || m.policy == MetadataPolicy::All),
                "video" | "subtitle" | "data" if m.cut => {
                    ch.warn(format!("{label} dropped: it cannot be trimmed to the cut sections"));
                    false
                }
                "video" if m.faces => {
                    ch.warn(format!("{label} dropped: only the main video stream is redacted"));
                    false
                }
                "video" | "audio" | "subtitle" => true,
                "attachment" => m.policy != MetadataPolicy::All,
                // Data streams: timecode is harmless, the rest is telemetry.
                "data" if s.codec_tag_string == "tmcd" => m.policy != MetadataPolicy::All,
                _ => m.policy == MetadataPolicy::Keep,
            };
            if keep {
                args.extend(["-map".into(), format!("{src}:{}", s.index)]);
            } else if s.codec_type != "video" || s.disposition.attached_pic == 1 {
                ch.removed(label.clone());
            }
            keep
        };
        if !include {
            continue;
        }
        match m.policy {
            MetadataPolicy::Keep => {
                if Some(s.index) == m.replaced_video {
                    args.extend([format!("-map_metadata:s:{out}"), format!("{src}:s:{}", s.index)]);
                }
            }
            _ => {
                for (k, v) in &s.tags {
                    if keep_tag(k) {
                        args.extend([format!("-metadata:s:{out}"), format!("{k}={v}")]);
                    } else if !MUXER_KEYS.contains(&k.as_str()) && m.policy == MetadataPolicy::Sensitive {
                        ch.removed(format!("{label} tag `{k}`"));
                    }
                }
            }
        }
        out += 1;
    }

    // Chapter times would no longer match once sections are cut.
    let chapters = if m.cut { "-1".to_string() } else { src.to_string() };
    if m.cut && m.policy != MetadataPolicy::All && !m.probe.chapters.is_empty() {
        ch.warn(format!("{} chapters dropped: their times do not match the cut video", m.probe.chapters.len()));
    }
    match m.policy {
        MetadataPolicy::Keep => {
            args.extend(["-map_metadata".into(), src.to_string(), "-map_chapters".into(), chapters]);
        }
        MetadataPolicy::Sensitive => {
            args.extend(["-map_metadata".into(), "-1".into(), "-map_chapters".into(), chapters]);
            for (k, v) in &m.probe.format.tags {
                if keep_tag(k) {
                    args.extend(["-metadata".into(), format!("{k}={v}")]);
                } else if !MUXER_KEYS.contains(&k.as_str()) {
                    ch.removed(format!("container tag `{k}`"));
                }
            }
        }
        MetadataPolicy::All => {
            args.extend(["-map_metadata".into(), "-1".into(), "-map_chapters".into(), "-1".into()]);
            let tags = m.probe.format.tags.keys().filter(|k| !MUXER_KEYS.contains(&k.as_str())).count();
            if tags > 0 {
                ch.removed(format!("{tags} container tags"));
            }
            if !m.probe.chapters.is_empty() {
                ch.removed(format!("{} chapters", m.probe.chapters.len()));
            }
            // Do not write the "encoder: Lavf..." tag or a creation time.
            args.extend(["-fflags".into(), "+bitexact".into()]);
        }
    }
    if m.mov_like && m.policy != MetadataPolicy::All {
        // Without this flag the MOV muxer silently drops vendor keys
        // (e.g. com.apple.quicktime.*), which would break the `keep` policy.
        args.extend(["-movflags".into(), "+use_metadata_tags".into()]);
    }
    args
}

fn stream_label(s: &Stream) -> String {
    let kind = match (s.codec_type.as_str(), s.disposition.attached_pic) {
        ("video", 1) => "cover art",
        ("data", _) => "data stream",
        (t, _) => t,
    };
    let tag = if s.codec_tag_string.is_empty() || s.codec_tag_string.starts_with('[') {
        s.codec_name.clone()
    } else {
        s.codec_tag_string.clone()
    };
    format!("{kind} #{} ({tag})", s.index)
}

/// Lists the metadata of a probed video.
pub fn inspect(p: &Probe, out: &mut Vec<Finding>) {
    for (k, v) in &p.format.tags {
        if MUXER_KEYS.contains(&k.as_str()) {
            continue;
        }
        out.push(Finding { block: "container".into(), detail: format!("{k}: {v}"), sensitive: is_sensitive_key(k) });
    }
    for s in &p.streams {
        let label = stream_label(s);
        match s.codec_type.as_str() {
            "data" => out.push(Finding {
                block: label.clone(),
                detail: "timed metadata (may contain a GPS track)".into(),
                sensitive: s.codec_tag_string != "tmcd",
            }),
            "video" if s.disposition.attached_pic == 1 => {
                out.push(Finding { block: label.clone(), detail: "embedded picture".into(), sensitive: true })
            }
            _ => {}
        }
        for (k, v) in &s.tags {
            if !MUXER_KEYS.contains(&k.as_str()) && is_sensitive_key(k) {
                out.push(Finding { block: label.clone(), detail: format!("{k}: {v}"), sensitive: true });
            }
        }
    }
    if !p.chapters.is_empty() {
        out.push(Finding {
            block: "chapters".into(),
            detail: format!("{} chapters", p.chapters.len()),
            sensitive: false,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::probe::Probe;

    fn sample() -> Probe {
        serde_json::from_str(
            r#"{
          "streams": [
            {"index": 0, "codec_type": "video", "codec_name": "h264", "tags": {"language": "eng", "handler_name": "Core Media Video"}},
            {"index": 1, "codec_type": "audio", "codec_name": "aac", "tags": {"language": "ita"}},
            {"index": 2, "codec_type": "data", "codec_name": "bin_data", "codec_tag_string": "gpmd"},
            {"index": 3, "codec_type": "video", "codec_name": "mjpeg", "disposition": {"attached_pic": 1}}
          ],
          "format": {"tags": {"com.apple.quicktime.location.ISO6709": "+45.5-073.5/",
                              "com.apple.quicktime.make": "Apple", "major_brand": "qt  ", "artist": "Jane"}}
        }"#,
        )
        .unwrap()
    }

    fn joined(args: &[String]) -> String {
        args.join(" ")
    }

    #[test]
    fn sensitive_drops_location_and_telemetry() {
        let p = sample();
        let mut ch = Changes::default();
        let m = MuxInputs {
            probe: &p,
            source: 0,
            replaced_video: None,
            policy: MetadataPolicy::Sensitive,
            faces: false,
            cut: false,
            mov_like: true,
        };
        let a = joined(&mux_args(&m, &mut ch));
        assert!(a.contains("-map 0:0 -metadata:s:0 handler_name=Core Media Video"));
        assert!(a.contains("-map 0:1"));
        assert!(!a.contains("0:2"), "GPS telemetry stream must be dropped: {a}");
        assert!(a.contains("-map 0:3"), "cover art kept without face redaction");
        assert!(a.contains("-metadata com.apple.quicktime.make=Apple"));
        assert!(!a.contains("ISO6709") && !a.contains("Jane") && !a.contains("major_brand"));
        assert!(ch.removed.iter().any(|r| r.contains("gpmd")));
    }

    #[test]
    fn all_with_faces_maps_encoded_video() {
        let p = sample();
        let mut ch = Changes::default();
        let m = MuxInputs {
            probe: &p,
            source: 1,
            replaced_video: Some(0),
            policy: MetadataPolicy::All,
            faces: true,
            cut: false,
            mov_like: true,
        };
        let a = joined(&mux_args(&m, &mut ch));
        assert!(a.starts_with("-map 0:0 -metadata:s:0 language=eng -map 1:1 -metadata:s:1 language=ita"), "{a}");
        assert!(!a.contains("1:3"), "cover art may show faces");
        assert!(a.contains("-map_metadata -1 -map_chapters -1"));
        assert!(!a.contains("handler_name"));
    }
}
