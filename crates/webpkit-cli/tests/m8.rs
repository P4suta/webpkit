//! Integration tests for M8: `webp diff`, `webp doctor`, glob self-expansion, and
//! the global `--threads` flag.
//!
//! Fixtures are binary PPMs written on the fly (the `webp` tool reads netpbm with
//! no `formats` feature), so nothing is committed and the tests run in any build.
#![allow(
    clippy::expect_used,
    reason = "integration-test helpers live outside #[test] fns; failures are bugs"
)]

use std::{fs, path::Path};

use assert_cmd::Command;
use predicates::str::contains;

fn webp() -> Command {
    Command::cargo_bin("webp").expect("binary builds")
}

/// A binary PPM (`P6`) of `w`x`h` pixels, every pixel `rgb`.
fn ppm(w: u32, h: u32, rgb: [u8; 3]) -> Vec<u8> {
    let mut bytes = format!("P6\n{w} {h}\n255\n").into_bytes();
    for _ in 0..(w * h) {
        bytes.extend_from_slice(&rgb);
    }
    bytes
}

/// Encode a PPM file to a lossless WebP with the CLI, returning the output path.
fn encode_to_webp(dir: &Path, ppm_name: &str, rgb: [u8; 3]) -> std::path::PathBuf {
    let src = dir.join(ppm_name);
    fs::write(&src, ppm(4, 4, rgb)).expect("write ppm");
    let out = dir.join("encoded.webp");
    webp()
        .arg("encode")
        .arg(&src)
        .arg("-o")
        .arg(&out)
        .assert()
        .success();
    out
}

// --- diff ---------------------------------------------------------------------

#[test]
fn diff_of_a_source_and_its_lossless_webp_is_identical() {
    let dir = tempfile::tempdir().expect("temp dir");
    let src = dir.path().join("a.ppm");
    fs::write(&src, ppm(4, 4, [10, 20, 30])).expect("write");
    let webp_out = encode_to_webp(dir.path(), "b.ppm", [10, 20, 30]);

    // A lossless round trip decodes back to the source pixels, so PSNR is infinite
    // and any `--min-psnr` threshold is met.
    webp()
        .arg("diff")
        .arg(&src)
        .arg(&webp_out)
        .args(["--min-psnr", "40"])
        .assert()
        .success()
        .stdout(contains("identical"));
}

#[test]
fn diff_below_min_psnr_exits_1() {
    let dir = tempfile::tempdir().expect("temp dir");
    let a = dir.path().join("black.ppm");
    let b = dir.path().join("red.ppm");
    fs::write(&a, ppm(4, 4, [0, 0, 0])).expect("a");
    fs::write(&b, ppm(4, 4, [255, 0, 0])).expect("b");

    // Two very different images: PSNR is a few dB, well under 40 — exit 1, the
    // grep/diff predicate convention.
    webp()
        .arg("diff")
        .arg(&a)
        .arg(&b)
        .args(["--min-psnr", "40"])
        .assert()
        .code(1);
}

#[test]
fn diff_without_a_threshold_always_succeeds() {
    let dir = tempfile::tempdir().expect("temp dir");
    let a = dir.path().join("black.ppm");
    let b = dir.path().join("red.ppm");
    fs::write(&a, ppm(4, 4, [0, 0, 0])).expect("a");
    fs::write(&b, ppm(4, 4, [255, 0, 0])).expect("b");

    // No predicate: the comparison is reported, and the run itself succeeds.
    webp()
        .arg("diff")
        .arg(&a)
        .arg(&b)
        .assert()
        .success()
        .stdout(contains("PSNR"))
        .stdout(contains("Max delta"));
}

#[test]
fn diff_json_is_parseable_and_carries_the_metrics() {
    let dir = tempfile::tempdir().expect("temp dir");
    let a = dir.path().join("black.ppm");
    let b = dir.path().join("red.ppm");
    fs::write(&a, ppm(2, 2, [0, 0, 0])).expect("a");
    fs::write(&b, ppm(2, 2, [255, 0, 0])).expect("b");

    let out = webp()
        .arg("diff")
        .arg(&a)
        .arg(&b)
        .arg("--json")
        .output()
        .expect("run diff");
    assert!(out.status.success());
    let value: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout is valid JSON");
    assert_eq!(value["width"], 2);
    assert_eq!(value["max_delta"], 255);
    assert!(
        value["psnr"].is_number(),
        "psnr should be a finite number here"
    );
}

#[test]
fn diff_of_different_sizes_is_a_usage_error() {
    let dir = tempfile::tempdir().expect("temp dir");
    let a = dir.path().join("small.ppm");
    let b = dir.path().join("big.ppm");
    fs::write(&a, ppm(2, 2, [0, 0, 0])).expect("a");
    fs::write(&b, ppm(4, 4, [0, 0, 0])).expect("b");

    webp()
        .arg("diff")
        .arg(&a)
        .arg(&b)
        .assert()
        .code(2)
        .stderr(contains("different sizes"));
}

// --- doctor -------------------------------------------------------------------

#[test]
fn doctor_reports_the_environment_and_succeeds() {
    webp()
        .arg("doctor")
        .assert()
        .success()
        .stdout(contains("network"))
        .stdout(contains("threads"))
        .stdout(contains("cwebp"));
}

#[test]
fn doctor_detects_a_path_shadow() {
    // A `cwebp` earlier on PATH, in a directory that is not this toolkit's, is
    // most likely libwebp's — the drop-in shadow the check exists to catch.
    let shadow = tempfile::tempdir().expect("temp dir");
    let name = if cfg!(windows) { "cwebp.exe" } else { "cwebp" };
    fs::write(shadow.path().join(name), b"not a real cwebp").expect("write fake cwebp");

    webp()
        .arg("doctor")
        .env("PATH", shadow.path())
        .assert()
        .success() // a shadow is a warning, not an error
        .stdout(contains("not this toolkit"));
}

// --- glob self-expansion ------------------------------------------------------

#[test]
fn a_star_pattern_is_expanded_when_the_shell_did_not() {
    let dir = tempfile::tempdir().expect("in dir");
    let out = tempfile::tempdir().expect("out dir");
    fs::write(dir.path().join("a.ppm"), ppm(2, 2, [1, 2, 3])).expect("a");
    fs::write(dir.path().join("b.ppm"), ppm(2, 2, [4, 5, 6])).expect("b");

    // The pattern reaches the binary literally (assert_cmd does not use a shell);
    // the tool expands it because `*.ppm` does not exist as a literal path.
    webp()
        .arg(dir.path().join("*.ppm"))
        .arg("-o")
        .arg(out.path())
        .assert()
        .success();

    assert!(out.path().join("a.webp").is_file(), "a.webp missing");
    assert!(out.path().join("b.webp").is_file(), "b.webp missing");
}

#[test]
fn a_real_file_with_glob_characters_stays_openable() {
    let dir = tempfile::tempdir().expect("in dir");
    // A literal file named with glob metacharacters must not be treated as a
    // pattern: it exists, so it is opened as itself. A sibling `a.ppm` makes the
    // guard bite — without it, the class `[a]` would match `a.ppm` and convert the
    // wrong file (producing `a.webp`, never `[a].webp`).
    let literal = dir.path().join("[a].ppm");
    fs::write(&literal, ppm(2, 2, [7, 8, 9])).expect("write literal");
    fs::write(dir.path().join("a.ppm"), ppm(2, 2, [1, 1, 1])).expect("write sibling");

    webp().arg(&literal).assert().success();
    assert!(
        dir.path().join("[a].webp").is_file(),
        "the literal [a].ppm should have produced [a].webp"
    );
    assert!(
        !dir.path().join("a.webp").is_file(),
        "the sibling a.ppm must not have been converted"
    );
}

// --- --threads ----------------------------------------------------------------

#[test]
fn threads_flag_bounds_the_worker_pool() {
    // `doctor` reports the rayon pool size, so a specific `--threads` value is
    // observable end-to-end: the flag really built the global pool.
    webp()
        .args(["--threads", "3", "doctor"])
        .assert()
        .success()
        .stdout(contains("rayon worker threads: 3"));
}
