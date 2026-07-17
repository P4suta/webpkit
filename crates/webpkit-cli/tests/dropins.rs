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

    // Find the echoed command line and the caret line beneath it, and check the
    // carets sit *exactly* under `-sns`. A substring assertion would tolerate a
    // caret shifted one column right (the actual column becomes a substring of a
    // wider run of spaces), so this compares real offsets.
    let lines: Vec<&str> = err.lines().collect();
    let command = lines
        .iter()
        .position(|l| l.contains("cwebp -sns 50 -o -"))
        .expect("the command line is echoed");
    let caret_line = lines.get(command + 1).expect("a caret line follows");

    let flag_col = lines[command].find("-sns").expect("`-sns` is in the echo");
    let caret_col = caret_line.find('^').expect("a caret is drawn");
    assert_eq!(caret_col, flag_col, "the caret must start under `-sns`");
    assert_eq!(
        caret_line.trim(),
        "^^^^",
        "exactly four carets, one per character of `-sns`: {caret_line:?}"
    );
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

/// The lossy-alpha tuning knobs are rejected: webpkit stores alpha losslessly, so
/// there is nothing for them to tune. Before the audit they were accepted no-ops.
#[test]
fn cwebp_rejects_alpha_tuning_knobs() {
    for flag in ["-alpha_q", "-alpha_method", "-alpha_filter"] {
        let out = run("cwebp", &[flag, "90", "-", "-o", "-"], vec![0; 16]);
        assert_eq!(out.status.code(), Some(2), "{flag} must be rejected");
        let err = stderr(&out);
        assert!(err.contains("losslessly"), "{flag}: {err:?}");
        assert!(err.contains("-noalpha"), "{flag}: {err:?}");
    }
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

/// `dwebp -dither` is rejected: this decoder reconstructs exact pixels, with no
/// dither stage. Before the audit the strength value was silently consumed.
#[test]
fn dwebp_rejects_dither() {
    let out = run("dwebp", &["-dither", "50", "-", "-o", "-"], vec![0; 16]);
    assert_eq!(out.status.code(), Some(2));
    let err = stderr(&out);
    assert!(err.contains("exact"), "{err:?}");
    assert!(err.contains('^'), "should draw a caret: {err:?}");
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
