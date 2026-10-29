//! Per-section video settings: keyframes, cuts and frame previews. Skipped
//! when FFmpeg is missing.

mod common;

use common::*;
use mediaanon_core::{CancelToken, Edits, FaceOptions, Keyframe, MetadataPolicy, Options, Processor, Report, Timeline};
use std::path::Path;

fn run_with(opts: Options, timeline: &Timeline, input: &Path, output: &Path) -> mediaanon_core::Result<Report> {
    let edits = Edits::from(timeline.clone());
    Processor::new(opts)?.process_file_with(input, output, &edits, &|_| {}, &CancelToken::new())
}

fn stream_duration(probe: &serde_json::Value, kind: &str) -> f64 {
    let s = probe["streams"].as_array().unwrap().iter().find(|s| s["codec_type"] == kind).expect(kind);
    s["duration"].as_str().and_then(|d| d.parse().ok()).expect("duration")
}

#[test]
fn sections_change_the_settings() {
    if !has_ffmpeg() {
        return;
    }
    let (_d, dir) = tmp();
    let input = make_clip(&dir, "in.mp4", "yuv420p", "libx264", "640");
    let out = dir.join("out.mp4");
    // Faces covered for frames 0..10, left alone from 0.4 s (frame 10).
    let timeline = Timeline::new(vec![Keyframe { time: 0.4, faces: None, ..Default::default() }]);
    let report = run_with(options(MetadataPolicy::All, Some(FaceOptions::default())), &timeline, &input, &out).unwrap();
    assert_eq!(report.frames, Some(25));
    assert_eq!(report.frames_with_faces, Some(10));

    let frame = 640 * 360 * 3 / 2;
    let (a, b) = (raw_frames(&input, "yuv420p"), raw_frames(&out, "yuv420p"));
    assert_eq!(a.len(), b.len());
    for (i, (fa, fb)) in a.chunks(frame).zip(b.chunks(frame)).enumerate() {
        if i < 10 {
            assert_ne!(fa, fb, "frame {i} must be redacted");
        } else {
            assert_eq!(fa, fb, "frame {i} must be untouched (lossless)");
        }
    }
}

#[test]
fn faces_can_be_enabled_for_a_section_only() {
    if !has_ffmpeg() {
        return;
    }
    let (_d, dir) = tmp();
    let input = make_clip(&dir, "in.mp4", "yuv420p", "libx264", "640");
    let out = dir.join("out.mp4");
    let timeline = Timeline::new(vec![Keyframe { time: 0.8, ..Default::default() }]);
    let report = run_with(options(MetadataPolicy::All, None), &timeline, &input, &out).unwrap();
    assert_eq!(report.frames_with_faces, Some(5), "frames 20..25");
}

#[test]
fn cut_sections_are_removed_with_their_audio() {
    if !has_ffmpeg() {
        return;
    }
    let (_d, dir) = tmp();
    let input = make_clip(&dir, "in.mp4", "yuv420p", "libx264", "640");
    let out = dir.join("out.mp4");
    // Cut 0.2 s – 0.6 s (frames 10..15 of 25 at 25 fps: 10 frames).
    let timeline = Timeline::new(vec![
        Keyframe { time: 0.2, cut: true, ..Default::default() },
        Keyframe { time: 0.6, faces: None, ..Default::default() },
    ]);
    // No face redaction at all: the cut alone forces the re-encode.
    let report = run_with(options(MetadataPolicy::All, None), &timeline, &input, &out).unwrap();
    assert!(report.notes.iter().any(|n| n.contains("10 frames cut")), "{:?}", report.notes);
    assert!(report.notes.iter().any(|n| n.contains("audio re-encoded")), "{:?}", report.notes);

    let frame = 640 * 360 * 3 / 2;
    let (a, b) = (raw_frames(&input, "yuv420p"), raw_frames(&out, "yuv420p"));
    assert_eq!(b.len() / frame, 15);
    let (a, b): (Vec<&[u8]>, Vec<&[u8]>) = (a.chunks(frame).collect(), b.chunks(frame).collect());
    assert_eq!(b[..5], a[..5], "frames before the cut are kept as is");
    assert_eq!(b[5..], a[15..], "frames after the cut follow directly");

    let p = ffprobe_json(&out);
    let audio = stream_duration(&p, "audio");
    assert!((audio - 0.6).abs() < 0.1, "audio trimmed to ~0.6 s, got {audio}");
}

#[test]
fn cutting_everything_fails() {
    if !has_ffmpeg() {
        return;
    }
    let (_d, dir) = tmp();
    let input = make_clip(&dir, "in.mp4", "yuv420p", "libx264", "320");
    let out = dir.join("out.mp4");
    let timeline = Timeline::new(vec![Keyframe { time: 0.0, cut: true, ..Default::default() }]);
    let err = run_with(options(MetadataPolicy::All, None), &timeline, &input, &out).unwrap_err();
    assert!(err.to_string().contains("every frame"), "{err}");
    assert!(!out.exists());
}

#[test]
fn frame_preview_and_info() {
    if !has_ffmpeg() {
        return;
    }
    let (_d, dir) = tmp();
    let input = make_clip(&dir, "in.mp4", "yuv420p", "libx264", "640");

    let info = mediaanon_core::video::info(&input).unwrap();
    assert_eq!((info.width, info.height), (640, 360));
    assert_eq!(info.frame_rate, 25.0);
    assert_eq!(info.frames, Some(25));
    assert!((info.duration - 1.0).abs() < 0.1, "{info:?}");

    let p = Processor::new(options(MetadataPolicy::All, Some(FaceOptions::default()))).unwrap();
    let f = p.preview_video_frame(&input, 0.5, &Timeline::default(), &[], 320).unwrap();
    assert_eq!((f.original.width(), f.original.height()), (320, 180));
    assert_eq!(f.faces.len(), 1);
    assert!(!f.cut);
    assert_ne!(f.original.to_rgb8().as_raw(), f.redacted.to_rgb8().as_raw());

    let cut = Timeline::new(vec![Keyframe { time: 0.4, cut: true, ..Default::default() }]);
    let f = p.preview_video_frame(&input, 0.5, &cut, &[], 320).unwrap();
    assert!(f.cut && f.faces.is_empty());

    let untouched = Timeline::new(vec![Keyframe { time: 0.0, faces: None, ..Default::default() }]);
    let f = p.preview_video_frame(&input, 0.5, &untouched, &[], 320).unwrap();
    assert_eq!(f.original.to_rgb8().as_raw(), f.redacted.to_rgb8().as_raw());
}
