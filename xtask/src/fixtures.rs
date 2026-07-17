//! Fixture synthesis: deterministic synthetic sources, the dependency-free PNG
//! writer, and the `gen-fixtures` pipeline that authors the decode/encode/metadata/
//! animation goldens via libwebp and gates each with a fail-closed round-trip.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::common::{decode_fixtures_dir, encode_fixtures_dir, webpkit_encode, workspace_root};
use crate::libwebp::{
    REQUIRED_LIBWEBP, check_version, cwebp_bin, dwebp_bin, img2webp_bin, run_cwebp, run_dwebp,
    run_img2webp, webpmux_bin, webpmux_get_frame, webpmux_set,
};

/// A single deterministic fixture: a synthetic source image plus its metadata.
struct FixtureCase {
    /// Directory name / case identifier under `fixtures/decode/`.
    name: &'static str,
    /// The codec feature label recorded in the case's `meta.toml`.
    feature: &'static str,
    /// Image width in pixels.
    width: u32,
    /// Image height in pixels.
    height: u32,
    /// Maps `(x, y)` to an `[R, G, B, A]` pixel.
    pixel: Box<dyn Fn(u32, u32) -> [u8; 4]>,
}

/// Low byte of `v` (equivalent to `(v & 0xff) as u8`) without a lossy cast.
#[must_use]
const fn lo(v: u32) -> u8 {
    v.to_le_bytes()[0]
}

/// A distinct opaque color for palette band `i`; `R = 16*i + 8` is unique per
/// band (for `i < 16`), so every band gets its own color and cwebp selects the
/// color-indexing (palette) transform.
#[must_use]
const fn band_color(i: u32) -> [u8; 4] {
    [lo(16 * i + 8), lo(53 * i + 17), lo(101 * i + 29), 255]
}

/// `n` equal-width vertical color bands across a `w`-pixel-wide image.
fn vertical_bands(n: u32, w: u32) -> Box<dyn Fn(u32, u32) -> [u8; 4]> {
    Box::new(move |x, _y| band_color((x * n) / w))
}

/// The full, deterministic fixture-case matrix.
fn cases() -> Vec<FixtureCase> {
    vec![
        FixtureCase {
            name: "palette_2color",
            feature: "palette",
            width: 32,
            height: 8,
            pixel: Box::new(|x, _y| {
                if x < 16 {
                    [10, 20, 30, 255]
                } else {
                    [200, 100, 50, 255]
                }
            }),
        },
        FixtureCase {
            name: "palette_4color",
            feature: "palette",
            width: 32,
            height: 8,
            pixel: vertical_bands(4, 32),
        },
        FixtureCase {
            name: "palette_16color",
            feature: "palette",
            width: 32,
            height: 32,
            pixel: vertical_bands(16, 32),
        },
        FixtureCase {
            name: "palette_256color",
            feature: "palette",
            width: 256,
            height: 16,
            pixel: Box::new(|x, _y| [lo(x), lo(x * 7), lo(x * 13), 255]),
        },
        FixtureCase {
            name: "gradient_rgb",
            feature: "predictor",
            width: 64,
            height: 64,
            pixel: Box::new(|x, y| [lo(4 * x), lo(4 * y), lo(2 * (x + y)), 255]),
        },
        FixtureCase {
            name: "cross_color_corr",
            feature: "cross-color",
            width: 64,
            height: 64,
            pixel: Box::new(|x, y| [lo(4 * x + y), lo(4 * x), lo(4 * x + 2 * y), 255]),
        },
        FixtureCase {
            name: "alpha_gradient",
            feature: "predictor",
            width: 32,
            height: 32,
            pixel: Box::new(|x, y| [lo(8 * x), lo(8 * y), 128, lo(8 * (x + y))]),
        },
        FixtureCase {
            name: "all_transparent",
            feature: "exact",
            width: 8,
            height: 8,
            pixel: Box::new(|_x, _y| [123, 45, 67, 0]),
        },
        FixtureCase {
            name: "row_256x1",
            feature: "predictor",
            width: 256,
            height: 1,
            pixel: Box::new(|x, _y| [lo(x), lo(x), lo(x), 255]),
        },
        FixtureCase {
            name: "col_1x256",
            feature: "predictor",
            width: 1,
            height: 256,
            pixel: Box::new(|_x, y| [lo(y), lo(y), lo(y), 255]),
        },
        FixtureCase {
            name: "single_pixel_1x1",
            feature: "literal",
            width: 1,
            height: 1,
            pixel: Box::new(|_x, _y| [17, 34, 51, 255]),
        },
        FixtureCase {
            // Repeating 4x4 color blocks: heavy LZ77 (RLE runs and repeated rows).
            name: "lz77_tiled",
            feature: "lz77",
            width: 32,
            height: 32,
            pixel: Box::new(|x, y| band_color((x / 4 + y / 4) % 5)),
        },
        FixtureCase {
            // Twelve colors scattered with no runs, so the cost model picks a cache.
            name: "color_cache_scatter",
            feature: "color-cache",
            width: 32,
            height: 32,
            pixel: Box::new(|x, y| band_color((x * 5 + y * 11) % 12)),
        },
    ]
}

/// Render a case to raw, row-major RGBA bytes (no padding).
fn synthesize(case: &FixtureCase) -> Vec<u8> {
    let mut buf = Vec::new();
    for y in 0..case.height {
        for x in 0..case.width {
            buf.extend_from_slice(&(case.pixel)(x, y));
        }
    }
    buf
}

/// Write raw RGBA (row-major, 8-bit) as an uncompressed PNG (color type 6).
///
/// libwebp's Windows `cwebp` decodes non-WebP inputs through WIC, which does not
/// read PNM but does read PNG. The image is stored (uncompressed DEFLATE) so it
/// needs no compression dependency, and cwebp `-lossless -exact` preserves every
/// byte (verified by the round-trip invariant in [`generate_case`]).
pub(crate) fn write_png(path: &Path, width: u32, height: u32, rgba: &[u8]) -> Result<()> {
    let mut png = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]); // 8-bit depth, RGBA, deflate, no filter/interlace
    write_png_chunk(&mut png, b"IHDR", &ihdr);

    // Scanlines, each prefixed with filter-type byte 0 (None).
    let stride = width as usize * 4;
    let mut raw = Vec::with_capacity(height as usize * (1 + stride));
    for row in rgba.chunks_exact(stride) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    write_png_chunk(&mut png, b"IDAT", &zlib_stored(&raw));
    write_png_chunk(&mut png, b"IEND", &[]);

    std::fs::write(path, &png).with_context(|| format!("writing PNG {}", path.display()))
}

/// Append a PNG chunk: length, type, data, then CRC-32 of `type ++ data`.
fn write_png_chunk(out: &mut Vec<u8>, chunk_type: &[u8], data: &[u8]) {
    let len = u32::try_from(data.len()).expect("PNG chunk length fits u32");
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(chunk_type);
    out.extend_from_slice(data);
    out.extend_from_slice(&png_crc32(chunk_type, data).to_be_bytes());
}

/// Wrap `raw` in a zlib stream built solely from stored (uncompressed) DEFLATE
/// blocks — valid input for any conformant decoder, with no compression code.
fn zlib_stored(raw: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01]; // zlib header: deflate/32K window/no dict/level 0
    if raw.is_empty() {
        out.extend_from_slice(&[0x01, 0x00, 0x00, 0xff, 0xff]); // single final empty block
    } else {
        let mut blocks = raw.chunks(0xffff).peekable();
        while let Some(block) = blocks.next() {
            out.push(u8::from(blocks.peek().is_none())); // BFINAL in bit 0, BTYPE=00 (stored)
            let n = u16::try_from(block.len()).expect("stored block <= 0xffff");
            out.extend_from_slice(&n.to_le_bytes());
            out.extend_from_slice(&(!n).to_le_bytes());
            out.extend_from_slice(block);
        }
    }
    out.extend_from_slice(&adler32(raw).to_be_bytes());
    out
}

/// CRC-32 (IEEE 802.3 polynomial) of `a ++ b`, as PNG chunks require.
fn png_crc32(a: &[u8], b: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in a.iter().chain(b) {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Adler-32 checksum (zlib trailer) of `data`.
fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &byte in data {
        a = (a + u32::from(byte)) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

/// Strip a PAM header, returning everything after the `ENDHDR\n` marker.
///
/// Mirrors the logic in `crates/webpkit/tests/golden_local.rs`.
fn strip_pam_header(pam: &[u8]) -> Result<Vec<u8>> {
    let marker = b"ENDHDR\n";
    let end = pam
        .windows(marker.len())
        .position(|w| w == marker)
        .map(|p| p + marker.len())
        .context("PAM output missing `ENDHDR` marker")?;
    Ok(pam[end..].to_vec())
}

/// Best-effort: report whether `webpinfo` mentions transform usage. Never fails.
fn report_transforms(input_webp: &Path, case_name: &str) {
    let webpinfo = std::env::var("WEBPKIT_WEBPINFO").unwrap_or_else(|_| "webpinfo".to_owned());
    let mut cmd = Command::new(&webpinfo);
    cmd.arg("-bitstream_info").arg(input_webp);
    if let Ok(out) = cmd.output() {
        let text = String::from_utf8_lossy(&out.stdout).to_lowercase();
        let verdict = if text.contains("transform") {
            "transform usage reported"
        } else {
            "no transform keyword in bitstream info"
        };
        println!("  webpinfo[{case_name}]: {verdict}");
    }
    // webpinfo missing or non-zero exit: silently skip (best-effort only).
}

/// Write the per-case `meta.toml` (feature / level / provenance note).
fn write_meta(case_dir: &Path, case: &FixtureCase) -> Result<()> {
    let note = format!(
        "{}x{} synthetic source; input.webp via cwebp {REQUIRED_LIBWEBP} \
         `-lossless -exact -m 6 -q 100`, expected.rgba via dwebp \
         {REQUIRED_LIBWEBP} `-pam` (header stripped)",
        case.width, case.height
    );
    let meta = format!(
        "# Generated by `cargo xtask gen-fixtures`. Do not hand-edit the goldens.\n\
         feature = \"{}\"\n\
         level = \"must\"\n\
         note = \"{note}\"\n",
        case.feature
    );
    std::fs::write(case_dir.join("meta.toml"), meta)
        .with_context(|| format!("writing meta.toml for {}", case.name))
}

/// Write the per-case encode `meta.toml` (feature / level / dims / provenance).
///
/// Keys are emitted in the same order taplo's canonical form keeps them
/// (`reorder_keys = false`), matching the decode metas, so `taplo fmt --check`
/// stays green: `feature`, `level`, `width`, `height`, `note`.
fn write_encode_meta(case_dir: &Path, case: &FixtureCase) -> Result<()> {
    let note = format!(
        "{}x{} synthetic source; input.rgba is raw row-major RGBA. Round-trip is \
         gated at generation time: self webpkit::lossless::decode(webpkit::lossless::encode) plus an \
         independent dwebp {REQUIRED_LIBWEBP} `-pam` decode. Encode goldens are \
         not committed (they are version-dependent).",
        case.width, case.height
    );
    let meta = format!(
        "# Generated by `cargo xtask gen-fixtures`. Do not hand-edit.\n\
         feature = \"{}\"\n\
         level = \"must\"\n\
         width = {}\n\
         height = {}\n\
         note = \"{note}\"\n",
        case.feature, case.width, case.height
    );
    std::fs::write(case_dir.join("meta.toml"), meta)
        .with_context(|| format!("writing encode meta.toml for {}", case.name))
}

/// Generate the full pipeline for one case, enforcing the round-trip invariant.
fn generate_case(case: &FixtureCase, decode_dir: &Path, cwebp: &str, dwebp: &str) -> Result<()> {
    let case_dir = decode_dir.join(case.name);
    std::fs::create_dir_all(&case_dir)
        .with_context(|| format!("creating {}", case_dir.display()))?;

    let rgba = synthesize(case);

    let tmp = tempfile::tempdir().context("creating tempdir for source image")?;
    let src_png = tmp.path().join("src.png");
    write_png(&src_png, case.width, case.height, &rgba)?;

    let input_webp = case_dir.join("input.webp");
    run_cwebp(cwebp, &src_png, &input_webp, None)?;

    let golden_pam = tmp.path().join("golden.pam");
    run_dwebp(dwebp, &input_webp, &golden_pam)?;

    let pam =
        std::fs::read(&golden_pam).with_context(|| format!("reading {}", golden_pam.display()))?;
    let expected = strip_pam_header(&pam)?;

    // Round-trip invariant: `cwebp -lossless -exact` then `dwebp` is an identity.
    if expected != rgba {
        bail!(
            "round-trip invariant violated for case `{}`: dwebp output ({} bytes) != \
             synthesized source ({} bytes); `-lossless -exact` must be an identity",
            case.name,
            expected.len(),
            rgba.len()
        );
    }

    std::fs::write(case_dir.join("expected.rgba"), &expected)
        .with_context(|| format!("writing expected.rgba for {}", case.name))?;
    write_meta(&case_dir, case)?;
    report_transforms(&input_webp, case.name);
    println!(
        "gen-fixtures: {} ({}x{}) OK -> {}",
        case.name,
        case.width,
        case.height,
        input_webp.display()
    );
    Ok(())
}

/// Fixed metadata blobs embedded into the VP8X metadata fixture. They are
/// authored here (arbitrary but deterministic bytes) and attached with libwebp
/// `webpmux`; decoding them back exercises our VP8X / chunk parser against
/// libwebp's *container writer* (external-independent, like the pixel goldens).
const META_ICC: &[u8] = b"webpkit::lossless-test-icc-profile\x00\x01\x02\x03\x04";
const META_EXIF: &[u8] = b"II*\x00webpkit_lossless-test-exif-payload";
const META_XMP: &[u8] = b"<x:xmpmeta>webpkit::lossless-test-xmp</x:xmpmeta>";

/// Generate the `vp8x_metadata` decode fixture: a libwebp-authored extended
/// (`VP8X`) container carrying ICC/Exif/XMP, gated on our decoder recovering both
/// the pixels (vs dwebp) and the exact metadata blobs.
fn generate_metadata_case(
    decode_dir: &Path,
    cwebp: &str,
    dwebp: &str,
    webpmux: &str,
) -> Result<()> {
    let name = "vp8x_metadata";
    let (width, height) = (16u32, 16u32);
    let case_dir = decode_dir.join(name);
    std::fs::create_dir_all(&case_dir)
        .with_context(|| format!("creating {}", case_dir.display()))?;

    // A small deterministic gradient source.
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            rgba.extend_from_slice(&[lo(x * 16), lo(y * 16), lo((x + y) * 8), 255]);
        }
    }

    let tmp = tempfile::tempdir().context("creating tempdir for metadata fixture")?;
    let src_png = tmp.path().join("src.png");
    write_png(&src_png, width, height, &rgba)?;
    let base = tmp.path().join("base.webp");
    run_cwebp(cwebp, &src_png, &base, None)?;

    // Commit the metadata blobs as the extraction goldens.
    let icc = case_dir.join("expected.icc");
    let exif = case_dir.join("expected.exif");
    let xmp = case_dir.join("expected.xmp");
    std::fs::write(&icc, META_ICC).with_context(|| format!("writing {}", icc.display()))?;
    std::fs::write(&exif, META_EXIF).with_context(|| format!("writing {}", exif.display()))?;
    std::fs::write(&xmp, META_XMP).with_context(|| format!("writing {}", xmp.display()))?;

    // Attach each metadata chunk with webpmux (chained), yielding a VP8X file.
    let t1 = tmp.path().join("t1.webp");
    let t2 = tmp.path().join("t2.webp");
    let input_webp = case_dir.join("input.webp");
    webpmux_set(webpmux, "icc", &icc, &base, &t1)?;
    webpmux_set(webpmux, "exif", &exif, &t1, &t2)?;
    webpmux_set(webpmux, "xmp", &xmp, &t2, &input_webp)?;

    // Pixel golden: dwebp ignores metadata, so it must match the source.
    let golden_pam = tmp.path().join("golden.pam");
    run_dwebp(dwebp, &input_webp, &golden_pam)?;
    let expected = strip_pam_header(&std::fs::read(&golden_pam)?)?;
    if expected != rgba {
        bail!("metadata fixture pixel round-trip failed: dwebp output != source");
    }
    std::fs::write(case_dir.join("expected.rgba"), &expected)
        .with_context(|| format!("writing expected.rgba for {name}"))?;

    let note = format!(
        "{width}x{height} VP8X container authored by webpmux {REQUIRED_LIBWEBP} \
         (`-set icc/exif/xmp`); expected.rgba via dwebp `-pam`; expected.icc/exif/xmp \
         are the embedded blobs. Verifies our VP8X + metadata parser against libwebp."
    );
    let meta = format!(
        "# Generated by `cargo xtask gen-fixtures`. Do not hand-edit the goldens.\n\
         feature = \"metadata\"\n\
         level = \"must\"\n\
         note = \"{note}\"\n"
    );
    std::fs::write(case_dir.join("meta.toml"), meta)
        .with_context(|| format!("writing meta.toml for {name}"))?;

    // GATE (fail-closed): our decoder must recover the exact embedded metadata.
    let image = webpkit::lossless::decode(&std::fs::read(&input_webp)?)
        .with_context(|| format!("webpkit::lossless decode of {name} fixture"))?;
    let md = image.metadata();
    if md.icc_profile.as_deref() != Some(META_ICC)
        || md.exif.as_deref() != Some(META_EXIF)
        || md.xmp.as_deref() != Some(META_XMP)
    {
        bail!(
            "metadata extraction gate failed for `{name}`: webpkit::lossless did not recover the \
             ICC/Exif/XMP blobs webpmux embedded"
        );
    }
    report_transforms(&input_webp, name);
    println!(
        "gen-fixtures: {name} ({width}x{height}) OK (webpmux VP8X + metadata extraction gate)"
    );
    Ok(())
}

/// Number of frames in the animation fixture.
const ANIM_FRAMES: u32 = 3;
/// Canvas size of the animation fixture.
const ANIM_SIZE: u32 = 16;

/// Synthesize animation frame `n`: a gradient globally shifted by `n`, so *every*
/// pixel differs between consecutive frames. libwebp therefore cannot optimize
/// the frames into sub-rectangles (they stay full-canvas), keeping the per-frame
/// golden equal to both the raw frame and the composited canvas.
fn animation_frame(width: u32, height: u32, n: u32) -> Vec<u8> {
    let mut buf = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            buf.extend_from_slice(&[
                lo(x * 16 + n * 80),
                lo(y * 16 + n * 50),
                lo((x + y) * 8 + n * 90),
                255,
            ]);
        }
    }
    buf
}

/// Build an animation from `frames` with our own [`webpkit::AnimationEncoder`], for
/// the encoder-side external gate.
fn build_webpkit_animation(width: u32, height: u32, frames: &[Vec<u8>]) -> Result<Vec<u8>> {
    let canvas = webpkit::lossless::Dimensions::new(width, height)?;
    let make_meta = || {
        webpkit::lossless::FrameMeta::new(
            0,
            0,
            canvas,
            100,
            webpkit::lossless::BlendMode::Blend,
            webpkit::lossless::DisposalMode::Keep,
        )
    };
    let (first, rest) = frames
        .split_first()
        .context("animation must have at least one frame")?;
    let mut encoder = webpkit::AnimationEncoder::new(canvas).add_frame(
        webpkit::lossless::ImageRef::new(canvas, webpkit::lossless::PixelLayout::Rgba8, first)?,
        make_meta(),
    )?;
    for rgba in rest {
        encoder = encoder.add_frame(
            webpkit::lossless::ImageRef::new(canvas, webpkit::lossless::PixelLayout::Rgba8, rgba)?,
            make_meta(),
        )?;
    }
    Ok(encoder.finish())
}

/// Generate the `animation_frames` fixture: a libwebp `img2webp`-authored
/// lossless animation, gated (fail-closed) on our decoder recovering every
/// frame byte-exactly and our encoder producing an animation libwebp reads back.
///
/// Three verification layers hang off this fixture:
/// * `expected.frame{0,1,2}.rgba` — each frame decoded by libwebp
///   (`webpmux -get frame` then `dwebp -pam`); the committed goldens for the
///   `webpkit-lossless-conformance` animation runner.
/// * `expected.rgba` — the first composited frame, so the tool-free ledger
///   (`compute_decode_results`) scores `decode_rgba(anim) == frame 0`.
/// * Inline gates here — every frame + composited canvas from `webpkit-lossless`, plus the
///   encoder round-trip through libwebp.
fn generate_animation_case(
    decode_dir: &Path,
    dwebp: &str,
    webpmux: &str,
    img2webp: &str,
) -> Result<()> {
    let name = "animation_frames";
    let (width, height) = (ANIM_SIZE, ANIM_SIZE);
    let case_dir = decode_dir.join(name);
    std::fs::create_dir_all(&case_dir)
        .with_context(|| format!("creating {}", case_dir.display()))?;

    let frames: Vec<Vec<u8>> = (0..ANIM_FRAMES)
        .map(|n| animation_frame(width, height, n))
        .collect();

    let tmp = tempfile::tempdir().context("creating tempdir for animation fixture")?;
    let mut png_paths = Vec::with_capacity(frames.len());
    for (n, rgba) in frames.iter().enumerate() {
        let png = tmp.path().join(format!("frame{n}.png"));
        write_png(&png, width, height, rgba)?;
        png_paths.push(png);
    }

    let input_webp = case_dir.join("input.webp");
    run_img2webp(img2webp, &png_paths, &input_webp)?;

    // Per-frame goldens: extract each frame with webpmux and decode with dwebp.
    // If img2webp had altered a frame (e.g. sub-rect optimization), this golden
    // would not equal the source and the check below fails closed.
    for (n, source) in frames.iter().enumerate() {
        let extracted = tmp.path().join(format!("get{n}.webp"));
        let index = u32::try_from(n).expect("frame index fits u32") + 1;
        webpmux_get_frame(webpmux, index, &input_webp, &extracted)?;
        let pam = tmp.path().join(format!("get{n}.pam"));
        run_dwebp(dwebp, &extracted, &pam)?;
        let golden = strip_pam_header(&std::fs::read(&pam)?)?;
        if &golden != source {
            bail!(
                "animation frame {n} golden (webpmux -get frame + dwebp) != source; \
                 img2webp did not keep it full-canvas/lossless"
            );
        }
        std::fs::write(case_dir.join(format!("expected.frame{n}.rgba")), &golden)?;
    }
    // Ledger golden: the first composited frame (== frame 0 for full-canvas).
    std::fs::write(case_dir.join("expected.rgba"), &frames[0])
        .with_context(|| format!("writing expected.rgba for {name}"))?;

    let note = format!(
        "{width}x{height} x{ANIM_FRAMES}-frame lossless animation authored by img2webp \
         {REQUIRED_LIBWEBP}; expected.frameN.rgba via webpmux `-get frame` + dwebp `-pam`; \
         expected.rgba is the first composited frame. Verifies our ANIM/ANMF reader + \
         compositor against libwebp."
    );
    let meta = format!(
        "# Generated by `cargo xtask gen-fixtures`. Do not hand-edit the goldens.\n\
         feature = \"animation\"\n\
         level = \"must\"\n\
         note = \"{note}\"\n"
    );
    std::fs::write(case_dir.join("meta.toml"), meta)
        .with_context(|| format!("writing meta.toml for {name}"))?;

    // GATE 1 (decode, fail-closed): every frame + composited canvas byte-exact.
    let file = std::fs::read(&input_webp)?;
    let decoded: Vec<_> = webpkit::lossless::decode_frames(&file)
        .context("webpkit::lossless decode_frames of animation fixture")?
        .collect::<webpkit::lossless::Result<_>>()
        .context("decoding an animation frame")?;
    if decoded.len() != frames.len() {
        bail!(
            "animation gate: webpkit::lossless decoded {} frames, expected {}",
            decoded.len(),
            frames.len()
        );
    }
    for (n, (frame, source)) in decoded.iter().zip(&frames).enumerate() {
        if frame.image().as_bytes() != source.as_slice() {
            bail!("animation gate: webpkit::lossless frame {n} pixels != source");
        }
    }
    let composited: Vec<_> = webpkit::lossless::decode_frames(&file)
        .context("webpkit::lossless decode_frames (composited)")?
        .composited()
        .collect::<webpkit::lossless::Result<_>>()
        .context("compositing an animation frame")?;
    for (n, (frame, source)) in composited.iter().zip(&frames).enumerate() {
        if frame.image().as_bytes() != source.as_slice() {
            bail!("animation gate: webpkit::lossless composited frame {n} != source");
        }
    }
    // decode() returns the first composited frame.
    let (_, first) = webpkit::lossless::decode_rgba(&file)
        .context("webpkit::lossless decode_rgba of animation")?;
    if first != frames[0] {
        bail!("animation gate: webpkit::lossless::decode_rgba(anim) != first frame");
    }

    // GATE 2 (encode, external): libwebp must read our AnimationEncoder output
    // back to the same frames.
    let ours = build_webpkit_animation(width, height, &frames)?;
    let our_webp = tmp.path().join("ours.webp");
    std::fs::write(&our_webp, &ours)?;
    for (n, source) in frames.iter().enumerate() {
        let extracted = tmp.path().join(format!("ours_get{n}.webp"));
        let index = u32::try_from(n).expect("frame index fits u32") + 1;
        webpmux_get_frame(webpmux, index, &our_webp, &extracted)?;
        let pam = tmp.path().join(format!("ours_get{n}.pam"));
        run_dwebp(dwebp, &extracted, &pam)?;
        let golden = strip_pam_header(&std::fs::read(&pam)?)?;
        if &golden != source {
            bail!("animation encode gate: libwebp read frame {n} of our animation != source");
        }
    }

    println!(
        "gen-fixtures: {name} ({width}x{height} x{ANIM_FRAMES}) OK \
         (img2webp author + decode/composite/encode gates)"
    );
    Ok(())
}

/// Generate one encode fixture, enforcing both round-trip gates (fail-closed).
///
/// Writes the raw RGBA source and its `meta.toml`, then gates:
/// * **GATE1 (self, tool-free)** — `webpkit::lossless::decode(webpkit::lossless::encode(rgba)) == rgba`.
/// * **GATE2 (independent)** — our encoder's bytes decode through libwebp
///   `dwebp -pam` back to the same RGBA.
///
/// Only the source and metadata are committed; encode goldens are intentionally
/// omitted because a valid VP8L stream is not unique (it is version-dependent),
/// so GATE2 runs here, at generation time, rather than in the committed ledger.
fn generate_encode_case(case: &FixtureCase, encode_dir: &Path, dwebp: &str) -> Result<()> {
    let case_dir = encode_dir.join(case.name);
    std::fs::create_dir_all(&case_dir)
        .with_context(|| format!("creating {}", case_dir.display()))?;

    let rgba = synthesize(case);
    std::fs::write(case_dir.join("input.rgba"), &rgba)
        .with_context(|| format!("writing input.rgba for {}", case.name))?;
    write_encode_meta(&case_dir, case)?;

    let webp = webpkit_encode(&rgba, case.width, case.height)
        .with_context(|| format!("webpkit::lossless encode failed for {}", case.name))?;

    // GATE1 (self, tool-free): our decoder must restore our encoder's output.
    let (dims, ours) = webpkit::lossless::decode_rgba(&webp)
        .with_context(|| format!("webpkit::lossless decode failed for {}", case.name))?;
    let dims_match = dims.width() == case.width && dims.height() == case.height;
    if !dims_match || ours != rgba {
        bail!(
            "GATE1 self round-trip failed for encode case `{}`: \
             webpkit::lossless::decode(webpkit::lossless::encode(rgba)) != rgba",
            case.name
        );
    }

    // GATE2 (independent): libwebp `dwebp` must restore our encoder's output.
    let tmp = tempfile::tempdir().context("creating tempdir for encode gate")?;
    let our_webp = tmp.path().join("ours.webp");
    std::fs::write(&our_webp, &webp).with_context(|| format!("writing {}", our_webp.display()))?;
    let golden_pam = tmp.path().join("golden.pam");
    run_dwebp(dwebp, &our_webp, &golden_pam)?;
    let pam =
        std::fs::read(&golden_pam).with_context(|| format!("reading {}", golden_pam.display()))?;
    let restored = strip_pam_header(&pam)?;
    if restored != rgba {
        bail!(
            "GATE2 independent round-trip failed for encode case `{}`: dwebp output \
             ({} bytes) != source ({} bytes); webpkit::lossless emitted a stream dwebp restores \
             differently",
            case.name,
            restored.len(),
            rgba.len()
        );
    }

    println!(
        "gen-fixtures: encode/{} ({}x{}) OK (self + dwebp gates)",
        case.name, case.width, case.height
    );
    Ok(())
}

pub(crate) fn gen_fixtures() -> Result<()> {
    let root = workspace_root()?;
    let decode_dir = decode_fixtures_dir(&root);
    let encode_dir = encode_fixtures_dir(&root);
    let cwebp = cwebp_bin();
    let dwebp = dwebp_bin();
    let webpmux = webpmux_bin();
    let img2webp = img2webp_bin();

    // Version guard: refuse to generate goldens with a non-pinned libwebp.
    check_version(&cwebp, "cwebp", "WEBPKIT_CWEBP")?;
    check_version(&dwebp, "dwebp", "WEBPKIT_DWEBP")?;
    check_version(&webpmux, "webpmux", "WEBPKIT_WEBPMUX")?;
    check_version(&img2webp, "img2webp", "WEBPKIT_IMG2WEBP")?;

    let cases = cases();
    for case in &cases {
        generate_case(case, &decode_dir, &cwebp, &dwebp)?;
    }
    // The extended-container fixture: a libwebp `webpmux`-authored VP8X file.
    generate_metadata_case(&decode_dir, &cwebp, &dwebp, &webpmux)?;
    // The animation fixture: a libwebp `img2webp`-authored ANIM/ANMF file.
    generate_animation_case(&decode_dir, &dwebp, &webpmux, &img2webp)?;
    println!(
        "gen-fixtures: {} decode case(s) written to {}",
        cases.len() + 2,
        decode_dir.display()
    );

    // Reuse the same source matrix for the encode fixtures.
    for case in &cases {
        generate_encode_case(case, &encode_dir, &dwebp)?;
    }
    println!(
        "gen-fixtures: {} encode case(s) written to {}",
        cases.len(),
        encode_dir.display()
    );
    Ok(())
}
