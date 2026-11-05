//! People filter with a real attribute model. The model is not bundled: these
//! tests run only when `MEDIAANON_ATTRIBUTE_MODEL` points to one (see
//! `models/attributes`), and the video test also needs FFmpeg.

mod common;

use common::*;
use mediaanon_core::{
    AttributeGroup, AttributeModel, CancelToken, Edits, FaceOptions, FilterMode, MetadataPolicy, PeopleFilter,
    Processor, Timeline,
};

fn model(p: &Processor) -> Option<AttributeModel> {
    let Some(path) = std::env::var_os("MEDIAANON_ATTRIBUTE_MODEL").filter(|p| !p.is_empty()) else {
        eprintln!("MEDIAANON_ATTRIBUTE_MODEL not set: skipping");
        return None;
    };
    Some(AttributeModel::load(path.as_ref(), &p.options().compute).expect("attribute model"))
}

fn processor() -> Processor {
    Processor::new(options(MetadataPolicy::All, Some(FaceOptions::default()))).unwrap()
}

#[test]
fn a_photo_is_scanned_and_filtered() {
    let p = processor();
    let Some(m) = model(&p) else { return };
    let scan = p.scan_people(FIXTURE.as_ref(), &Timeline::default(), &m, &|_| {}, &CancelToken::new()).unwrap();
    assert_eq!(scan.people.len(), 1);
    let person = &scan.people[0];
    assert_eq!(person.samples, 1);
    assert_eq!(person.scores.len(), scan.attributes.len());
    let roles: f32 = (0..scan.attributes.len())
        .filter(|&a| scan.attributes[a].group == AttributeGroup::Role)
        .map(|a| person.scores[a])
        .sum();
    assert!((roles - 1.0).abs() < 1e-3, "role scores sum to 1: {roles}");
    eprintln!(
        "{}",
        scan.attributes
            .iter()
            .zip(&person.scores)
            .map(|(a, s)| format!("{} {s:.2}", a.name))
            .collect::<Vec<_>>()
            .join(", ")
    );

    // Whatever the model says, the mark (if any) designates the only face.
    let all: Vec<String> = scan.attributes.iter().map(|a| a.name.clone()).collect();
    for mode in [FilterMode::CoverOnly, FilterMode::CoverAllExcept] {
        let filter = PeopleFilter { mode, attributes: all.clone() };
        let marks = scan.marks(&filter);
        assert_eq!(marks.len(), scan.is_visible(person, &filter) as usize);
        let preview = p.preview_data(&fixture_bytes(), None, &marks, 512).unwrap();
        assert_eq!(preview.faces[0].visible, !marks.is_empty());
    }
}

#[test]
fn a_video_is_scanned_like_processing_tracks() {
    if !has_ffmpeg() {
        return;
    }
    let p = processor();
    let Some(m) = model(&p) else { return };
    let (_d, dir) = tmp();
    let input = make_clip(&dir, "in.mp4", "yuv420p", "libx264", "640");
    let scan = p.scan_people(&input, &Timeline::default(), &m, &|_| {}, &CancelToken::new()).unwrap();
    assert_eq!(scan.people.len(), 1, "{:?}", scan.people);
    let person = &scan.people[0];
    assert_eq!(person.frames.len(), 25);
    assert_eq!(person.samples, 1, "one second of video: classified once");

    // A rule leaving the person visible gives a mark that processing follows
    // over the whole clip, and no further.
    let i = person.scores.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).map(|(i, _)| i).unwrap();
    let filter = PeopleFilter { mode: FilterMode::CoverAllExcept, attributes: vec![scan.attributes[i].name.clone()] };
    let marks = scan.marks(&filter);
    if marks.is_empty() {
        eprintln!("the model is unsure about this face: nothing to follow");
        return;
    }
    assert_eq!(marks[0].until, Some(person.end));
    let edits = Edits { visible: marks, ..Default::default() };
    let tracks = p.track_visible_faces(&input, &edits, &|_| {}, &CancelToken::new()).unwrap();
    assert_eq!(tracks[0].frames.keys().copied().collect::<Vec<_>>(), person.frames.keys().copied().collect::<Vec<_>>());
}
