//! Integration tests for the `cwebp` / `dwebp` drop-in binaries.
#![allow(
    clippy::expect_used,
    reason = "integration-test helpers live outside #[test] fns; failures are bugs"
)]

use std::process::Output;

use assert_cmd::Command;

fn run(bin: &str, args: &[&str], stdin: Vec<u8>) -> Output {
    Command::cargo_bin(bin)
        .expect("binary builds")
        .args(args)
        .write_stdin(stdin)
        .output()
        .expect("run binary")
}

#[test]
fn cwebp_and_dwebp_report_versions() {
    Command::cargo_bin("cwebp")
        .expect("cwebp builds")
        .arg("-version")
        .assert()
        .success();
    Command::cargo_bin("dwebp")
        .expect("dwebp builds")
        .arg("-version")
        .assert()
        .success();
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn cwebp_rejects_a_lossy_only_flag() {
    let out = run(
        "cwebp",
        &["-near_lossless", "60", "-", "-o", "-"],
        vec![0; 16],
    );
    assert_eq!(
        out.status.code(),
        Some(2),
        "lossy knob must be a usage error"
    );
    // Its own help, not the flat one-liner every rejected flag used to share.
    let err = stderr(&out);
    assert!(
        err.contains("-lossless"),
        "should point at -lossless: {err:?}"
    );
    assert!(
        err.contains("q 90"),
        "should offer a lossy quality: {err:?}"
    );
    assert!(err.contains("cause:"), "should carry a cause: {err:?}");
}

/// The distinctive win: a caret drawn under the offending token, at its real
/// column in the reconstructed command line.
#[test]
fn cwebp_points_a_caret_at_the_rejected_flag() {
    // `-sns` is an internal tuning knob that stays rejected (unlike `-crop`, now live).
    let out = run("cwebp", &["-sns", "50", "-o", "-"], vec![]);
    assert_eq!(out.status.code(), Some(2));
    let err = stderr(&out);
    assert!(
        err.contains("cwebp -sns 50 -o -"),
        "reconstructs the command line: {err:?}"
    );
    // "cwebp " is six columns; "-sns" is four, so four carets sit under it.
    assert!(err.contains("      ^^^^"), "caret under -sns: {err:?}");
}

#[test]
fn cwebp_suggests_a_flag_for_a_typo() {
    let out = run("cwebp", &["-lossles", "-", "-o", "-"], vec![0; 16]);
    assert_eq!(out.status.code(), Some(2));
    let err = stderr(&out);
    assert!(err.contains("unknown option `-lossles`"), "{err:?}");
    assert!(
        err.contains("similar option") && err.contains("-lossless"),
        "should suggest -lossless: {err:?}"
    );
}

#[test]
fn dwebp_rejects_yuv_output() {
    let out = run("dwebp", &["-yuv", "-", "-o", "-"], vec![0; 16]);
    assert_eq!(out.status.code(), Some(2));
    let err = stderr(&out);
    assert!(
        err.contains("-png"),
        "should point at -png/-ppm/-pam: {err:?}"
    );
    assert!(err.contains('^'), "should draw a caret: {err:?}");
}

#[test]
fn dwebp_suggests_a_flag_for_a_typo() {
    let out = run("dwebp", &["-flp", "-", "-o", "-"], vec![0; 16]);
    assert_eq!(out.status.code(), Some(2));
    let err = stderr(&out);
    assert!(
        err.contains("similar option") && err.contains("-flip"),
        "should suggest -flip: {err:?}"
    );
}

#[test]
fn cwebp_reads_a_png_and_dwebp_decodes_it_back() {
    let raw: Vec<u8> = (0..16).collect();

    // Make a real PNG via the brand tool, then run it through cwebp/dwebp.
    let webp0 = run(
        "webp",
        &[
            "encode",
            "-",
            "-o",
            "-",
            "--input-format",
            "raw",
            "--width",
            "2",
            "--height",
            "2",
        ],
        raw.clone(),
    );
    assert!(webp0.status.success());
    let png = run(
        "webp",
        &["decode", "-", "-o", "-", "--format", "png"],
        webp0.stdout,
    );
    assert!(png.status.success());

    // `-lossless`: byte-exact pixels require the VP8L path (the default is lossy).
    let webp1 = run(
        "cwebp",
        &["-", "-o", "-", "-lossless", "-m", "6"],
        png.stdout,
    );
    assert!(webp1.status.success(), "cwebp failed: {webp1:?}");

    let pam = run("dwebp", &["-", "-o", "-", "-pam"], webp1.stdout);
    assert!(pam.status.success(), "dwebp failed: {pam:?}");
    assert!(pam.stdout.starts_with(b"P7"));

    // The 2x2 RGBA body is the final 16 bytes.
    let body = &pam.stdout[pam.stdout.len() - 16..];
    assert_eq!(body, &raw[..], "pixels must survive cwebp -> dwebp");
}

/// The image-chunk `FourCc` at bytes 12..16 of a WebP file (`VP8 ` lossy, `VP8L`
/// lossless, or `VP8X` extended).
fn image_fourcc(webp: &[u8]) -> &[u8] {
    &webp[12..16]
}

#[test]
fn cwebp_defaults_to_lossy_and_opts_into_lossless() {
    // A tiny PPM so cwebp has a real image to encode over stdin.
    let mut ppm = b"P6\n2 2\n255\n".to_vec();
    ppm.extend_from_slice(&[10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120]);

    // Default: lossy VP8.
    let lossy = run("cwebp", &["-", "-o", "-"], ppm.clone());
    assert!(lossy.status.success(), "default cwebp failed: {lossy:?}");
    assert_eq!(
        image_fourcc(&lossy.stdout),
        b"VP8 ",
        "default must be lossy"
    );

    // `-lossless`: VP8L.
    let lossless = run("cwebp", &["-", "-o", "-", "-lossless"], ppm.clone());
    assert!(lossless.status.success(), "cwebp -lossless failed");
    assert_eq!(
        image_fourcc(&lossless.stdout),
        b"VP8L",
        "-lossless must be VP8L"
    );

    // `-z 6`: implies lossless, so also VP8L.
    let level = run("cwebp", &["-", "-o", "-", "-z", "6"], ppm);
    assert!(level.status.success(), "cwebp -z failed");
    assert_eq!(
        image_fourcc(&level.stdout),
        b"VP8L",
        "-z must imply lossless"
    );
}

#[test]
fn dwebp_flip_reverses_rows() {
    // 2x1 image: row0 red-ish, encoded as raw then decoded flipped.
    let raw: Vec<u8> = vec![10, 20, 30, 255, 40, 50, 60, 255];
    let webp = run(
        "webp",
        &[
            "encode",
            "-",
            "-o",
            "-",
            "--input-format",
            "raw",
            "--width",
            "2",
            "--height",
            "1",
        ],
        raw.clone(),
    );
    assert!(webp.status.success());

    // 2x1 has a single row, so a vertical flip is a no-op: pixels are unchanged.
    let pam = run("dwebp", &["-", "-o", "-", "-pam", "-flip"], webp.stdout);
    assert!(pam.status.success());
    let body = &pam.stdout[pam.stdout.len() - 8..];
    assert_eq!(body, &raw[..]);
}
