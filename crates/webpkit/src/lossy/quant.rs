//! Forward quantization and the quality → quantizer-index mapping.
//!
//! The inverse of the dequantization the decoder applies in [`crate::lossy::token`]:
//! given the same per-plane dequant steps `Q` (derived here from the base
//! quantizer index exactly as [`crate::lossy::header`]'s `parse_quant` derives them),
//! [`quantize_block`] divides each transform coefficient by `Q` (round to
//! nearest) to a signed *level*, and reconstructs `level * Q` — which is exactly
//! the value the decoder recovers (`token.rs`: `out[ZIGZAG[n]] = apply_sign(v) *
//! dq`). That identity is what makes the emitted bitstream decode back to the
//! encoder's own reconstruction.
//!
//! Bit-deterministic integer math only: a per-step reciprocal `iq` replaces
//! per-coefficient division, and the level is `(|coeff| * iq + bias) >> QFIX`.
#![expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "quantized levels and reconstructed coefficients are stored into i16 \
              with the C int16_t wrapping semantics of the reference encoder, and \
              the small 0..=16 zig-zag indices cast to i32; every value is bounded \
              well within range and the casts reproduce the reference exactly"
)]

use crate::lossy::constants::{
    AC_TABLE, DC_TABLE, FREQ_SHARPEN, QUALITY_TO_BASE_Q, SHARPEN_BITS, ZIGZAG,
};

/// Fixed-point precision of the reciprocal quantizer multiplier.
const QFIX: u32 = 17;
/// Rounding bias added before the `>> QFIX` descale (round to nearest).
const QBIAS: i32 = 1 << (QFIX - 1);
/// The largest coefficient magnitude VP8's token tree can code.
const MAX_LEVEL: i32 = 2047;

/// One plane class's dequant step `q` and its precomputed `Q(QFIX)` reciprocal.
#[derive(Clone, Copy)]
pub(crate) struct QFactor {
    /// The dequantization step: a level `l` reconstructs to `l * q`.
    pub(crate) q: i32,
    /// `((1 << QFIX) + q/2) / q`: multiply-and-shift stand-in for `/ q`.
    iq: i32,
}

impl QFactor {
    /// Build a factor for dequant step `q` (`q >= 1`, always true for VP8 tables).
    const fn new(q: i32) -> Self {
        Self {
            q,
            iq: ((1 << QFIX) + q / 2) / q,
        }
    }

    /// Quantize a signed coefficient to a level (round to nearest, clamped to the codable
    /// range), with an additive high-frequency `sharpen` bias on its magnitude (libwebp
    /// `kFreqSharpening`): `level = ((|coeff| + sharpen) * iq + QBIAS) >> QFIX`, clamped,
    /// with the sign of the original coefficient. `sharpen == 0` is the plain
    /// round-to-nearest quantizer, so the default path is unchanged.
    pub(crate) fn quantize_sharpened(self, coeff: i32, sharpen: i32) -> i32 {
        let mag = coeff.abs() + sharpen.max(0);
        let level = ((mag * self.iq + QBIAS) >> QFIX).min(MAX_LEVEL);
        if coeff < 0 { -level } else { level }
    }
}

/// A `[DC, AC]` factor pair for one plane class.
#[derive(Clone, Copy)]
pub(crate) struct QPair {
    /// The DC (position 0) factor.
    pub(crate) dc: QFactor,
    /// The AC (positions 1..16) factor.
    pub(crate) ac: QFactor,
    /// Per-coefficient additive sharpening bias in **natural** order (libwebp
    /// `kFreqSharpening`), all `0` unless the `freq_sharpen` knob is on (luma plane only).
    /// Index `0` (DC) is always `0`.
    pub(crate) sharpen: [i32; 16],
}

/// One segment's forward quantizers: luma AC/DC, second-order (Y2) and chroma —
/// the forward mirror of [`crate::lossy::decode::QuantMatrix`]. Derived from the base
/// quantizer index with the same table lookups and clamps the decoder uses, so
/// the reconstructed coefficients match bit-for-bit.
#[derive(Clone, Copy)]
pub(crate) struct Quantizer {
    /// Luma AC/DC factors.
    pub(crate) y1: QPair,
    /// Second-order (Y2 / WHT) factors.
    pub(crate) y2: QPair,
    /// Chroma factors.
    pub(crate) uv: QPair,
}

impl Quantizer {
    /// Build the per-plane quantizers for base index `base_q` (`0..=127`), with
    /// every DC/AC delta zero (the MVP single-segment case). Mirrors
    /// `header.rs::parse_quant`: `y1 = [DC[q], AC[q]]`, `y2 = [DC[q]*2,
    /// max((AC[q]*101581)>>16, 8)]`, `uv = [DC[clip_uv(q)], AC[q]]`.
    #[must_use]
    pub(crate) fn new(base_q: i32) -> Self {
        Self::build(base_q, false)
    }

    /// Like [`new`](Self::new), but with the luma per-frequency sharpening bias
    /// (`kFreqSharpening`) baked into `y1.sharpen`. `y2`/`uv` are never sharpened.
    #[must_use]
    pub(crate) fn freq_sharpened(base_q: i32) -> Self {
        Self::build(base_q, true)
    }

    fn build(base_q: i32, freq_sharpen: bool) -> Self {
        let q = clip_q(base_q);
        let dc = i32::from(DC_TABLE[q as usize]);
        let ac = i32::from(AC_TABLE[q as usize]);
        let uv_dc = i32::from(DC_TABLE[clip_uv(base_q) as usize]);
        // Per-frequency luma AC sharpening: `(kFreqSharpening[j] * ac_step) >> SHARPEN_BITS`
        // in natural order. `kFreqSharpening[0] == 0`, so DC is never biased. Zero when off,
        // which makes the whole quantizer byte-identical to the pre-`freq_sharpen` path.
        let mut sharpen = [0i32; 16];
        if freq_sharpen {
            for (s, &w) in sharpen.iter_mut().zip(&FREQ_SHARPEN) {
                *s = (w * ac) >> SHARPEN_BITS;
            }
        }
        Self {
            y1: QPair {
                dc: QFactor::new(dc),
                ac: QFactor::new(ac),
                sharpen,
            },
            y2: QPair {
                dc: QFactor::new(dc * 2),
                ac: QFactor::new(((ac * 101_581) >> 16).max(8)),
                sharpen: [0; 16],
            },
            uv: QPair {
                dc: QFactor::new(uv_dc),
                ac: QFactor::new(ac),
                sharpen: [0; 16],
            },
        }
    }
}

/// The result of quantizing one 4×4 coefficient block.
pub(crate) struct Quantized {
    /// Signed levels in **zig-zag** order (the token-emission order); positions
    /// below `first` are zero.
    pub(crate) levels: [i16; 16],
    /// Reconstructed (dequantized) coefficients in **natural** order — exactly
    /// what the decoder recovers. Positions below `first`'s natural index stay 0.
    pub(crate) recon: [i16; 16],
    /// Index of the last non-zero level in zig-zag order, or `first - 1` if the
    /// block is empty. `last + 1` equals the decoder's `GetCoeffs` return value,
    /// so it drives both token emission (EOB when `n > last`) and the non-zero
    /// context (`nz > first`).
    pub(crate) last: i32,
}

/// Quantize a 4×4 coefficient block `coeffs` (natural order) starting at zig-zag
/// position `first` (0 for a full block, 1 for a 16×16-luma AC block whose DC is
/// carried by the Y2 block). `pair` carries the plane's DC/AC factors and its
/// per-frequency sharpening bias (all `0` unless `freq_sharpen` is on), so a default
/// (unsharpened) block is byte-identical to the round-to-nearest quantizer.
#[must_use]
pub(crate) fn quantize_block(coeffs: [i16; 16], pair: QPair, first: usize) -> Quantized {
    let mut levels = [0i16; 16];
    let mut recon = [0i16; 16];
    let mut last = first as i32 - 1;
    for n in first..16 {
        let j = ZIGZAG[n];
        let factor = if j == 0 { pair.dc } else { pair.ac };
        let level = factor.quantize_sharpened(i32::from(coeffs[j]), pair.sharpen[j]);
        if level != 0 {
            last = n as i32;
        }
        levels[n] = level as i16;
        recon[j] = (level * factor.q) as i16;
    }
    Quantized {
        levels,
        recon,
        last,
    }
}

/// Map a 0..=100 quality to a base quantizer index 0..=127 (higher quality → a
/// smaller, finer index) via the committed non-linear [`QUALITY_TO_BASE_Q`]
/// curve, which gives finer control at high quality than a linear map.
#[must_use]
pub(crate) fn quality_to_base_q(quality: u8) -> i32 {
    i32::from(QUALITY_TO_BASE_Q[usize::from(quality.min(100))])
}

/// The activity value that produces a zero SNS quant delta — the midpoint of the
/// `0..=255` activity range, so a mid-activity block is neither coarsened nor refined.
const SNS_NEUTRAL_ACTIVITY: i32 = 128;
/// Divisor mapping the `activity × strength` product to quantizer-index units. Sized so
/// the extreme (`|activity - 128| = 128`, `strength = 100`) delta stays a handful of
/// index steps rather than swinging across the whole table.
const SNS_QUANT_SCALE: i32 = 512;

/// The zero-centered SNS quantizer-index delta for a macroblock of texture `activity`
/// (`0..=255`, mid `128`) at spatial-noise-shaping `strength` (`0..=100`). A flat
/// (low-activity) block is coded finer (negative delta, more bits where the eye sees
/// error); a busy block is coarsened (positive delta, texture masks the loss). Anchored
/// on the base quantizer curve rather than libwebp's `pow` law — no float, no LUT.
///
/// A mid activity or a zero strength returns exactly `0`, so a single-segment frame (and
/// any frame encoded at strength `0`) is byte-identical to the pre-SNS quantizer.
pub(crate) fn sns_quant_delta(activity: u8, strength: u8) -> i32 {
    let centered = i32::from(activity) - SNS_NEUTRAL_ACTIVITY;
    let strength = i32::from(strength.min(100));
    centered * strength / SNS_QUANT_SCALE
}

/// Clamp a quantizer index into the luma/AC table range `0..=127` (mirrors
/// `header.rs::clip_q`).
fn clip_q(v: i32) -> i32 {
    v.clamp(0, 127)
}

/// Clamp a quantizer index into the chroma-DC table range `0..=117` (mirrors
/// `header.rs::clip_uv`).
fn clip_uv(v: i32) -> i32 {
    v.clamp(0, 117)
}

#[cfg(test)]
mod tests {
    use super::{QFactor, QPair, Quantizer, quality_to_base_q, quantize_block};
    use crate::lossy::constants::ZIGZAG;
    use crate::lossy::fdct::fdct4x4;
    use crate::lossy::idct::transform_one;

    /// A `QPair` with no sharpening — the round-to-nearest quantizer the tests pin.
    fn pair(dc: QFactor, ac: QFactor) -> QPair {
        QPair {
            dc,
            ac,
            sharpen: [0; 16],
        }
    }

    #[test]
    fn dequant_factors_match_the_decoder_derivation() {
        // base_q = 40, all deltas zero. Derived independently from kDcTable /
        // kAcTable (the same tables header.rs::parse_quant reads):
        //   DC[40] = 37, AC[40] = 44
        //   y1 = [37, 44]
        //   y2 = [37*2, max((44*101581)>>16, 8)] = [74, max(68, 8)] = [74, 68]
        //   uv = [DC[clip_uv(40)=40], AC[40]] = [37, 44]
        let q = Quantizer::new(40);
        assert_eq!((q.y1.dc.q, q.y1.ac.q), (37, 44));
        assert_eq!((q.y2.dc.q, q.y2.ac.q), (74, 68));
        assert_eq!((q.uv.dc.q, q.uv.ac.q), (37, 44));
    }

    #[test]
    fn quantize_rounds_to_nearest_and_reconstructs_level_times_q() {
        // A hand-checked block against dc step 10, ac step 20 (first = 0):
        //   natural coeff[0] = 24 -> level round(24/10) = 2, recon 2*10 = 20
        //   natural coeff[1] = -35 (ZIGZAG position 1) -> round(-35/20) = -2,
        //     recon -40
        //   natural coeff[4] = 9 (ZIGZAG position 2) -> round(9/20) = 0, recon 0
        //   natural coeff[8] = 30 (ZIGZAG position 3) -> round(30/20) = 2, recon 40
        let dc = super::QFactor::new(10);
        let ac = super::QFactor::new(20);
        let mut coeffs = [0i16; 16];
        coeffs[0] = 24;
        coeffs[1] = -35;
        coeffs[4] = 9;
        coeffs[8] = 30;
        let q = quantize_block(coeffs, pair(dc, ac), 0);
        // levels are in zig-zag order: ZIGZAG = [0,1,4,8,...].
        assert_eq!(q.levels[0], 2, "DC level");
        assert_eq!(q.levels[1], -2, "AC level at natural 1");
        assert_eq!(q.levels[2], 0, "AC level at natural 4 rounds to 0");
        assert_eq!(q.levels[3], 2, "AC level at natural 8");
        // last non-zero zig-zag index is 3.
        assert_eq!(q.last, 3);
        // recon[j] == level * Q in NATURAL order — the decoder contract.
        assert_eq!(q.recon[0], 20);
        assert_eq!(q.recon[1], -40);
        assert_eq!(q.recon[4], 0);
        assert_eq!(q.recon[8], 40);
    }

    #[test]
    fn empty_block_reports_last_below_first() {
        // A block whose every quantized level is zero reports last = first - 1, so
        // nz = last + 1 = first (matching GetCoeffs' empty-block return).
        let dc = super::QFactor::new(200);
        let ac = super::QFactor::new(200);
        let coeffs = [1i16; 16]; // every |coeff| < 100 -> rounds to 0
        let q0 = quantize_block(coeffs, pair(dc, ac), 0);
        assert_eq!(q0.last, -1, "first=0 empty -> last -1 (nz 0)");
        let q1 = quantize_block(coeffs, pair(dc, ac), 1);
        assert_eq!(q1.last, 0, "first=1 empty -> last 0 (nz 1)");
    }

    #[test]
    fn quality_to_base_q_is_monotonic_and_bounded() {
        assert_eq!(quality_to_base_q(100), 0, "finest at q100");
        assert_eq!(quality_to_base_q(0), 127, "coarsest at q0");
        // Monotonically non-increasing in quality.
        let mut prev = quality_to_base_q(0);
        for quality in 1..=100u8 {
            let cur = quality_to_base_q(quality);
            assert!(cur <= prev, "q{quality}: {cur} > {prev}");
            assert!((0..=127).contains(&cur));
            prev = cur;
        }
        // Values above 100 saturate.
        assert_eq!(quality_to_base_q(200), quality_to_base_q(100));
    }

    #[test]
    fn full_pipeline_high_quality_is_near_lossless() {
        // fdct -> quantize (finest q, dequant step 4) -> dequant (recon) -> idct
        // must reconstruct the residual within the quantization error. At base_q 0
        // the luma steps are 4 (DC) / 4 (AC), so per-sample error stays tiny.
        let q = Quantizer::new(0);
        let residual: [i16; 16] = [
            -40, 12, -5, 33, 7, -18, 22, -3, 15, -27, 9, 41, -11, 6, -30, 19,
        ];
        let coeffs = fdct4x4(residual);
        let quantized = quantize_block(coeffs, q.y1, 0);
        let mut plane = [128u8; 16];
        transform_one(&quantized.recon, &mut plane, 0, 4);
        for (i, (&r, &p)) in residual.iter().zip(&plane).enumerate() {
            let want = 128 + i32::from(r);
            let got = i32::from(p);
            assert!(
                (got - want).abs() <= 6,
                "sample {i}: got {got}, want ~{want}"
            );
        }
    }

    #[test]
    fn sns_quant_delta_is_zero_centered_and_signed() {
        use super::sns_quant_delta;
        // Neutral activity or zero strength => exactly zero (the byte-identity anchor).
        assert_eq!(sns_quant_delta(128, 100), 0, "mid activity => 0 delta");
        assert_eq!(sns_quant_delta(0, 0), 0, "zero strength => 0 delta");
        assert_eq!(sns_quant_delta(255, 0), 0, "zero strength => 0 delta");
        // Flat (low activity) refines (negative); busy (high activity) coarsens (positive).
        assert!(sns_quant_delta(0, 100) < 0, "flat block coded finer");
        assert!(sns_quant_delta(255, 100) > 0, "busy block coarsened");
        // Monotonic in strength and symmetric in magnitude about the midpoint.
        assert!(sns_quant_delta(200, 100) > sns_quant_delta(200, 40));
        assert_eq!(sns_quant_delta(64, 100), -sns_quant_delta(192, 100));
        // Strength saturates at 100.
        assert_eq!(sns_quant_delta(255, 200), sns_quant_delta(255, 100));
    }

    #[test]
    fn zigzag_positions_map_levels_to_natural_recon() {
        // Sanity: a single AC coefficient at natural position ZIGZAG[5] produces a
        // level at zig-zag index 5 and a recon at that natural position only.
        let dc = super::QFactor::new(8);
        let ac = super::QFactor::new(8);
        let mut coeffs = [0i16; 16];
        let nat = ZIGZAG[5];
        coeffs[nat] = 40;
        let q = quantize_block(coeffs, pair(dc, ac), 0);
        assert_eq!(q.levels[5], 5, "level at zig-zag index 5");
        assert_eq!(q.last, 5);
        assert_eq!(q.recon[nat], 40, "recon at natural ZIGZAG[5]");
        assert_eq!(q.recon[0], 0, "DC untouched");
    }
}
