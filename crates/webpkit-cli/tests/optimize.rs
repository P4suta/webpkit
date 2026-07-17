//! Integration tests for the inter-frame animation optimizer wiring
//! (`webp animate --optimize` and the GIF transcode path's `--optimize`).
//!
//! The correctness gate is compositing identity: an optimized animation must
//! decode to the same canvases as the naive full-frame animation, only smaller.
//! Every fixture is built on the fly, and the CLI itself decodes the result, so
//! the test exercises the real diffing/keyframe/mixed behavior end to end.
#![expect(
    clippy::expect_used,
    reason = "integration-test helpers live outside #[test] fns; failures are bugs"
)]
#![expect(
    clippy::cast_possible_truncation,
    reason = "fixture pixel arithmetic is provably in 0..256 by the modulus"
)]

use std::{fs, path::Path};

use assert_cmd::Command;
use image::{Delay, Frame, RgbaImage, codecs::gif::GifEncoder};
use predicates::str::contains;
use tempfile::TempDir;

fn webp() -> Command {
    Command::cargo_bin("webp").expect("binary builds")
}

/// A `24x24` P6 PPM whose pixels come from `fill(x, y) -> [r, g, b]`.
fn ppm(dir: &Path, name: &str, fill: impl Fn(u32, u32) -> [u8; 3]) -> std::path::PathBuf {
    let (w, h) = (24u32, 24u32);
    let mut bytes = format!("P6\n{w} {h}\n255\n").into_bytes();
    for y in 0..h {
        for x in 0..w {
            bytes.extend_from_slice(&fill(x, y));
        }
    }
    let path = dir.join(name);
    fs::write(&path, bytes).expect("write ppm");
    path
}

/// Decode every composited canvas of `webp_path` to numbered PNG files under `stem`,
/// returning their bytes in frame order. Two animations that composite identically
/// yield equal byte vectors (same decoder, same pixels, deterministic PNG).
fn composited_pngs(dir: &Path, webp_path: &Path, stem: &str) -> Vec<Vec<u8>> {
    let target = dir.join(format!("{stem}.png"));
    webp()
        .arg("decode")
        .arg(webp_path)
        .args(["--frames", "all", "-o"])
        .arg(&target)
        .assert()
        .success();
    let mut frames = Vec::new();
    for index in 0.. {
        let path = dir.join(format!("{stem}-{index:03}.png"));
        if !path.is_file() {
            break;
        }
        frames.push(fs::read(&path).expect("read composited frame"));
    }
    assert!(!frames.is_empty(), "no composited frames were written");
    frames
}

/// Three `24x24` frames: a base gradient, a copy with a red block, and a copy of
/// that (redundant) — so the optimizer has both a real delta and a no-op frame.
fn three_ppm_frames(dir: &Path) -> [std::path::PathBuf; 3] {
    let base = |x: u32, y: u32| [((x * 10) % 256) as u8, ((y * 10) % 256) as u8, 128];
    let block = |x: u32, y: u32| {
        if (4..10).contains(&x) && (4..10).contains(&y) {
            [255, 0, 0]
        } else {
            base(x, y)
        }
    };
    [
        ppm(dir, "f0.ppm", base),
        ppm(dir, "f1.ppm", block),
        ppm(dir, "f2.ppm", block),
    ]
}

/// A three-frame animated GIF whose frames differ then repeat, looping forever.
fn differing_gif() -> Vec<u8> {
    let colors = [[40, 80, 120], [200, 30, 30], [200, 30, 30]];
    let mut buf = Vec::new();
    {
        let mut encoder = GifEncoder::new(&mut buf);
        for rgb in colors {
            let img = RgbaImage::from_pixel(12, 8, image::Rgba([rgb[0], rgb[1], rgb[2], 255]));
            let frame = Frame::from_parts(img, 0, 0, Delay::from_numer_denom_ms(80, 1));
            encoder.encode_frame(frame).expect("encode gif frame");
        }
    }
    buf
}

#[test]
fn animate_optimize_composites_identically_and_shrinks() {
    let dir = TempDir::new().expect("tempdir");
    let frames = three_ppm_frames(dir.path());
    let naive = dir.path().join("naive.webp");
    let opt = dir.path().join("opt.webp");

    webp()
        .arg("animate")
        .args(&frames)
        .args(["--delay", "40,50,60", "-o"])
        .arg(&naive)
        .assert()
        .success();
    webp()
        .arg("animate")
        .arg("--optimize")
        .args(&frames)
        .args(["--delay", "40,50,60", "-o"])
        .arg(&opt)
        .assert()
        .success();

    assert_eq!(
        composited_pngs(dir.path(), &opt, "o"),
        composited_pngs(dir.path(), &naive, "n"),
        "optimized animation must composite pixel-identically to the naive one",
    );
    let (n, o) = (
        fs::metadata(&naive).expect("naive size").len(),
        fs::metadata(&opt).expect("opt size").len(),
    );
    assert!(o < n, "optimized {o} must be smaller than naive {n}");
}

#[test]
fn animate_optimize_with_every_flag_stays_exact() {
    let dir = TempDir::new().expect("tempdir");
    let frames = three_ppm_frames(dir.path());
    let naive = dir.path().join("naive.webp");
    let opt = dir.path().join("opt.webp");

    webp()
        .arg("animate")
        .args(&frames)
        .args(["--delay", "40", "-o"])
        .arg(&naive)
        .assert()
        .success();
    webp()
        .arg("animate")
        .args([
            "--optimize",
            "--mixed",
            "--min-size",
            "--kmin",
            "1",
            "--kmax",
            "2",
        ])
        .args(&frames)
        .args(["--delay", "40", "-o"])
        .arg(&opt)
        .assert()
        .success();

    assert_eq!(
        composited_pngs(dir.path(), &opt, "o"),
        composited_pngs(dir.path(), &naive, "n"),
        "mixed/min-size/keyframe optimization must not change what the animation shows",
    );
}

#[test]
fn animate_default_is_unaffected_by_the_optimizer() {
    // Without --optimize the naive path must be untouched: two runs are identical.
    let dir = TempDir::new().expect("tempdir");
    let frames = three_ppm_frames(dir.path());
    let a = dir.path().join("a.webp");
    let b = dir.path().join("b.webp");
    for out in [&a, &b] {
        webp()
            .arg("animate")
            .args(&frames)
            .args(["--delay", "40", "-o"])
            .arg(out)
            .assert()
            .success();
    }
    assert_eq!(
        fs::read(&a).expect("read a"),
        fs::read(&b).expect("read b"),
        "the default animate output must be deterministic",
    );
}

#[test]
fn gif_transcode_optimize_composites_identically_and_shrinks() {
    let dir = TempDir::new().expect("tempdir");
    let input = dir.path().join("loop.gif");
    fs::write(&input, differing_gif()).expect("write gif");
    let naive = dir.path().join("naive.webp");
    let opt = dir.path().join("opt.webp");

    webp()
        .arg("encode")
        .arg(&input)
        .arg("-o")
        .arg(&naive)
        .assert()
        .success();
    webp()
        .arg("encode")
        .arg("--optimize")
        .arg(&input)
        .arg("-o")
        .arg(&opt)
        .assert()
        .success()
        .stderr(contains("animation"));

    assert_eq!(
        composited_pngs(dir.path(), &opt, "o"),
        composited_pngs(dir.path(), &naive, "n"),
        "the optimized GIF transcode must composite identically to the naive one",
    );
    let (n, o) = (
        fs::metadata(&naive).expect("naive size").len(),
        fs::metadata(&opt).expect("opt size").len(),
    );
    assert!(o <= n, "optimized GIF {o} must be no larger than naive {n}");
}

#[test]
fn optimize_on_a_still_input_is_a_usage_error() {
    // A PPM is a single still; there are no frames to diff, so --optimize is a
    // usage error rather than a silent no-op.
    let dir = TempDir::new().expect("tempdir");
    let still = ppm(dir.path(), "flat.ppm", |_, _| [10, 20, 30]);
    webp()
        .arg("encode")
        .arg("--optimize")
        .arg(&still)
        .args(["-o"])
        .arg(dir.path().join("out.webp"))
        .assert()
        .code(2)
        .stderr(contains("animated GIF"));
}

#[test]
fn optimizer_subflags_require_optimize() {
    // `--mixed` (and its siblings) are meaningless without --optimize; clap rejects
    // them so they can never be a silent no-op.
    let dir = TempDir::new().expect("tempdir");
    let frames = three_ppm_frames(dir.path());
    webp()
        .arg("animate")
        .arg("--mixed")
        .args(&frames)
        .args(["-o"])
        .arg(dir.path().join("out.webp"))
        .assert()
        .code(2);
}
