//! Per-macroblock intra-mode parsing (RFC 6386 §11, libwebp `ParseIntraMode` /
//! `VP8ParseIntraModeRow`).
//!
//! Reads, for every macroblock of a row, its segment id, skip flag, luma
//! prediction (one 16×16 mode or sixteen 4×4 modes) and chroma mode from the
//! control partition, threading the top/left mode contexts.

use crate::lossy::bool_dec::BoolDecoder;
use crate::lossy::constants::{
    B_DC_PRED, B_HD_PRED, B_HE_PRED, B_HU_PRED, B_LD_PRED, B_RD_PRED, B_TM_PRED, B_VE_PRED,
    B_VL_PRED, B_VR_PRED, BMODES_PROBA, DC_PRED, H_PRED, NUM_BMODES, Prob, TM_PRED, V_PRED,
};
use crate::lossy::decode::Frame;

impl Frame {
    /// Parse the intra prediction modes of every macroblock in the current row.
    pub(crate) fn parse_intra_mode_row(&mut self, br: &mut BoolDecoder<'_>) {
        for mb_x in 0..self.mb_w {
            self.parse_intra_mode(br, mb_x);
        }
    }

    /// Parse one macroblock's segment, skip flag and prediction modes.
    fn parse_intra_mode(&mut self, br: &mut BoolDecoder<'_>, mb_x: usize) {
        let segment = if self.segment.update_map {
            // Hardcoded 3-node segment-id tree.
            if br.read_bool(self.proba.segments[0]) {
                2 + u8::from(br.read_bool(self.proba.segments[2]))
            } else {
                u8::from(br.read_bool(self.proba.segments[1]))
            }
        } else {
            0
        };
        self.mb_data[mb_x].segment = segment;

        if self.proba.use_skip {
            self.mb_data[mb_x].skip = br.read_bool(self.proba.skip_p);
        }

        let is_i4x4 = !br.read_bool(145);
        self.mb_data[mb_x].is_i4x4 = is_i4x4;
        let top = 4 * mb_x;
        if is_i4x4 {
            self.parse_i4x4_modes(br, mb_x, top);
        } else {
            let ymode = read_ymode16(br);
            self.mb_data[mb_x].imodes[0] = ymode;
            self.intra_t[top..top + 4].fill(ymode);
            self.intra_l = [ymode; 4];
        }

        self.mb_data[mb_x].uvmode = read_uvmode(br);
    }

    /// Parse the sixteen 4×4 luma sub-block modes, threading the top/left mode
    /// neighbors through the `kBModesProba` context.
    fn parse_i4x4_modes(&mut self, br: &mut BoolDecoder<'_>, mb_x: usize, top: usize) {
        for y in 0..4 {
            let mut left_mode = self.intra_l[y];
            for x in 0..4 {
                let top_mode = self.intra_t[top + x];
                let prob = BMODES_PROBA[usize::from(top_mode)][usize::from(left_mode)];
                let mode = read_bmode(br, prob);
                self.intra_t[top + x] = mode;
                left_mode = mode;
            }
            let row = y * 4;
            self.mb_data[mb_x].imodes[row..row + 4].copy_from_slice(&self.intra_t[top..top + 4]);
            self.intra_l[y] = left_mode;
        }
    }
}

/// Decode a luma 16×16 prediction mode from the hardcoded decision tree.
fn read_ymode16(br: &mut BoolDecoder<'_>) -> u8 {
    if br.read_bool(156) {
        if br.read_bool(128) { TM_PRED } else { H_PRED }
    } else if br.read_bool(163) {
        V_PRED
    } else {
        DC_PRED
    }
}

/// Decode a chroma prediction mode from the hardcoded decision tree.
fn read_uvmode(br: &mut BoolDecoder<'_>) -> u8 {
    if !br.read_bool(142) {
        DC_PRED
    } else if !br.read_bool(114) {
        V_PRED
    } else if br.read_bool(183) {
        TM_PRED
    } else {
        H_PRED
    }
}

/// Decode one intra 4×4 (B) sub-block mode from its `kBModesProba` row using the
/// hardcoded decision tree (libwebp `USE_GENERIC_TREE == 0` path). `pub(crate)` so
/// the encoder's `tokens::put_bmode` round-trip test can cross-check its inverse.
pub(crate) fn read_bmode(br: &mut BoolDecoder<'_>, prob: [Prob; NUM_BMODES - 1]) -> u8 {
    if !br.read_bool(prob[0]) {
        return B_DC_PRED;
    }
    if !br.read_bool(prob[1]) {
        return B_TM_PRED;
    }
    if !br.read_bool(prob[2]) {
        return B_VE_PRED;
    }
    if !br.read_bool(prob[3]) {
        return if !br.read_bool(prob[4]) {
            B_HE_PRED
        } else if !br.read_bool(prob[5]) {
            B_RD_PRED
        } else {
            B_VR_PRED
        };
    }
    if !br.read_bool(prob[6]) {
        B_LD_PRED
    } else if !br.read_bool(prob[7]) {
        B_VL_PRED
    } else if !br.read_bool(prob[8]) {
        B_HD_PRED
    } else {
        B_HU_PRED
    }
}
