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

/// Convert refuses to overwrite an existing output by default; the file is left
/// untouched (the guard runs before any read or encode). Before the fix it was
/// silently overwritten.
#[test]
fn convert_refuses_to_overwrite_by_default() {
    let dir = tempfile::tempdir().expect("in dir");
    let out = tempfile::tempdir().expect("out dir");
    fs::write(dir.path().join("a.ppm"), ppm_1x1([255, 0, 0])).expect("write a");
    let sentinel = b"SENTINEL".to_vec();
    fs::write(out.path().join("a.webp"), &sentinel).expect("pre-create webp");

    Command::cargo_bin("webp")
        .expect("binary builds")
        .args(["convert"])
        .arg(dir.path())
        .arg("-o")
        .arg(out.path())
        .assert()
        .failure();

    assert_eq!(
        fs::read(out.path().join("a.webp")).expect("read"),
        sentinel,
        "the existing output must be left untouched"
    );
}

/// `--force` overwrites the existing output.
#[test]
fn convert_force_overwrites() {
    let dir = tempfile::tempdir().expect("in dir");
    let out = tempfile::tempdir().expect("out dir");
    fs::write(dir.path().join("a.ppm"), ppm_1x1([255, 0, 0])).expect("write a");
    let sentinel = b"SENTINEL".to_vec();
    fs::write(out.path().join("a.webp"), &sentinel).expect("pre-create webp");

    Command::cargo_bin("webp")
        .expect("binary builds")
        .args(["convert", "--force"])
        .arg(dir.path())
        .arg("-o")
        .arg(out.path())
        .assert()
        .success();

    assert_ne!(
        fs::read(out.path().join("a.webp")).expect("read"),
        sentinel,
        "--force must overwrite the existing output"
    );
}

/// `--no-clobber` skips an existing output and still exits 0.
#[test]
fn convert_no_clobber_skips_and_exits_zero() {
    let dir = tempfile::tempdir().expect("in dir");
    let out = tempfile::tempdir().expect("out dir");
    fs::write(dir.path().join("a.ppm"), ppm_1x1([255, 0, 0])).expect("write a");
    let sentinel = b"SENTINEL".to_vec();
    fs::write(out.path().join("a.webp"), &sentinel).expect("pre-create webp");

    Command::cargo_bin("webp")
        .expect("binary builds")
        .args(["convert", "--no-clobber"])
        .arg(dir.path())
        .arg("-o")
        .arg(out.path())
        .assert()
        .success();

    assert_eq!(
        fs::read(out.path().join("a.webp")).expect("read"),
        sentinel,
        "--no-clobber must leave the existing output intact"
    );
}
