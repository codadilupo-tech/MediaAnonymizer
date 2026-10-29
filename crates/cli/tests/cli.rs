//! Command-line behaviour tests.

use assert_cmd::Command;
use std::path::Path;

const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../core/tests/fixtures/astronaut_gps.jpg");

fn mediaanon() -> Command {
    Command::cargo_bin("mediaanon").unwrap()
}

fn copy_fixture(dir: &Path, name: &str) -> std::path::PathBuf {
    let p = dir.join(name);
    std::fs::copy(FIXTURE, &p).unwrap();
    p
}

#[test]
fn refuses_to_do_nothing() {
    let out = mediaanon().args(["process", FIXTURE]).assert().failure();
    let stderr = String::from_utf8_lossy(&out.get_output().stderr).to_string();
    assert!(stderr.contains("nothing to do"), "{stderr}");
}

#[test]
fn writes_next_to_input_and_skips_existing() {
    let dir = tempfile::tempdir().unwrap();
    let input = copy_fixture(dir.path(), "photo.jpg");
    mediaanon().args(["process", "--strip-metadata"]).arg(&input).assert().success();
    let output = dir.path().join("photo.anon.jpg");
    let bytes = std::fs::read(&output).unwrap();
    assert!(!bytes.windows(8).any(|w| w == b"Jane Doe"));

    let again = mediaanon().args(["process", "-m", "-q"]).arg(&input).assert().success();
    assert!(String::from_utf8_lossy(&again.get_output().stderr).is_empty(), "quiet mode prints nothing");
    let again = mediaanon().args(["process", "-m"]).arg(&input).assert().success();
    assert!(String::from_utf8_lossy(&again.get_output().stderr).contains("skipped"));
}

#[test]
fn json_report_and_inspect() {
    let dir = tempfile::tempdir().unwrap();
    let input = copy_fixture(dir.path(), "photo.jpg");
    let out = dir.path().join("clean.jpg");
    let res = mediaanon()
        .args(["--json", "process", "--strip-sensitive", "--faces", "--face-style", "blur", "-o"])
        .arg(&out)
        .arg(&input)
        .assert()
        .success();
    let line = String::from_utf8_lossy(&res.get_output().stdout).to_string();
    let report: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(report["faces"], 1);
    assert_eq!(report["kind"]["kind"], "image");

    let res = mediaanon().args(["--json", "inspect"]).arg(&out).assert().success();
    let v: serde_json::Value = serde_json::from_slice(&res.get_output().stdout).unwrap();
    let findings = v["metadata"].as_array().unwrap();
    assert!(findings.iter().all(|f| f["sensitive"] == false), "{findings:?}");
}

#[test]
fn directory_mode_mirrors_tree() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("src");
    std::fs::create_dir_all(src.join("nested")).unwrap();
    copy_fixture(&src, "a.jpg");
    copy_fixture(&src.join("nested"), "b.jpg");
    let dst = dir.path().join("dst");
    mediaanon().args(["process", "-m", "-r", "-o"]).arg(&dst).arg(&src).assert().success();
    assert!(dst.join("a.jpg").is_file());
    assert!(dst.join("nested/b.jpg").is_file());
}

#[test]
fn dry_run_writes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let input = copy_fixture(dir.path(), "photo.jpg");
    let res = mediaanon().args(["process", "-m", "--dry-run"]).arg(&input).assert().success();
    assert!(String::from_utf8_lossy(&res.get_output().stdout).contains("photo.anon.jpg"));
    assert!(!dir.path().join("photo.anon.jpg").exists());
}

#[test]
fn timeline_file_is_validated() {
    let dir = tempfile::tempdir().unwrap();
    let input = copy_fixture(dir.path(), "photo.jpg");
    let bad = dir.path().join("bad.json");
    std::fs::write(&bad, r#"{"keyframes":[{"time":-1}]}"#).unwrap();
    let out = mediaanon().args(["process", "-m", "--timeline"]).arg(&bad).arg(&input).assert().failure();
    assert!(String::from_utf8_lossy(&out.get_output().stderr).contains("invalid timeline"));

    // Photos ignore the timeline.
    let good = dir.path().join("good.json");
    std::fs::write(&good, r#"{"keyframes":[{"time":2.5,"cut":true}]}"#).unwrap();
    mediaanon().args(["process", "-m", "--timeline"]).arg(&good).arg(&input).assert().success();
}
