//! Codec-native rate control: search encode quality to hit a size or PSNR target.
//!
//! An encoder call answers "how big is quality `q`?" but rate control asks the
//! inverse — "which quality fits this byte budget / clears this PSNR floor?" —
//! which is a *search*, not a single call. This module owns that search so it is
//! reusable from the library (the [`Encoder`](crate::Encoder) rate-control terminals)
//! rather than an ad-hoc CLI concern. The search is a deterministic integer
//! bisection over `0..=100` at a fixed effort and tuning: encoded size rises with
//! quality and reconstruction PSNR rises with quality, so each is monotone and a
//! bisection converges in a logarithmic number of probes.
//!
//! Everything is integer / fixed-point (the crate forbids floating point): a size
//! target compares byte lengths directly, and a PSNR target compares a fixed-point
//! **centidecibel** PSNR (dB × 100) computed from the integer sum of squared error
//! via [`fixed_log2`](crate::lossless::histogram::fixed_log2).

use alloc::vec::Vec;

use crate::image::{self, Image, PixelLayout};
use crate::lossless::histogram::fixed_log2;
use crate::lossy::encoder::LossyConfig;
use crate::{Error, Result};

/// `log2(10)` scaled by `2^16` (the [`fixed_log2`] fractional scale), for the
/// natural-to-decimal log change of base. `3.321928… × 65536 ≈ 217706`.
const LOG2_10_Q16: u64 = 217_706;
/// The squared peak signal (`255²`) in the PSNR numerator.
const PEAK_SQUARED: u64 = 255 * 255;
/// The sentinel centidecibel PSNR of a byte-identical reconstruction (infinite PSNR):
/// larger than any finite score, so it clears every floor.
const PSNR_INFINITE_CENTIDB: u32 = u32::MAX;

/// A byte-budget and/or PSNR-floor target for the quality search.
///
/// At least one bound must be set for the search to mean anything;
/// [`with_size`](Self::with_size) / [`with_psnr`](Self::with_psnr) accumulate the two
/// into one target so a combined request (fit *and* clear a floor) is one search, not
/// two.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct RateTarget {
    max_bytes: Option<usize>,
    min_psnr_centidb: Option<u32>,
}

impl RateTarget {
    /// A target of the largest quality whose encode fits within `max_bytes`.
    #[must_use]
    pub const fn size(max_bytes: usize) -> Self {
        Self {
            max_bytes: Some(max_bytes),
            min_psnr_centidb: None,
        }
    }

    /// A target of the smallest quality whose reconstruction PSNR meets
    /// `min_psnr_centidb` (dB × 100 — a fixed-point centidecibel floor, so a
    /// `42.5 dB` request is `4250`).
    #[must_use]
    pub const fn psnr(min_psnr_centidb: u32) -> Self {
        Self {
            max_bytes: None,
            min_psnr_centidb: Some(min_psnr_centidb),
        }
    }

    /// Add a byte budget to this target (combine with a PSNR floor).
    #[must_use]
    pub const fn with_size(mut self, max_bytes: usize) -> Self {
        self.max_bytes = Some(max_bytes);
        self
    }

    /// Add a PSNR floor (centidecibels) to this target (combine with a byte budget).
    #[must_use]
    pub const fn with_psnr(mut self, min_psnr_centidb: u32) -> Self {
        self.min_psnr_centidb = Some(min_psnr_centidb);
        self
    }

    /// Whether either bound is set (a target with neither cannot be searched).
    #[must_use]
    pub const fn is_set(self) -> bool {
        self.max_bytes.is_some() || self.min_psnr_centidb.is_some()
    }
}

/// One encode a `search` performed: the quality it tried and the resulting size.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Attempt {
    /// The lossy quality probed.
    pub quality: u8,
    /// The resulting encoded file size in bytes.
    pub bytes: usize,
}

/// The outcome of a rate-control `search`: the chosen encode, the quality that
/// produced it, whether the target was actually met, and every probe in order.
#[derive(Clone, Debug)]
pub struct RateSearch {
    bytes: Vec<u8>,
    quality: u8,
    met: bool,
    attempts: Vec<Attempt>,
}

impl RateSearch {
    /// The chosen WebP file bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Consume the result, taking ownership of the chosen WebP file bytes.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// The quality the search settled on (`0..=100`).
    #[must_use]
    pub const fn quality(&self) -> u8 {
        self.quality
    }

    /// Whether the chosen encode actually met the target (a search still returns its
    /// best effort — the closest quality — when nothing satisfies the bound).
    #[must_use]
    pub const fn met(&self) -> bool {
        self.met
    }

    /// Every quality probed, in probe order, with its resulting size — so a caller can
    /// narrate the search (`q=75 → 412 KB; q=52 → 231 KB; …`).
    #[must_use]
    pub fn attempts(&self) -> &[Attempt] {
        &self.attempts
    }
}

/// State threaded through one search: the source image, the fixed config template
/// (quality is the searched axis), and a cache so no quality is ever encoded twice.
struct Searcher<'a> {
    image: &'a Image,
    template: &'a LossyConfig,
    /// The source RGB, unpacked once and only when a PSNR bound needs it.
    source_argb: Option<Vec<u32>>,
    /// Encoded bytes per quality, so a bisection never re-encodes a probe.
    cache: Vec<Option<Vec<u8>>>,
    attempts: Vec<Attempt>,
}

impl<'a> Searcher<'a> {
    fn new(image: &'a Image, template: &'a LossyConfig) -> Self {
        Self {
            image,
            template,
            source_argb: None,
            // 0..=100 inclusive: 101 slots.
            cache: (0..=100).map(|_| None).collect(),
            attempts: Vec::new(),
        }
    }

    /// The encode at `quality`, produced once and memoized. The template's quality is
    /// overridden; effort, tuning and metadata are held constant.
    fn encode_at(&mut self, quality: u8) -> Result<&[u8]> {
        let idx = usize::from(quality);
        if self.cache[idx].is_none() {
            let config = self.template.clone().with_quality(quality);
            let bytes = crate::lossy::encode_image(self.image, &config)?;
            self.attempts.push(Attempt {
                quality,
                bytes: bytes.len(),
            });
            self.cache[idx] = Some(bytes);
        }
        Ok(self.cache[idx].as_deref().unwrap_or(&[]))
    }

    /// The encoded size at `quality` (encoding on first request).
    fn size_at(&mut self, quality: u8) -> Result<usize> {
        Ok(self.encode_at(quality)?.len())
    }

    /// The reconstruction PSNR at `quality` in centidecibels (`u32::MAX` when the
    /// re-decode is byte-identical to the source).
    fn psnr_at(&mut self, quality: u8) -> Result<u32> {
        let bytes = self.encode_at(quality)?.to_vec();
        if self.source_argb.is_none() {
            self.source_argb = Some(image::unpack_pixels(
                self.image.layout(),
                self.image.as_bytes(),
            ));
        }
        let decoded = crate::decode(&bytes)?;
        let candidate = image::unpack_pixels(PixelLayout::Rgba8, decoded.as_bytes());
        let source = self.source_argb.as_deref().unwrap_or(&candidate);
        Ok(psnr_centidb(source, &candidate))
    }
}

/// Search encode quality for `target` over `image`, holding the `template` config's
/// effort / tuning / metadata constant.
///
/// A byte budget picks the *largest* quality still within it (size rises with
/// quality); a PSNR floor picks the *smallest* quality clearing it (PSNR rises with
/// quality); a combined target takes the floor's quality but no more than the
/// budget's, so the reconstruction target wins when the two conflict. The search is a
/// deterministic integer bisection with a fixed probe order, so the same inputs
/// always choose the same quality.
///
/// # Errors
///
/// Any error the underlying [`encode_image`](crate::lossy::encode_image) or decode
/// reports, or [`Error::InvalidDimensions`] when `target` sets no bound.
pub(crate) fn search(
    image: &Image,
    template: &LossyConfig,
    target: RateTarget,
) -> Result<RateSearch> {
    if !target.is_set() {
        return Err(Error::InvalidDimensions);
    }
    let mut searcher = Searcher::new(image, template);

    // Largest quality within the byte budget (size rises with quality).
    let q_size = match target.max_bytes {
        Some(max) => Some(last_true(0, 100, |q| Ok(searcher.size_at(q)? <= max))?),
        None => None,
    };
    // Smallest quality clearing the PSNR floor (PSNR rises with quality).
    let q_psnr = match target.min_psnr_centidb {
        Some(floor) => Some(first_true(0, 100, |q| Ok(searcher.psnr_at(q)? >= floor))?),
        None => None,
    };
    // The floor wins over the budget: at least `q_psnr`, at most `q_size` when
    // compatible. `max` of the set bounds yields exactly that; default 75 when
    // (unreachably, `is_set` held) neither is present.
    let chosen = [q_size, q_psnr].into_iter().flatten().max().unwrap_or(75);

    let met = target
        .max_bytes
        .is_none_or(|max| searcher.size_at(chosen).is_ok_and(|s| s <= max))
        && target
            .min_psnr_centidb
            .is_none_or(|floor| searcher.psnr_at(chosen).is_ok_and(|p| p >= floor));

    let bytes = searcher.encode_at(chosen)?.to_vec();
    Ok(RateSearch {
        bytes,
        quality: chosen,
        met,
        attempts: searcher.attempts,
    })
}

/// The reconstruction PSNR of `candidate` against `source` (native ARGB slices) in
/// centidecibels (dB × 100), over the RGB channels only. `u32::MAX` for a
/// byte-identical pair (infinite PSNR).
///
/// `PSNR = 10·log10(255² / MSE)` where `MSE = SSE / n`; rearranged to avoid the
/// division `= 10·log10(255²·n / SSE)`, and `log10` is `log2 / log2(10)` in
/// fixed point. All integer.
fn psnr_centidb(source: &[u32], candidate: &[u32]) -> u32 {
    let mut sse = 0u64;
    let mut n = 0u64;
    for (&s, &c) in source.iter().zip(candidate) {
        for shift in [0u32, 8, 16] {
            let a = i64::from((s >> shift) & 0xff);
            let b = i64::from((c >> shift) & 0xff);
            let d = a - b;
            sse += u64::try_from(d * d).unwrap_or(0);
            n += 1;
        }
    }
    if sse == 0 {
        return PSNR_INFINITE_CENTIDB;
    }
    // log2(255²·n / SSE) = log2(255²·n) − log2(SSE), in Q16.
    let num = PEAK_SQUARED.saturating_mul(n);
    let log2_ratio_q16 = fixed_log2(num).saturating_sub(fixed_log2(sse));
    // centidB = 100 · 10 · log10(ratio) = 1000 · log2(ratio) / log2(10).
    let centidb = log2_ratio_q16.saturating_mul(1000) / LOG2_10_Q16;
    u32::try_from(centidb).unwrap_or(u32::MAX)
}

/// Largest `q` in `lo..=hi` for which `pred(q)` holds, assuming `pred` is true on an
/// initial run of low values and false thereafter (a monotone step). Returns `lo`
/// when none hold. Integer bisection with a fixed probe order.
fn last_true(lo: u8, hi: u8, mut pred: impl FnMut(u8) -> Result<bool>) -> Result<u8> {
    let mut best: Option<u8> = None;
    let (mut a, mut b) = (i16::from(lo), i16::from(hi));
    while a <= b {
        let mid = u8::try_from(i16::midpoint(a, b)).unwrap_or(lo);
        if pred(mid)? {
            best = Some(mid);
            a = i16::from(mid) + 1;
        } else {
            b = i16::from(mid) - 1;
        }
    }
    Ok(best.unwrap_or(lo))
}

/// Smallest `q` in `lo..=hi` for which `pred(q)` holds, assuming `pred` is false on an
/// initial run of low values and true thereafter. Returns `hi` when none hold.
fn first_true(lo: u8, hi: u8, mut pred: impl FnMut(u8) -> Result<bool>) -> Result<u8> {
    let mut best: Option<u8> = None;
    let (mut a, mut b) = (i16::from(lo), i16::from(hi));
    while a <= b {
        let mid = u8::try_from(i16::midpoint(a, b)).unwrap_or(lo);
        if pred(mid)? {
            best = Some(mid);
            b = i16::from(mid) - 1;
        } else {
            a = i16::from(mid) + 1;
        }
    }
    Ok(best.unwrap_or(hi))
}

#[cfg(test)]
mod tests {
    use super::{RateTarget, first_true, last_true, psnr_centidb, search};
    use crate::image::{Dimensions, PixelLayout};
    use crate::lossy::encoder::LossyConfig;

    /// A deterministic `w`×`h` RGBA noise field so the encoded size genuinely varies
    /// with quality (a solid color compresses to nothing at every quality).
    fn noise(w: u32, h: u32, seed: u64) -> Vec<u8> {
        let mut s = seed;
        let mut v = Vec::new();
        for _ in 0..w * h {
            for _ in 0..3 {
                s = s
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                v.push(u8::try_from((s >> 33) & 0xff).unwrap_or(0));
            }
            v.push(255);
        }
        v
    }

    #[test]
    fn psnr_centidb_matches_the_float_formula_and_is_infinite_when_identical() {
        // Identical → infinite.
        let a = vec![0x00FF_8040u32; 8];
        assert_eq!(psnr_centidb(&a, &a), u32::MAX);
        // One channel off by 10 over 3 RGB channels, one pixel: SSE=100, n=3.
        // PSNR = 10·log10(255²·3/100) = 10·log10(1950.75) ≈ 32.90 dB → 3290 centidB.
        let src = vec![0x00_00_00_00u32];
        let cand = vec![0x00_00_00_0Au32]; // blue channel (shift 0) off by 10
        let got = psnr_centidb(&src, &cand);
        assert!((3285..=3295).contains(&got), "psnr centidB was {got}");
    }

    #[test]
    fn last_true_and_first_true_bisect_a_monotone_step() {
        assert_eq!(last_true(0, 100, |q| Ok(q <= 50)).unwrap(), 50);
        assert_eq!(last_true(0, 100, |_| Ok(false)).unwrap(), 0);
        assert_eq!(first_true(0, 100, |q| Ok(q >= 30)).unwrap(), 30);
        assert_eq!(first_true(0, 100, |_| Ok(false)).unwrap(), 100);
    }

    #[test]
    fn size_target_meets_the_budget_and_records_attempts() {
        let dims = Dimensions::new(48, 48).unwrap();
        let pixels = noise(48, 48, 3);
        let img = crate::Image::from_parts(
            dims,
            PixelLayout::Rgba8,
            pixels,
            false,
            crate::Metadata::none(),
        );
        // A budget between the smallest and largest encode: the search must land under
        // it, and must have probed more than one quality (a real bisection).
        let full = crate::lossy::encode_image(&img, &LossyConfig::new().with_quality(100)).unwrap();
        let budget = full.len() / 2;
        let result = search(&img, &LossyConfig::new(), RateTarget::size(budget)).unwrap();
        assert!(result.met(), "the chosen quality must fit the budget");
        assert!(
            result.bytes().len() <= budget,
            "encode must be within budget"
        );
        assert!(
            result.attempts().len() > 1,
            "a bisection probes several qualities"
        );
        // Re-decodes (the chosen encode is a valid WebP).
        assert!(crate::decode(result.bytes()).is_ok());
    }

    /// A smooth `w`×`h` RGBA gradient — compresses well and reconstructs at high PSNR,
    /// so a PSNR floor is reachable within `0..=100`.
    fn gradient(width: u32, height: u32) -> Vec<u8> {
        let mut buf = Vec::new();
        for y in 0..height {
            for x in 0..width {
                let red = u8::try_from((x * 255) / width).unwrap_or(0);
                let green = u8::try_from((y * 255) / height).unwrap_or(0);
                buf.extend_from_slice(&[red, green, 128, 255]);
            }
        }
        buf
    }

    #[test]
    fn psnr_target_picks_a_quality_that_clears_the_floor() {
        let dims = Dimensions::new(48, 48).unwrap();
        let pixels = gradient(48, 48);
        let img = crate::Image::from_parts(
            dims,
            PixelLayout::Rgba8,
            pixels,
            false,
            crate::Metadata::none(),
        );
        // A 30 dB floor (3000 centidB) is reachable on a smooth gradient within 0..=100.
        let result = search(&img, &LossyConfig::new(), RateTarget::psnr(3000)).unwrap();
        assert!(result.met(), "the chosen quality must clear the PSNR floor");
        let decoded = crate::decode(result.bytes()).unwrap();
        let src = crate::image::unpack_pixels(PixelLayout::Rgba8, img.as_bytes());
        let cand = crate::image::unpack_pixels(PixelLayout::Rgba8, decoded.as_bytes());
        assert!(psnr_centidb(&src, &cand) >= 3000);
        // The smallest quality that clears the floor: a lower quality would miss it.
        assert!(result.quality() > 0, "a non-trivial quality was needed");
    }

    #[test]
    fn an_unset_target_is_rejected() {
        let dims = Dimensions::new(8, 8).unwrap();
        let img = crate::Image::from_parts(
            dims,
            PixelLayout::Rgba8,
            vec![0u8; 8 * 8 * 4],
            false,
            crate::Metadata::none(),
        );
        assert!(search(&img, &LossyConfig::new(), RateTarget::default()).is_err());
    }
}
