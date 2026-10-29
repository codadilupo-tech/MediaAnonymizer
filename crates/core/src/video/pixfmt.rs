//! Raw frame layouts exchanged with FFmpeg.
//!
//! Frames are processed in the *source's own* pixel format whenever we can
//! describe it, so that untouched pixels go through the pipeline bit-exactly
//! (no YUV↔RGB round trip). Formats that only differ in memory layout are
//! mapped to an equivalent planar format (e.g. NV12 → yuv420p), which is a
//! lossless reshuffle.

/// Color family of a raw format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// Y, Cb, Cr planes (optionally followed by alpha).
    Yuv,
    /// G, B, R planes (FFmpeg's planar RGB order), optionally alpha.
    Gbr,
    Gray,
}

/// A planar pixel format understood by the pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PixFmt {
    /// FFmpeg name passed to `-pix_fmt`.
    pub name: &'static str,
    pub family: Family,
    pub bits: u32,
    /// Chroma subsampling as log2 shifts (x, y).
    pub chroma_shift: (u32, u32),
    pub alpha: bool,
    /// `yuvj*` formats are full range by definition.
    pub full_range: bool,
}

macro_rules! fmt {
    ($name:literal, $fam:ident, $bits:literal, $sx:literal, $sy:literal, $alpha:literal, $full:literal) => {
        PixFmt {
            name: $name,
            family: Family::$fam,
            bits: $bits,
            chroma_shift: ($sx, $sy),
            alpha: $alpha,
            full_range: $full,
        }
    };
}

const FORMATS: &[PixFmt] = &[
    fmt!("yuv420p", Yuv, 8, 1, 1, false, false),
    fmt!("yuvj420p", Yuv, 8, 1, 1, false, true),
    fmt!("yuv422p", Yuv, 8, 1, 0, false, false),
    fmt!("yuvj422p", Yuv, 8, 1, 0, false, true),
    fmt!("yuv444p", Yuv, 8, 0, 0, false, false),
    fmt!("yuvj444p", Yuv, 8, 0, 0, false, true),
    fmt!("yuv440p", Yuv, 8, 0, 1, false, false),
    fmt!("yuv411p", Yuv, 8, 2, 0, false, false),
    fmt!("yuv410p", Yuv, 8, 2, 2, false, false),
    fmt!("yuv420p10le", Yuv, 10, 1, 1, false, false),
    fmt!("yuv422p10le", Yuv, 10, 1, 0, false, false),
    fmt!("yuv444p10le", Yuv, 10, 0, 0, false, false),
    fmt!("yuv420p12le", Yuv, 12, 1, 1, false, false),
    fmt!("yuv422p12le", Yuv, 12, 1, 0, false, false),
    fmt!("yuv444p12le", Yuv, 12, 0, 0, false, false),
    fmt!("yuv420p16le", Yuv, 16, 1, 1, false, false),
    fmt!("yuv422p16le", Yuv, 16, 1, 0, false, false),
    fmt!("yuv444p16le", Yuv, 16, 0, 0, false, false),
    fmt!("yuva420p", Yuv, 8, 1, 1, true, false),
    fmt!("yuva422p", Yuv, 8, 1, 0, true, false),
    fmt!("yuva444p", Yuv, 8, 0, 0, true, false),
    fmt!("yuva420p10le", Yuv, 10, 1, 1, true, false),
    fmt!("yuva422p10le", Yuv, 10, 1, 0, true, false),
    fmt!("yuva444p10le", Yuv, 10, 0, 0, true, false),
    fmt!("yuva444p12le", Yuv, 12, 0, 0, true, false),
    fmt!("yuva444p16le", Yuv, 16, 0, 0, true, false),
    fmt!("gbrp", Gbr, 8, 0, 0, false, true),
    fmt!("gbrp10le", Gbr, 10, 0, 0, false, true),
    fmt!("gbrp12le", Gbr, 12, 0, 0, false, true),
    fmt!("gbrp16le", Gbr, 16, 0, 0, false, true),
    fmt!("gbrap", Gbr, 8, 0, 0, true, true),
    fmt!("gbrap10le", Gbr, 10, 0, 0, true, true),
    fmt!("gbrap12le", Gbr, 12, 0, 0, true, true),
    fmt!("gbrap16le", Gbr, 16, 0, 0, true, true),
    fmt!("gray", Gray, 8, 0, 0, false, true),
    fmt!("gray10le", Gray, 10, 0, 0, false, true),
    fmt!("gray12le", Gray, 12, 0, 0, false, true),
    fmt!("gray16le", Gray, 16, 0, 0, false, true),
];

/// Layout-only aliases: same samples, different memory arrangement.
const ALIASES: &[(&str, &str)] = &[
    ("nv12", "yuv420p"),
    ("nv21", "yuv420p"),
    ("nv16", "yuv422p"),
    ("nv24", "yuv444p"),
    ("yuyv422", "yuv422p"),
    ("uyvy422", "yuv422p"),
    ("p010le", "yuv420p10le"),
    ("p010be", "yuv420p10le"),
    ("p016le", "yuv420p16le"),
    ("p210le", "yuv422p10le"),
    ("p410le", "yuv444p10le"),
    ("y210le", "yuv422p10le"),
    ("yuv420p10be", "yuv420p10le"),
    ("yuv422p10be", "yuv422p10le"),
    ("yuv444p10be", "yuv444p10le"),
    ("rgb24", "gbrp"),
    ("bgr24", "gbrp"),
    ("rgba", "gbrap"),
    ("bgra", "gbrap"),
    ("argb", "gbrap"),
    ("abgr", "gbrap"),
    ("rgb0", "gbrp"),
    ("bgr0", "gbrp"),
    ("0rgb", "gbrp"),
    ("0bgr", "gbrp"),
    ("rgb48le", "gbrp16le"),
    ("rgba64le", "gbrap16le"),
    ("gbrp9le", "gbrp10le"),
    ("yuv420p9le", "yuv420p10le"),
    ("yuv444p9le", "yuv444p10le"),
    ("pal8", "gbrap"),
    ("gray8", "gray"),
    ("ya8", "yuva444p"),
];

impl PixFmt {
    /// Chooses the processing format for a source pixel format. The second
    /// value is `false` when the conversion is not a pure re-arrangement.
    pub fn for_source(src: &str) -> (PixFmt, bool) {
        if let Some(f) = FORMATS.iter().find(|f| f.name == src) {
            return (*f, true);
        }
        if let Some((_, to)) = ALIASES.iter().find(|(from, _)| *from == src) {
            return (*FORMATS.iter().find(|f| f.name == *to).expect("alias target exists"), true);
        }
        // Unknown layout: process in a high-precision 4:4:4 format.
        let deep = src.contains("10") || src.contains("12") || src.contains("16");
        let name = if deep { "yuv444p16le" } else { "yuv444p" };
        (*FORMATS.iter().find(|f| f.name == name).unwrap(), false)
    }

    pub fn bytes_per_sample(&self) -> usize {
        if self.bits > 8 {
            2
        } else {
            1
        }
    }

    pub fn plane_count(&self) -> usize {
        match self.family {
            Family::Gray => 1,
            _ => 3 + self.alpha as usize,
        }
    }

    /// Dimensions of plane `i` for a `w`×`h` frame.
    pub fn plane_dims(&self, i: usize, w: u32, h: u32) -> (u32, u32) {
        let chroma = self.family == Family::Yuv && (i == 1 || i == 2);
        if chroma {
            // FFmpeg rounds subsampled dimensions up (AV_CEIL_RSHIFT).
            let (sx, sy) = self.chroma_shift;
            (w.div_ceil(1 << sx), h.div_ceil(1 << sy))
        } else {
            (w, h)
        }
    }

    /// Size in bytes of one tightly packed frame (`-f rawvideo`).
    pub fn frame_size(&self, w: u32, h: u32) -> usize {
        (0..self.plane_count())
            .map(|i| {
                let (pw, ph) = self.plane_dims(i, w, h);
                pw as usize * ph as usize * self.bytes_per_sample()
            })
            .sum()
    }

    /// White in plane `i` (timestamp text).
    pub fn white(&self, i: usize, full_range: bool) -> u16 {
        let shift = self.bits - 8;
        let max = ((1u32 << self.bits) - 1) as u16;
        match (self.family, i) {
            (Family::Yuv, 0) if !full_range => 235 << shift,
            (Family::Yuv, 1 | 2) => 128 << shift,
            _ => max,
        }
    }

    /// Value painted by the black-band style in plane `i`.
    pub fn black(&self, i: usize, full_range: bool) -> u16 {
        let shift = self.bits - 8;
        match (self.family, i) {
            (Family::Yuv, 0) if !full_range => 16 << shift,
            (Family::Yuv, 1 | 2) => 128 << shift,
            _ => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts() {
        let (f, exact) = PixFmt::for_source("nv12");
        assert!(exact);
        assert_eq!(f.name, "yuv420p");
        assert_eq!(f.frame_size(1920, 1080), 1920 * 1080 * 3 / 2);
        assert_eq!(f.plane_dims(1, 1919, 1079), (960, 540));
        let (f, _) = PixFmt::for_source("yuv422p10le");
        assert_eq!(f.frame_size(4, 2), (8 + 4 + 4) * 2);
        assert_eq!(f.black(0, false), 64);
        assert_eq!(f.black(1, false), 512);
        assert_eq!(f.white(0, false), 940);
        assert_eq!(f.white(0, true), 1023);
        let (f, exact) = PixFmt::for_source("weird_fmt");
        assert!(!exact);
        assert_eq!(f.plane_count(), 3);
    }
}
