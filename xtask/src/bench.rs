//! Real-image benchmarking (`bench-real`) and the shared `cwebp`-backed
//! directory-to-ARGB front end reused by `metrics --real`. Print-only and never
//! gated: the timing plane (see `docs/benchmarking.md`).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

use crate::libwebp::{check_version, cwebp_bin, run_cwebp, run_cwebp_resized};

/// One real-image directory entry decoded to ARGB, ready for measurement.
///
/// `cwebp_size` is the size of the (optionally width-capped) `cwebp` lossless
/// encoding these exact pixels came from — the reference column for the size
/// comparison; the timing harness ignores it.
pub(crate) struct RealImage {
    /// Source file name (or `<non-utf8>`), for the printed row label.
    pub(crate) name: String,
    /// Decoded pixel dimensions (post-resize).
    pub(crate) dims: webpkit::lossless::Dimensions,
    /// Row-major RGBA8 pixels the codec is measured on.
    pub(crate) rgba: Vec<u8>,
    /// Byte length of the `cwebp` stream these pixels were decoded from.
    pub(crate) cwebp_size: u64,
}

/// Per-file outcome of [`prepare_real_images`]: either decoded pixels or a skip
/// notice for a file `cwebp` could not read. Order and name are preserved so both
/// the size (`metrics --real`) and timing (`bench-real`) consumers print stable,
/// matching rows.
pub(crate) enum RealPrep {
    /// `cwebp` read the file; pixels are ready.
    Ready(RealImage),
    /// `cwebp` could not read the file (non-image, unsupported, etc.).
    Skipped {
        /// Source file name (or `<non-utf8>`), for the printed `skipped` row.
        name: String,
    },
}

/// Decode a directory of real images to ARGB by shelling out to `cwebp` (the only
/// image reader wired into this tool), the shared front end of `metrics --real`
/// (size) and `bench-real` (timing).
///
/// PRIVACY: `real` is a pure runtime path; every temporary file lives inside a
/// `tempfile::tempdir()` dropped on return, so nothing is written into the repo
/// and no image path/name is baked in. Plain files only (subdirectories skipped),
/// visited in sorted name order for stable output. `cwebp` losslessly encodes each
/// source — width-capped via `-resize <max_edge> 0` (aspect preserved) when
/// `max_edge > 0`, else native — then our decoder reads the exact resized pixels
/// back, so every downstream consumer sees byte-identical input. A file `cwebp`
/// cannot read becomes a [`RealPrep::Skipped`] row, never an abort.
pub(crate) fn prepare_real_images(
    cwebp: &str,
    real: &Path,
    max_edge: u32,
) -> Result<Vec<RealPrep>> {
    // Enumerate plain files only (skip subdirs); sort by path for stable output.
    let mut files: Vec<PathBuf> = std::fs::read_dir(real)
        .with_context(|| format!("reading {}", real.display()))?
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    files.sort();

    let resize = (max_edge > 0).then_some(max_edge);
    let tmp = tempfile::tempdir().context("creating tempdir for real-image preparation")?;
    let out_webp = tmp.path().join("out.webp");

    let mut prepared = Vec::with_capacity(files.len());
    for path in &files {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .map_or_else(|| "<non-utf8>".to_owned(), str::to_owned);

        if run_cwebp(cwebp, path, &out_webp, resize).is_err() {
            prepared.push(RealPrep::Skipped { name });
            continue;
        }
        let webp_bytes =
            std::fs::read(&out_webp).with_context(|| format!("reading {}", out_webp.display()))?;
        let cwebp_size = webp_bytes.len() as u64;
        let (dims, rgba) = webpkit::lossless::decode_rgba(&webp_bytes)?;
        prepared.push(RealPrep::Ready(RealImage {
            name,
            dims,
            rgba,
            cwebp_size,
        }));
    }
    Ok(prepared)
}

/// Prepare real images with the longest dimension capped to `max_edge`.
///
/// Header-only readers establish native dimensions for supported source formats.
/// Oversized landscape images are then width-capped and oversized portrait images are height-capped, with the other dimension left at zero so cwebp preserves aspect ratio.
/// An unknown header format falls back to one native cwebp pass and,
/// only when oversized, one resized pass.
/// No source name is needed by the `surpass` consumer.
pub(crate) fn prepare_real_images_max_edge(
    cwebp: &str,
    real: &Path,
    max_edge: u32,
) -> Result<Vec<RealPrep>> {
    let directory =
        std::fs::read_dir(real).with_context(|| format!("reading {}", real.display()))?;
    let mut files = Vec::new();
    for entry in directory {
        let path = entry.context("reading real-image directory entry")?.path();
        if path.is_file() && is_real_image_path(&path) {
            files.push(path);
        }
    }
    files.sort();

    let tmp = tempfile::tempdir().context("creating tempdir for real-image preparation")?;
    let out_webp = tmp.path().join("out.webp");
    let mut prepared = Vec::with_capacity(files.len());
    for path in &files {
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .map_or_else(|| "<non-utf8>".to_owned(), str::to_owned);
        let known_dimensions = source_dimensions(path);
        let resize = known_dimensions
            .and_then(|(width, height)| longest_edge_resize(width, height, max_edge));
        if run_cwebp_resized(cwebp, path, &out_webp, resize).is_err() {
            prepared.push(RealPrep::Skipped { name });
            continue;
        }
        let mut webp_bytes = std::fs::read(&out_webp).context("reading anonymous prepared WebP")?;
        let (mut dims, mut rgba) = webpkit::lossless::decode_rgba(&webp_bytes)?;
        // Unknown formats retain the old two-pass fallback: cwebp establishes dimensions once, then only oversized inputs are encoded a second time.
        if known_dimensions.is_none()
            && let Some(target) = longest_edge_resize(dims.width(), dims.height(), max_edge)
        {
            if run_cwebp_resized(cwebp, path, &out_webp, Some(target)).is_err() {
                prepared.push(RealPrep::Skipped { name });
                continue;
            }
            webp_bytes = std::fs::read(&out_webp).context("reading anonymous resized WebP")?;
            (dims, rgba) = webpkit::lossless::decode_rgba(&webp_bytes)?;
        }
        prepared.push(RealPrep::Ready(RealImage {
            name,
            dims,
            rgba,
            cwebp_size: u64::try_from(webp_bytes.len()).unwrap_or(u64::MAX),
        }));
    }
    Ok(prepared)
}

fn is_real_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "jpg"
                    | "jpeg"
                    | "png"
                    | "webp"
                    | "tif"
                    | "tiff"
                    | "bmp"
                    | "pam"
                    | "pgm"
                    | "ppm"
                    | "pnm"
            )
        })
}

/// Read only source dimensions, avoiding a full-resolution lossless cwebp pass.
fn source_dimensions(path: &Path) -> Option<(u32, u32)> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    match extension.as_str() {
        "webp" => {
            let bytes = std::fs::read(path).ok()?;
            let info = webpkit::probe(&bytes).ok()?;
            Some((info.dimensions.width(), info.dimensions.height()))
        },
        "png" => {
            let file = std::fs::File::open(path).ok()?;
            let reader = png::Decoder::new(std::io::BufReader::new(file))
                .read_info()
                .ok()?;
            Some((reader.info().width, reader.info().height))
        },
        _ => image::ImageReader::open(path)
            .ok()?
            .with_guessed_format()
            .ok()?
            .into_dimensions()
            .ok(),
    }
}

fn longest_edge_resize(width: u32, height: u32, max_edge: u32) -> Option<(u32, u32)> {
    if max_edge == 0 || width.max(height) <= max_edge {
        None
    } else if width >= height {
        Some((max_edge, 0))
    } else {
        Some((0, max_edge))
    }
}

/// Run `op` `iters` times, returning the fastest wall-clock duration (min = least
/// scheduler/cache noise). The op's result is discarded but its error is
/// propagated, so a codec failure aborts the measurement instead of timing a
/// broken path.
fn best_time<F>(iters: u32, mut op: F) -> Result<Duration>
where
    F: FnMut() -> Result<usize>,
{
    let mut best = Duration::MAX;
    for _ in 0..iters.max(1) {
        let start = Instant::now();
        let _ = op()?;
        best = best.min(start.elapsed());
    }
    Ok(best)
}

/// Throughput in millions of units per second (`units / seconds / 1e6`), one
/// decimal. Serves both MB/s (units = bytes) and Mpixels/s (units = pixels).
/// `inf` guards a sub-tick zero-duration measurement.
fn throughput_mpps(units: f64, elapsed: Duration) -> String {
    let secs = elapsed.as_secs_f64();
    if secs <= 0.0 {
        return "inf".to_owned();
    }
    // Adaptive precision: the range spans the slow deep-effort (`l9`) encode (~0.05)
    // to fast PNG-icon decode (~300+), so more decimals are kept for small values
    // where a single fixed decimal would collapse the serial-vs-rayon delta to 0.0.
    let v = units / secs / 1_000_000.0;
    if v >= 100.0 {
        format!("{v:.0}")
    } else if v >= 1.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.3}")
    }
}

/// Print (never gate) encode/decode throughput of our codec over the real images
/// in `real` — the timing counterpart of the size-focused `metrics --real`.
///
/// PRIVACY / print-only contract is identical to `compare_vs_libwebp_real`: the
/// path is a runtime argument, temp files live in a dropped `tempdir()`, nothing is
/// written to the repo and no image path is baked in. Timing belongs to the
/// criterion plane (see `docs/benchmarking.md`), so this is never gated; it needs
/// libwebp (`cwebp`) to read sources into ARGB and soft-skips when it is absent.
///
/// Each image is encoded at the adaptive `AUTO` effort and the deepest level
/// (`l9`) and decoded from our own `l9` stream (so the timed bytes are a real
/// webpkit stream, not
/// cwebp's); every op is timed best-of-`iters`. Encode reports raw-RGBA MB/s and
/// decode reports Mpixels/s, matching the criterion benches' throughput basis.
/// Wall-clock / `f64` math is fine here: xtask is a CLI boundary, not the codec.
pub(crate) fn bench_real(real: &Path, max_edge: u32, iters: u32, limit: usize) -> Result<()> {
    let cwebp = cwebp_bin();
    // Probe cwebp; soft-skip (never fail) when it is absent or the wrong version.
    if let Err(e) = check_version(&cwebp, "cwebp", "WEBPKIT_CWEBP") {
        println!("bench-real: skipped ({e})");
        return Ok(());
    }

    println!();
    println!("webpkit encode/decode throughput on real images (printed only, NOT gated):");
    match (max_edge > 0).then_some(max_edge) {
        Some(width) => println!("  input width capped to {width}px by cwebp (aspect preserved)"),
        None => println!("  native resolution (no resize)"),
    }
    println!(
        "  best-of-{}; enc = raw RGBA MB/s, dec = Mpixels/s (higher = faster)",
        iters.max(1)
    );
    println!(
        "  {:<32} {:>11} {:>12} {:>12} {:>12}",
        "file", "dims", "enc auto MB/s", "enc l9 MB/s", "dec MP/s"
    );

    let mut prepared = prepare_real_images(&cwebp, real, max_edge)?;
    // `--limit N` caps the (heavy) run to the first N entries for a fast smoke.
    if limit > 0 {
        prepared.truncate(limit);
    }
    let auto = webpkit::lossless::EncoderConfig::new().with_effort(webpkit::lossless::Effort::AUTO);
    let best =
        webpkit::lossless::EncoderConfig::new().with_effort(webpkit::lossless::Effort::level(9));

    let mut measured = 0usize;
    for item in &prepared {
        let RealImage {
            name, dims, rgba, ..
        } = match item {
            RealPrep::Skipped { name } => {
                println!(
                    "  {name:<32} {:>11} {:>12} {:>12} {:>12}",
                    "-", "-", "-", "skipped"
                );
                continue;
            },
            RealPrep::Ready(image) => image,
        };

        // Raw sizes as f64 without a lossy usize cast: pixels = w*h, bytes = 4*px.
        let pixels = f64::from(dims.width()) * f64::from(dims.height());
        let raw_bytes = pixels * 4.0;

        // Encode is timed on a fresh borrow each pass (no per-pass pixel-buffer
        // allocation); the returned stream length keeps the encode from being
        // optimized away.
        let enc_bal = best_time(iters, || {
            let image = webpkit::lossless::ImageRef::new(
                *dims,
                webpkit::lossless::PixelLayout::Rgba8,
                rgba,
            )?;
            Ok(webpkit::lossless::encode(image, &auto)?.len())
        })?;
        let enc_best = best_time(iters, || {
            let image = webpkit::lossless::ImageRef::new(
                *dims,
                webpkit::lossless::PixelLayout::Rgba8,
                rgba,
            )?;
            Ok(webpkit::lossless::encode(image, &best)?.len())
        })?;

        // Decode is timed on our own Best output (a genuine webpkit stream).
        let image =
            webpkit::lossless::ImageRef::new(*dims, webpkit::lossless::PixelLayout::Rgba8, rgba)?;
        let best_stream = webpkit::lossless::encode(image, &best)?;
        let dec = best_time(iters, || {
            Ok(webpkit::lossless::decode_rgba(&best_stream)?.1.len())
        })?;

        let dims_label = format!("{}x{}", dims.width(), dims.height());
        let enc_bal_mbps = throughput_mpps(raw_bytes, enc_bal);
        let enc_best_mbps = throughput_mpps(raw_bytes, enc_best);
        let dec_mpps = throughput_mpps(pixels, dec);
        println!(
            "  {name:<32} {dims_label:>11} {enc_bal_mbps:>12} {enc_best_mbps:>12} {dec_mpps:>12}"
        );
        measured += 1;
    }

    if measured == 0 {
        println!("  (no comparable images in {})", real.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{is_real_image_path, longest_edge_resize};

    #[test]
    fn longest_edge_resize_selects_the_oversized_axis() {
        assert_eq!(longest_edge_resize(2049, 1025, 1024), Some((1024, 0)));
        assert_eq!(longest_edge_resize(1025, 2049, 1024), Some((0, 1024)));
        assert_eq!(longest_edge_resize(17, 31, 1024), None);
        assert_eq!(longest_edge_resize(2049, 1025, 0), None);
    }

    #[test]
    fn real_image_filter_ignores_unrelated_directory_entries() {
        assert!(is_real_image_path(Path::new("input.JPEG")));
        assert!(is_real_image_path(Path::new("input.webp")));
        assert!(!is_real_image_path(Path::new("notes.txt")));
        assert!(!is_real_image_path(Path::new("README")));
    }
}
