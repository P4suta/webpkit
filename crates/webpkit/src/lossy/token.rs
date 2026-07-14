//! Residual (DCT coefficient) parsing (RFC 6386 §13, libwebp `ParseResiduals` /
//! `GetCoeffs` / `GetLargeValue`).
//!
//! For each macroblock this reads the second-order (Y2) DC block, the sixteen
//! luma blocks and the eight chroma blocks, threading the top/left non-zero
//! contexts and dequantizing every coefficient into the block's coefficient
//! buffer in natural (de-zig-zagged) order.
#![allow(
    clippy::cast_possible_truncation,
    reason = "dequantized coefficients are stored into i16 with the C int16_t \
              wrapping semantics of the reference decoder, and the small u32→usize \
              context indices are provably in range; the casts reproduce libwebp"
)]

use crate::lossy::bool_dec::BoolDecoder;
use crate::lossy::constants::{BANDS, CAT_3456, CoeffProbas, NUM_PROBAS, Prob, ZIGZAG};
use crate::lossy::decode::Frame;
use crate::lossy::idct::transform_wht;
use crate::lossy::work::work;

impl Frame {
    /// Parse and dequantize the residual coefficients of macroblock `mb_x`.
    pub(crate) fn parse_residuals(&mut self, br: &mut BoolDecoder<'_>, mb_x: usize) {
        let seg = usize::from(self.mb_data[mb_x].segment);
        let q = self.dqm[seg];
        self.mb_data[mb_x].coeffs = [0; 384];
        let is_i4x4 = self.mb_data[mb_x].is_i4x4;

        // Non-zero contexts: copy out, update, write back at the end.
        let mb_nz = self.mb_info[mb_x + 1].nz;
        let left_nz = self.mb_info[0].nz;
        let mut mb_nz_dc = self.mb_info[mb_x + 1].nz_dc;
        let mut left_nz_dc = self.mb_info[0].nz_dc;

        // Second-order (Y2) DC block, for 16×16-predicted luma only.
        let (first, ac_type) = if is_i4x4 {
            (0usize, 3usize)
        } else {
            let mut dc = [0i16; 16];
            let ctx = (mb_nz_dc + left_nz_dc) as usize;
            let nz = get_coeffs(br, &self.proba.bands, 1, ctx, q.y2, 0, &mut dc);
            let dc_nz = u32::from(nz > 0);
            mb_nz_dc = dc_nz;
            left_nz_dc = dc_nz;
            if nz > 1 {
                transform_wht(dc, &mut self.mb_data[mb_x].coeffs);
            } else {
                let dc0 = ((i32::from(dc[0]) + 3) >> 3) as i16;
                let mut i = 0;
                while i < 256 {
                    self.mb_data[mb_x].coeffs[i] = dc0;
                    i += 16;
                }
            }
            (1usize, 0usize)
        };

        let mut non_zero_y = 0u32;
        let mut tnz = mb_nz & 0x0f;
        let mut lnz = left_nz & 0x0f;
        let mut off = 0usize;
        for _y in 0..4 {
            let mut l = lnz & 1;
            let mut nz_coeffs = 0u32;
            for _x in 0..4 {
                let ctx = (l + (tnz & 1)) as usize;
                let block = &mut self.mb_data[mb_x].coeffs[off..off + 16];
                let nz = get_coeffs(br, &self.proba.bands, ac_type, ctx, q.y1, first, block);
                l = u32::from(nz > first);
                tnz = (tnz >> 1) | (l << 7);
                nz_coeffs = nz_code_bits(nz_coeffs, nz, self.mb_data[mb_x].coeffs[off] != 0);
                off += 16;
            }
            tnz >>= 4;
            lnz = (lnz >> 1) | (l << 7);
            non_zero_y = (non_zero_y << 8) | nz_coeffs;
        }
        let mut out_top_nz = tnz;
        let mut out_left_nz = lnz >> 4;

        let mut non_zero_uv = 0u32;
        for ch in (0..4).step_by(2) {
            let mut nz_coeffs = 0u32;
            let mut tnz = mb_nz >> (4 + ch);
            let mut lnz = left_nz >> (4 + ch);
            for _y in 0..2 {
                let mut l = lnz & 1;
                for _x in 0..2 {
                    let ctx = (l + (tnz & 1)) as usize;
                    let block = &mut self.mb_data[mb_x].coeffs[off..off + 16];
                    let nz = get_coeffs(br, &self.proba.bands, 2, ctx, q.uv, 0, block);
                    l = u32::from(nz > 0);
                    tnz = (tnz >> 1) | (l << 3);
                    nz_coeffs = nz_code_bits(nz_coeffs, nz, self.mb_data[mb_x].coeffs[off] != 0);
                    off += 16;
                }
                tnz >>= 2;
                lnz = (lnz >> 1) | (l << 5);
            }
            non_zero_uv |= nz_coeffs << (4 * ch);
            out_top_nz |= (tnz << 4) << ch;
            out_left_nz |= (lnz & 0xf0) << ch;
        }

        self.mb_info[mb_x + 1].nz = out_top_nz & 0xff;
        self.mb_info[0].nz = out_left_nz & 0xff;
        self.mb_info[mb_x + 1].nz_dc = mb_nz_dc;
        self.mb_info[0].nz_dc = left_nz_dc;
        self.mb_data[mb_x].non_zero_y = non_zero_y;
        self.mb_data[mb_x].non_zero_uv = non_zero_uv;
    }

    /// Clear a skipped macroblock's residual state, mirroring libwebp's
    /// `VP8DecodeMB` skip branch. A macroblock coded with `mb_skip_coeff = 1`
    /// carries no coefficient tokens, so its coefficients are all zero (it
    /// reconstructs to pure prediction) and it contributes nothing to the
    /// top/left non-zero contexts the following macroblocks read.
    pub(crate) fn skip_residuals(&mut self, mb_x: usize) {
        self.mb_data[mb_x].coeffs = [0; 384];
        self.mb_data[mb_x].non_zero_y = 0;
        self.mb_data[mb_x].non_zero_uv = 0;
        self.mb_info[mb_x + 1].nz = 0; // top (this column)
        self.mb_info[0].nz = 0; // left (running)
        if !self.mb_data[mb_x].is_i4x4 {
            // libwebp clears nz_dc only for 16×16 (Y2-carrying) macroblocks.
            self.mb_info[mb_x + 1].nz_dc = 0;
            self.mb_info[0].nz_dc = 0;
        }
    }
}

/// Fold one block's non-zero count into the packed per-block `non_zero` code:
/// `3` for >3 coefficients, `2` for >1, else the DC-non-zero bit.
fn nz_code_bits(nz_coeffs: u32, nz: usize, dc_nz: bool) -> u32 {
    let code = if nz > 3 {
        3
    } else if nz > 1 {
        2
    } else {
        u32::from(dc_nz)
    };
    (nz_coeffs << 2) | code
}

/// Decode one 4×4 block of coefficients into `out` (16 entries, natural order),
/// starting at coefficient position `n_start`, returning the index one past the
/// last non-zero coefficient. Port of libwebp `GetCoeffsFast`.
fn get_coeffs(
    br: &mut BoolDecoder<'_>,
    bands: &CoeffProbas,
    plane: usize,
    ctx: usize,
    dq: [i32; 2],
    n_start: usize,
    out: &mut [i16],
) -> usize {
    work!(CoeffToken);
    let plane_bands = &bands[plane];
    let mut n = n_start;
    let mut p: &[Prob; NUM_PROBAS] = &plane_bands[BANDS[n]][ctx];
    loop {
        if !br.read_bool(p[0]) {
            return n; // the previous coefficient was the last non-zero one
        }
        // A run of zero coefficients.
        while !br.read_bool(p[1]) {
            n += 1;
            if n == 16 {
                return 16;
            }
            p = &plane_bands[BANDS[n]][0];
        }
        // A non-zero coefficient at position `n`.
        let v = if br.read_bool(p[2]) {
            let large = get_large_value(br, *p);
            p = &plane_bands[BANDS[n + 1]][2];
            large
        } else {
            p = &plane_bands[BANDS[n + 1]][1];
            1
        };
        out[ZIGZAG[n]] = (br.apply_sign(v) * dq[usize::from(n > 0)]) as i16;
        n += 1;
        if n == 16 {
            return 16;
        }
    }
}

/// Decode a coefficient magnitude of 2 or more, including the DCT-value category
/// extra bits. Port of libwebp `GetLargeValue`.
fn get_large_value(br: &mut BoolDecoder<'_>, p: [Prob; NUM_PROBAS]) -> i32 {
    if !br.read_bool(p[3]) {
        return if br.read_bool(p[4]) {
            3 + i32::from(br.read_bool(p[5]))
        } else {
            2
        };
    }
    if !br.read_bool(p[6]) {
        return if br.read_bool(p[7]) {
            let hi = 2 * i32::from(br.read_bool(165));
            7 + hi + i32::from(br.read_bool(145))
        } else {
            5 + i32::from(br.read_bool(159))
        };
    }
    // Categories 3..6: two selector bits then a run of category extra bits.
    let bit1 = usize::from(br.read_bool(p[8]));
    let bit0 = usize::from(br.read_bool(p[9 + bit1]));
    let cat = 2 * bit1 + bit0;
    let mut v = 0i32;
    for &prob in CAT_3456[cat] {
        v = 2 * v + i32::from(br.read_bool(prob));
    }
    v + 3 + (8 << cat)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unreadable_literal,
        reason = "hand-derived packed non-zero codes and coefficient magnitudes \
                  read clearest as the exact values worked out in the comments"
    )]

    use super::{get_large_value, nz_code_bits};
    use crate::lossy::bool_dec::BoolDecoder;
    use crate::lossy::bool_enc::BoolEncoder;
    use crate::lossy::decode::Frame;

    // A distinct, non-constant probability array so a wrongly indexed p[i] in
    // GetLargeValue would encode/decode against the wrong probability and
    // desync. Only indices 3..=10 are consulted by get_large_value.
    const P: [u8; 11] = [10, 20, 30, 40, 50, 60, 70, 80, 90, 100, 110];

    // Drive get_large_value over a bitstream built by `build`, which must emit
    // the exact bit sequence (against the correct probabilities) for one branch.
    fn large_value(build: impl FnOnce(&mut BoolEncoder)) -> i32 {
        let mut enc = BoolEncoder::new();
        build(&mut enc);
        let bytes = enc.finish();
        let mut br = BoolDecoder::new(&bytes);
        get_large_value(&mut br, P)
    }

    // The four short branches of GetLargeValue (RFC 6386 §13.2).
    #[test]
    fn get_large_value_short_branches() {
        // v == 2: p[3]=0, p[4]=0.
        assert_eq!(
            large_value(|e| {
                e.put_bool(P[3], false);
                e.put_bool(P[4], false);
            }),
            2
        );
        // v == 4: p[3]=0, p[4]=1, p[5]=1  ->  3 + 1.
        assert_eq!(
            large_value(|e| {
                e.put_bool(P[3], false);
                e.put_bool(P[4], true);
                e.put_bool(P[5], true);
            }),
            4
        );
        // v == 6: p[3]=1, p[6]=0, p[7]=0, bit(159)=1  ->  5 + 1.
        assert_eq!(
            large_value(|e| {
                e.put_bool(P[3], true);
                e.put_bool(P[6], false);
                e.put_bool(P[7], false);
                e.put_bool(159, true);
            }),
            6
        );
        // v == 10: p[3]=1, p[6]=0, p[7]=1, hi=bit(165)=1, lo=bit(145)=1 -> 7+2+1.
        assert_eq!(
            large_value(|e| {
                e.put_bool(P[3], true);
                e.put_bool(P[6], false);
                e.put_bool(P[7], true);
                e.put_bool(165, true);
                e.put_bool(145, true);
            }),
            10
        );
    }

    // Category 3 (cat index 0): p[3]=1, p[6]=1, bit1=p[8]=0, bit0=p[9]=0.
    // Extra bits over kCat3 = [173,148,140] with pattern 1,0,1:
    //   v: 0 -> 1 -> 2 -> 5 ; then v += 3 + (8 << 0) = 11  =>  16.
    #[test]
    fn get_large_value_category3() {
        let v = large_value(|e| {
            e.put_bool(P[3], true);
            e.put_bool(P[6], true);
            e.put_bool(P[8], false); // bit1 = 0
            e.put_bool(P[9], false); // bit0 = 0  (cat 0)
            e.put_bool(173, true);
            e.put_bool(148, false);
            e.put_bool(140, true);
        });
        assert_eq!(v, 16);
    }

    // Category 6 (cat index 3): p[3]=1, p[6]=1, bit1=p[8]=1, bit0=p[9+1]=p[10]=1.
    // Extra bits over kCat6 (11 probs) with pattern 1,0,1,1,0,0,1,0,1,1,0:
    //   v accumulates to 1430 ; then v += 3 + (8 << 3) = 67  =>  1497.
    #[test]
    fn get_large_value_category6() {
        let cat6 = [254u8, 254, 243, 230, 196, 177, 153, 140, 133, 130, 129];
        let bits = [
            true, false, true, true, false, false, true, false, true, true, false,
        ];
        let v = large_value(|e| {
            e.put_bool(P[3], true);
            e.put_bool(P[6], true);
            e.put_bool(P[8], true); // bit1 = 1
            e.put_bool(P[10], true); // bit0 = 1  (cat 3)
            for (prob, bit) in cat6.into_iter().zip(bits) {
                e.put_bool(prob, bit);
            }
        });
        assert_eq!(v, 1497);
    }

    // NzCodeBits packing (RFC 6386 §13.3 helper): code = 3 if nz>3, 2 if nz>1,
    // else the DC-non-zero bit; result = (prev << 2) | code.
    #[test]
    fn nz_code_bits_packs_counts() {
        assert_eq!(nz_code_bits(0b01, 5, false), 0b0111); // >3  -> 3
        assert_eq!(nz_code_bits(0b10, 2, true), 0b1010); // >1  -> 2
        // nz == 3 pins the `nz > 3` boundary: it is NOT >3, so it must still
        // fold to the `>1` code 2 (a `>` -> `>=` slip would misclassify it as 3).
        assert_eq!(nz_code_bits(0, 3, false), 0b10); // ==3 -> 2, not 3
        assert_eq!(nz_code_bits(0, 4, false), 0b11); // ==4 -> 3 (first >3)
        assert_eq!(nz_code_bits(0, 1, true), 1); // ==1, dc  -> 1
        assert_eq!(nz_code_bits(0, 1, false), 0); // ==1, none -> 0
        assert_eq!(nz_code_bits(0xF, 4, false), 0b111111); // >3, shifted
    }

    // Full i4x4 macroblock residual parse (no Y2). Flat 128 token probabilities
    // let us author the exact token tree; the *signal* under test is the
    // coefficient values, their zig-zag placement, the DC-vs-AC dequant
    // selector, sign application, the p[2] large-value hop, and the packed
    // per-block non-zero code. Block 0 (plane 3, first=0), dq y1 = [7, 13]:
    //   n=0: small v=1, sign-, DC dq[0]=7  -> coeff[ZIGZAG[0]=0] = -7
    //   n=1: large v=2, sign+, AC dq[1]=13 -> coeff[ZIGZAG[1]=1] = +26
    //   n=2: one zero, then n=3 small v=1, sign+, AC dq[1]=13 -> coeff[ZIGZAG[3]=8]=13
    //   n=4: terminate (returns 4).
    // All 15 other luma blocks and 8 chroma blocks are empty (one 0-bit each).
    #[test]
    fn parse_residuals_i4x4_dequant_and_zigzag() {
        let mut frame = Frame::test_frame(1, 1);
        frame.proba.bands = [[[[128; 11]; 3]; 8]; 4];
        frame.dqm[0].y1 = [7, 13];
        frame.mb_data[0].is_i4x4 = true;

        let mut enc = BoolEncoder::new();
        // block 0, n=0
        enc.put_bool(128, true); // p[0] more
        enc.put_bool(128, true); // p[1] non-zero
        enc.put_bool(128, false); // p[2] small -> v = 1
        enc.put_flag(true); // sign -> negative
        // block 0, n=1
        enc.put_bool(128, true); // p[0] more
        enc.put_bool(128, true); // p[1] non-zero
        enc.put_bool(128, true); // p[2] large
        enc.put_bool(128, false); // p[3] = 0
        enc.put_bool(128, false); // p[4] = 0 -> v = 2
        enc.put_flag(false); // sign -> positive
        // block 0, n=2 (one zero) then n=3
        enc.put_bool(128, true); // p[0] more
        enc.put_bool(128, false); // p[1] -> zero coeff, advance to n=3
        enc.put_bool(128, true); // p[1] -> non-zero at n=3
        enc.put_bool(128, false); // p[2] small -> v = 1
        enc.put_flag(false); // sign -> positive
        // block 0, n=4 terminate
        enc.put_bool(128, false);
        // 15 empty luma + 8 empty chroma blocks (first=0): one 0-bit each.
        for _ in 0..(15 + 8) {
            enc.put_bool(128, false);
        }
        let bytes = enc.finish();
        let mut br = BoolDecoder::new(&bytes);
        frame.parse_residuals(&mut br, 0);

        let coeffs = &frame.mb_data[0].coeffs;
        for (i, &c) in coeffs.iter().enumerate() {
            let want = match i {
                0 => -7,
                1 => 26,
                8 => 13,
                _ => 0,
            };
            assert_eq!(c, want, "coeff[{i}]");
        }
        // Per-4x4 non-zero code: block 0 has 4 coeffs (code 3 = 0b11) in the top
        // 2 bits of the first row's byte (0xC0), the other three rows are empty:
        //   row0 = 0xC0 -> non_zero_y = 0xC0 << 24 = 0xC000_0000.
        assert_eq!(frame.mb_data[0].non_zero_y, 0xC000_0000);
        assert_eq!(frame.mb_data[0].non_zero_uv, 0);
    }

    // Y2 / inverse-WHT path (is_i4x4 = false, nz > 1). The Y2 block carries two
    // dequantized DC coefficients A = 1*10 = 10 (dq y2[0]) and B = 1*4 = 4
    // (dq y2[1]); TransformWHT scatters them into the 16 luma DC slots. With
    // only in[0]=A, in[1]=B non-zero the butterfly gives, per group of four
    // consecutive blocks: [(A+3+B)>>3, (A+3+B)>>3, (A+3-B)>>3, (A+3-B)>>3]
    //   = [17>>3, 17>>3, 9>>3, 9>>3] = [2, 2, 1, 1], repeated for all 4 groups.
    // All 16 luma AC parses (first=1) and 8 chroma blocks are empty, so only the
    // DC slots (index % 16 == 0, index < 256) are non-zero.
    #[test]
    fn parse_residuals_y2_wht_scatters_dc() {
        let mut frame = Frame::test_frame(1, 1);
        frame.proba.bands = [[[[128; 11]; 3]; 8]; 4];
        frame.dqm[0].y2 = [10, 4];

        let mut enc = BoolEncoder::new();
        // Y2 block, n=0: small v=1, sign+ -> dc[0] = 1*10 = 10.
        enc.put_bool(128, true);
        enc.put_bool(128, true);
        enc.put_bool(128, false);
        enc.put_flag(false);
        // Y2 block, n=1: small v=1, sign+ -> dc[1] = 1*4 = 4.
        enc.put_bool(128, true);
        enc.put_bool(128, true);
        enc.put_bool(128, false);
        enc.put_flag(false);
        // Y2 block, n=2 terminate (returns 2 -> nz>1 -> WHT).
        enc.put_bool(128, false);
        // 16 empty luma (first=1) + 8 empty chroma blocks.
        for _ in 0..(16 + 8) {
            enc.put_bool(128, false);
        }
        let bytes = enc.finish();
        let mut br = BoolDecoder::new(&bytes);
        frame.parse_residuals(&mut br, 0);

        let coeffs = &frame.mb_data[0].coeffs;
        for (i, &c) in coeffs.iter().enumerate() {
            let want = if i < 256 && i % 16 == 0 {
                let block = i / 16;
                if block % 4 < 2 { 2 } else { 1 }
            } else {
                0
            };
            assert_eq!(c, want, "coeff[{i}]");
        }
    }

    // Y2 with only the DC coefficient (nz == 1) takes the inlined simplified
    // transform: dc0 = (dc[0] + 3) >> 3 broadcast to every luma DC slot. With
    // dq y2[0] = 50 and v=1, dc[0] = 50 -> dc0 = (50 + 3) >> 3 = 6.
    #[test]
    fn parse_residuals_y2_dc_only_rounding() {
        let mut frame = Frame::test_frame(1, 1);
        frame.proba.bands = [[[[128; 11]; 3]; 8]; 4];
        frame.dqm[0].y2 = [50, 4];

        let mut enc = BoolEncoder::new();
        // Y2 block: single DC coeff v=1 -> dc[0] = 50.
        enc.put_bool(128, true);
        enc.put_bool(128, true);
        enc.put_bool(128, false);
        enc.put_flag(false);
        enc.put_bool(128, false); // n=1 terminate -> nz = 1
        for _ in 0..(16 + 8) {
            enc.put_bool(128, false);
        }
        let bytes = enc.finish();
        let mut br = BoolDecoder::new(&bytes);
        frame.parse_residuals(&mut br, 0);

        let coeffs = &frame.mb_data[0].coeffs;
        for (i, &c) in coeffs.iter().enumerate() {
            let want = if i < 256 && i % 16 == 0 { 6 } else { 0 };
            assert_eq!(c, want, "coeff[{i}]");
        }
    }

    // Chroma residual parse: the packed per-block non-zero code of the U-group
    // (ch = 0) and V-group (ch = 2) must land in *disjoint* bit fields of
    // `non_zero_uv` — the U bits at shift `4 * 0 = 0` and the V bits at shift
    // `4 * 2 = 8`. A `*` -> `+` slip on that shift would place U at bit 4 and V
    // at bit 6, overlapping and corrupting the value. i4x4 macroblock (no Y2),
    // all 16 luma blocks empty; each chroma group's first (0,0) block carries a
    // single DC coefficient (nz = 1, dc-non-zero -> code 1), the other three
    // are empty (code 0), so each group's packed code is 1 << 6 = 0x40.
    //   non_zero_uv = (0x40 << 0) | (0x40 << 8) = 0x4040.
    // The DC also exercises the chroma dequant selector: dq uv[0] = 11.
    #[test]
    fn parse_residuals_chroma_nz_code_disjoint_shift() {
        let mut frame = Frame::test_frame(1, 1);
        frame.proba.bands = [[[[128; 11]; 3]; 8]; 4];
        frame.dqm[0].uv = [11, 17];
        frame.mb_data[0].is_i4x4 = true;

        let mut enc = BoolEncoder::new();
        // 16 empty luma blocks (i4x4, first = 0): one 0-bit each.
        for _ in 0..16 {
            enc.put_bool(128, false);
        }
        // A chroma block carrying exactly one DC coefficient (v = 1, sign +).
        let single_dc = |enc: &mut BoolEncoder| {
            enc.put_bool(128, true); // p[0] more
            enc.put_bool(128, true); // p[1] non-zero
            enc.put_bool(128, false); // p[2] small -> v = 1
            enc.put_flag(false); // sign -> positive
            enc.put_bool(128, false); // n = 1 terminate -> nz = 1
        };
        // U group: block (0,0) = single DC, blocks (0,1)/(1,0)/(1,1) empty.
        single_dc(&mut enc);
        for _ in 0..3 {
            enc.put_bool(128, false);
        }
        // V group: same shape.
        single_dc(&mut enc);
        for _ in 0..3 {
            enc.put_bool(128, false);
        }
        let bytes = enc.finish();
        let mut br = BoolDecoder::new(&bytes);
        frame.parse_residuals(&mut br, 0);

        // U DC is the first chroma block (luma occupies coeffs[0..256]).
        assert_eq!(frame.mb_data[0].coeffs[256], 11, "U DC");
        // V DC starts one chroma plane later (4 blocks * 16 = 64 slots on).
        assert_eq!(frame.mb_data[0].coeffs[320], 11, "V DC");
        assert_eq!(frame.mb_data[0].non_zero_y, 0);
        assert_eq!(frame.mb_data[0].non_zero_uv, 0x4040);
    }
}
