//! Helpers shared by the integration tests.
#![allow(dead_code)]

use mediaanon_core::{CancelToken, FaceOptions, MetadataPolicy, Options, Processor, Report};
use std::path::{Path, PathBuf};

pub const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/astronaut_gps.jpg");

pub fn fixture_bytes() -> Vec<u8> {
    std::fs::read(FIXTURE).expect("fixture")
}

pub fn options(policy: MetadataPolicy, faces: Option<FaceOptions>) -> Options {
    Options { metadata: policy, faces, ..Default::default() }
}

pub fn run(opts: Options, input: &Path, output: &Path) -> Report {
    Processor::new(opts).expect("processor").process_file(input, output, &|_| {}, &CancelToken::new()).expect("process")
}

pub fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Whether the FFmpeg tools are installed (video tests are skipped otherwise).
///
/// CI sets `MEDIAANON_REQUIRE_FFMPEG=1` so that a missing FFmpeg fails the
/// run instead of silently skipping the video tests.
pub fn has_ffmpeg() -> bool {
    let ok = |t: &str| std::process::Command::new(t).arg("-version").output().is_ok_and(|o| o.status.success());
    let found = ok("ffmpeg") && ok("ffprobe");
    if !found {
        assert!(
            std::env::var_os("MEDIAANON_REQUIRE_FFMPEG").is_none(),
            "ffmpeg/ffprobe not found but MEDIAANON_REQUIRE_FFMPEG is set"
        );
        eprintln!("ffmpeg/ffprobe not found: skipping video test");
    }
    found
}

pub fn ffmpeg(args: &[&str]) -> Vec<u8> {
    let out = std::process::Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y"])
        .args(args)
        .output()
        .expect("ffmpeg");
    assert!(out.status.success(), "ffmpeg {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    out.stdout
}

pub fn ffprobe_json(path: &Path) -> serde_json::Value {
    let out = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-print_format", "json", "-show_format", "-show_streams"])
        .arg(path)
        .output()
        .expect("ffprobe");
    serde_json::from_slice(&out.stdout).expect("json")
}

pub fn path_str(p: &Path) -> &str {
    p.to_str().expect("utf-8 path")
}

pub fn tmp() -> (tempfile::TempDir, PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().to_path_buf();
    (d, p)
}

/// Builds a short clip from the portrait: the camera pans so the face moves.
pub fn make_clip(dir: &Path, name: &str, pix_fmt: &str, codec: &str, size: &str) -> PathBuf {
    let out = dir.join(name);
    let filter =
        format!("[0:v]scale={size}:{size},crop=iw:ih*9/16:x=0:y='ih*0.05+ih*0.02*sin(t*4)',format={pix_fmt}[v]");
    ffmpeg(&[
        "-loop",
        "1",
        "-i",
        FIXTURE,
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:sample_rate=48000",
        "-t",
        "1",
        "-filter_complex",
        &filter,
        "-map",
        "[v]",
        "-map",
        "1:a",
        "-r",
        "25",
        "-c:v",
        codec,
        "-crf",
        "20",
        "-c:a",
        "aac",
        "-metadata",
        "location=+45.5000-073.2510/",
        "-metadata",
        "artist=Jane Doe",
        "-metadata",
        "title=Trip",
        "-metadata",
        "creation_time=2021-07-04T10:20:30Z",
        "-metadata:s:a:0",
        "language=ita",
        path_str(&out),
    ]);
    out
}

/// Decodes every frame to raw planes in `pix_fmt`.
pub fn raw_frames(path: &Path, pix_fmt: &str) -> Vec<u8> {
    ffmpeg(&["-i", path_str(path), "-map", "0:v:0", "-f", "rawvideo", "-pix_fmt", pix_fmt, "-"])
}
