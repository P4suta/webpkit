//! Push-based, suspend/resume VP8 key-frame decoding — the row-streaming core.
//!
//! [`Vp8Stream`] decodes a VP8 `payload` a chunk at a time, mirroring the `lossless` codec's
//! `Vp8lStream`: each [`Vp8Stream::advance`] consumes the currently-buffered
//! prefix and reports how far it got. It is driven by the public
//! [`crate::lossy::decoder::IncrementalDecoder`]; the pixels it produces are
//! **byte-for-byte identical** to the one-shot [`crate::lossy::decode::decode_frame`].
//!
//! Byte-identity rests on four facts (all verified against the one-shot path and
//! libwebp's incremental decoder `dec/idec_dec.c` / `dec/frame_dec.c`):
//!
//! 1. **Partition 0 is length-delimited.** The header phase waits (pure byte
//!    counting) until the whole control partition and the token-partition size
//!    table are buffered, so header parsing is decided by real bytes, never
//!    padding. Partition 0 also carries every row's intra modes, and since it is
//!    fully buffered it never suspends.
//! 2. **The token boolean decoder is resumable.** VP8's arithmetic decoder can't
//!    seek by bit offset like the lossless `BitReader`, so instead we snapshot
//!    its small [`crate::lossy::bool_dec::BoolState`] (plus the two macroblock non-zero
//!    context words a residual read mutates) before each macroblock, optimistically
//!    decode, and restore + suspend if it ran past the buffered bytes — libwebp's
//!    `SaveContext`/`RestoreContext`. Because the partition bytes are append-only,
//!    re-decoding after more arrive reads the same bits.
//! 3. **Reconstruction reads only unfiltered neighbors** (no `yuv_t` cache), so
//!    the in-loop filter is *deferred one macroblock row*: after reconstructing
//!    row `mb_y` we filter row `mb_y - 1`, whose bottom edge this row's
//!    reconstruction has now consumed. The resulting filter order `0, 1, 2, …` on
//!    the shared planes is identical to the one-shot whole-frame filter pass.
//! 4. **Padding is observational only.** A read past the buffer sets an `exhausted`
//!    latch but leaves the decoded value unchanged (`value |= 0`), so a truncated
//!    final buffer commits the same zero-padded image the one-shot path returns —
//!    the stream never turns token truncation into an error (unlike the lossless
//!    codec, whose bit reader fails hard at end-of-stream).

use crate::{Error, Result};

use crate::lossy::bool_dec::{BoolDecoder, BoolState};
use crate::lossy::decode::Frame;
use crate::lossy::frame_header::{FrameHeader, KEY_FRAME_HEADER_LEN};
use crate::lossy::loop_filter::FInfo;
use crate::lossy::prelude::*;
use crate::lossy::reconstruct::{self, Planes};
use crate::lossy::yuv::{Yuv420Ref, upsample_output_row};

/// What one [`Vp8Stream::advance`] achieved on the buffered prefix.
pub(crate) enum Step {
    /// The prefix was consumed without finishing anything reportable; feed more.
    NeedMore,
    /// The header is now known (reported exactly once, before any rows).
    Header {
        /// Picture width in pixels.
        width: u32,
        /// Picture height in pixels.
        height: u32,
    },
    /// Output rows `first_row..first_row + count` were finalized this call and are
    /// available (RGBA8, `width * 4` bytes each) via [`Vp8Stream::ready`].
    Rows {
        /// 0-based index of the first newly-finalized output row.
        first_row: u32,
        /// Number of newly-finalized output rows.
        count: u32,
    },
    /// Every pixel is decoded; retrieve them with [`Vp8Stream::into_pixels`].
    Done,
}

/// A push-based VP8 key-frame decoder.
pub(crate) struct Vp8Stream {
    state: Phase,
}

/// The decoder's coarse phase. Boxed streaming state keeps the enum small.
enum Phase {
    /// Accumulating bytes until the whole header + size table is buffered.
    ParsingHeaders,
    /// Reconstructing macroblock rows, resumable across pushes.
    Streaming(Box<StreamState>),
    /// Finished: owns the complete RGBA8 pixel buffer.
    Done(Vec<u8>),
}

/// All state needed to resume row reconstruction between pushes. Every field is
/// owned (no borrow of the payload), so the whole struct survives across calls;
/// the payload slices are re-derived and the boolean decoders rebuilt each push.
struct StreamState {
    /// Frame-persistent decoder state (headers, probabilities, per-row modes).
    frame: Frame,
    /// The padded reconstruction planes (allocated once).
    planes: Planes,
    /// Per-segment / per-mode filter strengths.
    fstrengths: [[FInfo; 2]; 4],
    /// 0 off, 1 simple, 2 normal.
    filter_type: u8,
    /// Whether the skip probability is in use (feeds `resolve_finfo`).
    use_skip: bool,
    /// Picture width in pixels.
    width: usize,
    /// Picture height in pixels.
    height: usize,

    /// Byte range of partition 0 within the payload (`[start, end)`), fully
    /// buffered; the intra-mode reader resumes from here every row.
    part0_range: (usize, usize),
    /// Partition-0 boolean-decoder state after the header, advanced per row.
    part0_state: BoolState,

    /// Absolute payload start offset of each token partition (fixed once known).
    part_start: Vec<usize>,
    /// Declared end offset of each non-final token partition; `None` for the last
    /// partition, which runs to the end of the payload.
    part_declared_end: Vec<Option<usize>>,
    /// Per-partition resumable boolean-decoder state; `None` until first primed.
    token_state: Vec<Option<BoolState>>,

    /// Next macroblock row to reconstruct.
    mb_y: usize,
    /// Next macroblock column whose residuals to decode (row resume point).
    mb_x: usize,
    /// Whether the current row's intra modes have been parsed already.
    intra_done: bool,

    /// The just-reconstructed row's resolved filter info (awaiting its deferred
    /// filter pass on the next row).
    finfo_prev: Vec<FInfo>,
    /// Scratch filter info for the row being reconstructed.
    finfo_cur: Vec<FInfo>,
    /// Highest macroblock row already in-loop filtered (`None` if none yet).
    filtered_through: Option<usize>,

    /// Finalized RGBA8 output rows, row-major (`width * 4` per row).
    ready: Vec<u8>,
    /// Number of output rows already appended to `ready`.
    out_rows_done: u32,
}

impl StreamState {
    /// Chroma plane height in rows (`ceil(height / 2)`).
    const fn chroma_height(&self) -> usize {
        self.height.div_ceil(2)
    }
}

impl Vp8Stream {
    /// A fresh stream, parked in the header phase.
    pub(crate) const fn new() -> Self {
        Self {
            state: Phase::ParsingHeaders,
        }
    }

    /// Consume the buffered `payload` prefix. `final_input` is true only when
    /// `payload` is the whole (possibly truncated) VP8 chunk and no more bytes
    /// will arrive, which turns "ran off the buffer" from a suspend into a commit
    /// (the one-shot decoder likewise zero-pads a truncated token stream).
    pub(crate) fn advance(&mut self, payload: &[u8], final_input: bool) -> Result<Step> {
        // Take ownership of the phase here (leaving a placeholder), so the streaming
        // path receives its `StreamState` by value — the phase is destructured in
        // exactly one place and `advance_streaming` can never be reached in the wrong
        // phase.
        match core::mem::replace(&mut self.state, Phase::ParsingHeaders) {
            Phase::ParsingHeaders => self.advance_parsing(payload, final_input),
            Phase::Streaming(ss) => self.advance_streaming(ss, payload, final_input),
            Phase::Done(pixels) => {
                self.state = Phase::Done(pixels);
                Ok(Step::Done)
            },
        }
    }

    /// The finalized output rows so far (RGBA8, row-major). A [`Step::Rows`] report
    /// names which slice became available; the driver reads it here.
    pub(crate) fn ready(&self) -> &[u8] {
        match &self.state {
            Phase::Streaming(ss) => &ss.ready,
            Phase::Done(pixels) => pixels,
            Phase::ParsingHeaders => &[],
        }
    }

    /// The complete pixel buffer, once [`Step::Done`] was reported.
    #[cfg(test)]
    pub(crate) fn into_pixels(self) -> Option<Vec<u8>> {
        match self.state {
            Phase::Done(pixels) => Some(pixels),
            _ => None,
        }
    }

    /// The token-partition layout `advance_parsing` derived from the size table
    /// (partition count, absolute start offsets, declared end offsets), or `None`
    /// before the header is parsed. Lets a test pin the size-table arithmetic
    /// directly, independent of how the (garbage) residuals happen to reconstruct.
    #[cfg(test)]
    pub(crate) fn token_layout(&self) -> Option<(usize, Vec<usize>, Vec<Option<usize>>)> {
        match &self.state {
            Phase::Streaming(ss) => Some((
                ss.frame.num_parts,
                ss.part_start.clone(),
                ss.part_declared_end.clone(),
            )),
            _ => None,
        }
    }

    /// Header phase: a pure byte-count gate (no arithmetic decoding decides
    /// suspension) that, once the control partition and token size table are
    /// buffered, sets up the streaming state and reports the dimensions once.
    fn advance_parsing(&mut self, payload: &[u8], final_input: bool) -> Result<Step> {
        // (1) The 10-byte uncompressed key-frame header.
        if payload.len() < KEY_FRAME_HEADER_LEN {
            return short_or_truncated(final_input);
        }
        let fh = FrameHeader::parse_key_frame(payload)?;
        let part0_len = usize::try_from(fh.first_partition_size).unwrap_or(usize::MAX);

        // (2) The whole control partition (partition 0) must be present.
        let Some(part0_end) = KEY_FRAME_HEADER_LEN
            .checked_add(part0_len)
            .filter(|&e| payload.len() >= e)
        else {
            return short_or_truncated(final_input);
        };
        let part0 = &payload[KEY_FRAME_HEADER_LEN..part0_end];

        let mut frame = Frame::new(fh)?;
        let mut br = BoolDecoder::new(part0);
        frame.parse_headers(&mut br);

        // Read the partition count (2 bits) exactly as `parse_partitions` does,
        // then derive each token partition's absolute byte range from the size
        // table rather than slicing — the slice would be short until buffered.
        let num_parts = 1usize << br.read_literal(2);
        frame.num_parts = num_parts;
        let last = num_parts - 1;
        let base = part0_end;

        // (3) The size table: 3 bytes per non-final partition.
        let Some(table_end) = base.checked_add(3 * last).filter(|&e| payload.len() >= e) else {
            return short_or_truncated(final_input);
        };
        let mut part_start = Vec::with_capacity(num_parts);
        let mut part_declared_end = Vec::with_capacity(num_parts);
        let mut cur = table_end;
        for p in 0..last {
            let sz = usize::from(payload[base + 3 * p])
                | usize::from(payload[base + 3 * p + 1]) << 8
                | usize::from(payload[base + 3 * p + 2]) << 16;
            part_start.push(cur);
            let end = cur.saturating_add(sz);
            part_declared_end.push(Some(end));
            cur = end;
        }
        part_start.push(cur);
        part_declared_end.push(None);

        // Consume the rest of partition 0 in the one-shot order.
        frame.parse_quant(&mut br);
        let _update_proba = br.read_flag();
        frame.parse_proba(&mut br);

        let planes = Planes::new(frame.mb_w, frame.mb_h);
        let fstrengths = reconstruct::compute_fstrengths(&frame.segment, &frame.filter);
        let (use_skip, filter_type, mb_w) = (frame.proba.use_skip, frame.filter_type, frame.mb_w);
        let ss = StreamState {
            part0_range: (KEY_FRAME_HEADER_LEN, part0_end),
            part0_state: br.state(),
            part_start,
            part_declared_end,
            token_state: vec![None; num_parts],
            mb_y: 0,
            mb_x: 0,
            intra_done: false,
            finfo_prev: vec![FInfo::default(); mb_w],
            finfo_cur: vec![FInfo::default(); mb_w],
            filtered_through: None,
            ready: Vec::new(),
            out_rows_done: 0,
            filter_type,
            use_skip,
            width: usize::from(fh.width),
            height: usize::from(fh.height),
            fstrengths,
            planes,
            frame,
        };
        self.state = Phase::Streaming(Box::new(ss));
        Ok(Step::Header {
            width: u32::from(fh.width),
            height: u32::from(fh.height),
        })
    }

    /// Streaming phase: reconstruct as many macroblock rows as the buffered token
    /// partitions allow, suspending mid-row if one runs dry, and emit each output
    /// row the instant the rolling filter and chroma upsampler have finalized it.
    fn advance_streaming(
        &mut self,
        mut ss: Box<StreamState>,
        payload: &[u8],
        final_input: bool,
    ) -> Result<Step> {
        let mb_w = ss.frame.mb_w;
        let mb_h = ss.frame.mb_h;
        let mask = ss.frame.num_parts - 1;
        let rows_before = ss.out_rows_done;

        loop {
            // All rows reconstructed: run the deferred final filter, drain, finish.
            if ss.mb_y == mb_h {
                if ss.filter_type != 0 && ss.filtered_through != Some(mb_h - 1) {
                    reconstruct::filter_mb_row(
                        &mut ss.planes,
                        &ss.finfo_prev,
                        mb_h - 1,
                        ss.filter_type,
                    );
                    ss.filtered_through = Some(mb_h - 1);
                }
                emit_finalized_rows(&mut ss);
                let new = ss.out_rows_done - rows_before;
                if new > 0 {
                    self.state = Phase::Streaming(ss);
                    return Ok(Step::Rows {
                        first_row: rows_before,
                        count: new,
                    });
                }
                let pixels = core::mem::take(&mut ss.ready);
                self.state = Phase::Done(pixels);
                return Ok(Step::Done);
            }

            let mb_y = ss.mb_y;

            // (a) Intra modes: partition 0 is fully buffered, so this never suspends.
            if !ss.intra_done {
                let (a, b) = ss.part0_range;
                let mut br0 = BoolDecoder::resume(&payload[a..b], ss.part0_state);
                ss.frame.parse_intra_mode_row(&mut br0);
                ss.part0_state = br0.state();
                ss.intra_done = true;
            }

            // (b) Residuals from this row's token partition (suspendable).
            let p = mb_y & mask;
            let start = ss.part_start[p].min(payload.len());
            let end = ss.part_declared_end[p].map_or(payload.len(), |e| e.min(payload.len()));
            let slice = &payload[start..end];
            // Whether the partition's declared window is fully buffered (or this is
            // the final input): a read into padding is then the same padding the
            // one-shot decoder sees, so we commit instead of suspending.
            let window_final =
                final_input || ss.part_declared_end[p].map_or(final_input, |e| payload.len() >= e);
            let mut token_br = match ss.token_state[p] {
                Some(st) => BoolDecoder::resume(slice, st),
                None => {
                    if slice.len() >= 2 || window_final {
                        // Prime once from the (stable) first two bytes.
                        BoolDecoder::new(slice)
                    } else {
                        let new = ss.out_rows_done - rows_before;
                        self.state = Phase::Streaming(ss);
                        return Ok(rows_or_needmore(rows_before, new));
                    }
                },
            };

            let mut suspended = false;
            while ss.mb_x < mb_w {
                let mb_x = ss.mb_x;
                // A skipped macroblock carries no coefficient tokens, so it reads
                // nothing from the partition — it can never suspend and needs no
                // SaveContext snapshot. Clear it and advance (mirrors the one-shot
                // decoder's `skip_residuals` branch).
                if ss.frame.mb_data[mb_x].skip {
                    ss.frame.skip_residuals(mb_x);
                    ss.mb_x += 1;
                    continue;
                }
                // SaveContext: the boolean state plus the two non-zero context
                // words `parse_residuals` mutates (coeffs are memset on entry, so
                // they need no snapshot).
                let pre_br = token_br.state();
                let pre_top = ss.frame.mb_info[mb_x + 1];
                let pre_left = ss.frame.mb_info[0];
                ss.frame.parse_residuals(&mut token_br, mb_x);
                if token_br.is_exhausted() && !window_final {
                    // Ran past real bytes before the window filled: RestoreContext.
                    ss.frame.mb_info[mb_x + 1] = pre_top;
                    ss.frame.mb_info[0] = pre_left;
                    ss.token_state[p] = Some(pre_br);
                    suspended = true;
                    break;
                }
                ss.mb_x += 1;
            }
            if suspended {
                let new = ss.out_rows_done - rows_before;
                self.state = Phase::Streaming(ss);
                return Ok(rows_or_needmore(rows_before, new));
            }
            ss.token_state[p] = Some(token_br.state());

            // (c) Reconstruct the row and (d) run the deferred filter of the row
            // above, whose bottom edge this row's reconstruction has now read.
            reconstruct_and_filter_row(&mut ss, mb_y, mb_w);

            // (e) Row complete: reset left contexts, advance the cursor.
            ss.frame.init_scanline();
            ss.mb_y += 1;
            ss.mb_x = 0;
            ss.intra_done = false;

            // (f) Emit whichever output rows the filter + upsampler just finalized.
            emit_finalized_rows(&mut ss);
        }
    }
}

/// `NeedMore`, or `Rows` if this call already finalized some output rows.
const fn rows_or_needmore(first_row: u32, new_rows: u32) -> Step {
    if new_rows > 0 {
        Step::Rows {
            first_row,
            count: new_rows,
        }
    } else {
        Step::NeedMore
    }
}

/// A header-phase byte shortfall: `Truncated` if no more input is coming, else
/// `NeedMore`.
const fn short_or_truncated(final_input: bool) -> Result<Step> {
    if final_input {
        Err(Error::Truncated)
    } else {
        Ok(Step::NeedMore)
    }
}

/// Reconstruct macroblock row `mb_y` from unfiltered neighbors, then run the
/// deferred in-loop filter of the row above it (whose bottom edge this row's
/// reconstruction has now consumed) and rotate the filter-info scratch buffers.
fn reconstruct_and_filter_row(ss: &mut StreamState, mb_y: usize, mb_w: usize) {
    for mb_x in 0..mb_w {
        reconstruct::reconstruct_mb(&mut ss.planes, &ss.frame.mb_data[mb_x], mb_x, mb_y, mb_w);
        ss.finfo_cur[mb_x] =
            reconstruct::resolve_finfo(ss.fstrengths, &ss.frame.mb_data[mb_x], ss.use_skip);
    }
    if ss.filter_type != 0 && mb_y >= 1 {
        reconstruct::filter_mb_row(&mut ss.planes, &ss.finfo_prev, mb_y - 1, ss.filter_type);
        ss.filtered_through = Some(mb_y - 1);
    }
    core::mem::swap(&mut ss.finfo_prev, &mut ss.finfo_cur);
}

/// Append every output row that the rolling filter and chroma upsampler have now
/// finalized, given the current reconstruction / filter progress.
fn emit_finalized_rows(ss: &mut StreamState) {
    let (luma_end, chroma_end) = finalized_plane_rows(
        ss.filter_type,
        ss.filtered_through,
        ss.mb_y,
        ss.frame.mb_h,
        ss.height,
        ss.chroma_height(),
    );
    let (width, height, chroma_height) = (ss.width, ss.height, ss.chroma_height());
    let row_bytes = width * 4;
    while (ss.out_rows_done as usize) < height {
        let y = ss.out_rows_done as usize;
        if y >= luma_end {
            break;
        }
        // The highest chroma row output row `y` reads (clamped for the mirrored
        // last row): all must be finalized before `y` can be emitted.
        let cmax = y.div_ceil(2).min(chroma_height - 1);
        if cmax >= chroma_end {
            break;
        }
        ss.ready.resize((y + 1) * row_bytes, 0);
        let y0 = ss.planes.y_stride + 1;
        let uv0 = ss.planes.uv_stride + 1;
        let src = Yuv420Ref {
            y: &ss.planes.y[y0..],
            y_stride: ss.planes.y_stride,
            u: &ss.planes.u[uv0..],
            v: &ss.planes.v[uv0..],
            uv_stride: ss.planes.uv_stride,
        };
        upsample_output_row(
            &src,
            width,
            height,
            y,
            &mut ss.ready[y * row_bytes..(y + 1) * row_bytes],
        );
        ss.out_rows_done += 1;
    }
}

/// How many top plane rows are finalized, given the filter type, the highest
/// filtered macroblock row `f_idx` (`None` = none yet), and the count `r` of
/// reconstructed macroblock rows. libwebp's `kFilterExtraRows` = `{0, 2, 8}`
/// (luma) / `{0, 1, 4}` (chroma) says how many bottom rows of a filtered row are
/// still provisional until the next row's filter runs; the last row has none.
fn finalized_plane_rows(
    filter_type: u8,
    f_idx: Option<usize>,
    r: usize,
    mb_h: usize,
    height: usize,
    chroma_height: usize,
) -> (usize, usize) {
    if filter_type == 0 {
        // No filter: a row is final as soon as it is reconstructed.
        return ((16 * r).min(height), (8 * r).min(chroma_height));
    }
    let extra = if filter_type == 1 { 2 } else { 8 };
    match f_idx {
        None => (0, 0),
        Some(k) if k == mb_h - 1 => (height, chroma_height),
        Some(k) => {
            let luma = (16 * (k + 1)).saturating_sub(extra).min(height);
            let chroma = if filter_type == 1 {
                // Simple filter leaves chroma untouched: final on reconstruction.
                (8 * r).min(chroma_height)
            } else {
                (8 * (k + 1)).saturating_sub(4).min(chroma_height)
            };
            (luma, chroma)
        },
    }
}

/// Drive a [`Vp8Stream`] over `payload`, feeding growing prefixes cut at `cuts`
/// (a non-decreasing list of byte offsets; the full payload is always fed last
/// with `final_input = true`). Returns the complete pixel buffer, asserting the
/// reported [`Step::Rows`] are contiguous from row 0.
#[cfg(test)]
fn stream_over_splits(payload: &[u8], cuts: &[usize]) -> Result<Vec<u8>> {
    let len = payload.len();
    let mut stream = Vp8Stream::new();
    let mut rows_reported = 0u32;
    // Feed each growing prefix, then a guaranteed final whole-payload feed so the
    // stream resolves regardless of `cuts`.
    let boundaries = cuts.iter().copied().chain(core::iter::once(len));
    'outer: for cut in boundaries {
        let cut = cut.min(len);
        let final_input = cut == len;
        loop {
            match stream.advance(&payload[..cut], final_input)? {
                Step::Header { .. } => {},
                Step::Rows { first_row, count } => {
                    assert_eq!(first_row, rows_reported, "Rows payout is not contiguous");
                    rows_reported += count;
                },
                Step::NeedMore => break,
                Step::Done => break 'outer,
            }
        }
    }
    stream.into_pixels().ok_or(Error::Truncated)
}

/// Canonical prefix-cut patterns for `stream_over_splits`: whole-at-once, one
/// byte at a time, halves, and thirds.
#[cfg(test)]
fn split_patterns(len: usize) -> Vec<Vec<usize>> {
    let mut pats = vec![Vec::new()];
    if len > 1 {
        pats.push((1..len).collect());
        pats.push(vec![len / 2]);
        pats.push(vec![len / 3, 2 * len / 3]);
    }
    pats
}

#[cfg(test)]
mod tests {
    use super::{Step, Vp8Stream, finalized_plane_rows, split_patterns, stream_over_splits};
    use crate::lossy::decode;
    use crate::lossy::prelude::*;
    use crate::lossy::{Dimensions, Effort, ImageRef, LossyConfig, PixelLayout, encode_vp8};

    /// A byte from a pattern value, wrapping into `0..=255` (no lossy cast).
    fn byte(v: u32) -> u8 {
        u8::try_from(v & 0xff).unwrap_or(0)
    }

    /// Build a `width`×`height` RGBA buffer from a per-pixel RGB function.
    fn rgba_image(width: u32, height: u32, f: impl Fn(u32, u32) -> [u8; 3]) -> Vec<u8> {
        let mut buf = Vec::new();
        for y in 0..height {
            for x in 0..width {
                let [r, g, b] = f(x, y);
                buf.extend_from_slice(&[r, g, b, 0xff]);
            }
        }
        buf
    }

    /// Encode `rgba` to a raw `VP8 ` payload at the given effort and quality.
    fn encode_payload(
        rgba: &[u8],
        width: u32,
        height: u32,
        effort: Effort,
        quality: u8,
    ) -> Vec<u8> {
        let dims = Dimensions::new(width, height).unwrap();
        let img = ImageRef::new(dims, PixelLayout::Rgba8, rgba).unwrap();
        let cfg = LossyConfig::new().with_quality(quality).with_effort(effort);
        encode_vp8(img, &cfg).unwrap().1
    }

    /// Every committed fixture is a real (loop-filtered, fancy-upsampled) VP8
    /// stream; streaming it in any split must reproduce the one-shot pixels byte
    /// for byte. Odd sizes (17x13, 5x9) exercise the crop + chroma-mirror edges.
    const FIXTURES: &[&[u8]] = &[
        include_bytes!("../../tests/fixtures/checker_16x16_q20.vp8"),
        include_bytes!("../../tests/fixtures/gradient_17x13_q80.vp8"),
        include_bytes!("../../tests/fixtures/noise_32x24_q30.vp8"),
        include_bytes!("../../tests/fixtures/noise_5x9_q50.vp8"),
    ];

    /// One-shot RGBA bytes for `payload`, for equivalence comparisons.
    fn one_shot(payload: &[u8]) -> crate::Result<Vec<u8>> {
        decode::decode_frame(payload).map(|img| img.as_bytes().to_vec())
    }

    /// Assert streaming `payload` in every split pattern matches the one-shot
    /// decode byte-for-byte (or errors identically).
    fn assert_stream_equivalence(payload: &[u8]) {
        let expected = one_shot(payload);
        for cuts in split_patterns(payload.len()) {
            let streamed = stream_over_splits(payload, &cuts);
            match (&expected, &streamed) {
                (Ok(a), Ok(b)) => assert_eq!(a, b, "pixels differ for cuts {cuts:?}"),
                (Err(e), Err(f)) => assert_eq!(e, f, "errors differ for cuts {cuts:?}"),
                (a, b) => panic!("stream/one-shot disagree (cuts {cuts:?}): {a:?} vs {b:?}"),
            }
        }
    }

    /// Assert streaming `payload` with a specific (non-decreasing) `cuts` list
    /// agrees with the one-shot decode.
    fn assert_cuts_match(payload: &[u8], cuts: &[usize]) {
        match (one_shot(payload), stream_over_splits(payload, cuts)) {
            (Ok(a), Ok(b)) => assert_eq!(a, b, "pixels differ for cuts {cuts:?}"),
            (Err(e), Err(f)) => assert_eq!(e, f, "errors differ for cuts {cuts:?}"),
            (a, b) => panic!("stream/one-shot disagree (cuts {cuts:?}): {a:?} vs {b:?}"),
        }
    }

    #[test]
    fn stream_equals_one_shot_on_fixtures() {
        for fixture in FIXTURES {
            assert_stream_equivalence(fixture);
        }
    }

    proptest::proptest! {
        /// Random non-decreasing split points over every fixture: the suspend/resume
        /// boundary can fall anywhere, and streaming must still equal the one-shot
        /// decode byte for byte.
        #[test]
        fn stream_equals_one_shot_over_random_splits(
            idx in 0usize..FIXTURES.len(),
            raw_cuts in proptest::collection::vec(0usize..4096, 0..10),
        ) {
            let mut cuts = raw_cuts;
            cuts.sort_unstable();
            assert_cuts_match(FIXTURES[idx], &cuts);
        }

        /// Arbitrary bytes rarely form a valid frame, but streaming must never
        /// panic and must agree with the one-shot decode (same error, or same
        /// pixels).
        #[test]
        fn stream_never_disagrees_on_arbitrary_bytes(
            data in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..600),
        ) {
            match (one_shot(&data), stream_over_splits(&data, &[])) {
                (Ok(a), Ok(b)) => proptest::prop_assert_eq!(a, b),
                (Err(_), Err(_)) => {}
                (a, b) => proptest::prop_assert!(
                    false, "disagree: one_shot_ok={} streamed_ok={}", a.is_ok(), b.is_ok()
                ),
            }
        }
    }

    #[test]
    fn stream_equals_one_shot_on_minimal_key_frame() {
        // A coefficient-free 16x16 key frame (empty first partition), filter off.
        let header = [0x10u8, 0x00, 0x00, 0x9d, 0x01, 0x2a, 16, 0, 16, 0];
        assert_stream_equivalence(&header);
    }

    #[test]
    fn finalized_plane_rows_reports_exact_finalized_counts() {
        // A direct, deterministic check of the finalization arithmetic that drives
        // `emit_finalized_rows`. libwebp's `kFilterExtraRows` = {0, 2, 8} (luma) /
        // {0, 1, 4} (chroma) says how many bottom rows of a filtered macroblock row
        // stay provisional until the next row's filter runs. `k` is the highest
        // already-filtered macroblock row and `r` the count of reconstructed rows.
        //
        // Filter off: a plane row is final the instant its macroblock row is
        // reconstructed, so `r` rows finalize 16*r luma and 8*r chroma rows.
        assert_eq!(finalized_plane_rows(0, None, 3, 5, 100, 100), (48, 24));
        // Simple filter (type 1): luma trails by 2; chroma is untouched by the simple
        // filter, so it is final on reconstruction (8*r, here r = 4).
        assert_eq!(finalized_plane_rows(1, Some(2), 4, 5, 200, 200), (46, 32));
        // Normal filter (type 2): luma trails by 8, chroma by 4 (from k = 2).
        assert_eq!(finalized_plane_rows(2, Some(2), 4, 5, 200, 200), (40, 20));
    }

    #[test]
    fn stream_matches_one_shot_on_a_filter_off_multi_row_frame() {
        // `Effort::Fast` disables the in-loop filter, so every plane row is final the
        // instant its macroblock row is reconstructed and `emit_finalized_rows` gates
        // purely on the chroma-plane height. On a frame two-plus macroblock rows tall
        // (32×48) with row-varying content, streaming is byte-identical to the one-shot
        // decode only when the chroma-height bound is exact: an under-counted bound
        // emits a bottom output row before the macroblock row supplying its upsampled
        // chroma has been reconstructed, committing that row from unwritten planes.
        let (w, h) = (32u32, 48u32);
        let rgba = rgba_image(w, h, |x, y| [byte(y * 5), byte(x * 3), byte((x + y) * 2)]);
        let payload = encode_payload(&rgba, w, h, Effort::Fast, 80);
        assert_stream_equivalence(&payload);
    }

    #[test]
    fn truncating_a_fixture_matches_one_shot() {
        // Cutting a stream short and declaring the input final must reproduce the
        // one-shot decode of that same truncated payload (zero-padded token data),
        // never an error — VP8's arithmetic decoder pads a short partition.
        for fixture in FIXTURES {
            for keep in [10, fixture.len() / 2, fixture.len() - 1] {
                if keep < 10 || keep >= fixture.len() {
                    continue;
                }
                let truncated = &fixture[..keep];
                assert_stream_equivalence(truncated);
            }
        }
    }

    // ---- crafted multi-partition frames (RFC §9.5 size table) ---------------
    //
    // Our encoder always emits a single token partition, so nothing it produces
    // ever runs `advance_parsing`'s size-table loop or reaches a starved,
    // not-yet-primed *non-final* partition in `advance_streaming`. These helpers
    // hand-build a genuine N-partition key frame: a minimal filter-off control
    // partition whose only load-bearing field is the 2-bit partition count, a size
    // table, and a high-entropy token region. The frame need not be a "real" image
    // — the one-shot decoder (`header::parse_partitions`, unmutated) and the
    // streaming decoder (`advance_parsing`, under test) parse the *identical* bytes,
    // so any offset-arithmetic mutation relocates a partition and diverges the
    // pixels, while an unmutated stream stays byte-identical.

    /// A minimal control partition (partition 0) declaring `1 << count_log2` token
    /// partitions, the in-loop filter off (level 0) and no segmentation — exactly
    /// what `Frame::parse_headers` plus the partition-count read consume. Everything
    /// after (quant / proba / skip) is read from the boolean decoder's zero padding,
    /// so both decoders derive identical default headers.
    fn control_partition(count_log2: u32) -> Vec<u8> {
        let mut enc = crate::lossy::bool_enc::BoolEncoder::new();
        enc.put_flag(false); // color space
        enc.put_flag(false); // clamp type
        enc.put_flag(false); // segmentation off
        enc.put_flag(false); // filter: simple
        enc.put_literal(6, 0); // filter level 0 -> filter type 0 (off)
        enc.put_literal(3, 0); // sharpness
        enc.put_flag(false); // use loop-filter deltas
        enc.put_literal(2, count_log2); // token-partition count
        enc.finish()
    }

    /// The little-endian 3-byte size-table entry for `sz`.
    fn size_entry(sz: usize) -> [u8; 3] {
        [
            u8::try_from(sz & 0xff).unwrap(),
            u8::try_from((sz >> 8) & 0xff).unwrap(),
            u8::try_from((sz >> 16) & 0xff).unwrap(),
        ]
    }

    /// Build a complete `1 << count_log2`-partition VP8 key frame for a
    /// `width`×`height` grid: the control partition, a size table declaring `sizes`
    /// (one entry per non-final token partition), and a high-entropy token region
    /// of `sum(sizes) + final_len` bytes. Returns the payload and the absolute start
    /// offset of every token partition, so a test can cut exactly at a boundary. A
    /// distinct-looking byte at every offset guarantees that any mutated partition
    /// start decodes different residuals.
    fn build_multipart(
        width: u16,
        height: u16,
        count_log2: u32,
        sizes: &[usize],
        final_len: usize,
    ) -> (Vec<u8>, Vec<usize>) {
        let num_parts = 1usize << count_log2;
        assert_eq!(
            sizes.len(),
            num_parts - 1,
            "one size per non-final partition"
        );
        // Pad the control partition with a run of a byte value (`0x5A`) that never
        // occurs in a size-table entry. These bytes are read as (ignored) quant /
        // proba padding by both decoders, but they occupy `[base - k, base)` — the
        // region a `base + 3*p` -> `base - 3*p` (or a `+1/+2` -> `-1/-2`) mutation
        // re-indexes into — so such a mutation can never coincidentally read back
        // the original size byte.
        let mut p0 = control_partition(count_log2);
        p0.resize(p0.len() + 16, 0x5Au8);
        let mut payload = crate::lossy::enc_header::frame_header_bytes(
            u32::try_from(p0.len()).unwrap(),
            width,
            height,
        )
        .to_vec();
        payload.extend_from_slice(&p0);
        for &sz in sizes {
            payload.extend_from_slice(&size_entry(sz));
        }
        let table_end = payload.len(); // == part0_end + 3 * (num_parts - 1)
        let token_total = sizes.iter().sum::<usize>() + final_len;
        for i in 0..token_total {
            // A Weyl hash: every index maps to a scattered byte, so relocating a
            // partition start lands on different residual bytes.
            let h = (i as u64)
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                .rotate_left(29);
            payload.push(u8::try_from((h >> 32) & 0xff).unwrap());
        }
        let mut starts = Vec::with_capacity(num_parts);
        let mut cur = table_end;
        for &sz in sizes {
            starts.push(cur);
            cur += sz;
        }
        starts.push(cur);
        (payload, starts)
    }

    #[test]
    fn multipart_size_table_offsets_are_pinned_exactly() {
        // A real four-partition frame (`count_log2 == 2` -> `1 << 2 == 4`). Every
        // size-table entry carries a distinct non-zero low/mid/high byte, so the
        // little-endian assembly (`byte | byte << 8 | byte << 16`), the per-entry
        // index arithmetic (`base + 3*p`, `+ 1`, `+ 2`) and the partition count
        // (`1 << literal`) are all load-bearing. Rather than trust that a relocated
        // partition perturbs the (near-flat, default-probability) garbage residuals
        // enough to survive rounding, this pins the derived layout *directly*: any
        // mutation to the count read or the size arithmetic yields a wrong
        // partition count / start / declared-end and fails an exact assertion (a
        // `>>` count gives `num_parts == 0` and a `3 / p` divides by zero — both
        // panic in `advance_parsing`).
        let sizes = [86945usize, 91571, 82375]; // bytes A1/53/01, B3/67/01, C7/41/01
        let (payload, starts) = build_multipart(16, 64, 2, &sizes, 512);
        assert!(
            starts[3] < payload.len(),
            "final partition must stay in bounds"
        );

        let mut stream = Vp8Stream::new();
        assert!(
            matches!(
                stream.advance(&payload, false).unwrap(),
                Step::Header { .. }
            ),
            "the full header must parse in one feed"
        );
        let (num_parts, part_start, part_end) = stream
            .token_layout()
            .expect("layout is known after the header");
        assert_eq!(num_parts, 4, "1 << read_literal(2) == 4 token partitions");
        // `starts` is computed independently by `build_multipart` from the same size
        // table, so it is the ground-truth offset of every partition.
        assert_eq!(part_start, starts, "cumulative partition starts");
        assert_eq!(
            part_end,
            vec![Some(starts[1]), Some(starts[2]), Some(starts[3]), None],
            "each non-final partition's declared end is its successor's start"
        );

        // And the layout really drives a byte-identical streamed decode.
        assert!(
            one_shot(&payload).is_ok(),
            "the crafted frame must decode (non-vacuous)"
        );
        assert_cuts_match(&payload, &[]);
    }

    #[test]
    fn streaming_a_skipped_macroblock_advances_the_column() {
        // A flat frame codes every macroblock with `mb_skip_coeff = 1` (all-zero
        // residuals) under `Effort::Balanced` (`use_skip` on), so the streaming
        // decoder takes the skip branch and must ADVANCE `mb_x` past it. Decrementing
        // instead underflows `mb_x` below 0. The frame is one macroblock wide
        // (`mb_w == 1`), so every skip is at column 0: a `-=` underflows `0usize`
        // and panics immediately (a wider frame could instead spin re-decoding a
        // non-skipped column 0 forever), and there is always at least one skip since
        // an interior row predicts from its flat, already-reconstructed neighbour.
        let (w, h) = (16u32, 48u32);
        let rgba = rgba_image(w, h, |_, _| [128, 128, 128]);
        let payload = encode_payload(&rgba, w, h, Effort::Balanced, 90);
        assert_stream_equivalence(&payload);
    }

    #[test]
    fn restarved_partition_reports_no_phantom_rows() {
        // Two partitions, two macroblock rows: row 0 reads partition 0, row 1 reads
        // partition 1. Cutting exactly at partition 1's start leaves it empty and
        // non-final, so once row 0's output rows are emitted the decoder re-enters
        // with `rows_before > 0`, finds partition 1 still un-primed with < 2 bytes,
        // and must report *no* new rows (`out_rows_done - rows_before == 0`). Summing
        // instead would announce phantom rows and break the row-payout contiguity
        // that `stream_over_splits` asserts.
        let (payload, starts) = build_multipart(16, 32, 1, &[8], 64);
        assert_cuts_match(&payload, &[starts[1]]);
    }

    #[test]
    fn a_fully_buffered_nonfinal_partition_commits_its_row() {
        // Partition 0 is declared empty (size 0): row 0 reads it, exhausts it into
        // padding, and — because the partition IS fully buffered even though the
        // input is not yet final — must COMMIT (the padding equals what the one-shot
        // decoder sees) and emit row 0's output. Treating "buffered" as
        // `payload.len() < declared_end` would suspend instead and emit nothing.
        let (payload, starts) = build_multipart(16, 16, 1, &[0], 32);
        let prefix = &payload[..starts[1]]; // header + size table, zero token bytes
        let mut stream = Vp8Stream::new();
        let mut committed = false;
        loop {
            match stream.advance(prefix, false).unwrap() {
                Step::Header { .. } => {},
                Step::Rows { .. } | Step::Done => {
                    committed = true;
                    break;
                },
                Step::NeedMore => break,
            }
        }
        assert!(
            committed,
            "a fully-buffered non-final partition must commit its row"
        );
        assert!(!stream.ready().is_empty(), "row 0 output must be available");
    }

    #[test]
    fn a_truncated_final_partition_commits_padding_not_an_error() {
        // Partition 0 declares 1000 bytes but the payload is cut off right after the
        // size table, so its window is empty and the (final) input is short. The
        // one-shot decoder pads the missing token bytes and returns an image; the
        // streaming decoder must do the same. Requiring the declared end to be
        // buffered *and* the input final (`&&`) would suspend forever and error.
        let (payload, starts) = build_multipart(16, 16, 1, &[1000], 0);
        let truncated = &payload[..starts[0]]; // header + size table only
        assert_cuts_match(truncated, &[]);
    }
}
