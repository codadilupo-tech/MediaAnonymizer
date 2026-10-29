//! PNG chunk filtering.
//!
//! Critical chunks (`IHDR`, `PLTE`, `IDAT`, `IEND`) and chunks that affect how
//! pixels are rendered are always kept; textual, EXIF, time and unknown
//! ancillary chunks are filtered according to the [`Plan`].

use super::{exif, human_size, Changes, Finding, Plan};
use crate::{Error, Result};

pub const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";

/// Chunks needed to display the image exactly as the author intended.
const RENDERING: &[&[u8; 4]] = &[
    b"iCCP", b"sRGB", b"gAMA", b"cHRM", b"cICP", b"mDCV", b"cLLI", b"mDCv", b"cLLi", b"sBIT", b"tRNS", b"bKGD",
    b"pHYs", b"sPLT", b"hIST", b"acTL", b"fcTL", b"fdAT",
];

/// Color-space chunks that stay valid after we re-encode the pixels.
const COLOR_SPACE: &[&[u8; 4]] =
    &[b"iCCP", b"sRGB", b"gAMA", b"cHRM", b"cICP", b"mDCV", b"cLLI", b"mDCv", b"cLLi", b"pHYs"];

/// Text keywords that carry no personal information.
const SAFE_TEXT_KEYWORDS: &[&str] = &["Title", "Software", "Disclaimer", "Warning"];

/// A PNG chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk<'a> {
    pub kind: [u8; 4],
    pub data: std::borrow::Cow<'a, [u8]>,
}

impl Chunk<'_> {
    fn is_critical(&self) -> bool {
        self.kind[0].is_ascii_uppercase()
    }

    fn name(&self) -> String {
        String::from_utf8_lossy(&self.kind).into_owned()
    }

    /// Keyword of `tEXt`/`zTXt`/`iTXt` chunks.
    fn keyword(&self) -> Option<String> {
        if !matches!(&self.kind, b"tEXt" | b"zTXt" | b"iTXt") {
            return None;
        }
        let end = self.data.iter().position(|&b| b == 0).unwrap_or(self.data.len());
        Some(String::from_utf8_lossy(&self.data[..end]).into_owned())
    }
}

/// Splits a PNG into chunks.
pub fn parse(data: &[u8]) -> Result<Vec<Chunk<'_>>> {
    if !data.starts_with(SIGNATURE) {
        return Err(Error::malformed("PNG", "missing signature"));
    }
    let mut pos = 8;
    let mut chunks = Vec::new();
    while pos + 12 <= data.len() {
        let len = u32::from_be_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        let kind: [u8; 4] = data[pos + 4..pos + 8].try_into().unwrap();
        let end = pos + 12 + len;
        if end > data.len() {
            return Err(Error::malformed("PNG", "chunk length out of bounds"));
        }
        chunks.push(Chunk { kind, data: std::borrow::Cow::Borrowed(&data[pos + 8..pos + 8 + len]) });
        pos = end;
        if &kind == b"IEND" {
            break;
        }
    }
    if chunks.first().map(|c| &c.kind) != Some(b"IHDR") || chunks.last().map(|c| &c.kind) != Some(b"IEND") {
        return Err(Error::malformed("PNG", "missing IHDR or IEND"));
    }
    Ok(chunks)
}

/// Whether the PNG is animated (APNG); re-encoding would lose frames.
pub fn is_animated(chunks: &[Chunk<'_>]) -> bool {
    chunks.iter().any(|c| &c.kind == b"acTL")
}

pub fn write(chunks: &[Chunk<'_>]) -> Vec<u8> {
    let mut out = Vec::with_capacity(chunks.iter().map(|c| c.data.len() + 12).sum::<usize>() + 8);
    out.extend_from_slice(SIGNATURE);
    for c in chunks {
        out.extend_from_slice(&(c.data.len() as u32).to_be_bytes());
        out.extend_from_slice(&c.kind);
        out.extend_from_slice(&c.data);
        let mut h = crc32fast::Hasher::new();
        h.update(&c.kind);
        h.update(&c.data);
        out.extend_from_slice(&h.finalize().to_be_bytes());
    }
    out
}

fn gone<T>(ch: &mut Changes, why: String) -> Option<T> {
    ch.removed(why);
    None
}

fn filter<'a>(c: &Chunk<'a>, plan: Plan, ch: &mut Changes) -> Option<Chunk<'a>> {
    let size = human_size(c.data.len());
    if c.is_critical() || RENDERING.contains(&&c.kind) {
        return Some(c.clone());
    }
    match &c.kind {
        b"eXIf" if plan.strip_all() => gone(ch, format!("eXIf EXIF ({size})")),
        b"eXIf" => {
            let mut v = c.data.to_vec();
            let edit = exif::ExifEdit { sensitive: plan.strip_sensitive(), previews: plan.strip_previews() };
            match exif::edit(&mut v, edit, ch) {
                Ok(()) => Some(Chunk { kind: c.kind, data: v.into() }),
                Err(_) => gone(ch, format!("eXIf EXIF ({size}): unparseable, removed entirely")),
            }
        }
        b"tIME" if plan.strip_all() => gone(ch, "tIME modification time".into()),
        b"tIME" => Some(c.clone()),
        b"tEXt" | b"zTXt" | b"iTXt" => {
            let kw = c.keyword().unwrap_or_default();
            let embeds_blob = kw.starts_with("Raw profile type") || kw == "XML:com.adobe.xmp";
            let remove = plan.strip_all()
                || (plan.strip_sensitive() && !SAFE_TEXT_KEYWORDS.contains(&kw.as_str()))
                || (plan.strip_previews() && embeds_blob);
            if remove {
                gone(ch, format!("{} \"{kw}\" ({size})", c.name()))
            } else {
                Some(c.clone())
            }
        }
        _ if plan.strip_unknown() => gone(ch, format!("{} ({size})", c.name())),
        _ => Some(c.clone()),
    }
}

/// Removes metadata chunks; image data is copied untouched.
pub fn strip(data: &[u8], plan: Plan, ch: &mut Changes) -> Result<Vec<u8>> {
    let chunks = parse(data)?;
    let kept: Vec<Chunk<'_>> = chunks.iter().filter_map(|c| filter(c, plan, ch)).collect();
    Ok(write(&kept))
}

/// Grafts the (filtered) metadata of `original` onto a freshly encoded PNG.
///
/// Only color-space and metadata chunks are carried over: palette and
/// transparency chunks describe the old pixel layout and would be wrong.
pub fn rebuild(original: &[u8], encoded: &[u8], plan: Plan, ch: &mut Changes) -> Result<Vec<u8>> {
    let orig = parse(original)?;
    let enc = parse(encoded)?;
    let mut out: Vec<Chunk<'_>> = Vec::with_capacity(enc.len() + orig.len());
    out.push(enc[0].clone()); // IHDR
    for c in &orig {
        let carry = COLOR_SPACE.contains(&&c.kind) || !(c.is_critical() || RENDERING.contains(&&c.kind));
        if carry {
            if let Some(k) = filter(c, plan, ch) {
                out.push(k);
            }
        }
    }
    out.extend(enc[1..].iter().cloned());
    Ok(write(&out))
}

/// Lists the metadata chunks of a PNG.
pub fn inspect(data: &[u8], out: &mut Vec<Finding>) -> Result<()> {
    for c in parse(data)? {
        if c.is_critical() {
            continue;
        }
        let size = human_size(c.data.len());
        match &c.kind {
            b"eXIf" => exif::inspect(&c.data, "eXIf", out),
            k if RENDERING.contains(&k) => {
                out.push(Finding { block: c.name(), detail: format!("{size} (kept: rendering)"), sensitive: false })
            }
            b"tIME" => out.push(Finding { block: c.name(), detail: "modification time".into(), sensitive: false }),
            _ => {
                let detail = match c.keyword() {
                    Some(kw) => format!("\"{kw}\" ({size})"),
                    None => size,
                };
                let sensitive = !c.keyword().is_some_and(|k| SAFE_TEXT_KEYWORDS.contains(&k.as_str()));
                out.push(Finding { block: c.name(), detail, sensitive });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::options::MetadataPolicy;

    fn chunk(kind: &[u8; 4], data: &[u8]) -> Chunk<'static> {
        Chunk { kind: *kind, data: data.to_vec().into() }
    }

    fn sample() -> Vec<u8> {
        write(&[
            chunk(b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 2, 0, 0, 0]),
            chunk(b"gAMA", &[0, 0, 0xB1, 0x8F]),
            chunk(b"tEXt", b"Author\0Jane Doe"),
            chunk(b"tEXt", b"Software\0paint"),
            chunk(b"iTXt", b"XML:com.adobe.xmp\0\0\0\0\0<xmp/>"),
            chunk(b"tIME", &[7, 0xE8, 1, 1, 0, 0, 0]),
            chunk(b"caBX", b"c2pa manifest"),
            chunk(b"IDAT", b"pixels"),
            chunk(b"IEND", b""),
        ])
    }

    fn kinds(data: &[u8]) -> Vec<String> {
        parse(data).unwrap().iter().map(|c| c.name()).collect()
    }

    #[test]
    fn strip_all() {
        let out = strip(&sample(), Plan::new(MetadataPolicy::All, false), &mut Changes::default()).unwrap();
        assert_eq!(kinds(&out), ["IHDR", "gAMA", "IDAT", "IEND"]);
    }

    #[test]
    fn strip_sensitive() {
        let out = strip(&sample(), Plan::new(MetadataPolicy::Sensitive, false), &mut Changes::default()).unwrap();
        assert_eq!(kinds(&out), ["IHDR", "gAMA", "tEXt", "tIME", "IDAT", "IEND"]);
        assert!(out.windows(8).any(|w| w == b"Software"));
    }

    #[test]
    fn crc_is_valid() {
        let out = strip(&sample(), Plan::new(MetadataPolicy::All, false), &mut Changes::default()).unwrap();
        let img = image::load_from_memory_with_format(&out, image::ImageFormat::Png);
        // Pixel data is fake so decoding fails later, but never on a CRC mismatch.
        if let Err(e) = img {
            assert!(!e.to_string().to_lowercase().contains("crc"), "{e}");
        }
    }
}
