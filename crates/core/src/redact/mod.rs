//! Face redaction: turns detections into regions and paints them.
//!
//! Regions are defined in full-resolution image coordinates. They are painted
//! on [`Plane`]s, which can describe an interleaved RGB(A) buffer, a single
//! luma plane or a subsampled chroma plane, in 8 or 16 bits. The same code
//! therefore serves photos and native-format video frames without any color
//! conversion, so pixels outside the regions are never modified.

mod plane;
mod shape;

pub use plane::{Plane, Sample};
pub use shape::Shape;

use crate::detect::Face;
use crate::options::{FaceMode, FaceOptions, FaceShape, RedactionStyle};

/// An area to redact.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Region {
    pub shape: Shape,
    /// Characteristic size of the face (pixels), used to scale blur radius and
    /// mosaic block size so the effect is equally strong on every face.
    pub scale: f32,
}

/// Default blur radius as a fraction of the face size.
const BLUR_FACTOR: f32 = 0.18;
/// Default number of mosaic blocks across a face.
const PIXEL_BLOCKS: f32 = 7.0;

/// Computes the regions covering `faces` according to `opts`.
pub fn regions(faces: &[Face], opts: &FaceOptions) -> Vec<Region> {
    faces.iter().filter_map(|f| region(f, opts)).collect()
}

fn region(f: &Face, opts: &FaceOptions) -> Option<Region> {
    let [re, le, ..] = f.landmarks;
    let (dx, dy) = (le.x - re.x, le.y - re.y);
    let eye_dist = (dx * dx + dy * dy).sqrt();
    // Roll of the head; ignore landmarks that are obviously inconsistent.
    let landmarks_ok = eye_dist > f.width * 0.15 && eye_dist < f.width * 1.5;
    let angle = if landmarks_ok { dy.atan2(dx) } else { 0.0 };
    let p = opts.padding;
    match opts.mode {
        FaceMode::FullFace => {
            let c = f.center();
            // YuNet boxes stop at the eyebrows: extend upwards to cover the forehead.
            let hw = f.width * (0.5 + p);
            let hh = f.height * (0.5 + p) * 1.1;
            let (cx, cy) = (c.x, c.y - f.height * 0.05);
            let shape = match opts.shape {
                FaceShape::Ellipse => Shape::Ellipse { cx, cy, rx: hw * 1.08, ry: hh * 1.08, angle },
                FaceShape::Rectangle => Shape::Rect { cx, cy, hw, hh, angle },
            };
            Some(Region { shape, scale: f.width.max(f.height) })
        }
        FaceMode::EyesOnly => {
            let eye_dist = if landmarks_ok { eye_dist } else { f.width * 0.4 };
            let (cx, cy) = if landmarks_ok {
                ((re.x + le.x) / 2.0, (re.y + le.y) / 2.0)
            } else {
                (f.x + f.width / 2.0, f.y + f.height * 0.38)
            };
            let hw = eye_dist * (1.0 + p * 2.0);
            let hh = eye_dist * (0.3 + p);
            Some(Region { shape: Shape::Rect { cx, cy, hw, hh, angle }, scale: eye_dist * 2.0 })
        }
    }
}

/// Paints `regions` onto a plane.
///
/// `fill` is the value of every channel for [`RedactionStyle::BlackBand`]
/// (black in that plane's color space).
pub fn paint<T: Sample>(
    plane: &mut Plane<'_, T>,
    regions: &[Region],
    style: RedactionStyle,
    strength: f32,
    fill: &[T],
) {
    for r in regions {
        match style {
            RedactionStyle::BlackBand => plane.fill(&r.shape, fill),
            RedactionStyle::Pixelate => plane.pixelate(&r.shape, (r.scale / PIXEL_BLOCKS * strength).max(4.0)),
            RedactionStyle::Blur => plane.blur(&r.shape, (r.scale * BLUR_FACTOR * strength).max(3.0)),
        }
    }
}

/// Bounding box `(x0, y0, x1, y1)` of all regions, clipped to `w`×`h`.
pub fn bounds(regions: &[Region], w: u32, h: u32) -> Option<(u32, u32, u32, u32)> {
    let mut acc: Option<(f32, f32, f32, f32)> = None;
    for r in regions {
        let (a, b, c, d) = r.shape.bounds();
        acc = Some(match acc {
            None => (a, b, c, d),
            Some((x0, y0, x1, y1)) => (x0.min(a), y0.min(b), x1.max(c), y1.max(d)),
        });
    }
    let (x0, y0, x1, y1) = acc?;
    let x0 = x0.floor().clamp(0.0, w as f32) as u32;
    let y0 = y0.floor().clamp(0.0, h as f32) as u32;
    let x1 = x1.ceil().clamp(0.0, w as f32) as u32;
    let y1 = y1.ceil().clamp(0.0, h as f32) as u32;
    (x1 > x0 && y1 > y0).then_some((x0, y0, x1, y1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::Point;

    fn face() -> Face {
        Face {
            x: 100.0,
            y: 100.0,
            width: 100.0,
            height: 120.0,
            landmarks: [
                Point::new(130.0, 150.0),
                Point::new(170.0, 150.0),
                Point::new(150.0, 175.0),
                Point::new(135.0, 195.0),
                Point::new(165.0, 195.0),
            ],
            score: 0.9,
        }
    }

    #[test]
    fn full_face_covers_box() {
        let r = regions(&[face()], &FaceOptions::default());
        let s = r[0].shape;
        for (x, y) in [(150.0, 160.0), (110.0, 160.0), (150.0, 105.0), (150.0, 215.0)] {
            assert!(s.contains(x, y), "({x},{y}) must be covered");
        }
        assert!(!s.contains(20.0, 20.0));
    }

    #[test]
    fn eyes_band_covers_eyes_only() {
        let opts = FaceOptions { mode: FaceMode::EyesOnly, ..Default::default() };
        let s = regions(&[face()], &opts)[0].shape;
        assert!(s.contains(130.0, 150.0) && s.contains(170.0, 150.0));
        assert!(!s.contains(150.0, 195.0), "mouth must stay visible");
    }

    #[test]
    fn tilted_band_follows_eye_line() {
        let mut f = face();
        f.landmarks[1] = Point::new(170.0, 190.0); // head rolled ~45°
        let opts = FaceOptions { mode: FaceMode::EyesOnly, ..Default::default() };
        let s = regions(&[f], &opts)[0].shape;
        assert!(s.contains(130.0, 150.0) && s.contains(170.0, 190.0));
        assert!(!s.contains(170.0, 150.0));
    }
}
