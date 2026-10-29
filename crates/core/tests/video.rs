//! End-to-end tests of the video pipeline. Skipped when FFmpeg is missing.

mod common;

use common::*;
use mediaanon_core::{FaceOptions, MetadataPolicy, Options, RedactionStyle, VideoOptions};
use std::path::Path;

fn tags(probe: &serde_json::Value) -> String {
    probe["format"]["tags"].to_string()
}

#[test]
fn metadata_only_is_a_bit_exact_remux() {
    if !has_ffmpeg() {
        return;
    }
    let (_d, dir) = tmp();
    let input = make_clip(&dir, "in.mp4", "yuv420p", "libx264", "640");
    let out = dir.join("out.mp4");
    let report = run(options(MetadataPolicy::Sensitive, None), &input, &out);
    assert!(report.removed.iter().any(|r| r.contains("location")), "{:?}", report.removed);

    let p = ffprobe_json(&out);
    let t = tags(&p);
    assert!(!t.contains("45.5") && !t.contains("Jane"), "{t}");
    assert!(t.contains("Trip"), "non-sensitive title is kept: {t}");
    let md5 = |f: &Path| ffmpeg(&["-i", path_str(f), "-map", "0", "-c", "copy", "-f", "streamhash", "-"]);
    assert_eq!(md5(&input), md5(&out), "packets must be copied unchanged");
}

#[test]
fn faces_are_redacted_losslessly() {
    if !has_ffmpeg() {
        return;
    }
    let (_d, dir) = tmp();
    let input = make_clip(&dir, "in.mp4", "yuv420p", "libx264", "640");
    let out = dir.join("out.mp4");
    let faces = FaceOptions { style: RedactionStyle::Pixelate, ..Default::default() };
    let report = run(options(MetadataPolicy::All, Some(faces)), &input, &out);
    assert_eq!(report.frames, Some(25));
    assert_eq!(report.frames_with_faces, Some(25), "the face is visible in every frame");

    let p = ffprobe_json(&out);
    let streams = p["streams"].as_array().unwrap();
    assert_eq!(streams.len(), 2, "video + audio");
    assert_eq!(streams[1]["tags"]["language"], "ita", "track language is kept");
    assert!(!tags(&p).contains("Jane") && !tags(&p).contains("45.5"));

    let (w, h) = (640usize, 360usize);
    let frame = w * h * 3 / 2;
    let (a, b) = (raw_frames(&input, "yuv420p"), raw_frames(&out, "yuv420p"));
    assert_eq!(a.len(), b.len(), "same number of frames");
    for (i, (fa, fb)) in a.chunks(frame).zip(b.chunks(frame)).enumerate() {
        let (ya, yb) = (&fa[..w * h], &fb[..w * h]);
        let changed: Vec<usize> = (0..w * h).filter(|&k| ya[k] != yb[k]).collect();
        assert!(!changed.is_empty(), "frame {i} must be redacted");
        // Lossless: rows far below the face are bit-identical.
        assert!(changed.iter().all(|&k| k / w < h * 3 / 4), "frame {i}: change outside the face area");
        assert_eq!(&ya[w * (h * 3 / 4)..], &yb[w * (h * 3 / 4)..], "frame {i} bottom must be identical");
    }
}

#[test]
fn ten_bit_hevc_keeps_its_format() {
    if !has_ffmpeg() {
        return;
    }
    let (_d, dir) = tmp();
    let input = make_clip(&dir, "in.mkv", "yuv420p10le", "libx265", "320");
    let out = dir.join("out.mkv");
    let opts = Options {
        metadata: MetadataPolicy::Keep,
        faces: Some(FaceOptions::default()),
        video: VideoOptions { preset: Some("ultrafast".into()), ..Default::default() },
        ..Default::default()
    };
    let report = run(opts, &input, &out);
    assert!(report.notes.iter().any(|n| n.contains("libx265") && n.contains("lossless")), "{:?}", report.notes);
    let p = ffprobe_json(&out);
    let v = &p["streams"][0];
    assert_eq!(v["codec_name"], "hevc");
    assert_eq!(v["pix_fmt"], "yuv420p10le");
    assert!(tags(&p).contains("Jane"), "keep policy keeps the tags");
}

#[test]
fn timestamp_only_reencodes_losslessly() {
    if !has_ffmpeg() {
        return;
    }
    let (_d, dir) = tmp();
    let input = make_clip(&dir, "in.mp4", "yuv420p", "libx264", "640");
    let out = dir.join("out.mp4");
    let opts = Options {
        metadata: MetadataPolicy::All,
        timestamp: Some(mediaanon_core::TimestampOptions::default()),
        ..Default::default()
    };
    let report = run(opts, &input, &out);
    assert_eq!(report.timestamp.as_deref(), Some("2021-07-04 10:20:30 (container creation_time (UTC))"));
    assert_eq!(report.frames, None, "no face detection");
    let (w, h) = (640usize, 360usize);
    let frame = w * h * 3 / 2;
    let (a, b) = (raw_frames(&input, "yuv420p"), raw_frames(&out, "yuv420p"));
    assert_eq!(a.len(), b.len());
    for (fa, fb) in a.chunks(frame).zip(b.chunks(frame)) {
        let (ya, yb) = (&fa[..w * h], &fb[..w * h]);
        assert_eq!(&ya[..w * (h * 3 / 4)], &yb[..w * (h * 3 / 4)], "top of the frame untouched");
        let changed: Vec<usize> = (0..w * h).filter(|&k| ya[k] != yb[k]).collect();
        assert!(!changed.is_empty() && changed.iter().all(|&k| k % w > w / 3), "text at the bottom right");
    }
}

#[test]
fn unicode_paths_reach_ffmpeg() {
    if !has_ffmpeg() {
        return;
    }
    let (_d, dir) = tmp();
    let sub = dir.join("vidéo été 写真");
    std::fs::create_dir(&sub).unwrap();
    let input = make_clip(&sub, "clip de Zoë.mp4", "yuv420p", "libx264", "320");
    let out = sub.join("clip de Zoë.anon.mp4");
    let report = run(options(MetadataPolicy::All, None), &input, &out);
    assert!(out.is_file());
    assert!(!report.removed.is_empty());
}
