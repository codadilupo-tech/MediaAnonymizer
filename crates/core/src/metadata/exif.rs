//! In-place editing of EXIF (TIFF-structured) metadata.
//!
//! EXIF is a TIFF directory tree: IFD0 (main image) → Exif IFD → Interop IFD,
//! IFD0 → GPS IFD, and IFD0 → IFD1 (thumbnail). Many values, including the
//! proprietary MakerNote, refer to other data through absolute offsets, so
//! relocating anything risks corrupting tags we want to keep.
//!
//! Instead of rebuilding the tree we edit it **in place**: removed entries are
//! taken out of their directory table (the remaining entries are compacted and
//! the entry count updated) and every byte that belonged to them is zeroed.
//! The block keeps its size and every offset stays valid.

use super::{Changes, Finding};
use crate::{Error, Result};
use std::collections::HashSet;
use std::ops::Range;

/// What to remove from an EXIF block.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExifEdit {
    /// Remove GPS, identity fields, serial numbers and the MakerNote.
    pub sensitive: bool,
    /// Remove the IFD1 thumbnail and the MakerNote (which can embed previews).
    pub previews: bool,
}

const TAG_EXIF_IFD: u16 = 0x8769;
const TAG_GPS_IFD: u16 = 0x8825;
const TAG_INTEROP_IFD: u16 = 0xA005;
const TAG_MAKER_NOTE: u16 = 0x927C;

/// Tags that identify a person or a device, or embed free-form text or
/// foreign metadata blocks. Location lives in the GPS IFD, handled apart.
const SENSITIVE_TAGS: &[(u16, &str)] = &[
    (0x010E, "ImageDescription"),
    (0x013B, "Artist"),
    (0x013C, "HostComputer"),
    (0x02BC, "XMP packet"),
    (0x8298, "Copyright"),
    (0x83BB, "IPTC block"),
    (0x8649, "Photoshop resources"),
    (0x9286, "UserComment"),
    (0x9C9B, "XPTitle"),
    (0x9C9C, "XPComment"),
    (0x9C9D, "XPAuthor"),
    (0x9C9E, "XPKeywords"),
    (0x9C9F, "XPSubject"),
    (0xA420, "ImageUniqueID"),
    (0xA430, "CameraOwnerName"),
    (0xA431, "BodySerialNumber"),
    (0xA435, "LensSerialNumber"),
    (0xA437, "Photographer"),
    (0xA438, "ImageEditor"),
    (0xC62F, "CameraSerialNumber"),
    (TAG_MAKER_NOTE, "MakerNote"),
];

/// Informational tags shown by `inspect` (kept by the `sensitive` policy).
const INFO_TAGS: &[(u16, &str)] =
    &[(0x010F, "Make"), (0x0110, "Model"), (0x0131, "Software"), (0x9003, "DateTimeOriginal"), (0xA434, "LensModel")];

fn sensitive_name(tag: u16) -> Option<&'static str> {
    SENSITIVE_TAGS.iter().find(|(t, _)| *t == tag).map(|(_, n)| *n)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IfdKind {
    Ifd0,
    Ifd1,
    Exif,
    Gps,
    Interop,
}

/// Byte-order aware accessors.
#[derive(Debug, Clone, Copy)]
struct Order {
    le: bool,
}

impl Order {
    fn detect(d: &[u8]) -> Result<Self> {
        match d.get(..4) {
            Some([b'I', b'I', 0x2A, 0x00]) => Ok(Order { le: true }),
            Some([b'M', b'M', 0x00, 0x2A]) => Ok(Order { le: false }),
            _ => Err(Error::malformed("EXIF", "missing TIFF header")),
        }
    }

    fn u16(self, d: &[u8], off: usize) -> Option<u16> {
        let b: [u8; 2] = d.get(off..off.checked_add(2)?)?.try_into().ok()?;
        Some(if self.le { u16::from_le_bytes(b) } else { u16::from_be_bytes(b) })
    }

    fn u32(self, d: &[u8], off: usize) -> Option<u32> {
        let b: [u8; 4] = d.get(off..off.checked_add(4)?)?.try_into().ok()?;
        Some(if self.le { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) })
    }

    fn put_u16(self, d: &mut [u8], off: usize, v: u16) {
        d[off..off + 2].copy_from_slice(&if self.le { v.to_le_bytes() } else { v.to_be_bytes() });
    }

    fn put_u32(self, d: &mut [u8], off: usize, v: u32) {
        d[off..off + 4].copy_from_slice(&if self.le { v.to_le_bytes() } else { v.to_be_bytes() });
    }
}

/// One 12-byte directory entry.
#[derive(Debug, Clone, Copy)]
struct Entry {
    tag: u16,
    typ: u16,
    count: u32,
    /// Absolute offset of the entry itself.
    pos: usize,
}

impl Entry {
    /// Offset of the 4-byte value/offset field.
    fn value_pos(&self) -> usize {
        self.pos + 8
    }

    fn data_len(&self) -> Option<usize> {
        let unit = match self.typ {
            1 | 2 | 6 | 7 => 1,
            3 | 8 => 2,
            4 | 9 | 11 | 13 => 4,
            5 | 10 | 12 | 16 | 17 | 18 => 8,
            _ => return None,
        };
        (self.count as usize).checked_mul(unit)
    }

    /// Byte range holding the value, inline or out-of-line.
    fn data_range(&self, d: &[u8], o: Order) -> Option<Range<usize>> {
        let len = self.data_len()?;
        let start = if len <= 4 { self.value_pos() } else { o.u32(d, self.value_pos())? as usize };
        let end = start.checked_add(len)?;
        (end <= d.len()).then_some(start..end)
    }

    /// Value interpreted as an offset (sub-IFD pointers, thumbnail offset).
    fn offset(&self, d: &[u8], o: Order) -> Option<usize> {
        match self.typ {
            3 => o.u16(d, self.value_pos()).map(usize::from),
            _ => o.u32(d, self.value_pos()).map(|v| v as usize),
        }
    }
}

fn read_entries(d: &[u8], o: Order, ifd: usize) -> Result<Vec<Entry>> {
    let bad = || Error::malformed("EXIF", format!("directory at offset {ifd} is out of bounds"));
    let n = o.u16(d, ifd).ok_or_else(bad)? as usize;
    let end = ifd + 2 + n * 12 + 4;
    if end > d.len() {
        return Err(bad());
    }
    Ok((0..n)
        .map(|i| {
            let pos = ifd + 2 + i * 12;
            Entry {
                tag: o.u16(d, pos).unwrap_or(0),
                typ: o.u16(d, pos + 2).unwrap_or(0),
                count: o.u32(d, pos + 4).unwrap_or(0),
                pos,
            }
        })
        .collect())
}

fn zero(d: &mut [u8], r: Range<usize>) {
    if let Some(s) = d.get_mut(r) {
        s.fill(0);
    }
}

struct Editor<'a> {
    d: &'a mut [u8],
    o: Order,
    edit: ExifEdit,
    changes: &'a mut Changes,
    visited: HashSet<usize>,
}

impl Editor<'_> {
    fn enter(&mut self, ifd: usize) -> bool {
        ifd != 0 && ifd < self.d.len() && self.visited.insert(ifd)
    }

    /// Edits one directory; returns the absolute position of its (possibly
    /// moved) next-IFD pointer.
    fn edit_ifd(&mut self, ifd: usize, kind: IfdKind) -> Result<usize> {
        let entries = read_entries(self.d, self.o, ifd)?;
        let mut keep = Vec::with_capacity(entries.len());
        for e in &entries {
            let drop = match e.tag {
                TAG_EXIF_IFD | TAG_INTEROP_IFD if matches!(kind, IfdKind::Ifd0 | IfdKind::Ifd1 | IfdKind::Exif) => {
                    if let Some(sub) = e.offset(self.d, self.o).filter(|&s| self.enter(s)) {
                        let sub_kind = if e.tag == TAG_EXIF_IFD { IfdKind::Exif } else { IfdKind::Interop };
                        self.edit_ifd(sub, sub_kind)?;
                    }
                    false
                }
                TAG_GPS_IFD if self.edit.sensitive => {
                    if let Some(sub) = e.offset(self.d, self.o).filter(|&s| self.enter(s)) {
                        let n = self.wipe_ifd(sub);
                        self.changes.removed(format!("EXIF GPS location ({n} tags)"));
                    }
                    true
                }
                TAG_MAKER_NOTE if self.edit.sensitive || self.edit.previews => {
                    self.changes.removed("EXIF MakerNote (vendor data: serial numbers, previews)");
                    true
                }
                tag if self.edit.sensitive && kind != IfdKind::Gps => match sensitive_name(tag) {
                    Some(name) => {
                        self.changes.removed(format!("EXIF {name}"));
                        true
                    }
                    None => false,
                },
                _ => false,
            };
            if drop {
                if let Some(r) = e.data_range(self.d, self.o) {
                    zero(self.d, r);
                }
            } else {
                keep.push(*e);
            }
        }
        Ok(self.rewrite_table(ifd, &entries, &keep))
    }

    /// Compacts `keep` at the start of the table and zeroes the freed slots.
    fn rewrite_table(&mut self, ifd: usize, all: &[Entry], keep: &[Entry]) -> usize {
        let next_old = ifd + 2 + all.len() * 12;
        let next_new = ifd + 2 + keep.len() * 12;
        if keep.len() == all.len() {
            return next_old;
        }
        let next_val = self.o.u32(self.d, next_old).unwrap_or(0);
        let rows: Vec<[u8; 12]> = keep.iter().map(|e| self.d[e.pos..e.pos + 12].try_into().unwrap()).collect();
        for (i, row) in rows.iter().enumerate() {
            let p = ifd + 2 + i * 12;
            self.d[p..p + 12].copy_from_slice(row);
        }
        self.o.put_u16(self.d, ifd, keep.len() as u16);
        zero(self.d, next_new..next_old + 4);
        self.o.put_u32(self.d, next_new, next_val);
        next_new
    }

    /// Zeroes a whole directory and its out-of-line values. Returns the
    /// number of entries it contained.
    fn wipe_ifd(&mut self, ifd: usize) -> usize {
        let Ok(entries) = read_entries(self.d, self.o, ifd) else { return 0 };
        for e in &entries {
            if let Some(r) = e.data_range(self.d, self.o) {
                zero(self.d, r);
            }
        }
        zero(self.d, ifd..ifd + 2 + entries.len() * 12 + 4);
        entries.len()
    }

    /// Zeroes the thumbnail directory together with the image data it
    /// points to.
    fn wipe_thumbnail(&mut self, ifd: usize) {
        if let Ok(entries) = read_entries(self.d, self.o, ifd) {
            let (d, o) = (&*self.d, self.o);
            let find = |tag| entries.iter().find(|e| e.tag == tag).and_then(|e| e.offset(d, o));
            // JPEG thumbnail (JPEGInterchangeFormat + Length) and uncompressed
            // single-strip thumbnail (StripOffsets + StripByteCounts).
            let ranges: Vec<Range<usize>> = [(0x0201, 0x0202), (0x0111, 0x0117)]
                .into_iter()
                .filter_map(|(off, len)| Some(find(off)?..find(off)?.saturating_add(find(len)?)))
                .collect();
            for r in ranges {
                zero(self.d, r);
            }
        }
        self.wipe_ifd(ifd);
        self.changes.removed("EXIF thumbnail");
    }
}

/// Removes the requested information from a TIFF/EXIF block in place.
///
/// `tiff` must start at the TIFF header (`II*\0` / `MM\0*`), i.e. after the
/// `Exif\0\0` prefix of a JPEG APP1 segment.
pub fn edit(tiff: &mut [u8], edit: ExifEdit, changes: &mut Changes) -> Result<()> {
    let o = Order::detect(tiff)?;
    let ifd0 = o.u32(tiff, 4).ok_or_else(|| Error::malformed("EXIF", "truncated header"))? as usize;
    let mut ed = Editor { d: tiff, o, edit, changes, visited: HashSet::new() };
    if !ed.enter(ifd0) {
        return Err(Error::malformed("EXIF", "invalid IFD0 offset"));
    }
    let next_pos = ed.edit_ifd(ifd0, IfdKind::Ifd0)?;
    let ifd1 = o.u32(ed.d, next_pos).unwrap_or(0) as usize;
    if ed.enter(ifd1) {
        if edit.previews {
            ed.wipe_thumbnail(ifd1);
            o.put_u32(ed.d, next_pos, 0);
        } else {
            ed.edit_ifd(ifd1, IfdKind::Ifd1)?;
        }
    }
    Ok(())
}

/// Lists noteworthy tags of a TIFF/EXIF block.
pub fn inspect(tiff: &[u8], block: &str, out: &mut Vec<Finding>) {
    let Ok(o) = Order::detect(tiff) else {
        out.push(Finding { block: block.into(), detail: "unparseable EXIF".into(), sensitive: true });
        return;
    };
    let mut visited = HashSet::new();
    let mut stack = vec![(o.u32(tiff, 4).unwrap_or(0) as usize, IfdKind::Ifd0)];
    while let Some((ifd, kind)) = stack.pop() {
        if ifd == 0 || !visited.insert(ifd) {
            continue;
        }
        let Ok(entries) = read_entries(tiff, o, ifd) else { continue };
        if kind == IfdKind::Gps {
            let detail = gps_position(tiff, o, &entries)
                .map(|(lat, lon)| format!("GPS position {lat:.6}, {lon:.6}"))
                .unwrap_or_else(|| format!("GPS directory ({} tags)", entries.len()));
            out.push(Finding { block: block.into(), detail, sensitive: true });
            continue;
        }
        if kind == IfdKind::Ifd1 {
            out.push(Finding { block: block.into(), detail: "embedded thumbnail".into(), sensitive: true });
            continue;
        }
        for e in &entries {
            match e.tag {
                TAG_EXIF_IFD => stack.extend(e.offset(tiff, o).map(|s| (s, IfdKind::Exif))),
                TAG_GPS_IFD => stack.extend(e.offset(tiff, o).map(|s| (s, IfdKind::Gps))),
                TAG_INTEROP_IFD => {}
                tag => {
                    let (name, sensitive) = match (sensitive_name(tag), INFO_TAGS.iter().find(|(t, _)| *t == tag)) {
                        (Some(n), _) => (n, true),
                        (None, Some((_, n))) => (*n, false),
                        _ => continue,
                    };
                    let detail = match ascii_value(tiff, o, e) {
                        Some(v) => format!("{name}: {v}"),
                        None => name.to_string(),
                    };
                    out.push(Finding { block: block.into(), detail, sensitive });
                }
            }
        }
        if kind == IfdKind::Ifd0 {
            let next = o.u32(tiff, ifd + 2 + entries.len() * 12).unwrap_or(0) as usize;
            stack.push((next, IfdKind::Ifd1));
        }
    }
}

/// Capture date of a TIFF/EXIF block: `DateTimeOriginal`, then
/// `DateTimeDigitized`, then the IFD0 `DateTime`. Returns the raw string and
/// the tag name.
pub fn capture_date(tiff: &[u8]) -> Option<(String, &'static str)> {
    let o = Order::detect(tiff).ok()?;
    let ifd0 = read_entries(tiff, o, o.u32(tiff, 4)? as usize).ok()?;
    let raw = |e: &Entry| -> Option<String> {
        let r = e.data_range(tiff, o).filter(|_| e.typ == 2)?;
        Some(String::from_utf8_lossy(&tiff[r]).trim_end_matches('\0').trim().to_string())
    };
    let exif = ifd0
        .iter()
        .find(|e| e.tag == TAG_EXIF_IFD)
        .and_then(|e| e.offset(tiff, o))
        .and_then(|off| read_entries(tiff, o, off).ok())
        .unwrap_or_default();
    let candidates = [
        (&exif, 0x9003, "EXIF DateTimeOriginal"),
        (&exif, 0x9004, "EXIF DateTimeDigitized"),
        (&ifd0, 0x0132, "EXIF DateTime"),
    ];
    let found = candidates
        .into_iter()
        .find_map(|(entries, tag, name)| entries.iter().find(|e| e.tag == tag).and_then(raw).map(|v| (v, name)));
    found
}

fn ascii_value(d: &[u8], o: Order, e: &Entry) -> Option<String> {
    if e.typ != 2 {
        return None;
    }
    let raw = d.get(e.data_range(d, o)?)?;
    let s = String::from_utf8_lossy(raw).trim_end_matches('\0').trim().to_string();
    let mut s: String = s.chars().take(60).collect();
    if s.is_empty() {
        return None;
    }
    if raw.len() > 60 {
        s.push('…');
    }
    Some(s)
}

/// Decodes GPSLatitude/GPSLongitude into signed decimal degrees.
fn gps_position(d: &[u8], o: Order, entries: &[Entry]) -> Option<(f64, f64)> {
    let get = |tag| entries.iter().find(|e| e.tag == tag);
    let dms = |e: &Entry| -> Option<f64> {
        let r = e.data_range(d, o).filter(|r| e.typ == 5 && r.len() == 24)?;
        let mut v = [0f64; 3];
        for (i, x) in v.iter_mut().enumerate() {
            let num = o.u32(d, r.start + i * 8)? as f64;
            let den = o.u32(d, r.start + i * 8 + 4)? as f64;
            *x = if den == 0.0 { 0.0 } else { num / den };
        }
        Some(v[0] + v[1] / 60.0 + v[2] / 3600.0)
    };
    let sign = |tag, neg: u8| get(tag).and_then(|e| d.get(e.value_pos())).map(|&c| if c == neg { -1.0 } else { 1.0 });
    let lat = dms(get(2)?)? * sign(1, b'S').unwrap_or(1.0);
    let lon = dms(get(4)?)? * sign(3, b'W').unwrap_or(1.0);
    Some((lat, lon))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Builds a little-endian EXIF block with Make, Artist, an Exif IFD
    /// (serial number + MakerNote), a GPS IFD and a thumbnail IFD.
    pub(crate) fn sample_exif() -> Vec<u8> {
        let mut d = vec![0u8; 512];
        d[..8].copy_from_slice(&[b'I', b'I', 0x2A, 0, 8, 0, 0, 0]);
        let w16 = |d: &mut Vec<u8>, o: usize, v: u16| d[o..o + 2].copy_from_slice(&v.to_le_bytes());
        let w32 = |d: &mut Vec<u8>, o: usize, v: u32| d[o..o + 4].copy_from_slice(&v.to_le_bytes());
        let entry = |d: &mut Vec<u8>, pos: usize, tag: u16, typ: u16, count: u32, val: u32| {
            w16(d, pos, tag);
            w16(d, pos + 2, typ);
            w32(d, pos + 4, count);
            w32(d, pos + 8, val);
        };
        // IFD0 @8: Make(inline "Cam"), Artist(@200 "Jane Doe"), ExifIFD(@100), GPS(@132)
        w16(&mut d, 8, 4);
        entry(&mut d, 10, 0x010F, 2, 4, u32::from_le_bytes(*b"Cam\0"));
        entry(&mut d, 22, 0x013B, 2, 9, 200);
        entry(&mut d, 34, TAG_EXIF_IFD, 4, 1, 100);
        entry(&mut d, 46, TAG_GPS_IFD, 4, 1, 132);
        w32(&mut d, 58, 300); // next IFD = IFD1
        d[200..209].copy_from_slice(b"Jane Doe\0");
        // Exif IFD @100: BodySerialNumber(@220), MakerNote(@240, 8 bytes)
        w16(&mut d, 100, 2);
        entry(&mut d, 102, 0xA431, 2, 7, 220);
        entry(&mut d, 114, TAG_MAKER_NOTE, 7, 8, 240);
        w32(&mut d, 126, 0);
        d[220..227].copy_from_slice(b"SN1234\0");
        d[240..248].copy_from_slice(b"MAKERNOT");
        // GPS IFD @132: LatitudeRef "N", Latitude (@400), LongitudeRef "W", Longitude (@424)
        w16(&mut d, 132, 4);
        entry(&mut d, 134, 1, 2, 2, u32::from_le_bytes(*b"N\0\0\0"));
        entry(&mut d, 146, 2, 5, 3, 400);
        entry(&mut d, 158, 3, 2, 2, u32::from_le_bytes(*b"W\0\0\0"));
        entry(&mut d, 170, 4, 5, 3, 424);
        w32(&mut d, 182, 0);
        for (i, (n, den)) in [(45u32, 1u32), (30, 1), (0, 1), (73, 1), (15, 1), (36, 10)].iter().enumerate() {
            w32(&mut d, 400 + i * 8, *n);
            w32(&mut d, 404 + i * 8, *den);
        }
        // IFD1 @300: JPEGInterchangeFormat(@450) + Length(16)
        w16(&mut d, 300, 2);
        entry(&mut d, 302, 0x0201, 4, 1, 450);
        entry(&mut d, 314, 0x0202, 4, 1, 16);
        w32(&mut d, 326, 0);
        d[450..466].copy_from_slice(b"THUMBNAILTHUMBNA");
        d
    }

    fn contains(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn sensitive_edit_removes_identity_and_location() {
        let mut d = sample_exif();
        let mut ch = Changes::default();
        edit(&mut d, ExifEdit { sensitive: true, previews: true }, &mut ch).unwrap();
        assert!(!contains(&d, b"Jane Doe"));
        assert!(!contains(&d, b"SN1234"));
        assert!(!contains(&d, b"MAKERNOT"));
        assert!(!contains(&d, b"THUMBNAIL"));
        assert!(d[132..186].iter().chain(&d[400..448]).all(|&b| b == 0), "GPS data must be zeroed");
        // Make is preserved and the structure is still parseable.
        let o = Order::detect(&d).unwrap();
        let ifd0 = read_entries(&d, o, 8).unwrap();
        assert_eq!(ifd0.iter().map(|e| e.tag).collect::<Vec<_>>(), vec![0x010F, TAG_EXIF_IFD]);
        assert_eq!(o.u32(&d, 8 + 2 + 2 * 12), Some(0), "IFD1 link must be cleared");
        let exif = read_entries(&d, o, 100).unwrap();
        assert!(exif.is_empty());
        assert!(ch.removed.iter().any(|r| r.contains("GPS")));
    }

    #[test]
    fn preview_only_edit_keeps_identity() {
        let mut d = sample_exif();
        let mut ch = Changes::default();
        edit(&mut d, ExifEdit { sensitive: false, previews: true }, &mut ch).unwrap();
        assert!(contains(&d, b"Jane Doe"));
        assert!(contains(&d, b"SN1234"));
        assert!(!contains(&d, b"THUMBNAIL"));
        assert!(!contains(&d, b"MAKERNOT"));
    }

    #[test]
    fn inspect_reports_gps() {
        let mut f = Vec::new();
        inspect(&sample_exif(), "EXIF", &mut f);
        assert!(f.iter().any(|x| x.detail == "GPS position 45.500000, -73.251000"), "{f:?}");
        assert!(f.iter().any(|x| x.detail == "Artist: Jane Doe" && x.sensitive));
        assert!(f.iter().any(|x| x.detail == "Make: Cam" && !x.sensitive));
    }

    #[test]
    fn rejects_garbage() {
        let mut d = b"not a tiff".to_vec();
        assert!(edit(&mut d, ExifEdit { sensitive: true, previews: true }, &mut Changes::default()).is_err());
    }
}
