//! Still image pipeline.
//!
//! * Metadata only: container-level edit, pixel data copied untouched.
//! * Faces / timestamp: decode, detect (upright), paint the regions, draw
//!   the timestamp, then re-encode without generation loss:
//!   - JPEG: DCT-domain edit, only blocks touching a face or the text change;
//!   - PNG/TIFF/BMP: lossless by nature, same bit depth and channels;
//!   - WebP: lossless WebP (no further loss even for lossy sources).

pub mod jpeg_dct;
pub mod orientation;

use crate::detect::{detection_dims, DetectorPool, Face, Point, RgbFrame};
use crate::edits::{FaceBox, PreviewFace, VisibleFace};
use crate::media::ImageFormat;
use crate::metadata::{exif, jpeg, png, webp, Changes, Plan};
use crate::options::{FaceOptions, Options, TimestampOptions};
use crate::overlay::{DateSource, DateTime, Stamp};
use crate::redact::{self, Plane, Region};
use crate::{CancelToken, Error, Result};
use image::metadata::Orientation;
use image::{DynamicImage, GenericImageView, ImageEncoder};
use std::io::Cursor;
use std::time::SystemTime;

/// Longest side of the detector input for photos when not configured.
pub const DEFAULT_DETECTION_SIZE: u32 = 1920;

/// Output of [`process`].
#[derive(Debug)]
pub struct Processed {
    pub data: Vec<u8>,
    /// Faces covered.
    pub faces: Vec<Face>,
    /// Faces left uncovered because of a [`VisibleFace`] mark.
    pub visible: usize,
    pub changes: Changes,
    /// How the pixels were written (e.g. "lossless: 3.1% of JPEG blocks re-encoded").
    pub notes: Vec<String>,
    /// Text of the timestamp overlay and where the date came from.
    pub timestamp: Option<String>,
}

/// Processes one image held in memory.
///
/// `file_time` (the file modification time) is the fallback date of the
/// timestamp overlay when the image carries no capture date.
pub fn process(
    input: &[u8],
    format: ImageFormat,
    opts: &Options,
    detector: Option<&DetectorPool>,
    visible: &[VisibleFace],
    file_time: Option<SystemTime>,
    cancel: &CancelToken,
) -> Result<Processed> {
    let plan = Plan::new(opts.metadata, opts.faces.is_some());
    let mut changes = Changes::default();
    let mut notes = Vec::new();

    let faces_opts = match (&opts.faces, detector) {
        (Some(f), Some(d)) => Some((f, d)),
        (Some(_), None) => return Err(Error::InvalidOptions("face redaction requested without a detector".into())),
        _ => None,
    };
    if faces_opts.is_none() && opts.timestamp.is_none() {
        let data = strip_metadata(input, format, plan, &mut changes, &mut notes)?;
        return Ok(Processed { data, faces: Vec::new(), visible: 0, changes, notes, timestamp: None });
    }

    reject_animation(input, format)?;
    let (mut img, orient) = decode(input, format)?;
    cancel.check()?;
    let mut kept = 0;
    let (faces, regions) = match faces_opts {
        Some((fopts, pool)) => {
            let found = detect(&img, orient, fopts, pool)?;
            let flags = visible_flags(&found, visible, orient, &img);
            kept = flags.iter().filter(|v| **v).count();
            let faces: Vec<Face> = found.into_iter().zip(flags).filter(|(_, v)| !v).map(|(f, _)| f).collect();
            let regions = redact::regions(&faces, fopts);
            (faces, regions)
        }
        None => (Vec::new(), Vec::new()),
    };
    if kept > 0 {
        notes.push(format!("{kept} face{} left visible as marked", if kept == 1 { "" } else { "s" }));
    }
    cancel.check()?;
    let (w, h) = img.dimensions();
    let stamp = match &opts.timestamp {
        Some(t) => timestamp_stamp(input, format, &img, orient, t, file_time, &mut changes),
        None => None,
    };
    let text = stamp.as_ref().map(|(_, text)| text.clone());
    let stamp = stamp.map(|(s, _)| s);

    let bounds = union(redact::bounds(&regions, w, h), stamp.as_ref().and_then(|s| s.bounds(w, h)));
    let Some(bounds) = bounds else {
        // Nothing to draw: the pixels stay as they are, only metadata changes.
        notes.push("no face to cover: image data copied unchanged".into());
        let data = strip_metadata(input, format, plan, &mut changes, &mut notes)?;
        return Ok(Processed { data, faces, visible: kept, changes, notes, timestamp: text });
    };

    if let Some((fopts, _)) = faces_opts {
        paint(&mut img, &regions, fopts)?;
    }
    if let Some(s) = &stamp {
        blend_stamp(&mut img, s)?;
    }
    cancel.check()?;
    let covered = |x: usize, y: usize| {
        regions.iter().any(|r| r.shape.contains(x as f32 + 0.5, y as f32 + 0.5))
            || stamp.as_ref().is_some_and(|s| s.covers(x as u32, y as u32))
    };
    let data = encode(input, format, &img, &covered, bounds, plan, &mut changes, &mut notes)?;
    Ok(Processed { data, faces, visible: kept, changes, notes, timestamp: text })
}

/// Which detected faces (stored coordinates) the marks designate.
fn visible_flags(faces: &[Face], visible: &[VisibleFace], orient: Orientation, img: &DynamicImage) -> Vec<bool> {
    let (sw, sh) = img.dimensions();
    let (ow, oh) = orientation::displayed_dims(orient, sw, sh);
    let points: Vec<Point> = visible
        .iter()
        .map(|v| {
            orientation::displayed_to_stored(orient, sw as f32, sh as f32, Point::new(v.x * ow as f32, v.y * oh as f32))
        })
        .collect();
    crate::edits::select(faces, &points)
}

/// A detected face (stored coordinates) as a box of the upright picture.
fn upright_box(f: &Face, orient: Orientation, img: &DynamicImage) -> FaceBox {
    let (sw, sh) = img.dimensions();
    let (ow, oh) = orientation::displayed_dims(orient, sw, sh);
    let up = f.map(|p| orientation::stored_to_displayed(orient, sw as f32, sh as f32, p));
    FaceBox::of(&up, ow as f32, oh as f32)
}

/// A rendered photo preview.
#[derive(Debug, Clone)]
pub struct PhotoPreview {
    /// Upright, longest side at most `max_side`.
    pub image: DynamicImage,
    /// Every detected face, covered or left visible.
    pub faces: Vec<PreviewFace>,
}

/// Renders a downscaled, upright copy of an image with faces redacted
/// (except the `visible` ones) and the timestamp drawn, for previews.
pub fn preview(
    input: &[u8],
    opts: &Options,
    pool: Option<&DetectorPool>,
    visible: &[VisibleFace],
    file_time: Option<SystemTime>,
    max_side: u32,
) -> Result<PhotoPreview> {
    let format = crate::media::sniff_image(input).ok_or_else(|| Error::Unsupported("not an image".into()))?;
    let (mut img, orient) = decode(input, format)?;
    let mut faces = Vec::new();
    if let (Some(fopts), Some(pool)) = (&opts.faces, pool) {
        let found = detect(&img, orient, fopts, pool)?;
        let flags = visible_flags(&found, visible, orient, &img);
        faces = found
            .iter()
            .zip(&flags)
            .map(|(f, &v)| PreviewFace { bbox: upright_box(f, orient, &img), visible: v })
            .collect();
        let covered: Vec<Face> = found.into_iter().zip(flags).filter(|(_, v)| !v).map(|(f, _)| f).collect();
        paint(&mut img, &redact::regions(&covered, fopts), fopts)?;
    }
    if let Some(t) = &opts.timestamp {
        if let Some((s, _)) = timestamp_stamp(input, format, &img, orient, t, file_time, &mut Changes::default()) {
            blend_stamp(&mut img, &s)?;
        }
    }
    img.apply_orientation(orient);
    Ok(PhotoPreview { image: img.thumbnail(max_side, max_side), faces })
}

fn union(a: Option<(u32, u32, u32, u32)>, b: Option<(u32, u32, u32, u32)>) -> Option<(u32, u32, u32, u32)> {
    match (a, b) {
        (Some(a), Some(b)) => Some((a.0.min(b.0), a.1.min(b.1), a.2.max(b.2), a.3.max(b.3))),
        (a, b) => a.or(b),
    }
}

/// Capture date stored in the image metadata.
fn capture_date(input: &[u8], format: ImageFormat) -> Option<(DateTime, DateSource)> {
    let from_exif = |tiff: &[u8]| {
        let (raw, tag) = exif::capture_date(tiff)?;
        Some((DateTime::parse(&raw)?, DateSource::Metadata(tag)))
    };
    let strip_prefix = |d: &[u8]| -> Vec<u8> { d.strip_prefix(b"Exif\0\0".as_slice()).unwrap_or(d).to_vec() };
    match format {
        ImageFormat::Jpeg => jpeg::parse(input)
            .ok()?
            .segments
            .iter()
            .filter(|s| s.kind() == jpeg::Kind::Exif)
            .find_map(|s| from_exif(&strip_prefix(&s.payload))),
        ImageFormat::Png => {
            let chunks = png::parse(input).ok()?;
            chunks.iter().filter(|c| &c.kind == b"eXIf").find_map(|c| from_exif(&c.data)).or_else(|| {
                let t = chunks.iter().find(|c| &c.kind == b"tIME" && c.data.len() == 7)?;
                let d = &t.data;
                let raw = format!("{}-{}-{} {}:{}:{}", u16::from_be_bytes([d[0], d[1]]), d[2], d[3], d[4], d[5], d[6]);
                Some((DateTime::parse(&raw)?, DateSource::Metadata("PNG tIME")))
            })
        }
        ImageFormat::Webp => webp::parse(input)
            .ok()?
            .iter()
            .filter(|c| &c.fourcc == b"EXIF")
            .find_map(|c| from_exif(&strip_prefix(&c.data))),
        ImageFormat::Tiff => from_exif(input),
        ImageFormat::Bmp => None,
    }
}

/// Renders the timestamp for the upright image and maps it to stored pixels.
fn timestamp_stamp(
    input: &[u8],
    format: ImageFormat,
    img: &DynamicImage,
    orient: Orientation,
    opts: &TimestampOptions,
    file_time: Option<SystemTime>,
    ch: &mut Changes,
) -> Option<(Stamp, String)> {
    let found =
        capture_date(input, format).or_else(|| Some((DateTime::from_system(file_time?)?, DateSource::FileModified)));
    let Some((date, source)) = found else {
        ch.warn("no capture date found: timestamp not drawn");
        return None;
    };
    let text = date.format(&opts.format);
    let (sw, sh) = img.dimensions();
    let (ow, oh) = orientation::displayed_dims(orient, sw, sh);
    let stamp = Stamp::render(&text, ow, oh, opts)?;
    let stamp = if orient == Orientation::NoTransforms {
        stamp
    } else {
        stamp.reproject(|x, y| orientation::stored_pixel(orient, sw, sh, x, y))
    };
    if source == DateSource::FileModified {
        ch.warn("no capture date in the metadata: timestamp taken from the file modification time (UTC)");
    }
    Some((stamp, format!("{text} ({source})")))
}

/// Draws a stamp: white text with a black outline (alpha untouched).
pub(crate) fn blend_stamp(img: &mut DynamicImage, stamp: &Stamp) -> Result<()> {
    let (w, h) = (img.width() as usize, img.height() as usize);
    macro_rules! go {
        ($buf:expr, $ty:ty, $step:expr, $ch:expr, $max:expr) => {{
            let mut p = Plane::<$ty>::new($buf, w, h, w * $step, $step, $ch);
            p.blend_stamp(stamp, &[$max; 4], &[0; 4]);
        }};
    }
    match img {
        DynamicImage::ImageLuma8(b) => go!(b, u8, 1, 1, u8::MAX),
        DynamicImage::ImageLumaA8(b) => go!(b, u8, 2, 1, u8::MAX),
        DynamicImage::ImageRgb8(b) => go!(b, u8, 3, 3, u8::MAX),
        DynamicImage::ImageRgba8(b) => go!(b, u8, 4, 3, u8::MAX),
        DynamicImage::ImageLuma16(b) => go!(b, u16, 1, 1, u16::MAX),
        DynamicImage::ImageLumaA16(b) => go!(b, u16, 2, 1, u16::MAX),
        DynamicImage::ImageRgb16(b) => go!(b, u16, 3, 3, u16::MAX),
        DynamicImage::ImageRgba16(b) => go!(b, u16, 4, 3, u16::MAX),
        _ => return Err(Error::Unsupported("floating-point images are not supported".into())),
    }
    Ok(())
}

fn strip_metadata(
    input: &[u8],
    format: ImageFormat,
    plan: Plan,
    ch: &mut Changes,
    notes: &mut Vec<String>,
) -> Result<Vec<u8>> {
    if plan.is_noop() {
        return Ok(input.to_vec());
    }
    match format {
        ImageFormat::Jpeg => jpeg::strip(input, plan, ch),
        ImageFormat::Png => png::strip(input, plan, ch),
        ImageFormat::Webp => webp::strip(input, plan, ch),
        ImageFormat::Tiff => {
            // TIFF *is* a metadata container: rewrite it with only the
            // structural tags. Pixels are decoded and stored losslessly.
            let (img, _) = decode(input, format)?;
            let mut out = Cursor::new(Vec::new());
            img.write_to(&mut out, image::ImageFormat::Tiff)?;
            ch.removed("all TIFF tags except the ones describing the pixels");
            notes.push("TIFF rewritten losslessly (pixels identical)".into());
            Ok(out.into_inner())
        }
        ImageFormat::Bmp => {
            notes.push("BMP files carry no metadata".into());
            Ok(input.to_vec())
        }
    }
}

fn reject_animation(input: &[u8], format: ImageFormat) -> Result<()> {
    let animated = match format {
        ImageFormat::Png => png::parse(input).map(|c| png::is_animated(&c)).unwrap_or(false),
        ImageFormat::Webp => webp::parse(input).map(|c| webp::is_animated(&c)).unwrap_or(false),
        _ => false,
    };
    if animated {
        return Err(Error::Unsupported(
            "face redaction on animated PNG/WebP is not supported yet; convert it to a video first".into(),
        ));
    }
    Ok(())
}

fn decode(input: &[u8], format: ImageFormat) -> Result<(DynamicImage, Orientation)> {
    use image::ImageDecoder;
    let mut reader = image::ImageReader::new(Cursor::new(input));
    reader.set_format(match format {
        ImageFormat::Jpeg => image::ImageFormat::Jpeg,
        ImageFormat::Png => image::ImageFormat::Png,
        ImageFormat::Webp => image::ImageFormat::WebP,
        ImageFormat::Tiff => image::ImageFormat::Tiff,
        ImageFormat::Bmp => image::ImageFormat::Bmp,
    });
    reader.no_limits();
    let mut decoder = reader.into_decoder()?;
    let orient = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let img = DynamicImage::from_decoder(decoder)?;
    Ok((img, orient))
}

/// Fast RGB accessor specialised per pixel layout.
fn rgb_sampler(img: &DynamicImage) -> Box<dyn Fn(u32, u32) -> [u8; 3] + Sync + '_> {
    let w = img.width() as usize;
    match img {
        DynamicImage::ImageRgb8(b) => {
            let d = b.as_raw();
            Box::new(move |x, y| {
                let i = (y as usize * w + x as usize) * 3;
                [d[i], d[i + 1], d[i + 2]]
            })
        }
        DynamicImage::ImageRgba8(b) => {
            let d = b.as_raw();
            Box::new(move |x, y| {
                let i = (y as usize * w + x as usize) * 4;
                [d[i], d[i + 1], d[i + 2]]
            })
        }
        DynamicImage::ImageLuma8(b) => {
            let d = b.as_raw();
            Box::new(move |x, y| {
                let v = d[y as usize * w + x as usize];
                [v, v, v]
            })
        }
        other => Box::new(move |x, y| {
            let p = other.get_pixel(x, y).0;
            [p[0], p[1], p[2]]
        }),
    }
}

/// Detects faces and returns them in *stored* pixel coordinates.
pub(crate) fn detect(
    img: &DynamicImage,
    orient: Orientation,
    opts: &FaceOptions,
    pool: &DetectorPool,
) -> Result<Vec<Face>> {
    let (sw, sh) = img.dimensions();
    let (ow, oh) = orientation::displayed_dims(orient, sw, sh);
    let (dw, dh) = detection_dims(ow, oh, opts.detection_size.unwrap_or(DEFAULT_DETECTION_SIZE));
    let sample = rgb_sampler(img);
    let frame = RgbFrame::downscale(ow, oh, dw, dh, |x, y| {
        let (sx, sy) = orientation::stored_pixel(orient, sw, sh, x, y);
        sample(sx, sy)
    });
    let (kx, ky) = (ow as f32 / dw as f32, oh as f32 / dh as f32);
    let faces = pool.detect(&frame, opts.min_confidence)?;
    Ok(faces
        .into_iter()
        .map(|f| {
            f.map(|p| orientation::displayed_to_stored(orient, sw as f32, sh as f32, Point::new(p.x * kx, p.y * ky)))
        })
        .collect())
}

pub(crate) fn paint(img: &mut DynamicImage, regions: &[Region], opts: &FaceOptions) -> Result<()> {
    let (w, h) = (img.width() as usize, img.height() as usize);
    let (style, k) = (opts.style, opts.strength);
    macro_rules! go {
        ($buf:expr, $ty:ty, $step:expr, $ch:expr) => {{
            let mut p = Plane::<$ty>::new($buf, w, h, w * $step, $step, $ch);
            redact::paint(&mut p, regions, style, k, &[0; 4]);
        }};
    }
    match img {
        DynamicImage::ImageLuma8(b) => go!(b, u8, 1, 1),
        DynamicImage::ImageLumaA8(b) => go!(b, u8, 2, 1),
        DynamicImage::ImageRgb8(b) => go!(b, u8, 3, 3),
        DynamicImage::ImageRgba8(b) => go!(b, u8, 4, 3),
        DynamicImage::ImageLuma16(b) => go!(b, u16, 1, 1),
        DynamicImage::ImageLumaA16(b) => go!(b, u16, 2, 1),
        DynamicImage::ImageRgb16(b) => go!(b, u16, 3, 3),
        DynamicImage::ImageRgba16(b) => go!(b, u16, 4, 3),
        _ => return Err(Error::Unsupported("floating-point images cannot be redacted".into())),
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn encode(
    input: &[u8],
    format: ImageFormat,
    img: &DynamicImage,
    covered: &(dyn Fn(usize, usize) -> bool + Sync),
    bounds: (u32, u32, u32, u32),
    plan: Plan,
    ch: &mut Changes,
    notes: &mut Vec<String>,
) -> Result<Vec<u8>> {
    let (w, h) = img.dimensions();
    match format {
        ImageFormat::Jpeg => encode_jpeg(input, img, covered, bounds, plan, ch, notes),
        ImageFormat::Png => {
            let mut out = Vec::new();
            image::codecs::png::PngEncoder::new(&mut out).write_image(img.as_bytes(), w, h, img.color().into())?;
            notes.push("PNG re-encoded losslessly".into());
            png::rebuild(input, &out, plan, ch)
        }
        ImageFormat::Webp => {
            let rgba = img.color().has_alpha();
            let mut out = Vec::new();
            let enc = image::codecs::webp::WebPEncoder::new_lossless(&mut out);
            if rgba {
                enc.write_image(img.to_rgba8().as_raw(), w, h, image::ExtendedColorType::Rgba8)?;
            } else {
                enc.write_image(img.to_rgb8().as_raw(), w, h, image::ExtendedColorType::Rgb8)?;
            }
            notes.push("WebP re-encoded as lossless WebP".into());
            webp::rebuild(input, &out, w, h, rgba, plan, ch)
        }
        ImageFormat::Tiff | ImageFormat::Bmp => {
            let fmt = if format == ImageFormat::Tiff { image::ImageFormat::Tiff } else { image::ImageFormat::Bmp };
            let mut out = Cursor::new(Vec::new());
            img.write_to(&mut out, fmt)?;
            if format == ImageFormat::Tiff {
                ch.removed("all TIFF tags except the ones describing the pixels");
            }
            notes.push(format!("{} re-encoded losslessly", format.name()));
            Ok(out.into_inner())
        }
    }
}

fn encode_jpeg(
    input: &[u8],
    img: &DynamicImage,
    covered: &(dyn Fn(usize, usize) -> bool + Sync),
    bounds: (u32, u32, u32, u32),
    plan: Plan,
    ch: &mut Changes,
    notes: &mut Vec<String>,
) -> Result<Vec<u8>> {
    let original = jpeg::parse(input)?;
    let (w, h) = (img.width() as usize, img.height() as usize);
    let (data, channels): (&[u8], usize) = match img {
        DynamicImage::ImageRgb8(b) => (b.as_raw(), 3),
        DynamicImage::ImageLuma8(b) => (b.as_raw(), 1),
        _ => (&[], 0),
    };
    let b = (bounds.0 as usize, bounds.1 as usize, bounds.2 as usize, bounds.3 as usize);
    let pixels = jpeg_dct::Pixels { width: w, height: h, channels, data };

    match jpeg_dct::redact(input, original.is_progressive(), &pixels, covered, b) {
        Ok((encoded, stats)) => {
            let pct = 100.0 * stats.blocks_changed as f64 / stats.blocks_total.max(1) as f64;
            notes.push(format!(
                "JPEG edited in the DCT domain: {} of {} blocks re-encoded ({pct:.2}%), the rest are bit-identical",
                stats.blocks_changed, stats.blocks_total
            ));
            jpeg::rebuild(&original, &jpeg::parse(&encoded)?, plan, ch, true)
        }
        Err(e @ (Error::Unsupported(_) | Error::Codec(_))) => {
            // Rare color spaces (CMYK/YCCK/RGB JPEG) or coding modes libjpeg was
            // built without (arithmetic coding): full re-encode at maximum quality.
            let why = e.to_string();
            tracing::warn!("{why}: falling back to a full JPEG re-encode");
            ch.warn(format!("{why}: JPEG fully re-encoded at quality 100 (not lossless)"));
            let mut out = Vec::new();
            let rgb = img.to_rgb8();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 100).write_image(
                rgb.as_raw(),
                rgb.width(),
                rgb.height(),
                image::ExtendedColorType::Rgb8,
            )?;
            jpeg::rebuild(&original, &jpeg::parse(&out)?, plan, ch, false)
        }
        Err(e) => Err(e),
    }
}
