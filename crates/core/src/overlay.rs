//! Capture-date overlay ("timestamp").
//!
//! The text is rasterised once per file into an anti-aliased coverage mask
//! (glyphs plus a dark outline for legibility on any background) and then
//! blended onto the pixels with the same [`Plane`](crate::redact::Plane)
//! machinery used for faces, so it works on RGB photos and native-format
//! video planes alike.

use crate::detect::Point;
use crate::options::{Corner, TimestampOptions};
use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use std::time::SystemTime;

/// Hack Regular (MIT / Bitstream Vera license, see `assets/LICENSE-Hack.txt`).
static FONT: &[u8] = include_bytes!("../assets/Hack-Regular.ttf");

/// Smallest text height in pixels, so that the date stays readable on small images.
const MIN_TEXT_PX: f32 = 12.0;

/// A calendar date and time, as recorded by the device (no time zone).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DateTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl DateTime {
    /// Parses the common textual forms: EXIF `2024:05:01 12:34:56`, ISO 8601
    /// `2024-05-01T12:34:56.000000Z` / `2024-05-01T12:34:56+0200` (the local
    /// time as written is kept, the offset ignored).
    pub fn parse(s: &str) -> Option<Self> {
        let mut nums = s.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty());
        let mut next = || nums.next()?.parse::<u32>().ok();
        let (year, month, day) = (next()?, next()?, next()?);
        let (hour, minute, second) = (next().unwrap_or(0), next().unwrap_or(0), next().unwrap_or(0));
        let valid = (1900..=9999).contains(&year)
            && (1..=12).contains(&month)
            && (1..=31).contains(&day)
            && hour < 24
            && minute < 60
            && second < 61;
        valid.then_some(DateTime {
            year: year as u16,
            month: month as u8,
            day: day as u8,
            hour: hour as u8,
            minute: minute as u8,
            second: second.min(59) as u8,
        })
    }

    /// Converts a system time to a UTC calendar date.
    pub fn from_system(t: SystemTime) -> Option<Self> {
        let secs = t.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_secs() as i64;
        let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
        // Howard Hinnant's civil_from_days.
        let z = days + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
        let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
        let year = (yoe + era * 400 + i64::from(month <= 2)) as u16;
        Some(DateTime {
            year,
            month,
            day,
            hour: (rem / 3600) as u8,
            minute: (rem / 60 % 60) as u8,
            second: (rem % 60) as u8,
        })
    }

    /// Formats with `%Y %y %m %b %d %H %M %S %%`; other characters are copied.
    pub fn format(&self, pattern: &str) -> String {
        const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
        let mut out = String::with_capacity(pattern.len() + 8);
        let mut chars = pattern.chars();
        while let Some(c) = chars.next() {
            if c != '%' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('Y') => out.push_str(&format!("{:04}", self.year)),
                Some('y') => out.push_str(&format!("{:02}", self.year % 100)),
                Some('m') => out.push_str(&format!("{:02}", self.month)),
                Some('b') => out.push_str(MONTHS[(self.month as usize).clamp(1, 12) - 1]),
                Some('d') => out.push_str(&format!("{:02}", self.day)),
                Some('H') => out.push_str(&format!("{:02}", self.hour)),
                Some('M') => out.push_str(&format!("{:02}", self.minute)),
                Some('S') => out.push_str(&format!("{:02}", self.second)),
                Some('%') => out.push('%'),
                Some(other) => {
                    out.push('%');
                    out.push(other);
                }
                None => out.push('%'),
            }
        }
        out
    }
}

/// Where a capture date was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateSource {
    Metadata(&'static str),
    FileModified,
}

impl std::fmt::Display for DateSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DateSource::Metadata(what) => f.write_str(what),
            DateSource::FileModified => f.write_str("file modification time (UTC)"),
        }
    }
}

/// A rendered text mask placed in image coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct Stamp {
    /// Top-left corner in image pixels.
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    /// Glyph coverage, `width * height`, 0–255.
    text: Vec<u8>,
    /// Outline coverage (glyphs dilated), 0–255.
    outline: Vec<u8>,
}

impl Stamp {
    /// Renders `text` for a `img_w`×`img_h` picture at the requested corner.
    pub fn render(text: &str, img_w: u32, img_h: u32, opts: &TimestampOptions) -> Option<Stamp> {
        let font = FontRef::try_from_slice(FONT).ok()?;
        let mut px = (img_h as f32 * opts.size).max(MIN_TEXT_PX);
        let measure = |px: f32| {
            let sf = font.as_scaled(PxScale::from(px));
            let mut w = 0.0;
            let mut prev = None;
            for c in text.chars() {
                let id = sf.glyph_id(c);
                if let Some(p) = prev {
                    w += sf.kern(p, id);
                }
                w += sf.h_advance(id);
                prev = Some(id);
            }
            w
        };
        // Shrink the text if it does not fit in 90% of the width.
        let w = measure(px);
        if w > img_w as f32 * 0.9 {
            px *= img_w as f32 * 0.9 / w;
        }
        if px < 4.0 {
            return None;
        }
        let sf = font.as_scaled(PxScale::from(px));
        let pad = (px / 10.0).ceil().max(1.0) as i32; // outline thickness
        let text_w = measure(px).ceil() as i32 + 2 * pad;
        let text_h = (sf.ascent() - sf.descent()).ceil() as i32 + 2 * pad;
        let (w, h) = (text_w.max(1) as usize, text_h.max(1) as usize);

        let mut glyphs = vec![0u8; w * h];
        let mut caret = pad as f32;
        let mut prev = None;
        for c in text.chars() {
            let id = sf.glyph_id(c);
            if let Some(p) = prev {
                caret += sf.kern(p, id);
            }
            let glyph = id.with_scale_and_position(sf.scale(), ab_glyph::point(caret, pad as f32 + sf.ascent()));
            caret += sf.h_advance(id);
            prev = Some(id);
            if let Some(outlined) = font.outline_glyph(glyph) {
                let b = outlined.px_bounds();
                outlined.draw(|gx, gy, cov| {
                    let (x, y) = (b.min.x as i32 + gx as i32, b.min.y as i32 + gy as i32);
                    if (0..w as i32).contains(&x) && (0..h as i32).contains(&y) {
                        let i = y as usize * w + x as usize;
                        glyphs[i] = glyphs[i].max((cov.clamp(0.0, 1.0) * 255.0) as u8);
                    }
                });
            }
        }
        let outline = dilate(&glyphs, w, h, pad as usize);

        let margin = (px * 0.6) as u32;
        let (sw, sh) = (w as u32, h as u32);
        let x = match opts.position {
            Corner::BottomLeft => margin,
            Corner::BottomRight => img_w.saturating_sub(margin + sw),
        };
        let y = img_h.saturating_sub(margin + sh);
        Some(Stamp { x, y, width: sw, height: sh, text: glyphs, outline })
    }

    /// Re-projects a stamp rendered on the *displayed* image onto the
    /// *stored* pixel grid, given a pixel mapping display → stored.
    pub fn reproject(&self, map: impl Fn(u32, u32) -> (u32, u32)) -> Stamp {
        let corners = [
            map(self.x, self.y),
            map(self.x + self.width - 1, self.y),
            map(self.x, self.y + self.height - 1),
            map(self.x + self.width - 1, self.y + self.height - 1),
        ];
        let x0 = corners.iter().map(|c| c.0).min().unwrap();
        let y0 = corners.iter().map(|c| c.1).min().unwrap();
        let x1 = corners.iter().map(|c| c.0).max().unwrap();
        let y1 = corners.iter().map(|c| c.1).max().unwrap();
        let (w, h) = ((x1 - x0 + 1) as usize, (y1 - y0 + 1) as usize);
        let (mut text, mut outline) = (vec![0u8; w * h], vec![0u8; w * h]);
        for sy in 0..self.height {
            for sx in 0..self.width {
                let (dx, dy) = map(self.x + sx, self.y + sy);
                let (src, dst) = ((sy * self.width + sx) as usize, (dy - y0) as usize * w + (dx - x0) as usize);
                text[dst] = self.text[src];
                outline[dst] = self.outline[src];
            }
        }
        Stamp { x: x0, y: y0, width: w as u32, height: h as u32, text, outline }
    }

    /// Bounding box `(x0, y0, x1, y1)` (exclusive) clipped to the image.
    pub fn bounds(&self, img_w: u32, img_h: u32) -> Option<(u32, u32, u32, u32)> {
        let (x1, y1) = ((self.x + self.width).min(img_w), (self.y + self.height).min(img_h));
        (x1 > self.x && y1 > self.y).then_some((self.x, self.y, x1, y1))
    }

    /// `(text, outline)` coverage in `0..=1` at an image point.
    #[inline]
    pub fn coverage(&self, p: Point) -> (f32, f32) {
        let (x, y) = (p.x.floor() as i64 - self.x as i64, p.y.floor() as i64 - self.y as i64);
        if x < 0 || y < 0 || x >= self.width as i64 || y >= self.height as i64 {
            return (0.0, 0.0);
        }
        let i = y as usize * self.width as usize + x as usize;
        (self.text[i] as f32 / 255.0, self.outline[i] as f32 / 255.0)
    }

    /// Whether the stamp changes the pixel at `(x, y)`.
    #[inline]
    pub fn covers(&self, x: u32, y: u32) -> bool {
        let (t, o) = self.coverage(Point::new(x as f32 + 0.5, y as f32 + 0.5));
        t > 0.0 || o > 0.0
    }
}

/// Max filter with a disc of radius `r` (separable square approximation is
/// visibly boxy for outlines, so a disc is used; masks are small).
fn dilate(src: &[u8], w: usize, h: usize, r: usize) -> Vec<u8> {
    let r = r as i32;
    let offsets: Vec<(i32, i32)> = (-r..=r)
        .flat_map(|dy| (-r..=r).map(move |dx| (dx, dy)))
        .filter(|(dx, dy)| dx * dx + dy * dy <= r * r)
        .collect();
    let mut out = vec![0u8; src.len()];
    for y in 0..h as i32 {
        for x in 0..w as i32 {
            let mut m = 0u8;
            for &(dx, dy) in &offsets {
                let (sx, sy) = (x + dx, y + dy);
                if sx >= 0 && sy >= 0 && sx < w as i32 && sy < h as i32 {
                    m = m.max(src[sy as usize * w + sx as usize]);
                    if m == 255 {
                        break;
                    }
                }
            }
            out[y as usize * w + x as usize] = m;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_forms() {
        let exif = DateTime::parse("2024:05:01 12:34:56").unwrap();
        assert_eq!(exif, DateTime { year: 2024, month: 5, day: 1, hour: 12, minute: 34, second: 56 });
        assert_eq!(DateTime::parse("2024-05-01T12:34:56.000000Z"), Some(exif));
        assert_eq!(DateTime::parse("2024-05-01T12:34:56+0200"), Some(exif));
        assert!(DateTime::parse("0000:00:00 00:00:00").is_none());
        assert!(DateTime::parse("garbage").is_none());
    }

    #[test]
    fn converts_system_time() {
        let t = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_709_210_096); // 2024-02-29 12:34:56 UTC
        let d = DateTime::from_system(t).unwrap();
        assert_eq!(d.format("%Y-%m-%d %H:%M:%S"), "2024-02-29 12:34:56");
        assert_eq!(d.format("%d %b %y, 100%%"), "29 Feb 24, 100%");
    }

    #[test]
    fn stamp_sits_in_the_requested_corner() {
        let mut o = TimestampOptions::default();
        let right = Stamp::render("2024-05-01 12:34:56", 1000, 800, &o).unwrap();
        o.position = Corner::BottomLeft;
        let left = Stamp::render("2024-05-01 12:34:56", 1000, 800, &o).unwrap();
        assert!(left.x < 100 && right.x > 500);
        assert_eq!(left.y, right.y);
        assert!(left.y + left.height <= 800 && left.y > 650);
        assert!(right.x + right.width <= 1000);
        assert!(left.text.contains(&255), "glyphs are rasterised");
        assert!(left.outline.iter().zip(&left.text).all(|(o, t)| o >= t), "outline surrounds glyphs");
    }

    #[test]
    fn long_text_is_shrunk_to_fit() {
        let s = Stamp::render("a very long timestamp text that cannot fit", 200, 2000, &Default::default()).unwrap();
        assert!(s.width <= 200);
    }

    #[test]
    fn reprojection_rotates_the_mask() {
        let s = Stamp::render("12", 100, 100, &Default::default()).unwrap();
        // 90° rotation of a 100x100 image: display (x, y) -> stored (y, 99 - x).
        let r = s.reproject(|x, y| (y, 99 - x));
        assert_eq!((r.width, r.height), (s.height, s.width));
        assert_eq!(r.text.iter().map(|&v| v as u32).sum::<u32>(), s.text.iter().map(|&v| v as u32).sum::<u32>());
    }
}
