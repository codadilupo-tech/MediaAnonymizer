//! Rotated rectangles and ellipses.

/// A redaction outline in image coordinates. `angle` is in radians,
/// clockwise in image space (y grows downwards).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Shape {
    Rect { cx: f32, cy: f32, hw: f32, hh: f32, angle: f32 },
    Ellipse { cx: f32, cy: f32, rx: f32, ry: f32, angle: f32 },
}

impl Shape {
    fn center_axes_angle(&self) -> (f32, f32, f32, f32, f32) {
        match *self {
            Shape::Rect { cx, cy, hw, hh, angle } => (cx, cy, hw, hh, angle),
            Shape::Ellipse { cx, cy, rx, ry, angle } => (cx, cy, rx, ry, angle),
        }
    }

    /// Whether the point lies inside the shape.
    #[inline]
    pub fn contains(&self, x: f32, y: f32) -> bool {
        let (cx, cy, a, b, angle) = self.center_axes_angle();
        let (s, c) = angle.sin_cos();
        let (dx, dy) = (x - cx, y - cy);
        // Rotate the point into the shape's frame.
        let u = dx * c + dy * s;
        let v = -dx * s + dy * c;
        match self {
            Shape::Rect { .. } => u.abs() <= a && v.abs() <= b,
            Shape::Ellipse { .. } => {
                let (nu, nv) = (u / a.max(1e-3), v / b.max(1e-3));
                nu * nu + nv * nv <= 1.0
            }
        }
    }

    /// Axis-aligned bounding box `(x0, y0, x1, y1)`.
    pub fn bounds(&self) -> (f32, f32, f32, f32) {
        let (cx, cy, a, b, angle) = self.center_axes_angle();
        let (s, c) = angle.sin_cos();
        let (ex, ey) = match self {
            // Extent of a rotated rectangle / ellipse along x and y.
            Shape::Rect { .. } => (a * c.abs() + b * s.abs(), a * s.abs() + b * c.abs()),
            Shape::Ellipse { .. } => ((a * a * c * c + b * b * s * s).sqrt(), (a * a * s * s + b * b * c * c).sqrt()),
        };
        (cx - ex, cy - ey, cx + ex, cy + ey)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotated_rect_bounds_contain_shape() {
        let s = Shape::Rect { cx: 50.0, cy: 50.0, hw: 20.0, hh: 5.0, angle: 0.6 };
        let (x0, y0, x1, y1) = s.bounds();
        for y in 0..100 {
            for x in 0..100 {
                let (fx, fy) = (x as f32, y as f32);
                if s.contains(fx, fy) {
                    assert!(fx >= x0 && fx <= x1 && fy >= y0 && fy <= y1);
                }
            }
        }
    }

    #[test]
    fn ellipse_contains() {
        let s = Shape::Ellipse { cx: 0.0, cy: 0.0, rx: 10.0, ry: 5.0, angle: 0.0 };
        assert!(s.contains(9.9, 0.0));
        assert!(!s.contains(0.0, 5.1));
        assert_eq!(s.bounds(), (-10.0, -5.0, 10.0, 5.0));
    }
}
