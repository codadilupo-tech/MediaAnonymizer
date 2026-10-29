//! Per-section video settings.
//!
//! A [`Timeline`] is a list of [`Keyframe`]s. Each keyframe starts a
//! *section* that lasts until the next keyframe (or the end of the video) and
//! decides, for the frames of that section, whether faces are covered (and
//! how), whether the timestamp is drawn, or whether the frames are cut from
//! the output altogether. Before the first keyframe the global [`Options`]
//! apply.
//!
//! ```
//! use mediaanon_core::{FaceOptions, Keyframe, RedactionStyle, Timeline};
//!
//! let timeline = Timeline::new(vec![
//!     // From 10 s: blur instead of the global style.
//!     Keyframe { time: 10.0, faces: Some(FaceOptions { style: RedactionStyle::Blur, ..Default::default() }), ..Default::default() },
//!     // From 20 s to 25 s: removed from the output.
//!     Keyframe { time: 20.0, cut: true, ..Default::default() },
//!     // From 25 s: nothing covered.
//!     Keyframe { time: 25.0, faces: None, ..Default::default() },
//! ]);
//! assert_eq!(timeline.cut_ranges(), vec![(20.0, 25.0)]);
//! ```
//!
//! [`Options`]: crate::Options

use crate::options::{FaceOptions, Options, TimestampOptions};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};

/// Start of a section and its settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Keyframe {
    /// Start of the section, in seconds from the first video frame.
    pub time: f64,
    /// Face redaction in this section; `None` leaves faces uncovered.
    pub faces: Option<FaceOptions>,
    /// Draw the timestamp in this section (when the overlay is enabled in
    /// the global options).
    pub timestamp: bool,
    /// Remove the section from the output (video and audio).
    pub cut: bool,
}

impl Default for Keyframe {
    fn default() -> Self {
        Self { time: 0.0, faces: Some(FaceOptions::default()), timestamp: true, cut: false }
    }
}

impl Keyframe {
    /// A keyframe at `time` that repeats the global settings.
    pub fn from_options(time: f64, options: &Options) -> Self {
        Self { time, faces: options.faces.clone(), timestamp: true, cut: false }
    }
}

/// Settings that apply to one frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Section<'a> {
    pub faces: Option<&'a FaceOptions>,
    pub timestamp: Option<&'a TimestampOptions>,
    pub cut: bool,
}

/// Keyframes of one video, sorted by time.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(from = "RawTimeline")]
pub struct Timeline {
    keyframes: Vec<Keyframe>,
}

/// Deserialization helper: keyframes in files may be in any order.
#[derive(Deserialize)]
struct RawTimeline {
    #[serde(default)]
    keyframes: Vec<Keyframe>,
}

impl From<RawTimeline> for Timeline {
    fn from(raw: RawTimeline) -> Self {
        Self::new(raw.keyframes)
    }
}

impl Timeline {
    /// Builds a timeline; keyframes are sorted by time.
    pub fn new(mut keyframes: Vec<Keyframe>) -> Self {
        keyframes.sort_by(|a, b| a.time.total_cmp(&b.time));
        Self { keyframes }
    }

    pub fn keyframes(&self) -> &[Keyframe] {
        &self.keyframes
    }

    pub fn is_empty(&self) -> bool {
        self.keyframes.is_empty()
    }

    /// Adds a keyframe, replacing one at the same time (within `epsilon`
    /// seconds); returns its index.
    pub fn insert(&mut self, keyframe: Keyframe, epsilon: f64) -> usize {
        if let Some(i) = self.keyframes.iter().position(|k| (k.time - keyframe.time).abs() <= epsilon) {
            self.keyframes[i] = keyframe;
            return i;
        }
        let i = self.keyframes.partition_point(|k| k.time < keyframe.time);
        self.keyframes.insert(i, keyframe);
        i
    }

    pub fn remove(&mut self, index: usize) -> Option<Keyframe> {
        (index < self.keyframes.len()).then(|| self.keyframes.remove(index))
    }

    pub fn get_mut(&mut self, index: usize) -> Option<&mut Keyframe> {
        self.keyframes.get_mut(index)
    }

    /// Index of the keyframe whose section contains `time`, `None` before the
    /// first keyframe.
    pub fn index_at(&self, time: f64) -> Option<usize> {
        self.keyframes.partition_point(|k| k.time <= time).checked_sub(1)
    }

    /// Settings in effect at `time`.
    pub fn section_at<'a>(&'a self, time: f64, options: &'a Options) -> Section<'a> {
        section(self.index_at(time).map(|i| &self.keyframes[i]), options)
    }

    /// Whether any section needs the face detector.
    pub fn uses_faces(&self) -> bool {
        self.keyframes.iter().any(|k| !k.cut && k.faces.is_some())
    }

    pub fn has_cuts(&self) -> bool {
        self.keyframes.iter().any(|k| k.cut)
    }

    /// Cut sections as merged `[start, end)` ranges in seconds; the last
    /// range ends at infinity when the video ends in a cut.
    pub fn cut_ranges(&self) -> Vec<(f64, f64)> {
        let mut out: Vec<(f64, f64)> = Vec::new();
        for (i, k) in self.keyframes.iter().enumerate() {
            if !k.cut {
                continue;
            }
            let end = self.keyframes.get(i + 1).map_or(f64::INFINITY, |n| n.time);
            match out.last_mut() {
                Some(last) if last.1 >= k.time => last.1 = end,
                _ => out.push((k.time, end)),
            }
        }
        out
    }

    /// Checks times and per-section face settings.
    pub fn validate(&self) -> Result<()> {
        for k in &self.keyframes {
            if !(k.time.is_finite() && k.time >= 0.0) {
                return Err(Error::InvalidOptions(format!("invalid keyframe time {}", k.time)));
            }
            if let Some(f) = &k.faces {
                f.validate().map_err(|e| Error::InvalidOptions(format!("keyframe at {:.3}s: {e}", k.time)))?;
            }
        }
        Ok(())
    }
}

/// Settings of the section started by `keyframe` (`None`: global settings).
fn section<'a>(keyframe: Option<&'a Keyframe>, options: &'a Options) -> Section<'a> {
    match keyframe {
        None => Section { faces: options.faces.as_ref(), timestamp: options.timestamp.as_ref(), cut: false },
        Some(k) => Section {
            faces: k.faces.as_ref().filter(|_| !k.cut),
            timestamp: options.timestamp.as_ref().filter(|_| k.timestamp && !k.cut),
            cut: k.cut,
        },
    }
}

/// Maps the timeline onto frame indices of a constant-rate stream.
#[derive(Debug)]
pub(crate) struct FrameSections<'a> {
    options: &'a Options,
    timeline: &'a Timeline,
    /// First frame of each keyframe's section.
    starts: Vec<u64>,
}

impl<'a> FrameSections<'a> {
    pub fn new(options: &'a Options, timeline: &'a Timeline, fps: f64) -> Self {
        let starts = timeline.keyframes.iter().map(|k| (k.time * fps).round().max(0.0) as u64).collect();
        Self { options, timeline, starts }
    }

    /// Settings of frame `idx`.
    pub fn at(&self, idx: u64) -> Section<'a> {
        let i = self.starts.partition_point(|&s| s <= idx).checked_sub(1);
        section(i.map(|i| &self.timeline.keyframes[i]), self.options)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> Options {
        Options {
            faces: Some(FaceOptions::default()),
            timestamp: Some(TimestampOptions::default()),
            ..Default::default()
        }
    }

    #[test]
    fn sections_follow_keyframes() {
        let o = opts();
        let t = Timeline::new(vec![
            Keyframe { time: 5.0, cut: true, ..Default::default() },
            Keyframe { time: 2.0, faces: None, timestamp: false, cut: false },
        ]);
        assert_eq!(t.keyframes()[0].time, 2.0, "sorted");
        let s = t.section_at(1.0, &o);
        assert!(s.faces.is_some() && s.timestamp.is_some() && !s.cut);
        let s = t.section_at(2.0, &o);
        assert!(s.faces.is_none() && s.timestamp.is_none() && !s.cut);
        let s = t.section_at(7.0, &o);
        assert!(s.cut && s.faces.is_none() && s.timestamp.is_none());
        assert_eq!(t.cut_ranges(), vec![(5.0, f64::INFINITY)]);
        assert!(!t.uses_faces());
    }

    #[test]
    fn timestamp_needs_the_global_overlay() {
        let o = Options { faces: Some(FaceOptions::default()), ..Default::default() };
        let t = Timeline::new(vec![Keyframe { time: 0.0, ..Default::default() }]);
        assert!(t.section_at(1.0, &o).timestamp.is_none());
    }

    #[test]
    fn adjacent_cuts_merge() {
        let t = Timeline::new(vec![
            Keyframe { time: 1.0, cut: true, ..Default::default() },
            Keyframe { time: 2.0, cut: true, ..Default::default() },
            Keyframe { time: 3.0, ..Default::default() },
            Keyframe { time: 4.0, cut: true, ..Default::default() },
            Keyframe { time: 4.5, ..Default::default() },
        ]);
        assert_eq!(t.cut_ranges(), vec![(1.0, 3.0), (4.0, 4.5)]);
    }

    #[test]
    fn frames_map_to_sections() {
        let o = opts();
        let t = Timeline::new(vec![Keyframe { time: 1.0, cut: true, ..Default::default() }]);
        let f = FrameSections::new(&o, &t, 25.0);
        assert!(!f.at(24).cut);
        assert!(f.at(25).cut);
        assert!(f.at(1000).cut);
    }

    #[test]
    fn insert_replaces_same_time() {
        let mut t = Timeline::default();
        assert_eq!(t.insert(Keyframe { time: 2.0, ..Default::default() }, 0.01), 0);
        assert_eq!(t.insert(Keyframe { time: 1.0, ..Default::default() }, 0.01), 0);
        assert_eq!(t.insert(Keyframe { time: 2.001, cut: true, ..Default::default() }, 0.01), 1);
        assert_eq!(t.keyframes().len(), 2);
        assert!(t.keyframes()[1].cut);
        assert_eq!(t.index_at(0.5), None);
        assert_eq!(t.index_at(1.5), Some(0));
    }

    #[test]
    fn json_round_trip() {
        let t = Timeline::new(vec![Keyframe { time: 3.5, cut: true, ..Default::default() }]);
        let s = serde_json::to_string(&t).unwrap();
        assert_eq!(serde_json::from_str::<Timeline>(&s).unwrap(), t);
        let parsed: Timeline =
            serde_json::from_str(r#"{"keyframes":[{"time":4,"cut":true},{"time":1,"faces":null}]}"#).unwrap();
        assert_eq!(parsed.keyframes()[0].time, 1.0, "sorted on load");
        assert!(parsed.keyframes()[0].faces.is_none() && parsed.keyframes()[0].timestamp);
    }
}
