//! Inverse transforms (RFC 6386 §14, transcribed from libwebp `dsp/dec.c`).
//!
//! Fixed-point integer only. The 4×4 inverse DCT (`transform_one` and friends)
//! serves reconstruction; the inverse Walsh–Hadamard transform is used while
//! parsing a macroblock's second-order (Y2) DC block.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "VP8 transforms store results into i16 with the C int16_t wrapping \
              semantics of the reference decoder, and clip8 narrows an already-\
              clamped [0,255] value to u8; the casts reproduce that exactly"
)]

use crate::lossy::work::work;

/// Inverse Walsh–Hadamard transform of the 16 second-order (Y2) DC coefficients
/// in `input`, scattering the 16 results into the DC slot (stride 16) of each of
/// the 16 luma 4×4 coefficient blocks in `out`. Port of libwebp `TransformWHT_C`.
pub(crate) fn transform_wht(input: [i16; 16], out: &mut [i16; 384]) {
    work!(IdctCall);
    let mut tmp = [0i32; 16];
    for i in 0..4 {
        let a0 = i32::from(input[i]) + i32::from(input[12 + i]);
        let a1 = i32::from(input[4 + i]) + i32::from(input[8 + i]);
        let a2 = i32::from(input[4 + i]) - i32::from(input[8 + i]);
        let a3 = i32::from(input[i]) - i32::from(input[12 + i]);
        tmp[i] = a0 + a1;
        tmp[8 + i] = a0 - a1;
        tmp[4 + i] = a3 + a2;
        tmp[12 + i] = a3 - a2;
    }
    for i in 0..4 {
        let dc = tmp[i * 4] + 3; // with rounder
        let a0 = dc + tmp[3 + i * 4];
        let a1 = tmp[1 + i * 4] + tmp[2 + i * 4];
        let a2 = tmp[1 + i * 4] - tmp[2 + i * 4];
        let a3 = dc - tmp[3 + i * 4];
        let base = i * 64;
        out[base] = ((a0 + a1) >> 3) as i16;
        out[base + 16] = ((a3 + a2) >> 3) as i16;
        out[base + 32] = ((a0 - a1) >> 3) as i16;
        out[base + 48] = ((a3 - a2) >> 3) as i16;
    }
}

/// DC-only inverse DCT: when the only non-zero coefficient of a 4×4 block is its
/// DC term, the full [`transform_one`] reduces to adding one constant residual
/// `(dc + 4) >> 3` to every one of the 16 samples. Bit-exact with `transform_one`
/// on a DC-only block (port of libwebp `TransformDC_C`), skipping both butterfly
/// passes. `dc` is the block's DCT-domain DC coefficient (`coeffs[0]`).
pub(crate) fn transform_dc(dc: i16, plane: &mut [u8], off: usize, stride: usize) {
    work!(IdctCall);
    let d = (i32::from(dc) + 4) >> 3;
    for row in 0..4 {
        let base = off + row * stride;
        for x in 0..4 {
            let idx = base + x;
            plane[idx] = clip8(i32::from(plane[idx]) + d);
        }
    }
}

/// The `20091` fixed-point multiply of libwebp's `WEBP_TRANSFORM_AC3_MUL1`
/// macro (`dsp/dsp.h`): `((a * 20091) >> 16) + a`, an arithmetic (sign-extending)
/// `i32` shift, exactly matching the reference.
const fn mul1(a: i32) -> i32 {
    ((a * 20091) >> 16) + a
}

/// The `35468` fixed-point multiply of libwebp's `WEBP_TRANSFORM_AC3_MUL2`
/// macro (`dsp/dsp.h`): `(a * 35468) >> 16`, an arithmetic `i32` shift.
const fn mul2(a: i32) -> i32 {
    (a * 35468) >> 16
}

/// Reconstruction clip to `[0, 255]`, matching libwebp's `clip_8b` (`dsp/dec.c`).
fn clip8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// libwebp's `STORE(x, y, v)` macro (`dsp/dec.c`) for a single output row: add
/// the rounded residual `v >> 3` (arithmetic shift) to the destination sample
/// and clip to `[0, 255]`.
fn store(plane: &mut [u8], row: usize, x: usize, v: i32) {
    let idx = row + x;
    plane[idx] = clip8(i32::from(plane[idx]) + (v >> 3));
}

/// Full 4×4 inverse DCT of `coeffs` (natural raster order, `coeffs[0..16]`),
/// adding the reconstructed residual on top of the existing 4×4 block at
/// `plane[off..]` (row `stride`) and clipping every sample to `[0, 255]`.
/// Bit-exact port of libwebp `TransformOne_C` (`dsp/dec.c`): a vertical pass
/// folds columns into an intermediate `i32` block, a horizontal pass folds rows
/// and stores. `dst[x + y * BPS]` maps to `plane[off + x + y * stride]`.
pub(crate) fn transform_one(coeffs: &[i16], plane: &mut [u8], off: usize, stride: usize) {
    work!(IdctCall);
    let mut tmp = [0i32; 16];
    for i in 0..4 {
        let in0 = i32::from(coeffs[i]);
        let in8 = i32::from(coeffs[8 + i]);
        let in4 = i32::from(coeffs[4 + i]);
        let in12 = i32::from(coeffs[12 + i]);
        let a = in0 + in8;
        let b = in0 - in8;
        let c = mul2(in4) - mul1(in12);
        let d = mul1(in4) + mul2(in12);
        let base = i * 4;
        tmp[base] = a + d;
        tmp[base + 1] = b + c;
        tmp[base + 2] = b - c;
        tmp[base + 3] = a - d;
    }
    for i in 0..4 {
        let dc = tmp[i] + 4;
        let t8 = tmp[8 + i];
        let a = dc + t8;
        let b = dc - t8;
        let c = mul2(tmp[4 + i]) - mul1(tmp[12 + i]);
        let d = mul1(tmp[4 + i]) + mul2(tmp[12 + i]);
        let row = off + i * stride;
        store(plane, row, 0, a + d);
        store(plane, row, 1, b + c);
        store(plane, row, 2, b - c);
        store(plane, row, 3, a - d);
    }
}

#[cfg(test)]
mod tests {
    use super::{mul1, mul2, transform_dc, transform_one, transform_wht};

    // ---- DC-only fast path equivalence -----------------------------------

    #[test]
    fn transform_dc_matches_full_transform_on_dc_only_blocks() {
        // The DC-only fast path must be byte-identical to running the full 4×4
        // inverse DCT on a block whose only non-zero coefficient is the DC term,
        // across a spread of DC magnitudes, background samples and a padded stride.
        let stride = 6;
        for dc in [-2048i16, -600, -100, -8, -1, 0, 1, 7, 8, 100, 600, 2047] {
            for bg in [0u8, 1, 5, 100, 128, 250, 255] {
                let mut full = [bg; 4 * 6];
                let mut fast = [bg; 4 * 6];
                let mut coeffs = [0i16; 16];
                coeffs[0] = dc;
                transform_one(&coeffs, &mut full, 0, stride);
                transform_dc(dc, &mut fast, 0, stride);
                assert_eq!(fast, full, "dc={dc} bg={bg}");
            }
        }
    }

    // ---- second-order WHT (DC scatter) -----------------------------------

    #[test]
    fn all_zero_input_is_all_zero_dc() {
        let mut out = [1i16; 384];
        transform_wht([0; 16], &mut out);
        // Every DC slot (stride 16) becomes (0 + 3) >> 3 = 0.
        for b in 0..16 {
            assert_eq!(out[b * 16], 0);
        }
    }

    #[test]
    fn uniform_dc_propagates_to_every_block() {
        // A single non-zero DC coefficient produces a constant field: with only
        // in[0] set to 8, both passes sum it into every block's DC as 1.
        let mut input = [0i16; 16];
        input[0] = 8;
        let mut out = [0i16; 384];
        transform_wht(input, &mut out);
        for b in 0..16 {
            assert_eq!(out[b * 16], 1, "block {b} DC");
        }
    }

    #[test]
    fn wht_horizontal_split_from_in0_and_in1() {
        // Non-uniform Y2 input in[0]=40, in[1]=80 (a DC + one *horizontal*
        // frequency). Derived from TransformWHT_C by hand:
        //
        // Column pass (only columns 0 and 1 have input):
        //   col 0 (in[0]=40): a0=40, a1=0, a2=0, a3=40
        //     -> tmp[0]=tmp[4]=tmp[8]=tmp[12]=40
        //   col 1 (in[1]=80): a0=80, a1=0, a2=0, a3=80
        //     -> tmp[1]=tmp[5]=tmp[9]=tmp[13]=80
        //   all other tmp = 0, so every row i has tmp[i*4..]= [40, 80, 0, 0].
        // Row pass, every i (identical rows): dc=40+3=43
        //   a0=dc+tmp3=43, a1=tmp1+tmp2=80, a2=tmp1-tmp2=80, a3=dc-tmp3=43
        //   out[+0]=(a0+a1)>>3=(123)>>3=15   -> blocks 0,1 of the row-group
        //   out[+16]=(a3+a2)>>3=(123)>>3=15
        //   out[+32]=(a0-a1)>>3=(-37)>>3=-5  (arith: floor(-37/8)=-5)
        //   out[+48]=(a3-a2)>>3=(-37)>>3=-5  -> blocks 2,3 of the row-group
        // => within each group of 4 blocks: [15, 15, -5, -5], same for all rows.
        const SENT: i16 = 999;
        let mut input = [0i16; 16];
        input[0] = 40;
        input[1] = 80;
        let mut out = [SENT; 384];
        transform_wht(input, &mut out);

        let expected: [i16; 16] = [
            15, 15, -5, -5, 15, 15, -5, -5, 15, 15, -5, -5, 15, 15, -5, -5,
        ];
        for (b, &want) in expected.iter().enumerate() {
            assert_eq!(out[b * 16], want, "block {b} DC");
        }
        // The WHT must write ONLY the DC slots (index % 16 == 0); every other
        // coefficient position must remain untouched at the sentinel.
        for (i, &v) in out.iter().enumerate() {
            if i % 16 != 0 {
                assert_eq!(v, SENT, "non-DC coeff slot {i} was clobbered");
            }
        }
    }

    #[test]
    fn wht_vertical_split_from_in0_and_in4() {
        // Transpose companion of the previous test: in[0]=40, in[4]=80 is a DC
        // plus one *vertical* frequency, so the same magnitudes (123 -> 15,
        // -37 -> -5) must scatter DOWN the block rows instead of across.
        //
        // Column pass: only column 0 has input (in[0]=40, in[4]=80):
        //   a0=40, a1=80, a2=80, a3=40
        //   tmp[0]=a0+a1=120, tmp[4]=a3+a2=120, tmp[8]=a0-a1=-40, tmp[12]=a3-a2=-40
        //   all other tmp = 0.
        // Row pass: each row i reads tmp[i*4..], only column 0 nonzero:
        //   i=0: dc=120+3=123, a0=a3=123, a1=a2=0 -> all four outs=(123)>>3=15
        //   i=1: tmp[4]=120  -> same -> 15
        //   i=2: tmp[8]=-40  -> dc=-37, all outs=(-37)>>3=-5
        //   i=3: tmp[12]=-40 -> -5
        // => blocks 0..7 = 15, blocks 8..15 = -5.
        let mut input = [0i16; 16];
        input[0] = 40;
        input[4] = 80;
        let mut out = [0i16; 384];
        transform_wht(input, &mut out);

        let expected: [i16; 16] = [
            15, 15, 15, 15, 15, 15, 15, 15, -5, -5, -5, -5, -5, -5, -5, -5,
        ];
        for (b, &want) in expected.iter().enumerate() {
            assert_eq!(out[b * 16], want, "block {b} DC");
        }
    }

    // ---- fixed-point multipliers -----------------------------------------

    #[test]
    fn mul_constants_match_reference() {
        // Hand-computed with arithmetic (sign-extending) 16-bit right shifts.
        // mul1(a)=((a*20091)>>16)+a, mul2(a)=(a*35468)>>16.
        assert_eq!((mul1(0), mul2(0)), (0, 0));
        assert_eq!((mul1(100), mul2(100)), (130, 54));
        assert_eq!((mul1(-100), mul2(-100)), (-131, -55));
        // The exact multiplier values the AC KATs below depend on:
        //   200*20091=4_018_200; >>16 = 61; +200 => 261
        //   200*35468=7_093_600; >>16 = 108
        //  -160*20091=-3_214_560; >>16 = -50 (floor -49.05); +(-160) => -210
        //  -160*35468=-5_674_880; >>16 = -87 (floor -86.59)
        assert_eq!((mul1(200), mul2(200)), (261, 108));
        assert_eq!((mul1(-160), mul2(-160)), (-210, -87));
    }

    // ---- 4x4 inverse DCT: DC-only regressions (kept) ---------------------

    #[test]
    fn transform_one_dc_only_adds_uniform_offset() {
        // DC = 28 => every sample gains (28 + 4) >> 3 == 4, over the existing 100.
        let stride = 4;
        let mut plane = [100u8; 16];
        let mut coeffs = [0i16; 16];
        coeffs[0] = 28;
        transform_one(&coeffs, &mut plane, 0, stride);
        assert!(plane.iter().all(|&p| p == 104));
    }

    #[test]
    fn transform_one_respects_stride_and_clips() {
        // Stride 6: columns 4,5 of each row are outside the 4x4 block; DC = -100
        // gives residual (-96) >> 3 == -12, so 5 + (-12) clips to 0 inside.
        let stride = 6;
        let mut plane = [5u8; 4 * 6];
        let mut coeffs = [0i16; 16];
        coeffs[0] = -100;
        transform_one(&coeffs, &mut plane, 0, stride);
        for y in 0..4 {
            for x in 0..6 {
                let expected = if x < 4 { 0 } else { 5 };
                assert_eq!(plane[x + y * stride], expected, "x={x} y={y}");
            }
        }
    }

    // ---- 4x4 inverse DCT: AC cosine butterfly (new) ----------------------

    #[test]
    fn transform_one_single_ac_coeff1_is_horizontal_ripple() {
        // Single AC coefficient coeffs[1]=200 (a pure horizontal frequency).
        // Vertical pass: only column 1 nonzero (in0=200, in4=in8=in12=0):
        //   a=200, b=200, c=mul2(0)-mul1(0)=0, d=0
        //   -> tmp[4]=tmp[5]=tmp[6]=tmp[7]=200; all other tmp=0.
        // Horizontal pass (every row i identical: tmp[i]=0, tmp[4+i]=200,
        // tmp[8+i]=0, tmp[12+i]=0): dc=0+4=4, t8=0, a=b=4,
        //   c=mul2(200)-mul1(0)=108, d=mul1(200)+mul2(0)=261.
        //   STORE v = [a+d, b+c, b-c, a-d] = [265, 112, -104, -257]
        //   residual = v>>3 = [33, 14, -13, -33]  (arith: -104>>3=-13, -257>>3=-33)
        // Over flat dst=128 => [161, 142, 115, 95] on every row.
        let stride = 4;
        let mut plane = [128u8; 16];
        let mut coeffs = [0i16; 16];
        coeffs[1] = 200;
        transform_one(&coeffs, &mut plane, 0, stride);

        let expected_row = [161u8, 142, 115, 95];
        for y in 0..4 {
            for (x, &want) in expected_row.iter().enumerate() {
                assert_eq!(plane[x + y * stride], want, "x={x} y={y}");
            }
        }
    }

    #[test]
    fn transform_one_single_ac_coeff4_is_vertical_ripple() {
        // Single AC coefficient coeffs[4]=200 (a pure vertical frequency) — the
        // transpose of the coeffs[1] case; a vertical/horizontal-pass swap bug
        // would produce the coeffs[1] block instead of this one.
        // Vertical pass: only column 0 nonzero (in0=0, in4=200, in8=in12=0):
        //   a=0, b=0, c=mul2(200)-mul1(0)=108, d=mul1(200)+mul2(0)=261
        //   -> tmp[0]=261, tmp[1]=108, tmp[2]=-108, tmp[3]=-261; other tmp=0.
        // Horizontal pass: each row i reads tmp[i] with tmp[4+i]=tmp[8+i]=tmp[12+i]=0,
        //   so c=d=0 and all four stores equal dc=tmp[i]+4:
        //   row0: 261+4=265 -> 33 ; row1: 108+4=112 -> 14
        //   row2: -108+4=-104 -> -13 ; row3: -261+4=-257 -> -33
        // Over flat dst=128 => row0=161, row1=142, row2=115, row3=95 (constant
        // across each row).
        let stride = 4;
        let mut plane = [128u8; 16];
        let mut coeffs = [0i16; 16];
        coeffs[4] = 200;
        transform_one(&coeffs, &mut plane, 0, stride);

        let expected_rows = [161u8, 142, 115, 95];
        for (y, &want) in expected_rows.iter().enumerate() {
            for x in 0..4 {
                assert_eq!(plane[x + y * stride], want, "x={x} y={y}");
            }
        }
    }

    #[test]
    fn transform_one_mixed_dc_and_two_ac_coeffs() {
        // Mixed {DC=coeffs[0]=100, coeffs[1]=200, coeffs[4]=-160} on a padded
        // plane (stride 6) so this also exercises AC + stride, and the +4 rounder
        // / >>3 flooring / clip interplay on a fully non-constant 4x4 pattern.
        //
        // Vertical pass:
        //   col 0 (in0=100, in4=-160): a=100, b=100,
        //     c=mul2(-160)-mul1(0)=-87, d=mul1(-160)+mul2(0)=-210
        //     -> tmp[0]=a+d=-110, tmp[1]=b+c=13, tmp[2]=b-c=187, tmp[3]=a-d=310
        //   col 1 (in0=200): a=b=200, c=d=0 -> tmp[4..8]=200
        //   cols 2,3 -> 0.
        // Horizontal pass, row i uses tmp[i] and tmp[4+i]=200 (tmp[8+i]=tmp[12+i]=0):
        //   dc=tmp[i]+4, c=mul2(200)=108, d=mul1(200)=261, t8=0
        //   STORE v = [dc+d, dc+c, dc-c, dc-d]
        //   row0 dc=-110+4=-106: v=[155, 2, -214, -367]  -> >>3 [19, 0, -27, -46]
        //   row1 dc= 13+4= 17 : v=[278, 125, -91, -244] -> >>3 [34, 15, -12, -31]
        //   row2 dc=187+4=191 : v=[452, 299, 83, -70]   -> >>3 [56, 37, 10, -9]
        //   row3 dc=310+4=314 : v=[575, 422, 206, 53]   -> >>3 [71, 52, 25, 6]
        // Over flat dst=100 (all in [0,255], no clip), padding cols 4,5 untouched.
        let stride = 6;
        let mut plane = [100u8; 4 * 6];
        let mut coeffs = [0i16; 16];
        coeffs[0] = 100;
        coeffs[1] = 200;
        coeffs[4] = -160;
        transform_one(&coeffs, &mut plane, 0, stride);

        let block: [[u8; 4]; 4] = [
            [119, 100, 73, 54],
            [134, 115, 88, 69],
            [156, 137, 110, 91],
            [171, 152, 125, 106],
        ];
        for (y, row) in block.iter().enumerate() {
            for x in 0..6 {
                let want = if x < 4 { row[x] } else { 100 };
                assert_eq!(plane[x + y * stride], want, "x={x} y={y}");
            }
        }
    }
}
