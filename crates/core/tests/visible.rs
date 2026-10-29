//! Faces marked to stay visible. Video tests are skipped when FFmpeg is missing.

mod common;

use common::*;
use mediaanon_core::{CancelToken, Edits, FaceOptions, MetadataPolicy, Processor, Timeline, VisibleFace};

fn processor() -> Processor {
    Processor::new(options(MetadataPolicy::All, Some(FaceOptions::default()))).unwrap()
}

#[test]
fn a_marked_photo_face_stays_visible() {
    let p = processor();
    let data = fixture_bytes();
    let plain = p.preview_data(&data, None, &[], 512).unwrap();
    assert_eq!(plain.faces.len(), 1);
    assert!(!plain.faces[0].visible);
    let (cx, cy) = plain.faces[0].bbox.center();

    let mark = VisibleFace { time: 0.0, x: cx, y: cy };
    let kept = p.preview_data(&data, None, &[mark], 512).unwrap();
    assert!(kept.faces[0].visible);
    assert_ne!(plain.image.to_rgb8().as_raw(), kept.image.to_rgb8().as_raw(), "the face is no longer covered");

    // Processing: nothing left to cover, so the pixels are copied unchanged.
    let (_d, dir) = tmp();
    let out = dir.join("out.jpg");
    let edits = Edits { visible: vec![mark], ..Default::default() };
    let report = p.process_file_with(FIXTURE.as_ref(), &out, &edits, &|_| {}, &CancelToken::new()).unwrap();
    assert_eq!(report.faces, 0);
    assert!(report.notes.iter().any(|n| n.contains("1 face left visible")), "{:?}", report.notes);
    let decode = |b: &[u8]| image::load_from_memory(b).unwrap().to_rgb8().into_raw();
    assert_eq!(decode(&std::fs::read(&out).unwrap()), decode(&data));
}

#[test]
fn a_mark_away_from_faces_changes_nothing() {
    let p = processor();
    let far = VisibleFace { time: 0.0, x: 0.02, y: 0.98 };
    let preview = p.preview_data(&fixture_bytes(), None, &[far], 512).unwrap();
    assert!(preview.faces.iter().all(|f| !f.visible));

    let (_d, dir) = tmp();
    let edits = Edits { visible: vec![far], ..Default::default() };
    let report =
        p.process_file_with(FIXTURE.as_ref(), &dir.join("o.jpg"), &edits, &|_| {}, &CancelToken::new()).unwrap();
    assert_eq!(report.faces, 1, "the face is still covered");
}

#[test]
fn a_marked_video_face_is_tracked() {
    if !has_ffmpeg() {
        return;
    }
    let (_d, dir) = tmp();
    let input = make_clip(&dir, "in.mp4", "yuv420p", "libx264", "640");
    let p = processor();

    // Mark the face on frame 10 (0.4 s at 25 fps).
    let frame = p.preview_video_frame(&input, 0.4, &Timeline::default(), &[], 320).unwrap();
    assert_eq!(frame.faces.len(), 1);
    let (cx, cy) = frame.faces[0].bbox.center();
    let edits = Edits { visible: vec![VisibleFace { time: 0.4, x: cx, y: cy }], ..Default::default() };

    // Tracking pass: followed from the mark to the end of the clip.
    let tracks = p.track_visible_faces(&input, &edits, &|_| {}, &CancelToken::new()).unwrap();
    assert_eq!(tracks.len(), 1);
    let t = &tracks[0];
    assert_eq!(t.frames.keys().copied().collect::<Vec<_>>(), (10..25).collect::<Vec<_>>());
    assert!(!t.lost, "{t:?}");
    let (start, _) = t.span.unwrap();
    assert!((start - 0.4).abs() < 1e-6);

    // The preview with the tracked box leaves the face visible.
    let keep = [t.frames[&15]];
    let f = p.preview_video_frame(&input, 0.6, &Timeline::default(), &keep, 320).unwrap();
    assert!(f.faces[0].visible);
    assert_eq!(f.original.to_rgb8().as_raw(), f.redacted.to_rgb8().as_raw());

    // Processing: covered before the mark, untouched once temporal smoothing
    // no longer carries the earlier detections (persistence = 3 frames).
    let out = dir.join("out.mp4");
    let report = p.process_file_with(&input, &out, &edits, &|_| {}, &CancelToken::new()).unwrap();
    assert!(
        report.notes.iter().any(|n| n.contains("left visible as marked (1 of 1 marks found)")),
        "{:?}",
        report.notes
    );
    let size = 640 * 360 * 3 / 2;
    let (a, b) = (raw_frames(&input, "yuv420p"), raw_frames(&out, "yuv420p"));
    for (i, (fa, fb)) in a.chunks(size).zip(b.chunks(size)).enumerate() {
        if i < 10 {
            assert_ne!(fa, fb, "frame {i} is before the mark: covered");
        } else if i >= 13 {
            assert_eq!(fa, fb, "frame {i}: the marked face stays visible");
        }
    }
}
