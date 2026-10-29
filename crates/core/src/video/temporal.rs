//! Temporal smoothing of detections.
//!
//! Detectors occasionally miss a face for a frame or two (motion blur,
//! occlusion, profile views). Leaving such a frame unredacted would leak the
//! identity, so each frame is also covered by the detections of its
//! neighbours within `±persistence` frames. Frames are released once all
//! their future neighbours are known, which introduces a delay of
//! `persistence` frames but no reordering.

use crate::detect::Face;
use std::collections::VecDeque;

/// Neighbour detections overlapping an own detection by more than this are
/// considered the same face.
const SAME_FACE_IOU: f32 = 0.3;

pub struct Temporal<T> {
    persistence: u64,
    /// Frames waiting for their future neighbours, in index order.
    pending: VecDeque<(u64, T, Vec<Face>)>,
    /// Detections of recently released frames (for backward coverage).
    history: VecDeque<(u64, Vec<Face>)>,
}

impl<T> Temporal<T> {
    pub fn new(persistence: u32) -> Self {
        Self { persistence: persistence as u64, pending: VecDeque::new(), history: VecDeque::new() }
    }

    /// Adds the next frame (indices must be consecutive) and returns the
    /// frames that are ready, with their merged detections.
    pub fn push(&mut self, idx: u64, item: T, faces: Vec<Face>) -> Vec<(u64, T, Vec<Face>)> {
        debug_assert!(self.pending.back().is_none_or(|(i, ..)| *i + 1 == idx));
        self.pending.push_back((idx, item, faces));
        let mut out = Vec::new();
        while let Some(front) = self.pending.front().map(|(i, ..)| *i) {
            if idx < front + self.persistence {
                break;
            }
            out.push(self.release());
        }
        out
    }

    /// Releases every remaining frame (end of stream).
    pub fn flush(&mut self) -> Vec<(u64, T, Vec<Face>)> {
        let mut out = Vec::with_capacity(self.pending.len());
        while !self.pending.is_empty() {
            out.push(self.release());
        }
        out
    }

    fn release(&mut self) -> (u64, T, Vec<Face>) {
        let (idx, item, own) = self.pending.pop_front().expect("pending frame");
        let p = self.persistence;
        // Neighbours sorted by temporal distance: the closest detection wins.
        let mut neighbours: Vec<(u64, &Face)> = self
            .history
            .iter()
            .filter(|(i, _)| idx - i <= p)
            .flat_map(|(i, f)| f.iter().map(move |f| (idx - i, f)))
            .chain(
                self.pending
                    .iter()
                    .filter(|(i, ..)| i - idx <= p)
                    .flat_map(|(i, _, f)| f.iter().map(move |f| (i - idx, f))),
            )
            .collect();
        neighbours.sort_by_key(|(d, _)| *d);
        let mut merged = own.clone();
        for (_, f) in neighbours {
            if merged.iter().all(|m| m.iou(f) <= SAME_FACE_IOU) {
                merged.push(*f);
            }
        }
        self.history.push_back((idx, own));
        while self.history.front().is_some_and(|(i, _)| idx - i >= p) {
            self.history.pop_front();
        }
        (idx, item, merged)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::Point;

    fn face(x: f32) -> Face {
        Face { x, y: 0.0, width: 10.0, height: 10.0, landmarks: [Point::default(); 5], score: 1.0 }
    }

    #[test]
    fn fills_gaps_and_preserves_order() {
        let mut t = Temporal::new(2);
        let mut out = Vec::new();
        // Face seen on frames 0 and 3; frames 1, 2 are misses; 6 is far away.
        let dets = [vec![face(0.0)], vec![], vec![], vec![face(1.0)], vec![], vec![], vec![]];
        for (i, d) in dets.into_iter().enumerate() {
            out.extend(t.push(i as u64, i, d));
        }
        out.extend(t.flush());
        let order: Vec<usize> = out.iter().map(|(_, it, _)| *it).collect();
        assert_eq!(order, vec![0, 1, 2, 3, 4, 5, 6]);
        let counts: Vec<usize> = out.iter().map(|(.., f)| f.len()).collect();
        // Frame 1/2: covered by both neighbours but they overlap (same face) -> 1.
        assert_eq!(counts, vec![1, 1, 1, 1, 1, 1, 0]);
    }

    #[test]
    fn zero_persistence_is_passthrough() {
        let mut t = Temporal::new(0);
        assert_eq!(t.push(0, 'a', vec![]).len(), 1);
        assert_eq!(t.push(1, 'b', vec![face(0.0)])[0].2.len(), 1);
        assert!(t.flush().is_empty());
    }
}
