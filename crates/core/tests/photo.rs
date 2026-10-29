//! End-to-end tests of the photo pipeline on a real portrait.

mod common;

use common::*;
use image::ImageEncoder;
use mediaanon_core::metadata::jpeg;
use mediaanon_core::{inspect, FaceMode, FaceOptions, MetadataPolicy, RedactionStyle};

/// Pixels farther than this from the redaction bounds must be untouched
/// (JPEG chroma upsampling may blur one block around edited blocks).
const MARGIN: u32 = 24;

fn decode(path: &std::path::Path) -> image::RgbImage {
    image::open(path).expect("decodable output").to_rgb8()
}

/// Bounding box of the pixels that differ between two images.
fn changed_box(a: &image::RgbImage, b: &image::RgbImage) -> Option<(u32, u32, u32, u32)> {
    let mut bb: Option<(u32, u32, u32, u32)> = None;
    for (x, y, p) in a.enumerate_pixels() {
        if p != b.get_pixel(x, y) {
            bb = Some(match bb {
                None => (x, y, x, y),
                Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
            });
        }
    }
    bb
}

#[test]
fn strip_all_keeps_jpeg_image_data_bit_exact() {
    let (_d, dir) = tmp();
    let out = dir.join("out.jpg");
    let report = run(options(MetadataPolicy::All, None), FIXTURE.as_ref(), &out);
    let input = fixture_bytes();
    let output = std::fs::read(&out).unwrap();
    assert_eq!(jpeg::parse(&input).unwrap().scan, jpeg::parse(&output).unwrap().scan);
    assert!(!contains(&output, b"Jane Doe") && !contains(&output, b"TestCam") && !contains(&output, b"shot by"));
    assert!(inspect(&out).unwrap().iter().all(|f| !f.sensitive));
    assert!(!report.removed.is_empty());
}

#[test]
fn strip_sensitive_keeps_camera_model() {
    let (_d, dir) = tmp();
    let out = dir.join("out.jpg");
    run(options(MetadataPolicy::Sensitive, None), FIXTURE.as_ref(), &out);
    let findings = inspect(&out).unwrap();
    assert!(findings.iter().any(|f| f.detail == "Model: Model X"));
    assert!(findings.iter().all(|f| !f.sensitive), "{findings:?}");
}

#[test]
fn jpeg_faces_are_redacted_and_the_rest_is_untouched() {
    let (_d, dir) = tmp();
    for style in [RedactionStyle::BlackBand, RedactionStyle::Blur, RedactionStyle::Pixelate] {
        let out = dir.join(format!("{style}.jpg"));
        let faces = FaceOptions { style, ..Default::default() };
        let report = run(options(MetadataPolicy::Keep, Some(faces)), FIXTURE.as_ref(), &out);
        assert_eq!(report.faces, 1, "{style}: the astronaut must be found");
        assert!(report.notes.iter().any(|n| n.contains("DCT domain")), "{:?}", report.notes);

        let (a, b) = (decode(FIXTURE.as_ref()), decode(&out));
        let (x0, y0, x1, y1) = changed_box(&a, &b).expect("pixels changed");
        // The face of the fixture is roughly at x 170-290, y 40-200.
        assert!(x0 > 100 && x1 < 360 && y1 < 260, "{style}: change confined to the face, got {x0},{y0}-{x1},{y1}");
        for (x, y, p) in a.enumerate_pixels() {
            let far = x + MARGIN < x0 || x > x1 + MARGIN || y + MARGIN < y0 || y > y1 + MARGIN;
            if far {
                assert_eq!(p, b.get_pixel(x, y), "{style}: pixel ({x},{y}) outside the face changed");
            }
        }
        // Metadata untouched with the `keep` policy (except previews).
        assert!(contains(&std::fs::read(&out).unwrap(), b"Jane Doe"));
    }
}

#[test]
fn black_band_actually_hides_the_face() {
    let (_d, dir) = tmp();
    let out = dir.join("black.jpg");
    run(options(MetadataPolicy::All, Some(FaceOptions::default())), FIXTURE.as_ref(), &out);
    let img = decode(&out);
    // Center of the face (between nose and eyes) must be black.
    let p = img.get_pixel(232, 120);
    assert!(p.0.iter().all(|&c| c < 30), "expected black, got {p:?}");
}

#[test]
fn eyes_only_leaves_the_mouth_visible() {
    let (_d, dir) = tmp();
    let out = dir.join("eyes.jpg");
    let faces = FaceOptions { mode: FaceMode::EyesOnly, ..Default::default() };
    run(options(MetadataPolicy::All, Some(faces)), FIXTURE.as_ref(), &out);
    let (a, b) = (decode(FIXTURE.as_ref()), decode(&out));
    let (_, y0, _, y1) = changed_box(&a, &b).unwrap();
    assert!(y1 - y0 < 70, "the band must be thin, got {y0}-{y1}");
}

#[test]
fn png_is_lossless_and_metadata_is_filtered() {
    let (_d, dir) = tmp();
    let src = decode(FIXTURE.as_ref());
    let input = dir.join("in.png");
    let mut png = Vec::new();
    image::codecs::png::PngEncoder::new(&mut png)
        .write_image(src.as_raw(), src.width(), src.height(), image::ExtendedColorType::Rgb8)
        .unwrap();
    // Insert a tEXt Author chunk after IHDR (8 + 25 bytes).
    let mut chunk = Vec::new();
    let body = b"tEXtAuthor\0Jane Doe";
    chunk.extend_from_slice(&((body.len() - 4) as u32).to_be_bytes());
    chunk.extend_from_slice(body);
    chunk.extend_from_slice(&crc32(body).to_be_bytes());
    png.splice(33..33, chunk);
    std::fs::write(&input, &png).unwrap();

    let out = dir.join("out.png");
    let report = run(options(MetadataPolicy::Sensitive, Some(FaceOptions::default())), &input, &out);
    assert_eq!(report.faces, 1);
    let bytes = std::fs::read(&out).unwrap();
    assert!(!contains(&bytes, b"Jane Doe"));
    let b = decode(&out);
    let (x0, y0, x1, y1) = changed_box(&src, &b).unwrap();
    for (x, y, p) in src.enumerate_pixels() {
        if x < x0 || x > x1 || y < y0 || y > y1 {
            assert_eq!(p, b.get_pixel(x, y), "PNG must be lossless outside the face");
        }
    }
}

#[test]
fn webp_output_is_lossless() {
    let (_d, dir) = tmp();
    let src = decode(FIXTURE.as_ref());
    let input = dir.join("in.webp");
    src.save(&input).unwrap();
    let out = dir.join("out.webp");
    let report = run(options(MetadataPolicy::All, Some(FaceOptions::default())), &input, &out);
    assert_eq!(report.faces, 1);
    let (a, b) = (decode(&input), decode(&out));
    let (x0, _, x1, _) = changed_box(&a, &b).unwrap();
    assert!(a.enumerate_pixels().all(|(x, y, p)| (x0..=x1).contains(&x) || p == b.get_pixel(x, y)));
}

#[test]
fn exif_orientation_is_honoured() {
    // Store the portrait rotated by 90° counter-clockwise and declare
    // orientation 6 (rotate 90° clockwise to display): the face lies on its
    // side in the stored pixels and is only found if orientation is applied.
    let (_d, dir) = tmp();
    let src = image::DynamicImage::ImageRgb8(decode(FIXTURE.as_ref())).rotate270();
    let mut jpg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpg, 92)
        .write_image(src.as_bytes(), src.width(), src.height(), image::ExtendedColorType::Rgb8)
        .unwrap();
    let mut app1 = vec![0xFF, 0xE1, 0, 0];
    app1.extend_from_slice(b"Exif\0\0II*\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0\x06\0\0\0\0\0\0\0");
    let len = (app1.len() - 2) as u16;
    app1[2..4].copy_from_slice(&len.to_be_bytes());
    jpg.splice(2..2, app1);
    let input = dir.join("rotated.jpg");
    std::fs::write(&input, &jpg).unwrap();

    let out = dir.join("out.jpg");
    let report = run(options(MetadataPolicy::Keep, Some(FaceOptions::default())), &input, &out);
    assert_eq!(report.faces, 1, "face must be detected on the upright image");
    let (a, b) = (decode(&input), decode(&out));
    assert_eq!(a.dimensions(), b.dimensions(), "stored orientation unchanged");
    // In stored coordinates the face is on the left half (rotated CCW).
    let (x0, _, x1, _) = changed_box(&a, &b).unwrap();
    assert!(x1 < a.width() / 2 + 40 && x0 < x1);
}

#[test]
fn image_without_faces_is_copied_unchanged() {
    let (_d, dir) = tmp();
    let input = dir.join("flat.png");
    image::RgbImage::from_pixel(64, 64, image::Rgb([90, 120, 150])).save(&input).unwrap();
    let out = dir.join("out.png");
    let report = run(options(MetadataPolicy::Keep, Some(FaceOptions::default())), &input, &out);
    assert_eq!(report.faces, 0);
    assert_eq!(std::fs::read(&input).unwrap(), std::fs::read(&out).unwrap());
}

#[test]
fn failed_runs_leave_no_partial_file() {
    let (_d, dir) = tmp();
    let input = dir.join("broken.jpg");
    std::fs::write(&input, [0xFF, 0xD8, 0xFF, 0xE0, 0, 0]).unwrap();
    let out = dir.join("out.jpg");
    let p = mediaanon_core::Processor::new(options(MetadataPolicy::All, None)).unwrap();
    assert!(p.process_file(&input, &out, &|_| {}, &mediaanon_core::CancelToken::new()).is_err());
    let leftovers: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(leftovers, vec![std::ffi::OsString::from("broken.jpg")]);
}

fn crc32(data: &[u8]) -> u32 {
    let mut h = crc32fast::Hasher::new();
    h.update(data);
    h.finalize()
}

/// JPEG of the fixture whose only metadata is an IFD0 `DateTime` and, if
/// given, an orientation tag.
fn dated_jpeg(dir: &std::path::Path, orientation: Option<u16>, rotate: bool) -> std::path::PathBuf {
    let mut src = image::DynamicImage::ImageRgb8(decode(FIXTURE.as_ref()));
    if rotate {
        src = src.rotate270();
    }
    let mut jpg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpg, 92)
        .write_image(src.as_bytes(), src.width(), src.height(), image::ExtendedColorType::Rgb8)
        .unwrap();
    // TIFF: header, IFD0 with DateTime (ASCII, 20 bytes at offset 38) [+ Orientation].
    let n: u16 = if orientation.is_some() { 2 } else { 1 };
    let data_off = 8 + 2 + 12 * n as u32 + 4;
    let mut tiff = b"II*\0\x08\0\0\0".to_vec();
    tiff.extend_from_slice(&n.to_le_bytes());
    tiff.extend_from_slice(&[0x32, 0x01, 2, 0, 20, 0, 0, 0]);
    tiff.extend_from_slice(&data_off.to_le_bytes());
    if let Some(o) = orientation {
        tiff.extend_from_slice(&[0x12, 0x01, 3, 0, 1, 0, 0, 0]);
        tiff.extend_from_slice(&(o as u32).to_le_bytes());
    }
    tiff.extend_from_slice(&[0, 0, 0, 0]);
    tiff.extend_from_slice(b"2019:07:20 20:17:40\0");
    let mut app1 = vec![0xFF, 0xE1, 0, 0];
    app1.extend_from_slice(b"Exif\0\0");
    app1.extend(tiff);
    let len = (app1.len() - 2) as u16;
    app1[2..4].copy_from_slice(&len.to_be_bytes());
    jpg.splice(2..2, app1);
    let p = dir.join(format!("dated-{orientation:?}.jpg"));
    std::fs::write(&p, &jpg).unwrap();
    p
}

#[test]
fn timestamp_is_drawn_in_the_chosen_corner() {
    use mediaanon_core::{Corner, Options, TimestampOptions};
    let (_d, dir) = tmp();
    let input = dated_jpeg(&dir, None, false);
    for corner in [Corner::BottomLeft, Corner::BottomRight] {
        let out = dir.join(format!("{corner}.jpg"));
        let opts = Options {
            metadata: MetadataPolicy::All,
            timestamp: Some(TimestampOptions { position: corner, ..Default::default() }),
            ..Default::default()
        };
        let report = run(opts, &input, &out);
        assert_eq!(report.timestamp.as_deref(), Some("2019-07-20 20:17:40 (EXIF DateTime)"));
        assert!(!contains(&std::fs::read(&out).unwrap(), b"2019:07:20"), "the date tag itself is removed");
        let (a, b) = (decode(&input), decode(&out));
        let (x0, y0, x1, _) = changed_box(&a, &b).expect("text drawn");
        assert!(y0 > 512 * 3 / 4, "{corner}: text at the bottom, got y0={y0}");
        match corner {
            Corner::BottomLeft => assert!(x1 < 400 && x0 < 40, "{corner}: {x0}-{x1}"),
            Corner::BottomRight => assert!(x0 > 112 && x1 > 470, "{corner}: {x0}-{x1}"),
        }
    }
}

#[test]
fn timestamp_follows_exif_orientation() {
    use mediaanon_core::{Options, TimestampOptions};
    let (_d, dir) = tmp();
    // Stored rotated 90° CCW, orientation 6: the displayed bottom-right
    // corner is the stored top-right one... after rotating back, the text must
    // read upright at the displayed bottom-right.
    let input = dated_jpeg(&dir, Some(6), true);
    let out = dir.join("out.jpg");
    let opts = Options { timestamp: Some(TimestampOptions::default()), ..Default::default() };
    run(opts, &input, &out);
    let (mut a, mut b) = (image::open(&input).unwrap(), image::open(&out).unwrap());
    a.apply_orientation(image::metadata::Orientation::Rotate90);
    b.apply_orientation(image::metadata::Orientation::Rotate90);
    let (x0, y0, ..) = changed_box(&a.to_rgb8(), &b.to_rgb8()).unwrap();
    assert!(y0 > 512 * 3 / 4 && x0 > 112, "upright bottom-right, got {x0},{y0}");
}
