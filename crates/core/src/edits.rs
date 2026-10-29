//! Per-file decisions: the video [`Timeline`] and the faces to leave visible.
//!
//! Every detected face is covered unless the user marks it with a
//! [`VisibleFace`]: a point on the face, in the upright picture, normalised
//! to `0..=1`.
//!
//! * In a photo the mark selects the detected face that contains the point.
//! * In a video the mark also carries a time: the face found at the point on
//!   that frame is then *tracked* forward, frame after frame, by overlap with
//!   its previous position. The track ends when the face is lost for more
//!   than a short tolerance, and from then on the face is covered again. A
//!   lost face is therefore never left visible by accident; the user adds a
//!   new mark where needed.
//!
//! A face that cannot be matched with confidence stays covered: leaving the
//! wrong face visible would defeat the purpose of the tool.

use crate::detect::{Face, Point};
use crate::timeline::Timeline;
use serde::{Deserialize, Serialize};

/// A face to leave uncovered, identified by a point on it.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct VisibleFace {
    /// Videos: time of the frame the point refers to, in seconds from the
    /// first frame. Ignored for photos.
    pub time: f64,
    /// Point on the face, as a fraction of the upright picture's width and
    /// height.
    pub x: f32,
    pub y: f32,
}

/// Everything decided for one file besides the global options.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(from = "RawEdits", into = "RawEdits")]
pub struct Edits {
    /// Per-section settings of a video.
    pub timeline: Timeline,
    /// Faces left uncovered.
    pub visible: Vec<VisibleFace>,
}

/// Flat file format: `{"keyframes": [...], "visible": [...]}`.
#[derive(Serialize, Deserialize)]
struct RawEdits {
    #[serde(default)]
    keyframes: Vec<crate::timeline::Keyframe>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    visible: Vec<VisibleFace>,
}

impl From<RawEdits> for Edits {
    fn from(raw: RawEdits) -> Self {
        Self { timeline: Timeline::new(raw.keyframes), visible: raw.visible }
    }
}

impl From<Edits> for RawEdits {
    fn from(e: Edits) -> Self {
        Self { keyframes: e.timeline.keyframes().to_vec(), visible: e.visible }
    }
}

impl From<Timeline> for Edits {
    fn from(timeline: Timeline) -> Self {
        Self { timeline, visible: Vec::new() }
    }
}

impl Edits {
    pub fn validate(&self) -> crate::Result<()> {
        self.timeline.validate()?;
        for v in &self.visible {
            let ok = v.time.is_finite() && v.time >= 0.0 && (0.0..=1.0).contains(&v.x) && (0.0..=1.0).contains(&v.y);
            if !ok {
                return Err(crate::Error::InvalidOptions(format!(
                    "invalid visible face mark at {:.3}s ({}, {}): x and y must be in 0..=1",
                    v.time, v.x, v.y
                )));
            }
        }
        Ok(())
    }
}

/// A box as fractions of the upright picture's width and height.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct FaceBox {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl FaceBox {
    /// Normalises a face of a `w`×`h` upright picture.
    pub(crate) fn of(f: &Face, w: f32, h: f32) -> Self {
        Self { x: f.x / w, y: f.y / h, width: f.width / w, height: f.height / h }
    }

    /// The box in pixels of a `w`×`h` picture, as a face (for overlap tests).
    pub(crate) fn to_face(self, w: f32, h: f32) -> Face {
        Face {
            x: self.x * w,
            y: self.y * h,
            width: self.width * w,
            height: self.height * h,
            landmarks: Default::default(),
            score: 1.0,
        }
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x <= self.x + self.width && y >= self.y && y <= self.y + self.height
    }

    pub fn center(&self) -> (f32, f32) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }

    /// Intersection over union of two boxes.
    pub fn iou(&self, other: &FaceBox) -> f32 {
        self.to_face(1.0, 1.0).iou(&other.to_face(1.0, 1.0))
    }
}

/// A face found in a preview.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct PreviewFace {
    pub bbox: FaceBox,
    /// Left uncovered because of a [`VisibleFace`] mark.
    pub visible: bool,
}

/// Overlap with the previous position needed to continue a track.
const TRACK_IOU: f32 = 0.3;

/// The face a mark at `p` designates: the smallest face containing the point,
/// else the nearest one whose centre is within its own size; `None` if the
/// point is on no face. Faces already `taken` are skipped.
pub(crate) fn pick(faces: &[Face], p: Point, taken: &[bool]) -> Option<usize> {
    let dist = |f: &Face| {
        let c = f.center();
        ((c.x - p.x).powi(2) + (c.y - p.y).powi(2)).sqrt()
    };
    let inside = |f: &Face| p.x >= f.x && p.x <= f.x + f.width && p.y >= f.y && p.y <= f.y + f.height;
    let free = || faces.iter().enumerate().filter(|(i, _)| !taken.get(*i).copied().unwrap_or(false));
    free()
        .filter(|(_, f)| inside(f))
        .min_by(|a, b| (a.1.width * a.1.height).total_cmp(&(b.1.width * b.1.height)))
        .or_else(|| {
            free().filter(|(_, f)| dist(f) <= f.width.max(f.height)).min_by(|a, b| dist(a.1).total_cmp(&dist(b.1)))
        })
        .map(|(i, _)| i)
}

/// Faces of a photo designated by `marks` (points in the same pixel space as
/// the faces); one flag per face.
pub(crate) fn select(faces: &[Face], marks: &[Point]) -> Vec<bool> {
    let mut visible = vec![false; faces.len()];
    for &p in marks {
        if let Some(i) = pick(faces, p, &visible) {
            visible[i] = true;
        }
    }
    visible
}

#[derive(Debug, Clone, Copy)]
enum State {
    /// Not reached yet, or looking for the face near the mark.
    Waiting,
    Active {
        last: Face,
        last_seen: u64,
    },
    Ended,
    /// No face at the mark within the tolerance.
    NotFound,
}

#[derive(Debug, Clone)]
struct Mark {
    frame: u64,
    point: Point,
    state: State,
    start: Option<u64>,
    end: Option<u64>,
}

/// Follows the marked faces through the frames of a video, in order.
#[derive(Debug, Clone)]
pub(crate) struct Tracker {
    marks: Vec<Mark>,
    /// Frames a face may be missing before its track ends.
    tolerance: u64,
}

/// What became of one mark.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Span {
    /// First and last frame where the face was left visible; `None` if the
    /// face was never found at the mark.
    pub frames: Option<(u64, u64)>,
    /// The track ended before the last processed frame.
    pub lost: bool,
}

impl Tracker {
    /// `fps` maps mark times to frames; `width`×`height` is the pixel space
    /// of the faces given to [`Tracker::step`].
    pub fn new(visible: &[VisibleFace], fps: f64, width: f32, height: f32, tolerance: u64) -> Self {
        let marks = visible
            .iter()
            .map(|v| Mark {
                frame: (v.time.max(0.0) * fps).round() as u64,
                point: Point::new(v.x * width, v.y * height),
                state: State::Waiting,
                start: None,
                end: None,
            })
            .collect();
        Self { marks, tolerance }
    }

    /// First frame that matters (the earliest mark).
    pub fn first_frame(&self) -> Option<u64> {
        self.marks.iter().map(|m| m.frame).min()
    }

    /// Whether every mark is resolved: nothing can become visible any more.
    pub fn finished(&self) -> bool {
        self.marks.iter().all(|m| matches!(m.state, State::Ended | State::NotFound))
    }

    /// Advances to frame `idx` (frames must come in order). `faces` is
    /// `None` when no detection ran on this frame. Returns, per face, the
    /// index of the mark that keeps it visible.
    pub fn step(&mut self, idx: u64, faces: Option<&[Face]>) -> Vec<Option<usize>> {
        let tol = self.tolerance;
        let Some(faces) = faces else {
            for m in &mut self.marks {
                expire(m, idx, tol);
            }
            return Vec::new();
        };
        let mut owner: Vec<Option<usize>> = vec![None; faces.len()];
        let taken = |owner: &[Option<usize>]| owner.iter().map(Option::is_some).collect::<Vec<_>>();

        // Continue the active tracks: best overlap with the last position.
        for (mi, m) in self.marks.iter_mut().enumerate() {
            let State::Active { last, .. } = m.state else { continue };
            let best = faces
                .iter()
                .enumerate()
                .filter(|(i, _)| owner[*i].is_none())
                .map(|(i, f)| (i, f.iou(&last)))
                .filter(|&(_, iou)| iou >= TRACK_IOU)
                .max_by(|a, b| a.1.total_cmp(&b.1));
            match best {
                Some((i, _)) => {
                    owner[i] = Some(mi);
                    m.state = State::Active { last: faces[i], last_seen: idx };
                    m.end = Some(idx);
                }
                None => expire(m, idx, tol),
            }
        }
        // Start the tracks whose mark is reached.
        for (mi, m) in self.marks.iter_mut().enumerate() {
            if !matches!(m.state, State::Waiting) || idx < m.frame {
                continue;
            }
            match pick(faces, m.point, &taken(&owner)) {
                Some(i) => {
                    owner[i] = Some(mi);
                    m.state = State::Active { last: faces[i], last_seen: idx };
                    m.start = Some(idx);
                    m.end = Some(idx);
                }
                None => expire(m, idx, tol),
            }
        }
        owner
    }

    /// Outcome of each mark, in the order they were given.
    pub fn spans(&self) -> Vec<Span> {
        self.marks
            .iter()
            .map(|m| Span { frames: m.start.zip(m.end), lost: matches!(m.state, State::Ended | State::NotFound) })
            .collect()
    }
}

/// Ends a track (or gives up a mark) once it has been missing for too long.
fn expire(m: &mut Mark, idx: u64, tol: u64) {
    match m.state {
        State::Active { last_seen, .. } if idx > last_seen + tol => m.state = State::Ended,
        State::Waiting if idx > m.frame + tol => m.state = State::NotFound,
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(x: f32, y: f32, s: f32) -> Face {
        Face { x, y, width: s, height: s, landmarks: Default::default(), score: 0.9 }
    }

    #[test]
    fn pick_prefers_the_face_under_the_point() {
        let faces = [face(0.0, 0.0, 100.0), face(40.0, 40.0, 20.0), face(300.0, 0.0, 50.0)];
        assert_eq!(pick(&faces, Point::new(50.0, 50.0), &[]), Some(1), "smallest containing face");
        assert_eq!(pick(&faces, Point::new(10.0, 10.0), &[]), Some(0));
        assert_eq!(pick(&faces, Point::new(360.0, 20.0), &[]), Some(2), "close to a face");
        assert_eq!(pick(&faces, Point::new(200.0, 300.0), &[]), None, "on no face");
        assert_eq!(select(&faces, &[Point::new(50.0, 50.0), Point::new(52.0, 52.0)]), vec![true, true, false]);
    }

    #[test]
    fn a_marked_face_is_followed_until_lost() {
        let v = VisibleFace { time: 1.0, x: 0.1, y: 0.1 };
        let mut t = Tracker::new(&[v], 10.0, 1000.0, 1000.0, 3);
        let other = face(600.0, 600.0, 100.0);
        // Before the mark: nothing visible.
        assert_eq!(t.step(9, Some(&[face(50.0, 50.0, 100.0), other])), vec![None, None]);
        // At the mark, then moving a little each frame.
        for (i, idx) in (10..20).enumerate() {
            let x = 50.0 + i as f32 * 10.0;
            assert_eq!(t.step(idx, Some(&[other, face(x, 50.0, 100.0)])), vec![None, Some(0)], "frame {idx}");
        }
        // Missing for longer than the tolerance: the track ends.
        for idx in 20..24 {
            t.step(idx, Some(&[other]));
        }
        assert!(t.finished());
        assert_eq!(t.step(24, Some(&[face(150.0, 50.0, 100.0)])), vec![None], "never resumes");
        assert_eq!(t.spans(), vec![Span { frames: Some((10, 19)), lost: true }]);
    }

    #[test]
    fn short_gaps_and_skipped_frames_keep_the_track() {
        let v = VisibleFace { time: 0.0, x: 0.1, y: 0.1 };
        let mut t = Tracker::new(&[v], 25.0, 1000.0, 1000.0, 4);
        assert_eq!(t.step(0, Some(&[face(50.0, 50.0, 100.0)])), vec![Some(0)]);
        t.step(1, None);
        t.step(2, Some(&[]));
        assert_eq!(t.step(3, Some(&[face(55.0, 50.0, 100.0)])), vec![Some(0)]);
    }

    #[test]
    fn a_track_never_jumps_to_a_distant_face() {
        let v = VisibleFace { time: 0.0, x: 0.1, y: 0.1 };
        let mut t = Tracker::new(&[v], 25.0, 1000.0, 1000.0, 4);
        t.step(0, Some(&[face(50.0, 50.0, 100.0)]));
        assert_eq!(t.step(1, Some(&[face(400.0, 50.0, 100.0)])), vec![None]);
    }

    #[test]
    fn a_mark_on_no_face_is_given_up() {
        let v = VisibleFace { time: 0.0, x: 0.9, y: 0.9 };
        let mut t = Tracker::new(&[v], 25.0, 1000.0, 1000.0, 2);
        for idx in 0..4 {
            assert_eq!(t.step(idx, Some(&[face(50.0, 50.0, 100.0)])), vec![None]);
        }
        assert!(t.finished());
        assert_eq!(t.spans(), vec![Span { frames: None, lost: true }]);
    }

    #[test]
    fn edits_file_format() {
        let e: Edits =
            serde_json::from_str(r#"{"keyframes":[{"time":2,"cut":true}],"visible":[{"time":1.5,"x":0.4,"y":0.3}]}"#)
                .unwrap();
        assert_eq!(e.timeline.keyframes().len(), 1);
        assert_eq!(e.visible, vec![VisibleFace { time: 1.5, x: 0.4, y: 0.3 }]);
        assert_eq!(serde_json::from_str::<Edits>(&serde_json::to_string(&e).unwrap()).unwrap(), e);
        let old: Edits = serde_json::from_str(r#"{"keyframes":[]}"#).unwrap();
        assert!(old.visible.is_empty(), "timeline-only files still load");
        let bad = Edits { visible: vec![VisibleFace { time: 0.0, x: 1.5, y: 0.0 }], ..Default::default() };
        assert!(bad.validate().is_err());
    }
}
