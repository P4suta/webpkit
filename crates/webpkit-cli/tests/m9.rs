//! Integration tests for M9: the crop/resize preprocessing pipeline and the
//! target-size quality search.
//!
//! Fixtures are binary PPMs written on the fly, so nothing is committed. Crop and
//! the size search run in any build; resize needs the default `formats` feature
//! (the `image` crate's resampler), like the rest of the format tests.
#![allow(
    clippy::expect_used,
    reason = "integration-test helpers live outside #[test] fns; failures are bugs"
)]

use std::{fs, path::Path};

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt as _;
use predicates::str::contains;
use tempfile::TempDir;

fn webp() -> Command {
    Command::cargo_bin("webp").expect("binary builds")
}

fn cwebp() -> Command {
    Command::cargo_bin("cwebp").expect("binary builds")
}

/// A binary PPM (`P6`) of `w`x`h` pixels, every pixel `rgb`.
fn solid_ppm(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
    let mut bytes = format!("P6\n{w} {h}\n255\n").into_bytes();
    for _ in 0..(w * h) {
        bytes.extend_from_slice(&rgb);
    }
    bytes
}

/// A binary PPM whose pixels vary (a cheap LCG), so lossy size tracks quality —
/// a solid color compresses to nearly nothing and would not exercise the search.
fn noisy_ppm(w: u32, h: u32) -> Vec<u8> {
    let mut bytes = format!("P6\n{w} {h}\n255\n").into_bytes();
    let mut state: u32 = 0x1234_5678;
    for _ in 0..(w * h) {
        let mut byte = || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            u8::try_from(state >> 24).unwrap_or(0)
        };
        bytes.extend_from_slice(&[byte(), byte(), byte()]);
    }
    bytes
}

/// Write `bytes` to `dir/name` and return the path.
fn write(dir: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = dir.join(name);
    fs::write(&path, bytes).expect("write fixture");
    path
}

/// The `Dimensions: WxH` line `webp info` prints for a still WebP.
fn dimensions_of(path: &Path) -> String {
    let out = webp().arg("info").arg(path).assert().success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).expect("utf8");
    stdout
        .lines()
        .find_map(|l| l.strip_prefix("Dimensions:"))
        .map_or_else(
            || format!("<no dimensions in: {stdout}>"),
            |s| s.trim().to_owned(),
        )
}

// --- crop ---------------------------------------------------------------------

/// `cwebp -crop x y w h` yields a WebP with the region's dimensions — matching
/// what libwebp's cwebp produces for the same args (dimensions, not pixels).
#[test]
fn cwebp_crop_output_has_the_region_dimensions() {
    let dir = TempDir::new().expect("tempdir");
    let src = write(dir.path(), "in.ppm", &solid_ppm(20, 10, [30, 60, 90]));
    let out = dir.path().join("out.webp");
    cwebp()
        .args(["-crop", "2", "1", "8", "4"])
        .arg(&src)
        .arg("-o")
        .arg(&out)
        .assert()
        .success();
    assert_eq!(dimensions_of(&out), "8x4");
}

/// The `webp` bare form takes `--crop x,y,w,h` and produces the same dimensions.
#[test]
fn webp_bare_crop_output_has_the_region_dimensions() {
    let dir = TempDir::new().expect("tempdir");
    let src = write(dir.path(), "in.ppm", &solid_ppm(20, 10, [30, 60, 90]));
    let out = dir.path().join("in.webp");
    webp()
        .arg("--crop")
        .arg("2,1,8,4")
        .arg(&src)
        .assert()
        .success();
    assert_eq!(dimensions_of(&out), "8x4");
}

/// An out-of-bounds crop is refused before anything is written — the projection
/// rejects it, exit 2 (usage), and no output file appears.
#[test]
fn out_of_bounds_crop_fails_at_plan_time() {
    let dir = TempDir::new().expect("tempdir");
    let src = write(dir.path(), "small.ppm", &solid_ppm(16, 16, [1, 2, 3]));
    let out = dir.path().join("out.webp");
    webp()
        .arg("encode")
        .arg("--crop")
        .arg("0,0,9999,9999")
        .arg(&src)
        .arg("-o")
        .arg(&out)
        .assert()
        .code(2)
        .stderr(contains("does not fit"));
    assert!(
        !out.exists(),
        "no output should be written on a plan-time failure"
    );
}

// --- resize (needs the `formats` feature for the resampler) -------------------

/// `cwebp -resize w h` yields the requested dimensions.
#[test]
fn cwebp_resize_output_has_the_requested_dimensions() {
    let dir = TempDir::new().expect("tempdir");
    let src = write(dir.path(), "in.ppm", &noisy_ppm(20, 10));
    let out = dir.path().join("out.webp");
    cwebp()
        .args(["-resize", "10", "6"])
        .arg(&src)
        .arg("-o")
        .arg(&out)
        .assert()
        .success();
    assert_eq!(dimensions_of(&out), "10x6");
}

/// `-resize` with a `0` axis preserves the source aspect ratio: a 20x10 image
/// resized to width 10 becomes 10x5, and to height 5 becomes 10x5.
#[test]
fn cwebp_resize_zero_axis_preserves_aspect() {
    let dir = TempDir::new().expect("tempdir");
    let src = write(dir.path(), "in.ppm", &noisy_ppm(20, 10));

    let by_width = dir.path().join("w.webp");
    cwebp()
        .args(["-resize", "10", "0"])
        .arg(&src)
        .arg("-o")
        .arg(&by_width)
        .assert()
        .success();
    assert_eq!(dimensions_of(&by_width), "10x5");

    let by_height = dir.path().join("h.webp");
    cwebp()
        .args(["-resize", "0", "5"])
        .arg(&src)
        .arg("-o")
        .arg(&by_height)
        .assert()
        .success();
    assert_eq!(dimensions_of(&by_height), "10x5");
}

/// Crop precedes resize: crop 20x10 to a 16x8 region, then resize that to 8x4.
#[test]
fn crop_then_resize_composes_in_order() {
    let dir = TempDir::new().expect("tempdir");
    let src = write(dir.path(), "in.ppm", &noisy_ppm(20, 10));
    let out = dir.path().join("out.webp");
    cwebp()
        .args(["-crop", "0", "0", "16", "8", "-resize", "8", "4"])
        .arg(&src)
        .arg("-o")
        .arg(&out)
        .assert()
        .success();
    assert_eq!(dimensions_of(&out), "8x4");
}

// --- target-size search -------------------------------------------------------

/// `webp encode --target-size N` selects lossy, searches quality, meets the byte
/// budget, and narrates the search under `-v`.
#[test]
fn target_size_meets_the_budget_and_shows_the_search() {
    let dir = TempDir::new().expect("tempdir");
    let src = write(dir.path(), "photo.ppm", &noisy_ppm(64, 64));
    let out = dir.path().join("out.webp");
    let budget = 2500;
    webp()
        .arg("encode")
        .arg("--target-size")
        .arg(budget.to_string())
        .arg("-v")
        .arg(&src)
        .arg("-o")
        .arg(&out)
        .assert()
        .success()
        .stderr(contains("search:").and(contains("q=")));
    let size = fs::metadata(&out).expect("output exists").len();
    assert!(
        size <= budget,
        "output {size} bytes should fit the {budget}-byte target",
    );
    // Lossy output, as a size target implies.
    let info = webp().arg("info").arg(&out).assert().success();
    let stdout = String::from_utf8(info.get_output().stdout.clone()).expect("utf8");
    assert!(
        stdout.contains("lossy"),
        "size target should encode lossy: {stdout}"
    );
}

/// `cwebp -size N` is now live (was rejected): it produces a lossy WebP within
/// the byte budget.
#[test]
fn cwebp_size_targets_the_budget() {
    let dir = TempDir::new().expect("tempdir");
    let src = write(dir.path(), "photo.ppm", &noisy_ppm(64, 64));
    let out = dir.path().join("out.webp");
    let budget = 2500;
    cwebp()
        .arg("-size")
        .arg(budget.to_string())
        .arg(&src)
        .arg("-o")
        .arg(&out)
        .assert()
        .success();
    let size = fs::metadata(&out).expect("output exists").len();
    assert!(size <= budget, "output {size} should fit {budget}");
}

/// A size target on lossless output is a usage error, not a silent ignore.
#[test]
fn target_size_with_lossless_is_a_usage_error() {
    let dir = TempDir::new().expect("tempdir");
    let src = write(dir.path(), "in.ppm", &noisy_ppm(16, 16));
    let out = dir.path().join("out.webp");
    webp()
        .arg("encode")
        .arg("--lossless")
        .arg("--target-size")
        .arg("1000")
        .arg(&src)
        .arg("-o")
        .arg(&out)
        .assert()
        .failure();
}

// --- the --optimize --lossy contradiction -------------------------------------

/// `--optimize --lossy` no longer silently drops `--optimize`: the combination is
/// a usage error (exit 2), because a lossless effort sweep cannot apply to lossy.
#[test]
fn optimize_with_lossy_is_a_usage_error() {
    let dir = TempDir::new().expect("tempdir");
    let src = write(dir.path(), "in.ppm", &solid_ppm(8, 8, [10, 20, 30]));
    webp()
        .arg("convert")
        .arg("--optimize")
        .arg("--lossy")
        .arg(&src)
        .assert()
        .code(2)
        .stderr(contains("optimize"));
}

/// `--optimize` on a lossless input still works — the contradiction is only with
/// an explicit lossy request.
#[test]
fn optimize_without_lossy_still_converts() {
    let dir = TempDir::new().expect("tempdir");
    let src = write(dir.path(), "in.ppm", &noisy_ppm(16, 16));
    webp()
        .arg("convert")
        .arg("--optimize")
        .arg(&src)
        .assert()
        .success();
    assert!(
        dir.path().join("in.webp").is_file(),
        "optimize should still emit a webp"
    );
}
