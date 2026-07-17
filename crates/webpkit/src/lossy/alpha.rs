//! Assembling a lossy image's `ALPH` chunk: the encode-side counterpart of the
//! umbrella crate's alpha compositor.
//!
//! A lossy `VP8 ` frame carries no alpha; a WebP image's 8-bit alpha plane rides
//! in a sibling `ALPH` chunk, whose payload is a 1-byte header plus the plane
//! stored either raw (`method = 0`) or as a lossless VP8L stream (`method = 1`),
//! each optionally spatially pre-filtered (none / horizontal / vertical /
//! gradient). The **stored** plane is always coded losslessly, so the search's sole
//! objective is the smallest valid payload; [`compress_alpha`] trials the filter ×
//! method combinations the [`AlphaTuning`] selects and keeps the smallest,
//! deterministically.
//!
//! Optional lossiness comes only from an up-front level-quantization pre-pass
//! ([`quantize_levels`], libwebp `QuantizeLevels`): when `alpha_q < 100` the plane is
//! mapped onto fewer distinct alpha levels before the lossless store, trading soft-
//! transparency fidelity for size. `alpha_q = 100` (the default) is the identity, so
//! the stored bytes are byte-for-byte the prior always-lossless output.
//!
//! The filter kernels and the 1-byte header live in the core shell (bitstream-
//! agnostic); the VP8L compression is delegated to the `lossless` codec. This module is only
//! the orchestration seam that ties them together — the layering the umbrella
//! crate mirrors on decode (core-shell un-filter + `crate::lossless::decode_alpha`).

use crate::alpha::{self, AlphaCompression, AlphaFilter};
use crate::lossy::tuning::{AlphaFilterMode, AlphaMethod};

use crate::lossy::prelude::*;

/// The four spatial filters, tried in this fixed order so ties break
/// deterministically toward the earliest (and, within a filter, toward the
/// lossless method — see [`compress_alpha`]).
const FILTERS: [AlphaFilter; 4] = [
    AlphaFilter::None,
    AlphaFilter::Horizontal,
    AlphaFilter::Vertical,
    AlphaFilter::Gradient,
];

/// The 256 possible 8-bit alpha values (the level-quantization histogram width).
const NUM_SYMBOLS: usize = 256;
/// Convergence steps of the level-quantization refinement (libwebp `MAX_ITER`).
const MAX_ITER: usize = 6;
/// Fractional bits of the fixed-point centroid representation (a `Q16` value holds an
/// alpha level in `0..=255` with 16 fractional bits — libwebp uses `double` here).
const FP_SHIFT: u32 = 16;
/// The rounding bias added before a `>> FP_SHIFT` descale (round to nearest).
const FP_HALF: i64 = 1 << (FP_SHIFT - 1);

/// The lossy-alpha knobs the still and animation encoders project a [`LossyTuning`]
/// onto: the level-quantization quality plus the stored-plane search bounds.
#[derive(Clone, Copy)]
pub(crate) struct AlphaTuning {
    /// Alpha quality `0..=100`; `100` is the lossless identity pre-pass.
    pub(crate) quality: u8,
    /// Whether the lossless VP8L candidate stays in the search, or the plane is raw.
    pub(crate) method: AlphaMethod,
    /// How many spatial predictors the search trials.
    pub(crate) filter: AlphaFilterMode,
}

/// Compress an 8-bit alpha `plane` (`width * height` bytes, row-major) into the
/// smallest valid `ALPH` chunk payload the [`AlphaTuning`] admits: its 1-byte header
/// followed by the stored plane.
///
/// The plane is first passed through the [`quantize_levels`] pre-pass (identity when
/// `tuning.quality == 100`). Then, for each spatial filter the search admits, the
/// (quantized) plane is forward-filtered once and the candidate encodings are formed
/// — lossless VP8L (`method = 1`, only when [`AlphaMethod::Compressed`]) and raw
/// (`method = 0`) — and the globally smallest payload is returned. The scan order
/// (filter `None → H → V → Gradient`, lossless before raw within each filter) with a
/// strict "smaller wins" rule makes the choice deterministic: on a tie the earlier
/// candidate is kept.
#[must_use]
pub(crate) fn compress_alpha(
    plane: &[u8],
    width: u32,
    height: u32,
    tuning: AlphaTuning,
) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let mut plane = plane.to_vec();
    quantize_levels(&mut plane, levels_for_quality(tuning.quality));
    let mut best: Option<Vec<u8>> = None;
    for filter in filter_candidates(tuning.filter, &plane, w, h) {
        let filtered = alpha::filter_plane(filter, &plane, w, h);
        if matches!(tuning.method, AlphaMethod::Compressed) {
            // method 1: the filtered plane as a headerless (green-lane) VP8L stream.
            let lossless = crate::lossless::encode_alpha(&filtered, width, height);
            consider(
                &mut best,
                assemble(AlphaCompression::Lossless, filter, &lossless),
            );
        }
        // method 0: the filtered plane stored raw.
        consider(
            &mut best,
            assemble(AlphaCompression::None, filter, &filtered),
        );
    }
    // width*height >= 1 guarantees at least one candidate (`None`/raw always fits).
    best.unwrap_or_else(|| assemble(AlphaCompression::None, AlphaFilter::None, &plane))
}

/// The spatial filters [`compress_alpha`] trials for `mode`: every filter for
/// [`AlphaFilterMode::Best`], a single cheap-estimated one for
/// [`AlphaFilterMode::Fast`], and only [`AlphaFilter::None`] for
/// [`AlphaFilterMode::None`].
fn filter_candidates(mode: AlphaFilterMode, plane: &[u8], w: usize, h: usize) -> Vec<AlphaFilter> {
    match mode {
        AlphaFilterMode::None => vec![AlphaFilter::None],
        AlphaFilterMode::Fast => vec![estimate_filter(plane, w, h)],
        AlphaFilterMode::Best => FILTERS.to_vec(),
    }
}

/// Pick one spatial predictor by a cheap residual estimate: the filter whose
/// forward-filtered plane has the smallest total absolute delta (a proxy for its
/// coded size). Ties break toward the earliest filter in [`FILTERS`] order, so the
/// choice is deterministic.
fn estimate_filter(plane: &[u8], w: usize, h: usize) -> AlphaFilter {
    let mut best = AlphaFilter::None;
    let mut best_cost = u64::MAX;
    for &filter in &FILTERS {
        let filtered = alpha::filter_plane(filter, plane, w, h);
        let cost: u64 = filtered.iter().map(|&d| u64::from(d.min(255 - d))).sum();
        if cost < best_cost {
            best_cost = cost;
            best = filter;
        }
    }
    best
}

/// Map an alpha quality `alpha_q` (`0..=100`) onto a target distinct-level count in
/// `2..=256`. `100` yields `256` — the identity, since a plane can hold at most 256
/// distinct 8-bit levels — so the default pre-pass never changes a byte.
fn levels_for_quality(alpha_q: u8) -> u16 {
    let q = u16::from(alpha_q.min(100));
    2 + (q * 254 + 50) / 100
}

/// Reduce `plane` to at most `num_levels` distinct alpha levels in place, a fixed-
/// point port of libwebp's `QuantizeLevels` (`utils/quant_levels_utils.c`).
///
/// A histogram-seeded Lloyd–Max refinement: centroids start uniformly spread across
/// the plane's `[min, max]` range, then boundaries and centroids are alternately
/// refined for [`MAX_ITER`] passes before each pixel is remapped to its cluster's
/// rounded centroid. libwebp's `double` centroids are reformulated as `Q16`
/// fixed-point integers so the result is bit-deterministic across platforms.
///
/// Leaves the plane untouched when it already holds no more than `num_levels`
/// distinct values (so `num_levels == 256`, hence `alpha_q == 100`, is always the
/// identity).
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    reason = "centroids are Q16 values bounded to 0..=255<<16 and slot indices to \
              0..=255; every cast is of a value proven within the destination range, \
              mirroring the reference's int/uint8_t narrowing"
)]
fn quantize_levels(plane: &mut [u8], num_levels: u16) {
    let levels = usize::from(num_levels);
    if plane.is_empty() || levels < 2 {
        return;
    }
    let mut freq = [0u32; NUM_SYMBOLS];
    let mut min_s = u8::MAX;
    let mut max_s = 0u8;
    let mut distinct = 0u32;
    for &v in plane.iter() {
        if freq[usize::from(v)] == 0 {
            distinct += 1;
        }
        freq[usize::from(v)] += 1;
        min_s = min_s.min(v);
        max_s = max_s.max(v);
    }
    if distinct <= u32::from(num_levels) {
        return; // already within the target level count — nothing to do (identity).
    }

    // Uniformly spread the initial centroids across [min_s, max_s], as Q16 values.
    let span = i64::from(max_s) - i64::from(min_s);
    let denom = (levels as i64) - 1;
    let mut inv_q = [0i64; NUM_SYMBOLS];
    for (i, centroid) in inv_q.iter_mut().take(levels).enumerate() {
        let numer = (span * (i as i64)) << FP_SHIFT;
        *centroid = (i64::from(min_s) << FP_SHIFT) + div_round(numer, denom);
    }

    // Alternately assign each level to its nearest centroid and recentre the clusters.
    let mut q_level = [0u8; NUM_SYMBOLS];
    for _ in 0..MAX_ITER {
        let mut q_sum = [0i64; NUM_SYMBOLS];
        let mut q_count = [0i64; NUM_SYMBOLS];
        let mut slot = 0usize;
        for s in i64::from(min_s)..=i64::from(max_s) {
            let freq_s = freq[s as usize];
            if freq_s == 0 {
                continue;
            }
            // Advance to the cluster whose lower boundary this level has crossed; the
            // boundary is the centroid midpoint, tested in Q16 (`2*s` becomes `s<<17`).
            while slot + 1 < levels && (s << (FP_SHIFT + 1)) > inv_q[slot] + inv_q[slot + 1] {
                slot += 1;
            }
            q_level[s as usize] = slot as u8;
            q_sum[slot] += s * i64::from(freq_s);
            q_count[slot] += i64::from(freq_s);
        }
        for (slot, centroid) in inv_q.iter_mut().take(levels).enumerate() {
            if q_count[slot] > 0 {
                *centroid = div_round(q_sum[slot] << FP_SHIFT, q_count[slot]);
            }
        }
    }

    // Remap every pixel to its cluster's rounded centroid.
    for v in plane.iter_mut() {
        let slot = usize::from(q_level[usize::from(*v)]);
        *v = ((inv_q[slot] + FP_HALF) >> FP_SHIFT).clamp(0, 255) as u8;
    }
}

/// Round-to-nearest integer division of a non-negative `n` by a positive `d`.
const fn div_round(n: i64, d: i64) -> i64 {
    (n + d / 2) / d
}

/// Materialize a frame's `ALPH` chunk payload (1-byte header + stored plane) from
/// native-ARGB `argb` (`dims.pixel_count()` pixels), or `None` when every pixel is
/// opaque (top byte `0xff`) — in which case no `ALPH` chunk is written.
///
/// The alpha lane is the top byte of each pixel; `>> 24` narrows a `u32` to its
/// high 8 bits, so `as u8` cannot truncate.
#[must_use]
pub(crate) fn alph_chunk(
    argb: &[u32],
    dims: crate::Dimensions,
    tuning: AlphaTuning,
) -> Option<Vec<u8>> {
    crate::image::argb_has_alpha(argb).then(|| {
        let alpha: Vec<u8> = argb.iter().map(|&p| (p >> 24) as u8).collect();
        compress_alpha(&alpha, dims.width(), dims.height(), tuning)
    })
}

/// Build one `ALPH` payload: the 1-byte header (`compression`, `filter`,
/// pre-processing = 0) followed by the stored `data`.
fn assemble(compression: AlphaCompression, filter: AlphaFilter, data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + data.len());
    out.push(alpha::build_header(compression, filter, 0));
    out.extend_from_slice(data);
    out
}

/// Replace `best` with `candidate` when it is strictly smaller (ties keep the
/// incumbent, so the earlier scan position wins).
fn consider(best: &mut Option<Vec<u8>>, candidate: Vec<u8>) {
    if best.as_ref().is_none_or(|b| candidate.len() < b.len()) {
        *best = Some(candidate);
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use crate::alpha::{AlphaCompression, AlphaFilter, build_header, parse_header, unfilter};
    use crate::lossy::tuning::{AlphaFilterMode, AlphaMethod};

    use super::{
        AlphaTuning, compress_alpha, filter_candidates, levels_for_quality, quantize_levels,
    };

    /// The always-lossless, exhaustive-search default — the tuning under which the
    /// output must stay byte-identical to the pre-P5 encoder.
    const LOSSLESS: AlphaTuning = AlphaTuning {
        quality: 100,
        method: AlphaMethod::Compressed,
        filter: AlphaFilterMode::Best,
    };

    /// A pattern value narrowed into a byte (no lossy cast).
    fn byte(v: u32) -> u8 {
        u8::try_from(v & 0xff).unwrap_or(0)
    }

    /// Decompress an `ALPH` payload back to a `width * height` alpha plane, the
    /// exact inverse the umbrella crate performs on decode.
    fn decompress(alph: &[u8], width: u32, height: u32) -> Vec<u8> {
        let (w, h) = (width as usize, height as usize);
        let (header, data) = parse_header(alph).unwrap();
        let mut plane = match header.compression {
            AlphaCompression::None => data[..w * h].to_vec(),
            AlphaCompression::Lossless => {
                crate::lossless::decode_alpha(data, width, height).unwrap()
            },
        };
        unfilter(header.filter, &mut plane, w, h);
        plane
    }

    /// The pre-P5 reference: the exhaustive filter × {lossless, raw} search with no
    /// level-quantization pre-pass. The default `compress_alpha` must reproduce this
    /// byte-for-byte (the identity guarantee).
    fn exhaustive_reference(plane: &[u8], width: u32, height: u32) -> Vec<u8> {
        let (w, h) = (width as usize, height as usize);
        let mut best: Option<Vec<u8>> = None;
        for &filter in &super::FILTERS {
            let filtered = super::alpha::filter_plane(filter, plane, w, h);
            let lossless = crate::lossless::encode_alpha(&filtered, width, height);
            super::consider(
                &mut best,
                super::assemble(AlphaCompression::Lossless, filter, &lossless),
            );
            super::consider(
                &mut best,
                super::assemble(AlphaCompression::None, filter, &filtered),
            );
        }
        best.unwrap()
    }

    #[test]
    fn compress_alpha_round_trips_byte_exact() {
        // A flat run, a smooth ramp, a scattered pattern, and a two-region field:
        // every one must decompress back to the source plane byte-for-byte.
        let cases: [(u32, u32, Vec<u8>); 4] = [
            (4, 4, vec![0xC0u8; 16]),
            (8, 3, (0..24u32).map(|v| byte(v * 10)).collect()),
            (
                5,
                5,
                (0..25u32)
                    .map(|v| byte(v.wrapping_mul(53) ^ 0x1f))
                    .collect(),
            ),
            (
                6,
                4,
                (0..24u32)
                    .map(|i| if i % 6 < 3 { 0 } else { 255 })
                    .collect(),
            ),
        ];
        for (w, h, plane) in cases {
            let alph = compress_alpha(&plane, w, h, LOSSLESS);
            assert_eq!(decompress(&alph, w, h), plane, "{w}x{h} alpha round-trip");
        }
    }

    #[test]
    fn compress_alpha_keeps_the_smallest_candidate() {
        // A flat plane: every filter's deltas are trivially compressible, but the
        // chosen payload must be no larger than any individual candidate. Compare
        // against the raw method-0 none-filter baseline (1 header byte + plane).
        let plane = vec![0x42u8; 32];
        let chosen = compress_alpha(&plane, 8, 4, LOSSLESS);
        let raw_baseline = 1 + plane.len();
        assert!(
            chosen.len() <= raw_baseline,
            "chosen {} must not exceed raw baseline {raw_baseline}",
            chosen.len()
        );
    }

    #[test]
    fn compress_alpha_incompressible_plane_keeps_the_first_smallest_raw() {
        // A high-entropy plane VP8L cannot shrink below its raw size, so every
        // lossless candidate is strictly larger than the raw ones; the four raw
        // candidates (one per filter) then tie at the global minimum length
        // (1 header byte + `width*height`). The strict "smaller wins" rule must
        // keep the FIRST such candidate in scan order — filter None, method 0
        // (raw) — which for the identity None filter is exactly the header byte
        // followed by the untouched plane. A `<`->`<=` flip picks the LAST tying
        // raw (filter Gradient); `<`->`==`/`>` picks a larger lossless candidate;
        // each changes these exact bytes.
        let plane: Vec<u8> = (0..64u32)
            .map(|i| {
                let mut z = i.wrapping_add(1).wrapping_mul(0x9E37_79B1);
                z ^= z >> 15;
                z = z.wrapping_mul(0x85EB_CA77);
                z ^= z >> 13;
                z = z.wrapping_mul(0xC2B2_AE3D);
                z ^= z >> 16;
                byte(z)
            })
            .collect();
        let mut expected = vec![build_header(AlphaCompression::None, AlphaFilter::None, 0)];
        expected.extend_from_slice(&plane);
        assert_eq!(compress_alpha(&plane, 8, 8, LOSSLESS), expected);
    }

    #[test]
    fn compress_alpha_flat_plane_chooses_lossless() {
        // A flat plane collapses to a handful of VP8L bytes, far under the raw
        // header+plane size, so the smallest candidate MUST be a lossless one.
        // A `consider` turned into a no-op — or a `>` flip that keeps the largest
        // candidate — instead returns the raw None-filter fallback (method 0),
        // which this pins against.
        let plane = vec![0xA7u8; 32];
        let chosen = compress_alpha(&plane, 8, 4, LOSSLESS);
        let (header, _) = parse_header(&chosen).unwrap();
        assert_eq!(header.compression, AlphaCompression::Lossless);
        assert!(
            chosen.len() < 1 + plane.len(),
            "lossless flat-plane payload {} must beat the {}-byte raw baseline",
            chosen.len(),
            1 + plane.len()
        );
    }

    #[test]
    fn compress_alpha_is_deterministic() {
        let plane: Vec<u8> = (0..48u32).map(|v| byte(v.wrapping_mul(29))).collect();
        assert_eq!(
            compress_alpha(&plane, 8, 6, LOSSLESS),
            compress_alpha(&plane, 8, 6, LOSSLESS)
        );
    }

    #[test]
    fn default_tuning_is_byte_identical_to_the_prepass_free_exhaustive_search() {
        // The identity guarantee: with the default (lossless, exhaustive) tuning —
        // AND with an explicit alpha_q of 100 — `compress_alpha` must match, byte for
        // byte, the pre-P5 filter × {lossless, raw} search that ran no pre-pass. Any
        // non-identity pre-pass or altered search order changes these bytes.
        let explicit_q100 = AlphaTuning {
            quality: 100,
            method: AlphaMethod::Compressed,
            filter: AlphaFilterMode::Best,
        };
        let planes: [(u32, u32, Vec<u8>); 3] = [
            (8, 6, (0..48u32).map(|v| byte(v.wrapping_mul(29))).collect()),
            (7, 5, (0..35u32).map(|v| byte((v * 37) ^ 0x5a)).collect()),
            (4, 4, vec![0xC0u8; 16]),
        ];
        for (w, h, plane) in planes {
            let reference = exhaustive_reference(&plane, w, h);
            assert_eq!(
                compress_alpha(&plane, w, h, LOSSLESS),
                reference,
                "{w}x{h}: default tuning must equal the pre-pass-free reference"
            );
            assert_eq!(
                compress_alpha(&plane, w, h, explicit_q100),
                reference,
                "{w}x{h}: alpha_q=100 must equal the pre-pass-free reference"
            );
        }
    }

    #[test]
    fn quantize_levels_is_identity_at_full_quality() {
        // alpha_q = 100 maps to 256 target levels; a plane holds at most 256 distinct
        // 8-bit values, so the pre-pass must leave every byte untouched.
        assert_eq!(levels_for_quality(100), 256);
        let plane: Vec<u8> = (0..=255u8).chain(0..=255u8).collect();
        let mut quantized = plane.clone();
        quantize_levels(&mut quantized, levels_for_quality(100));
        assert_eq!(quantized, plane, "q=100 pre-pass must be the identity");
    }

    #[test]
    fn quantize_levels_reduces_distinct_levels_and_is_bounded() {
        // A full 0..=255 ramp reduced to a small level budget must collapse to no
        // more than that many distinct values, stay within the source range, and be
        // deterministic run to run.
        let source: Vec<u8> = (0..256u32).map(byte).collect();
        for &q in &[0u8, 20, 50, 80] {
            let target = usize::from(levels_for_quality(q));
            let mut a = source.clone();
            let mut b = source.clone();
            quantize_levels(&mut a, levels_for_quality(q));
            quantize_levels(&mut b, levels_for_quality(q));
            assert_eq!(a, b, "q={q}: quantization must be deterministic");
            let mut seen = a.clone();
            seen.sort_unstable();
            seen.dedup();
            assert!(
                seen.len() <= target,
                "q={q}: {} distinct levels exceeds the {target} budget",
                seen.len()
            );
        }
    }

    #[test]
    fn levels_for_quality_spans_the_full_range_monotonically() {
        assert_eq!(levels_for_quality(0), 2, "q=0 keeps the two extremes");
        assert_eq!(levels_for_quality(100), 256, "q=100 keeps every level");
        let mut prev = 0u16;
        for q in 0..=100u8 {
            let n = levels_for_quality(q);
            assert!(n >= prev, "levels must not decrease as quality rises");
            assert!((2..=256).contains(&n));
            prev = n;
        }
    }

    #[test]
    fn filter_candidates_bounds_the_search() {
        let plane: Vec<u8> = (0..16u32).map(|v| byte(v * 17)).collect();
        assert_eq!(
            filter_candidates(AlphaFilterMode::None, &plane, 4, 4),
            vec![AlphaFilter::None]
        );
        assert_eq!(
            filter_candidates(AlphaFilterMode::Fast, &plane, 4, 4).len(),
            1,
            "fast trials a single estimated filter"
        );
        assert_eq!(
            filter_candidates(AlphaFilterMode::Best, &plane, 4, 4).len(),
            4,
            "best trials every filter"
        );
    }

    #[test]
    fn alpha_method_none_stores_the_plane_raw() {
        // A flat plane the lossless coder would shrink: forcing method None must keep
        // a raw (method-0) payload regardless.
        let plane = vec![0xA7u8; 32];
        let tuning = AlphaTuning {
            quality: 100,
            method: AlphaMethod::None,
            filter: AlphaFilterMode::Best,
        };
        let chosen = compress_alpha(&plane, 8, 4, tuning);
        let (header, _) = parse_header(&chosen).unwrap();
        assert_eq!(header.compression, AlphaCompression::None);
        assert_eq!(decompress(&chosen, 8, 4), plane, "raw store round-trips");
    }

    #[test]
    fn alph_chunk_none_for_opaque() {
        use super::alph_chunk;
        use crate::Dimensions;
        // Every pixel's top byte is 0xff -> no ALPH chunk.
        let argb: Vec<u32> = vec![0xff10_2030; 12];
        assert!(alph_chunk(&argb, Dimensions::new(4, 3).unwrap(), LOSSLESS).is_none());
    }

    #[test]
    fn alph_chunk_some_for_translucent_round_trips() {
        use super::alph_chunk;
        use crate::Dimensions;
        // A per-pixel alpha ramp in the top byte; the recovered plane must be
        // byte-exact (alpha is lossless at q=100).
        let argb: Vec<u32> = (0..16u32).map(|i| (i * 15) << 24 | 0x0011_2233).collect();
        let dims = Dimensions::new(4, 4).unwrap();
        let alph = alph_chunk(&argb, dims, LOSSLESS).expect("translucent -> Some");
        let expected: Vec<u8> = argb.iter().map(|&p| (p >> 24) as u8).collect();
        assert_eq!(decompress(&alph, 4, 4), expected);
    }

    proptest! {
        /// Decode safety over the whole alpha knob space: for any small plane and any
        /// (alpha_q, method, filter) combination, `compress_alpha` must produce a
        /// payload that decodes (never panicking) back to *exactly* the quantized
        /// plane the encoder stored — the ALPH path is byte-exact for its own input.
        #[test]
        fn compress_alpha_decodes_to_the_quantized_plane(
            (width, height, plane) in (1usize..=12, 1usize..=12).prop_flat_map(|(w, h)| {
                prop::collection::vec(any::<u8>(), w * h).prop_map(move |p| (w, h, p))
            }),
            quality in 0u8..=100,
            method in prop_oneof![Just(AlphaMethod::None), Just(AlphaMethod::Compressed)],
            filter in prop_oneof![
                Just(AlphaFilterMode::None),
                Just(AlphaFilterMode::Fast),
                Just(AlphaFilterMode::Best),
            ],
        ) {
            let (w, h) = (u32::try_from(width).unwrap(), u32::try_from(height).unwrap());
            let tuning = AlphaTuning { quality, method, filter };
            // The plane the encoder actually stores: the quantized pre-pass output.
            let mut stored = plane.clone();
            quantize_levels(&mut stored, levels_for_quality(quality));
            let alph = compress_alpha(&plane, w, h, tuning);
            prop_assert_eq!(decompress(&alph, w, h), stored);
        }
    }
}
