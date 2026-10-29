//! Face geometry and the coordinate transforms applied to detections.

use serde::Serialize;

/// A 2-D point in pixel coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// A detected face.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Face {
    /// Top-left corner of the bounding box.
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// Right eye, left eye, nose tip, right mouth corner, left mouth corner
    /// (subject's point of view: the "right eye" appears on the image left).
    pub landmarks: [Point; 5],
    /// Detector confidence in `0..=1`.
    pub score: f32,
}

impl Face {
    pub fn center(&self) -> Point {
        Point::new(self.x + self.width / 2.0, self.y + self.height / 2.0)
    }

    /// Intersection over union of the two bounding boxes.
    pub fn iou(&self, other: &Face) -> f32 {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = (self.x + self.width).min(other.x + other.width);
        let y1 = (self.y + self.height).min(other.y + other.height);
        let inter = (x1 - x0).max(0.0) * (y1 - y0).max(0.0);
        let union = self.width * self.height + other.width * other.height - inter;
        if union <= 0.0 {
            0.0
        } else {
            inter / union
        }
    }

    /// Applies `f` to every point of the face and recomputes the bounding box.
    ///
    /// Used to map detections from the detector frame back to the source
    /// (scaling, undoing EXIF rotations...).
    pub fn map(&self, f: impl Fn(Point) -> Point) -> Face {
        let a = f(Point::new(self.x, self.y));
        let b = f(Point::new(self.x + self.width, self.y + self.height));
        Face {
            x: a.x.min(b.x),
            y: a.y.min(b.y),
            width: (a.x - b.x).abs(),
            height: (a.y - b.y).abs(),
            landmarks: self.landmarks.map(&f),
            score: self.score,
        }
    }

    /// Scales all coordinates.
    pub fn scaled(&self, sx: f32, sy: f32) -> Face {
        self.map(|p| Point::new(p.x * sx, p.y * sy))
    }
}

/// Greedy non-maximum suppression; `faces` is sorted by descending score.
pub(crate) fn nms(mut faces: Vec<Face>, iou_threshold: f32) -> Vec<Face> {
    faces.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut keep: Vec<Face> = Vec::with_capacity(faces.len().min(64));
    for f in faces {
        if keep.iter().all(|k| k.iou(&f) <= iou_threshold) {
            keep.push(f);
        }
    }
    keep
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(x: f32, y: f32, s: f32, score: f32) -> Face {
        Face { x, y, width: s, height: s, landmarks: [Point::default(); 5], score }
    }

    #[test]
    fn iou_and_nms() {
        let a = face(0.0, 0.0, 10.0, 0.9);
        let b = face(1.0, 1.0, 10.0, 0.8);
        let c = face(50.0, 50.0, 10.0, 0.7);
        assert!((a.iou(&a) - 1.0).abs() < 1e-6);
        assert_eq!(a.iou(&c), 0.0);
        let kept = nms(vec![b, c, a], 0.3);
        assert_eq!(kept, vec![a, c]);
    }

    #[test]
    fn map_normalises_boxes() {
        // A 90° rotation flips the box corners; the result must stay positive.
        let f = face(10.0, 20.0, 5.0, 1.0).map(|p| Point::new(100.0 - p.y, p.x));
        assert_eq!((f.x, f.y, f.width, f.height), (75.0, 10.0, 5.0, 5.0));
    }
}
