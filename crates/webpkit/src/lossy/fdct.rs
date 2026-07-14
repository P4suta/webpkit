//! Forward transforms (the encoder counterparts of [`crate::lossy::idct`]).
//!
//! Fixed-point integer only. [`fdct4x4`] is the forward 4×4 DCT (libwebp
//! `FTransform_C`, multipliers 2217/5352) and [`fwht`] the forward Walsh–Hadamard
//! transform of the sixteen luma DC coefficients (libwebp `FTransformWHT_C`).
//! They are the matched forward pair of `idct::transform_one` / `transform_wht`:
//! feeding an `fdct4x4` output back through `transform_one` reconstructs the
//! residual to within the transform's rounding, and quantizing in between yields
//! the lossy round-trip the whole encoder is built on.
//!
//! Bit-exactness against libwebp is *not* required for correctness — the encoder
//! reconstructs with the very same inverse transforms the decoder uses, so any
//! forward transform yields a self-consistent bitstream. Matching libwebp's
//! forward constants only maximizes quality (rate/distortion), which the
//! round-trip unit tests below pin.
#![allow(
    clippy::cast_possible_truncation,
    reason = "VP8 forward transforms store results into i16 with the C int16_t \
              wrapping semantics of the reference encoder; the casts reproduce \
              that exactly (the dynamic range is bounded well within i16)"
)]

use crate::lossy::work::work;

/// Forward 4×4 DCT of a residual block `residual` (natural raster order, samples
/// in `-255..=255`), returning the 16 transform coefficients in natural order.
/// Bit-exact port of libwebp `FTransform_C` (`dsp/enc.c`): a horizontal pass
/// folds rows into an `i32` scratch, a vertical pass folds columns and descales.
#[must_use]
pub(crate) fn fdct4x4(residual: [i16; 16]) -> [i16; 16] {
    work!(FdctCall);
    let mut tmp = [0i32; 16];
    for i in 0..4 {
        let d0 = i32::from(residual[i * 4]);
        let d1 = i32::from(residual[i * 4 + 1]);
        let d2 = i32::from(residual[i * 4 + 2]);
        let d3 = i32::from(residual[i * 4 + 3]);
        let a0 = d0 + d3;
        let a1 = d1 + d2;
        let a2 = d1 - d2;
        let a3 = d0 - d3;
        tmp[i * 4] = (a0 + a1) * 8;
        tmp[i * 4 + 1] = (a2 * 2217 + a3 * 5352 + 1812) >> 9;
        tmp[i * 4 + 2] = (a0 - a1) * 8;
        tmp[i * 4 + 3] = (a3 * 2217 - a2 * 5352 + 937) >> 9;
    }
    let mut out = [0i16; 16];
    for i in 0..4 {
        let a0 = tmp[i] + tmp[12 + i];
        let a1 = tmp[4 + i] + tmp[8 + i];
        let a2 = tmp[4 + i] - tmp[8 + i];
        let a3 = tmp[i] - tmp[12 + i];
        out[i] = ((a0 + a1 + 7) >> 4) as i16;
        out[4 + i] = (((a2 * 2217 + a3 * 5352 + 12000) >> 16) + i32::from(a3 != 0)) as i16;
        out[8 + i] = ((a0 - a1 + 7) >> 4) as i16;
        out[12 + i] = ((a3 * 2217 - a2 * 5352 + 51000) >> 16) as i16;
    }
    out
}

/// Forward Walsh–Hadamard transform of the sixteen luma-block DC coefficients
/// `dc` (block raster order: `dc[b]` is the DC of luma sub-block `b`), returning
/// the sixteen second-order (Y2) coefficients in natural order. Bit-exact port of
/// libwebp `FTransformWHT_C`; the final `>> 1` descale is the forward companion of
/// `idct::transform_wht`'s `+3` / `>> 3` inverse.
#[must_use]
pub(crate) fn fwht(dc: [i16; 16]) -> [i16; 16] {
    let mut tmp = [0i32; 16];
    for i in 0..4 {
        let a0 = i32::from(dc[i * 4]) + i32::from(dc[i * 4 + 2]);
        let a1 = i32::from(dc[i * 4 + 1]) + i32::from(dc[i * 4 + 3]);
        let a2 = i32::from(dc[i * 4 + 1]) - i32::from(dc[i * 4 + 3]);
        let a3 = i32::from(dc[i * 4]) - i32::from(dc[i * 4 + 2]);
        tmp[i * 4] = a0 + a1;
        tmp[i * 4 + 1] = a3 + a2;
        tmp[i * 4 + 2] = a3 - a2;
        tmp[i * 4 + 3] = a0 - a1;
    }
    let mut out = [0i16; 16];
    for i in 0..4 {
        let a0 = tmp[i] + tmp[8 + i];
        let a1 = tmp[4 + i] + tmp[12 + i];
        let a2 = tmp[4 + i] - tmp[12 + i];
        let a3 = tmp[i] - tmp[8 + i];
        out[i] = ((a0 + a1) >> 1) as i16;
        out[4 + i] = ((a3 + a2) >> 1) as i16;
        out[8 + i] = ((a3 - a2) >> 1) as i16;
        out[12 + i] = ((a0 - a1) >> 1) as i16;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{fdct4x4, fwht};
    use crate::lossy::idct::{transform_one, transform_wht};

    #[test]
    fn fdct_dc_only_scales_by_eight() {
        // A constant residual C has all its energy in the DC term. FTransform's DC
        // path is out[0] = (128*C + 7) >> 4 = 8*C for the values here; C=10 -> 80.
        // (A tiny AC ripple out[1]=1 arises from the reference's rounding
        // constants and is exactly what libwebp emits — checked below.)
        let coeffs = fdct4x4([10; 16]);
        assert_eq!(coeffs[0], 80, "DC");
        assert_eq!(coeffs[1], 1, "libwebp rounding ripple");
        for (i, &c) in coeffs.iter().enumerate().skip(2) {
            assert_eq!(c, 0, "coeff[{i}] must be zero for a flat block");
        }
    }

    #[test]
    fn fdct_single_horizontal_frequency() {
        // A pure horizontal ramp residual [-3,-1,1,3] repeated down the rows is a
        // single horizontal frequency: energy concentrates in out[1] (the first
        // horizontal AC), and vertical AC positions (out[4], out[8], out[12]) stay
        // zero. This catches a transposed pass.
        let mut residual = [0i16; 16];
        for row in 0..4 {
            residual[row * 4] = -3;
            residual[row * 4 + 1] = -1;
            residual[row * 4 + 2] = 1;
            residual[row * 4 + 3] = 3;
        }
        let coeffs = fdct4x4(residual);
        assert_eq!(coeffs[0], 0, "no DC");
        assert!(coeffs[1] != 0, "horizontal AC present");
        assert_eq!(coeffs[4], 0, "no vertical AC");
        assert_eq!(coeffs[8], 0);
        assert_eq!(coeffs[12], 0);
    }

    #[test]
    fn fdct_then_idct_reconstructs_within_rounding() {
        // The load-bearing property: fdct4x4 is the forward inverse of
        // `transform_one`. Without quantization, forward-then-inverse must
        // reproduce the residual to within the transforms' fixed-point rounding.
        // A deterministic spread of residuals over a flat 128 prediction is
        // reconstructed and compared sample-by-sample.
        let residuals: [[i16; 16]; 3] = [
            [
                -40, 12, -5, 33, 7, -18, 22, -3, 15, -27, 9, 41, -11, 6, -30, 19,
            ],
            [
                64, 64, 64, 64, -64, -64, -64, -64, 32, 32, 32, 32, -32, -32, -32, -32,
            ],
            [
                1, -2, 3, -4, 5, -6, 7, -8, 9, -10, 11, -12, 13, -14, 15, -16,
            ],
        ];
        for &residual in &residuals {
            let coeffs = fdct4x4(residual);
            // Reconstruct onto a flat 128 prediction (stride 4, one 4x4 block).
            let mut plane = [128u8; 16];
            transform_one(&coeffs, &mut plane, 0, 4);
            for (i, (&r, &p)) in residual.iter().zip(&plane).enumerate() {
                let want = 128 + i32::from(r);
                let got = i32::from(p);
                assert!(
                    (got - want).abs() <= 2,
                    "sample {i}: got {got}, want ~{want} (residual {r})"
                );
            }
        }
    }

    #[test]
    fn fwht_then_iwht_reconstructs_within_rounding() {
        // fwht is the forward inverse of `transform_wht`: transforming the 16 luma
        // DCs then inverse-WHT-scattering them must reproduce the DCs (which land
        // in the DC slot, stride 16, of each luma block) to within rounding.
        let dcs: [i16; 16] = [
            10, -20, 30, -40, 5, 15, -25, 35, -8, 18, -28, 38, 12, -22, 32, -42,
        ];
        let y2 = fwht(dcs);
        let mut coeffs = [0i16; 384];
        transform_wht(y2, &mut coeffs);
        for (b, &dc) in dcs.iter().enumerate() {
            let got = coeffs[b * 16];
            assert!(
                (i32::from(got) - i32::from(dc)).abs() <= 1,
                "block {b} DC: got {got}, want ~{dc}"
            );
        }
    }

    #[test]
    fn fdct_output_is_pinned_byte_for_byte() {
        // A golden exact-coefficient assertion over a residual with genuine vertical
        // variation (so the second-pass `a3 = tmp[i] - tmp[12+i]` is non-zero in
        // several columns). This pins the exact `out[4+i]` rounding term
        // `+ i32::from(a3 != 0)`: turning that `+` into `-` (drops to `-1`) or `*`
        // (zeroes / drops the +1) changes at least one coefficient, and any other
        // arithmetic mutation in the transform flips a coefficient too.
        let residual: [i16; 16] = [
            -40, 12, -5, 33, 7, -18, 22, -3, 15, -27, 9, 41, -11, 6, -30, 19,
        ];
        assert_eq!(
            fdct4x4(residual),
            [
                15, -83, 46, -17, 3, -31, -49, 8, -31, -22, -28, -108, 24, -47, 36, -15
            ],
            "fdct4x4 golden coefficients",
        );
    }

    #[test]
    fn fwht_constant_dc_is_exact() {
        // A constant DC field K round-trips exactly: fwht concentrates it into a
        // single Y2 DC of 8*K, and transform_wht's (8K+3)>>3 recovers K precisely.
        let y2 = fwht([10; 16]);
        assert_eq!(y2[0], 80, "Y2 DC = 8*K");
        for (i, &c) in y2.iter().enumerate().skip(1) {
            assert_eq!(c, 0, "Y2 coeff[{i}] must be zero for a flat DC field");
        }
        let mut coeffs = [0i16; 384];
        transform_wht(y2, &mut coeffs);
        for b in 0..16 {
            assert_eq!(coeffs[b * 16], 10, "block {b} DC recovered exactly");
        }
    }
}
