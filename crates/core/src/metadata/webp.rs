//! WebP (RIFF) chunk filtering.
//!
//! An extended WebP file is `RIFF <size> WEBP` followed by a `VP8X` header
//! whose flags announce optional `ICCP`, `EXIF` and `XMP ` chunks. We drop or
//! rewrite those chunks and keep the flags and the RIFF size consistent.

use super::{exif, human_size, Changes, Finding, Plan};
use crate::{Error, Result};

const FLAG_ICC: u8 = 0x20;
const FLAG_ALPHA: u8 = 0x10;
const FLAG_EXIF: u8 = 0x08;
const FLAG_XMP: u8 = 0x04;
const FLAG_ANIM: u8 = 0x02;

/// Chunks that make up the image itself.
const IMAGE: &[&[u8; 4]] = &[b"VP8 ", b"VP8L", b"VP8X", b"ALPH", b"ANIM", b"ANMF", b"ICCP"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk<'a> {
    pub fourcc: [u8; 4],
    pub data: std::borrow::Cow<'a, [u8]>,
}

impl Chunk<'_> {
    fn name(&self) -> String {
        String::from_utf8_lossy(&self.fourcc).trim_end().to_string()
    }
}

pub fn parse(data: &[u8]) -> Result<Vec<Chunk<'_>>> {
    if data.len() < 12 || &data[..4] != b"RIFF" || &data[8..12] != b"WEBP" {
        return Err(Error::malformed("WebP", "missing RIFF/WEBP header"));
    }
    let riff_end = (u32::from_le_bytes(data[4..8].try_into().unwrap()) as usize + 8).min(data.len());
    let mut pos = 12;
    let mut chunks = Vec::new();
    while pos + 8 <= riff_end {
        let fourcc: [u8; 4] = data[pos..pos + 4].try_into().unwrap();
        let len = u32::from_le_bytes(data[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let end = pos + 8 + len;
        if end > riff_end {
            return Err(Error::malformed("WebP", "chunk length out of bounds"));
        }
        chunks.push(Chunk { fourcc, data: std::borrow::Cow::Borrowed(&data[pos + 8..end]) });
        pos = end + (len & 1);
    }
    Ok(chunks)
}

pub fn is_animated(chunks: &[Chunk<'_>]) -> bool {
    chunks.iter().any(|c| &c.fourcc == b"ANIM" || &c.fourcc == b"ANMF")
}

/// Serialises chunks, fixing up the `VP8X` flags and the RIFF size.
pub fn write(chunks: &[Chunk<'_>]) -> Vec<u8> {
    let has = |cc: &[u8; 4]| chunks.iter().any(|c| &c.fourcc == cc);
    let mut body = Vec::with_capacity(chunks.iter().map(|c| c.data.len() + 9).sum());
    for c in chunks {
        let mut data = c.data.to_vec();
        if &c.fourcc == b"VP8X" && !data.is_empty() {
            let mut flags = data[0] & !(FLAG_ICC | FLAG_EXIF | FLAG_XMP);
            if has(b"ICCP") {
                flags |= FLAG_ICC;
            }
            if has(b"EXIF") {
                flags |= FLAG_EXIF;
            }
            if has(b"XMP ") {
                flags |= FLAG_XMP;
            }
            data[0] = flags;
        }
        body.extend_from_slice(&c.fourcc);
        body.extend_from_slice(&(data.len() as u32).to_le_bytes());
        body.extend_from_slice(&data);
        if data.len() % 2 == 1 {
            body.push(0);
        }
    }
    let mut out = Vec::with_capacity(body.len() + 12);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((body.len() + 4) as u32).to_le_bytes());
    out.extend_from_slice(b"WEBP");
    out.extend(body);
    out
}

fn filter<'a>(c: &Chunk<'a>, plan: Plan, ch: &mut Changes) -> Option<Chunk<'a>> {
    let size = human_size(c.data.len());
    if IMAGE.contains(&&c.fourcc) {
        return Some(c.clone());
    }
    match &c.fourcc {
        b"EXIF" if !plan.strip_all() => {
            let mut v = c.data.to_vec();
            // Some writers prefix the TIFF header with the JPEG-style "Exif\0\0".
            let skip = if v.starts_with(b"Exif\0\0") { 6 } else { 0 };
            let edit = exif::ExifEdit { sensitive: plan.strip_sensitive(), previews: plan.strip_previews() };
            match exif::edit(&mut v[skip..], edit, ch) {
                Ok(()) => Some(Chunk { fourcc: c.fourcc, data: v.into() }),
                Err(_) => {
                    ch.removed(format!("EXIF ({size}): unparseable, removed entirely"));
                    None
                }
            }
        }
        b"XMP " if !plan.strip_previews() => Some(c.clone()),
        _ if !plan.strip_unknown() && !matches!(&c.fourcc, b"EXIF" | b"XMP ") => Some(c.clone()),
        _ => {
            ch.removed(format!("{} ({size})", c.name()));
            None
        }
    }
}

/// Removes metadata chunks; image data is copied untouched.
pub fn strip(data: &[u8], plan: Plan, ch: &mut Changes) -> Result<Vec<u8>> {
    let chunks = parse(data)?;
    let kept: Vec<Chunk<'_>> = chunks.iter().filter_map(|c| filter(c, plan, ch)).collect();
    Ok(write(&kept))
}

/// Grafts the (filtered) ICC/EXIF/XMP chunks of `original` onto a freshly
/// encoded single-frame WebP of size `width`×`height`.
pub fn rebuild(
    original: &[u8],
    encoded: &[u8],
    width: u32,
    height: u32,
    alpha: bool,
    plan: Plan,
    ch: &mut Changes,
) -> Result<Vec<u8>> {
    let orig = parse(original)?;
    let enc = parse(encoded)?;
    let extras: Vec<Chunk<'_>> = orig
        .iter()
        .filter(|c| matches!(&c.fourcc, b"ICCP" | b"EXIF" | b"XMP "))
        .filter_map(|c| filter(c, plan, ch))
        .collect();
    let image: Vec<Chunk<'_>> = enc.into_iter().filter(|c| &c.fourcc != b"VP8X").collect();
    if extras.is_empty() {
        return Ok(write(&image));
    }
    let mut vp8x = vec![if alpha { FLAG_ALPHA } else { 0 }, 0, 0, 0];
    vp8x.extend_from_slice(&(width - 1).to_le_bytes()[..3]);
    vp8x.extend_from_slice(&(height - 1).to_le_bytes()[..3]);
    // Order mandated by the spec: VP8X, ICCP, image data, EXIF, XMP.
    let mut out = vec![Chunk { fourcc: *b"VP8X", data: vp8x.into() }];
    out.extend(extras.iter().filter(|c| &c.fourcc == b"ICCP").cloned());
    out.extend(image);
    out.extend(extras.iter().filter(|c| &c.fourcc != b"ICCP").cloned());
    debug_assert!(out[0].data[0] & FLAG_ANIM == 0);
    Ok(write(&out))
}

pub fn inspect(data: &[u8], out: &mut Vec<Finding>) -> Result<()> {
    for c in parse(data)? {
        let size = human_size(c.data.len());
        match &c.fourcc {
            b"EXIF" => {
                let skip = if c.data.starts_with(b"Exif\0\0") { 6 } else { 0 };
                exif::inspect(&c.data[skip..], "EXIF", out)
            }
            b"ICCP" => {
                out.push(Finding { block: c.name(), detail: format!("{size} (kept: rendering)"), sensitive: false })
            }
            cc if IMAGE.contains(&cc) => {}
            _ => out.push(Finding { block: c.name(), detail: size, sensitive: true }),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::MetadataPolicy;

    fn chunk(cc: &[u8; 4], data: &[u8]) -> Chunk<'static> {
        Chunk { fourcc: *cc, data: data.to_vec().into() }
    }

    #[test]
    fn strips_and_fixes_flags() {
        let exif_data = exif::tests::sample_exif();
        let file = write(&[
            chunk(b"VP8X", &[FLAG_EXIF | FLAG_XMP | FLAG_ICC, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
            chunk(b"ICCP", b"icc"),
            chunk(b"VP8L", b"pixels"),
            chunk(b"EXIF", &exif_data),
            chunk(b"XMP ", b"<xmp>Jane</xmp>"),
        ]);
        let out = strip(&file, Plan::new(MetadataPolicy::All, false), &mut Changes::default()).unwrap();
        let chunks = parse(&out).unwrap();
        let names: Vec<String> = chunks.iter().map(|c| c.name()).collect();
        assert_eq!(names, ["VP8X", "ICCP", "VP8L"]);
        assert_eq!(chunks[0].data[0], FLAG_ICC);
        assert_eq!(u32::from_le_bytes(out[4..8].try_into().unwrap()) as usize, out.len() - 8);

        let out = strip(&file, Plan::new(MetadataPolicy::Sensitive, false), &mut Changes::default()).unwrap();
        let chunks = parse(&out).unwrap();
        assert_eq!(chunks.len(), 4, "EXIF is kept (filtered), XMP removed");
        assert_eq!(chunks[0].data[0], FLAG_ICC | FLAG_EXIF);
    }
}
