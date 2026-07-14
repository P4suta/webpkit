//! DCT-coefficient token emission and intra-mode emission (the encoder
//! counterparts of [`crate::lossy::token`] and [`crate::lossy::mb`]).
//!
//! [`emit_mb_residuals`] mirrors `token::parse_residuals` in reverse: it walks a
//! macroblock's second-order (Y2), luma and chroma blocks in the same order,
//! threads the identical top/left non-zero contexts, and for each block emits the
//! token tree ([`put_coeffs`], the inverse of `token::get_coeffs`). Because the
//! encoder holds the quantized levels, the last non-zero position is known, so
//! the context bookkeeping is a byte-for-byte copy of the decoder's — which is
//! what keeps the token partition in sync.
//!
//! [`put_ymode16`] / [`put_uvmode`] emit the intra prediction modes into the
//! control partition (inverse of `mb::read_ymode16` / `read_uvmode`).
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    reason = "the small 0..=16 coefficient positions and category selectors cast \
              between usize and i32; every value is provably in range"
)]

use crate::lossy::bool_enc::BoolEncoder;
use crate::lossy::constants::{
    B_DC_PRED, B_HD_PRED, B_HE_PRED, B_LD_PRED, B_RD_PRED, B_TM_PRED, B_VE_PRED, B_VL_PRED,
    B_VR_PRED, BANDS, CAT_3456, CoeffProbas, CoeffStats, H_PRED, NUM_BMODES, NUM_PROBAS, Prob,
    TM_PRED, V_PRED,
};
use crate::lossy::prelude::*;

/// One 4×4 block's quantized levels (zig-zag order) and last non-zero index.
#[derive(Clone, Copy)]
pub(crate) struct Block {
    /// Signed levels in zig-zag order.
    pub(crate) levels: [i16; 16],
    /// Last non-zero zig-zag index, or `first - 1` if empty. `last + 1` is the
    /// decoder's `GetCoeffs` return value (`nz`).
    pub(crate) last: i32,
}

impl Default for Block {
    fn default() -> Self {
        Self {
            levels: [0; 16],
            last: -1,
        }
    }
}

/// All the token blocks of one macroblock, ready to emit.
pub(crate) struct MbTokens {
    /// Whether the luma is coded as 4×4 blocks (else 16×16 with a Y2 block).
    pub(crate) is_i4x4: bool,
    /// The second-order (Y2) block; meaningful only when `!is_i4x4`.
    pub(crate) y2: Block,
    /// The sixteen luma blocks in raster order.
    pub(crate) luma: [Block; 16],
    /// The eight chroma blocks: U sub-blocks `0..4`, V sub-blocks `4..8`.
    pub(crate) chroma: [Block; 8],
}

/// The rolling top/left non-zero context threaded across macroblocks — the
/// encoder mirror of the decoder's `mb_info` (`token::parse_residuals`).
pub(crate) struct NzContext {
    /// Per-column top non-zero word (4 luma + 4 chroma bits).
    top: Vec<u32>,
    /// Per-column top second-order (Y2) non-zero flag.
    top_dc: Vec<u32>,
    /// Running left non-zero word (reset each macroblock row).
    left: u32,
    /// Running left second-order non-zero flag.
    left_dc: u32,
}

impl NzContext {
    /// A cleared context for a `mb_w`-wide frame.
    #[must_use]
    pub(crate) fn new(mb_w: usize) -> Self {
        Self {
            top: vec![0; mb_w],
            top_dc: vec![0; mb_w],
            left: 0,
            left_dc: 0,
        }
    }

    /// Reset the left context before the next macroblock row.
    pub(crate) const fn init_scanline(&mut self) {
        self.left = 0;
        self.left_dc = 0;
    }

    /// Clear the non-zero context of a skipped 16×16 macroblock column, the
    /// encoder mirror of the decoder's `Frame::skip_residuals`: a skipped
    /// macroblock emits no tokens and zeroes both its top (per-column) and left
    /// (running) non-zero words, including the second-order (Y2) flags.
    pub(crate) fn skip_mb(&mut self, mb_x: usize) {
        self.top[mb_x] = 0;
        self.top_dc[mb_x] = 0;
        self.left = 0;
        self.left_dc = 0;
    }
}

/// Emit one macroblock's residual tokens into the token partition `enc`, threading
/// `ctx`. Exact reverse of `token::parse_residuals`.
pub(crate) fn emit_mb_residuals(
    enc: &mut BoolEncoder,
    bands: &CoeffProbas,
    mb: &MbTokens,
    ctx: &mut NzContext,
    mb_x: usize,
) {
    let mb_nz = ctx.top[mb_x];
    let left_nz = ctx.left;
    let mut mb_nz_dc = ctx.top_dc[mb_x];
    let mut left_nz_dc = ctx.left_dc;

    // Second-order (Y2) DC block, for 16×16-predicted luma only.
    let (first, ac_type) = if mb.is_i4x4 {
        (0usize, 3usize)
    } else {
        let c = (mb_nz_dc + left_nz_dc) as usize;
        let nz = put_coeffs(enc, bands, 1, c, mb.y2.levels, 0, mb.y2.last);
        let dc_nz = u32::from(nz > 0);
        mb_nz_dc = dc_nz;
        left_nz_dc = dc_nz;
        (1usize, 0usize)
    };
    let first_i32 = first as i32;

    let mut tnz = mb_nz & 0x0f;
    let mut lnz = left_nz & 0x0f;
    let mut off = 0usize;
    for _y in 0..4 {
        let mut l = lnz & 1;
        for _x in 0..4 {
            let c = (l + (tnz & 1)) as usize;
            let nz = put_coeffs(
                enc,
                bands,
                ac_type,
                c,
                mb.luma[off].levels,
                first,
                mb.luma[off].last,
            );
            l = u32::from(nz > first_i32);
            tnz = (tnz >> 1) | (l << 7);
            off += 1;
        }
        tnz >>= 4;
        lnz = (lnz >> 1) | (l << 7);
    }
    let mut out_top_nz = tnz;
    let mut out_left_nz = lnz >> 4;

    for ch in (0..4).step_by(2) {
        let mut tnz = mb_nz >> (4 + ch);
        let mut lnz = left_nz >> (4 + ch);
        for y in 0..2 {
            let mut l = lnz & 1;
            for x in 0..2 {
                let c = (l + (tnz & 1)) as usize;
                let idx = ch * 2 + y * 2 + x;
                let nz = put_coeffs(
                    enc,
                    bands,
                    2,
                    c,
                    mb.chroma[idx].levels,
                    0,
                    mb.chroma[idx].last,
                );
                l = u32::from(nz > 0);
                tnz = (tnz >> 1) | (l << 3);
            }
            tnz >>= 2;
            lnz = (lnz >> 1) | (l << 5);
        }
        out_top_nz |= (tnz << 4) << ch;
        out_left_nz |= (lnz & 0xf0) << ch;
    }

    ctx.top[mb_x] = out_top_nz & 0xff;
    ctx.left = out_left_nz & 0xff;
    ctx.top_dc[mb_x] = mb_nz_dc;
    ctx.left_dc = left_nz_dc;
}

/// Emit one 4×4 block's coefficient tokens (inverse of `token::get_coeffs`),
/// returning `nz = last + 1` (the decoder's `GetCoeffs` return value) so the
/// caller can update the non-zero context. `levels` are in zig-zag order; `first`
/// is the starting position (1 for a 16×16-luma AC block, else 0).
fn put_coeffs(
    enc: &mut BoolEncoder,
    bands: &CoeffProbas,
    plane: usize,
    ctx: usize,
    levels: [i16; 16],
    first: usize,
    last: i32,
) -> i32 {
    let plane_bands = &bands[plane];
    let mut n = first;
    let mut p: &[Prob; NUM_PROBAS] = &plane_bands[BANDS[n]][ctx];
    loop {
        if n as i32 > last {
            enc.put_bool(p[0], false); // EOB (also the all-empty block: one 0-bit)
            return last + 1;
        }
        enc.put_bool(p[0], true);
        while levels[n] == 0 {
            enc.put_bool(p[1], false);
            n += 1;
            p = &plane_bands[BANDS[n]][0];
        }
        enc.put_bool(p[1], true);
        let v = i32::from(levels[n]).abs();
        let p_next = if v == 1 {
            enc.put_bool(p[2], false);
            &plane_bands[BANDS[n + 1]][1]
        } else {
            enc.put_bool(p[2], true);
            put_large_value(enc, *p, v);
            &plane_bands[BANDS[n + 1]][2]
        };
        enc.put_flag(levels[n] < 0);
        n += 1;
        if n == 16 {
            return 16;
        }
        p = p_next;
    }
}

/// Emit a coefficient magnitude of 2 or more (inverse of `token::get_large_value`),
/// using node probabilities `p[3..=10]`, the hardcoded literals 159/165/145, and
/// the `CAT_3456` category extra bits (MSB first).
fn put_large_value(enc: &mut BoolEncoder, p: [Prob; NUM_PROBAS], v: i32) {
    if v <= 4 {
        enc.put_bool(p[3], false);
        if v == 2 {
            enc.put_bool(p[4], false);
        } else {
            enc.put_bool(p[4], true);
            enc.put_bool(p[5], v == 4); // v == 3 -> 0, v == 4 -> 1
        }
    } else if v <= 10 {
        enc.put_bool(p[3], true);
        enc.put_bool(p[6], false);
        if v <= 6 {
            enc.put_bool(p[7], false);
            enc.put_bool(159, v == 6); // v == 5 -> 0, v == 6 -> 1
        } else {
            enc.put_bool(p[7], true);
            let hi = v - 7; // 0..=3
            enc.put_bool(165, (hi >> 1) & 1 == 1);
            enc.put_bool(145, hi & 1 == 1);
        }
    } else {
        enc.put_bool(p[3], true);
        enc.put_bool(p[6], true);
        let cat = match v {
            11..=18 => 0usize,
            19..=34 => 1,
            35..=66 => 2,
            _ => 3,
        };
        enc.put_bool(p[8], (cat >> 1) & 1 == 1);
        enc.put_bool(p[9 + (cat >> 1)], cat & 1 == 1);
        let extra = v - 3 - (8 << cat);
        let probs = CAT_3456[cat];
        let nbits = probs.len();
        for (i, &prob) in probs.iter().enumerate() {
            enc.put_bool(prob, (extra >> (nbits - 1 - i)) & 1 == 1);
        }
    }
}

/// Tally one macroblock's residual tokens into `stats`, threading `ctx` exactly
/// as [`emit_mb_residuals`] does. This is the statistical mirror of emission: it
/// visits the Y2, luma and chroma blocks in the identical order, with the same
/// non-zero context bookkeeping, but instead of writing bits it records, per
/// main-tree node, how often each boolean would code a zero or a one. The
/// encoder feeds the accumulated [`CoeffStats`] to `prob_opt::optimize_probas`.
pub(crate) fn count_mb_residuals(
    stats: &mut CoeffStats,
    mb: &MbTokens,
    ctx: &mut NzContext,
    mb_x: usize,
) {
    let mb_nz = ctx.top[mb_x];
    let left_nz = ctx.left;
    let mut mb_nz_dc = ctx.top_dc[mb_x];
    let mut left_nz_dc = ctx.left_dc;

    // Second-order (Y2) DC block, for 16×16-predicted luma only.
    let (first, ac_type) = if mb.is_i4x4 {
        (0usize, 3usize)
    } else {
        let c = (mb_nz_dc + left_nz_dc) as usize;
        let nz = count_coeffs(stats, 1, c, mb.y2.levels, 0, mb.y2.last);
        let dc_nz = u32::from(nz > 0);
        mb_nz_dc = dc_nz;
        left_nz_dc = dc_nz;
        (1usize, 0usize)
    };
    let first_i32 = first as i32;

    let mut tnz = mb_nz & 0x0f;
    let mut lnz = left_nz & 0x0f;
    let mut off = 0usize;
    for _y in 0..4 {
        let mut l = lnz & 1;
        for _x in 0..4 {
            let c = (l + (tnz & 1)) as usize;
            let nz = count_coeffs(
                stats,
                ac_type,
                c,
                mb.luma[off].levels,
                first,
                mb.luma[off].last,
            );
            l = u32::from(nz > first_i32);
            tnz = (tnz >> 1) | (l << 7);
            off += 1;
        }
        tnz >>= 4;
        lnz = (lnz >> 1) | (l << 7);
    }
    let mut out_top_nz = tnz;
    let mut out_left_nz = lnz >> 4;

    for ch in (0..4).step_by(2) {
        let mut tnz = mb_nz >> (4 + ch);
        let mut lnz = left_nz >> (4 + ch);
        for y in 0..2 {
            let mut l = lnz & 1;
            for x in 0..2 {
                let c = (l + (tnz & 1)) as usize;
                let idx = ch * 2 + y * 2 + x;
                let nz = count_coeffs(stats, 2, c, mb.chroma[idx].levels, 0, mb.chroma[idx].last);
                l = u32::from(nz > 0);
                tnz = (tnz >> 1) | (l << 3);
            }
            tnz >>= 2;
            lnz = (lnz >> 1) | (l << 5);
        }
        out_top_nz |= (tnz << 4) << ch;
        out_left_nz |= (lnz & 0xf0) << ch;
    }

    ctx.top[mb_x] = out_top_nz & 0xff;
    ctx.left = out_left_nz & 0xff;
    ctx.top_dc[mb_x] = mb_nz_dc;
    ctx.left_dc = left_nz_dc;
}

/// Record one 4×4 block's main-tree node decisions into `stats`, returning
/// `nz = last + 1` (the decoder's `GetCoeffs` value) so the caller threads the
/// non-zero context identically to [`put_coeffs`]. Only nodes 0 (EOB / more),
/// 1 (zero-run) and 2 (one-vs-large) are counted — the large-value sub-nodes and
/// sign bit are entropy-neutral for the probability search — walking `(band, ctx)`
/// in lockstep with `put_coeffs`.
fn count_coeffs(
    stats: &mut CoeffStats,
    plane: usize,
    ctx0: usize,
    levels: [i16; 16],
    first: usize,
    last: i32,
) -> i32 {
    let mut n = first;
    let mut band = BANDS[n];
    let mut ctx = ctx0;
    loop {
        if n as i32 > last {
            stats[plane][band][ctx][0][0] += 1; // node 0, bit 0: EOB / empty block
            return last + 1;
        }
        stats[plane][band][ctx][0][1] += 1; // node 0, bit 1: more coefficients
        while levels[n] == 0 {
            stats[plane][band][ctx][1][0] += 1; // node 1, bit 0: another zero
            n += 1;
            band = BANDS[n];
            ctx = 0;
        }
        stats[plane][band][ctx][1][1] += 1; // node 1, bit 1: this position is non-zero
        if i32::from(levels[n]).abs() == 1 {
            stats[plane][band][ctx][2][0] += 1; // node 2, bit 0: magnitude 1
            n += 1;
            if n == 16 {
                return 16;
            }
            band = BANDS[n];
            ctx = 1;
        } else {
            stats[plane][band][ctx][2][1] += 1; // node 2, bit 1: magnitude >= 2
            n += 1;
            if n == 16 {
                return 16;
            }
            band = BANDS[n];
            ctx = 2;
        }
    }
}

/// Emit one macroblock's segment id (`0..=3`) into the control partition — the
/// exact inverse of the hardcoded 3-node segment-id tree `mb::parse_intra_mode`
/// reads (`if read_bool(p[0]) { 2 + read_bool(p[2]) } else { read_bool(p[1]) }`).
/// Emitted first for every macroblock, before the skip flag, and only when the
/// frame codes an updated segment map (`use_segment && update_map`).
pub(crate) fn put_segment_id(enc: &mut BoolEncoder, probs: [Prob; 3], seg: u8) {
    if seg >= 2 {
        enc.put_bool(probs[0], true);
        enc.put_bool(probs[2], seg == 3);
    } else {
        enc.put_bool(probs[0], false);
        enc.put_bool(probs[1], seg == 1);
    }
}

/// Emit the luma-type selector into the control partition (inverse of the
/// `is_i4x4 = !read_bool(145)` decode).
pub(crate) fn put_is_i4x4(enc: &mut BoolEncoder, is_i4x4: bool) {
    enc.put_bool(145, !is_i4x4);
}

/// Emit a 16×16 luma prediction mode (inverse of `mb::read_ymode16`, tree probs
/// 156/128/163).
pub(crate) fn put_ymode16(enc: &mut BoolEncoder, mode: u8) {
    match mode {
        H_PRED => {
            enc.put_bool(156, true);
            enc.put_bool(128, false);
        },
        TM_PRED => {
            enc.put_bool(156, true);
            enc.put_bool(128, true);
        },
        V_PRED => {
            enc.put_bool(156, false);
            enc.put_bool(163, true);
        },
        _ => {
            // DC_PRED (and any unexpected value defaults to the DC branch).
            enc.put_bool(156, false);
            enc.put_bool(163, false);
        },
    }
}

/// Emit a chroma prediction mode (inverse of `mb::read_uvmode`, tree probs
/// 142/114/183).
pub(crate) fn put_uvmode(enc: &mut BoolEncoder, mode: u8) {
    match mode {
        V_PRED => {
            enc.put_bool(142, true);
            enc.put_bool(114, false);
        },
        TM_PRED => {
            enc.put_bool(142, true);
            enc.put_bool(114, true);
            enc.put_bool(183, true);
        },
        H_PRED => {
            enc.put_bool(142, true);
            enc.put_bool(114, true);
            enc.put_bool(183, false);
        },
        _ => {
            // DC_PRED (and any unexpected value defaults to the DC branch).
            enc.put_bool(142, false);
        },
    }
}

/// Emit one intra 4×4 (B) sub-block mode into the control partition — the exact
/// inverse of `mb::read_bmode`, walking the same hardcoded decision tree over the
/// nine `kBModesProba` node probabilities `prob` (indexed by the top/left
/// neighbor modes). Each `put_bool(prob[k], bit)` mirrors a `read_bool(prob[k])`.
pub(crate) fn put_bmode(enc: &mut BoolEncoder, prob: [Prob; NUM_BMODES - 1], mode: u8) {
    // Node 0: DC vs the rest.
    if mode == B_DC_PRED {
        enc.put_bool(prob[0], false);
        return;
    }
    enc.put_bool(prob[0], true);
    // Node 1: TM vs the rest.
    if mode == B_TM_PRED {
        enc.put_bool(prob[1], false);
        return;
    }
    enc.put_bool(prob[1], true);
    // Node 2: VE vs the rest.
    if mode == B_VE_PRED {
        enc.put_bool(prob[2], false);
        return;
    }
    enc.put_bool(prob[2], true);
    // Node 3 splits the tree into the {HE,RD,VR} and {LD,VL,HD,HU} halves.
    match mode {
        B_HE_PRED => {
            enc.put_bool(prob[3], false);
            enc.put_bool(prob[4], false);
        },
        B_RD_PRED => {
            enc.put_bool(prob[3], false);
            enc.put_bool(prob[4], true);
            enc.put_bool(prob[5], false);
        },
        B_VR_PRED => {
            enc.put_bool(prob[3], false);
            enc.put_bool(prob[4], true);
            enc.put_bool(prob[5], true);
        },
        B_LD_PRED => {
            enc.put_bool(prob[3], true);
            enc.put_bool(prob[6], false);
        },
        B_VL_PRED => {
            enc.put_bool(prob[3], true);
            enc.put_bool(prob[6], true);
            enc.put_bool(prob[7], false);
        },
        B_HD_PRED => {
            enc.put_bool(prob[3], true);
            enc.put_bool(prob[6], true);
            enc.put_bool(prob[7], true);
            enc.put_bool(prob[8], false);
        },
        _ => {
            // B_HU_PRED (and any unexpected value defaults to the last leaf).
            enc.put_bool(prob[3], true);
            enc.put_bool(prob[6], true);
            enc.put_bool(prob[7], true);
            enc.put_bool(prob[8], true);
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Block, MbTokens, NzContext, count_mb_residuals, emit_mb_residuals, put_bmode, put_is_i4x4,
        put_segment_id, put_uvmode, put_ymode16,
    };
    use crate::lossy::bool_dec::BoolDecoder;
    use crate::lossy::bool_enc::BoolEncoder;
    use crate::lossy::constants::{
        B_DC_PRED, B_HD_PRED, B_HE_PRED, B_HU_PRED, B_LD_PRED, B_RD_PRED, B_TM_PRED, B_VE_PRED,
        B_VL_PRED, B_VR_PRED, BMODES_PROBA, COEFFS_PROBA_0, CoeffStats, DC_PRED, H_PRED, TM_PRED,
        V_PRED, ZIGZAG,
    };
    use crate::lossy::decode::Frame;
    use crate::lossy::idct::transform_wht;
    use crate::lossy::mb::read_bmode;

    /// Build a block from levels given in NATURAL order (converted to zig-zag) and
    /// compute `last`.
    fn block_from_natural(natural: [i16; 16], first: usize) -> Block {
        let mut levels = [0i16; 16];
        let mut last = first as i32 - 1;
        for n in first..16 {
            let v = natural[ZIGZAG[n]];
            levels[n] = v;
            if v != 0 {
                last = n as i32;
            }
        }
        Block { levels, last }
    }

    /// A decode `Frame` primed with the given per-plane dequant steps and the
    /// default coefficient probabilities, ready for `parse_residuals`.
    fn primed_frame(y1: [i32; 2], y2: [i32; 2], uv: [i32; 2], is_i4x4: bool) -> Frame {
        let mut frame = Frame::test_frame(1, 1);
        frame.proba.bands = COEFFS_PROBA_0;
        frame.dqm[0].y1 = y1;
        frame.dqm[0].y2 = y2;
        frame.dqm[0].uv = uv;
        frame.mb_data[0].is_i4x4 = is_i4x4;
        frame
    }

    #[test]
    fn i4x4_residuals_round_trip_through_the_decoder() {
        // The i4x4 path (no Y2): each luma/chroma block dequantizes directly to
        // level*dq in natural order, so the decoded coeffs equal our expected
        // reconstruction. Distinct levels across small/large/sign/zero-run cases
        // exercise put_coeffs, put_large_value and the non-zero context threading.
        let y1 = [7, 13];
        let uv = [5, 9];
        let mut luma = [Block::default(); 16];
        // Block 0: DC=-1, AC[1]=+2 (large), zero at 2, AC[3]=+1.
        luma[0] = block_from_natural([-1, 2, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 0);
        // Block 5: a single large AC value 17 (category 3) at natural pos 1.
        luma[5] = block_from_natural([0, 17, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 0);
        let mut chroma = [Block::default(); 8];
        // U block 0 (chroma idx 0): DC=3. V block 2 (chroma idx 6): DC=-2.
        chroma[0] = block_from_natural([3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 0);
        chroma[6] = block_from_natural([-2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 0);
        let mb = MbTokens {
            is_i4x4: true,
            y2: Block::default(),
            luma,
            chroma,
        };

        let mut enc = BoolEncoder::new();
        let mut ctx = NzContext::new(1);
        emit_mb_residuals(&mut enc, &COEFFS_PROBA_0, &mb, &mut ctx, 0);
        let bytes = enc.finish();

        let mut frame = primed_frame(y1, y1, uv, true);
        let mut br = BoolDecoder::new(&bytes);
        frame.parse_residuals(&mut br, 0);

        let coeffs = &frame.mb_data[0].coeffs;
        // Block 0 luma coeffs = level*dq: coeff[0]=-1*7=-7, coeff[1]=2*13=26,
        // coeff[3]=1*13=13.
        assert_eq!(coeffs[0], -7);
        assert_eq!(coeffs[1], 26);
        assert_eq!(coeffs[3], 13);
        // Block 5 luma (offset 5*16=80): coeff[1] = 17*13 = 221.
        assert_eq!(coeffs[80 + 1], 221);
        // Chroma U block 0 (offset 16*16=256): coeff[0] = 3*5 = 15.
        assert_eq!(coeffs[256], 15);
        // Chroma V block 2 (offset (20+2)*16=352): coeff[0] = -2*5 = -10.
        assert_eq!(coeffs[352], -10);
    }

    #[test]
    fn y2_dc_only_round_trips_to_broadcast_dc() {
        // A 16×16 MB with a single Y2 DC and empty luma/chroma. The decoder takes
        // the nz==1 broadcast path: every luma DC slot = (dc[0] + 3) >> 3, where
        // dc[0] = level * y2_dc. level 1, y2_dc 50 -> (50 + 3) >> 3 = 6.
        let mut mb = MbTokens {
            is_i4x4: false,
            y2: block_from_natural([1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 0),
            luma: [Block::default(); 16],
            chroma: [Block::default(); 8],
        };
        // Luma blocks start at first=1 (16×16 AC), and are empty here.
        for b in &mut mb.luma {
            b.last = 0; // empty AC block: last = first - 1 = 0
        }

        let mut enc = BoolEncoder::new();
        let mut ctx = NzContext::new(1);
        emit_mb_residuals(&mut enc, &COEFFS_PROBA_0, &mb, &mut ctx, 0);
        let bytes = enc.finish();

        let mut frame = primed_frame([10, 20], [50, 20], [10, 20], false);
        let mut br = BoolDecoder::new(&bytes);
        frame.parse_residuals(&mut br, 0);

        let coeffs = &frame.mb_data[0].coeffs;
        for (i, &c) in coeffs.iter().enumerate() {
            let want = if i < 256 && i % 16 == 0 { 6 } else { 0 };
            assert_eq!(c, want, "coeff[{i}]");
        }
    }

    #[test]
    fn y2_wht_scatter_round_trips() {
        // A 16×16 MB with a multi-coefficient Y2 (nz > 1) so the decoder runs the
        // inverse WHT. The decoded luma DC slots must equal transform_wht of the
        // dequantized Y2 — computed here independently as the expected value.
        let y2_dq = [10, 4];
        let y2 = block_from_natural([1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 0);
        let mut luma = [Block::default(); 16];
        for b in &mut luma {
            b.last = 0; // empty AC
        }
        let mb = MbTokens {
            is_i4x4: false,
            y2,
            luma,
            chroma: [Block::default(); 8],
        };

        let mut enc = BoolEncoder::new();
        let mut ctx = NzContext::new(1);
        emit_mb_residuals(&mut enc, &COEFFS_PROBA_0, &mb, &mut ctx, 0);
        let bytes = enc.finish();

        let mut frame = primed_frame([10, 20], y2_dq, [10, 20], false);
        let mut br = BoolDecoder::new(&bytes);
        frame.parse_residuals(&mut br, 0);

        // Expected: dequantized Y2 = [1*10, 1*4, 0, ...] then inverse WHT.
        let mut expected = [0i16; 384];
        let mut dc = [0i16; 16];
        dc[0] = 10;
        dc[1] = 4;
        transform_wht(dc, &mut expected);
        assert_eq!(frame.mb_data[0].coeffs, expected);
    }

    #[test]
    fn intra_modes_round_trip_through_parse_intra_mode() {
        // Emit the 16×16 luma-type flag + a ymode + uvmode for each of the four
        // whole-block modes, and confirm the decoder's parse path recovers them.
        for &(ymode, uvmode) in &[
            (DC_PRED, DC_PRED),
            (V_PRED, H_PRED),
            (H_PRED, V_PRED),
            (TM_PRED, TM_PRED),
        ] {
            let mut enc = BoolEncoder::new();
            put_is_i4x4(&mut enc, false);
            put_ymode16(&mut enc, ymode);
            put_uvmode(&mut enc, uvmode);
            let bytes = enc.finish();

            let mut frame = Frame::test_frame(1, 1);
            let mut br = BoolDecoder::new(&bytes);
            frame.parse_intra_mode_row(&mut br);
            assert!(!frame.mb_data[0].is_i4x4, "16x16 luma-type");
            assert_eq!(frame.mb_data[0].imodes[0], ymode, "ymode");
            assert_eq!(frame.mb_data[0].uvmode, uvmode, "uvmode");
        }
    }

    #[test]
    fn segment_ids_round_trip_through_the_decoder_tree() {
        // put_segment_id is the exact inverse of the 3-node segment-id tree
        // mb::parse_intra_mode reads: every id 0..=3, emitted against skewed tree
        // probs, must decode back to itself. A swapped branch or a wrong prob index
        // would misdecode one of the four ids.
        let probs = [200u8, 60, 140];
        for seg in 0..4u8 {
            let mut enc = BoolEncoder::new();
            put_segment_id(&mut enc, probs, seg);
            let bytes = enc.finish();
            let mut br = BoolDecoder::new(&bytes);
            let got = if br.read_bool(probs[0]) {
                2 + u8::from(br.read_bool(probs[2]))
            } else {
                u8::from(br.read_bool(probs[1]))
            };
            assert_eq!(got, seg, "segment id {seg}");
        }
    }

    #[test]
    fn bmodes_round_trip_through_read_bmode() {
        // put_bmode is the exact inverse of mb::read_bmode: every one of the ten B
        // modes, emitted with put_bmode, must decode back to itself. Sweep a few
        // distinct kBModesProba rows so the round-trip is exercised against
        // non-trivial node probabilities (a wrong tree branch would misdecode).
        let modes = [
            B_DC_PRED, B_TM_PRED, B_VE_PRED, B_HE_PRED, B_RD_PRED, B_VR_PRED, B_LD_PRED, B_VL_PRED,
            B_HD_PRED, B_HU_PRED,
        ];
        for &(top, left) in &[(0usize, 0usize), (2, 3), (9, 5), (4, 8)] {
            let prob = BMODES_PROBA[top][left];
            for &mode in &modes {
                let mut enc = BoolEncoder::new();
                put_bmode(&mut enc, prob, mode);
                let bytes = enc.finish();
                let mut br = BoolDecoder::new(&bytes);
                let got = read_bmode(&mut br, prob);
                assert_eq!(got, mode, "mode {mode} at prob[{top}][{left}]");
            }
        }
    }

    /// Independent reference: the number of main-tree node-0/1/2 `put_bool` events
    /// one block would emit. It mirrors the token-tree walk but drops all `(band,
    /// ctx)` bookkeeping, so it cross-checks the *event count* `count_coeffs`
    /// records without duplicating the attribution logic under test.
    fn node012_events(levels: [i16; 16], first: usize, last: i32) -> u64 {
        let mut n = first;
        let mut count = 0u64;
        loop {
            if n as i32 > last {
                count += 1; // node 0: EOB / empty
                break;
            }
            count += 1; // node 0: more
            while levels[n] == 0 {
                count += 1; // node 1: another zero
                n += 1;
            }
            count += 1; // node 1: non-zero
            count += 1; // node 2: one-vs-large
            n += 1;
            if n == 16 {
                break;
            }
        }
        count
    }

    #[test]
    fn count_records_exactly_the_main_tree_events_emit_would_code() {
        // A 16×16-predicted MB (with a Y2 block) whose luma/chroma exercise empty
        // blocks, zero-runs, unit levels and large values. The total node-0/1/2
        // count recorded by count_mb_residuals must equal an independent per-block
        // event count summed over the identical block order.
        let y2 = block_from_natural([1, 2, 0, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 0);
        let mut luma = [Block::default(); 16];
        for b in &mut luma {
            b.last = 0; // empty AC (16×16 luma starts at first = 1)
        }
        luma[0] = block_from_natural([0, 5, 0, 0, 17, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1], 1);
        luma[7] = block_from_natural([0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 1);
        let mut chroma = [Block::default(); 8];
        chroma[0] = block_from_natural([3, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 0);
        chroma[5] = block_from_natural([0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 40], 0);
        let mb = MbTokens {
            is_i4x4: false,
            y2,
            luma,
            chroma,
        };

        let mut stats = Box::<CoeffStats>::default();
        let mut ctx = NzContext::new(2);
        count_mb_residuals(&mut stats, &mb, &mut ctx, 0);

        let mut recorded = 0u64;
        for plane in stats.iter() {
            for band in plane {
                for c in band {
                    for node in c {
                        recorded += node[0] + node[1];
                    }
                }
            }
        }

        let mut expected = node012_events(mb.y2.levels, 0, mb.y2.last);
        for b in &mb.luma {
            expected += node012_events(b.levels, 1, b.last);
        }
        for b in &mb.chroma {
            expected += node012_events(b.levels, 0, b.last);
        }

        assert_eq!(
            recorded, expected,
            "count must record every main-tree event"
        );
        // Non-vacuity: 25 blocks means at least 25 EOB events, and the crafted
        // non-empty blocks push it well past that floor.
        assert!(
            expected > 25,
            "the crafted MB should code more than the EOB minimum"
        );
    }

    /// Three macroblocks with deliberately sparse, asymmetric non-zero patterns so
    /// the threaded top/left context words carry many distinct bit positions: a
    /// 16×16 MB with a non-empty Y2, a 16×16 MB with an EMPTY Y2 (to exercise the
    /// Y2 dc-nz test), and an i4×4 MB.
    fn sample_token_mbs() -> Vec<MbTokens> {
        let empty_ac = block_from_natural([0; 16], 1); // 16×16 luma: last = 0, nz = 1
        let nz_ac = block_from_natural([0, 0, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 1);
        let dc_chroma = block_from_natural([1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 0);

        // MB 0: 16×16, non-empty Y2, luma nz at raster {0,1,4,6,9,11,14}.
        let mut luma0 = [empty_ac; 16];
        for &i in &[0usize, 1, 4, 6, 9, 11, 14] {
            luma0[i] = nz_ac;
        }
        let mut chroma0 = [Block::default(); 8];
        for &i in &[0usize, 3, 5, 6] {
            chroma0[i] = dc_chroma;
        }
        let mb0 = MbTokens {
            is_i4x4: false,
            y2: block_from_natural([1, 2, 0, 3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 0),
            luma: luma0,
            chroma: chroma0,
        };

        // MB 1: 16×16 with an EMPTY Y2 (nz == 0) and a different nz pattern.
        let mut luma1 = [empty_ac; 16];
        for &i in &[2usize, 3, 5, 8, 12, 15] {
            luma1[i] = nz_ac;
        }
        let mut chroma1 = [Block::default(); 8];
        for &i in &[1usize, 2, 4, 7] {
            chroma1[i] = dc_chroma;
        }
        let mb1 = MbTokens {
            is_i4x4: false,
            y2: block_from_natural([0; 16], 0), // last = -1, nz = 0
            luma: luma1,
            chroma: chroma1,
        };

        // MB 2: i4×4 (no Y2, luma AC starts at 0) with yet another pattern.
        let dc_luma = block_from_natural([2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 0);
        let mut luma2 = [Block::default(); 16];
        for &i in &[0usize, 5, 7, 10, 13] {
            luma2[i] = dc_luma;
        }
        let mut chroma2 = [Block::default(); 8];
        for &i in &[0usize, 2, 5] {
            chroma2[i] = dc_chroma;
        }
        let mb2 = MbTokens {
            is_i4x4: true,
            y2: Block::default(),
            luma: luma2,
            chroma: chroma2,
        };

        vec![mb0, mb1, mb2]
    }

    #[test]
    fn count_threads_the_nz_context_exactly_like_emit() {
        // count_mb_residuals must thread the top/left non-zero context byte-for-byte
        // the way emit_mb_residuals does (the encoder relies on the two staying in
        // lockstep). emit's threading is pinned by the decoder round-trip tests, so
        // we use it as the oracle: run both over the same macroblocks from the same
        // starting context and assert the resulting NzContext is identical after
        // each MB. Any mutation to count's context math — the l-threading shifts and
        // masks, the Y2 dc-nz `> 0` test, or the `& 0xff` output masks — makes
        // count's context diverge from emit's here.
        let mbs = sample_token_mbs();

        let mut ctx_e = NzContext::new(1);
        let mut ctx_c = NzContext::new(1);
        let mut stats = Box::<CoeffStats>::default();
        let mut saw_dc_nz = false;
        for mb in &mbs {
            let mut enc = BoolEncoder::new();
            emit_mb_residuals(&mut enc, &COEFFS_PROBA_0, mb, &mut ctx_e, 0);
            count_mb_residuals(&mut stats, mb, &mut ctx_c, 0);
            assert_eq!(ctx_e.top, ctx_c.top, "top nz word");
            assert_eq!(ctx_e.top_dc, ctx_c.top_dc, "top dc-nz word");
            assert_eq!(ctx_e.left, ctx_c.left, "left nz word");
            assert_eq!(ctx_e.left_dc, ctx_c.left_dc, "left dc-nz flag");
            saw_dc_nz |= ctx_c.top_dc[0] != 0;
        }
        // Non-vacuity: the non-empty-Y2 MB must have set a Y2 dc-nz flag, and the
        // final threaded words must be non-zero (otherwise the compare is trivial).
        assert!(saw_dc_nz, "a Y2 dc-nz flag should have been set");
        assert!(
            ctx_c.top[0] != 0 && ctx_c.left != 0,
            "threaded context should carry non-zero bits"
        );
    }

    #[test]
    fn count_attributes_node0_events_to_the_correct_nz_context_bins() {
        // Every block is a single DC = 1 coefficient, so each block is non-zero
        // (nz > 0) and contributes exactly one node-0 "more" event, recorded at
        // stats[plane][band 0][c][0][1] where c = (top-nz + left-nz) is that block's
        // non-zero context. The MB is i4×4 (luma plane 3, no Y2; chroma plane 2), so
        // every block is non-zero and its context is fully determined by its
        // neighbours: an interior block sees two non-zero neighbours (c = 2), an
        // edge block sees the input top/left context bits. An independent
        // neighbour-grid reference computes the exact per-(plane, c) event
        // distribution; any mutation to the context derivation — the input masks
        // (`& 0x0f`), the `l + top` sum, or the chroma `4 + ch` offset — moves an
        // event into the wrong [c] bin.
        let dc1 = block_from_natural([1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], 0);
        let mb = MbTokens {
            is_i4x4: true,
            y2: Block::default(),
            luma: [dc1; 16],
            chroma: [dc1; 8],
        };

        // Rich, asymmetric input context: distinct bits at every offset the
        // derivation reads (luma low nibble, chroma bits 4/5 for U and 6/7 for V,
        // and — for the chroma left offset — bits 0..8 all in play).
        let mb_nz_in = 0xCAu32;
        let left_nz_in = 0x6Cu32;

        let mut stats = Box::<CoeffStats>::default();
        let mut ctx = NzContext::new(1);
        ctx.top[0] = mb_nz_in;
        ctx.left = left_nz_in;
        count_mb_residuals(&mut stats, &mb, &mut ctx, 0);

        // Independent reference: every block is non-zero, so the top neighbour of an
        // interior row is 1 and the left neighbour of an interior column is 1; edge
        // neighbours come from the input context bits.
        let bit = |word: u32, i: u32| (word >> i) & 1 == 1;
        let mut expected = [[0u64; 3]; 4]; // [plane][c]
        // Luma (plane 3): 4×4 grid, top bits = mb_nz[0..4], left bits = left_nz[0..4].
        for r in 0..4u32 {
            for col in 0..4u32 {
                let top = if r == 0 { bit(mb_nz_in, col) } else { true };
                let left = if col == 0 { bit(left_nz_in, r) } else { true };
                expected[3][usize::from(top) + usize::from(left)] += 1;
            }
        }
        // Chroma (plane 2): U (ch = 0) then V (ch = 2), each a 2×2 grid. Top and
        // left bits come from mb_nz / left_nz at offset 4 + ch.
        for ch in [0u32, 2] {
            for r in 0..2u32 {
                for col in 0..2u32 {
                    let top = if r == 0 {
                        bit(mb_nz_in, 4 + ch + col)
                    } else {
                        true
                    };
                    let left = if col == 0 {
                        bit(left_nz_in, 4 + ch + r)
                    } else {
                        true
                    };
                    expected[2][usize::from(top) + usize::from(left)] += 1;
                }
            }
        }

        for plane in [2usize, 3] {
            for c in 0..3usize {
                assert_eq!(
                    stats[plane][0][c][0][1], expected[plane][c],
                    "plane {plane} ctx {c} node-0 events"
                );
            }
        }
        // Non-vacuity: the interior blocks must populate the c == 2 bin in both
        // planes, so the `l + top` sum and the neighbour bits are actually exercised.
        assert!(
            expected[3][2] > 0 && expected[2][2] > 0,
            "interior blocks should reach context 2"
        );
    }
}
