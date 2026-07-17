//! Perceptual (texture-aware) distortion for the luma mode decision.
//!
//! Plain sum-of-squared-error ([`crate::lossy::frame::sse_block`]) measures how far the
//! reconstruction drifts from the source pixel by pixel, but it is blind to *where*
//! the error lands in the frequency plane: smoothing a textured block and adding an
//! equal amount of energy to a flat one score alike, yet the eye only forgives the
//! former. Libwebp corrects for this by adding a **texture** term — the difference of
//! the two blocks under a 4×4 Hadamard transform, weighted by [`K_WEIGHT_Y`] so
//! low-frequency structure counts for more — scaled by a strength derived from the
//! `sns_strength` knob and the quantizer step.
//!
//! [`disto16`] is that combined distortion, `SSE + MULT(tlambda, TDisto)`, replacing
//! the bare SSE in the 16×16 / intra-4×4 luma decision only (chroma keeps plain SSE).
//! Everything here is integer / fixed-point — no `float_arithmetic` — and the Hadamard
//! [`ttransform`] mirrors libwebp's `TTransform` term for term.
//!
//! `tlambda == 0` (the `sns_strength == 0` case, and the current not-yet-wired
//! default) short-circuits [`disto16`] to the exact SSE, so the perceptual term is a
//! strict superset of the plain-SSE decision and cannot perturb it when disabled.

/// Per-Hadamard-coefficient weights for the 4×4 texture transform ([`ttransform`]),
/// laid out row-major over the transform's 4×4 output. Libwebp's `kWeightY`: low
/// spatial frequencies (top-left) dominate, so structural error is penalized far more
/// than high-frequency noise the eye tolerates. A committed constant, not derived.
pub(crate) const K_WEIGHT_Y: [i64; 16] = [
    38, 32, 20, 9, //
    32, 28, 17, 7, //
    20, 17, 10, 4, //
    9, 7, 4, 2, //
];

/// Down-shift folded into [`tdisto4`] (libwebp `Disto4x4`'s `>> 5`): the raw weighted
/// Hadamard difference is coarse, so it is scaled back before it competes with SSE.
const TDISTO_SHIFT: u32 = 5;
/// Right shift of the `tlambda * TDisto` product in [`disto16`] (libwebp `MULT_8B`),
/// with a rounding bias so the fixed-point multiply rounds to nearest.
const MULT_SHIFT: u32 = 8;
/// Rounding bias for the [`MULT_SHIFT`] descale.
const MULT_BIAS: i64 = 1 << (MULT_SHIFT - 1);
/// Right shift turning the `sns_strength * q_ac` product into `tlambda` (libwebp's
/// texture-lambda scale).
const TLAMBDA_SHIFT: u32 = 5;

/// The texture-distortion strength `tlambda = (sns_strength * q_ac) >> 5` weighting
/// the Hadamard term against SSE in [`disto16`]. Grows with both the `sns_strength`
/// knob (`0..=100`) and the plane's AC dequant step `q_ac`, so coarser quantization
/// leans harder on texture preservation. `sns_strength == 0` yields `0`, which
/// [`disto16`] treats as "plain SSE".
pub(crate) fn tlambda(sns_strength: u8, q_ac: i32) -> i64 {
    let strength = i64::from(sns_strength.min(100));
    (strength * i64::from(q_ac)) >> TLAMBDA_SHIFT
}

/// A read window into a plane: the sample buffer, the offset of the region's top-left
/// sample, and the row stride. Bundles the `(plane, offset, stride)` triple the texture
/// transform walks so the distortion entry points stay within the argument budget.
pub(crate) struct PlaneWindow<'a> {
    data: &'a [u8],
    off: usize,
    stride: usize,
}

impl<'a> PlaneWindow<'a> {
    /// A window into `data` whose region starts at `off` with row `stride`.
    pub(crate) const fn new(data: &'a [u8], off: usize, stride: usize) -> Self {
        Self { data, off, stride }
    }
}

/// The 4×4 Hadamard ("T") transform of one block, returning the [`K_WEIGHT_Y`]-weighted
/// sum of absolute transformed coefficients — libwebp's `TTransform`. `block` is the
/// 4×4 samples in row-major order.
fn ttransform(block: [i32; 16]) -> i64 {
    let mut tmp = [0i32; 16];
    // Horizontal pass over the four rows.
    for i in 0..4 {
        let r = i * 4;
        let a0 = block[r] + block[r + 2];
        let a1 = block[r + 1] + block[r + 3];
        let a2 = block[r + 1] - block[r + 3];
        let a3 = block[r] - block[r + 2];
        tmp[r] = a0 + a1;
        tmp[r + 1] = a3 + a2;
        tmp[r + 2] = a3 - a2;
        tmp[r + 3] = a0 - a1;
    }
    // Vertical pass over the four columns, weighting each output by K_WEIGHT_Y.
    let mut sum = 0i64;
    for i in 0..4 {
        let a0 = tmp[i] + tmp[8 + i];
        let a1 = tmp[4 + i] + tmp[12 + i];
        let a2 = tmp[4 + i] - tmp[12 + i];
        let a3 = tmp[i] - tmp[8 + i];
        let b0 = a0 + a1;
        let b1 = a3 + a2;
        let b2 = a3 - a2;
        let b3 = a0 - a1;
        sum += K_WEIGHT_Y[i] * i64::from(b0.abs());
        sum += K_WEIGHT_Y[i + 4] * i64::from(b1.abs());
        sum += K_WEIGHT_Y[i + 8] * i64::from(b2.abs());
        sum += K_WEIGHT_Y[i + 12] * i64::from(b3.abs());
    }
    sum
}

/// Read the 4×4 sub-block at offset `(dx, dy)` within `w`'s region into a row-major
/// `[i32; 16]`.
fn gather4(w: &PlaneWindow<'_>, dx: usize, dy: usize) -> [i32; 16] {
    let mut block = [0i32; 16];
    let base = w.off + dy * w.stride + dx;
    for row in 0..4 {
        for col in 0..4 {
            block[row * 4 + col] = i32::from(w.data[base + row * w.stride + col]);
        }
    }
    block
}

/// The 4×4 texture distortion between the source and reconstruction sub-blocks at
/// `(dx, dy)`: the absolute difference of their [`ttransform`] sums, descaled by
/// [`TDISTO_SHIFT`] (libwebp `Disto4x4`).
fn tdisto4(src: &PlaneWindow<'_>, rec: &PlaneWindow<'_>, dx: usize, dy: usize) -> i64 {
    let ts = ttransform(gather4(src, dx, dy));
    let tr = ttransform(gather4(rec, dx, dy));
    (tr - ts).abs() >> TDISTO_SHIFT
}

/// The 16×16 texture distortion (libwebp `Disto16x16`): the sum of [`tdisto4`] over the
/// sixteen 4×4 sub-blocks of a macroblock's luma, over the `src` and `rec` windows.
fn tdisto16(src: &PlaneWindow<'_>, rec: &PlaneWindow<'_>) -> i64 {
    let mut d = 0i64;
    for n in 0..16usize {
        let (dx, dy) = ((n % 4) * 4, (n / 4) * 4);
        d += tdisto4(src, rec, dx, dy);
    }
    d
}

/// Combine a macroblock's luma `sse` with its texture distortion into the perceptual
/// distortion `SSE + MULT(tlambda, TDisto)` (libwebp's `D + SD`). When `tlambda == 0`
/// this is exactly `sse`, so a disabled perceptual term leaves the SSE decision
/// bit-identical. Otherwise the [`tdisto16`] texture term over the `src`/`rec` windows
/// is folded in through the rounding fixed-point multiply.
pub(crate) fn disto16(sse: i64, tlambda: i64, src: &PlaneWindow<'_>, rec: &PlaneWindow<'_>) -> i64 {
    if tlambda == 0 {
        return sse;
    }
    let texture = tdisto16(src, rec);
    sse + ((tlambda * texture + MULT_BIAS) >> MULT_SHIFT)
}

#[cfg(test)]
mod tests {
    use super::{PlaneWindow, disto16, tlambda, ttransform};

    #[test]
    fn tlambda_scales_with_strength_and_step_and_zeroes_at_zero_strength() {
        // (sns * q_ac) >> 5, clamped strength at 100.
        assert_eq!(tlambda(0, 200), 0, "zero strength => zero lambda");
        assert_eq!(tlambda(50, 0), 0, "zero step => zero lambda");
        assert_eq!(tlambda(50, 64), (50 * 64) >> 5);
        assert_eq!(
            tlambda(200, 64),
            tlambda(100, 64),
            "strength saturates at 100"
        );
        assert!(tlambda(80, 128) > tlambda(40, 128), "monotone in strength");
        assert!(tlambda(50, 256) > tlambda(50, 32), "monotone in step");
    }

    #[test]
    fn ttransform_of_a_flat_block_is_only_its_dc() {
        // A constant block has a single non-zero Hadamard coefficient (the DC, output
        // position 0), so the weighted sum is K_WEIGHT_Y[0] * |16 * value|.
        let flat = [7i32; 16];
        assert_eq!(ttransform(flat), 38 * (16 * 7));
        // The all-zero block transforms to zero.
        assert_eq!(ttransform([0i32; 16]), 0);
    }

    #[test]
    fn ttransform_is_hand_verified_on_a_single_impulse() {
        // A unit impulse at the top-left sample spreads to every Hadamard output with
        // magnitude 1, so the weighted sum equals the full K_WEIGHT_Y total.
        let mut block = [0i32; 16];
        block[0] = 1;
        let total: i64 = super::K_WEIGHT_Y.iter().sum();
        assert_eq!(ttransform(block), total);
    }

    #[test]
    fn disto16_is_pure_sse_when_lambda_is_zero() {
        // With tlambda == 0 the texture term is skipped entirely: disto16 returns the
        // SSE argument verbatim regardless of the pixel content, which is what keeps
        // the perceptual wiring byte-neutral until it is enabled.
        let src = vec![10u8; 20 * 20];
        let rec = vec![200u8; 20 * 20];
        let sw = PlaneWindow::new(&src, 21, 20);
        let rw = PlaneWindow::new(&rec, 21, 20);
        assert_eq!(
            disto16(12_345, 0, &sw, &rw),
            12_345,
            "lambda 0 must pass SSE through untouched"
        );
    }

    #[test]
    fn disto16_adds_a_texture_penalty_when_enabled() {
        // A source with structure vs a flat reconstruction has a non-zero texture term,
        // so an enabled disto16 exceeds the bare SSE by MULT(tlambda, TDisto) > 0.
        let mut src = vec![0u8; 20 * 20];
        for (i, p) in src.iter_mut().enumerate() {
            *p = u8::try_from((i * 7) % 200).unwrap_or(0);
        }
        let rec = vec![100u8; 20 * 20];
        let sw = PlaneWindow::new(&src, 21, 20);
        let rw = PlaneWindow::new(&rec, 21, 20);
        let plain = disto16(1000, 0, &sw, &rw);
        let perceptual = disto16(1000, 4000, &sw, &rw);
        assert_eq!(plain, 1000);
        assert!(
            perceptual > plain,
            "an enabled texture term must raise the distortion ({perceptual} <= {plain})"
        );
    }
}
