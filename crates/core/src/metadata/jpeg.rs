//! JPEG marker-segment parsing and lossless metadata filtering.
//!
//! A JPEG file is `SOI`, a list of marker segments (metadata in `APPn`/`COM`,
//! decoding tables in `DQT`/`DHT`/`SOFn`/`DRI`), one or more scans of
//! entropy-coded data, `EOI`, and optionally a trailer (multi-picture
//! secondary images, gain maps, vendor blobs). Metadata filtering only ever
//! drops or rewrites `APPn`/`COM` segments and the trailer; the entropy-coded
//! image data is copied untouched.

use super::{exif, human_size, irb, Changes, Finding, Plan};
use crate::{Error, Result};
use std::borrow::Cow;

pub const SOI: u8 = 0xD8;
pub const EOI: u8 = 0xD9;
pub const SOS: u8 = 0xDA;
pub const COM: u8 = 0xFE;
pub const APP0: u8 = 0xE0;
pub const APP15: u8 = 0xEF;

const EXIF_SIG: &[u8] = b"Exif\0";
const XMP_SIG: &[u8] = b"http://ns.adobe.com/xap/1.0/\0";
const XMP_EXT_SIG: &[u8] = b"http://ns.adobe.com/xmp/extension/\0";
const ICC_SIG: &[u8] = b"ICC_PROFILE\0";
const MPF_SIG: &[u8] = b"MPF\0";
const FPXR_SIG: &[u8] = b"FPXR\0";
const IRB_SIG: &[u8] = b"Photoshop 3.0\0";
const JFIF_SIG: &[u8] = b"JFIF\0";
const JFXX_SIG: &[u8] = b"JFXX\0";
const ADOBE_SIG: &[u8] = b"Adobe";

/// A marker segment with a length field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment<'a> {
    pub marker: u8,
    /// Bytes after the 2-byte length field.
    pub payload: Cow<'a, [u8]>,
}

/// Semantic classification of a segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Jfif,
    Jfxx,
    Exif,
    Xmp,
    XmpExtension,
    Icc,
    Mpf,
    FlashPix,
    Photoshop,
    Adobe,
    Comment,
    OtherApp,
    /// Tables, frame headers, everything that is not metadata.
    Structural,
}

impl Segment<'_> {
    pub fn kind(&self) -> Kind {
        let p = &self.payload[..];
        match self.marker {
            0xE0 if p.starts_with(JFIF_SIG) => Kind::Jfif,
            0xE0 if p.starts_with(JFXX_SIG) => Kind::Jfxx,
            0xE1 if p.starts_with(EXIF_SIG) => Kind::Exif,
            0xE1 if p.starts_with(XMP_SIG) => Kind::Xmp,
            0xE1 if p.starts_with(XMP_EXT_SIG) => Kind::XmpExtension,
            0xE2 if p.starts_with(ICC_SIG) => Kind::Icc,
            0xE2 if p.starts_with(MPF_SIG) => Kind::Mpf,
            0xE2 if p.starts_with(FPXR_SIG) => Kind::FlashPix,
            0xED if p.starts_with(IRB_SIG) => Kind::Photoshop,
            0xEE if p.starts_with(ADOBE_SIG) => Kind::Adobe,
            COM => Kind::Comment,
            APP0..=APP15 => Kind::OtherApp,
            _ => Kind::Structural,
        }
    }

    fn name(&self) -> String {
        let app = format!("APP{}", self.marker.wrapping_sub(APP0));
        match self.kind() {
            Kind::Jfif => "APP0 JFIF".into(),
            Kind::Jfxx => "APP0 JFXX thumbnail".into(),
            Kind::Exif => "APP1 EXIF".into(),
            Kind::Xmp => "APP1 XMP".into(),
            Kind::XmpExtension => "APP1 Extended XMP".into(),
            Kind::Icc => "APP2 ICC profile".into(),
            Kind::Mpf => "APP2 MPF (multi-picture index)".into(),
            Kind::FlashPix => "APP2 FlashPix".into(),
            Kind::Photoshop => "APP13 Photoshop/IPTC".into(),
            Kind::Adobe => "APP14 Adobe".into(),
            Kind::Comment => "COM comment".into(),
            Kind::OtherApp => {
                let tag: String = self
                    .payload
                    .iter()
                    .take_while(|&&b| b != 0 && b.is_ascii_graphic())
                    .take(16)
                    .map(|&b| b as char)
                    .collect();
                if tag.is_empty() {
                    app
                } else {
                    format!("{app} {tag}")
                }
            }
            Kind::Structural => format!("marker 0x{:02X}", self.marker),
        }
    }
}

/// A parsed JPEG file.
#[derive(Debug, Clone)]
pub struct Layout<'a> {
    /// Segments between `SOI` and the first `SOS`.
    pub segments: Vec<Segment<'a>>,
    /// From the first `SOS` marker through `EOI` (inclusive).
    pub scan: &'a [u8],
    /// Bytes after `EOI`.
    pub trailer: &'a [u8],
}

impl Layout<'_> {
    /// Whether the frame uses progressive coding (SOF2/6/10/14).
    pub fn is_progressive(&self) -> bool {
        self.segments.iter().any(|s| matches!(s.marker, 0xC2 | 0xC6 | 0xCA | 0xCE))
    }
}

fn be16(d: &[u8], pos: usize) -> Option<usize> {
    d.get(pos..pos + 2).map(|b| u16::from_be_bytes([b[0], b[1]]) as usize)
}

/// Splits a JPEG into header segments, scan data and trailer.
pub fn parse(data: &[u8]) -> Result<Layout<'_>> {
    let bad = |r: &str| Error::malformed("JPEG", r);
    if !data.starts_with(&[0xFF, SOI]) {
        return Err(bad("missing SOI marker"));
    }
    let mut pos = 2;
    let mut segments = Vec::new();
    loop {
        // Markers may be preceded by any number of 0xFF fill bytes.
        while data.get(pos) == Some(&0xFF) && data.get(pos + 1) == Some(&0xFF) {
            pos += 1;
        }
        let (Some(&0xFF), Some(&marker)) = (data.get(pos), data.get(pos + 1)) else {
            return Err(bad("expected a marker before the image data"));
        };
        if marker == SOS {
            break;
        }
        if marker == EOI || (0xD0..=0xD7).contains(&marker) || marker == 0x01 {
            // Standalone marker before any scan: skip it.
            pos += 2;
            continue;
        }
        let len = be16(data, pos + 2).ok_or_else(|| bad("truncated segment"))?;
        let end = pos + 2 + len;
        if len < 2 || end > data.len() {
            return Err(bad("segment length out of bounds"));
        }
        segments.push(Segment { marker, payload: Cow::Borrowed(&data[pos + 4..end]) });
        pos = end;
    }
    let scan_start = pos;
    let scan_end = find_eoi(data, scan_start).unwrap_or(data.len());
    Ok(Layout { segments, scan: &data[scan_start..scan_end], trailer: &data[scan_end..] })
}

/// Returns the offset just past the `EOI` that terminates the image whose
/// first `SOS` marker starts at `pos`.
fn find_eoi(d: &[u8], mut pos: usize) -> Option<usize> {
    loop {
        if d.get(pos) != Some(&0xFF) {
            return None;
        }
        let marker = *d.get(pos + 1)?;
        match marker {
            0xFF => pos += 1,
            EOI => return Some(pos + 2),
            0xD0..=0xD7 | 0x01 => pos += 2,
            _ => {
                let len = be16(d, pos + 2)?;
                pos += 2 + len;
                if marker == SOS {
                    // Skip entropy-coded data: 0xFF00 is a stuffed byte and
                    // RSTn markers live inside the scan.
                    loop {
                        let i = pos + d.get(pos..)?.iter().position(|&b| b == 0xFF)?;
                        match *d.get(i + 1)? {
                            0x00 | 0xD0..=0xD7 => pos = i + 2,
                            0xFF => pos = i + 1,
                            _ => {
                                pos = i;
                                break;
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Decides the fate of one segment. `None` drops it.
fn filter<'a>(seg: &Segment<'a>, plan: Plan, ch: &mut Changes) -> Option<Segment<'a>> {
    let size = human_size(seg.payload.len());
    let drop = |ch: &mut Changes, why: &str| {
        ch.removed(format!("{} ({size}){why}", seg.name()));
        None
    };
    match seg.kind() {
        Kind::Structural | Kind::Icc | Kind::Adobe => Some(seg.clone()),
        Kind::Jfif => {
            // JFIF may carry an uncompressed RGB thumbnail after its 14-byte header.
            let p = &seg.payload;
            if plan.strip_previews() && p.len() > 14 {
                let mut v = p[..14].to_vec();
                v[12] = 0;
                v[13] = 0;
                ch.removed("APP0 JFIF thumbnail");
                Some(Segment { marker: seg.marker, payload: Cow::Owned(v) })
            } else {
                Some(seg.clone())
            }
        }
        Kind::Exif if plan.strip_all() => drop(ch, ""),
        Kind::Exif => {
            let mut v = seg.payload.to_vec();
            let edit = exif::ExifEdit { sensitive: plan.strip_sensitive(), previews: plan.strip_previews() };
            let start = 6.min(v.len());
            match exif::edit(&mut v[start..], edit, ch) {
                Ok(()) => Some(Segment { marker: seg.marker, payload: Cow::Owned(v) }),
                Err(_) => drop(ch, ": unparseable, removed entirely"),
            }
        }
        Kind::Photoshop if plan.strip_sensitive() => drop(ch, ""),
        Kind::Photoshop if plan.strip_previews() => match irb::strip_thumbnails(&seg.payload[IRB_SIG.len()..]) {
            Some((rest, removed)) => {
                if removed {
                    ch.removed("APP13 Photoshop thumbnail");
                }
                let mut v = IRB_SIG.to_vec();
                v.extend(rest);
                Some(Segment { marker: seg.marker, payload: Cow::Owned(v) })
            }
            None => drop(ch, ": unparseable, removed entirely"),
        },
        Kind::Comment if plan.strip_sensitive() => drop(ch, ""),
        Kind::Xmp | Kind::XmpExtension | Kind::Mpf | Kind::FlashPix | Kind::Jfxx if plan.strip_previews() => {
            drop(ch, "")
        }
        Kind::OtherApp if plan.strip_unknown() => drop(ch, ""),
        _ => Some(seg.clone()),
    }
}

fn write_segment(out: &mut Vec<u8>, seg: &Segment<'_>) -> Result<()> {
    let len = seg.payload.len() + 2;
    if len > u16::MAX as usize {
        return Err(Error::malformed("JPEG", "segment too large"));
    }
    out.extend_from_slice(&[0xFF, seg.marker]);
    out.extend_from_slice(&(len as u16).to_be_bytes());
    out.extend_from_slice(&seg.payload);
    Ok(())
}

fn filter_trailer(trailer: &[u8], plan: Plan, ch: &mut Changes) -> bool {
    if trailer.is_empty() {
        return true;
    }
    if plan.strip_unknown() {
        ch.removed(format!("data after end of image ({}): secondary images / vendor data", human_size(trailer.len())));
        false
    } else {
        true
    }
}

/// Removes metadata from a JPEG without touching the compressed image data.
pub fn strip(data: &[u8], plan: Plan, ch: &mut Changes) -> Result<Vec<u8>> {
    let layout = parse(data)?;
    let mut out = Vec::with_capacity(data.len());
    out.extend_from_slice(&[0xFF, SOI]);
    for seg in &layout.segments {
        if let Some(s) = filter(seg, plan, ch) {
            write_segment(&mut out, &s)?;
        }
    }
    out.extend_from_slice(layout.scan);
    if filter_trailer(layout.trailer, plan, ch) {
        out.extend_from_slice(layout.trailer);
    }
    Ok(out)
}

/// Combines the (filtered) metadata of `original` with the frame headers and
/// scans of a freshly written `encoded` JPEG.
///
/// With `keep_color == false` (the pixels were converted to a different
/// color space) the original ICC profile and JFIF/Adobe markers are replaced
/// by the ones of `encoded`.
pub fn rebuild(
    original: &Layout<'_>,
    encoded: &Layout<'_>,
    plan: Plan,
    ch: &mut Changes,
    keep_color: bool,
) -> Result<Vec<u8>> {
    let color = |k: Kind| matches!(k, Kind::Icc | Kind::Adobe | Kind::Jfif);
    let mut out = Vec::with_capacity(original.scan.len() + 64 * 1024);
    out.extend_from_slice(&[0xFF, SOI]);
    if !keep_color {
        for seg in encoded.segments.iter().filter(|s| color(s.kind())) {
            write_segment(&mut out, seg)?;
        }
    }
    for seg in original.segments.iter().filter(|s| s.kind() != Kind::Structural && (keep_color || !color(s.kind()))) {
        if let Some(s) = filter(seg, plan, ch) {
            write_segment(&mut out, &s)?;
        }
    }
    for seg in encoded.segments.iter().filter(|s| s.kind() == Kind::Structural) {
        write_segment(&mut out, seg)?;
    }
    out.extend_from_slice(encoded.scan);
    if filter_trailer(original.trailer, plan, ch) {
        out.extend_from_slice(original.trailer);
    }
    Ok(out)
}

/// Lists the metadata blocks of a JPEG.
pub fn inspect(data: &[u8], out: &mut Vec<Finding>) -> Result<()> {
    let layout = parse(data)?;
    for seg in &layout.segments {
        let kind = seg.kind();
        if kind == Kind::Structural {
            continue;
        }
        let block = seg.name();
        let size = human_size(seg.payload.len());
        match kind {
            Kind::Exif => exif::inspect(&seg.payload[6.min(seg.payload.len())..], &block, out),
            Kind::Comment => out.push(Finding {
                block,
                detail: String::from_utf8_lossy(&seg.payload).chars().take(60).collect(),
                sensitive: true,
            }),
            Kind::Icc | Kind::Adobe | Kind::Jfif => {
                out.push(Finding { block, detail: format!("{size} (kept: needed for rendering)"), sensitive: false })
            }
            _ => out.push(Finding { block, detail: size, sensitive: true }),
        }
    }
    if !layout.trailer.is_empty() {
        out.push(Finding {
            block: "trailer".into(),
            detail: format!("{} after end of image", human_size(layout.trailer.len())),
            sensitive: true,
        });
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::options::MetadataPolicy;

    fn seg(marker: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = vec![0xFF, marker];
        v.extend_from_slice(&((payload.len() + 2) as u16).to_be_bytes());
        v.extend_from_slice(payload);
        v
    }

    /// A syntactically valid JPEG skeleton with every interesting segment.
    pub(crate) fn sample_jpeg() -> Vec<u8> {
        let mut v = vec![0xFF, SOI];
        v.extend(seg(0xE0, b"JFIF\0\x01\x02\0\0\x01\0\x01\0\0"));
        let mut exif_payload = b"Exif\0\0".to_vec();
        exif_payload.extend(exif::tests::sample_exif());
        v.extend(seg(0xE1, &exif_payload));
        v.extend(seg(0xE1, b"http://ns.adobe.com/xap/1.0/\0<x:xmpmeta>secret</x:xmpmeta>"));
        v.extend(seg(0xE2, b"ICC_PROFILE\0\x01\x01profile"));
        v.extend(seg(0xEC, b"Ducky\0data"));
        v.extend(seg(COM, b"shot by Jane"));
        v.extend(seg(0xDB, &[0u8; 65])); // DQT
        v.extend(seg(0xC0, &[8, 0, 1, 0, 1, 1, 1, 0x11, 0])); // SOF0
        v.extend(seg(SOS, &[1, 1, 0, 0, 63, 0]));
        v.extend_from_slice(&[0x12, 0xFF, 0x00, 0x34, 0xFF, 0xD0, 0x56]); // entropy data with stuffing + RST0
        v.extend_from_slice(&[0xFF, EOI]);
        v.extend_from_slice(b"\xFF\xD8MPF-SECONDARY-IMAGE");
        v
    }

    fn has(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn parses_layout() {
        let data = sample_jpeg();
        let l = parse(&data).unwrap();
        assert_eq!(l.segments.len(), 8);
        assert!(l.scan.starts_with(&[0xFF, SOS]));
        assert!(l.scan.ends_with(&[0xFF, EOI]));
        assert_eq!(l.trailer, b"\xFF\xD8MPF-SECONDARY-IMAGE");
        assert!(!l.is_progressive());
    }

    #[test]
    fn strip_all_keeps_only_rendering_data() {
        let data = sample_jpeg();
        let mut ch = Changes::default();
        let out = strip(&data, Plan::new(MetadataPolicy::All, false), &mut ch).unwrap();
        let l = parse(&out).unwrap();
        let kinds: Vec<Kind> = l.segments.iter().map(|s| s.kind()).collect();
        assert_eq!(kinds, vec![Kind::Jfif, Kind::Icc, Kind::Structural, Kind::Structural]);
        assert!(l.trailer.is_empty());
        assert_eq!(l.scan, parse(&data).unwrap().scan, "image data must be untouched");
        assert!(!has(&out, b"Jane"));
    }

    #[test]
    fn strip_sensitive_keeps_camera_info() {
        let data = sample_jpeg();
        let mut ch = Changes::default();
        let out = strip(&data, Plan::new(MetadataPolicy::Sensitive, false), &mut ch).unwrap();
        assert!(has(&out, b"Exif\0\0"));
        assert!(has(&out, b"Cam\0"), "camera make is kept");
        assert!(!has(&out, b"Jane Doe"));
        assert!(!has(&out, b"secret"));
        assert!(!has(&out, b"Ducky"));
        assert!(!has(&out, b"SECONDARY"));
        assert!(ch.removed.iter().any(|r| r.contains("GPS")));
    }

    #[test]
    fn keep_with_faces_removes_previews_only() {
        let data = sample_jpeg();
        let out = strip(&data, Plan::new(MetadataPolicy::Keep, true), &mut Changes::default()).unwrap();
        assert!(has(&out, b"Jane Doe"));
        assert!(has(&out, b"shot by Jane"));
        assert!(!has(&out, b"THUMBNAIL"));
        assert!(!has(&out, b"SECONDARY"));
        assert!(!has(&out, b"xmpmeta"));
    }

    #[test]
    fn inspect_lists_blocks() {
        let mut f = Vec::new();
        inspect(&sample_jpeg(), &mut f).unwrap();
        assert!(f.iter().any(|x| x.block == "APP1 XMP"));
        assert!(f.iter().any(|x| x.block == "APP12 Ducky"));
        assert!(f.iter().any(|x| x.block == "trailer"));
        assert!(f.iter().any(|x| x.detail.contains("GPS position")));
    }
}
