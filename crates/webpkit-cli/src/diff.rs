//! `webp diff`: fidelity metrics between two decoded images.
//!
//! This is CLI-side arithmetic over two RGBA buffers, not a codec knob: it works
//! on any two images — a source and its WebP, or two WebP files — at any time, unlike
//! an encoder's internal `-print_psnr`, which can only report during an encode.

use std::path::Path;

use serde::Serialize;

use crate::{
    codec,
    error::CliError,
    format::{self, InputFormat},
    io::{self, Source},
};

/// The result of comparing two equally sized images.
#[derive(Debug, Serialize)]
pub(crate) struct Comparison {
    /// Shared width in pixels.
    pub(crate) width: u32,
    /// Shared height in pixels.
    pub(crate) height: u32,
    /// RGB PSNR in dB, or `None` when the two are byte-identical (infinite PSNR).
    pub(crate) psnr: Option<f64>,
    /// The largest absolute per-channel difference (`0..=255`), over all of RGBA.
    pub(crate) max_delta: u8,
}

impl Comparison {
    /// Whether this comparison meets a minimum-PSNR predicate. A byte-identical
    /// pair (`psnr: None`, infinite PSNR) meets any threshold.
    #[must_use]
    pub(crate) fn meets(&self, min_psnr: f64) -> bool {
        self.psnr.is_none_or(|psnr| psnr >= min_psnr)
    }
}

/// Compare two image files, decoding each to RGBA8 first.
///
/// # Errors
///
/// [`CliError`] if either file cannot be read or decoded, or [`CliError::Usage`]
/// if the two images have different dimensions (PSNR is undefined then).
pub(crate) fn compare(a: &Path, b: &Path) -> Result<Comparison, CliError> {
    let (aw, ah, ap) = load_rgba(a)?;
    let (bw, bh, bp) = load_rgba(b)?;
    if (aw, ah) != (bw, bh) {
        return Err(CliError::Usage(format!(
            "cannot compare images of different sizes: {} is {aw}x{ah}, {} is {bw}x{bh}",
            a.display(),
            b.display(),
        )));
    }
    Ok(Comparison {
        width: aw,
        height: ah,
        psnr: psnr_rgb(&ap, &bp),
        max_delta: max_delta(&ap, &bp),
    })
}

/// Read a file and decode it to `(width, height, rgba8)`, accepting a WebP (still
/// or the first frame of an animation) or any input format the encoder side reads.
fn load_rgba(path: &Path) -> Result<(u32, u32, Vec<u8>), CliError> {
    let bytes = Source::File(path.to_path_buf()).read()?;
    let image = if codec::is_webp(&bytes) {
        codec::decode_still_or_first_frame(&bytes, Some(webpkit::DEFAULT_MAX_PIXELS))?
    } else {
        let format = InputFormat::resolve(None, io::extension_of(path).as_deref(), &bytes);
        format::read_image(&bytes, format, None)?
    };
    Ok((image.width(), image.height(), format::to_rgba8(&image)))
}

/// RGB PSNR in dB, or `None` for a byte-identical pair.
///
/// The formula the workspace measures fidelity by (`webpkit-lossy-proptest`,
/// `xtask/metrics`). It is spelled again here only because those live in
/// `publish = false` helpers that a published `webpkit-cli` cannot depend on, and
/// the codec crate forbids the floating point this needs (bit-determinism) — so
/// the one place the CLI can host it is here. `None`, rather than a `99.0`
/// sentinel, is returned for an identical pair, so it reads as identical rather
/// than as a merely-high finite score.
///
/// Shared with `strategy`'s `-psnr` target search so both spell fidelity the same
/// way.
pub(crate) fn psnr_rgb(a: &[u8], b: &[u8]) -> Option<f64> {
    let mut se = 0.0f64;
    let mut n = 0.0f64;
    for (pa, pb) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        for c in 0..3 {
            let d = f64::from(pa[c]) - f64::from(pb[c]);
            se = d.mul_add(d, se);
            n += 1.0;
        }
    }
    if se < 1.0 {
        return None; // identical (the squared-error sum is integer-valued)
    }
    Some(10.0 * (255.0 * 255.0 / (se / n)).log10())
}

/// The largest absolute per-channel difference across every RGBA channel.
///
/// Unlike [`psnr_rgb`], this includes alpha, so a difference in transparency that
/// PSNR ignores is still surfaced.
fn max_delta(a: &[u8], b: &[u8]) -> u8 {
    a.iter()
        .zip(b)
        .map(|(x, y)| x.abs_diff(*y))
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{max_delta, psnr_rgb};

    #[test]
    fn identical_buffers_have_no_finite_psnr_and_zero_delta() {
        let a = [10, 20, 30, 255, 40, 50, 60, 255];
        assert_eq!(psnr_rgb(&a, &a), None);
        assert_eq!(max_delta(&a, &a), 0);
    }

    #[test]
    fn a_difference_lowers_psnr_and_shows_in_the_delta() {
        let a = [0, 0, 0, 255];
        let b = [10, 0, 0, 255];
        let psnr = psnr_rgb(&a, &b).expect("not identical");
        // One channel off by 10 over three RGB channels: MSE = 100/3, PSNR ~= 32.9 dB.
        assert!((32.0..34.0).contains(&psnr), "psnr was {psnr}");
        assert_eq!(max_delta(&a, &b), 10);
    }

    #[test]
    fn max_delta_sees_alpha_that_psnr_ignores() {
        let a = [0, 0, 0, 255];
        let b = [0, 0, 0, 0];
        // PSNR is over RGB only, so an alpha-only change is "identical" to it...
        assert_eq!(psnr_rgb(&a, &b), None);
        // ...but the max-delta catches it.
        assert_eq!(max_delta(&a, &b), 255);
    }
}
