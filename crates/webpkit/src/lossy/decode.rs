//! VP8 key-frame decode orchestration and the frame-persistent decoder state.
//!
//! This drives the bottom-up pipeline: parse the compressed control-partition
//! headers (`header`), then walk the macroblock grid parsing intra modes (`mb`)
//! and residual coefficients (`token`). Reconstruction (prediction, IDCT, loop
//! filter, YUV→RGB) is not wired yet, so [`decode_frame`] parses the whole frame
//! and then returns [`Error::UnsupportedFeature`].

use crate::{Codec, Dimensions, Error, Image, Result};

use crate::lossy::bool_dec::BoolDecoder;
use crate::lossy::constants::{
    B_DC_PRED, CoeffProbas, NUM_MB_SEGMENTS, NUM_MODE_LF_DELTAS, NUM_REF_LF_DELTAS,
};
use crate::lossy::frame_header::{FrameHeader, KEY_FRAME_HEADER_LEN};
use crate::lossy::loop_filter::FInfo;
use crate::lossy::prelude::*;
use crate::lossy::reconstruct::{self, Planes};

/// Per-segment quantizer / filter adjustments (RFC §9.3, libwebp
/// `VP8SegmentHeader`).
pub(crate) struct SegmentHeader {
    /// Whether segment-based adjustments are enabled.
    pub(crate) use_segment: bool,
    /// Whether the per-macroblock segment map is (re)transmitted this frame.
    pub(crate) update_map: bool,
    /// Whether `quantizer`/`filter_strength` are absolute or deltas over the base.
    pub(crate) absolute_delta: bool,
    /// Per-segment quantizer adjustment.
    pub(crate) quantizer: [i32; NUM_MB_SEGMENTS],
    /// Per-segment loop-filter strength adjustment.
    pub(crate) filter_strength: [i32; NUM_MB_SEGMENTS],
}

/// Loop-filter parameters (RFC §9.4, libwebp `VP8FilterHeader`).
#[derive(Default)]
pub(crate) struct FilterHeader {
    /// Simple (`true`) vs normal (`false`) in-loop filter.
    pub(crate) simple: bool,
    /// Base filter level in `0..=63` (`0` disables filtering).
    pub(crate) level: i32,
    /// Filter sharpness in `0..=7`.
    pub(crate) sharpness: i32,
    /// Whether per-reference / per-mode filter-level deltas are in use.
    pub(crate) use_lf_delta: bool,
    /// Per-reference-frame filter-level deltas.
    pub(crate) ref_lf_delta: [i32; NUM_REF_LF_DELTAS],
    /// Per-prediction-mode filter-level deltas.
    pub(crate) mode_lf_delta: [i32; NUM_MODE_LF_DELTAS],
}

/// One segment's dequantization factors: `[DC, AC]` for each plane class.
#[derive(Clone, Copy, Default)]
pub(crate) struct QuantMatrix {
    /// Luma AC/DC dequant factors.
    pub(crate) y1: [i32; 2],
    /// Second-order (Y2 / WHT) dequant factors.
    pub(crate) y2: [i32; 2],
    /// Chroma dequant factors.
    pub(crate) uv: [i32; 2],
}

/// Frame-persistent entropy-coding probabilities (RFC §13, libwebp `VP8Proba`).
pub(crate) struct Proba {
    /// Segment-id decision-tree probabilities.
    pub(crate) segments: [u8; crate::lossy::constants::MB_FEATURE_TREE_PROBS],
    /// Coefficient token probabilities, `[type][band][context][proba]`.
    pub(crate) bands: CoeffProbas,
    /// Whether macroblocks may carry an explicit skip flag.
    pub(crate) use_skip: bool,
    /// The probability that a macroblock is skipped.
    pub(crate) skip_p: u8,
}

/// Everything decoded for one macroblock (RFC §, libwebp `VP8MBData`).
pub(crate) struct MbData {
    /// Dequantized coefficients: `(16 luma + 4 U + 4 V) * 16`.
    pub(crate) coeffs: [i16; 384],
    /// Whether the luma is coded as sixteen 4×4 blocks (else one 16×16).
    pub(crate) is_i4x4: bool,
    /// One 16×16 mode (`imodes[0]`) or sixteen 4×4 modes.
    pub(crate) imodes: [u8; 16],
    /// Chroma prediction mode.
    pub(crate) uvmode: u8,
    /// Per-4×4 non-zero code for the 16 luma blocks (2 bits each).
    pub(crate) non_zero_y: u32,
    /// Per-4×4 non-zero code for the 8 chroma blocks.
    pub(crate) non_zero_uv: u32,
    /// Whether this macroblock's residual is skipped.
    pub(crate) skip: bool,
    /// The macroblock's segment id (`0..=3`).
    pub(crate) segment: u8,
}

impl Default for MbData {
    fn default() -> Self {
        Self {
            coeffs: [0; 384],
            is_i4x4: false,
            imodes: [0; 16],
            uvmode: 0,
            non_zero_y: 0,
            non_zero_uv: 0,
            skip: false,
            segment: 0,
        }
    }
}

/// Top/left non-zero context carried between macroblocks (libwebp `VP8MB`).
#[derive(Clone, Copy, Default)]
pub(crate) struct MbContext {
    /// Non-zero AC/DC flags: 4 luma bits + 4 chroma bits.
    pub(crate) nz: u32,
    /// Non-zero second-order (Y2) DC flag.
    pub(crate) nz_dc: u32,
}

/// Frame-persistent VP8 decoder state for a single key frame.
pub(crate) struct Frame {
    /// Frame width in macroblocks.
    pub(crate) mb_w: usize,
    /// Frame height in macroblocks.
    pub(crate) mb_h: usize,
    /// Segment header.
    pub(crate) segment: SegmentHeader,
    /// Filter header.
    pub(crate) filter: FilterHeader,
    /// Filter type: 0 off, 1 simple, 2 normal.
    pub(crate) filter_type: u8,
    /// Entropy-coding probabilities.
    pub(crate) proba: Proba,
    /// Per-segment dequantization factors.
    pub(crate) dqm: [QuantMatrix; NUM_MB_SEGMENTS],
    /// Number of token partitions (a power of two, `1..=8`).
    pub(crate) num_parts: usize,
    /// Reconstruction data for the macroblocks of the current row.
    pub(crate) mb_data: Vec<MbData>,
    /// Top intra-mode context: 4 sub-block modes per macroblock column.
    pub(crate) intra_t: Vec<u8>,
    /// Left intra-mode context: 4 sub-block modes.
    pub(crate) intra_l: [u8; 4],
    /// Non-zero context: index 0 is the running "left" sentinel, index `x + 1`
    /// carries the "top" context for macroblock column `x`.
    pub(crate) mb_info: Vec<MbContext>,
}

impl Frame {
    /// Allocate frame state for the key frame described by `fh`.
    pub(crate) fn new(fh: FrameHeader) -> Result<Self> {
        let width = u32::from(fh.width);
        let height = u32::from(fh.height);
        // Validate against the shared 1..=16384 bound; VP8's own 14-bit fields
        // already cap each side at 16383.
        Dimensions::new(width, height).map_err(|_| Error::InvalidBitstream {
            codec: Codec::Lossy,
        })?;
        let mb_w = (usize::try_from(width).unwrap_or(0) + 15) >> 4;
        let mb_h = (usize::try_from(height).unwrap_or(0) + 15) >> 4;
        Ok(Self {
            mb_w,
            mb_h,
            segment: SegmentHeader {
                use_segment: false,
                update_map: false,
                absolute_delta: true,
                quantizer: [0; NUM_MB_SEGMENTS],
                filter_strength: [0; NUM_MB_SEGMENTS],
            },
            filter: FilterHeader::default(),
            filter_type: 0,
            proba: Proba {
                segments: [255; crate::lossy::constants::MB_FEATURE_TREE_PROBS],
                bands: [[[[0; 11]; 3]; 8]; 4],
                use_skip: false,
                skip_p: 0,
            },
            dqm: [QuantMatrix::default(); NUM_MB_SEGMENTS],
            num_parts: 1,
            mb_data: (0..mb_w).map(|_| MbData::default()).collect(),
            intra_t: vec![B_DC_PRED; 4 * mb_w],
            intra_l: [B_DC_PRED; 4],
            mb_info: vec![MbContext::default(); mb_w + 1],
        })
    }

    /// Reset the per-row left contexts before decoding the next macroblock row.
    pub(crate) fn init_scanline(&mut self) {
        self.mb_info[0] = MbContext::default();
        self.intra_l = [B_DC_PRED; 4];
    }

    /// Test-only constructor: frame state for an `mb_w` × `mb_h` grid with the
    /// same field defaults as [`Frame::new`], bypassing header parsing so unit
    /// tests can drive the individual parse methods directly.
    #[cfg(test)]
    pub(crate) fn test_frame(mb_w: usize, mb_h: usize) -> Self {
        Self {
            mb_w,
            mb_h,
            segment: SegmentHeader {
                use_segment: false,
                update_map: false,
                absolute_delta: true,
                quantizer: [0; NUM_MB_SEGMENTS],
                filter_strength: [0; NUM_MB_SEGMENTS],
            },
            filter: FilterHeader::default(),
            filter_type: 0,
            proba: Proba {
                segments: [255; crate::lossy::constants::MB_FEATURE_TREE_PROBS],
                bands: [[[[0; 11]; 3]; 8]; 4],
                use_skip: false,
                skip_p: 0,
            },
            dqm: [QuantMatrix::default(); NUM_MB_SEGMENTS],
            num_parts: 1,
            mb_data: (0..mb_w).map(|_| MbData::default()).collect(),
            intra_t: vec![B_DC_PRED; 4 * mb_w],
            intra_l: [B_DC_PRED; 4],
            mb_info: vec![MbContext::default(); mb_w + 1],
        }
    }
}

/// Parse and reconstruct a VP8 key-frame `payload` into padded, loop-filtered
/// Y/U/V planes plus the picture dimensions, stopping short of YUV→RGB. This is
/// the reconstruction core shared by [`decode_frame`] and the in-crate Level-A
/// oracle (which compares these planes to libwebp's `WebPDecodeYUV`).
///
/// # Errors
///
/// [`Error::Truncated`] / [`Error::InvalidBitstream`] for a malformed stream.
pub(crate) fn reconstruct_to_planes(payload: &[u8]) -> Result<(Planes, usize, usize)> {
    let fh = FrameHeader::parse_key_frame(payload)?;
    let (width, height) = (usize::from(fh.width), usize::from(fh.height));
    let mut frame = Frame::new(fh)?;

    // Split off the compressed control partition (partition 0), then the token
    // partitions that follow it.
    let after_header = payload
        .get(KEY_FRAME_HEADER_LEN..)
        .ok_or(Error::Truncated)?;
    let part0_len = usize::try_from(fh.first_partition_size).unwrap_or(usize::MAX);
    let part0 = after_header.get(..part0_len).ok_or(Error::Truncated)?;
    let after_part0 = &after_header[part0_len..];

    let mut br = BoolDecoder::new(part0);
    frame.parse_headers(&mut br);
    let token_partitions = frame.parse_partitions(&mut br, after_part0)?;
    frame.parse_quant(&mut br);
    let _update_proba = br.read_flag(); // value is ignored on a key frame
    frame.parse_proba(&mut br);

    // Reconstruction state: padded planes, per-segment filter strengths, and the
    // resolved per-macroblock filter info.
    let mut planes = Planes::new(frame.mb_w, frame.mb_h);
    let fstrengths = reconstruct::compute_fstrengths(&frame.segment, &frame.filter);
    let use_skip = frame.proba.use_skip;
    let mut finfo = vec![FInfo::default(); frame.mb_w * frame.mb_h];

    // Walk the macroblock grid: parse each row's modes (partition 0) and residual
    // coefficients (the row's token partition), then reconstruct the row.
    let mut token_brs: Vec<BoolDecoder<'_>> = token_partitions
        .iter()
        .copied()
        .map(BoolDecoder::new)
        .collect();
    let part_mask = frame.num_parts - 1;
    for mb_y in 0..frame.mb_h {
        frame.parse_intra_mode_row(&mut br);
        let token_br = &mut token_brs[mb_y & part_mask];
        for mb_x in 0..frame.mb_w {
            if frame.mb_data[mb_x].skip {
                frame.skip_residuals(mb_x);
            } else {
                frame.parse_residuals(token_br, mb_x);
            }
        }
        for mb_x in 0..frame.mb_w {
            let block = &frame.mb_data[mb_x];
            reconstruct::reconstruct_mb(&mut planes, block, mb_x, mb_y, frame.mb_w);
            finfo[mb_y * frame.mb_w + mb_x] =
                reconstruct::resolve_finfo(fstrengths, block, use_skip);
        }
        frame.init_scanline();
    }

    reconstruct::filter_frame(
        &mut planes,
        &finfo,
        frame.mb_w,
        frame.mb_h,
        frame.filter_type,
    );
    Ok((planes, width, height))
}

/// Decode a VP8 key-frame `payload` (the raw contents of a WebP `VP8 ` chunk)
/// into an RGBA [`Image`].
///
/// # Errors
///
/// [`Error::Truncated`] / [`Error::InvalidBitstream`] for a malformed stream.
pub(crate) fn decode_frame(payload: &[u8]) -> Result<Image> {
    let (planes, width, height) = reconstruct_to_planes(payload)?;
    reconstruct::to_image(&planes, width, height)
}

/// Parse just far enough into `payload` to report whether the frame codes
/// per-macroblock skip (`proba.use_skip`); `None` for a malformed stream. The
/// differential oracle uses this to prove a skip test is non-vacuous — that the
/// stream really exercises the skip decode path.
#[cfg(feature = "oracle")]
pub(crate) fn frame_uses_skip(payload: &[u8]) -> Option<bool> {
    let fh = FrameHeader::parse_key_frame(payload).ok()?;
    let mut frame = Frame::new(fh).ok()?;
    let after_header = payload.get(KEY_FRAME_HEADER_LEN..)?;
    let part0_len = usize::try_from(fh.first_partition_size).unwrap_or(usize::MAX);
    let part0 = after_header.get(..part0_len)?;
    let after_part0 = &after_header[part0_len..];
    let mut br = BoolDecoder::new(part0);
    frame.parse_headers(&mut br);
    frame.parse_partitions(&mut br, after_part0).ok()?;
    frame.parse_quant(&mut br);
    let _update_proba = br.read_flag();
    frame.parse_proba(&mut br);
    Some(frame.proba.use_skip)
}

/// Parse just the filter header of `payload` to report the in-loop filter level
/// (`0` disables filtering); `None` for a malformed stream. The differential
/// oracle uses this to prove a filtered-encode test is non-vacuous — that the
/// stream really carries a non-zero deblocking filter.
#[cfg(feature = "oracle")]
pub(crate) fn frame_filter_level(payload: &[u8]) -> Option<i32> {
    let fh = FrameHeader::parse_key_frame(payload).ok()?;
    let mut frame = Frame::new(fh).ok()?;
    let after_header = payload.get(KEY_FRAME_HEADER_LEN..)?;
    let part0_len = usize::try_from(fh.first_partition_size).unwrap_or(usize::MAX);
    let part0 = after_header.get(..part0_len)?;
    let mut br = BoolDecoder::new(part0);
    frame.parse_headers(&mut br);
    Some(frame.filter.level)
}

/// Parse `payload`'s control partition — headers plus every macroblock row's intra
/// modes — to report whether any macroblock is coded as intra-4×4 (`B_PRED`);
/// `None` for a malformed stream. Intra-mode parsing lives entirely in partition 0,
/// so no residual token partition is touched. The differential oracle uses this to
/// prove an i4x4-encode test is non-vacuous (the stream really exercises the i4x4
/// luma path).
#[cfg(feature = "oracle")]
pub(crate) fn frame_uses_i4x4(payload: &[u8]) -> Option<bool> {
    let fh = FrameHeader::parse_key_frame(payload).ok()?;
    let mut frame = Frame::new(fh).ok()?;
    let after_header = payload.get(KEY_FRAME_HEADER_LEN..)?;
    let part0_len = usize::try_from(fh.first_partition_size).unwrap_or(usize::MAX);
    let part0 = after_header.get(..part0_len)?;
    let after_part0 = &after_header[part0_len..];
    let mut br = BoolDecoder::new(part0);
    frame.parse_headers(&mut br);
    frame.parse_partitions(&mut br, after_part0).ok()?;
    frame.parse_quant(&mut br);
    let _update_proba = br.read_flag();
    frame.parse_proba(&mut br);
    // The per-row `mb_data` is reused, so scan each row's modes as it is parsed.
    for _mb_y in 0..frame.mb_h {
        frame.parse_intra_mode_row(&mut br);
        if frame.mb_data[..frame.mb_w].iter().any(|d| d.is_i4x4) {
            return Some(true);
        }
        frame.init_scanline();
    }
    Some(false)
}

/// Parse `payload`'s control partition and every macroblock's intra modes to report
/// the number of distinct macroblock segments the frame actually uses (`1` when
/// segmentation is off or a single segment is coded); `None` for a malformed stream.
/// The differential oracle uses this to prove a segmented-encode test is non-vacuous
/// (the stream really partitions the macroblocks into multiple quantizer segments).
#[cfg(feature = "oracle")]
pub(crate) fn frame_segment_count(payload: &[u8]) -> Option<usize> {
    let fh = FrameHeader::parse_key_frame(payload).ok()?;
    let mut frame = Frame::new(fh).ok()?;
    let after_header = payload.get(KEY_FRAME_HEADER_LEN..)?;
    let part0_len = usize::try_from(fh.first_partition_size).unwrap_or(usize::MAX);
    let part0 = after_header.get(..part0_len)?;
    let after_part0 = &after_header[part0_len..];
    let mut br = BoolDecoder::new(part0);
    frame.parse_headers(&mut br);
    frame.parse_partitions(&mut br, after_part0).ok()?;
    frame.parse_quant(&mut br);
    let _update_proba = br.read_flag();
    frame.parse_proba(&mut br);
    if !frame.segment.use_segment {
        return Some(1);
    }
    let mut seen = [false; NUM_MB_SEGMENTS];
    for _mb_y in 0..frame.mb_h {
        frame.parse_intra_mode_row(&mut br);
        for d in &frame.mb_data[..frame.mb_w] {
            seen[usize::from(d.segment)] = true;
        }
        frame.init_scanline();
    }
    Some(seen.iter().filter(|&&b| b).count())
}
