//! Integration tests for the `webp` CLI binary: help/version, stdin→stdout
//! round-trips, PNG round-trips, and the meaningful exit codes.
#![allow(
    clippy::expect_used,
    reason = "integration-test helpers live outside #[test] fns; failures are bugs"
)]

use assert_cmd::Command;
use predicates::str::contains;

fn webp() -> Command {
    Command::cargo_bin("webp").expect("binary builds")
}

#[test]
fn version_flag_succeeds() {
    webp().arg("--version").assert().success();
}

#[test]
fn help_lists_subcommands() {
    webp()
        .arg("--help")
        .assert()
        .success()
        .stdout(contains("decode"))
        .stdout(contains("encode"));
}

fn encode_raw_2x2(raw: Vec<u8>) -> Vec<u8> {
    let out = webp()
        .args([
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
        ])
        .write_stdin(raw)
        .output()
        .expect("run encode");
    assert!(out.status.success(), "encode failed: {out:?}");
    out.stdout
}

#[test]
fn raw_round_trips_through_a_stdin_stdout_pipe() {
    // A 2x2 RGBA image (16 bytes).
    let raw: Vec<u8> = (0..16).collect();
    let webp_bytes = encode_raw_2x2(raw.clone());

    let decoded = webp()
        .args(["decode", "-", "-o", "-", "--format", "raw"])
        .write_stdin(webp_bytes)
        .output()
        .expect("run decode");
    assert!(decoded.status.success(), "decode failed: {decoded:?}");

    assert_eq!(decoded.stdout, raw, "round-trip must be byte-exact");
}

#[test]
fn png_round_trips_pixels_through_encode_and_decode() {
    // raw -> webp -> PNG (exercises PNG write) -> webp -> raw (exercises PNG read).
    let raw: Vec<u8> = (0..16).collect();
    let webp1 = encode_raw_2x2(raw.clone());

    let png = webp()
        .args(["decode", "-", "-o", "-", "--format", "png"])
        .write_stdin(webp1)
        .output()
        .expect("run decode to png");
    assert!(png.status.success(), "decode to png failed: {png:?}");
    assert!(
        png.stdout.starts_with(b"\x89PNG\r\n\x1a\n"),
        "output must be a PNG"
    );

    let webp2 = webp()
        .args(["encode", "-", "-o", "-"])
        .write_stdin(png.stdout)
        .output()
        .expect("run encode from png");
    assert!(webp2.status.success(), "encode from png failed: {webp2:?}");

    let raw2 = webp()
        .args(["decode", "-", "-o", "-", "--format", "raw"])
        .write_stdin(webp2.stdout)
        .output()
        .expect("run decode to raw");
    assert!(raw2.status.success());
    assert_eq!(raw2.stdout, raw, "pixels must survive the PNG round-trip");
}

#[test]
fn pam_round_trips_pixels_including_alpha() {
    let raw: Vec<u8> = (0..16).collect();
    let webp1 = encode_raw_2x2(raw.clone());

    let pam = webp()
        .args(["decode", "-", "-o", "-", "--format", "pam"])
        .write_stdin(webp1)
        .output()
        .expect("decode to pam");
    assert!(pam.status.success());
    assert!(pam.stdout.starts_with(b"P7"), "output must be a PAM");

    let webp2 = webp()
        .args(["encode", "-", "-o", "-"])
        .write_stdin(pam.stdout)
        .output()
        .expect("encode from pam");
    assert!(webp2.status.success());

    let raw2 = webp()
        .args(["decode", "-", "-o", "-", "--format", "raw"])
        .write_stdin(webp2.stdout)
        .output()
        .expect("decode to raw");
    assert_eq!(raw2.stdout, raw, "RGBA must survive the PAM round-trip");
}

#[test]
fn ppm_output_is_a_valid_p6_stream() {
    let raw: Vec<u8> = (0..16).collect();
    let webp1 = encode_raw_2x2(raw);

    let ppm = webp()
        .args(["decode", "-", "-o", "-", "--format", "ppm"])
        .write_stdin(webp1)
        .output()
        .expect("decode to ppm");
    assert!(ppm.status.success());
    assert!(ppm.stdout.starts_with(b"P6\n2 2\n255\n"), "PPM header");
    // 2x2 RGB body is 12 bytes after the header.
    assert_eq!(ppm.stdout.len() - b"P6\n2 2\n255\n".len(), 12);
}

#[test]
fn encode_lossy_emits_a_vp8_chunk() {
    // An opaque 2x2 image (alpha 255) so lossy output stays a bare `VP8 ` file
    // rather than upgrading to the extended VP8X+ALPH form.
    let raw: Vec<u8> = vec![
        10, 20, 30, 255, 40, 50, 60, 255, 70, 80, 90, 255, 100, 110, 120, 255,
    ];
    let out = webp()
        .args([
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
            "--lossy",
            "--quality",
            "80",
        ])
        .write_stdin(raw)
        .output()
        .expect("run lossy encode");
    assert!(out.status.success(), "lossy encode failed: {out:?}");
    assert!(out.stdout.starts_with(b"RIFF"), "output must be a WebP");
    assert_eq!(
        &out.stdout[12..16],
        b"VP8 ",
        "--lossy must emit a VP8 chunk"
    );
}

#[test]
fn info_reports_dimensions_and_format() {
    let raw: Vec<u8> = (0..16).collect();
    let webp_bytes = encode_raw_2x2(raw);
    webp()
        .args(["info", "-"])
        .write_stdin(webp_bytes)
        .assert()
        .success()
        .stdout(contains("2x2"))
        .stdout(contains("lossless"));
}

#[test]
fn quiet_encode_writes_nothing_to_stderr() {
    let raw: Vec<u8> = (0..16).collect();
    let out = webp()
        .args([
            "--quiet", "encode", "-", "-o", "-", "--width", "2", "--height", "2",
        ])
        .write_stdin(raw)
        .output()
        .expect("run encode");
    assert!(out.status.success());
    assert!(out.stderr.is_empty(), "quiet must silence stderr");
}

#[test]
fn decoding_non_webp_input_exits_5() {
    webp()
        .args(["decode", "-", "-o", "-"])
        .write_stdin(b"not a webp file".to_vec())
        .assert()
        .code(5);
}

#[test]
fn encoding_a_mismatched_buffer_exits_8() {
    // 2x2 needs 16 bytes; give 10.
    webp()
        .args(["encode", "-", "-o", "-", "--width", "2", "--height", "2"])
        .write_stdin(vec![0_u8; 10])
        .assert()
        .code(8);
}

#[test]
fn missing_input_file_exits_3() {
    webp()
        .args(["decode", "definitely-not-here.webp", "-o", "-"])
        .assert()
        .code(3);
}

#[test]
fn misused_arguments_exit_2() {
    // Missing required -o.
    webp().args(["encode", "-"]).assert().code(2);
}
