//! Real-image multi-axis scoreboard (`scoreboard --real <dir>`): a print-only,
//! never-gated per-category comparison of webpkit against libwebp on size,
//! fidelity, encode/decode speed, and webpkit peak memory.
//!
//! Rigor is uneven by column and stated as such (see `docs/benchmarking.md`):
//! size and fidelity are exact and deterministic; webpkit peak memory is exact (the counting allocator); speed is the noisy timing plane (median of repeats,
//! non-deterministic); libwebp peak memory is omitted (its C `malloc` is invisible to the Rust allocator).
//! libwebp numbers need the in-process reference behind the `scoreboard-libwebp` feature; without it only the webpkit columns are shown.

use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};

use crate::bench::{RealImage, RealPrep, prepare_real_images};
use crate::libwebp::{check_version, cwebp_bin};
use crate::metrics::{extract_vp8_chunk, psnr_rgb, ssim_rgb};

/// libwebp reference, present only behind the `scoreboard-libwebp` feature.
/// The two module bodies give every call site one uniform signature; the stubs are reached only when [`lw::AVAILABLE`] is `false`, which gates every libwebp column.
#[cfg(feature = "scoreboard-libwebp")]
mod lw {
    /// Whether the in-process libwebp reference is linked (the feature is on).
    pub(super) const AVAILABLE: bool = true;
    pub(super) use webpkit_libwebp_timing::{
        decode_rgba_len, decoder_version, encode_lossless_len, encode_lossy_bytes, encode_lossy_len,
    };
}
#[cfg(not(feature = "scoreboard-libwebp"))]
mod lw {
    /// Whether the in-process libwebp reference is linked (the feature is off).
    pub(super) const AVAILABLE: bool = false;
    pub(super) const fn decoder_version() -> String {
        String::new()
    }
    pub(super) const fn encode_lossless_len(_: &[u8], _: u32, _: u32) -> usize {
        0
    }
    pub(super) const fn encode_lossy_bytes(_: &[u8], _: u32, _: u32, _: f32) -> Vec<u8> {
        Vec::new()
    }
    pub(super) const fn encode_lossy_len(_: &[u8], _: u32, _: u32, _: f32) -> usize {
        0
    }
    pub(super) const fn decode_rgba_len(_: &[u8]) -> usize {
        0
    }
}

/// Passes each op is timed; the median is reported (odd count → true middle).
/// Kept small because the webpkit `AUTO` lossless encode is deep (Best-tier at ≤1MP);
/// timing is the noisy plane regardless (never gated).
const TIMING_ITERS: u32 = 3;

/// The single quality the lossy section runs both encoders at (webpkit `AUTO`,
/// libwebp simple-API default shaping) — a representative default, not a sweep.
const LOSSY_QUALITY: u8 = 75;

/// A real image's pixel-derived content category (never its filename), so the rollup names no private file.
/// The four buckets the corpus spans, mapped to the pixels that define them (see [`classify`]).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cat {
    /// Alpha present, small distinct-color count — flat UI icons.
    FlatIcon,
    /// Alpha present, large distinct-color count — transparent graphics/exports.
    Transparent,
    /// Opaque, small distinct-color count — flat charts / line art.
    Graphic,
    /// Opaque, large distinct-color count — photographs and museum scans.
    Photo,
}

impl Cat {
    /// The categories in printed/aggregation order.
    const ALL: [Self; 4] = [
        Self::FlatIcon,
        Self::Transparent,
        Self::Graphic,
        Self::Photo,
    ];

    /// The row label used in the printed rollup.
    const fn label(self) -> &'static str {
        match self {
            Self::FlatIcon => "flat-icon",
            Self::Transparent => "transparent",
            Self::Graphic => "graphic",
            Self::Photo => "photo",
        }
    }

    /// Dense index into the per-category accumulator array.
    const fn idx(self) -> usize {
        match self {
            Self::FlatIcon => 0,
            Self::Transparent => 1,
            Self::Graphic => 2,
            Self::Photo => 3,
        }
    }
}

/// Alpha-present flat content stays under this many distinct RGB triples.
const FLAT_ICON_MAX_COLORS: usize = 4096;

/// Opaque photographic content exceeds this many distinct RGB triples; below it is flat opaque graphics (shared with the lossy AUTO sweep's threshold).
const GRAPHIC_MAX_COLORS: usize = 8192;

/// Classify a decoded RGBA buffer from its pixels alone: transparency first, then a distinct-color count capped to the relevant threshold.
/// No filename is inspected.
fn classify(rgba: &[u8]) -> Cat {
    let transparent = rgba.as_chunks::<4>().0.iter().any(|p| p[3] < 255);
    let cap = if transparent {
        FLAT_ICON_MAX_COLORS
    } else {
        GRAPHIC_MAX_COLORS
    };
    let mut colors = std::collections::BTreeSet::new();
    let mut over = false;
    for p in rgba.as_chunks::<4>().0 {
        colors.insert([p[0], p[1], p[2]]);
        if colors.len() > cap {
            over = true;
            break;
        }
    }
    match (transparent, over) {
        (true, false) => Cat::FlatIcon,
        (true, true) => Cat::Transparent,
        (false, false) => Cat::Graphic,
        (false, true) => Cat::Photo,
    }
}

/// Running per-category totals for one mode (lossless or lossy).
/// Sizes are exact byte sums; speed is summed work / summed seconds (a size-weighted throughput);
/// memory is the category's worst-case webpkit peak; fidelity is averaged by `n`.
#[derive(Default, Clone, Copy)]
struct Acc {
    /// Images folded into this bucket.
    n: u32,
    /// Summed webpkit encoded bytes (full WebP for lossless, `VP8 ` payload for lossy).
    ours_bytes: u64,
    /// Summed libwebp encoded bytes on the same basis (`0` if libwebp is absent).
    lib_bytes: u64,
    /// Summed (ours − libwebp) reconstruction PSNR dB, lossy only.
    d_psnr: f64,
    /// Summed (ours − libwebp) reconstruction MSSIM, lossy only.
    d_ssim: f64,
    /// Summed webpkit encode raw-input bytes and seconds (throughput = work/secs).
    enc_ours_work: f64,
    enc_ours_secs: f64,
    /// Summed libwebp encode raw-input bytes and seconds.
    enc_lib_work: f64,
    enc_lib_secs: f64,
    /// Summed decoded pixels and webpkit decode seconds.
    dec_ours_px: f64,
    dec_ours_secs: f64,
    /// Summed decoded pixels and libwebp decode seconds.
    dec_lib_px: f64,
    dec_lib_secs: f64,
    /// Worst-case webpkit encode / decode peak additional bytes across the bucket.
    mem_enc: usize,
    mem_dec: usize,
}

impl Acc {
    /// Fold this bucket into `other` (the `overall` rollup): sums add, memory maxes.
    fn add_into(self, other: &mut Self) {
        other.n += self.n;
        other.ours_bytes += self.ours_bytes;
        other.lib_bytes += self.lib_bytes;
        other.d_psnr += self.d_psnr;
        other.d_ssim += self.d_ssim;
        other.enc_ours_work += self.enc_ours_work;
        other.enc_ours_secs += self.enc_ours_secs;
        other.enc_lib_work += self.enc_lib_work;
        other.enc_lib_secs += self.enc_lib_secs;
        other.dec_ours_px += self.dec_ours_px;
        other.dec_ours_secs += self.dec_ours_secs;
        other.dec_lib_px += self.dec_lib_px;
        other.dec_lib_secs += self.dec_lib_secs;
        other.mem_enc = other.mem_enc.max(self.mem_enc);
        other.mem_dec = other.mem_dec.max(self.mem_dec);
    }
}

/// `num * 100 / den` as a one-decimal percent string (integer permille math, no float cast), or `-` when `den` is zero (libwebp absent).
fn pct(num: u64, den: u64) -> String {
    if den == 0 {
        return "-".to_owned();
    }
    let tenths = num.saturating_mul(1000) / den;
    format!("{}.{}", tenths / 10, tenths % 10)
}

/// Throughput in millions of units per second, adaptive precision (mirrors the `bench-real` formatter).
/// `inf` guards a sub-tick zero duration; `-` a bucket with no timing (libwebp absent).
fn format_throughput(work: f64, secs: f64) -> String {
    if work <= 0.0 {
        return "-".to_owned();
    }
    if secs <= 0.0 {
        return "inf".to_owned();
    }
    let v = work / secs / 1_000_000.0;
    if v >= 100.0 {
        format!("{v:.0}")
    } else if v >= 1.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.3}")
    }
}

/// Median wall-clock seconds of `f` over [`TIMING_ITERS`] passes (`black_box`ing the result so the op is not optimized away).
/// The timing plane: noisy, never gated.
fn median_secs(mut f: impl FnMut() -> usize) -> f64 {
    let mut times = Vec::new();
    for _ in 0..TIMING_ITERS {
        let start = Instant::now();
        let n = f();
        let elapsed = start.elapsed();
        std::hint::black_box(n);
        times.push(elapsed);
    }
    times.sort_unstable();
    times[times.len() / 2].as_secs_f64()
}

/// Expand row-major RGBA to RGB (drop alpha), the input basis both lossy encoders see (webpkit drops alpha; libwebp is fed RGB).
fn rgba_to_rgb(rgba: &[u8]) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(rgba.len() / 4 * 3);
    for p in rgba.as_chunks::<4>().0 {
        rgb.extend_from_slice(&p[0..3]);
    }
    rgb
}

/// Wrap a bare `VP8 ` payload in a minimal RIFF/WEBP container so libwebp's container decoder can time the exact same compressed bytes our payload decoder consumes (apples-to-apples decode speed on one stream).
fn wrap_vp8(payload: &[u8]) -> Vec<u8> {
    let chunk_len = u32::try_from(payload.len()).unwrap_or(0);
    let pad = payload.len() & 1;
    let riff_size = u32::try_from(4 + 8 + payload.len() + pad).unwrap_or(0);
    let mut out = Vec::with_capacity(8 + 4 + 8 + payload.len() + pad);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_size.to_le_bytes());
    out.extend_from_slice(b"WEBP");
    out.extend_from_slice(b"VP8 ");
    out.extend_from_slice(&chunk_len.to_le_bytes());
    out.extend_from_slice(payload);
    if pad == 1 {
        out.push(0);
    }
    out
}

/// Fold one image's lossless measurements into `acc`: webpkit `AUTO` vs libwebp's simple-API default, both on identical resized pixels.
/// Fidelity is exact (both reproduce the source), so only size, speed, and webpkit memory are recorded.
fn measure_lossless(img: &RealImage, acc: &mut Acc) -> Result<()> {
    let (w, h) = (img.dims.width(), img.dims.height());
    let rgba = img.rgba.as_slice();
    let cfg = webpkit::lossless::EncoderConfig::new().with_effort(webpkit::lossless::Effort::AUTO);

    // One measured encode yields size, exact peak memory, and a genuine webpkit stream reused for the decode memory + timing (both decoders time these bytes).
    let image =
        webpkit::lossless::ImageRef::new(img.dims, webpkit::lossless::PixelLayout::Rgba8, rgba)?;
    webpkit_alloc_count::reset_peak();
    let our_webp = webpkit::lossless::encode(image, &cfg)?;
    acc.mem_enc = acc.mem_enc.max(webpkit_alloc_count::peak_since_reset());
    acc.ours_bytes += our_webp.len() as u64;
    if lw::AVAILABLE {
        acc.lib_bytes += lw::encode_lossless_len(rgba, w, h) as u64;
    }
    webpkit_alloc_count::reset_peak();
    let _ = webpkit::lossless::decode_rgba(&our_webp)?;
    acc.mem_dec = acc.mem_dec.max(webpkit_alloc_count::peak_since_reset());

    // Speed (timing plane).
    let raw = f64::from(w) * f64::from(h) * 4.0;
    let px = f64::from(w) * f64::from(h);
    acc.enc_ours_work += raw;
    acc.enc_ours_secs += median_secs(|| {
        let image =
            webpkit::lossless::ImageRef::new(img.dims, webpkit::lossless::PixelLayout::Rgba8, rgba)
                .unwrap();
        webpkit::lossless::encode(image, &cfg).map_or(0, |v| v.len())
    });
    acc.dec_ours_px += px;
    acc.dec_ours_secs +=
        median_secs(|| webpkit::lossless::decode_rgba(&our_webp).map_or(0, |v| v.1.len()));
    if lw::AVAILABLE {
        acc.enc_lib_work += raw;
        acc.enc_lib_secs += median_secs(|| lw::encode_lossless_len(rgba, w, h));
        acc.dec_lib_px += px;
        acc.dec_lib_secs += median_secs(|| lw::decode_rgba_len(&our_webp));
    }

    acc.n += 1;
    Ok(())
}

/// Fold one image's lossy measurements into `acc`: webpkit `AUTO` vs libwebp default shaping at [`LOSSY_QUALITY`], both on RGB.
/// Both encoders' `VP8 ` payloads are decoded by webpkit's decoder for an apples-to-apples PSNR/SSIM.
fn measure_lossy(img: &RealImage, acc: &mut Acc) -> Result<()> {
    let (w, h) = (img.dims.width(), img.dims.height());
    let (wu, hu) = (w as usize, h as usize);
    let rgba = img.rgba.as_slice();
    let rgb = rgba_to_rgb(rgba);
    let dims = webpkit::lossy::Dimensions::new(w, h)?;
    let cfg = webpkit::lossy::LossyConfig::new()
        .with_quality(LOSSY_QUALITY)
        .with_effort(webpkit::lossy::Effort::AUTO);

    // Ours: one measured encode yields size, exact encode peak, and the payload reused for reconstruction + decode memory/timing.
    let image = webpkit::lossy::ImageRef::new(dims, webpkit::lossy::PixelLayout::Rgba8, rgba)?;
    webpkit_alloc_count::reset_peak();
    let (_d, our_payload) = webpkit::lossy::encode_vp8(image, &cfg)?;
    acc.mem_enc = acc.mem_enc.max(webpkit_alloc_count::peak_since_reset());
    acc.ours_bytes += our_payload.len() as u64;
    webpkit_alloc_count::reset_peak();
    let our_dec = webpkit::lossy::decode(&our_payload)?;
    acc.mem_dec = acc.mem_dec.max(webpkit_alloc_count::peak_since_reset());

    // libwebp: size + reconstruction (its VP8 payload decoded by OUR decoder too).
    if lw::AVAILABLE {
        let lib_file = lw::encode_lossy_bytes(&rgb, w, h, f32::from(LOSSY_QUALITY));
        let lib_vp8 = extract_vp8_chunk(&lib_file)?;
        acc.lib_bytes += lib_vp8.len() as u64;
        let lib_dec = webpkit::lossy::decode(lib_vp8)?;
        acc.d_psnr += psnr_rgb(rgba, our_dec.as_bytes()) - psnr_rgb(rgba, lib_dec.as_bytes());
        acc.d_ssim +=
            ssim_rgb(rgba, our_dec.as_bytes(), wu, hu) - ssim_rgb(rgba, lib_dec.as_bytes(), wu, hu);
    }

    // Speed (timing plane).
    // Decode times the SAME VP8 bytes on both sides: our payload for our decoder, the same payload wrapped in a container for libwebp.
    let raw = f64::from(w) * f64::from(h) * 3.0;
    let px = f64::from(w) * f64::from(h);
    let container = wrap_vp8(&our_payload);
    acc.enc_ours_work += raw;
    acc.enc_ours_secs += median_secs(|| {
        let image =
            webpkit::lossy::ImageRef::new(dims, webpkit::lossy::PixelLayout::Rgba8, rgba).unwrap();
        webpkit::lossy::encode_vp8(image, &cfg).map_or(0, |(_d, v)| v.len())
    });
    acc.dec_ours_px += px;
    acc.dec_ours_secs +=
        median_secs(|| webpkit::lossy::decode(&our_payload).map_or(0, |i| i.as_bytes().len()));
    if lw::AVAILABLE {
        acc.enc_lib_work += raw;
        acc.enc_lib_secs +=
            median_secs(|| lw::encode_lossy_len(&rgb, w, h, f32::from(LOSSY_QUALITY)));
        acc.dec_lib_px += px;
        acc.dec_lib_secs += median_secs(|| lw::decode_rgba_len(&container));
    }

    acc.n += 1;
    Ok(())
}

/// Print the lossless section: one row per category plus `overall`.
fn print_lossless(acc: &[Acc; 4]) {
    println!();
    println!(
        "LOSSLESS  (webpkit AUTO vs libwebp default; both reproduce the source, so no fidelity):"
    );
    println!(
        "  {:<12} {:>3} {:>12} {:>12} {:>6} {:>9} {:>9} {:>9} {:>9} {:>8} {:>8}",
        "category",
        "n",
        "ours B",
        "lib B",
        "size%",
        "encO MB/s",
        "encL MB/s",
        "decO MP/s",
        "decL MP/s",
        "encKiB",
        "decKiB",
    );
    let mut overall = Acc::default();
    for cat in Cat::ALL {
        print_lossless_row(cat.label(), acc[cat.idx()]);
        acc[cat.idx()].add_into(&mut overall);
    }
    print_lossless_row("overall", overall);
}

/// Print one lossless rollup row (skips an empty bucket).
fn print_lossless_row(label: &str, a: Acc) {
    if a.n == 0 {
        return;
    }
    let lib_b = if lw::AVAILABLE {
        a.lib_bytes.to_string()
    } else {
        "n/a".to_owned()
    };
    println!(
        "  {label:<12} {:>3} {:>12} {:>12} {:>6} {:>9} {:>9} {:>9} {:>9} {:>8} {:>8}",
        a.n,
        a.ours_bytes,
        lib_b,
        pct(a.ours_bytes, a.lib_bytes),
        format_throughput(a.enc_ours_work, a.enc_ours_secs),
        format_throughput(a.enc_lib_work, a.enc_lib_secs),
        format_throughput(a.dec_ours_px, a.dec_ours_secs),
        format_throughput(a.dec_lib_px, a.dec_lib_secs),
        a.mem_enc / 1024,
        a.mem_dec / 1024,
    );
}

/// Print the lossy section: one row per category plus `overall`.
fn print_lossy(acc: &[Acc; 4]) {
    println!();
    println!(
        "LOSSY q{LOSSY_QUALITY}  (webpkit AUTO vs libwebp default; bytes = VP8 payload, both decoded by webpkit):"
    );
    println!(
        "  size% = ours*100/lib (<100 = ours smaller); +dB / +SSIM = ours - lib (>0 = ours better)"
    );
    println!(
        "  {:<12} {:>3} {:>12} {:>12} {:>6} {:>7} {:>8} {:>9} {:>9} {:>9} {:>9} {:>8} {:>8}",
        "category",
        "n",
        "ours B",
        "lib B",
        "size%",
        "+dB",
        "+SSIM",
        "encO MB/s",
        "encL MB/s",
        "decO MP/s",
        "decL MP/s",
        "encKiB",
        "decKiB",
    );
    let mut overall = Acc::default();
    for cat in Cat::ALL {
        print_lossy_row(cat.label(), acc[cat.idx()]);
        acc[cat.idx()].add_into(&mut overall);
    }
    print_lossy_row("overall", overall);
}

/// Print one lossy rollup row (skips an empty bucket).
fn print_lossy_row(label: &str, a: Acc) {
    if a.n == 0 {
        return;
    }
    let n = f64::from(a.n);
    let (lib_b, d_db, d_ssim) = if lw::AVAILABLE {
        (
            a.lib_bytes.to_string(),
            format!("{:+.2}", a.d_psnr / n),
            format!("{:+.4}", a.d_ssim / n),
        )
    } else {
        ("n/a".to_owned(), "n/a".to_owned(), "n/a".to_owned())
    };
    println!(
        "  {label:<12} {:>3} {:>12} {:>12} {:>6} {d_db:>7} {d_ssim:>8} {:>9} {:>9} {:>9} {:>9} {:>8} {:>8}",
        a.n,
        a.ours_bytes,
        lib_b,
        pct(a.ours_bytes, a.lib_bytes),
        format_throughput(a.enc_ours_work, a.enc_ours_secs),
        format_throughput(a.enc_lib_work, a.enc_lib_secs),
        format_throughput(a.dec_ours_px, a.dec_ours_secs),
        format_throughput(a.dec_lib_px, a.dec_lib_secs),
        a.mem_enc / 1024,
        a.mem_dec / 1024,
    );
}

/// Print (never gate) the real-image multi-axis scoreboard over `real`.
///
/// PRIVACY: `real` is a pure runtime path; images are aggregated by pixel-derived
/// [`Cat`] (never the filename) and no file name is printed or recorded. Every temp
/// file lives in a dropped `tempfile::tempdir()` (inside `prepare_real_images`), so
/// nothing is written into the repo. Needs `cwebp` to READ the source images into
/// RGBA; libwebp comparison columns additionally need the `scoreboard-libwebp`
/// feature (else they print `n/a`). Soft-skips when `cwebp` is absent.
pub(crate) fn scoreboard(real: &Path, max_edge: u32) -> Result<()> {
    let cwebp = cwebp_bin();
    if let Err(e) = check_version(&cwebp, "cwebp", "WEBPKIT_CWEBP") {
        println!("scoreboard: skipped ({e})");
        return Ok(());
    }

    println!("webpkit vs libwebp real-image scoreboard (printed only, NOT gated):");
    match (max_edge > 0).then_some(max_edge) {
        Some(w) => println!("  input width capped to {w}px by cwebp (aspect preserved)"),
        None => println!("  native resolution (no resize)"),
    }
    if lw::AVAILABLE {
        println!(
            "  libwebp: in-process libwebp-sys {} reference, shipped simple-API defaults",
            lw::decoder_version()
        );
    } else {
        println!(
            "  libwebp: NOT linked — rebuild with `--features scoreboard-libwebp` for libwebp columns"
        );
    }
    println!(
        "  size = exact bytes (rigorous); fidelity = exact, both decoded by webpkit (rigorous);"
    );
    println!(
        "  speed = median of {TIMING_ITERS} in-process passes (timing plane: noisy, non-deterministic);"
    );
    println!(
        "  encKiB/decKiB = webpkit peak additional alloc (exact); libwebp memory omitted (C malloc)."
    );

    let prepared = prepare_real_images(&cwebp, real, max_edge)?;
    let mut lossless = [Acc::default(); 4];
    let mut lossy = [Acc::default(); 4];
    let mut skipped = 0usize;

    for item in &prepared {
        let RealPrep::Ready(img) = item else {
            skipped += 1;
            continue;
        };
        let cat = classify(&img.rgba);
        measure_lossless(img, &mut lossless[cat.idx()])
            .with_context(|| format!("lossless measurement of a {} image", cat.label()))?;
        measure_lossy(img, &mut lossy[cat.idx()])
            .with_context(|| format!("lossy measurement of a {} image", cat.label()))?;
    }

    if lossless.iter().all(|a| a.n == 0) {
        println!();
        println!("  (no comparable images in {})", real.display());
        return Ok(());
    }
    print_lossless(&lossless);
    print_lossy(&lossy);
    if skipped > 0 {
        println!();
        println!("  ({skipped} non-image file(s) skipped)");
    }
    Ok(())
}
