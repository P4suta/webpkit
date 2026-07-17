//! Integration tests for M7: JPEG/GIF/TIFF/BMP input, GIF → animated WebP, and
//! the bare direction-detected form (`webp photo.png` → `photo.webp`).
//!
//! Fixtures are encoded on the fly with the `image` crate (a dev-dependency), so
//! no binary blobs are committed; the CLI decodes them with the same crate.
#![expect(
    clippy::expect_used,
    reason = "integration-test helpers live outside #[test] fns; failures are bugs"
)]

use std::{fs, path::Path};

use assert_cmd::Command;
use image::{
    Delay, Frame, ImageFormat, Rgba, RgbaImage, codecs::gif::GifEncoder, codecs::gif::Repeat,
};
use predicates::str::contains;
use tempfile::TempDir;

fn webp() -> Command {
    Command::cargo_bin("webp").expect("binary builds")
}

/// A small solid-color RGBA image.
fn sample() -> RgbaImage {
    RgbaImage::from_pixel(6, 4, Rgba([40, 80, 120, 255]))
}

/// Encode [`sample`] into a still-image format's bytes.
fn still(format: ImageFormat) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(sample())
        .write_to(&mut buf, format)
        .expect("encode fixture");
    buf.into_inner()
}

/// A PNG of [`sample`], via the `png` crate (the workspace `image` dep carries no
/// PNG codec — the CLI keeps `png` for metadata fidelity).
fn png_bytes() -> Vec<u8> {
    let img = sample();
    let mut bytes = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut bytes, img.width(), img.height());
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("png header");
        writer.write_image_data(&img).expect("png data");
    }
    bytes
}

/// Encode a three-frame animated GIF that loops forever (no loop extension).
fn gif_animation() -> Vec<u8> {
    gif_with_repeat(None)
}

/// Encode a three-frame animated GIF, optionally stamping a finite loop count.
fn gif_with_repeat(repeat: Option<u16>) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let mut encoder = GifEncoder::new(&mut buf);
        if let Some(count) = repeat {
            encoder
                .set_repeat(Repeat::Finite(count))
                .expect("set gif repeat");
        }
        for _ in 0..3 {
            let frame = Frame::from_parts(sample(), 0, 0, Delay::from_numer_denom_ms(100, 1));
            encoder.encode_frame(frame).expect("encode gif frame");
        }
    }
    buf
}

/// The image-chunk `FourCc` at bytes 12..16 of a WebP file.
fn image_fourcc(webp: &[u8]) -> &[u8] {
    &webp[12..16]
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = dir.join(name);
    fs::write(&path, bytes).expect("write fixture");
    path
}

#[test]
fn jpeg_gif_tiff_bmp_all_encode_to_webp() {
    let dir = TempDir::new().expect("tempdir");
    for (name, bytes) in [
        ("pic.jpg", still(ImageFormat::Jpeg)),
        ("pic.tiff", still(ImageFormat::Tiff)),
        ("pic.bmp", still(ImageFormat::Bmp)),
        ("pic.gif", still(ImageFormat::Gif)),
    ] {
        let input = write(dir.path(), name, &bytes);
        let out = dir.path().join(format!("{name}.webp"));
        webp()
            .arg("encode")
            .arg(&input)
            .arg("-o")
            .arg(&out)
            .assert()
            .success();
        let webp = fs::read(&out).expect("read output");
        assert!(webp.starts_with(b"RIFF"), "{name} must produce a WebP");
    }
}

#[test]
fn a_jpeg_picks_lossy_by_default_and_says_so() {
    let dir = TempDir::new().expect("tempdir");
    let input = write(dir.path(), "photo.jpg", &still(ImageFormat::Jpeg));
    // Bare form: no -o, derives photo.webp beside the input.
    webp()
        .arg(&input)
        .assert()
        .success()
        .stderr(contains("lossy q75"))
        .stderr(contains("from Jpeg source"));
    let webp = fs::read(dir.path().join("photo.webp")).expect("read output");
    assert_eq!(image_fourcc(&webp), b"VP8 ", "JPEG default must be lossy");
}

#[test]
fn a_png_stays_lossless_by_default() {
    let dir = TempDir::new().expect("tempdir");
    let input = write(dir.path(), "flat.png", &png_bytes());
    webp()
        .arg(&input)
        .assert()
        .success()
        .stderr(contains("lossless"))
        .stderr(contains("from Png source"));
    let webp = fs::read(dir.path().join("flat.webp")).expect("read output");
    assert_eq!(image_fourcc(&webp), b"VP8L", "PNG default must be lossless");
}

#[test]
fn quality_flag_sets_lossy_quality() {
    let dir = TempDir::new().expect("tempdir");
    let input = write(dir.path(), "flat.png", &png_bytes());
    // `-q 80` on the bare form selects lossy at that quality (the breaking change:
    // `-q` was quiet, is now quality).
    webp()
        .arg(&input)
        .args(["-q", "80"])
        .assert()
        .success()
        .stderr(contains("lossy q80"));
    let webp = fs::read(dir.path().join("flat.webp")).expect("read output");
    assert_eq!(image_fourcc(&webp), b"VP8 ", "-q must select lossy");
}

#[test]
fn a_non_numeric_quality_hints_at_quiet() {
    let dir = TempDir::new().expect("tempdir");
    let input = write(dir.path(), "flat.png", &png_bytes());
    // The muscle-memory failure: `-q` used to be quiet, now takes a number.
    webp()
        .args(["-q", "loud"])
        .arg(&input)
        .assert()
        .code(2)
        .stderr(contains("--quiet"));
}

#[test]
fn a_gif_becomes_an_animated_webp() {
    let dir = TempDir::new().expect("tempdir");
    let input = write(dir.path(), "loop.gif", &gif_animation());
    webp()
        .arg(&input)
        .assert()
        .success()
        .stderr(contains("animation"));
    let out = dir.path().join("loop.webp");
    // `info` reads it back as a three-frame animation.
    webp()
        .args(["info"])
        .arg(&out)
        .assert()
        .success()
        .stdout(contains("animation"))
        .stdout(contains("Frames:     3"));
}

/// `--quality 80` reaches the GIF animation path: every frame is a lossy `VP8 `
/// key-frame, not the lossless `VP8L` the path used to force. `info` reports the
/// animation's codec as `lossy` only when no frame is lossless.
#[test]
fn a_gif_with_lossy_encodes_every_frame_as_vp8() {
    let dir = TempDir::new().expect("tempdir");
    let input = write(dir.path(), "loop.gif", &gif_animation());
    webp()
        .arg(&input)
        .args(["--quality", "80"])
        .assert()
        .success()
        .stderr(contains("animation (lossy q80)"));
    let out = dir.path().join("loop.webp");
    webp()
        .args(["info"])
        .arg(&out)
        .assert()
        .success()
        .stdout(contains("WebP animation (lossy)"));
}

/// A finite-loop GIF's loop count survives into the animated WebP; the path used
/// to hardcode a forever loop, discarding the source's intent.
#[test]
fn a_finite_loop_gif_preserves_its_loop_count() {
    let dir = TempDir::new().expect("tempdir");
    let input = write(dir.path(), "thrice.gif", &gif_with_repeat(Some(3)));
    webp().arg(&input).assert().success();
    let out = dir.path().join("thrice.webp");
    webp()
        .args(["info"])
        .arg(&out)
        .assert()
        .success()
        .stdout(contains("Loop:       3 time(s)"));
}

#[test]
fn bare_png_encodes_and_bare_webp_decodes() {
    let dir = TempDir::new().expect("tempdir");
    let png = write(dir.path(), "shot.png", &png_bytes());

    // `webp shot.png` → shot.webp
    webp().arg(&png).assert().success();
    assert!(dir.path().join("shot.webp").is_file(), "shot.webp missing");

    // `webp other.webp` → other.png (a fresh stem, so no overwrite guard fires).
    let webp_in = dir.path().join("other.webp");
    fs::copy(dir.path().join("shot.webp"), &webp_in).expect("copy webp");
    webp().arg(&webp_in).assert().success();
    let decoded = dir.path().join("other.png");
    assert!(decoded.is_file(), "other.png missing");
    assert!(
        fs::read(&decoded)
            .expect("read png")
            .starts_with(b"\x89PNG\r\n\x1a\n"),
        "output must be a PNG"
    );
}

#[test]
fn a_derived_output_is_guarded_but_force_and_no_clobber_work() {
    let dir = TempDir::new().expect("tempdir");
    let png = write(dir.path(), "a.png", &png_bytes());
    webp().arg(&png).assert().success();
    // a.webp now exists; a second derived run must refuse (exit 4).
    webp().arg(&png).assert().code(4);
    // `--no-clobber` skips it and still exits 0.
    webp().arg(&png).arg("--no-clobber").assert().success();
    // `--force` overwrites.
    webp().arg(&png).arg("--force").assert().success();
}

#[test]
fn an_explicitly_named_output_overwrites_without_a_flag() {
    let dir = TempDir::new().expect("tempdir");
    let png = write(dir.path(), "b.png", &png_bytes());
    let out = dir.path().join("out.webp");
    fs::write(&out, b"stale").expect("seed output");
    // A single input with an explicit `-o FILE` overwrites: naming it is intent.
    webp().arg(&png).arg("-o").arg(&out).assert().success();
    assert!(
        fs::read(&out).expect("read out").starts_with(b"RIFF"),
        "explicit -o must have been overwritten with a WebP"
    );
}

/// `dwebp -bmp` / `-tiff` write real BMP/TIFF files: the `image` crate reads each
/// back to the exact pixels a lossless round-trip preserves. Without the `formats`
/// feature these flags stay rejected (covered by the drop-in reject tests).
#[test]
fn dwebp_writes_bmp_and_tiff() {
    let dir = TempDir::new().expect("tempdir");
    let png = write(dir.path(), "pic.png", &png_bytes());
    // A lossless WebP, so the decoded pixels are byte-exact.
    let webp_file = dir.path().join("pic.webp");
    Command::cargo_bin("cwebp")
        .expect("cwebp builds")
        .arg(&png)
        .arg("-o")
        .arg(&webp_file)
        .arg("-lossless")
        .arg("-m")
        .arg("6")
        .assert()
        .success();

    for (flag, name, format) in [
        ("-bmp", "out.bmp", ImageFormat::Bmp),
        ("-tiff", "out.tiff", ImageFormat::Tiff),
    ] {
        let out = dir.path().join(name);
        Command::cargo_bin("dwebp")
            .expect("dwebp builds")
            .arg(&webp_file)
            .arg("-o")
            .arg(&out)
            .arg(flag)
            .assert()
            .success();
        let bytes = fs::read(&out).expect("read decoded output");
        let decoded = image::load_from_memory_with_format(&bytes, format)
            .expect("the image crate reads its own encoding back")
            .to_rgba8();
        assert_eq!(decoded, sample(), "{flag} must round-trip the pixels");
    }
}
