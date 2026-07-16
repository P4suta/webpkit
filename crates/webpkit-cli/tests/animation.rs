//! Integration tests for animation decoding, driven by a committed fixture.
#![allow(
    clippy::expect_used,
    reason = "integration-test helpers live outside #[test] fns; failures are bugs"
)]

use assert_cmd::Command;
use predicates::str::contains;

/// A committed 16x16, 3-frame animation from the conformance fixtures.
const ANIM: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../webpkit-lossless-conformance/fixtures/decode/animation_frames/input.webp"
);

fn webp() -> Command {
    Command::cargo_bin("webp").expect("binary builds")
}

#[test]
fn info_reports_the_animation_frame_count() {
    webp()
        .args(["info", ANIM])
        .assert()
        .success()
        .stdout(contains("animation"))
        .stdout(contains("Frames:     3"));
}

#[test]
fn decode_frames_all_writes_numbered_files() {
    let out = tempfile::tempdir().expect("out dir");
    let target = out.path().join("frame.png");
    webp()
        .args(["decode", ANIM, "--frames", "all", "-o"])
        .arg(&target)
        .assert()
        .success();

    for index in 0..3 {
        let path = out.path().join(format!("frame-{index:03}.png"));
        assert!(path.is_file(), "missing {}", path.display());
    }
}

#[test]
fn decode_single_frame_writes_one_file() {
    let out = tempfile::tempdir().expect("out dir");
    let target = out.path().join("one.png");
    webp()
        .args(["decode", ANIM, "--frame", "1", "-o"])
        .arg(&target)
        .assert()
        .success();
    assert!(target.is_file());
}

#[test]
fn decode_out_of_range_frame_is_a_usage_error() {
    let out = tempfile::tempdir().expect("out dir");
    let target = out.path().join("x.png");
    webp()
        .args(["decode", ANIM, "--frame", "99", "-o"])
        .arg(&target)
        .assert()
        .code(2);
}

/// `--layout` reached the still path and not the animation path, so
/// `webp decode anim.webp --format raw --layout bgra8` wrote RGBA bytes and
/// exited 0. Nothing caught it: the two paths took different arguments, so the
/// compiler could not notice the omission and no test compared them.
#[test]
fn layout_is_honored_for_animations_as_it_is_for_stills() {
    let dir = tempfile::tempdir().expect("tempdir");
    let raw = |name: &str, layout: &str| {
        let out = dir.path().join(name);
        webp()
            .args(["decode", ANIM, "--frame", "0", "--format", "raw"])
            .args(["--layout", layout])
            .arg("-o")
            .arg(&out)
            .assert()
            .success();
        std::fs::read(&out).expect("read output")
    };

    let rgba = raw("rgba.raw", "rgba8");
    let bgra = raw("bgra.raw", "bgra8");

    assert_eq!(rgba.len(), bgra.len(), "same pixels, different order");
    assert_ne!(rgba, bgra, "--layout bgra8 must not emit rgba8 bytes");
    // Byte 0 is R in rgba8 and B in bgra8: the first pixel's channels swap.
    assert_eq!(rgba[0], bgra[2], "R and B are exchanged");
    assert_eq!(rgba[2], bgra[0], "R and B are exchanged");
    assert_eq!(rgba[3], bgra[3], "alpha stays put");
}
