//! Compressed control-partition header parsing (RFC 6386 §9.3–9.9, libwebp
//! `VP8GetHeaders` / `VP8ParseQuant` / `VP8ParseProba`).
//!
//! These methods read the segment, filter, partition, quantizer and probability
//! headers from partition 0 into the frame-persistent [`Frame`] state.

use crate::{Error, Result};

use crate::lossy::bool_dec::BoolDecoder;
use crate::lossy::constants::{
    AC_TABLE, COEFFS_PROBA_0, COEFFS_UPDATE_PROBA, DC_TABLE, NUM_BANDS, NUM_CTX, NUM_MB_SEGMENTS,
    NUM_PROBAS, NUM_TYPES,
};
use crate::lossy::decode::Frame;
use crate::lossy::prelude::*;

/// The DC dequant factor for quantizer index `idx` (clamped into the table).
fn dc_quant(idx: i32) -> i32 {
    i32::from(DC_TABLE[usize::try_from(idx).unwrap_or(0)])
}

/// The AC dequant factor for quantizer index `idx` (clamped into the table).
fn ac_quant(idx: i32) -> i32 {
    i32::from(AC_TABLE[usize::try_from(idx).unwrap_or(0)])
}

impl Frame {
    /// Parse the partition-0 headers up to (but not including) the token
    /// partitions: color space, segment header and filter header.
    pub(crate) fn parse_headers(&mut self, br: &mut BoolDecoder<'_>) {
        // Color space and pixel-clamping flags (key frame); neither affects the
        // decoded YCbCr samples, so we read past them.
        let _color_space = br.read_flag();
        let _clamp_type = br.read_flag();
        self.parse_segment_header(br);
        self.parse_filter_header(br);
    }

    /// RFC §9.3: segmentation enable, per-segment quantizer/filter deltas, and
    /// the segment-id tree probabilities.
    fn parse_segment_header(&mut self, br: &mut BoolDecoder<'_>) {
        self.segment.use_segment = br.read_flag();
        if !self.segment.use_segment {
            self.segment.update_map = false;
            return;
        }
        self.segment.update_map = br.read_flag();
        if br.read_flag() {
            // Update the per-segment quantizer / filter data.
            self.segment.absolute_delta = br.read_flag();
            for q in &mut self.segment.quantizer {
                *q = if br.read_flag() { br.read_signed(7) } else { 0 };
            }
            for f in &mut self.segment.filter_strength {
                *f = if br.read_flag() { br.read_signed(6) } else { 0 };
            }
        }
        if self.segment.update_map {
            for p in &mut self.proba.segments {
                *p = if br.read_flag() {
                    u8::try_from(br.read_literal(8)).unwrap_or(255)
                } else {
                    255
                };
            }
        }
    }

    /// RFC §9.4: the in-loop filter type, level, sharpness and the optional
    /// per-reference / per-mode level deltas.
    fn parse_filter_header(&mut self, br: &mut BoolDecoder<'_>) {
        self.filter.simple = br.read_flag();
        self.filter.level = i32::try_from(br.read_literal(6)).unwrap_or(0);
        self.filter.sharpness = i32::try_from(br.read_literal(3)).unwrap_or(0);
        self.filter.use_lf_delta = br.read_flag();
        if self.filter.use_lf_delta && br.read_flag() {
            for d in &mut self.filter.ref_lf_delta {
                if br.read_flag() {
                    *d = br.read_signed(6);
                }
            }
            for d in &mut self.filter.mode_lf_delta {
                if br.read_flag() {
                    *d = br.read_signed(6);
                }
            }
        }
        self.filter_type = if self.filter.level == 0 {
            0
        } else if self.filter.simple {
            1
        } else {
            2
        };
    }

    /// RFC §9.5: read the token-partition count and sizes, returning a slice for
    /// each partition carved out of `after` (the bytes following partition 0).
    ///
    /// # Errors
    ///
    /// [`Error::Truncated`] if `after` is too short to hold the size table.
    pub(crate) fn parse_partitions<'a>(
        &mut self,
        br: &mut BoolDecoder<'_>,
        after: &'a [u8],
    ) -> Result<Vec<&'a [u8]>> {
        let num_parts = 1usize << br.read_literal(2);
        self.num_parts = num_parts;
        let last = num_parts - 1;
        // The first `3 * last` bytes are the little-endian sizes of all but the
        // final partition; the partitions themselves follow.
        if after.len() < 3 * last {
            return Err(Error::Truncated);
        }
        let mut parts = Vec::with_capacity(num_parts);
        let mut start = 3 * last;
        let mut left = after.len() - 3 * last;
        for p in 0..last {
            let sz = usize::from(after[3 * p])
                | usize::from(after[3 * p + 1]) << 8
                | usize::from(after[3 * p + 2]) << 16;
            let psize = sz.min(left);
            parts.push(&after[start..start + psize]);
            start += psize;
            left -= psize;
        }
        parts.push(&after[start..start + left]);
        Ok(parts)
    }

    /// RFC §9.6: the base quantizer plus the five DC/AC deltas, expanded into a
    /// per-segment [`crate::lossy::decode::QuantMatrix`]. Port of `VP8ParseQuant`.
    #[allow(
        clippy::similar_names,
        reason = "the delta bindings mirror the libwebp field names \
                  (dqy1_dc, dqy2_dc, dqy2_ac, dquv_dc, dquv_ac) verbatim so the \
                  quantizer math stays traceable to the reference"
    )]
    pub(crate) fn parse_quant(&mut self, br: &mut BoolDecoder<'_>) {
        let base_q0 = i32::try_from(br.read_literal(7)).unwrap_or(0);
        let dqy1_dc = read_delta(br);
        let dqy2_dc = read_delta(br);
        let dqy2_ac = read_delta(br);
        let dquv_dc = read_delta(br);
        let dquv_ac = read_delta(br);

        for i in 0..NUM_MB_SEGMENTS {
            let q = if self.segment.use_segment {
                let mut q = self.segment.quantizer[i];
                if !self.segment.absolute_delta {
                    q += base_q0;
                }
                q
            } else if i > 0 {
                self.dqm[i] = self.dqm[0];
                continue;
            } else {
                base_q0
            };
            let m = &mut self.dqm[i];
            m.y1[0] = dc_quant(clip_q(q + dqy1_dc));
            m.y1[1] = ac_quant(clip_q(q));
            m.y2[0] = dc_quant(clip_q(q + dqy2_dc)) * 2;
            // x * 155 / 100, computed exactly as (x * 101581) >> 16.
            m.y2[1] = ((ac_quant(clip_q(q + dqy2_ac)) * 101_581) >> 16).max(8);
            m.uv[0] = dc_quant(clip_uv(q + dquv_dc));
            m.uv[1] = ac_quant(clip_q(q + dquv_ac));
        }
    }

    /// RFC §13.4 / §9.9: apply the transmitted coefficient-probability updates
    /// over the defaults, then read the macroblock skip probability. Port of
    /// `VP8ParseProba`.
    pub(crate) fn parse_proba(&mut self, br: &mut BoolDecoder<'_>) {
        for t in 0..NUM_TYPES {
            for b in 0..NUM_BANDS {
                for c in 0..NUM_CTX {
                    for p in 0..NUM_PROBAS {
                        self.proba.bands[t][b][c][p] =
                            if br.read_bool(COEFFS_UPDATE_PROBA[t][b][c][p]) {
                                u8::try_from(br.read_literal(8)).unwrap_or(0)
                            } else {
                                COEFFS_PROBA_0[t][b][c][p]
                            };
                    }
                }
            }
        }
        self.proba.use_skip = br.read_flag();
        if self.proba.use_skip {
            self.proba.skip_p = u8::try_from(br.read_literal(8)).unwrap_or(0);
        }
    }
}

/// Read an optional signed 4-bit quantizer delta (`0` when its flag is clear).
fn read_delta(br: &mut BoolDecoder<'_>) -> i32 {
    if br.read_flag() { br.read_signed(4) } else { 0 }
}

/// Clamp a quantizer index into the luma/AC table range `0..=127`.
fn clip_q(v: i32) -> i32 {
    v.clamp(0, 127)
}

/// Clamp a quantizer index into the chroma-DC table range `0..=117`.
fn clip_uv(v: i32) -> i32 {
    v.clamp(0, 117)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::cast_possible_truncation,
        reason = "test fixtures build header field values with casts that fit their targets \
                  by construction"
    )]

    use crate::lossy::bool_dec::BoolDecoder;
    use crate::lossy::bool_enc::BoolEncoder;
    use crate::lossy::constants::{COEFFS_PROBA_0, COEFFS_UPDATE_PROBA};
    use crate::lossy::decode::Frame;

    // ---- segment header (RFC 6386 §9.3, ParseSegmentHeader) -----------------

    // Segmentation on, "update data" present in ABSOLUTE mode with every
    // per-segment field transmitted. Distinct signed magnitudes (a ramp of
    // mixed signs) catch a swapped quantizer/filter loop, an off-by-one in the
    // field widths (7-bit q vs 6-bit filter), or a dropped sign flag; the three
    // segment-tree probs are read as raw 8-bit literals.
    #[test]
    fn segment_header_absolute_update_all_present() {
        let quant = [5i32, -13, 40, -60];
        let filt = [10i32, -20, 31, -7];
        let segp = [200u32, 17, 128];
        let mut enc = BoolEncoder::new();
        enc.put_flag(true); // use_segment
        enc.put_flag(true); // update_map
        enc.put_flag(true); // update data
        enc.put_flag(true); // absolute_delta
        for &q in &quant {
            enc.put_flag(true);
            enc.put_signed(7, q);
        }
        for &f in &filt {
            enc.put_flag(true);
            enc.put_signed(6, f);
        }
        for &p in &segp {
            enc.put_flag(true);
            enc.put_literal(8, p);
        }
        let bytes = enc.finish();
        let mut br = BoolDecoder::new(&bytes);
        let mut frame = Frame::test_frame(1, 1);
        frame.parse_segment_header(&mut br);
        assert!(frame.segment.use_segment);
        assert!(frame.segment.update_map);
        assert!(frame.segment.absolute_delta);
        assert_eq!(frame.segment.quantizer, [5, -13, 40, -60]);
        assert_eq!(frame.segment.filter_strength, [10, -20, 31, -7]);
        assert_eq!(frame.proba.segments, [200, 17, 128]);
    }

    // Segmentation on, DELTA mode, update_map == false, and only some of the
    // per-segment fields present (the others must stay 0, exercising the `else
    // { 0 }` arm). Because update_map is false the tree probs are NOT read and
    // remain at the reset default of 255 — a decoder that reads them anyway
    // would desync and this assertion would fail.
    #[test]
    fn segment_header_delta_partial_and_no_map() {
        let mut enc = BoolEncoder::new();
        enc.put_flag(true); // use_segment
        enc.put_flag(false); // update_map = false
        enc.put_flag(true); // update data
        enc.put_flag(false); // absolute_delta = false (delta mode)
        // quantizer: present 63, absent, present -1, absent.
        enc.put_flag(true);
        enc.put_signed(7, 63);
        enc.put_flag(false);
        enc.put_flag(true);
        enc.put_signed(7, -1);
        enc.put_flag(false);
        // filter_strength: absent, present -33, absent, present 7.
        enc.put_flag(false);
        enc.put_flag(true);
        enc.put_signed(6, -33);
        enc.put_flag(false);
        enc.put_flag(true);
        enc.put_signed(6, 7);
        let bytes = enc.finish();
        let mut br = BoolDecoder::new(&bytes);
        let mut frame = Frame::test_frame(1, 1);
        frame.parse_segment_header(&mut br);
        assert!(frame.segment.use_segment);
        assert!(!frame.segment.update_map);
        assert!(!frame.segment.absolute_delta);
        assert_eq!(frame.segment.quantizer, [63, 0, -1, 0]);
        assert_eq!(frame.segment.filter_strength, [0, -33, 0, 7]);
        // update_map false → probs untouched (test_frame resets them to 255).
        assert_eq!(frame.proba.segments, [255, 255, 255]);
    }

    // Segmentation OFF: exactly ONE flag is consumed, then the parser returns.
    // A trailing 1-bit is appended and must still be readable afterwards — this
    // pins the early-return path and catches any over-reading.
    #[test]
    fn segment_header_disabled_reads_single_flag() {
        let mut enc = BoolEncoder::new();
        enc.put_flag(false); // use_segment = false
        enc.put_flag(true); // sentinel that MUST survive
        let bytes = enc.finish();
        let mut br = BoolDecoder::new(&bytes);
        let mut frame = Frame::test_frame(1, 1);
        frame.parse_segment_header(&mut br);
        assert!(!frame.segment.use_segment);
        assert!(!frame.segment.update_map);
        assert!(br.read_flag(), "only one flag should have been consumed");
    }

    // ---- filter header (RFC 6386 §9.4, ParseFilterHeader) -------------------

    // Normal (non-simple) filter with per-ref / per-mode level deltas, some
    // present and some absent. Absent entries keep their prior 0 (there is no
    // `else` arm here, unlike the segment quantizers). filter_type resolves to 2
    // (level != 0 && !simple). Distinct level/sharpness literals (42 / 5) catch
    // a 6-bit vs 3-bit field mixup.
    #[test]
    fn filter_header_with_lf_deltas() {
        let mut enc = BoolEncoder::new();
        enc.put_flag(false); // simple = false (normal)
        enc.put_literal(6, 42); // level
        enc.put_literal(3, 5); // sharpness
        enc.put_flag(true); // use_lf_delta
        enc.put_flag(true); // update lf-delta
        // ref_lf_delta: 2, -3, (absent), 15.
        enc.put_flag(true);
        enc.put_signed(6, 2);
        enc.put_flag(true);
        enc.put_signed(6, -3);
        enc.put_flag(false);
        enc.put_flag(true);
        enc.put_signed(6, 15);
        // mode_lf_delta: -1, (absent), 4, -8.
        enc.put_flag(true);
        enc.put_signed(6, -1);
        enc.put_flag(false);
        enc.put_flag(true);
        enc.put_signed(6, 4);
        enc.put_flag(true);
        enc.put_signed(6, -8);
        let bytes = enc.finish();
        let mut br = BoolDecoder::new(&bytes);
        let mut frame = Frame::test_frame(1, 1);
        frame.parse_filter_header(&mut br);
        assert!(!frame.filter.simple);
        assert_eq!(frame.filter.level, 42);
        assert_eq!(frame.filter.sharpness, 5);
        assert!(frame.filter.use_lf_delta);
        assert_eq!(frame.filter.ref_lf_delta, [2, -3, 0, 15]);
        assert_eq!(frame.filter.mode_lf_delta, [-1, 0, 4, -8]);
        assert_eq!(frame.filter_type, 2);
    }

    // Simple filter, non-zero level, no lf-delta block → filter_type == 1.
    #[test]
    fn filter_header_simple_no_delta() {
        let mut enc = BoolEncoder::new();
        enc.put_flag(true); // simple
        enc.put_literal(6, 17); // level
        enc.put_literal(3, 0); // sharpness
        enc.put_flag(false); // use_lf_delta = false
        let bytes = enc.finish();
        let mut br = BoolDecoder::new(&bytes);
        let mut frame = Frame::test_frame(1, 1);
        frame.parse_filter_header(&mut br);
        assert!(frame.filter.simple);
        assert_eq!(frame.filter.level, 17);
        assert_eq!(frame.filter.sharpness, 0);
        assert!(!frame.filter.use_lf_delta);
        assert_eq!(frame.filter_type, 1);
    }

    // level == 0 forces filter_type 0 regardless of `simple`; use_lf_delta is
    // set but its update flag is clear, so the delta arrays stay all-zero.
    #[test]
    fn filter_header_level_zero_disables_filter() {
        let mut enc = BoolEncoder::new();
        enc.put_flag(true); // simple (irrelevant when level == 0)
        enc.put_literal(6, 0); // level 0
        enc.put_literal(3, 4); // sharpness
        enc.put_flag(true); // use_lf_delta
        enc.put_flag(false); // ...but no update
        let bytes = enc.finish();
        let mut br = BoolDecoder::new(&bytes);
        let mut frame = Frame::test_frame(1, 1);
        frame.parse_filter_header(&mut br);
        assert_eq!(frame.filter.level, 0);
        assert_eq!(frame.filter_type, 0);
        assert_eq!(frame.filter.ref_lf_delta, [0, 0, 0, 0]);
        assert_eq!(frame.filter.mode_lf_delta, [0, 0, 0, 0]);
    }

    // ---- quantizer (RFC 6386 §9.6, VP8ParseQuant) ---------------------------

    // No segmentation: q == base_q0 for segment 0, and segments 1..3 mirror it.
    // base_q0 = 40 with the five distinct DC/AC deltas. Expected factors derived
    // independently from kDcTable / kAcTable (indices in comments):
    //   y1[0] = DC[40+4=44]   = 40
    //   y1[1] = AC[40]        = 44
    //   y2[0] = DC[40-8=32]*2 = 29*2 = 58        (the ×2 rule)
    //   y2[1] = (AC[40+10=50] * 101581) >> 16 = (54*101581)>>16 = 83
    //   uv[0] = DC[40-5=35]   = 32               (clip cap 117 not reached)
    //   uv[1] = AC[40+3=43]   = 47
    #[test]
    fn parse_quant_base_and_deltas_no_segments() {
        let mut enc = BoolEncoder::new();
        enc.put_literal(7, 40); // base_q0
        enc.put_flag(true);
        enc.put_signed(4, 4); // dqy1_dc
        enc.put_flag(true);
        enc.put_signed(4, -8); // dqy2_dc
        enc.put_flag(true);
        enc.put_signed(4, 10); // dqy2_ac
        enc.put_flag(true);
        enc.put_signed(4, -5); // dquv_dc
        enc.put_flag(true);
        enc.put_signed(4, 3); // dquv_ac
        let bytes = enc.finish();
        let mut br = BoolDecoder::new(&bytes);
        let mut frame = Frame::test_frame(1, 1);
        frame.parse_quant(&mut br);
        let m = frame.dqm[0];
        assert_eq!(m.y1, [40, 44]);
        assert_eq!(m.y2, [58, 83]);
        assert_eq!(m.uv, [32, 47]);
        for seg in &frame.dqm[1..4] {
            assert_eq!(seg.y1, m.y1);
            assert_eq!(seg.y2, m.y2);
            assert_eq!(seg.uv, m.uv);
        }
    }

    // Per-segment (absolute) quantizers exercising every clip boundary:
    //   segment 0, q = 120:
    //     y1[0] = DC[clip(130→127)] = DC[127] = 157
    //     y1[1] = AC[120]           = 249
    //     y2[0] = DC[120]*2         = 138*2 = 276
    //     y2[1] = (AC[120]*101581)>>16 = (249*101581)>>16 = 385
    //     uv[0] = DC[clip_uv(130→117)] = DC[117] = 132  (≠ 157: proves the
    //             chroma-DC cap is 117, distinct from the luma cap of 127)
    //     uv[1] = AC[120]           = 249
    //   segment 3, q = -5 (clamps low to 0):
    //     y1[0] = DC[clip(5)]  = 9        (q+10)
    //     y1[1] = AC[clip(0)]  = 4
    //     y2[0] = DC[0]*2      = 8
    //     y2[1] = max((AC[0]*101581)>>16, 8) = max(6, 8) = 8  (the floor-at-8)
    //     uv[0] = DC[clip_uv(5)] = 9
    //     uv[1] = AC[0]        = 4
    #[test]
    fn parse_quant_per_segment_clamps() {
        let mut enc = BoolEncoder::new();
        enc.put_literal(7, 5); // base_q0 (unused for q in absolute mode)
        enc.put_flag(true);
        enc.put_signed(4, 10); // dqy1_dc = +10
        enc.put_flag(false); // dqy2_dc = 0
        enc.put_flag(false); // dqy2_ac = 0
        enc.put_flag(true);
        enc.put_signed(4, 10); // dquv_dc = +10
        enc.put_flag(false); // dquv_ac = 0
        let bytes = enc.finish();
        let mut br = BoolDecoder::new(&bytes);
        let mut frame = Frame::test_frame(1, 1);
        frame.segment.use_segment = true;
        frame.segment.absolute_delta = true;
        frame.segment.quantizer = [120, 125, 3, -5];
        frame.parse_quant(&mut br);
        assert_eq!(frame.dqm[0].y1, [157, 249]);
        assert_eq!(frame.dqm[0].y2, [276, 385]);
        assert_eq!(frame.dqm[0].uv, [132, 249]);
        assert_eq!(frame.dqm[3].y1, [9, 4]);
        assert_eq!(frame.dqm[3].y2, [8, 8]);
        assert_eq!(frame.dqm[3].uv, [9, 4]);
    }

    // ---- coefficient probabilities (RFC 6386 §13.4/§9.9, VP8ParseProba) -----

    // Read all 4*8*3*11 = 1056 update decisions, of which exactly two carry an
    // update. Each update reads the update flag at CoeffsUpdateProba[t][b][c][p]
    // then an 8-bit literal replacement; every other node falls back to the
    // CoeffsProba0 default (NOT left as 0). Both updated nodes and their
    // untouched neighbors are asserted, then use_skip + skip_p.
    #[test]
    fn parse_proba_selective_updates_and_skip() {
        let mut enc = BoolEncoder::new();
        for (t, plane) in COEFFS_UPDATE_PROBA.iter().enumerate() {
            for (b, band) in plane.iter().enumerate() {
                for (c, ctx) in band.iter().enumerate() {
                    for (p, &up) in ctx.iter().enumerate() {
                        match (t, b, c, p) {
                            (0, 1, 0, 0) => {
                                enc.put_bool(up, true);
                                enc.put_literal(8, 7);
                            },
                            (1, 0, 2, 0) => {
                                enc.put_bool(up, true);
                                enc.put_literal(8, 200);
                            },
                            _ => enc.put_bool(up, false),
                        }
                    }
                }
            }
        }
        enc.put_flag(true); // use_skip
        enc.put_literal(8, 42); // skip_p
        let bytes = enc.finish();
        let mut br = BoolDecoder::new(&bytes);
        let mut frame = Frame::test_frame(1, 1);
        frame.parse_proba(&mut br);
        // The two transmitted overrides.
        assert_eq!(frame.proba.bands[0][1][0][0], 7);
        assert_eq!(frame.proba.bands[1][0][2][0], 200);
        // Untouched nodes fall back to CoeffsProba0 (distinctive non-128 values):
        assert_eq!(frame.proba.bands[0][0][0][0], 128); // CoeffsProba0[0][0][0][0]
        assert_eq!(frame.proba.bands[0][1][0][1], 136); // neighbor of node A
        assert_eq!(frame.proba.bands[1][0][2][1], 47); // neighbor of node B
        assert!(frame.proba.use_skip);
        assert_eq!(frame.proba.skip_p, 42);
    }

    // No updates at all: the whole table must equal CoeffsProba0 verbatim, and
    // with use_skip == false the skip probability is not read (stays 0).
    #[test]
    fn parse_proba_no_updates_uses_defaults_and_no_skip() {
        let mut enc = BoolEncoder::new();
        for plane in &COEFFS_UPDATE_PROBA {
            for band in plane {
                for ctx in band {
                    for &up in ctx {
                        enc.put_bool(up, false);
                    }
                }
            }
        }
        enc.put_flag(false); // use_skip = false
        let bytes = enc.finish();
        let mut br = BoolDecoder::new(&bytes);
        let mut frame = Frame::test_frame(1, 1);
        frame.parse_proba(&mut br);
        assert_eq!(frame.proba.bands, COEFFS_PROBA_0);
        assert!(!frame.proba.use_skip);
        assert_eq!(frame.proba.skip_p, 0);
    }

    // ---- token partitions (RFC 6386 §9.5, parse_partitions) -----------------

    // Four token partitions (`read_literal(2) == 2` -> `1 << 2 == 4`). The
    // `3 * 3 == 9`-byte size table declares the first three partition sizes
    // little-endian; the fourth partition is the trailing remainder. The sizes
    // are chosen to fit exactly (no `left`-clamping) with distinct low and mid
    // bytes, so every returned slice can be asserted against its exact byte
    // range. This one test pins the partition count (`1 << literal`), the
    // per-entry index arithmetic (`3 * p`, `+ 1`, `+ 2`), the little-endian
    // byte assembly and its `<< 8` shift, the running `start`/`left` cursors,
    // and the final remainder push.
    #[test]
    fn parse_partitions_four_exact_sizes() {
        // sizes: 261 == 5 | 1 << 8, 522 == 10 | 2 << 8, 3.
        let mut after = vec![0u8; 9 + 261 + 522 + 3 + 4];
        for (i, b) in after.iter_mut().enumerate() {
            if i >= 9 {
                *b = i as u8;
            }
        }
        after[0..3].copy_from_slice(&[5, 1, 0]);
        after[3..6].copy_from_slice(&[10, 2, 0]);
        after[6..9].copy_from_slice(&[3, 0, 0]);
        let mut enc = BoolEncoder::new();
        enc.put_literal(2, 2); // 1 << 2 == 4 partitions
        let bytes = enc.finish();
        let mut br = BoolDecoder::new(&bytes);
        let mut frame = Frame::test_frame(1, 1);
        let parts = frame.parse_partitions(&mut br, &after).unwrap();
        assert_eq!(frame.num_parts, 4);
        assert_eq!(parts.len(), 4);
        assert_eq!(parts[0], &after[9..270]); // 261 bytes
        assert_eq!(parts[1], &after[270..792]); // 522 bytes
        assert_eq!(parts[2], &after[792..795]); // 3 bytes
        assert_eq!(parts[3], &after[795..799]); // 4-byte remainder
    }

    // A two-partition stream whose only declared size has its non-zero byte in
    // the HIGH (`<< 16`) lane, so the third size-table byte and its 16-bit
    // shift are load-bearing (the exact-sizes test above keeps those bytes 0).
    // The declared size (65536) exceeds the 20 available data bytes, so it
    // clamps to `left`: partition 0 receives every data byte and partition 1 is
    // empty. Dropping the high byte (or flipping `<< 16` to `>> 16`) collapses
    // the size to 0 and would hand partition 0 nothing.
    #[test]
    fn parse_partitions_high_byte_size_clamped() {
        let mut after = vec![0u8; 3 + 20];
        for (i, b) in after.iter_mut().enumerate() {
            if i >= 3 {
                *b = i as u8;
            }
        }
        after[0..3].copy_from_slice(&[0, 0, 1]); // size0 == 1 << 16 == 65536
        let mut enc = BoolEncoder::new();
        enc.put_literal(2, 1); // 1 << 1 == 2 partitions
        let bytes = enc.finish();
        let mut br = BoolDecoder::new(&bytes);
        let mut frame = Frame::test_frame(1, 1);
        let parts = frame.parse_partitions(&mut br, &after).unwrap();
        assert_eq!(frame.num_parts, 2);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0], &after[3..23]); // all 20 data bytes (clamped)
        assert!(parts[1].is_empty());
    }

    // The size table for N partitions needs `3 * (N - 1)` bytes; a shorter
    // `after` must fail with `Truncated` rather than index out of bounds.
    #[test]
    fn parse_partitions_truncated_size_table() {
        let after = [0u8; 5]; // < 3 * (4 - 1) == 9
        let mut enc = BoolEncoder::new();
        enc.put_literal(2, 2); // 4 partitions -> 9 header bytes required
        let bytes = enc.finish();
        let mut br = BoolDecoder::new(&bytes);
        let mut frame = Frame::test_frame(1, 1);
        assert!(matches!(
            frame.parse_partitions(&mut br, &after),
            Err(crate::Error::Truncated)
        ));
    }
}
