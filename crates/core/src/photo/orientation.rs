//! EXIF orientation handling.
//!
//! Faces are detected on the image *as displayed* (upright), but pixels are
//! redacted in the *stored* orientation so that the encoded data and the EXIF
//! orientation tag stay untouched.

use crate::detect::Point;
use image::metadata::Orientation;

/// Size of the displayed image for a stored `w`×`h` image.
pub fn displayed_dims(o: Orientation, w: u32, h: u32) -> (u32, u32) {
    match o {
        Orientation::Rotate90 | Orientation::Rotate270 | Orientation::Rotate90FlipH | Orientation::Rotate270FlipH => {
            (h, w)
        }
        _ => (w, h),
    }
}

/// Maps a continuous point of the displayed image to the stored image
/// (`w`×`h` are the *stored* dimensions).
pub fn displayed_to_stored(o: Orientation, w: f32, h: f32, p: Point) -> Point {
    let Point { x, y } = p;
    match o {
        Orientation::NoTransforms => Point::new(x, y),
        Orientation::Rotate90 => Point::new(y, h - x),
        Orientation::Rotate180 => Point::new(w - x, h - y),
        Orientation::Rotate270 => Point::new(w - y, x),
        Orientation::FlipHorizontal => Point::new(w - x, y),
        Orientation::FlipVertical => Point::new(x, h - y),
        Orientation::Rotate90FlipH => Point::new(y, x),
        Orientation::Rotate270FlipH => Point::new(w - y, h - x),
    }
}

/// Inverse of [`displayed_to_stored`]: where a stored point is displayed
/// (`w`×`h` are the *stored* dimensions).
pub fn stored_to_displayed(o: Orientation, w: f32, h: f32, p: Point) -> Point {
    let Point { x, y } = p;
    match o {
        Orientation::NoTransforms => Point::new(x, y),
        Orientation::Rotate90 => Point::new(h - y, x),
        Orientation::Rotate180 => Point::new(w - x, h - y),
        Orientation::Rotate270 => Point::new(y, w - x),
        Orientation::FlipHorizontal => Point::new(w - x, y),
        Orientation::FlipVertical => Point::new(x, h - y),
        Orientation::Rotate90FlipH => Point::new(y, x),
        Orientation::Rotate270FlipH => Point::new(h - y, w - x),
    }
}

/// Stored pixel shown at displayed pixel `(x, y)`.
pub fn stored_pixel(o: Orientation, w: u32, h: u32, x: u32, y: u32) -> (u32, u32) {
    let p = displayed_to_stored(o, w as f32, h as f32, Point::new(x as f32 + 0.5, y as f32 + 0.5));
    ((p.x.max(0.0) as u32).min(w - 1), (p.y.max(0.0) as u32).min(h - 1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, GenericImageView, Rgb, RgbImage};

    #[test]
    fn stored_to_displayed_inverts() {
        let all = [
            Orientation::NoTransforms,
            Orientation::Rotate90,
            Orientation::Rotate180,
            Orientation::Rotate270,
            Orientation::FlipHorizontal,
            Orientation::FlipVertical,
            Orientation::Rotate90FlipH,
            Orientation::Rotate270FlipH,
        ];
        let (w, h) = (40.0, 30.0);
        for o in all {
            let p = Point::new(7.0, 3.0);
            let back = stored_to_displayed(o, w, h, displayed_to_stored(o, w, h, p));
            assert!((back.x - p.x).abs() < 1e-4 && (back.y - p.y).abs() < 1e-4, "{o:?}: {back:?}");
        }
    }

    #[test]
    fn matches_image_crate_semantics() {
        let (w, h) = (5u32, 3u32);
        let stored = RgbImage::from_fn(w, h, |x, y| Rgb([x as u8, y as u8, 0]));
        let all = [
            Orientation::NoTransforms,
            Orientation::Rotate90,
            Orientation::Rotate180,
            Orientation::Rotate270,
            Orientation::FlipHorizontal,
            Orientation::FlipVertical,
            Orientation::Rotate90FlipH,
            Orientation::Rotate270FlipH,
        ];
        for o in all {
            let mut shown = DynamicImage::ImageRgb8(stored.clone());
            shown.apply_orientation(o);
            assert_eq!(shown.dimensions(), displayed_dims(o, w, h), "{o:?}");
            for (x, y, px) in shown.to_rgb8().enumerate_pixels() {
                let (sx, sy) = stored_pixel(o, w, h, x, y);
                assert_eq!(*stored.get_pixel(sx, sy), *px, "{o:?} at ({x},{y})");
            }
        }
    }
}
