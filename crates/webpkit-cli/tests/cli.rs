//! Integration tests for the `webp` CLI binary: help/version, stdin→stdout
//! round-trips, PNG round-trips, and the meaningful exit codes.
#![expect(
    clippy::expect_used,
    reason = "integration-test helpers live outside #[test] fns; failures are bugs"
)]

use assert_cmd::Command;
use predicates::str::contains;
use webpkit::{Dimensions, Encoder, ImageRef, Metadata, PixelLayout};

fn webp() -> Command {
    Command::cargo_bin("webp").expect("binary builds")
}

/// An opaque 2x2 lossless WebP carrying an Exif payload, built via the `webpkit`
/// facade so the bare-decode metadata behavior can be observed end to end.
fn webp_with_exif() -> Vec<u8> {
    let dims = Dimensions::new(2, 2).expect("dims");
    let pixels = vec![0xff_u8; 16];
    let img = ImageRef::new(dims, PixelLayout::Rgba8, &pixels).expect("ref");
    Encoder::lossless()
        .metadata(Metadata::none().with_exif(b"MM\0*exif-payload".to_vec()))
        .encode_ref(img)
        .expect("encode")
}

/// The bare direction-detected decode honors `--metadata`: it preserves Exif by
/// default (PNG `eXIf`) and drops it under `--metadata none`. Before the fix the
/// bare form hardcoded `Selection::all()`, so `--metadata none` was ignored.
#[test]
fn bare_decode_honors_metadata() {
    let dir = tempfile::tempdir().expect("temp dir");
    let input = dir.path().join("in.webp");
    std::fs::write(&input, webp_with_exif()).expect("write webp");

    let keep = dir.path().join("keep.png");
    webp().arg(&input).arg("-o").arg(&keep).assert().success();
    let keep_bytes = std::fs::read(&keep).expect("read keep");
    assert!(
        keep_bytes.windows(4).any(|w| w == b"eXIf"),
        "default bare decode should preserve Exif as eXIf"
    );

    let strip = dir.path().join("strip.png");
    webp()
        .arg(&input)
        .arg("-o")
        .arg(&strip)
        .args(["--metadata", "none"])
        .assert()
        .success();
    let strip_bytes = std::fs::read(&strip).expect("read strip");
    assert!(
        !strip_bytes.windows(4).any(|w| w == b"eXIf"),
        "--metadata none must drop Exif from the bare decode"
    );
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

#[test]
fn explain_prints_the_limit_meaning() {
    webp()
        .args(["explain", "7"])
        .assert()
        .success()
        .stdout(contains("limit"))
        .stdout(contains("memory"));
}

#[test]
fn explain_accepts_a_short_name() {
    webp()
        .args(["explain", "read"])
        .assert()
        .success()
        .stdout(contains("could not be read"));
}

#[test]
fn explain_rejects_an_unknown_code_with_exit_2() {
    webp().args(["explain", "42"]).assert().code(2);
}

/// The OS message survives to the user instead of an `ErrorKind` summary: a
/// missing file reads as "no such file", not "entity not found".
#[test]
fn a_read_error_carries_the_os_message() {
    webp()
        .args(["info", "definitely-not-here.webp"])
        .assert()
        .code(3)
        .stderr(contains("cannot read `definitely-not-here.webp`"));
}

/// `--dry-run` reports the plan and writes nothing — and exits 0, so it composes
/// in a script that then decides whether to run for real.
#[test]
fn dry_run_writes_nothing_and_reports_the_plan() {
    let dir = tempfile::tempdir().expect("temp dir");
    let input = dir.path().join("in.ppm");
    let mut bytes = b"P6\n4 4\n255\n".to_vec();
    bytes.extend(std::iter::repeat_n(0x40u8, 4 * 4 * 3));
    std::fs::write(&input, &bytes).expect("write input");
    let output = dir.path().join("out.webp");

    Command::cargo_bin("webp")
        .expect("binary")
        .arg("encode")
        .arg(&input)
        .arg("-o")
        .arg(&output)
        .arg("--dry-run")
        .assert()
        .success()
        .stderr(predicates::str::contains("dry run:"));

    assert!(
        !output.exists(),
        "--dry-run created the output file it promised not to write"
    );
}
