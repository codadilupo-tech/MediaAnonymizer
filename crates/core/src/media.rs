//! Media type sniffing.
//!
//! Detection relies on magic bytes first and falls back to the file
//! extension, so a mislabelled file is still handled correctly.

use serde::{Deserialize, Serialize};
use std::path::Path;

/// Still image container formats handled natively.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageFormat {
    Jpeg,
    Png,
    Webp,
    Tiff,
    Bmp,
}

impl ImageFormat {
    pub fn name(self) -> &'static str {
        match self {
            ImageFormat::Jpeg => "JPEG",
            ImageFormat::Png => "PNG",
            ImageFormat::Webp => "WebP",
            ImageFormat::Tiff => "TIFF",
            ImageFormat::Bmp => "BMP",
        }
    }
}

/// High-level category of an input file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", tag = "kind", content = "format")]
pub enum MediaKind {
    Image(ImageFormat),
    /// Anything FFmpeg can demux (MP4, MOV, MKV, WebM, AVI, ...).
    Video,
}

/// File extensions considered when scanning directories.
pub const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "jpe", "jfif", "png", "webp", "tif", "tiff", "bmp"];
pub const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "m4v", "mov", "qt", "mkv", "webm", "avi", "mts", "m2ts", "ts", "3gp", "3g2", "mxf", "wmv", "flv", "ogv",
    "mpg", "mpeg",
];

/// Returns `true` if the extension of `path` is one we know how to process.
pub fn has_supported_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            let e = e.to_ascii_lowercase();
            IMAGE_EXTENSIONS.contains(&e.as_str()) || VIDEO_EXTENSIONS.contains(&e.as_str())
        })
        .unwrap_or(false)
}

/// Identifies an image format from its first bytes.
pub fn sniff_image(head: &[u8]) -> Option<ImageFormat> {
    match head {
        [0xFF, 0xD8, 0xFF, ..] => Some(ImageFormat::Jpeg),
        [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, ..] => Some(ImageFormat::Png),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some(ImageFormat::Webp),
        [b'I', b'I', 0x2A, 0x00, ..] | [b'M', b'M', 0x00, 0x2A, ..] => Some(ImageFormat::Tiff),
        [b'B', b'M', ..] => Some(ImageFormat::Bmp),
        _ => None,
    }
}

/// Detects the media kind of a file on disk.
pub fn detect(path: &Path) -> crate::Result<MediaKind> {
    use std::io::Read;
    let mut head = [0u8; 16];
    let mut file = std::fs::File::open(path).map_err(|e| crate::Error::io(path, e))?;
    let n = file.read(&mut head).map_err(|e| crate::Error::io(path, e))?;
    if let Some(fmt) = sniff_image(&head[..n]) {
        return Ok(MediaKind::Image(fmt));
    }
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).unwrap_or_default();
    if VIDEO_EXTENSIONS.contains(&ext.as_str()) || looks_like_video(&head[..n]) {
        return Ok(MediaKind::Video);
    }
    Err(crate::Error::Unsupported(format!("{}: unrecognised file type", path.display())))
}

/// Cheap container signatures for videos without a known extension.
fn looks_like_video(head: &[u8]) -> bool {
    let iso_bmff = head.len() >= 8 && &head[4..8] == b"ftyp";
    let ebml = head.starts_with(&[0x1A, 0x45, 0xDF, 0xA3]); // Matroska / WebM
    let avi = head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"AVI ";
    let mpeg_ts = head.first() == Some(&0x47);
    iso_bmff || ebml || avi || mpeg_ts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_images() {
        assert_eq!(sniff_image(&[0xFF, 0xD8, 0xFF, 0xE0]), Some(ImageFormat::Jpeg));
        assert_eq!(sniff_image(b"\x89PNG\r\n\x1a\n...."), Some(ImageFormat::Png));
        assert_eq!(sniff_image(b"RIFF\0\0\0\0WEBPVP8 "), Some(ImageFormat::Webp));
        assert_eq!(sniff_image(b"MM\0*"), Some(ImageFormat::Tiff));
        assert_eq!(sniff_image(b"\0\0\0\x18ftypmp42"), None);
        assert!(looks_like_video(b"\0\0\0\x18ftypmp42"));
    }
}
