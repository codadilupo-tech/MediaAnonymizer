//! Exercises the C ABI the way the Flutter app does: C strings in, JSON out.

use mediaanon_mobile::*;
use serde_json::Value;
use std::ffi::{c_char, CStr, CString};
use std::path::{Path, PathBuf};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../core/tests/fixtures/astronaut_gps.jpg")
}

fn c(s: &str) -> CString {
    CString::new(s).unwrap()
}

fn take(ptr: *mut c_char) -> Value {
    assert!(!ptr.is_null());
    let text = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap().to_owned();
    unsafe { mediaanon_string_free(ptr) };
    serde_json::from_str(&text).unwrap()
}

fn path_arg(p: &Path) -> CString {
    c(p.to_str().unwrap())
}

const FACES_AND_METADATA: &str = r#"{"metadata": "all", "faces": {"style": "pixelate"}, "compute": {"device": "cpu"}}"#;

#[test]
fn version_is_the_crate_version() {
    let ptr = mediaanon_version();
    let v = unsafe { CStr::from_ptr(ptr) }.to_str().unwrap().to_owned();
    unsafe { mediaanon_string_free(ptr) };
    assert_eq!(v, env!("CARGO_PKG_VERSION"));
}

#[test]
fn inspect_flags_gps() {
    let out = take(unsafe { mediaanon_inspect(path_arg(&fixture()).as_ptr()) });
    assert_eq!(out["ok"]["format"], "JPEG");
    let findings = out["ok"]["findings"].as_array().unwrap();
    assert!(findings.iter().any(|f| f["sensitive"] == true && f["detail"].as_str().unwrap().contains("GPS")), "{out}");
}

#[test]
fn process_strips_metadata_and_redacts_faces() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.jpg");
    let out = take(unsafe {
        mediaanon_process(c(FACES_AND_METADATA).as_ptr(), path_arg(&fixture()).as_ptr(), path_arg(&output).as_ptr(), 0)
    });
    let report = &out["ok"];
    assert!(report["faces"].as_u64().unwrap() >= 1, "{out}");
    assert!(output.is_file());

    let after = take(unsafe { mediaanon_inspect(path_arg(&output).as_ptr()) });
    let findings = after["ok"]["findings"].as_array().unwrap();
    assert!(findings.iter().all(|f| f["sensitive"] == false), "{after}");
}

#[test]
fn preview_writes_a_jpeg() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("preview.jpg");
    let out = take(unsafe {
        mediaanon_preview(
            c(FACES_AND_METADATA).as_ptr(),
            path_arg(&fixture()).as_ptr(),
            path_arg(&output).as_ptr(),
            256,
        )
    });
    assert!(out["ok"]["faces"].as_u64().unwrap() >= 1, "{out}");
    assert!(out["ok"]["width"].as_u64().unwrap().max(out["ok"]["height"].as_u64().unwrap()) <= 256);
    assert_eq!(&std::fs::read(&output).unwrap()[..3], &[0xFF, 0xD8, 0xFF]);
}

#[test]
fn cancelled_before_start() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("out.jpg");
    let handle = mediaanon_cancel_new();
    assert_ne!(handle, 0);
    mediaanon_cancel(handle);
    let out = take(unsafe {
        mediaanon_process(
            c(r#"{"metadata": "all"}"#).as_ptr(),
            path_arg(&fixture()).as_ptr(),
            path_arg(&output).as_ptr(),
            handle,
        )
    });
    mediaanon_cancel_free(handle);
    assert_eq!(out["error"]["kind"], "cancelled", "{out}");
    assert!(!output.exists());
}

#[test]
fn errors_are_reported_as_json() {
    let out = take(unsafe { mediaanon_inspect(std::ptr::null()) });
    assert_eq!(out["error"]["kind"], "invalid-argument");

    let out = take(unsafe {
        mediaanon_process(c("{}").as_ptr(), path_arg(&fixture()).as_ptr(), c("/nonexistent/out.jpg").as_ptr(), 0)
    });
    assert_eq!(out["error"]["kind"], "invalid-options", "{out}");

    let out = take(unsafe { mediaanon_process(c("not json").as_ptr(), c("a").as_ptr(), c("b").as_ptr(), 0) });
    assert_eq!(out["error"]["kind"], "invalid-options", "{out}");
}

#[test]
fn heic_and_videos_are_rejected_clearly() {
    let dir = tempfile::tempdir().unwrap();
    let heic = dir.path().join("IMG_0001.HEIC");
    std::fs::write(&heic, b"\0\0\0\x18ftypheic\0\0\0\0mif1heic").unwrap();
    let out = take(unsafe { mediaanon_inspect(path_arg(&heic).as_ptr()) });
    assert_eq!(out["error"]["kind"], "unsupported");
    assert!(out["error"]["message"].as_str().unwrap().contains("HEIC"), "{out}");

    let mp4 = dir.path().join("clip.mp4");
    std::fs::write(&mp4, b"\0\0\0\x18ftypisom\0\0\0\0isomiso2").unwrap();
    let out = take(unsafe { mediaanon_inspect(path_arg(&mp4).as_ptr()) });
    assert!(out["error"]["message"].as_str().unwrap().contains("videos"), "{out}");
}
