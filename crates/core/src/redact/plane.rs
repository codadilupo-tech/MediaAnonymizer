//! Pixel kernels operating on a strided view of an image plane.

use super::shape::Shape;
use rayon::prelude::*;

/// A pixel component type (8-bit or 16-bit integer samples).
pub trait Sample: Copy + Send + Sync + 'static {
    fn to_f32(self) -> f32;
    fn from_f32(v: f32) -> Self;
}

impl Sample for u8 {
    #[inline]
    fn to_f32(self) -> f32 {
        self as f32
    }
    #[inline]
    fn from_f32(v: f32) -> Self {
        v.round().clamp(0.0, 255.0) as u8
    }
}

impl Sample for u16 {
    #[inline]
    fn to_f32(self) -> f32 {
        self as f32
    }
    #[inline]
    fn from_f32(v: f32) -> Self {
        v.round().clamp(0.0, 65535.0) as u16
    }
}

/// A mutable, strided view over one plane of an image.
///
/// * planar data (a YUV plane): `step = 1`, `channels = 1`;
/// * interleaved RGB: `step = 3`, `channels = 3`;
/// * interleaved RGBA where alpha must be preserved: `step = 4`, `channels = 3`.
///
/// `scale_x`/`scale_y` map image coordinates (where regions live) to plane
/// coordinates, e.g. `0.5` for the chroma planes of 4:2:0 video.
#[derive(Debug)]
pub struct Plane<'a, T> {
    data: &'a mut [T],
    width: usize,
    height: usize,
    stride: usize,
    step: usize,
    channels: usize,
    scale_x: f32,
    scale_y: f32,
}

type Rect = (usize, usize, usize, usize);

impl<'a, T: Sample> Plane<'a, T> {
    /// Creates a view. `stride` and `step` are in elements of `T`.
    ///
    /// # Panics
    /// If `data` is too small for the described layout.
    pub fn new(data: &'a mut [T], width: usize, height: usize, stride: usize, step: usize, channels: usize) -> Self {
        assert!((1..=4).contains(&channels) && channels <= step, "invalid channel layout");
        assert!(stride >= width * step, "stride too small");
        if height > 0 {
            assert!(data.len() >= (height - 1) * stride + width * step, "buffer too small for plane");
        }
        Self { data, width, height, stride, step, channels, scale_x: 1.0, scale_y: 1.0 }
    }

    /// Declares the plane as subsampled relative to image coordinates.
    pub fn with_scale(mut self, scale_x: f32, scale_y: f32) -> Self {
        self.scale_x = scale_x;
        self.scale_y = scale_y;
        self
    }

    #[inline]
    fn idx(&self, x: usize, y: usize) -> usize {
        y * self.stride + x * self.step
    }

    /// Whether the plane pixel `(px, py)` is covered by `shape`; the pixel
    /// center is mapped back to image coordinates.
    #[inline]
    fn covered(&self, shape: &Shape, px: usize, py: usize) -> bool {
        shape.contains((px as f32 + 0.5) / self.scale_x, (py as f32 + 0.5) / self.scale_y)
    }

    /// Bounding box of `shape` in plane pixels, grown by `margin`, clipped.
    fn rect(&self, shape: &Shape, margin: usize) -> Option<Rect> {
        let (x0, y0, x1, y1) = shape.bounds();
        let m = margin as f32;
        let x0 = (x0 * self.scale_x - m).floor().max(0.0) as usize;
        let y0 = (y0 * self.scale_y - m).floor().max(0.0) as usize;
        let x1 = ((x1 * self.scale_x + m).ceil().max(0.0) as usize).min(self.width);
        let y1 = ((y1 * self.scale_y + m).ceil().max(0.0) as usize).min(self.height);
        (x1 > x0 && y1 > y0).then_some((x0, y0, x1, y1))
    }

    /// Fills the shape with a constant color (one value per channel).
    pub fn fill(&mut self, shape: &Shape, value: &[T]) {
        let Some((x0, y0, x1, y1)) = self.rect(shape, 0) else { return };
        let ch = self.channels.min(value.len());
        for y in y0..y1 {
            for x in x0..x1 {
                if self.covered(shape, x, y) {
                    let i = self.idx(x, y);
                    self.data[i..i + ch].copy_from_slice(&value[..ch]);
                }
            }
        }
    }

    /// Replaces the shape by a mosaic of flat blocks of `block` image pixels.
    ///
    /// The grid is anchored at the plane origin so that blocks of subsampled
    /// planes line up exactly with the luma blocks.
    pub fn pixelate(&mut self, shape: &Shape, block: f32) {
        let bx = ((block * self.scale_x).round() as usize).max(1);
        let by = ((block * self.scale_y).round() as usize).max(1);
        let Some((x0, y0, x1, y1)) = self.rect(shape, 0) else { return };
        let ch = self.channels;
        let mut by0 = y0 / by * by;
        while by0 < y1 {
            let by1 = (by0 + by).min(self.height);
            let mut bx0 = x0 / bx * bx;
            while bx0 < x1 {
                let bx1 = (bx0 + bx).min(self.width);
                let mut sum = [0f64; 4];
                for y in by0..by1 {
                    for x in bx0..bx1 {
                        let i = self.idx(x, y);
                        for (c, s) in sum.iter_mut().enumerate().take(ch) {
                            *s += self.data[i + c].to_f32() as f64;
                        }
                    }
                }
                let n = ((by1 - by0) * (bx1 - bx0)) as f64;
                let mean: [T; 4] = std::array::from_fn(|c| T::from_f32((sum[c] / n) as f32));
                for y in by0..by1 {
                    for x in bx0..bx1 {
                        if self.covered(shape, x, y) {
                            let i = self.idx(x, y);
                            self.data[i..i + ch].copy_from_slice(&mean[..ch]);
                        }
                    }
                }
                bx0 += bx;
            }
            by0 += by;
        }
    }

    /// Blurs the shape with three box-filter passes per axis (a close
    /// approximation of a Gaussian with σ ≈ `radius / 2`).
    pub fn blur(&mut self, shape: &Shape, radius: f32) {
        let r = ((radius * self.scale_x.max(self.scale_y) / 1.5).round() as usize).max(1);
        let Some((x0, y0, x1, y1)) = self.rect(shape, 3 * r) else { return };
        let (w, h, ch) = (x1 - x0, y1 - y0, self.channels);

        let mut buf = vec![0f32; w * h * ch];
        for y in 0..h {
            for x in 0..w {
                let i = self.idx(x0 + x, y0 + y);
                for c in 0..ch {
                    buf[(y * w + x) * ch + c] = self.data[i + c].to_f32();
                }
            }
        }
        for _ in 0..3 {
            box_rows(&mut buf, w, ch, r);
        }
        let mut t = transpose(&buf, w, h, ch);
        for _ in 0..3 {
            box_rows(&mut t, h, ch, r);
        }
        let buf = transpose(&t, h, w, ch);

        for y in 0..h {
            for x in 0..w {
                if self.covered(shape, x0 + x, y0 + y) {
                    let i = self.idx(x0 + x, y0 + y);
                    for c in 0..ch {
                        self.data[i + c] = T::from_f32(buf[(y * w + x) * ch + c]);
                    }
                }
            }
        }
    }
}

impl<T: Sample> Plane<'_, T> {
    /// Alpha-blends a text stamp: `outline` color under the dilated glyphs,
    /// `text` color on the glyphs (one value per channel).
    pub fn blend_stamp(&mut self, stamp: &crate::overlay::Stamp, text: &[T], outline: &[T]) {
        let (sx, sy) = (self.scale_x, self.scale_y);
        let px0 = (stamp.x as f32 * sx).floor() as usize;
        let py0 = (stamp.y as f32 * sy).floor() as usize;
        let px1 = (((stamp.x + stamp.width) as f32 * sx).ceil() as usize).min(self.width);
        let py1 = (((stamp.y + stamp.height) as f32 * sy).ceil() as usize).min(self.height);
        let ch = self.channels.min(text.len()).min(outline.len());
        for y in py0..py1 {
            for x in px0..px1 {
                let p = crate::detect::Point::new((x as f32 + 0.5) / sx, (y as f32 + 0.5) / sy);
                let (t, o) = stamp.coverage(p);
                if t == 0.0 && o == 0.0 {
                    continue;
                }
                let i = self.idx(x, y);
                for c in 0..ch {
                    let v = self.data[i + c].to_f32();
                    let v = v + (outline[c].to_f32() - v) * o;
                    let v = v + (text[c].to_f32() - v) * t;
                    self.data[i + c] = T::from_f32(v);
                }
            }
        }
    }
}

/// One box-filter pass of radius `r` along every row (edges clamped).
fn box_rows(buf: &mut [f32], w: usize, ch: usize, r: usize) {
    let win = (2 * r + 1) as f64;
    buf.par_chunks_mut(w * ch).for_each_init(
        || vec![0f64; w + 2 * r + 1],
        |prefix, row| {
            for c in 0..ch {
                // prefix[k] = sum of the first k samples of the edge-extended row.
                prefix[0] = 0.0;
                for k in 0..w + 2 * r {
                    let src = k.saturating_sub(r).min(w - 1);
                    prefix[k + 1] = prefix[k] + row[src * ch + c] as f64;
                }
                for x in 0..w {
                    row[x * ch + c] = ((prefix[x + 2 * r + 1] - prefix[x]) / win) as f32;
                }
            }
        },
    );
}

fn transpose(src: &[f32], w: usize, h: usize, ch: usize) -> Vec<f32> {
    let mut dst = vec![0f32; src.len()];
    for y in 0..h {
        for x in 0..w {
            let s = (y * w + x) * ch;
            let d = (x * h + y) * ch;
            dst[d..d + ch].copy_from_slice(&src[s..s + ch]);
        }
    }
    dst
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checker(w: usize, h: usize) -> Vec<u8> {
        (0..w * h * 3).map(|i| if ((i / 3) % w + (i / 3) / w).is_multiple_of(2) { 255 } else { 0 }).collect()
    }

    fn square(c: f32, half: f32) -> Shape {
        Shape::Rect { cx: c, cy: c, hw: half, hh: half, angle: 0.0 }
    }

    #[test]
    fn fill_touches_only_the_shape() {
        let mut d = vec![7u8; 10 * 10 * 4];
        let mut p = Plane::new(&mut d, 10, 10, 40, 4, 3);
        p.fill(&square(5.0, 2.0), &[1, 2, 3]);
        let px = |x: usize, y: usize| &d[(y * 10 + x) * 4..(y * 10 + x) * 4 + 4];
        assert_eq!(px(5, 5), &[1, 2, 3, 7], "alpha untouched");
        assert_eq!(px(0, 0), &[7, 7, 7, 7]);
        assert_eq!(px(8, 5), &[7, 7, 7, 7]);
    }

    #[test]
    fn pixelate_flattens_blocks() {
        let mut d = checker(16, 16);
        let orig = d.clone();
        Plane::new(&mut d, 16, 16, 48, 3, 3).pixelate(&square(8.0, 8.0), 4.0);
        // Each 4x4 block of a checkerboard averages to mid grey.
        assert!(d.iter().all(|&v| v == 128));
        assert_ne!(d, orig);
    }

    #[test]
    fn blur_smooths_and_respects_bounds() {
        let (w, h) = (40, 40);
        let mut d = checker(w, h);
        let orig = d.clone();
        Plane::new(&mut d, w, h, w * 3, 3, 3).blur(&square(20.0, 8.0), 6.0);
        let at = |v: &[u8], x: usize, y: usize| v[(y * w + x) * 3];
        assert!((at(&d, 20, 20) as i32 - 128).abs() < 20, "center is smoothed");
        assert_eq!(at(&d, 2, 2), at(&orig, 2, 2), "outside untouched");
    }

    #[test]
    fn subsampled_planes_follow_image_coordinates() {
        let mut chroma = vec![0u16; 8 * 8];
        Plane::new(&mut chroma, 8, 8, 8, 1, 1).with_scale(0.5, 0.5).fill(&square(8.0, 4.0), &[512]);
        // Image square [4,12) maps to chroma [2,6).
        assert_eq!(chroma[3 * 8 + 3], 512);
        assert_eq!(chroma[3 * 8 + 6], 0);
        assert_eq!(chroma[3 * 8 + 1], 0);
    }
}
