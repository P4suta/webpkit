//! Integration tests for `webp convert` (bulk / directory conversion).
#![allow(
    clippy::expect_used,
    reason = "integration-test helpers live outside #[test] fns; failures are bugs"
)]

use std::fs;

use assert_cmd::Command;

/// A 1x1 binary PPM (P6) of a single RGB pixel.
fn ppm_1x1(rgb: [u8; 3]) -> Vec<u8> {
    let mut bytes = b"P6\n1 1\n255\n".to_vec();
    bytes.extend_from_slice(&rgb);
    bytes
}

#[test]
fn convert_a_directory_of_ppms_to_webp() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = tempfile::tempdir().expect("out dir");
    fs::write(dir.path().join("a.ppm"), ppm_1x1([255, 0, 0])).expect("write a");
    fs::write(dir.path().join("b.ppm"), ppm_1x1([0, 255, 0])).expect("write b");
    // A non-image file must be ignored, not fail the run.
    fs::write(dir.path().join("notes.txt"), b"ignore me").expect("write txt");

    Command::cargo_bin("webp")
        .expect("binary builds")
        .args(["convert"])
        .arg(dir.path())
        .arg("-o")
        .arg(out.path())
        .assert()
        .success();

    assert!(out.path().join("a.webp").is_file(), "a.webp missing");
    assert!(out.path().join("b.webp").is_file(), "b.webp missing");
}

#[test]
fn convert_optimize_produces_a_valid_webp() {
    let dir = tempfile::tempdir().expect("temp dir");
    let out = tempfile::tempdir().expect("out dir");
    fs::write(dir.path().join("x.ppm"), ppm_1x1([1, 2, 3])).expect("write x");

    Command::cargo_bin("webp")
        .expect("binary builds")
        .args(["convert", "--optimize"])
        .arg(dir.path())
        .arg("-o")
        .arg(out.path())
        .assert()
        .success();

    let webp = fs::read(out.path().join("x.webp")).expect("read webp");
    assert!(webp.starts_with(b"RIFF"), "output must be a WebP");
}
