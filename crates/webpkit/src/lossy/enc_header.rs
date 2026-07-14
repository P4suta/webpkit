//! Header emission (the encoder counterpart of [`crate::lossy::frame_header`] and
//! [`crate::lossy::header`]).
//!
//! [`frame_header_bytes`] writes the fixed 10-byte uncompressed key-frame header
//! (inverse of `frame_header::parse_key_frame`); [`write_control_header`] writes
//! the compressed partition-0 header — color space, segment, filter, partition
//! count, quantizer and coefficient-probability sections — in the exact order the
//! decoder reads them (`decode::reconstruct_to_planes` →
//! `header::parse_headers`/`parse_quant`/`parse_proba`). The header transmits a
//! single segment, one token partition, base quantizer with zero deltas, the
//! chosen in-loop filter (level `0` when filtering is off), and an optional skip
//! flag — but never segmentation or per-reference/per-mode loop-filter deltas.

use crate::lossy::bool_enc::BoolEncoder;
use crate::lossy::constants::{
    COEFFS_UPDATE_PROBA, CoeffProbas, CoeffUpdateFlags, NUM_BANDS, NUM_CTX, NUM_PROBAS, NUM_TYPES,
};
use crate::lossy::decode::FilterHeader;

/// The fixed 3-byte key-frame start code (`frame_header::KEY_FRAME_START_CODE`).
const START_CODE: [u8; 3] = [0x9d, 0x01, 0x2a];

/// Build the 10-byte uncompressed key-frame header for a `width`×`height` frame
/// whose control partition is `first_partition_size` bytes. Inverse of
/// `frame_header::parse_key_frame`: a 3-byte little-endian frame tag (key frame,
/// version 0, show-frame set, 19-bit partition size), the start code, and two
/// 14-bit dimensions (upscale factors zero).
#[must_use]
pub(crate) const fn frame_header_bytes(
    first_partition_size: u32,
    width: u16,
    height: u16,
) -> [u8; 10] {
    // Tag bits: bit0 = 0 (key frame), bits1-3 = version 0, bit4 = 1 (show frame),
    // bits5-23 = first_partition_size.
    let tag = (1u32 << 4) | (first_partition_size << 5);
    let t = tag.to_le_bytes();
    let w = (width & 0x3fff).to_le_bytes();
    let h = (height & 0x3fff).to_le_bytes();
    [
        t[0],
        t[1],
        t[2],
        START_CODE[0],
        START_CODE[1],
        START_CODE[2],
        w[0],
        w[1],
        h[0],
        h[1],
    ]
}

/// The macroblock-segmentation portion of the control header (RFC §9.3), emitted
/// only for the `Full`/`Best` tiers when the frame uses more than one segment. The
/// quantizers are transmitted as *relative* deltas over the base index
/// (`absolute_delta = false`), the per-segment loop-filter strengths are all zero
/// (so the deblocking filter behaves exactly as an unsegmented frame), and the map
/// is always retransmitted (`update_map = true`), so each macroblock carries its
/// segment id. The exact inverse of `header::parse_segment_header`.
#[derive(Clone, Copy)]
pub(crate) struct SegmentParams {
    /// Per-segment quantizer delta over the base index (`put_signed(7)`); unused
    /// segments carry `0`.
    pub(crate) quantizer: [i32; 4],
    /// Segment-id decision-tree probabilities (`put_literal(8)`), derived from the
    /// segment populations.
    pub(crate) tree_probs: [u8; 3],
}

/// The quantizer, segmentation and in-loop-filter portions of the control header,
/// bundled so the emission passes thread a single value (and stay under the
/// argument-count lint). `filter` carries the chosen simple/level/sharpness; a
/// `level` of `0` disables filtering. The encoder never emits per-reference/per-mode
/// deltas, so `filter.use_lf_delta` is ignored here (always coded as `false`).
/// `segments` is `Some` only when the frame uses macroblock segmentation; otherwise
/// segmentation is coded off (a single `0` flag), byte-identical to a single-segment
/// frame.
#[derive(Clone, Copy)]
pub(crate) struct HeaderParams<'a> {
    /// Base quantizer index (`0..=127`).
    pub(crate) base_q: i32,
    /// The chosen loop-filter parameters (`level == 0` when filtering is off).
    pub(crate) filter: &'a FilterHeader,
    /// The macroblock-segmentation header, or `None` for a single-segment frame.
    pub(crate) segments: Option<SegmentParams>,
}

/// Narrow a value already clamped into `0..=max` to `u32`. The clamp makes the
/// value non-negative and bounded, so the cast is lossless — this states that
/// invariant directly instead of routing it through a `try_from(..).unwrap_or(..)`
/// whose error arm can never be taken.
#[expect(
    clippy::cast_sign_loss,
    reason = "clamp(0, max) guarantees a non-negative value, so `as u32` cannot lose sign"
)]
fn clamped_u32(value: i32, max: i32) -> u32 {
    value.clamp(0, max) as u32
}

/// Emit the compressed control-partition header into `enc` from `header` (base
/// quantizer and in-loop filter), transmitting the coefficient-probability table
/// `probas` (the nodes flagged in `updated` carry an 8-bit literal; the rest fall
/// back to [`crate::lossy::constants::COEFFS_PROBA_0`]). When `use_skip` is set the header
/// also carries the per-macroblock skip probability `skip_p`, and each macroblock
/// then prefixes an explicit skip flag; otherwise no skip section is emitted (every
/// block codes its tokens). Covers everything the decoder reads before the
/// per-macroblock intra modes; the caller emits those modes into the same
/// partition afterwards.
pub(crate) fn write_control_header(
    enc: &mut BoolEncoder,
    header: HeaderParams<'_>,
    probas: &CoeffProbas,
    updated: &CoeffUpdateFlags,
    use_skip: bool,
    skip_p: u8,
) {
    // Color space + pixel-clamping flags (key frame): the decoder reads and
    // discards both.
    enc.put_flag(false); // color space
    enc.put_flag(false); // clamp type

    // Segment header (the exact inverse of `header::parse_segment_header`).
    match header.segments {
        None => enc.put_flag(false), // segmentation off (a single `0` flag)
        Some(seg) => {
            enc.put_flag(true); // use_segment
            enc.put_flag(true); // update_map (each macroblock carries its segment id)
            enc.put_flag(true); // "update data" present
            enc.put_flag(false); // absolute_delta = false (quantizers are relative)
            // Per-segment quantizer deltas: present + signed(7) when non-zero.
            for &q in &seg.quantizer {
                if q == 0 {
                    enc.put_flag(false);
                } else {
                    enc.put_flag(true);
                    enc.put_signed(7, q);
                }
            }
            // Per-segment filter-strength deltas: all absent (0), so the loop
            // filter behaves exactly as an unsegmented frame.
            for _ in 0..4 {
                enc.put_flag(false);
            }
            // update_map is set, so the three segment-id tree probabilities follow.
            for &p in &seg.tree_probs {
                enc.put_flag(true);
                enc.put_literal(8, u32::from(p));
            }
        },
    }

    // Filter header: the chosen in-loop filter (the exact inverse of
    // `header::parse_filter_header`). `level == 0` disables filtering; the encoder
    // emits no per-reference/per-mode deltas, so `use_lf_delta` is always false.
    let filter = header.filter;
    enc.put_flag(filter.simple); // simple (irrelevant at level 0)
    enc.put_literal(6, clamped_u32(filter.level, 63));
    enc.put_literal(3, clamped_u32(filter.sharpness, 7));
    enc.put_flag(false); // use loop-filter deltas

    // Partition count: 2^0 = 1 token partition (no size table follows).
    enc.put_literal(2, 0);

    // Quantizer: base index, then five absent DC/AC deltas.
    enc.put_literal(7, clamped_u32(header.base_q, 127));
    for _ in 0..5 {
        enc.put_flag(false);
    }

    // refresh_entropy_probs: read and ignored on a key frame.
    enc.put_flag(false);

    // Coefficient probabilities: the exact inverse of `header::parse_proba`. For
    // each node, transmit its update flag against COEFFS_UPDATE_PROBA, and when set
    // follow it with the 8-bit replacement probability.
    for t in 0..NUM_TYPES {
        for b in 0..NUM_BANDS {
            for c in 0..NUM_CTX {
                for p in 0..NUM_PROBAS {
                    if updated[t][b][c][p] {
                        enc.put_bool(COEFFS_UPDATE_PROBA[t][b][c][p], true);
                        enc.put_literal(8, u32::from(probas[t][b][c][p]));
                    } else {
                        enc.put_bool(COEFFS_UPDATE_PROBA[t][b][c][p], false);
                    }
                }
            }
        }
    }

    // Per-macroblock skip section (the exact inverse of `header::parse_proba`'s
    // trailing `use_skip = read_flag(); if use_skip { skip_p = read_literal(8) }`).
    enc.put_flag(use_skip);
    if use_skip {
        enc.put_literal(8, u32::from(skip_p));
    }
}

#[cfg(test)]
mod tests {
    use super::{HeaderParams, SegmentParams, frame_header_bytes, write_control_header};
    use crate::lossy::bool_dec::BoolDecoder;
    use crate::lossy::bool_enc::BoolEncoder;
    use crate::lossy::constants::{COEFFS_PROBA_0, CoeffUpdateFlags};
    use crate::lossy::decode::{FilterHeader, Frame};
    use crate::lossy::frame_header::FrameHeader;

    /// A `HeaderParams` at `base_q` with a borrowed filter (defaulting to level 0,
    /// filtering off) and no segmentation — the common shape the round-trip tests emit.
    fn params(base_q: i32, filter: &FilterHeader) -> HeaderParams<'_> {
        HeaderParams {
            base_q,
            filter,
            segments: None,
        }
    }

    #[test]
    fn frame_header_round_trips_through_parse_key_frame() {
        let bytes = frame_header_bytes(1234, 640, 480);
        let fh = FrameHeader::parse_key_frame(&bytes).unwrap();
        assert!(fh.key_frame);
        assert!(fh.show_frame);
        assert_eq!(fh.version, 0);
        assert_eq!(fh.first_partition_size, 1234);
        assert_eq!((fh.width, fh.height), (640, 480));
        assert_eq!((fh.x_scale, fh.y_scale), (0, 0));
    }

    #[test]
    fn frame_header_handles_max_dimensions_and_zero_partition() {
        let bytes = frame_header_bytes(0, 16383, 16383);
        let fh = FrameHeader::parse_key_frame(&bytes).unwrap();
        assert_eq!(fh.first_partition_size, 0);
        assert_eq!((fh.width, fh.height), (16383, 16383));
    }

    #[test]
    fn control_header_round_trips_through_the_decoder() {
        // Emit the control header, then replay the decoder's exact read sequence
        // (parse_headers -> parse_partitions -> parse_quant -> refresh flag ->
        // parse_proba) and assert the MVP invariants: no segmentation, filter off,
        // one partition, the transmitted base quantizer, default probabilities and
        // no skip.
        let mut enc = BoolEncoder::new();
        let no_updates = CoeffUpdateFlags::default();
        let filter = FilterHeader::default();
        write_control_header(
            &mut enc,
            params(32, &filter),
            &COEFFS_PROBA_0,
            &no_updates,
            false,
            0,
        );
        let bytes = enc.finish();

        let mut frame = Frame::test_frame(1, 1);
        let mut br = BoolDecoder::new(&bytes);
        frame.parse_headers(&mut br);
        let parts = frame.parse_partitions(&mut br, &[]).unwrap();
        frame.parse_quant(&mut br);
        let _refresh = br.read_flag();
        frame.parse_proba(&mut br);

        assert!(!frame.segment.use_segment, "segmentation off");
        assert_eq!(frame.filter.level, 0, "filter level 0");
        assert_eq!(frame.filter_type, 0, "filter off");
        assert_eq!(frame.num_parts, 1, "one token partition");
        assert_eq!(parts.len(), 1);
        // base_q 32, all deltas 0: y1 = [DC[32], AC[32]] = [29, 36].
        assert_eq!(frame.dqm[0].y1, [29, 36], "base quantizer decoded");
        assert_eq!(frame.proba.bands, COEFFS_PROBA_0, "default probabilities");
        assert!(!frame.proba.use_skip, "no skip flag");
    }

    #[test]
    fn control_header_transmits_distinct_base_quantizers() {
        // A different base_q must decode to different dequant factors, proving the
        // 7-bit field is really carried (not a constant).
        let no_updates = CoeffUpdateFlags::default();
        let filter = FilterHeader::default();
        for &(base_q, y1) in &[(0i32, [4, 4]), (64, [59, 78]), (127, [157, 284])] {
            let mut enc = BoolEncoder::new();
            write_control_header(
                &mut enc,
                params(base_q, &filter),
                &COEFFS_PROBA_0,
                &no_updates,
                false,
                0,
            );
            let bytes = enc.finish();
            let mut frame = Frame::test_frame(1, 1);
            let mut br = BoolDecoder::new(&bytes);
            frame.parse_headers(&mut br);
            frame.parse_partitions(&mut br, &[]).unwrap();
            frame.parse_quant(&mut br);
            assert_eq!(frame.dqm[0].y1, y1, "base_q {base_q}");
        }
    }

    #[test]
    fn control_header_round_trips_the_segment_header() {
        // A segmented control header must decode back through parse_segment_header /
        // parse_quant to exactly the emitted per-segment quantizers and tree probs:
        // use_segment + update_map set, relative deltas, all filter strengths zero.
        // Distinct signed quantizer deltas (mixed signs, one zero) catch a swapped or
        // dropped field, and the derived dqm proves the relative q = base_q + delta
        // derivation the decoder applies.
        let seg = SegmentParams {
            quantizer: [8, -12, 0, 20],
            tree_probs: [200, 60, 140],
        };
        let filter = FilterHeader::default();
        let base_q = 48;
        let mut enc = BoolEncoder::new();
        let header = HeaderParams {
            base_q,
            filter: &filter,
            segments: Some(seg),
        };
        let no_updates = CoeffUpdateFlags::default();
        write_control_header(&mut enc, header, &COEFFS_PROBA_0, &no_updates, false, 0);
        let bytes = enc.finish();

        let mut frame = Frame::test_frame(1, 1);
        let mut br = BoolDecoder::new(&bytes);
        frame.parse_headers(&mut br);
        frame.parse_partitions(&mut br, &[]).unwrap();
        frame.parse_quant(&mut br);

        assert!(frame.segment.use_segment, "segmentation on");
        assert!(frame.segment.update_map, "segment map retransmitted");
        assert!(!frame.segment.absolute_delta, "relative deltas");
        assert_eq!(
            frame.segment.quantizer,
            [8, -12, 0, 20],
            "per-segment quantizers"
        );
        assert_eq!(
            frame.segment.filter_strength,
            [0, 0, 0, 0],
            "no filter deltas"
        );
        assert_eq!(frame.proba.segments, [200, 60, 140], "segment tree probs");
        // The relative derivation: segment i uses q = base_q + delta, so segment 0's
        // luma dequant is that of base_q + 8 = 56, distinct from segment 2's base_q.
        let q0 = crate::lossy::quant::Quantizer::new(base_q + 8);
        assert_eq!(
            frame.dqm[0].y1,
            [q0.y1.dc.q, q0.y1.ac.q],
            "segment 0 dequant"
        );
        let q2 = crate::lossy::quant::Quantizer::new(base_q);
        assert_eq!(
            frame.dqm[2].y1,
            [q2.y1.dc.q, q2.y1.ac.q],
            "segment 2 dequant"
        );
    }

    #[test]
    fn control_header_round_trips_a_normal_loop_filter() {
        // A non-zero normal filter (simple = false, level = 27, sharpness = 3) must
        // decode back through `parse_filter_header` to exactly those values and
        // resolve to filter_type 2 (level != 0 && !simple) — the exact inverse the
        // encoder relies on so the decoder derives matching per-MB filter strengths.
        let no_updates = CoeffUpdateFlags::default();
        let filter = FilterHeader {
            simple: false,
            level: 27,
            sharpness: 3,
            ..FilterHeader::default()
        };
        let mut enc = BoolEncoder::new();
        write_control_header(
            &mut enc,
            params(48, &filter),
            &COEFFS_PROBA_0,
            &no_updates,
            false,
            0,
        );
        let bytes = enc.finish();

        let mut frame = Frame::test_frame(1, 1);
        let mut br = BoolDecoder::new(&bytes);
        frame.parse_headers(&mut br);
        frame.parse_partitions(&mut br, &[]).unwrap();
        frame.parse_quant(&mut br);

        assert!(!frame.filter.simple, "normal filter");
        assert_eq!(frame.filter.level, 27, "filter level");
        assert_eq!(frame.filter.sharpness, 3, "filter sharpness");
        assert!(!frame.filter.use_lf_delta, "no loop-filter deltas");
        assert_eq!(frame.filter_type, 2, "normal filter type");
    }
}
