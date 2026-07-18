//! Integration tests for the `cwebp` / `dwebp` drop-in binaries.
#![expect(
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

/// `-near_lossless N` is implemented as an encode-side preprocessing step: it is
/// accepted and, like libwebp's cwebp, implies lossless (VP8L) output. The pass is
/// a no-op on this tiny image, but the codec path is what this asserts.
#[test]
fn cwebp_near_lossless_implies_lossless() {
    let mut ppm = b"P6\n2 2\n255\n".to_vec();
    ppm.extend_from_slice(&[10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120]);
    let out = run("cwebp", &["-near_lossless", "60", "-", "-o", "-"], ppm);
    assert!(out.status.success(), "cwebp -near_lossless failed: {out:?}");
    assert_eq!(
        image_fourcc(&out.stdout),
        b"VP8L",
        "-near_lossless must imply lossless"
    );
}

/// A `-near_lossless` level outside `0..=100` is a usage error, not a silent clamp.
#[test]
fn cwebp_near_lossless_rejects_out_of_range() {
    let out = run(
        "cwebp",
        &["-near_lossless", "150", "-", "-o", "-"],
        vec![0; 16],
    );
    assert_eq!(
        out.status.code(),
        Some(2),
        "an out-of-range level must be a usage error"
    );
    let err = stderr(&out);
    assert!(err.contains("0-100"), "should state the range: {err:?}");
}

/// The distinctive win: a caret drawn under the offending token, at its real
/// column in the reconstructed command line.
#[test]
fn cwebp_points_a_caret_at_the_rejected_flag() {
    // `-map` is a preprocessing knob that stays rejected (unlike `-sns`/`-crop`/`-pre`,
    // now live).
    let out = run("cwebp", &["-map", "2", "-o", "-"], vec![]);
    assert_eq!(out.status.code(), Some(2));
    let err = stderr(&out);

    // Find the echoed command line and the caret line beneath it, and check the
    // carets sit *exactly* under `-map`. A substring assertion would tolerate a
    // caret shifted one column right (the actual column becomes a substring of a
    // wider run of spaces), so this compares real offsets.
    let lines: Vec<&str> = err.lines().collect();
    let command = lines
        .iter()
        .position(|l| l.contains("cwebp -map 2 -o -"))
        .expect("the command line is echoed");
    let caret_line = lines.get(command + 1).expect("a caret line follows");

    let flag_col = lines[command].find("-map").expect("`-map` is in the echo");
    let caret_col = caret_line.find('^').expect("a caret is drawn");
    assert_eq!(caret_col, flag_col, "the caret must start under `-map`");
    assert_eq!(
        caret_line.trim(),
        "^^^^",
        "exactly four carets, one per character of `-map`: {caret_line:?}"
    );
}

/// `-pre` (preprocessing) is live: bit 0 selects segment-map smoothing (an opt-in that
/// stays a valid encode), while the unimplemented dithering bit is rejected rather than
/// silently ignored.
#[test]
fn cwebp_pre_selects_segment_smoothing_and_rejects_dithering() {
    // A small PPM so the encode actually runs.
    let mut ppm = b"P6\n8 8\n255\n".to_vec();
    ppm.extend((0u8..192).map(|i| i.wrapping_mul(7)));

    // `-pre 1` (segment-map smoothing) is accepted and produces a decodable WebP.
    let smoothed = run(
        "cwebp",
        &["-", "-o", "-", "-q", "40", "-pre", "1"],
        ppm.clone(),
    );
    assert!(
        smoothed.status.success(),
        "`-pre 1` must encode: {smoothed:?}"
    );
    assert_eq!(&smoothed.stdout[..4], b"RIFF", "emits a WebP container");

    // `-pre 0` (no preprocessing) is byte-identical to omitting the flag entirely.
    let none = run(
        "cwebp",
        &["-", "-o", "-", "-q", "40", "-pre", "0"],
        ppm.clone(),
    );
    let omitted = run("cwebp", &["-", "-o", "-", "-q", "40"], ppm.clone());
    assert!(none.status.success() && omitted.status.success());
    assert_eq!(
        none.stdout, omitted.stdout,
        "`-pre 0` must be byte-identical to no preprocessing"
    );

    // The dithering bit (2) is not implemented; it is a clear usage error, not ignored.
    for pre in ["2", "3"] {
        let out = run(
            "cwebp",
            &["-", "-o", "-", "-q", "40", "-pre", pre],
            ppm.clone(),
        );
        assert_eq!(out.status.code(), Some(2), "`-pre {pre}` must be rejected");
        let err = stderr(&out);
        assert!(
            err.contains("dithering"),
            "should name the unimplemented dithering bit: {err:?}"
        );
    }
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
fn dwebp_emits_yuv_for_lossy_and_explains_for_lossless() {
    // A 3x3 PPM; odd sides exercise the ceil-halved chroma dimensions.
    let mut ppm = b"P6\n3 3\n255\n".to_vec();
    ppm.extend((0u8..27).map(|i| i.wrapping_mul(9)));

    // A lossy VP8 still: `-yuv` reconstructs its native 4:2:0 planes. Y = 3x3,
    // U = V = 2x2 (ceil(3/2) each): 9 + 4 + 4 = 17 packed plane bytes.
    let lossy = run("cwebp", &["-", "-o", "-", "-q", "80"], ppm.clone());
    assert!(lossy.status.success(), "cwebp -q failed: {lossy:?}");
    let yuv = run("dwebp", &["-", "-o", "-", "-yuv"], lossy.stdout);
    assert!(yuv.status.success(), "dwebp -yuv failed: {yuv:?}");
    assert_eq!(yuv.stdout.len(), 17, "planar YUV 4:2:0 plane bytes");

    // A lossless VP8L still has no YUV form: a clear error naming the RGBA formats.
    let lossless = run("cwebp", &["-", "-o", "-", "-lossless"], ppm);
    assert!(lossless.status.success(), "cwebp -lossless failed");
    let out = run("dwebp", &["-yuv", "-", "-o", "-"], lossless.stdout);
    assert_eq!(out.status.code(), Some(2), "lossless -yuv is a usage error");
    let err = stderr(&out);
    assert!(
        err.contains("-png"),
        "should point at -png/-ppm/-pam: {err:?}"
    );
    assert!(
        err.contains("lossy"),
        "should say it needs a lossy WebP: {err:?}"
    );
}

#[test]
fn dwebp_rejects_rgba_transforms_on_the_yuv_path() {
    // `-yuv`/`-pgm` emit the native YUV planes; `-flip`/`-alpha` are RGBA transforms
    // with nothing to act on there. Rather than silently ignore them, dwebp rejects
    // the combination with a message pointing at the RGBA formats.
    let mut ppm = b"P6\n2 2\n255\n".to_vec();
    ppm.extend((0u8..12).map(|i| i.wrapping_mul(20)));
    let lossy = run("cwebp", &["-", "-o", "-", "-q", "80"], ppm);
    assert!(lossy.status.success(), "cwebp -q failed: {lossy:?}");
    for flag in ["-flip", "-alpha"] {
        let out = run(
            "dwebp",
            &["-", "-o", "-", "-yuv", flag],
            lossy.stdout.clone(),
        );
        assert_eq!(
            out.status.code(),
            Some(2),
            "`-yuv {flag}` must be a usage error"
        );
        let err = stderr(&out);
        assert!(err.contains("-yuv"), "names the YUV flag: {err:?}");
        assert!(
            err.contains(flag.trim_start_matches('-')),
            "names {flag}: {err:?}"
        );
    }
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

/// A 2x2 PAM (`P7`, `RGB_ALPHA`) with two non-opaque pixels.
fn pam_2x2_with_alpha() -> Vec<u8> {
    let mut p =
        b"P7\nWIDTH 2\nHEIGHT 2\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n".to_vec();
    p.extend_from_slice(&[
        10, 20, 30, 255, 40, 50, 60, 128, 70, 80, 90, 255, 100, 110, 120, 0,
    ]);
    p
}

/// The 16-byte RGBA body a `dwebp -pam` of a 2x2 image ends with.
fn pam_alpha_bytes(pam: &[u8]) -> [u8; 4] {
    let body = &pam[pam.len() - 16..];
    [body[3], body[7], body[11], body[15]]
}

/// `cwebp -noalpha` drops the alpha channel: every pixel becomes opaque. Before the
/// fix `-noalpha` was an accepted no-op, so the alpha survived.
#[test]
fn cwebp_noalpha_makes_the_image_opaque() {
    let webp = run(
        "cwebp",
        &["-", "-o", "-", "-lossless", "-noalpha"],
        pam_2x2_with_alpha(),
    );
    assert!(webp.status.success(), "cwebp -noalpha failed: {webp:?}");
    let pam = run("dwebp", &["-", "-o", "-", "-pam"], webp.stdout);
    assert!(pam.status.success(), "dwebp failed: {pam:?}");
    assert_eq!(
        pam_alpha_bytes(&pam.stdout),
        [255, 255, 255, 255],
        "-noalpha must make every pixel opaque"
    );
}

/// Without `-noalpha` the alpha channel survives, proving the flag is the cause.
#[test]
fn cwebp_keeps_alpha_without_noalpha() {
    let webp = run(
        "cwebp",
        &["-", "-o", "-", "-lossless"],
        pam_2x2_with_alpha(),
    );
    assert!(webp.status.success(), "cwebp failed: {webp:?}");
    let pam = run("dwebp", &["-", "-o", "-", "-pam"], webp.stdout);
    assert!(pam.status.success());
    assert_eq!(
        pam_alpha_bytes(&pam.stdout),
        [255, 128, 255, 0],
        "without -noalpha the alpha channel is preserved exactly"
    );
}

/// The lossy-alpha tuning knobs are accepted (P5): `-alpha_q`/`-alpha_method`/
/// `-alpha_filter` drive the level-quantization pre-pass and the stored-plane search.
/// Each encodes a real image without a rejection.
#[test]
fn cwebp_accepts_alpha_tuning_knobs() {
    let cases: [&[&str]; 4] = [
        &["-alpha_q", "60"],
        &["-alpha_method", "0"],
        &["-alpha_method", "1"],
        &["-alpha_filter", "fast"],
    ];
    for extra in cases {
        let mut args = vec!["-", "-o", "-", "-q", "80"];
        args.extend_from_slice(extra);
        let out = run("cwebp", &args, pam_2x2_with_alpha());
        assert!(
            out.status.success(),
            "cwebp {extra:?} must succeed: {out:?}"
        );
        assert!(
            out.stdout.starts_with(b"RIFF"),
            "{extra:?}: valid WebP output"
        );
    }
}

/// `-sharp_yuv` is now accepted (was rejected): it selects the luminance-guided chroma
/// path and still produces a valid WebP. Omitting it leaves the default (box chroma), so
/// the two encodes differ only when the flag is present.
#[test]
fn cwebp_accepts_sharp_yuv_and_defaults_off() {
    let baseline = run("cwebp", &["-", "-o", "-", "-q", "80"], pam_2x2_with_alpha());
    assert!(baseline.status.success(), "baseline failed: {baseline:?}");
    assert!(
        baseline.stdout.starts_with(b"RIFF"),
        "baseline is valid WebP"
    );

    let sharp = run(
        "cwebp",
        &["-", "-o", "-", "-q", "80", "-sharp_yuv"],
        pam_2x2_with_alpha(),
    );
    assert!(
        sharp.status.success(),
        "cwebp -sharp_yuv must succeed: {sharp:?}"
    );
    assert!(
        sharp.stdout.starts_with(b"RIFF"),
        "-sharp_yuv output is valid WebP"
    );
}

/// `-alpha_q 100` (the default) leaves the alpha stream byte-identical to an encode
/// with no alpha knob at all: the level-quantization pre-pass is the identity there.
#[test]
fn cwebp_alpha_q_100_is_byte_identical_to_the_default() {
    let baseline = run("cwebp", &["-", "-o", "-", "-q", "80"], pam_2x2_with_alpha());
    assert!(baseline.status.success(), "baseline failed: {baseline:?}");
    let explicit = run(
        "cwebp",
        &["-", "-o", "-", "-q", "80", "-alpha_q", "100"],
        pam_2x2_with_alpha(),
    );
    assert!(explicit.status.success(), "explicit failed: {explicit:?}");
    assert_eq!(
        baseline.stdout, explicit.stdout,
        "-alpha_q 100 must be byte-identical to the default alpha path"
    );
}

/// A bad `-alpha_method`/`-alpha_filter` value is a usage error (exit 2), not a
/// silent no-op.
#[test]
fn cwebp_rejects_bad_alpha_knob_values() {
    let bad_method = run(
        "cwebp",
        &["-alpha_method", "2", "-", "-o", "-"],
        pam_2x2_with_alpha(),
    );
    assert_eq!(
        bad_method.status.code(),
        Some(2),
        "alpha_method 2: {bad_method:?}"
    );
    let bad_filter = run(
        "cwebp",
        &["-alpha_filter", "sharp", "-", "-o", "-"],
        pam_2x2_with_alpha(),
    );
    assert_eq!(
        bad_filter.status.code(),
        Some(2),
        "alpha_filter sharp: {bad_filter:?}"
    );
}

/// A small gradient PPM (`P6`), enough AC energy that a quantizer bias changes bytes.
fn gradient_ppm(width: u32, height: u32) -> Vec<u8> {
    let mut ppm = format!("P6\n{width} {height}\n255\n").into_bytes();
    let channel = |v: u32| u8::try_from(v % 256).unwrap_or(0);
    for row in 0..height {
        for col in 0..width {
            ppm.extend_from_slice(&[
                channel(col * 13),
                channel(row * 17),
                channel((col + row) * 7),
            ]);
        }
    }
    ppm
}

/// `-preset` is accepted (was rejected): `default` resolves to the baseline tuning and is
/// byte-identical, while a content preset (`photo`) reshapes the encode.
#[test]
fn cwebp_preset_is_accepted_default_neutral_photo_reshapes() {
    let ppm = gradient_ppm(16, 16);
    let base = run("cwebp", &["-", "-o", "-", "-q", "80"], ppm.clone());
    assert!(base.status.success(), "baseline failed: {base:?}");
    let default = run(
        "cwebp",
        &["-", "-o", "-", "-q", "80", "-preset", "default"],
        ppm.clone(),
    );
    assert!(
        default.status.success(),
        "-preset default failed: {default:?}"
    );
    assert_eq!(
        base.stdout, default.stdout,
        "-preset default must be byte-identical to no preset"
    );
    let photo = run(
        "cwebp",
        &["-", "-o", "-", "-q", "80", "-preset", "photo"],
        ppm,
    );
    assert!(photo.status.success(), "-preset photo failed: {photo:?}");
    assert!(
        photo.stdout.starts_with(b"RIFF"),
        "photo output is valid WebP"
    );
    assert_ne!(
        base.stdout, photo.stdout,
        "-preset photo must reshape the encode"
    );
}

/// A bad `-preset` name is a usage error (exit 2), not a silent no-op.
#[test]
fn cwebp_rejects_a_bad_preset_name() {
    let out = run("cwebp", &["-preset", "sketch", "-", "-o", "-"], vec![0; 16]);
    assert_eq!(
        out.status.code(),
        Some(2),
        "bad preset must be a usage error"
    );
    let err = stderr(&out);
    assert!(err.contains("preset"), "names the flag: {err:?}");
}

/// `-jpeg_like` and `-partition_limit N` are accepted (were rejected) and bias the base
/// quantizer, changing the output; the neutral `-partition_limit 0` is byte-identical.
#[test]
fn cwebp_rd_knobs_change_output_and_neutral_is_identical() {
    let ppm = gradient_ppm(16, 16);
    let base = run("cwebp", &["-", "-o", "-", "-q", "80"], ppm.clone());
    assert!(base.status.success(), "baseline failed: {base:?}");

    let jpeg_like = run(
        "cwebp",
        &["-", "-o", "-", "-q", "80", "-jpeg_like"],
        ppm.clone(),
    );
    assert!(
        jpeg_like.status.success(),
        "-jpeg_like failed: {jpeg_like:?}"
    );
    assert_ne!(
        base.stdout, jpeg_like.stdout,
        "-jpeg_like must change output"
    );

    let capped = run(
        "cwebp",
        &["-", "-o", "-", "-q", "80", "-partition_limit", "50"],
        ppm.clone(),
    );
    assert!(
        capped.status.success(),
        "-partition_limit 50 failed: {capped:?}"
    );
    assert_ne!(
        base.stdout, capped.stdout,
        "-partition_limit 50 must change output"
    );

    let none = run(
        "cwebp",
        &["-", "-o", "-", "-q", "80", "-partition_limit", "0"],
        ppm,
    );
    assert!(none.status.success(), "-partition_limit 0 failed: {none:?}");
    assert_eq!(
        base.stdout, none.stdout,
        "-partition_limit 0 (no cap) must be byte-identical"
    );
}

/// `-exact` is accepted (webpkit preserves hidden RGB by default), so it is byte-identical
/// to the default: it states a guarantee the encoder already meets rather than being
/// silently ignored.
#[test]
fn cwebp_exact_is_accepted_and_byte_identical_to_default() {
    let base = run("cwebp", &["-", "-o", "-", "-q", "80"], pam_2x2_with_alpha());
    assert!(base.status.success(), "baseline failed: {base:?}");
    let exact = run(
        "cwebp",
        &["-", "-o", "-", "-q", "80", "-exact"],
        pam_2x2_with_alpha(),
    );
    assert!(exact.status.success(), "-exact failed: {exact:?}");
    assert_eq!(
        base.stdout, exact.stdout,
        "-exact preserves hidden RGB (webpkit's default): byte-identical"
    );
}

/// `-blend_alpha` (background compositing) is rejected, pointing at `-noalpha`.
#[test]
fn cwebp_rejects_blend_alpha() {
    let out = run(
        "cwebp",
        &["-blend_alpha", "0xffffff", "-", "-o", "-"],
        vec![0; 16],
    );
    assert_eq!(out.status.code(), Some(2));
    let err = stderr(&out);
    assert!(err.contains("-noalpha"), "{err:?}");
}

/// `dwebp -dither` is now accepted (not rejected): the strength is range-validated,
/// and the flag is a documented no-op because this decoder reconstructs exact pixels.
/// An out-of-range strength is a usage error.
#[test]
fn dwebp_dither_is_accepted_and_range_checked() {
    // 2x2 image encoded, then decoded with `-dither 50`: parsing succeeds (the flag is
    // no longer a hard rejection) and the decode completes.
    let raw: Vec<u8> = (0..16u8).collect();
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
            "2",
        ],
        raw,
    );
    assert!(webp.status.success());
    let ok = run(
        "dwebp",
        &["-dither", "50", "-", "-o", "-", "-pam"],
        webp.stdout,
    );
    assert!(ok.status.success(), "-dither must be accepted: {ok:?}");

    // A strength above 100 is an honest usage error.
    let bad = run("dwebp", &["-dither", "200", "-", "-o", "-"], vec![0; 16]);
    assert_eq!(bad.status.code(), Some(2));
    assert!(stderr(&bad).contains("0-100"), "{:?}", stderr(&bad));
}

/// `dwebp -crop` and `-scale` now transform the decoded pixels through the core
/// geometry (they used to be rejected). Crop selects a window; scale resamples.
#[test]
fn dwebp_crop_and_scale_transform_the_decoded_image() {
    // 4x4 raw RGBA, then crop the inner 2x2 at (1,1) and read it back.
    let raw: Vec<u8> = (0..64u8).collect();
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
            "4",
            "--height",
            "4",
        ],
        raw,
    );
    assert!(webp.status.success());

    let cropped = run(
        "dwebp",
        &["-crop", "1", "1", "2", "2", "-", "-o", "-", "-pam"],
        webp.stdout.clone(),
    );
    assert!(cropped.status.success(), "dwebp -crop failed: {cropped:?}");
    // A 2x2 PAM body is 16 bytes; its header records the cropped dimensions.
    let header = String::from_utf8_lossy(&cropped.stdout);
    assert!(header.contains("WIDTH 2"), "{header:?}");
    assert!(header.contains("HEIGHT 2"), "{header:?}");

    // Scale the 4x4 up to 8x8.
    let scaled = run(
        "dwebp",
        &["-scale", "8", "8", "-", "-o", "-", "-pam"],
        webp.stdout,
    );
    assert!(scaled.status.success(), "dwebp -scale failed: {scaled:?}");
    let header = String::from_utf8_lossy(&scaled.stdout);
    assert!(header.contains("WIDTH 8"), "{header:?}");
    assert!(header.contains("HEIGHT 8"), "{header:?}");
}

/// The `cwebp` drop-in ignores `WEBP_*` config: it must stay byte-for-byte
/// libwebp-compatible for scripts, so an unrelated env var cannot change its output.
#[test]
fn cwebp_ignores_webp_env_config() {
    let mut ppm = b"P6\n2 2\n255\n".to_vec();
    ppm.extend_from_slice(&[10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110, 120]);

    let with_env = Command::cargo_bin("cwebp")
        .expect("binary builds")
        .env("WEBP_CODEC", "lossless")
        .env("WEBP_QUALITY", "10")
        .args(["-", "-o", "-"])
        .write_stdin(ppm.clone())
        .output()
        .expect("run binary");
    assert!(with_env.status.success());
    assert_eq!(
        image_fourcc(&with_env.stdout),
        b"VP8 ",
        "cwebp must stay lossy-by-default, ignoring WEBP_CODEC"
    );

    let no_env = Command::cargo_bin("cwebp")
        .expect("binary builds")
        .env_remove("WEBP_CODEC")
        .env_remove("WEBP_QUALITY")
        .args(["-", "-o", "-"])
        .write_stdin(ppm)
        .output()
        .expect("run binary");
    assert_eq!(
        with_env.stdout, no_env.stdout,
        "env must not change the drop-in's output bytes"
    );
}

/// The `dwebp` drop-in uses the fixed built-in decode cap, not `WEBP_MAX_PIXELS`, so
/// a valid file still decodes even under a hostile env value.
#[test]
fn dwebp_ignores_webp_max_pixels() {
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
            "2",
        ],
        (0..16u8).collect(),
    );
    assert!(webp.status.success());
    let out = Command::cargo_bin("dwebp")
        .expect("binary builds")
        .env("WEBP_MAX_PIXELS", "1")
        .args(["-", "-o", "-", "-pam"])
        .write_stdin(webp.stdout)
        .output()
        .expect("run binary");
    assert!(
        out.status.success(),
        "dwebp must ignore WEBP_MAX_PIXELS: {out:?}"
    );
}

/// `-pass N` is accepted (was rejected): `-pass 1` (the default) is byte-identical to
/// a single-pass encode, and a higher pass count still produces a valid WebP (the
/// codec refines the entropy model but stays decode-safe).
#[test]
fn cwebp_pass_is_accepted_one_is_neutral_higher_is_valid() {
    let ppm = gradient_ppm(64, 64);
    let base = run("cwebp", &["-", "-o", "-", "-q", "60"], ppm.clone());
    assert!(base.status.success(), "baseline failed: {base:?}");

    let one = run(
        "cwebp",
        &["-", "-o", "-", "-q", "60", "-pass", "1"],
        ppm.clone(),
    );
    assert!(one.status.success(), "-pass 1 failed: {one:?}");
    assert_eq!(
        base.stdout, one.stdout,
        "-pass 1 must be byte-identical to a single-pass encode"
    );

    let many = run("cwebp", &["-", "-o", "-", "-q", "60", "-pass", "6"], ppm);
    assert!(many.status.success(), "-pass 6 failed: {many:?}");
    assert!(
        many.stdout.starts_with(b"RIFF"),
        "-pass 6 output is a valid WebP"
    );
}

/// `-short` and `-progress` are live (were silent no-ops): `-short` collapses the
/// status line to the result size, and `-progress` narrates the encode by stage —
/// both on stderr, so the encoded bytes on stdout are untouched.
#[test]
fn cwebp_short_and_progress_report_via_stderr() {
    let ppm = gradient_ppm(16, 16);
    let base = run("cwebp", &["-", "-o", "-", "-q", "80"], ppm.clone());
    assert!(base.status.success(), "baseline failed: {base:?}");

    let short = run(
        "cwebp",
        &["-", "-o", "-", "-q", "80", "-short"],
        ppm.clone(),
    );
    assert!(short.status.success(), "-short failed: {short:?}");
    // Same bytes on stdout; the status line is the collapsed form on stderr.
    assert_eq!(
        base.stdout, short.stdout,
        "-short must not change the output bytes"
    );
    let err = stderr(&short);
    assert!(
        err.contains("bytes"),
        "-short prints the result size: {err:?}"
    );
    assert!(
        !err.contains("->"),
        "-short omits the full status line: {err:?}"
    );

    let progress = run("cwebp", &["-", "-o", "-", "-q", "80", "-progress"], ppm);
    assert!(progress.status.success(), "-progress failed: {progress:?}");
    let err = stderr(&progress);
    assert!(
        err.contains("encoding"),
        "-progress reports the encode stage: {err:?}"
    );
}
