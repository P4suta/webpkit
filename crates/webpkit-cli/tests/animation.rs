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
