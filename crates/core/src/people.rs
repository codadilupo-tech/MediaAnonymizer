//! People found in a file, their attributes, and rules that decide from
//! those attributes which faces to leave visible.
//!
//! A *scan* ([`PeopleScan`]) follows every face through a video (or takes
//! every face of a photo), and classifies each person a few times with an
//! [`AttributeModel`](crate::detect::AttributeModel); the scores are
//! averaged per person.
//!
//! A [`PeopleFilter`] then turns the scan into ordinary
//! [`VisibleFace`] marks, which the user reviews like hand-made ones. The
//! rule only ever *uncovers* people it is confident about:
//!
//! * [`FilterMode::CoverOnly`]: a person stays visible only if the model is
//!   confident that they have *none* of the chosen attributes.
//! * [`FilterMode::CoverAllExcept`]: a person stays visible only if the
//!   model is confident that they have *at least one* of them.
//!
//! When the model is unsure, when a face was too small to classify, or when
//! a face appears that the scan did not see, the face is covered.

use crate::detect::{AttributeGroup, AttributeInfo, Face};
use crate::edits::{FaceBox, VisibleFace, TRACK_IOU};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One person: a face followed through the frames of a video, or one face
/// of a photo.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Person {
    /// Number shown to the user (1-based, in order of appearance).
    pub id: usize,
    /// First and last time (seconds) the face was seen; 0 for photos.
    pub start: f64,
    pub end: f64,
    /// Position on each frame where the face was detected, by frame index
    /// (photos: frame 0).
    pub frames: BTreeMap<u64, FaceBox>,
    /// Mean score of each attribute (same order as
    /// [`PeopleScan::attributes`]); empty if the face was never large
    /// enough to classify.
    pub scores: Vec<f32>,
    /// Number of crops classified.
    pub samples: u32,
}

impl Person {
    /// Position where the person was first seen.
    pub fn first(&self) -> Option<FaceBox> {
        self.frames.values().next().copied()
    }
}

/// What the model says about one attribute of one person.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Presence {
    Yes,
    No,
    Unsure,
}

/// Result of scanning a file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PeopleScan {
    pub attributes: Vec<AttributeInfo>,
    pub people: Vec<Person>,
    /// Frame rate of the video (0 for photos).
    pub fps: f64,
}

/// How a [`PeopleFilter`] uses the chosen attributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FilterMode {
    /// Cover only the people with one of the attributes; leave the others
    /// visible ("include only").
    #[default]
    CoverOnly,
    /// Cover everyone except the people with one of the attributes ("exclude
    /// all").
    CoverAllExcept,
}

/// A rule choosing the people to leave visible.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PeopleFilter {
    pub mode: FilterMode,
    /// Attribute names; a person matches with any one of them.
    pub attributes: Vec<String>,
}

impl PeopleScan {
    /// What the model says about attribute `a` of `p`.
    pub fn presence(&self, p: &Person, a: usize) -> Presence {
        match (p.scores.get(a), self.attributes.get(a)) {
            (Some(&s), Some(info)) if s >= info.present => Presence::Yes,
            (Some(&s), Some(info)) if s < info.absent => Presence::No,
            _ => Presence::Unsure,
        }
    }

    /// The most likely role of `p`, if the model is confident about it.
    pub fn role(&self, p: &Person) -> Option<&AttributeInfo> {
        (0..self.attributes.len())
            .filter(|&a| self.attributes[a].group == AttributeGroup::Role)
            .find(|&a| self.presence(p, a) == Presence::Yes)
            .map(|a| &self.attributes[a])
    }

    /// People with each attribute: `(confident, unsure)` counts.
    pub fn counts(&self) -> Vec<(usize, usize)> {
        (0..self.attributes.len())
            .map(|a| {
                let of = |want| self.people.iter().filter(|p| self.presence(p, a) == want).count();
                (of(Presence::Yes), of(Presence::Unsure))
            })
            .collect()
    }

    /// Whether `filter` leaves `p` visible. An empty filter leaves no one
    /// visible.
    pub fn is_visible(&self, p: &Person, filter: &PeopleFilter) -> bool {
        let chosen: Vec<usize> =
            filter.attributes.iter().filter_map(|name| self.attributes.iter().position(|a| &a.name == name)).collect();
        if chosen.is_empty() {
            return false;
        }
        match filter.mode {
            FilterMode::CoverOnly => chosen.iter().all(|&a| self.presence(p, a) == Presence::No),
            FilterMode::CoverAllExcept => chosen.iter().any(|&a| self.presence(p, a) == Presence::Yes),
        }
    }

    /// Marks leaving visible the people `filter` selects: one mark on the
    /// first position of each, valid until they were last seen.
    pub fn marks(&self, filter: &PeopleFilter) -> Vec<VisibleFace> {
        self.people
            .iter()
            .filter(|p| self.is_visible(p, filter))
            .filter_map(|p| {
                let (x, y) = p.first()?.center();
                Some(VisibleFace { time: p.start, x, y, until: (self.fps > 0.0).then_some(p.end) })
            })
            .collect()
    }
}

/// A person being followed during a scan.
#[derive(Debug, Clone)]
struct Track {
    last: Face,
    last_seen: u64,
    frames: BTreeMap<u64, FaceBox>,
    sum: Vec<f32>,
    samples: u32,
    last_sample: Option<u64>,
    ended: bool,
}

/// Follows every face of a video, the way [`crate::edits::Tracker`] follows
/// a marked face: by best overlap with its previous position, ending a
/// track once the face is missing for longer than `tolerance` frames.
#[derive(Debug)]
pub(crate) struct PeopleTracker {
    tracks: Vec<Track>,
    tolerance: u64,
    /// Space between two classifications of the same person, in frames.
    resample: u64,
    max_samples: u32,
    /// Faces smaller than this (detection pixels) are not classified.
    min_face: f32,
    width: f32,
    height: f32,
}

impl PeopleTracker {
    /// `width`×`height` is the pixel space of the faces given to `step`.
    pub fn new(width: f32, height: f32, tolerance: u64, resample: u64) -> Self {
        Self { tracks: Vec::new(), tolerance, resample: resample.max(1), max_samples: 8, min_face: 12.0, width, height }
    }

    /// Advances to frame `idx` (frames in order; `None`: no detection ran).
    /// Returns, per face, the track it belongs to.
    pub fn step(&mut self, idx: u64, faces: Option<&[Face]>) -> Vec<usize> {
        let tol = self.tolerance;
        let Some(faces) = faces else {
            for t in &mut self.tracks {
                t.ended |= idx > t.last_seen + tol;
            }
            return Vec::new();
        };
        let mut owner: Vec<Option<usize>> = vec![None; faces.len()];
        for (ti, t) in self.tracks.iter_mut().enumerate() {
            if t.ended {
                continue;
            }
            let best = faces
                .iter()
                .enumerate()
                .filter(|(i, _)| owner[*i].is_none())
                .map(|(i, f)| (i, f.iou(&t.last)))
                .filter(|&(_, iou)| iou >= TRACK_IOU)
                .max_by(|a, b| a.1.total_cmp(&b.1));
            match best {
                Some((i, _)) => {
                    owner[i] = Some(ti);
                    t.last = faces[i];
                    t.last_seen = idx;
                }
                None => t.ended |= idx > t.last_seen + tol,
            }
        }
        for (i, f) in faces.iter().enumerate() {
            if owner[i].is_none() {
                owner[i] = Some(self.tracks.len());
                self.tracks.push(Track {
                    last: *f,
                    last_seen: idx,
                    frames: BTreeMap::new(),
                    sum: Vec::new(),
                    samples: 0,
                    last_sample: None,
                    ended: false,
                });
            }
        }
        let owner: Vec<usize> = owner.into_iter().map(|o| o.expect("every face has a track")).collect();
        for (f, &t) in faces.iter().zip(&owner) {
            self.tracks[t].frames.insert(idx, FaceBox::of(f, self.width, self.height));
        }
        owner
    }

    /// Faces of the last `step` (by index) worth classifying now.
    pub fn to_classify(&self, idx: u64, faces: &[Face], owner: &[usize]) -> Vec<usize> {
        (0..faces.len())
            .filter(|&i| {
                let t = &self.tracks[owner[i]];
                faces[i].height >= self.min_face
                    && t.samples < self.max_samples
                    && t.last_sample.is_none_or(|s| idx >= s + self.resample)
            })
            .collect()
    }

    /// Adds the scores of one crop of track `t`.
    pub fn add_scores(&mut self, t: usize, idx: u64, scores: &[f32]) {
        let t = &mut self.tracks[t];
        if t.sum.is_empty() {
            t.sum = vec![0.0; scores.len()];
        }
        t.sum.iter_mut().zip(scores).for_each(|(a, s)| *a += s);
        t.samples += 1;
        t.last_sample = Some(idx);
    }

    /// The people found, in order of appearance (`fps` = 0 for photos).
    pub fn finish(self, fps: f64) -> Vec<Person> {
        let time = |f: u64| if fps > 0.0 { f as f64 / fps } else { 0.0 };
        self.tracks
            .into_iter()
            .enumerate()
            .map(|(i, t)| Person {
                id: i + 1,
                start: time(*t.frames.keys().next().unwrap_or(&0)),
                end: time(t.last_seen),
                scores: t.sum.iter().map(|s| s / t.samples.max(1) as f32).collect(),
                samples: t.samples,
                frames: t.frames,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(x: f32, y: f32, s: f32) -> Face {
        Face { x, y, width: s, height: s, landmarks: Default::default(), score: 0.9 }
    }

    fn attr(name: &str, group: AttributeGroup) -> AttributeInfo {
        AttributeInfo { name: name.into(), label: name.into(), group, present: 0.7, absent: 0.3 }
    }

    fn scan(scores: &[[f32; 3]]) -> PeopleScan {
        let people = scores
            .iter()
            .enumerate()
            .map(|(i, s)| Person {
                id: i + 1,
                start: i as f64,
                end: i as f64 + 2.0,
                frames: [(i as u64 * 10, FaceBox { x: 0.25 * i as f32, y: 0.25, width: 0.5, height: 0.5 })].into(),
                scores: s.to_vec(),
                samples: 1,
            })
            .collect();
        PeopleScan {
            attributes: vec![
                attr("civilian", AttributeGroup::Role),
                attr("police", AttributeGroup::Role),
                attr("helmet", AttributeGroup::Gear),
            ],
            people,
            fps: 10.0,
        }
    }

    #[test]
    fn rules_only_uncover_confident_people() {
        // A confident civilian, a confident police officer with a helmet, an unsure person.
        let s = scan(&[[0.9, 0.1, 0.1], [0.05, 0.95, 0.8], [0.5, 0.5, 0.5]]);
        let f = |mode, a: &[&str]| PeopleFilter { mode, attributes: a.iter().map(|s| s.to_string()).collect() };
        let visible = |filter: &PeopleFilter| s.people.iter().map(|p| s.is_visible(p, filter)).collect::<Vec<_>>();
        assert_eq!(visible(&f(FilterMode::CoverOnly, &["civilian"])), [false, true, false]);
        assert_eq!(visible(&f(FilterMode::CoverAllExcept, &["police"])), [false, true, false]);
        assert_eq!(visible(&f(FilterMode::CoverAllExcept, &["civilian", "helmet"])), [true, true, false]);
        assert_eq!(visible(&f(FilterMode::CoverOnly, &["police", "helmet"])), [true, false, false]);
        assert_eq!(visible(&f(FilterMode::CoverOnly, &[])), [false, false, false], "no attribute, no rule");
        assert_eq!(visible(&f(FilterMode::CoverOnly, &["unknown"])), [false, false, false]);
        let unclassified = Person { scores: Vec::new(), ..s.people[0].clone() };
        assert!(!s.is_visible(&unclassified, &f(FilterMode::CoverOnly, &["police"])));
        assert_eq!(s.role(&s.people[1]).map(|a| a.name.as_str()), Some("police"));
        assert_eq!(s.role(&s.people[2]), None);
        assert_eq!(s.counts(), vec![(1, 1), (1, 1), (1, 1)]);
    }

    #[test]
    fn marks_point_at_the_first_position_until_the_end() {
        let s = scan(&[[0.9, 0.1, 0.1], [0.05, 0.95, 0.8]]);
        let marks = s.marks(&PeopleFilter { mode: FilterMode::CoverAllExcept, attributes: vec!["police".into()] });
        assert_eq!(marks, vec![VisibleFace { time: 1.0, x: 0.5, y: 0.5, until: Some(3.0) }]);
    }

    #[test]
    fn the_tracker_follows_every_face() {
        let mut t = PeopleTracker::new(1000.0, 1000.0, 2, 5);
        let a = |x| face(x, 100.0, 50.0);
        let b = |x| face(x, 600.0, 50.0);
        assert_eq!(t.step(0, Some(&[a(100.0), b(500.0)])), vec![0, 1]);
        assert_eq!(t.to_classify(0, &[a(100.0), b(500.0)], &[0, 1]), vec![0, 1]);
        t.add_scores(0, 0, &[1.0, 0.0]);
        t.add_scores(1, 0, &[0.0, 1.0]);
        assert_eq!(t.step(1, Some(&[b(505.0), a(105.0)])), vec![1, 0]);
        assert!(t.to_classify(1, &[b(505.0), a(105.0)], &[1, 0]).is_empty(), "too soon to classify again");
        t.step(2, None);
        // Face `a` gone for longer than the tolerance: it comes back as a new person.
        for idx in 3..6 {
            t.step(idx, Some(&[b(505.0)]));
        }
        assert_eq!(t.step(6, Some(&[a(105.0), b(505.0)])), vec![2, 1]);
        t.add_scores(0, 1, &[0.0, 0.0]);
        let people = t.finish(10.0);
        assert_eq!(people.len(), 3);
        assert_eq!((people[0].start, people[0].end, people[0].samples), (0.0, 0.1, 2));
        assert_eq!(people[0].scores, vec![0.5, 0.0]);
        assert_eq!((people[1].start, people[1].end), (0.0, 0.6));
        assert!(people[2].scores.is_empty(), "never classified");
    }
}
