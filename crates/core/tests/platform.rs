//! Behaviour that differs between operating systems: path handling, file
//! replacement semantics, permissions and platform-specific inference
//! back-ends. Everything here runs on Linux, macOS and Windows in CI; tests
//! that only make sense on one platform are gated with `cfg`.

mod common;

use common::*;
use mediaanon_core::{
    inspect, CancelToken, ComputeOptions, FaceOptions, InferenceDevice, MetadataPolicy, Options, Processor,
};
use std::path::Path;

fn strip_all(input: &Path, output: &Path) {
    run(options(MetadataPolicy::All, None), input, output);
}

fn faces_on(device: InferenceDevice) -> Options {
    Options {
        metadata: MetadataPolicy::Keep,
        faces: Some(FaceOptions::default()),
        compute: ComputeOptions { device, ..Default::default() },
        ..Default::default()
    }
}

#[test]
fn unicode_and_spaces_in_paths() {
    let (_d, dir) = tmp();
    let sub = dir.join("dossier été").join("写真 2024");
    std::fs::create_dir_all(&sub).unwrap();
    let input = sub.join("photo de Zoë.jpg");
    std::fs::copy(FIXTURE, &input).unwrap();
    let output = sub.join("photo de Zoë.anon.jpg");
    strip_all(&input, &output);
    assert!(!contains(&std::fs::read(&output).unwrap(), b"Jane Doe"));
    assert!(inspect(&output).unwrap().iter().all(|f| !f.sensitive));
}

#[test]
fn in_place_replaces_the_input() {
    // On Windows a rename onto an existing file needs MOVEFILE_REPLACE_EXISTING;
    // this makes sure the atomic write works over the original.
    let (_d, dir) = tmp();
    let input = dir.join("photo.jpg");
    std::fs::copy(FIXTURE, &input).unwrap();
    strip_all(&input, &input);
    assert!(!contains(&std::fs::read(&input).unwrap(), b"Jane Doe"));
    let names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names, vec![std::ffi::OsString::from("photo.jpg")], "no temporary file left behind");
}

#[test]
fn existing_output_is_replaced() {
    let (_d, dir) = tmp();
    let output = dir.join("out.jpg");
    std::fs::write(&output, b"stale").unwrap();
    strip_all(FIXTURE.as_ref(), &output);
    assert!(std::fs::read(&output).unwrap().starts_with(&[0xFF, 0xD8]));
}

#[test]
fn missing_output_directory_is_an_error_without_leftovers() {
    let (_d, dir) = tmp();
    let output = dir.join("does-not-exist").join("out.jpg");
    let p = Processor::new(options(MetadataPolicy::All, None)).unwrap();
    let err = p.process_file(FIXTURE.as_ref(), &output, &|_| {}, &CancelToken::new()).unwrap_err();
    assert!(matches!(err, mediaanon_core::Error::Io { .. }), "{err}");
    assert!(std::fs::read_dir(&dir).unwrap().next().is_none());
}

#[test]
fn cpu_detection_finds_the_face() {
    let (_d, dir) = tmp();
    let report = run(faces_on(InferenceDevice::Cpu), FIXTURE.as_ref(), &dir.join("cpu.jpg"));
    assert_eq!(report.faces, 1);
}

#[test]
fn auto_device_finds_the_face() {
    // Regression test for macOS: ONNX Runtime used to attach Core ML to every
    // session (even CPU ones), which failed at inference time.
    let (_d, dir) = tmp();
    let report = run(faces_on(InferenceDevice::Auto), FIXTURE.as_ref(), &dir.join("auto.jpg"));
    assert_eq!(report.faces, 1);
}

#[test]
fn varying_image_sizes_reuse_the_detector() {
    // Dynamic input shapes: the same sessions must handle several sizes.
    let (_d, dir) = tmp();
    let p = Processor::new(faces_on(InferenceDevice::Auto)).unwrap();
    let src = image::open(FIXTURE).unwrap();
    for side in [512u32, 384, 700] {
        let input = dir.join(format!("in{side}.png"));
        src.resize_exact(side, side, image::imageops::FilterType::Triangle).save(&input).unwrap();
        let report = p.process_file(&input, &dir.join(format!("out{side}.png")), &|_| {}, &CancelToken::new()).unwrap();
        assert_eq!(report.faces, 1, "size {side}");
    }
}

#[cfg(unix)]
#[test]
fn read_only_directory_fails_cleanly() {
    use std::os::unix::fs::PermissionsExt;
    let (_d, dir) = tmp();
    let ro = dir.join("ro");
    std::fs::create_dir(&ro).unwrap();
    std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
    if std::fs::write(ro.join("probe"), b"x").is_ok() {
        eprintln!("running as root: permissions are not enforced, skipping");
        return;
    }
    let p = Processor::new(options(MetadataPolicy::All, None)).unwrap();
    assert!(p.process_file(FIXTURE.as_ref(), &ro.join("out.jpg"), &|_| {}, &CancelToken::new()).is_err());
    std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(std::fs::read_dir(&ro).unwrap().next().is_none());
}

#[cfg(windows)]
#[test]
fn windows_verbatim_paths() {
    // `canonicalize` returns `\\?\C:\...` paths on Windows.
    let (_d, dir) = tmp();
    let dir = std::fs::canonicalize(dir).unwrap();
    assert!(dir.to_string_lossy().starts_with(r"\\?\"));
    let input = dir.join("photo.jpg");
    std::fs::copy(FIXTURE, &input).unwrap();
    strip_all(&input, &dir.join("photo.anon.jpg"));
    assert!(dir.join("photo.anon.jpg").is_file());
}

#[cfg(windows)]
#[test]
fn windows_rejects_reserved_names_cleanly() {
    let (_d, dir) = tmp();
    let p = Processor::new(options(MetadataPolicy::All, None)).unwrap();
    // `CON` is a device name on Windows: writing must fail with an error, not hang or panic.
    let out = dir.join("CON").join("out.jpg");
    assert!(p.process_file(FIXTURE.as_ref(), &out, &|_| {}, &CancelToken::new()).is_err());
}

#[cfg(target_os = "macos")]
#[test]
fn macos_coreml_is_never_used_implicitly() {
    let devices = mediaanon_core::available_devices().unwrap();
    let p = Processor::new(faces_on(InferenceDevice::Auto)).unwrap();
    assert_ne!(p.inference_device(), Some("coreml"), "auto must not pick Core ML ({devices:?})");
    let p = Processor::new(faces_on(InferenceDevice::Cpu)).unwrap();
    assert_eq!(p.inference_device(), Some("cpu"));
}
