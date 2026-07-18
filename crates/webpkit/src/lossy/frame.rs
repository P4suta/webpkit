//! Key-frame encode orchestration (the encoder counterpart of [`crate::lossy::decode`]).
//!
//! Drives the forward pipeline for one intra key frame: convert the source to
//! YUV 4:2:0 ([`crate::lossy::rgb_to_yuv`]); walk the macroblock grid in raster order,
//! predicting each block by a whole-block intra **rate-distortion mode search**
//! (the four 16×16 luma modes `DC`/`V`/`H`/`TM` and, independently, the four 8×8
//! chroma modes, each chosen to minimize a full rate-distortion score against the
//! source), transforming and quantizing the residual, reconstructing it exactly
//! as the decoder will (reusing [`crate::lossy::predict`] / [`crate::lossy::idct`] via
//! [`crate::lossy::reconstruct`]), and emitting its intra modes (control partition) and
//! coefficient tokens (token partition); then assemble the 10-byte frame header,
//! the control partition and the token partition into the raw `VP8 ` payload.
//!
//! # Rate-distortion mode decision
//!
//! For the `Full`/`Best` tiers each candidate mode is scored by its true
//! rate-distortion cost: predict, forward-transform, (trellis-)quantize and
//! reconstruct the block, then take `RD_DISTO_MULT * reconstruction_SSE + lambda *
//! token_bits`, where the distortion is the *post-quant* squared error the decoder
//! will see and the rate is the exact token-tree bit cost
//! ([`block_token_cost`](crate::lossy::trellis::block_token_cost)) — the same 1/256-bit
//! units the trellis optimizes, so trellis and the mode decision compose. `lambda`
//! rises with the plane's AC quant step (coarser quant → bits weigh more). The
//! winner's quantized coefficients drive *both* the reconstruction and the emitted
//! tokens, so decode-equals-reconstruction still holds by construction. The winner is
//! the strict minimum with the modes tried in a fixed `DC, V, H, TM` order, so ties
//! resolve deterministically to the earliest (and `DC`, the availability-safe
//! fallback, wins an all-equal block). The `Fast` tier skips the search entirely and
//! fixes `DC_PRED`.
//!
//! Scope: whole-block (16×16 / 8×8) intra mode search, a single segment, one
//! token partition, an optional per-macroblock skip flag, and the in-loop
//! deblocking filter run as a frame-final post-process. Because reconstruction
//! reuses the very inverse transforms the decoder uses, the emitted tokens
//! dequantize back to the same coefficients, and the filter is applied to the
//! returned planes with the exact per-macroblock strengths the decoder re-derives,
//! `crate::lossy::decode` of the output is byte-identical to the (filtered)
//! reconstruction built here — the self-consistency invariant the tests pin.
#![expect(
    clippy::cast_possible_truncation,
    reason = "residual samples (src - pred) lie in -255..=255 and the broadcast DC \
              in i16 range, so the casts reproduce the reference encoder's int16_t \
              wrapping; the test pattern casts truncate intentionally"
)]

use crate::lossy::bool_enc::BoolEncoder;
use crate::lossy::constants::{
    AC_TABLE, B_DC_PRED, BMODES_PROBA, COEFFS_PROBA_0, CoeffProbas, CoeffStats, CoeffUpdateFlags,
    DC_PRED, H_PRED, NUM_BMODES, NUM_MB_SEGMENTS, TM_PRED, V_PRED,
};
use crate::lossy::decode::{FilterHeader, MbData, SegmentHeader};
use crate::lossy::enc_header::{
    HeaderParams, SegmentParams, frame_header_bytes, write_control_header,
};
use crate::lossy::fdct::{fdct4x4, fwht};
use crate::lossy::idct::{transform_one, transform_wht};
use crate::lossy::loop_filter::FInfo;
use crate::lossy::perceptual;
use crate::lossy::predict::{predict_chroma8, predict_luma4, predict_luma16};
use crate::lossy::prelude::*;
use crate::lossy::prob_opt;
use crate::lossy::quant::{QPair, Quantized, Quantizer, quantize_block, sns_quant_delta};
use crate::lossy::reconstruct::{
    Planes, compute_fstrengths, fill_top_right_lane, filter_frame, reconstruct_mb_at, resolve_finfo,
};
use crate::lossy::rgb_to_yuv::{self, SourceYuv};
use crate::lossy::sharp_yuv;
use crate::lossy::tokens::{
    Block, MbTokens, NzContext, count_mb_residuals, emit_mb_residuals, put_bmode, put_is_i4x4,
    put_segment_id, put_uvmode, put_ymode16,
};
use crate::lossy::trellis::{
    RD_DISTO_MULT, block_token_cost, trellis_lambda, trellis_quantize_block,
};
use crate::lossy::work::work;

/// The resolved search effort for one frame — the internal tier the public
/// [`Effort`](crate::Effort) preset maps onto (via `encoder::effort_tier`),
/// so this row-streaming layer needs no dependency on that enum. `Fast` turns every
/// gate off; `Full` turns the four whole-block gates on; `Best` is `Full` plus the
/// per-macroblock intra-4×4 (`B_PRED`) luma search — the only gate that separates
/// `Best` from `Full`, keeping `Full` (Balanced) output byte-identical.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Effort {
    /// Fastest: fixed `DC_PRED`, default probabilities, no skip coding, no filter.
    Fast,
    /// Full search: whole-block intra-mode search, probability optimization,
    /// per-macroblock skip coding, and the in-loop deblocking filter.
    Full,
    /// Everything `Full` does plus the intra-4×4 (`B_PRED`) luma mode search, coded
    /// per macroblock when its rate-distortion cost beats the 16×16 candidate.
    Best,
}

impl Effort {
    /// Whether to run the whole-block intra-mode search (else fix `DC_PRED`).
    const fn search_modes(self) -> bool {
        matches!(self, Self::Full | Self::Best)
    }

    /// Whether to derive an optimized coefficient-probability table (else default).
    const fn optimize_probas(self) -> bool {
        matches!(self, Self::Full | Self::Best)
    }

    /// Whether to code flat macroblocks with a per-macroblock skip flag.
    const fn consider_skip(self) -> bool {
        matches!(self, Self::Full | Self::Best)
    }

    /// Whether to apply the in-loop deblocking filter (else code it off, level 0).
    const fn apply_filter(self) -> bool {
        matches!(self, Self::Full | Self::Best)
    }

    /// Whether to additionally consider coding luma as sixteen intra-4×4 sub-blocks
    /// (`Best` only) — the single gate that distinguishes `Best` from `Full`.
    const fn uses_i4x4(self) -> bool {
        matches!(self, Self::Best)
    }

    /// Whether to select coefficient levels by rate-distortion trellis quantization
    /// (`Full`/`Best`) instead of round-to-nearest (`Fast`). Trellis is
    /// self-consistent by construction (`recon = level * q`), so this only changes the
    /// size/quality trade-off, never the decode identity.
    const fn uses_trellis(self) -> bool {
        matches!(self, Self::Full | Self::Best)
    }

    /// Whether to partition the macroblocks into up to four quantizer segments by a
    /// per-macroblock complexity metric (`Full`/`Best`). Segmentation is
    /// decoder-safe by construction (`recon = level * q_seg` on both sides), so it
    /// only shifts the rate-distortion trade-off, never the decode identity. `Fast`
    /// keeps a single segment, byte-identical to the pre-segmentation encoder.
    const fn uses_segments(self) -> bool {
        matches!(self, Self::Full | Self::Best)
    }
}

/// The four active psychovisual knobs threaded from
/// [`LossyTuning`](crate::lossy::LossyTuning) into the frame encode: spatial-noise
/// shaping strength (`0..=100`), macroblock segment count (`1..=4`), and the in-loop
/// deblocking-filter strength (`0..=100`) and sharpness (`0..=7`). Every knob is gated
/// by the resolved [`Effort`] (`Fast` runs no mode search / segmentation / filter, so
/// the knobs are inert there and its output stays byte-identical).
#[derive(Clone, Copy)]
pub(crate) struct FrameTuning {
    /// Spatial-noise-shaping strength (`0..=100`); `0` disables perceptual shaping.
    pub(crate) sns_strength: u8,
    /// Number of macroblock quantizer segments (`1..=4`).
    pub(crate) segments: u8,
    /// In-loop deblocking-filter strength (`0..=100`); `0` disables the filter.
    pub(crate) filter_strength: u8,
    /// In-loop deblocking-filter sharpness (`0..=7`).
    pub(crate) filter_sharpness: u8,
    /// Whether to replace the plain 4:2:0 box chroma with luminance-guided (sharp) chroma
    /// ([`crate::lossy::sharp_yuv`]). `false` (the default) leaves the box path — and every
    /// output byte — unchanged; only the U/V planes are affected when it is set.
    pub(crate) sharp_yuv: bool,
    /// Number of entropy-refinement passes (`1..=10`; libwebp's `StatLoop`). `1` (the
    /// default) is the single-pass, byte-identical encode; a higher count re-plans the
    /// frame against the previous pass's optimized coefficient probabilities, so the
    /// trellis rate model — and the encoded size — converge. Only acted on by tiers that
    /// optimize probabilities (`Full`/`Best`); `Fast` has no proba stage and always runs
    /// one pass.
    pub(crate) passes: u8,
}

impl FrameTuning {
    /// The auto / near-best baseline matching `cwebp`'s default shaping — the tuning
    /// [`encode_frame`] uses when no explicit [`FrameTuning`] is supplied. Production
    /// threads an explicit [`FrameTuning`] built from the caller's
    /// [`LossyTuning`](crate::lossy::LossyTuning) (whose default holds the same values),
    /// so this constant backs only the test-facing [`encode_frame`] convenience.
    #[cfg(test)]
    pub(crate) const AUTO: Self = Self {
        sns_strength: 50,
        segments: 4,
        filter_strength: 60,
        filter_sharpness: 0,
        sharp_yuv: false,
        passes: 1,
    };
}

/// Encode an RGBA source (`rgba`, 4 bytes/pixel, row-major `width` × `height`) at
/// base quantizer index `base_q` (`0..=127`) into a raw `VP8 ` key-frame payload,
/// with the search depth selected by the resolved [`Effort`] gates and the near-best
/// [`FrameTuning::AUTO`] psychovisual shaping. The test-facing convenience over
/// [`encode_frame_tuned`], which production uses to thread the caller's tuning.
#[cfg(test)]
#[must_use]
pub(crate) fn encode_frame(
    rgba: &[u8],
    width: usize,
    height: usize,
    base_q: i32,
    effort: Effort,
) -> Vec<u8> {
    encode_frame_impl(rgba, width, height, base_q, effort, FrameTuning::AUTO).0
}

/// Encode an RGBA source with an explicit [`FrameTuning`] — the seam the public
/// encoder uses to thread the caller's [`LossyTuning`](crate::lossy::LossyTuning).
#[must_use]
pub(crate) fn encode_frame_tuned(
    rgba: &[u8],
    width: usize,
    height: usize,
    base_q: i32,
    effort: Effort,
    tuning: FrameTuning,
) -> Vec<u8> {
    encode_frame_impl(rgba, width, height, base_q, effort, tuning).0
}

/// The frame's per-macroblock skip-coding decision (libwebp `CalcSkipProba`):
/// whether an explicit skip flag is coded and, if so, its probability. Bundled so
/// it threads as one value through the emission passes.
#[derive(Clone, Copy)]
struct SkipCoding {
    /// Whether every macroblock carries an explicit skip flag (`proba.use_skip`).
    use_skip: bool,
    /// The probability of the non-skip bit — the `read_bool` prob-of-0 the decoder
    /// reads for each macroblock's skip flag.
    skip_p: u8,
}

/// One macroblock's Pass-1 result, buffered for Pass-2 emission.
struct MbPlan {
    /// The 16×16 luma prediction mode (meaningful only when `!is_i4x4`).
    ymode: u8,
    /// The sixteen intra-4×4 luma sub-block modes in raster order (meaningful only
    /// when `is_i4x4`); Pass 2 emits them through the `kBModesProba` top/left
    /// context.
    imodes: [u8; 16],
    /// The chroma prediction mode.
    uvmode: u8,
    /// Whether the luma is coded as sixteen intra-4×4 blocks (`Best` only) rather
    /// than one 16×16 block with a second-order (Y2) block.
    is_i4x4: bool,
    /// Whether every quantized coefficient is zero, so the macroblock can be
    /// coded with `mb_skip_coeff = 1` (no residual tokens) — it already
    /// reconstructs to pure prediction.
    skippable: bool,
    /// Whether any reconstructed coefficient is non-zero. This is exactly the
    /// decoder's `(non_zero_y | non_zero_uv) != 0` (the emitted tokens dequantize
    /// to these same coefficients), so it drives the loop filter's per-macroblock
    /// `f_inner` resolution identically on both sides.
    has_residual: bool,
    /// The quantized coefficient tokens ready for the token partition.
    tokens: MbTokens,
    /// The macroblock's segment id (`0..=3`); `0` for a single-segment frame. Emitted
    /// before the skip flag when the frame codes a segment map, and it selects the
    /// per-segment quantizer used for this macroblock.
    segment: u8,
}

/// The encode core, additionally returning the reconstructed [`Planes`] so tests
/// can assert self-consistency against the decoder's reconstruction.
///
/// Drives up to `tuning.passes` entropy-refinement passes (libwebp's `StatLoop`).
/// The first pass plans the frame against the default coefficient probabilities and
/// is byte-identical to a single pass; each further pass re-plans against the
/// previous pass's optimized table, so the trellis rate model — and the encoded
/// size — converge. Only tiers that optimize probabilities (`Full`/`Best`) refine;
/// `Fast` has no proba stage and runs exactly one pass, so `passes` is inert there
/// (and the whole tier stays byte-identical). The smallest payload across the passes
/// is returned, so a multi-pass encode is never larger than the single-pass floor.
fn encode_frame_impl(
    rgba: &[u8],
    width: usize,
    height: usize,
    base_q: i32,
    effort: Effort,
    tuning: FrameTuning,
) -> (Vec<u8>, Planes) {
    // Only the proba-optimizing tiers refine; Fast has no table to iterate.
    let passes = if effort.optimize_probas() {
        usize::from(tuning.passes.max(1))
    } else {
        1
    };
    // Pass 1 plans against the defaults (the byte-identical floor). Its optimized
    // table (`next`) becomes the cost model the next pass's trellis charges against.
    let (mut best_payload, mut best_planes, mut next) =
        encode_one_pass(rgba, width, height, base_q, effort, tuning, &COEFFS_PROBA_0);
    for _ in 1..passes {
        // No optimized table (a Fast tier that never runs multi-pass) → nothing to
        // refine against; stop.
        let Some(cost) = next else { break };
        let (payload, planes, n) =
            encode_one_pass(rgba, width, height, base_q, effort, tuning, &cost);
        // Keep the smallest payload (ties resolve to the earlier pass), so pass 1 is
        // the floor a refinement can only match or beat.
        if payload.len() < best_payload.len() {
            best_payload = payload;
            best_planes = planes;
        }
        next = n;
    }
    (best_payload, best_planes)
}

/// One encode pass: plan, choose the filter, emit the partitions against `cost` (the
/// trellis cost model), and deblock. Returns the raw `VP8 ` payload, the reconstructed
/// (filtered) [`Planes`], and — for a proba-optimizing tier — the optimized
/// coefficient-probability table the next pass should plan against.
///
/// Pass 1 ([`plan_frame`]) predicts (searching intra modes when
/// `effort.search_modes`, else fixing `DC_PRED`), transforms, quantizes and
/// reconstructs every macroblock, buffering each one's modes and tokens in an
/// [`MbPlan`]. Pass 2 replays those plans in the identical raster order to write
/// the control and token partitions — via [`emit_best_partitions`] when
/// `effort.optimize_probas` (which keeps the smaller of the default and optimized
/// tables), else emitting the default table directly. The split is byte-identical
/// to a single interleaved pass: nothing emitted feeds prediction, quantization or
/// reconstruction, so the deferred `put_bool` sequence and the [`NzContext`]
/// evolution are unchanged.
///
/// When `effort.consider_skip` is set, the per-macroblock skip probability is
/// derived from the Pass-1 skippable count (libwebp `CalcSkipProba`) and, if enough
/// macroblocks skip, the frame codes them with an explicit skip flag and no residual
/// tokens; the pixels are unchanged (a skippable macroblock has all-zero
/// coefficients, so it already reconstructs to pure prediction). Finally, when
/// `effort.apply_filter` is set, the reconstructed [`Planes`] are deblocked in place
/// by the frame-final in-loop filter before they are returned — with the exact
/// per-macroblock strengths the decoder re-derives from the emitted filter header.
fn encode_one_pass(
    rgba: &[u8],
    width: usize,
    height: usize,
    base_q: i32,
    effort: Effort,
    tuning: FrameTuning,
    cost: &CoeffProbas,
) -> (Vec<u8>, Planes, Option<CoeffProbas>) {
    let gates = SearchGates {
        search_modes: effort.search_modes(),
        uses_i4x4: effort.uses_i4x4(),
        uses_trellis: effort.uses_trellis(),
        sns_strength: tuning.sns_strength,
    };
    let FramePlan {
        plans: mb_plans,
        mut planes,
        mb_w,
        mb_h,
        mut seg_params,
        seg_base_q,
    } = plan_frame(
        rgba,
        width,
        height,
        base_q,
        gates,
        effort.uses_segments(),
        tuning,
        cost,
    );
    let skip = resolve_skip(&mb_plans, mb_w * mb_h, effort.consider_skip());
    // The in-loop filter is a frame-final post-process: it never feeds prediction,
    // quantization or token emission, so the token stream (and its byte size) is
    // unchanged by it. Choose it once (per-segment strengths derived from each
    // segment's quantizer, the filter strength/sharpness knobs), emit it into the
    // control header, and apply it to the reconstruction below.
    let filter = choose_filter(
        base_q,
        effort.apply_filter(),
        tuning.filter_strength,
        tuning.filter_sharpness,
    );
    // Per-segment filter deltas ride in the segment header (relative to `filter.level`),
    // so a coarser (busier) segment deblocks harder than a flat one. When the frame is
    // unsegmented there is nothing to carry — the base level already reflects `base_q`.
    if let Some(seg) = seg_params.as_mut() {
        seg.filter_strength = segment_filter_deltas(
            filter.level,
            seg_base_q,
            effort.apply_filter(),
            tuning.filter_strength,
        );
    }
    let header = HeaderParams {
        base_q,
        filter: &filter,
        segments: seg_params,
    };
    let (part0_bytes, token_bytes, next) = if effort.optimize_probas() {
        let (part0, token, _default_total, _optimized_total, opt_probas) =
            emit_best_partitions(&mb_plans, mb_w, mb_h, header, skip);
        (part0, token, Some(opt_probas))
    } else {
        // Fast: emit the default probability section and default-table tokens
        // directly — no count / optimize / keep-smaller. This reproduces the
        // default candidate `emit_best_partitions` would have kept as its floor.
        let (part0, token) = emit_partitions(
            &mb_plans,
            mb_w,
            mb_h,
            header,
            &COEFFS_PROBA_0,
            &CoeffUpdateFlags::default(),
            skip,
        );
        (part0, token, None)
    };

    // Deblock the reconstruction with the exact per-macroblock strengths the
    // decoder re-derives from the emitted header, so decode-of-output stays
    // byte-identical to these returned planes (a no-op when the filter is off).
    apply_loop_filter(
        &mut planes,
        &mb_plans,
        mb_w,
        mb_h,
        &filter,
        skip.use_skip,
        seg_params,
    );

    let fps = u32::try_from(part0_bytes.len()).unwrap_or(0);
    let header = frame_header_bytes(
        fps,
        u16::try_from(width).unwrap_or(0),
        u16::try_from(height).unwrap_or(0),
    );

    let mut payload = Vec::with_capacity(header.len() + part0_bytes.len() + token_bytes.len());
    payload.extend_from_slice(&header);
    payload.extend_from_slice(&part0_bytes);
    payload.extend_from_slice(&token_bytes);
    (payload, planes, next)
}

/// Decide whether the frame codes per-macroblock skip and, if so, the skip
/// probability (libwebp `CalcSkipProba`). `skip_p` is the probability of the
/// *non*-skip bit — `(non_skipped * 255) / total`, i.e. the `read_bool` prob-of-0
/// the decoder reads. Skip is used only when the caller allows it, at least one
/// macroblock is skippable, and it clears libwebp's `SKIP_PROBA` threshold (250);
/// otherwise every macroblock codes its tokens.
fn resolve_skip(mb_plans: &[MbPlan], total: usize, consider_skip: bool) -> SkipCoding {
    let nb_skip = mb_plans.iter().filter(|p| p.skippable).count();
    // `checked_div` folds in the `total == 0` guard (an empty frame → 255).
    let skip_p = ((total - nb_skip) * 255)
        .checked_div(total)
        .and_then(|p| u8::try_from(p).ok())
        .unwrap_or(255);
    let use_skip = consider_skip && nb_skip > 0 && skip_p < 250;
    SkipCoding { use_skip, skip_p }
}

/// The Pass-1 search-depth gates (the effort tier's three whole-block toggles),
/// bundled so [`plan_frame`] threads one value. Segmentation is a separate axis
/// (quantizer allocation, not mode search) and rides as its own `plan_frame`
/// argument.
#[derive(Clone, Copy)]
struct SearchGates {
    /// Run the whole-block intra-mode search (else fix `DC_PRED`).
    search_modes: bool,
    /// Also try coding luma as sixteen intra-4×4 sub-blocks (`Best`).
    uses_i4x4: bool,
    /// Select coefficient levels by rate-distortion trellis (else round-to-nearest).
    uses_trellis: bool,
    /// Spatial-noise-shaping strength (`0..=100`) weighting the perceptual texture term
    /// in the luma mode decision; carried into each macroblock's [`QuantPlan`].
    sns_strength: u8,
}

/// The activity value that yields a zero SNS quant delta (the midpoint of the
/// `0..=255` [`mb_activity`] range) — the single-segment / zero-strength anchor.
const NEUTRAL_ACTIVITY: u8 = 128;
/// The most k-means refinement passes over the per-macroblock activity. Fixed so the
/// segmentation is fully deterministic; the displacement early-out (below) usually
/// stops sooner.
const KMEANS_ITERS: usize = 6;
/// The k-means early-out threshold (libwebp `AssignSegments`): once the total centroid
/// displacement across a pass drops below this many activity units, the clustering has
/// converged and further passes cannot move a macroblock.
const KMEANS_MIN_DISPLACEMENT: i64 = 5;
/// The number of histogram bins over `|fdct| >> 3` (libwebp `MAX_COEFF_THRESH + 1`) the
/// [`mb_activity`] metric accumulates into.
const ACTIVITY_BINS: usize = 32;
/// The activity numerator (libwebp `ALPHA_SCALE = 2 * MAX_ALPHA`): the `GetAlpha` ratio is
/// `510 * last_non_zero / max_value`, clamped into `0..=255`.
const ACTIVITY_SCALE: i32 = 510;

/// The frame's macroblock segmentation: the per-macroblock segment id, the four
/// per-segment base quantizer indices (to build the `[Quantizer; 4]`), and the
/// emittable header params. `params` is `None` when the content collapses to a
/// single segment, in which case the frame is coded with `use_segment = false`
/// (byte-identical to the pre-segmentation encoder).
struct Segmentation {
    /// Per-macroblock segment id in raster order (`0` everywhere when unsegmented).
    seg_ids: Vec<u8>,
    /// Per-segment base quantizer index (all `base_q` when unsegmented).
    base_q: [i32; 4],
    /// The emittable segment header, or `None` for a single-segment frame.
    params: Option<SegmentParams>,
}

/// Partition the macroblocks into up to `tuning.segments` (`1..=4`) quantizer segments
/// by an integer k-means over each macroblock's source luma **activity** (libwebp's
/// `GetAlpha` texture metric, [`mb_activity`]), then set each segment's quantizer by the
/// zero-centered SNS delta ([`sns_quant_delta`]) for its representative activity at
/// `tuning.sns_strength`: flat (low-activity) segments are coded finer, busy ones
/// coarser. Deterministic (fixed init + bounded passes, all integer). `enabled` is the
/// tier gate; when it is false, `tuning.segments == 1`, or `tuning.sns_strength == 0`
/// (every delta is zero) the frame stays a single segment at `base_q`, byte-identical
/// to the pre-SNS encoder.
fn plan_segmentation(
    src: &SourceYuv,
    base_q: i32,
    enabled: bool,
    tuning: FrameTuning,
) -> Segmentation {
    let n = src.mb_w * src.mb_h;
    let single = || Segmentation {
        seg_ids: vec![0u8; n],
        base_q: [base_q; 4],
        params: None,
    };
    let nb = usize::from(tuning.segments.clamp(1, 4));
    if !enabled || nb <= 1 || tuning.sns_strength == 0 {
        return single();
    }
    let mut activity = Vec::with_capacity(n);
    for mb_y in 0..src.mb_h {
        for mb_x in 0..src.mb_w {
            activity.push(i64::from(mb_activity(src, mb_x, mb_y)));
        }
    }
    let (seg_ids, count, seg_a) = kmeans_segments(&activity, nb);
    let seg_base_q = segment_base_qs(base_q, count, seg_a, tuning.sns_strength);
    // If the SNS deltas collapse every segment to the same quantizer (uniform activity,
    // or the deltas cancel), segmentation buys nothing but header bytes — fall back.
    if count <= 1 || seg_base_q[..count].iter().all(|&q| q == base_q) {
        return single();
    }
    let mut quantizer = [0i32; 4];
    for (delta, &bq) in quantizer.iter_mut().zip(&seg_base_q) {
        *delta = bq - base_q;
    }
    let tree_probs = segment_tree_probs(&seg_ids);
    Segmentation {
        seg_ids,
        base_q: seg_base_q,
        params: Some(SegmentParams {
            quantizer,
            filter_strength: [0; 4],
            tree_probs,
        }),
    }
}

/// One macroblock's texture **activity** in `0..=255` (libwebp `GetAlpha` over the
/// `VP8CollectHistogram` distribution): forward-DCT each of the sixteen 4×4 source luma
/// blocks, drop the DC (so brightness does not register), bin every remaining
/// coefficient by `|coeff| >> 3` into [`ACTIVITY_BINS`] buckets, and score
/// `510 * last_non_zero_bin / max_bin_population`, clamped into `0..=255`. Flat blocks
/// pile into bin `0` and score ~0; busy blocks spread into high bins and score high.
fn mb_activity(src: &SourceYuv, mb_x: usize, mb_y: usize) -> u8 {
    let stride = src.y_stride();
    let base = mb_y * 16 * stride + mb_x * 16;
    let mut dist = [0i32; ACTIVITY_BINS];
    for n in 0..16usize {
        let (bx, by) = ((n % 4) * 4, (n / 4) * 4);
        let mut block = [0i16; 16];
        for row in 0..4 {
            for col in 0..4 {
                block[row * 4 + col] = i16::from(src.y[base + (by + row) * stride + (bx + col)]);
            }
        }
        work!(MbComplexityFdct);
        let mut coeffs = fdct4x4(block);
        coeffs[0] = 0; // drop the DC so activity measures texture, not brightness
        for &c in &coeffs {
            let bin = usize::from(c.unsigned_abs() >> 3).min(ACTIVITY_BINS - 1);
            dist[bin] += 1;
        }
    }
    get_alpha(dist)
}

/// libwebp `GetAlpha`: `510 * last_non_zero_bin / max_bin_population`, clamped into
/// `0..=255`. `0` when no bin above the empty floor is populated.
fn get_alpha(dist: [i32; ACTIVITY_BINS]) -> u8 {
    let mut max_value = 0i32;
    let mut last_non_zero = 1usize;
    for (k, &value) in dist.iter().enumerate() {
        if value > 0 {
            if value > max_value {
                max_value = value;
            }
            last_non_zero = k;
        }
    }
    let alpha = if max_value > 1 {
        ACTIVITY_SCALE * i32::try_from(last_non_zero).unwrap_or(0) / max_value
    } else {
        0
    };
    u8::try_from(alpha.clamp(0, 255)).unwrap_or(0)
}

/// Integer k-means over the per-macroblock `activity` into `nb` (`1..=4`) segments
/// (libwebp `AssignSegments`): evenly-spaced init, monotone nearest-centroid assign,
/// weighted-mean update, and the [`KMEANS_MIN_DISPLACEMENT`] early-out. Returns the
/// per-macroblock segment id (contiguous `0..count`), the number of non-empty segments
/// `count`, and each segment's representative (mean) activity. A degenerate all-equal
/// input returns a single segment.
fn kmeans_segments(activity: &[i64], nb: usize) -> (Vec<u8>, usize, [i64; 4]) {
    let n = activity.len();
    let nb = nb.clamp(1, 4);
    let min_a = activity.iter().copied().min().unwrap_or(0);
    let max_a = activity.iter().copied().max().unwrap_or(0);
    if n == 0 || nb <= 1 || min_a == max_a {
        return (vec![0u8; n], 1, [min_a; 4]);
    }
    // Even-init: `nb` centroids spread across `[min, max]` (`nb - 1` gaps).
    let span = max_a - min_a;
    let mut centroids = [0i64; 4];
    let divisor = i64::try_from(nb - 1).unwrap_or(1).max(1);
    for (k, cen) in centroids.iter_mut().take(nb).enumerate() {
        *cen = min_a + span * i64::try_from(k).unwrap_or(0) / divisor;
    }
    let mut assign = vec![0u8; n];
    for _ in 0..KMEANS_ITERS {
        assign_nearest(activity, centroids, nb, &mut assign);
        if update_centroids(activity, &assign, &mut centroids, nb) < KMEANS_MIN_DISPLACEMENT {
            break;
        }
    }
    assign_nearest(activity, centroids, nb, &mut assign);

    // Collapse empty clusters: remap the used centroid indices to contiguous ids and
    // record each surviving segment's mean activity.
    let (sum, cnt) = cluster_sums(activity, &assign);
    let mut remap = [0u8; 4];
    let mut seg_a = [0i64; 4];
    let mut count = 0usize;
    for k in 0..nb {
        if cnt[k] > 0 {
            remap[k] = u8::try_from(count).unwrap_or(0);
            seg_a[count] = sum[k] / cnt[k];
            count += 1;
        }
    }
    let seg_ids = assign.iter().map(|&a| remap[usize::from(a)]).collect();
    (seg_ids, count, seg_a)
}

/// Assign each value to its nearest of the first `nb` centroids (ties → lowest index).
fn assign_nearest(values: &[i64], centroids: [i64; 4], nb: usize, assign: &mut [u8]) {
    for (&c, a) in values.iter().zip(assign.iter_mut()) {
        let mut best = 0usize;
        let mut best_d = i64::MAX;
        for (k, &cen) in centroids.iter().take(nb).enumerate() {
            work!(KmeansCompare);
            let d = (c - cen).abs();
            if d < best_d {
                best_d = d;
                best = k;
            }
        }
        *a = u8::try_from(best).unwrap_or(0);
    }
}

/// Per-cluster value sum and count under `assign`.
fn cluster_sums(values: &[i64], assign: &[u8]) -> ([i64; 4], [i64; 4]) {
    let mut sum = [0i64; 4];
    let mut cnt = [0i64; 4];
    for (&c, &a) in values.iter().zip(assign) {
        let k = usize::from(a);
        sum[k] += c;
        cnt[k] += 1;
    }
    (sum, cnt)
}

/// Move each non-empty of the first `nb` centroids to its cluster's integer mean
/// (empty clusters keep their position, so the count can only shrink, never wander),
/// and return the total absolute centroid displacement — the k-means early-out signal.
fn update_centroids(values: &[i64], assign: &[u8], centroids: &mut [i64; 4], nb: usize) -> i64 {
    let (sum, cnt) = cluster_sums(values, assign);
    let mut displacement = 0i64;
    for (cen, (&s, &c)) in centroids.iter_mut().zip(sum.iter().zip(&cnt)).take(nb) {
        if c > 0 {
            let moved = s / c;
            displacement += (moved - *cen).abs();
            *cen = moved;
        }
    }
    displacement
}

/// Map each of the `count` segments' representative activity to a base quantizer index
/// by the zero-centered SNS delta ([`sns_quant_delta`]) at `sns_strength`: a flat
/// (low-activity) segment is coded finer (negative delta), a busy one coarser (positive
/// delta), anchored on `base_q` and clamped into `0..=127`. A mid-activity segment or a
/// zero strength leaves the quantizer at `base_q`. Unused segments (`count..4`) keep
/// `base_q`.
fn segment_base_qs(base_q: i32, count: usize, seg_a: [i64; 4], sns_strength: u8) -> [i32; 4] {
    let mut qs = [base_q; 4];
    for (q, &a) in qs.iter_mut().take(count).zip(&seg_a) {
        let activity = u8::try_from(a.clamp(0, 255)).unwrap_or(NEUTRAL_ACTIVITY);
        *q = (base_q + sns_quant_delta(activity, sns_strength)).clamp(0, 127);
    }
    qs
}

/// Derive the three segment-id decision-tree probabilities from the segment
/// populations, so a macroblock's id costs about its information content. Each is the
/// probability (`1..=255`, `256`-scaled) that the corresponding tree bit is zero.
fn segment_tree_probs(seg_ids: &[u8]) -> [u8; 3] {
    let mut cnt = [0u64; 4];
    for &s in seg_ids {
        cnt[usize::from(s)] += 1;
    }
    let n01 = cnt[0] + cnt[1];
    let n23 = cnt[2] + cnt[3];
    [
        tree_prob(n01, n01 + n23),
        tree_prob(cnt[0], n01),
        tree_prob(cnt[2], n23),
    ]
}

/// The `256`-scaled probability that a segment-tree bit is zero, given `zeros`
/// zero-outcomes out of `total`, clamped to the valid `1..=255` range (an empty
/// branch is irrelevant, so it takes the neutral `128`).
fn tree_prob(zeros: u64, total: u64) -> u8 {
    if total == 0 {
        return 128;
    }
    u8::try_from((zeros * 255 / total).clamp(1, 255)).unwrap_or(128)
}

/// The result of [`plan_frame`]: the buffered per-macroblock plans, the finished
/// reconstruction, the macroblock grid size, the optional segmentation params, and the
/// per-segment base quantizer indices (used to derive the per-segment filter strengths).
struct FramePlan {
    plans: Vec<MbPlan>,
    planes: Planes,
    mb_w: usize,
    mb_h: usize,
    seg_params: Option<SegmentParams>,
    /// Per-segment base quantizer index (all `base_q` when unsegmented).
    seg_base_q: [i32; 4],
}

/// Pass 1: predict, transform/quantize and reconstruct every macroblock, returning
/// the buffered per-MB [`MbPlan`]s, the finished reconstruction [`Planes`] and the
/// macroblock grid size. Nothing here depends on entropy coding, so Pass 2 is free
/// to try several coefficient-probability tables against the same plans.
///
/// When `search_modes` is set, each block's luma and chroma modes are chosen by the
/// pre-quant SSE search; otherwise every block is fixed to `DC_PRED` (the fastest
/// path), with the `DC` prediction committed into the planes exactly as the search
/// commits its winner, so [`encode_mb`] reads the same samples either way.
#[allow(
    clippy::too_many_lines,
    reason = "a cohesive two-pass macroblock planner: the RGB->YUV + segmentation \
              setup and the single MB raster loop (mode search, i4x4 RD, skip/residual \
              flags, reconstruction) share tightly-threaded `&mut planes` state, so \
              splitting it would need a 7+ argument helper and fragment one unit"
)]
#[expect(
    clippy::too_many_arguments,
    reason = "the frame plan is the source, its geometry, the base quantizer, the \
              search gates, the segmentation gate, the psychovisual tuning and the \
              trellis cost model — independent inputs, not a bundle"
)]
fn plan_frame(
    rgba: &[u8],
    width: usize,
    height: usize,
    base_q: i32,
    gates: SearchGates,
    uses_segments: bool,
    tuning: FrameTuning,
    probas: &CoeffProbas,
) -> FramePlan {
    let mut src = rgb_to_yuv::from_rgba(rgba, width, height);
    // Sharp-YUV is an opt-in chroma refinement: it overwrites only the U/V planes with the
    // luminance-guided subsampling, leaving the luma plane (and, when off, every byte)
    // untouched. The subsequent VP8 encode is agnostic to which chroma it received.
    if tuning.sharp_yuv {
        sharp_yuv::refine_chroma(rgba, width, height, &mut src);
    }
    let (mb_w, mb_h) = (src.mb_w, src.mb_h);
    // Partition the macroblocks into up to four quantizer segments (Full/Best), then
    // build one forward quantizer per segment. A single-segment frame reuses
    // `Quantizer::new(base_q)` for all four slots, so the fast path is unchanged.
    let seg = plan_segmentation(&src, base_q, uses_segments, tuning);
    let quants = seg.base_q.map(Quantizer::new);

    let mut planes = Planes::new(mb_w, mb_h);
    let mb_plans = run_mb_planning(
        &mut planes,
        &src,
        &seg.seg_ids,
        &quants,
        mb_w,
        mb_h,
        gates,
        probas,
    );
    FramePlan {
        plans: mb_plans,
        planes,
        mb_w,
        mb_h,
        seg_params: seg.params,
        seg_base_q: seg.base_q,
    }
}

/// Plan one macroblock: run its intra-mode / i4x4 rate-distortion search, quantize,
/// reconstruct it into `planes`, and return the buffered [`MbPlan`]. This is a pure
/// function of the source macroblock, the segment quantizer, and the *reconstructed
/// neighbor pixels already in `planes`* — it reads NO raster-threaded entropy or
/// intra-mode-prediction context (those are applied only in the serial emit pass).
/// Consequently it can be driven either against the whole-frame `planes` in raster
/// order (`plan_frame_serial`) or against a per-macroblock scratch buffer with the
/// macroblock placed at local `(0, 0)` (the wavefront scheduler), provided the
/// caller supplies the true `has_top`/`has_left`/`is_rightmost` grid geometry (the
/// intra-4×4 top-right lane depends on it).
#[expect(
    clippy::too_many_arguments,
    reason = "the per-macroblock plan is a pure function of its position, the three \
              independent edge flags, its segment/quantizer and the search gates; \
              bundling them would only move the argument list into a struct literal"
)]
fn plan_one_mb(
    planes: &mut Planes,
    src: &SourceYuv,
    mb_x: usize,
    mb_y: usize,
    has_top: bool,
    has_left: bool,
    is_rightmost: bool,
    segment: u8,
    plan: QuantPlan<'_>,
    gates: SearchGates,
) -> MbPlan {
    let y_off = (mb_y * 16 + 1) * planes.y_stride + (mb_x * 16 + 1);
    let uv_off = (mb_y * 8 + 1) * planes.uv_stride + (mb_x * 8 + 1);

    // Choose the whole-block luma and chroma modes and their quantized
    // coefficients. For Full/Best this is a full rate-distortion search that
    // folds in quantization (each candidate is transformed, quantized and
    // reconstructed); the winner's prediction is left committed in the planes
    // and its coefficients/tokens are returned directly, so the winner is never
    // re-encoded. The fast path skips the search, commits fixed DC prediction
    // and encodes it. `coeffs` carries the whole macroblock; `tokens` holds its
    // 16×16 luma / Y2 / chroma blocks.
    let (ymode, uvmode, mut coeffs, mut tokens) = if gates.search_modes {
        let (ymode, luma) =
            select_luma16_mode_rd(planes, y_off, src, (mb_x, mb_y), has_top, has_left, plan);
        let (uvmode, chroma) =
            select_chroma8_mode_rd(planes, uv_off, src, (mb_x, mb_y), has_top, has_left, plan);
        let mut coeffs = [0i16; 384];
        coeffs[..256].copy_from_slice(&luma.coeffs);
        coeffs[256..].copy_from_slice(&chroma.coeffs);
        let tokens = MbTokens {
            is_i4x4: false,
            y2: luma.y2,
            luma: luma.blocks,
            chroma: chroma.blocks,
        };
        (ymode, uvmode, coeffs, tokens)
    } else {
        let (y_stride, uv_stride) = (planes.y_stride, planes.uv_stride);
        predict_luma16(&mut planes.y, y_off, y_stride, DC_PRED, has_top, has_left);
        predict_chroma8(&mut planes.u, uv_off, uv_stride, DC_PRED, has_top, has_left);
        predict_chroma8(&mut planes.v, uv_off, uv_stride, DC_PRED, has_top, has_left);
        let (coeffs, tokens) = encode_mb(src, planes, mb_x, mb_y, plan);
        (DC_PRED, DC_PRED, coeffs, tokens)
    };

    let mut is_i4x4 = false;
    let mut imodes = [0u8; 16];
    imodes[0] = ymode;

    // Best only: also encode the luma as sixteen intra-4×4 sub-blocks and
    // keep whichever candidate wins the rate-distortion decision. Chroma is
    // shared (identical either way) so it stays out of the comparison.
    let i4x4 = gates
        .uses_i4x4
        .then(|| {
            try_i4x4_luma(
                planes,
                &coeffs,
                &tokens,
                src,
                (mb_x, mb_y),
                has_top,
                is_rightmost,
                plan,
            )
        })
        .flatten();
    if let Some(i4) = i4x4 {
        is_i4x4 = true;
        imodes = i4.imodes;
        coeffs[..256].copy_from_slice(&i4.coeffs);
        tokens.is_i4x4 = true;
        tokens.y2 = Block::default();
        tokens.luma = i4.luma;
    }

    // A 16×16 macroblock is skippable when every quantized coefficient is
    // zero: the Y2 block (first=0) empty (last < 0), all 16 luma AC blocks
    // (first=1) empty (last < 1), and all 8 chroma blocks (first=0) empty
    // (last < 0). Such a block reconstructs to pure prediction, so it can be
    // coded with a skip flag instead of all-zero residual tokens. An i4x4
    // macroblock is never coded skippable (its DC is per-sub-block, not in a
    // Y2 block), mirroring the decoder's `skip_residuals` clearing `nz_dc`
    // only for `!is_i4x4`.
    let skippable = !is_i4x4
        && tokens.y2.last < 0
        && tokens.luma.iter().all(|b| b.last < 1)
        && tokens.chroma.iter().all(|b| b.last < 0);

    // Whether the dequantized block carries any non-zero coefficient — the
    // decoder computes the same `(non_zero_y | non_zero_uv) != 0` from these
    // exact coefficients, so it drives the loop filter's `f_inner` decision.
    let has_residual = coeffs.iter().any(|&c| c != 0);

    // Reconstruct exactly as the decoder will: re-predict with the chosen
    // modes and add the inverse-transformed residual. The chosen modes must
    // ride along in the MbData so the reconstruction re-predicts identically.
    let mb_data = MbData {
        coeffs,
        is_i4x4,
        imodes,
        uvmode,
        ..MbData::default()
    };
    reconstruct_mb_at(
        planes,
        &mb_data,
        y_off,
        uv_off,
        has_top,
        has_left,
        is_rightmost,
    );

    MbPlan {
        ymode,
        imodes,
        uvmode,
        is_i4x4,
        skippable,
        has_residual,
        tokens,
        segment,
    }
}

/// Plan every macroblock in raster order against the shared frame `planes`. The
/// serial reference: also the wavefront scheduler's small-frame fallback and the
/// byte-for-byte oracle its parallel result is tested against.
#[expect(
    clippy::too_many_arguments,
    reason = "the planner takes the target planes, the source, the segment map and \
              its quantizers, the grid size, the search gates and the trellis cost \
              model — independent inputs threaded straight to `plan_one_mb`"
)]
fn plan_frame_serial(
    planes: &mut Planes,
    src: &SourceYuv,
    seg_ids: &[u8],
    quants: &[Quantizer; NUM_MB_SEGMENTS],
    mb_w: usize,
    mb_h: usize,
    gates: SearchGates,
    probas: &CoeffProbas,
) -> Vec<MbPlan> {
    let mut mb_plans = Vec::with_capacity(mb_w * mb_h);
    for mb_y in 0..mb_h {
        for mb_x in 0..mb_w {
            let segment = seg_ids[mb_y * mb_w + mb_x];
            let plan = QuantPlan {
                quant: quants[usize::from(segment)],
                uses_trellis: gates.uses_trellis,
                sns_strength: gates.sns_strength,
                probas,
            };
            mb_plans.push(plan_one_mb(
                planes,
                src,
                mb_x,
                mb_y,
                mb_y > 0,
                mb_x > 0,
                mb_x == mb_w - 1,
                segment,
                plan,
                gates,
            ));
        }
    }
    mb_plans
}

/// Drive the per-macroblock planning. Serial in the default build; the `rayon`
/// build overrides this with a wavefront-parallel scheduler (see the cfg'd
/// definition below).
#[cfg(not(feature = "rayon"))]
#[expect(
    clippy::too_many_arguments,
    reason = "forwards the planner's inputs (planes, source, segment map/quantizers, \
              grid size, gates, trellis cost model) straight to `plan_frame_serial`"
)]
fn run_mb_planning(
    planes: &mut Planes,
    src: &SourceYuv,
    seg_ids: &[u8],
    quants: &[Quantizer; NUM_MB_SEGMENTS],
    mb_w: usize,
    mb_h: usize,
    gates: SearchGates,
    probas: &CoeffProbas,
) -> Vec<MbPlan> {
    plan_frame_serial(planes, src, seg_ids, quants, mb_w, mb_h, gates, probas)
}

/// Below this macroblock count the wavefront's fork/join overhead outweighs the
/// per-macroblock search, so small frames fall back to the serial planner.
#[cfg(feature = "rayon")]
const PARALLEL_MB_THRESHOLD: usize = 256;

/// One reconstructed macroblock's committable pixels (luma 16×16, chroma 8×8 each),
/// carried out of a wavefront task so the serial commit can write them into the
/// shared frame planes.
#[cfg(feature = "rayon")]
struct ReconBlocks {
    y: [u8; 256],
    u: [u8; 64],
    v: [u8; 64],
}

/// Wavefront-parallel planner. Anti-diagonal `d = skew*mb_y + mb_x` orders the grid
/// so every neighbor a macroblock reads — left `(x-1,y)`, top `(x,y-1)`, top-left
/// `(x-1,y-1)`, and (for i4x4) top-right `(x+1,y-1)` — lies in a strictly earlier
/// diagonal, while macroblocks sharing a diagonal are mutually independent. Each
/// diagonal's macroblocks are searched in parallel against a per-macroblock scratch
/// buffer seeded with the neighbor halo copied from the (already-committed) shared
/// planes; results are committed serially before the next diagonal. `plan_one_mb`
/// reads no raster-threaded state, so this is byte-identical to
/// [`plan_frame_serial`] (proven by a `rayon`-gated test).
///
/// `skew` is 2 when the intra-4×4 top-right lane `(x+1,y-1)` is live (`Best`) so it
/// lands in an earlier diagonal, else 1 (`Balanced`: no top-right dependency → wider
/// diagonals). Coarsening the wavefront unit to tiles was tried and reverted: the
/// per-MB search is already parallelized to the Amdahl ceiling set by the serial
/// entropy-coding emit, so larger units only added overhead.
#[cfg(feature = "rayon")]
#[expect(
    clippy::too_many_arguments,
    reason = "the wavefront planner takes the target planes, the source, the segment \
              map/quantizers, the grid size, the search gates and the trellis cost \
              model — independent inputs seeded into each per-macroblock task"
)]
fn run_mb_planning(
    planes: &mut Planes,
    src: &SourceYuv,
    seg_ids: &[u8],
    quants: &[Quantizer; NUM_MB_SEGMENTS],
    mb_w: usize,
    mb_h: usize,
    gates: SearchGates,
    probas: &CoeffProbas,
) -> Vec<MbPlan> {
    use rayon::prelude::*;

    // Parallelize only when the per-macroblock work is heavy enough to amortize the
    // scratch-buffer seeding and fork/join overhead: a large-enough grid AND an
    // effort that actually runs the rate-distortion search. The `Fast` tier (fixed
    // DC prediction, no mode search or trellis) does almost no per-macroblock work,
    // so the wavefront would only add overhead — it stays serial.
    let n = mb_w * mb_h;
    if n < PARALLEL_MB_THRESHOLD || !gates.search_modes {
        return plan_frame_serial(planes, src, seg_ids, quants, mb_w, mb_h, gates, probas);
    }

    // Skew 2 pushes the i4x4 top-right neighbor to an earlier diagonal; Balanced
    // (no top-right) uses skew 1 for wider, fewer diagonals.
    let skew = if gates.uses_i4x4 { 2 } else { 1 };

    // Accumulate every macroblock's plan tagged with its raster index; a final
    // sort restores raster order without needing a placeholder-filled buffer.
    let mut indexed: Vec<(usize, MbPlan)> = Vec::with_capacity(n);
    let num_diagonals = skew * (mb_h - 1) + (mb_w - 1) + 1;
    for d in 0..num_diagonals {
        let members = diagonal_members(d, mb_w, mb_h, skew);
        // Scope the shared borrow of `planes` to the parallel map so the serial
        // commit below can take it mutably.
        let results: Vec<(usize, MbPlan, ReconBlocks)> = {
            let planes_ref: &Planes = planes;
            members
                .par_iter()
                .with_max_len(1)
                .map(|&(mb_x, mb_y)| {
                    let segment = seg_ids[mb_y * mb_w + mb_x];
                    let plan = QuantPlan {
                        quant: quants[usize::from(segment)],
                        uses_trellis: gates.uses_trellis,
                        sns_strength: gates.sns_strength,
                        probas,
                    };
                    // Reuse this worker thread's scratch pair (allocated once) instead
                    // of allocating a mini `Planes`/`SourceYuv` per macroblock.
                    SCRATCH.with_borrow_mut(|slot| {
                        let scratch = slot.get_or_insert_with(MbScratch::new);
                        scratch.reseed(planes_ref, src, mb_x, mb_y);
                        let mbplan = plan_one_mb(
                            &mut scratch.mini,
                            &scratch.msrc,
                            0,
                            0,
                            mb_y > 0,
                            mb_x > 0,
                            mb_x == mb_w - 1,
                            segment,
                            plan,
                            gates,
                        );
                        (mb_y * mb_w + mb_x, mbplan, extract_blocks(&scratch.mini))
                    })
                })
                .collect()
        };
        for (idx, mbplan, blocks) in results {
            commit_blocks(planes, idx, mb_w, &blocks);
            indexed.push((idx, mbplan));
        }
    }
    indexed.sort_by_key(|&(idx, _)| idx);
    indexed.into_iter().map(|(_, plan)| plan).collect()
}

/// The `(mb_x, mb_y)` members of anti-diagonal `d` for the given `skew`
/// (`mb_x = d - skew*mb_y`), in increasing `mb_y`. Ordering is irrelevant to
/// correctness (members are independent and write distinct grid cells), but fixed
/// for a deterministic commit sequence.
#[cfg(feature = "rayon")]
fn diagonal_members(d: usize, mb_w: usize, mb_h: usize, skew: usize) -> Vec<(usize, usize)> {
    let mut members = Vec::new();
    let mb_y_max = (d / skew).min(mb_h - 1);
    for mb_y in 0..=mb_y_max {
        let mb_x = d - skew * mb_y;
        if mb_x < mb_w {
            members.push((mb_x, mb_y));
        }
    }
    members
}

/// A rayon-worker-local scratch pair reused across every macroblock: a 1×1 mini
/// [`Planes`] and a 1×1 mini [`SourceYuv`]. Allocated once per worker thread (via
/// the [`SCRATCH`] thread-local) and reseeded in place per macroblock, so the
/// wavefront does zero per-macroblock heap allocation.
#[cfg(feature = "rayon")]
struct MbScratch {
    mini: Planes,
    msrc: SourceYuv,
}

#[cfg(feature = "rayon")]
impl MbScratch {
    /// Allocate the reusable buffers once (mini planes + one-macroblock source).
    fn new() -> Self {
        Self {
            mini: Planes::new(1, 1),
            msrc: SourceYuv {
                y: vec![0u8; 256],
                u: vec![0u8; 64],
                v: vec![0u8; 64],
                mb_w: 1,
                mb_h: 1,
            },
        }
    }

    /// Reseed both buffers for macroblock `(mb_x, mb_y)` in place — no allocation.
    fn reseed(&mut self, planes: &Planes, src: &SourceYuv, mb_x: usize, mb_y: usize) {
        reseed_mini_planes(&mut self.mini, planes, mb_x, mb_y);
        reseed_mini_src(&mut self.msrc, src, mb_x, mb_y);
    }
}

#[cfg(feature = "rayon")]
thread_local! {
    /// One reusable [`MbScratch`] per rayon worker thread, materialized on first use
    /// and reused for every macroblock the thread ever plans (across all diagonals
    /// and all encodes on the shared pool).
    static SCRATCH: core::cell::RefCell<Option<MbScratch>> =
        const { core::cell::RefCell::new(None) };
}

/// Reseed a mini [`Planes`] for macroblock `(mb_x, mb_y)`: overwrite the neighbor
/// halo (top row + top-right margin, left column, top-left corner) with the bytes
/// the serial planner would read from the shared `planes`, so the macroblock —
/// placed at local `(0, 0)` — predicts identically. Frame-edge `127`/`129` borders
/// ride along automatically (they are the actual bytes in `planes`). The macroblock
/// interior and the intra-4×4 lane margins are fully rewritten by the prediction /
/// reconstruction of this macroblock, so leftover bytes from a previously-planned
/// macroblock in the same reused buffer are never read (the serial-equality test
/// guards this).
#[cfg(feature = "rayon")]
fn reseed_mini_planes(mini: &mut Planes, planes: &Planes, mb_x: usize, mb_y: usize) {
    let ys = planes.y_stride;
    let y_off = (mb_y * 16 + 1) * ys + (mb_x * 16 + 1);
    let mys = mini.y_stride;
    // Top row: corner + 16 top + 4 top-right margin = 21 samples into mini row 0.
    let top = y_off - ys - 1;
    mini.y[0..21].copy_from_slice(&planes.y[top..top + 21]);
    // Left column: 16 samples into mini column 0, rows 1..17.
    for j in 0..16 {
        mini.y[(j + 1) * mys] = planes.y[y_off + j * ys - 1];
    }
    let uvs = planes.uv_stride;
    let uv_off = (mb_y * 8 + 1) * uvs + (mb_x * 8 + 1);
    let muvs = mini.uv_stride;
    // Chroma top row: corner + 8 top = 9 samples (no top-right for 8×8 predictors).
    let ctop = uv_off - uvs - 1;
    mini.u[0..9].copy_from_slice(&planes.u[ctop..ctop + 9]);
    mini.v[0..9].copy_from_slice(&planes.v[ctop..ctop + 9]);
    for j in 0..8 {
        mini.u[(j + 1) * muvs] = planes.u[uv_off + j * uvs - 1];
        mini.v[(j + 1) * muvs] = planes.v[uv_off + j * uvs - 1];
    }
}

/// Reseed a 1×1 [`SourceYuv`] in place with macroblock `(mb_x, mb_y)`'s source
/// samples, so `plan_one_mb`'s local `(0, 0)` source reads land correctly
/// (`y_stride()=16`, `uv_stride()=8`).
#[cfg(feature = "rayon")]
fn reseed_mini_src(msrc: &mut SourceYuv, src: &SourceYuv, mb_x: usize, mb_y: usize) {
    let ys = src.y_stride();
    for r in 0..16 {
        let s = (mb_y * 16 + r) * ys + mb_x * 16;
        msrc.y[r * 16..r * 16 + 16].copy_from_slice(&src.y[s..s + 16]);
    }
    let uvs = src.uv_stride();
    for r in 0..8 {
        let s = (mb_y * 8 + r) * uvs + mb_x * 8;
        msrc.u[r * 8..r * 8 + 8].copy_from_slice(&src.u[s..s + 8]);
        msrc.v[r * 8..r * 8 + 8].copy_from_slice(&src.v[s..s + 8]);
    }
}

/// Read the reconstructed 16×16 luma / 8×8 U,V out of a solved mini buffer.
#[cfg(feature = "rayon")]
fn extract_blocks(mini: &Planes) -> ReconBlocks {
    let mys = mini.y_stride;
    let y_off = mys + 1;
    let mut y = [0u8; 256];
    for r in 0..16 {
        let s = y_off + r * mys;
        y[r * 16..r * 16 + 16].copy_from_slice(&mini.y[s..s + 16]);
    }
    let muvs = mini.uv_stride;
    let uv_off = muvs + 1;
    let mut u = [0u8; 64];
    let mut v = [0u8; 64];
    for r in 0..8 {
        let s = uv_off + r * muvs;
        u[r * 8..r * 8 + 8].copy_from_slice(&mini.u[s..s + 8]);
        v[r * 8..r * 8 + 8].copy_from_slice(&mini.v[s..s + 8]);
    }
    ReconBlocks { y, u, v }
}

/// Commit a reconstructed macroblock's pixels into the shared frame planes.
#[cfg(feature = "rayon")]
fn commit_blocks(planes: &mut Planes, idx: usize, mb_w: usize, b: &ReconBlocks) {
    let (mb_y, mb_x) = (idx / mb_w, idx % mb_w);
    let ys = planes.y_stride;
    let y_off = (mb_y * 16 + 1) * ys + (mb_x * 16 + 1);
    for r in 0..16 {
        let dst = y_off + r * ys;
        planes.y[dst..dst + 16].copy_from_slice(&b.y[r * 16..r * 16 + 16]);
    }
    let uvs = planes.uv_stride;
    let uv_off = (mb_y * 8 + 1) * uvs + (mb_x * 8 + 1);
    for r in 0..8 {
        let dst = uv_off + r * uvs;
        planes.u[dst..dst + 8].copy_from_slice(&b.u[r * 8..r * 8 + 8]);
        planes.v[dst..dst + 8].copy_from_slice(&b.v[r * 8..r * 8 + 8]);
    }
}

/// Pass 2 with coefficient-probability optimization. Tallies the buffered plans'
/// token statistics, derives an optimized probability table, emits the control +
/// token partitions once with the defaults and once with the optimized table, and
/// keeps whichever pair is smaller (ties → default, deterministically). Returns the
/// chosen `(part0, token)` bytes and both candidate totals (`part0.len() +
/// token.len()`) so tests can observe the shrink.
///
/// Only entropy coding differs between the two candidates; the tokens dequantize to
/// the same coefficients and the transmitted probabilities exactly match those used
/// to code them, so the decoder's reconstruction is identical either way. The
/// default candidate reproduces the pre-optimization bytes, so the output is never
/// larger than it.
fn emit_best_partitions(
    mb_plans: &[MbPlan],
    mb_w: usize,
    mb_h: usize,
    header: HeaderParams<'_>,
    skip: SkipCoding,
) -> (Vec<u8>, Vec<u8>, usize, usize, CoeffProbas) {
    // Count pass: tally the token statistics with the identical NzContext threading
    // that emission uses (including skipping the same macroblocks), then derive the
    // optimized table. The statistics table is ~16 KiB, so it lives on the heap.
    let mut stats = Box::<CoeffStats>::default();
    let mut ctx = NzContext::new(mb_w);
    work!(TokenPartitionWalk);
    for mb_y in 0..mb_h {
        for mb_x in 0..mb_w {
            let plan = &mb_plans[mb_y * mb_w + mb_x];
            if skip.use_skip && plan.skippable {
                ctx.skip_mb(mb_x);
            } else {
                count_mb_residuals(&mut stats, &plan.tokens, &mut ctx, mb_x);
            }
        }
        ctx.init_scanline();
    }
    let (opt_probas, opt_updated) = prob_opt::optimize_probas(&stats);

    // The default-table and optimized-table candidates are independent full
    // emission passes over the same plans; evaluate both (in parallel under
    // `rayon`), then keep the smaller.
    let ((part0_d, token_d), (part0_o, token_o)) = emit_partition_candidates(
        mb_plans,
        mb_w,
        mb_h,
        header,
        skip,
        &opt_probas,
        &opt_updated,
    );
    let default_total = part0_d.len() + token_d.len();
    let optimized_total = part0_o.len() + token_o.len();

    if optimized_total < default_total {
        (part0_o, token_o, default_total, optimized_total, opt_probas)
    } else {
        (part0_d, token_d, default_total, optimized_total, opt_probas)
    }
}

/// Evaluate the two entropy-coding candidates for `emit_best_partitions`: the
/// default probability table and the optimized one. They read only immutable
/// inputs (the plans, header and the two proba tables) and produce independent
/// byte vectors, so the pair — and therefore the smaller-of-two choice — is
/// identical however they are scheduled.
///
/// `(default, optimized)` where each is the `(part0, token)` byte pair.
#[cfg(not(feature = "rayon"))]
#[expect(
    clippy::type_complexity,
    reason = "returns the two candidates' (part0, token) byte pairs; naming a \
              struct for this one internal call-site would not aid clarity"
)]
fn emit_partition_candidates(
    mb_plans: &[MbPlan],
    mb_w: usize,
    mb_h: usize,
    header: HeaderParams<'_>,
    skip: SkipCoding,
    opt_probas: &CoeffProbas,
    opt_updated: &CoeffUpdateFlags,
) -> ((Vec<u8>, Vec<u8>), (Vec<u8>, Vec<u8>)) {
    let no_updates = CoeffUpdateFlags::default();
    let default = emit_partitions(
        mb_plans,
        mb_w,
        mb_h,
        header,
        &COEFFS_PROBA_0,
        &no_updates,
        skip,
    );
    let optimized = emit_partitions(mb_plans, mb_w, mb_h, header, opt_probas, opt_updated, skip);
    (default, optimized)
}

/// Parallel counterpart: the two independent emission passes run on the rayon pool
/// via `join`. Byte-identical to the serial version — `join` returns both results
/// and the caller's comparison is order-independent (ties resolve to default).
#[cfg(feature = "rayon")]
#[expect(
    clippy::type_complexity,
    reason = "returns the two candidates' (part0, token) byte pairs; naming a \
              struct for this one internal call-site would not aid clarity"
)]
fn emit_partition_candidates(
    mb_plans: &[MbPlan],
    mb_w: usize,
    mb_h: usize,
    header: HeaderParams<'_>,
    skip: SkipCoding,
    opt_probas: &CoeffProbas,
    opt_updated: &CoeffUpdateFlags,
) -> ((Vec<u8>, Vec<u8>), (Vec<u8>, Vec<u8>)) {
    let no_updates = CoeffUpdateFlags::default();
    rayon::join(
        || {
            emit_partitions(
                mb_plans,
                mb_w,
                mb_h,
                header,
                &COEFFS_PROBA_0,
                &no_updates,
                skip,
            )
        },
        || emit_partitions(mb_plans, mb_w, mb_h, header, opt_probas, opt_updated, skip),
    )
}

/// Emit the control partition (header + intra modes) and the token partition for
/// the buffered plans, coding the residuals against `probas` and transmitting that
/// table via `updated`. When `use_skip` is set, each macroblock prefixes an
/// explicit skip flag (probability `skip_p`) and a skippable one emits no residual
/// tokens — its non-zero context is cleared instead, mirroring the decoder's
/// `skip_residuals`. Both encoders are flushed and returned as raw bytes.
fn emit_partitions(
    mb_plans: &[MbPlan],
    mb_w: usize,
    mb_h: usize,
    header: HeaderParams<'_>,
    probas: &CoeffProbas,
    updated: &CoeffUpdateFlags,
    skip: SkipCoding,
) -> (Vec<u8>, Vec<u8>) {
    work!(TokenPartitionWalk);
    let mut part0 = BoolEncoder::new();
    write_control_header(
        &mut part0,
        header,
        probas,
        updated,
        skip.use_skip,
        skip.skip_p,
    );
    let mut token_enc = BoolEncoder::new();
    let mut ctx = NzContext::new(mb_w);
    // Encoder-side intra-mode context, threaded exactly as the decoder's
    // `Frame::{intra_t, intra_l}`: `intra_t` carries four sub-block modes per column
    // (persisting across rows), `intra_l` the four left modes (reset each row). Both
    // start at `B_DC_PRED`. For a 16×16 macroblock the context is filled with its
    // single mode, so byte-for-byte this changes nothing when no macroblock is i4x4.
    let mut intra_t = vec![B_DC_PRED; 4 * mb_w];
    let mut intra_l = [B_DC_PRED; 4];
    for mb_y in 0..mb_h {
        for mb_x in 0..mb_w {
            let plan = &mb_plans[mb_y * mb_w + mb_x];
            // Intra-mode order (mirrors `mb::parse_intra_mode`): [segment id if the
            // frame codes a segment map][skip if use_skip][is_i4x4][modes].
            if let Some(seg) = header.segments {
                put_segment_id(&mut part0, seg.tree_probs, plan.segment);
            }
            if skip.use_skip {
                part0.put_bool(skip.skip_p, plan.skippable);
            }
            put_is_i4x4(&mut part0, plan.is_i4x4);
            emit_intra_modes(&mut part0, plan, mb_x, &mut intra_t, &mut intra_l);
            put_uvmode(&mut part0, plan.uvmode);
            if skip.use_skip && plan.skippable {
                ctx.skip_mb(mb_x);
            } else {
                emit_mb_residuals(&mut token_enc, probas, &plan.tokens, &mut ctx, mb_x);
            }
        }
        ctx.init_scanline();
        intra_l = [B_DC_PRED; 4];
    }
    (part0.finish(), token_enc.finish())
}

/// Emit one macroblock's luma prediction modes into the control partition and
/// advance the top/left intra-mode context — the exact inverse of
/// `mb::parse_intra_mode`'s luma branch. An i4x4 macroblock emits its sixteen
/// sub-block modes through the `kBModesProba` top/left context (mirroring
/// `parse_i4x4_modes`, including how each written mode becomes the next left and
/// top neighbor); a 16×16 macroblock emits its one mode and fills both contexts
/// with it (mirroring the `intra_t[..].fill(ymode); intra_l = [ymode; 4]` update).
fn emit_intra_modes(
    part0: &mut BoolEncoder,
    plan: &MbPlan,
    mb_x: usize,
    intra_t: &mut [u8],
    intra_l: &mut [u8; 4],
) {
    let top = 4 * mb_x;
    if plan.is_i4x4 {
        for (y, left_slot) in intra_l.iter_mut().enumerate() {
            let mut left = *left_slot;
            for x in 0..4 {
                let t = intra_t[top + x];
                let mode = plan.imodes[y * 4 + x];
                put_bmode(part0, BMODES_PROBA[usize::from(t)][usize::from(left)], mode);
                intra_t[top + x] = mode;
                left = mode;
            }
            *left_slot = left;
        }
    } else {
        put_ymode16(part0, plan.ymode);
        intra_t[top..top + 4].fill(plan.ymode);
        *intra_l = [plan.ymode; 4];
    }
}

/// The per-segment in-loop deblocking-filter level for a segment coded at quantizer
/// index `q` under the `filter_strength` knob (`0..=100`), a fixed-point port of
/// libwebp's `SetupFilterStrength`: `level0 = 5 * filter_strength`, the quantizer step
/// `qstep = kAcTable[clip(q)] >> 2` stands in for the pixel-difference delta, and
/// `f = level0 * qstep / 256`. A result below `2` disables the filter for that segment
/// (`0`), else it clamps into `0..=63`. `filter_strength == 0` always yields `0`. The
/// sharpness term is not folded in here — it rides in the emitted [`FilterHeader`] and
/// is applied identically by `compute_fstrengths`/the decoder.
///
/// (libwebp additionally divides by `256 + beta_s`, a per-segment SNS bias we do not
/// model; `beta_s = 0` here. Byte-exact parity with `cwebp` filtering is not a goal —
/// the derivation is self-consistent because the decoder re-derives from the emitted
/// header — so the term is dropped.)
fn segment_filter_level(q: i32, filter_strength: u8, apply_filter: bool) -> i32 {
    if !apply_filter || filter_strength == 0 {
        return 0;
    }
    let level0 = 5 * i32::from(filter_strength);
    let idx = usize::try_from(q.clamp(0, 127)).unwrap_or(0);
    let qstep = i32::from(AC_TABLE[idx]) >> 2;
    let f = level0 * qstep / 256;
    if f < 2 { 0 } else { f.min(63) }
}

/// Choose the frame's in-loop deblocking-filter parameters. The normal (non-simple)
/// filter is used at a base level derived from the frame's base quantizer and the
/// `filter_strength` knob ([`segment_filter_level`]), so it strengthens with coarser
/// quantization and is `0` at the highest quality (or when `filter_strength == 0`). The
/// `filter_sharpness` knob (`0..=7`) rides in the header and is applied by the decoder.
/// When `apply_filter` is false (the `Fast` method) the level is `0`, so the filter is
/// off and the frame is byte-identical to an unfiltered encode.
fn choose_filter(
    base_q: i32,
    apply_filter: bool,
    filter_strength: u8,
    filter_sharpness: u8,
) -> FilterHeader {
    FilterHeader {
        simple: false,
        level: segment_filter_level(base_q, filter_strength, apply_filter),
        sharpness: i32::from(filter_sharpness),
        ..FilterHeader::default()
    }
}

/// The per-segment loop-filter strength deltas emitted in the segment header, each
/// relative to the base `filter.level` (the decoder adds `filter.level` back, since the
/// deltas are coded `absolute_delta = false`). A busier segment (coarser quantizer)
/// deblocks harder than a flat one. Unused segments (`count..4`) carry `0`.
fn segment_filter_deltas(
    base_level: i32,
    seg_base_q: [i32; 4],
    apply_filter: bool,
    filter_strength: u8,
) -> [i32; 4] {
    let mut deltas = [0i32; 4];
    for (delta, &q) in deltas.iter_mut().zip(&seg_base_q) {
        *delta = segment_filter_level(q, filter_strength, apply_filter) - base_level;
    }
    deltas
}

/// The VP8 filter type of a header: `0` off (level 0), `1` simple, `2` normal —
/// exactly the decoder's `parse_filter_header` derivation.
const fn filter_type_of(filter: &FilterHeader) -> u8 {
    if filter.level == 0 {
        0
    } else if filter.simple {
        1
    } else {
        2
    }
}

/// Deblock the reconstructed `planes` with the frame-final in-loop filter, using
/// the exact per-macroblock [`FInfo`] the decoder re-derives from the same header.
/// A no-op when the chosen filter level is 0.
///
/// Each macroblock's `f_inner` must match the decoder's `resolve_finfo`, which is
/// driven by its filter strength (per-segment), whether it is coded i4x4
/// (`plan.is_i4x4`; an i4x4 MB always forces `f_inner = true`), its skip flag, and
/// whether its residual is non-zero. The encoder codes `skip = plan.skippable` and
/// its emitted tokens dequantize to exactly the reconstructed coefficients, so a
/// lightweight `MbData` carrying `skip = plan.skippable` and
/// `non_zero_y = plan.has_residual` reproduces the decoder's
/// `(non_zero_y | non_zero_uv) == 0` test — and therefore its `f_inner` —
/// byte-for-byte. `use_skip` must equal the header's skip flag so the
/// `use_skip && skip` term matches on both sides.
///
/// `seg_params` is the exact segment header the control partition emitted (per-segment
/// quantizer and filter-strength deltas), so rebuilding a matching [`SegmentHeader`]
/// and threading each macroblock's `plan.segment` reproduces the decoder's per-segment
/// [`compute_fstrengths`] table byte-for-byte. An unsegmented frame passes `None` and
/// every macroblock resolves against the single base strength.
fn apply_loop_filter(
    planes: &mut Planes,
    mb_plans: &[MbPlan],
    mb_w: usize,
    mb_h: usize,
    filter: &FilterHeader,
    use_skip: bool,
    seg_params: Option<SegmentParams>,
) {
    let filter_type = filter_type_of(filter);
    if filter_type == 0 {
        return;
    }
    // Rebuild the emitted segment header: with per-segment filter-strength deltas
    // (relative, `absolute_delta = false`) `compute_fstrengths` yields a distinct
    // strength per segment, and `plan.segment` selects it — matching the decoder.
    let segment = seg_params.map_or(
        SegmentHeader {
            use_segment: false,
            update_map: false,
            absolute_delta: true,
            quantizer: [0; NUM_MB_SEGMENTS],
            filter_strength: [0; NUM_MB_SEGMENTS],
        },
        |seg| SegmentHeader {
            use_segment: true,
            update_map: true,
            absolute_delta: false,
            quantizer: seg.quantizer,
            filter_strength: seg.filter_strength,
        },
    );
    let fstrengths = compute_fstrengths(&segment, filter);
    let finfo: Vec<FInfo> = mb_plans
        .iter()
        .map(|plan| {
            let mb = MbData {
                segment: plan.segment,
                is_i4x4: plan.is_i4x4,
                skip: plan.skippable,
                non_zero_y: u32::from(plan.has_residual),
                non_zero_uv: 0,
                ..MbData::default()
            };
            resolve_finfo(fstrengths, &mb, use_skip)
        })
        .collect();
    filter_frame(planes, &finfo, mb_w, mb_h, filter_type);
}

/// One frame's quantization plan: the derived per-plane quantizers and whether to
/// select coefficient levels by rate-distortion trellis (`Full`/`Best`) or
/// round-to-nearest (`Fast`). Threaded as one value so the macroblock and intra-4×4
/// encode paths stay within their argument budgets.
#[derive(Clone, Copy)]
struct QuantPlan<'a> {
    /// The per-plane forward quantizers for the frame's base index.
    quant: Quantizer,
    /// Whether to run trellis quantization instead of round-to-nearest.
    uses_trellis: bool,
    /// Spatial-noise-shaping strength (`0..=100`) weighting the perceptual texture
    /// term in the luma mode decision; `0` reduces the decision to plain SSE.
    sns_strength: u8,
    /// The coefficient-probability table the trellis charges its rate against. Pass 1
    /// (the byte-identical default) uses [`COEFFS_PROBA_0`]; a multi-pass encode threads
    /// the previous pass's optimized table so the level decisions converge with the
    /// distribution that will actually code them (libwebp's `StatLoop`).
    probas: &'a CoeffProbas,
}

/// Quantize one 4×4 coefficient block, choosing rate-distortion trellis levels
/// (`uses_trellis`, the Balanced/Best path) or round-to-nearest (`Fast`). Both
/// return the same self-consistent [`Quantized`] (`recon = level * q`), so the choice
/// only trades size for quality. `plane` is the token type (0 i16-AC, 1 Y2, 2 chroma,
/// 3 i4-AC) and `first` the starting zig-zag position; the entry non-zero context is
/// approximated as 0 (it only tunes the rate estimate, never self-consistency).
fn quantize_one(
    coeffs: [i16; 16],
    pair: QPair,
    first: usize,
    plane: usize,
    uses_trellis: bool,
    probas: &CoeffProbas,
) -> Quantized {
    work!(QuantizeCall);
    if uses_trellis {
        let lambda = trellis_lambda(pair.ac.q);
        trellis_quantize_block(coeffs, pair, first, 0, plane, probas, lambda)
    } else {
        quantize_block(coeffs, pair.dc, pair.ac, first)
    }
}

/// One macroblock's 16×16-luma encode: the dequantized luma coefficients (blocks
/// `0..16`, decoder layout — DC carried by Y2), the Y2 token block and the sixteen
/// luma AC token blocks.
struct Luma16Encode {
    /// Dequantized luma coefficients (16 blocks × 16, natural order per block).
    coeffs: [i16; 256],
    /// The second-order (Y2) token block.
    y2: Block,
    /// The sixteen luma AC token blocks (`first = 1`).
    blocks: [Block; 16],
}

/// One macroblock's 8×8-chroma encode: the dequantized chroma coefficients (U
/// blocks `0..4`, V blocks `4..8`) and their eight token blocks.
struct Chroma8Encode {
    /// Dequantized chroma coefficients (8 blocks × 16, natural order per block).
    coeffs: [i16; 128],
    /// The eight chroma token blocks (U `0..4`, V `4..8`, `first = 0`).
    blocks: [Block; 8],
}

/// Transform and quantize the 16×16-luma residual against the prediction already
/// written into `planes.y`, returning the dequantized luma coefficients (for
/// reconstruction) and the Y2 + luma AC token blocks (for emission and rate).
fn encode_luma16_residual(
    src: &SourceYuv,
    planes: &Planes,
    mb_x: usize,
    mb_y: usize,
    plan: QuantPlan<'_>,
) -> Luma16Encode {
    let QuantPlan {
        quant,
        uses_trellis,
        ..
    } = plan;
    // The 16 luma blocks occupy decoder indices 0..256; a full 384 scratch lets the
    // shared `transform_wht` scatter the reconstructed DCs into their slots.
    let mut coeffs = [0i16; 384];
    let y_off = (mb_y * 16 + 1) * planes.y_stride + (mb_x * 16 + 1);

    // Forward DCT of each 4×4 residual, collecting the DCs for Y2.
    let mut luma_coeffs = [[0i16; 16]; 16];
    let mut dcs = [0i16; 16];
    for n in 0..16 {
        let (bx, by) = ((n % 4) * 4, (n / 4) * 4);
        let residual = residual_block(
            &src.y,
            src.y_stride(),
            mb_x * 16 + bx,
            mb_y * 16 + by,
            &planes.y,
            y_off + by * planes.y_stride + bx,
            planes.y_stride,
        );
        luma_coeffs[n] = fdct4x4(residual);
        dcs[n] = luma_coeffs[n][0];
    }

    // Y2: forward-WHT the 16 DCs, quantize, and scatter the reconstructed DC into
    // each luma block's DC slot exactly as the decoder does.
    let y2 = quantize_one(fwht(dcs), quant.y2, 0, 1, uses_trellis, plan.probas);
    if y2.last + 1 > 1 {
        transform_wht(y2.recon, &mut coeffs);
    } else {
        let dc0 = ((i32::from(y2.recon[0]) + 3) >> 3) as i16;
        for b in 0..16 {
            coeffs[b * 16] = dc0;
        }
    }

    // Luma AC: quantize positions 1..16 with the luma factors (DC carried by Y2).
    let mut blocks = [Block::default(); 16];
    for n in 0..16 {
        let q = quantize_one(luma_coeffs[n], quant.y1, 1, 0, uses_trellis, plan.probas);
        coeffs[n * 16 + 1..n * 16 + 16].copy_from_slice(&q.recon[1..16]);
        blocks[n] = Block {
            levels: q.levels,
            last: q.last,
        };
    }

    let mut luma = [0i16; 256];
    luma.copy_from_slice(&coeffs[..256]);
    Luma16Encode {
        coeffs: luma,
        y2: Block {
            levels: y2.levels,
            last: y2.last,
        },
        blocks,
    }
}

/// Transform and quantize the 8×8-chroma residual against the prediction already
/// written into `planes.u` / `planes.v`, returning the dequantized chroma
/// coefficients and their eight token blocks (U `0..4`, then V `4..8`).
fn encode_chroma8_residual(
    src: &SourceYuv,
    planes: &Planes,
    mb_x: usize,
    mb_y: usize,
    plan: QuantPlan<'_>,
) -> Chroma8Encode {
    let QuantPlan {
        quant,
        uses_trellis,
        ..
    } = plan;
    let uv_off = (mb_y * 8 + 1) * planes.uv_stride + (mb_x * 8 + 1);
    let mut coeffs = [0i16; 128];
    let mut blocks = [Block::default(); 8];
    for (which, plane) in [&planes.u, &planes.v].into_iter().enumerate() {
        let src_plane = if which == 0 { &src.u } else { &src.v };
        for n in 0..4 {
            let (bx, by) = ((n % 2) * 4, (n / 2) * 4);
            let residual = residual_block(
                src_plane,
                src.uv_stride(),
                mb_x * 8 + bx,
                mb_y * 8 + by,
                plane,
                uv_off + by * planes.uv_stride + bx,
                planes.uv_stride,
            );
            let q = quantize_one(fdct4x4(residual), quant.uv, 0, 2, uses_trellis, plan.probas);
            let idx = which * 4 + n;
            coeffs[idx * 16..idx * 16 + 16].copy_from_slice(&q.recon);
            blocks[idx] = Block {
                levels: q.levels,
                last: q.last,
            };
        }
    }
    Chroma8Encode { coeffs, blocks }
}

/// Transform and quantize one macroblock's residual, returning the dequantized
/// coefficient block (`[i16; 384]`, decoder layout) for reconstruction and the
/// [`MbTokens`] (levels) for emission. Reads the source samples and the
/// prediction already written into `planes`. Used by the fast path (fixed `DC`);
/// the RD path calls [`encode_luma16_residual`] / [`encode_chroma8_residual`]
/// per candidate mode.
fn encode_mb(
    src: &SourceYuv,
    planes: &Planes,
    mb_x: usize,
    mb_y: usize,
    plan: QuantPlan<'_>,
) -> ([i16; 384], MbTokens) {
    let luma = encode_luma16_residual(src, planes, mb_x, mb_y, plan);
    let chroma = encode_chroma8_residual(src, planes, mb_x, mb_y, plan);
    let mut coeffs = [0i16; 384];
    coeffs[..256].copy_from_slice(&luma.coeffs);
    coeffs[256..].copy_from_slice(&chroma.coeffs);
    (
        coeffs,
        MbTokens {
            is_i4x4: false,
            y2: luma.y2,
            luma: luma.blocks,
            chroma: chroma.blocks,
        },
    )
}

/// The `src - pred` residual of one 4×4 block (natural raster order). `src` is a
/// row-contiguous source plane read at `(src_x, src_y)`; `pred` is a padded
/// reconstruction plane read at `pred_off`.
pub(crate) fn residual_block(
    src: &[u8],
    src_stride: usize,
    src_x: usize,
    src_y: usize,
    pred: &[u8],
    pred_off: usize,
    pred_stride: usize,
) -> [i16; 16] {
    let mut residual = [0i16; 16];
    for row in 0..4 {
        // Per-row contiguous 4-wide slices of the source, prediction and output:
        // each is bounds-checked once, not per column, and the per-element index
        // arithmetic (`(src_y+row)*src_stride + src_x + col`, recomputed 16×) becomes
        // a walking pointer. That scalar tightening measures ~1.4× at the 4×4 block
        // (`just bench-kernels residual_block`); like `sse_block` it is *not*
        // vectorized (`--emit asm` shows the fully-unrolled block stays scalar on the
        // baseline SSE2 target). Each byte pair differs by `-255..=255`, which fits
        // `i16` exactly, so `i16::from(s) - i16::from(p)` equals the old
        // `(i32 - i32) as i16` bit for bit.
        let s_row = &src[(src_y + row) * src_stride + src_x..][..4];
        let p_row = &pred[pred_off + row * pred_stride..][..4];
        for (out, (&s, &p)) in residual[row * 4..][..4]
            .iter_mut()
            .zip(s_row.iter().zip(p_row))
        {
            *out = i16::from(s) - i16::from(p);
        }
    }
    residual
}

/// The pre-optimization [`residual_block`] verbatim: a flat nested loop with
/// per-element index arithmetic folding `(i32 - i32) as i16` into the output array.
/// The slice-mapped [`residual_block`] must return this array bit for bit (the
/// difference is in `-255..=255`, so the narrower `i16` subtraction is exact).
/// Compiled only for the equivalence proptest (`test`) and the `kernels` microbench
/// (`bench` feature) — never in a real build.
#[cfg(any(test, feature = "bench"))]
pub(crate) fn residual_block_reference(
    src: &[u8],
    src_stride: usize,
    src_x: usize,
    src_y: usize,
    pred: &[u8],
    pred_off: usize,
    pred_stride: usize,
) -> [i16; 16] {
    let mut residual = [0i16; 16];
    for row in 0..4 {
        for col in 0..4 {
            let s = i32::from(src[(src_y + row) * src_stride + (src_x + col)]);
            let p = i32::from(pred[pred_off + row * pred_stride + col]);
            residual[row * 4 + col] = (s - p) as i16;
        }
    }
    residual
}

/// One macroblock's winning intra-4×4 luma candidate: the sixteen sub-block modes,
/// their quantized token blocks, and the dequantized luma coefficients (blocks
/// `0..16`, decoder layout — no Y2).
struct I4x4Luma {
    /// The sixteen 4×4 sub-block modes in raster order (`imodes[0..16]`).
    imodes: [u8; 16],
    /// The sixteen 4×4 token blocks (quantized levels, `first = 0`).
    luma: [Block; 16],
    /// The dequantized luma coefficients in decoder layout (16 blocks × 16).
    coeffs: [i16; 256],
}

/// The signaling cost (1/256-bit units) charged to a 16×16 luma mode and to each
/// intra-4×4 sub-block mode in the rate term of the i4x4-vs-16×16 decision — roughly
/// two coded bits (`2 * 256`). The gap (one 16×16 mode vs sixteen sub-block modes) is
/// what keeps smooth content on the cheaper 16×16 path.
const I16X16_MODE_COST: i64 = 2 * 256;
/// Per-sub-block mode signaling cost (see [`I16X16_MODE_COST`]).
const I4X4_SUBMODE_COST: i64 = 2 * 256;
/// Right shift turning the squared luma-AC dequant step into the rate-distortion
/// multiplier `lambda` for the i4x4-vs-16×16 decision (coarser quant → larger
/// `lambda`, so bits weigh more against distortion). Tuned so i4x4 is picked on
/// detailed content but not on smooth or flat content, where the extra sub-block mode
/// bits do not pay for themselves. Kept distinct from the whole-block mode lambdas
/// ([`luma16_lambda`] / [`chroma_lambda`]): the i4/i16 mode-count trade-off is a
/// separate empirical balance from the `DC`/`V`/`H`/`TM` choice.
const I4X4_LAMBDA_SHIFT: u32 = 7;
/// Right shift for the 16×16 whole-block luma mode-decision lambda (libwebp scale
/// `lambda_i16 ≈ (3 * q^2) >> 7`).
const LUMA16_LAMBDA_SHIFT: u32 = 7;
/// Right shift for the 8×8 chroma mode-decision lambda (libwebp scale
/// `lambda_uv ≈ (3 * q^2) >> 6`).
const CHROMA_LAMBDA_SHIFT: u32 = 6;

/// The rate-distortion multiplier for the i4x4 decision, derived from the luma-AC
/// dequant step `q_ac`: `lambda = max(1, q_ac^2 >> I4X4_LAMBDA_SHIFT)`.
const fn i4x4_lambda(q_ac: i32) -> i64 {
    let q = q_ac as i64;
    let l = (q * q) >> I4X4_LAMBDA_SHIFT;
    if l < 1 { 1 } else { l }
}

/// The rate-distortion multiplier for the 16×16 luma whole-block mode decision,
/// derived from the luma-AC dequant step `q_ac`: `lambda = max(1, (3 * q^2) >>
/// LUMA16_LAMBDA_SHIFT)`. Coarser quantizers weigh the token rate more heavily
/// against the reconstruction distortion.
const fn luma16_lambda(q_ac: i32) -> i64 {
    let q = q_ac as i64;
    let l = (3 * q * q) >> LUMA16_LAMBDA_SHIFT;
    if l < 1 { 1 } else { l }
}

/// The rate-distortion multiplier for the 8×8 chroma whole-block mode decision,
/// derived from the chroma-AC dequant step `q_ac`: `lambda = max(1, (3 * q^2) >>
/// CHROMA_LAMBDA_SHIFT)`.
const fn chroma_lambda(q_ac: i32) -> i64 {
    let q = q_ac as i64;
    let l = (3 * q * q) >> CHROMA_LAMBDA_SHIFT;
    if l < 1 { 1 } else { l }
}

/// The exact token-tree bit cost (1/256-bit units) of one 16×16-luma macroblock's
/// coefficients: the Y2 block plus the sixteen luma AC blocks (`first = 1`). The
/// entry non-zero context of every block is approximated as 0 (it only tunes the rate
/// estimate for the mode comparison, never self-consistency), matching the trellis.
fn luma16_token_bits(y2: Block, luma: &[Block; 16], probas: &CoeffProbas) -> i64 {
    let mut bits = block_token_cost(y2.levels, 0, y2.last, 1, 0, probas);
    for b in luma {
        bits += block_token_cost(b.levels, 1, b.last, 0, 0, probas);
    }
    bits
}

/// The exact token-tree bit cost (1/256-bit units) of one macroblock's eight chroma
/// blocks (`first = 0`), entry context approximated as 0 (see [`luma16_token_bits`]).
fn chroma8_token_bits(chroma: &[Block; 8], probas: &CoeffProbas) -> i64 {
    chroma
        .iter()
        .map(|b| block_token_cost(b.levels, 0, b.last, 2, 0, probas))
        .sum()
}

/// Reconstruct the 16×16 luma into `planes.y` (which still holds its committed
/// prediction) by adding the inverse-transformed `coeffs` (blocks `0..16`, so a 256-
/// or 384-length slice), and return its post-quant sum of squared errors against the
/// source. The scribble is transient — the caller re-predicts / reconstructs the
/// winning candidate's pixels afterwards.
fn luma16_reconstruction_sse(
    planes: &mut Planes,
    coeffs: &[i16],
    y_off: usize,
    src: &SourceYuv,
    mb_x: usize,
    mb_y: usize,
) -> i64 {
    let stride = planes.y_stride;
    for n in 0..16 {
        let sub = y_off + (n % 4) * 4 + (n / 4) * 4 * stride;
        let block = &coeffs[n * 16..n * 16 + 16];
        if block.iter().any(|&c| c != 0) {
            transform_one(block, &mut planes.y, sub, stride);
        }
    }
    let src_stride = src.y_stride();
    let src_off = mb_y * 16 * src_stride + mb_x * 16;
    sse_block(&src.y, src_off, src_stride, &planes.y, y_off, stride, 16)
}

/// The luma **perceptual** distortion of the 16×16 reconstruction currently in
/// `planes.y` (at `y_off`) against the source macroblock: the plain reconstruction
/// `sse` plus libwebp's texture term ([`perceptual::disto16`]). The texture term is
/// gated by `tlambda = (sns_strength * q_ac) >> 5`; at `sns_strength == 0` this is
/// exactly `sse`, so the luma mode decision reduces to plain SSE. Only the luma i16/i4
/// decisions use this — chroma keeps plain SSE.
fn luma16_perceptual_disto(
    sse: i64,
    planes: &Planes,
    y_off: usize,
    src: &SourceYuv,
    mb_x: usize,
    mb_y: usize,
    plan: QuantPlan<'_>,
) -> i64 {
    let tlambda = perceptual::tlambda(plan.sns_strength, plan.quant.y1.ac.q);
    let src_stride = src.y_stride();
    let src_off = mb_y * 16 * src_stride + mb_x * 16;
    let src_win = perceptual::PlaneWindow::new(&src.y, src_off, src_stride);
    let rec_win = perceptual::PlaneWindow::new(&planes.y, y_off, planes.y_stride);
    perceptual::disto16(sse, tlambda, &src_win, &rec_win)
}

/// Encode the macroblock's luma as sixteen intra-4×4 sub-blocks, reconstructing each
/// into `planes.y` in raster order (so later sub-blocks predict from reconstructed
/// siblings, exactly as [`reconstruct::reconstruct_luma_i4x4`] does). Returns the
/// candidate, its post-quant SSE against the source, and its true token bits
/// (1/256-bit units) including the per-sub-block mode-signaling cost.
///
/// Each sub-block picks the least-SSE `B_PRED` mode (fixed `B_DC..B_HU` order, so
/// ties resolve to `B_DC`), quantizes the residual with `first = 0` (DC coded per
/// sub-block — no Y2), and reconstructs in place.
#[expect(
    clippy::too_many_arguments,
    reason = "the i4x4 search needs the plane, its offset, the source, the grid \
              position (for source indexing) and the two top-right-lane edge flags; \
              these are independent inputs, not a bundle with a natural struct"
)]
fn search_luma_i4x4(
    planes: &mut Planes,
    y_off: usize,
    src: &SourceYuv,
    mb_x: usize,
    mb_y: usize,
    has_top: bool,
    is_rightmost: bool,
    plan: QuantPlan<'_>,
) -> (I4x4Luma, i64, i64) {
    let QuantPlan {
        quant,
        uses_trellis,
        ..
    } = plan;
    let stride = planes.y_stride;
    let src_stride = src.y_stride();
    // Set up the top-right lane once, so every sub-block's predictor reads the same
    // top-right samples the decoder will reconstruct from.
    fill_top_right_lane(&mut planes.y, y_off, stride, has_top, is_rightmost);

    let mut cand = I4x4Luma {
        imodes: [0u8; 16],
        luma: [Block::default(); 16],
        coeffs: [0i16; 256],
    };
    let mut bits = 0i64;
    for n in 0..16 {
        let (bx, by) = ((n % 4) * 4, (n / 4) * 4);
        let sub = y_off + by * stride + bx;
        let src_off = (mb_y * 16 + by) * src_stride + (mb_x * 16 + bx);

        // Least-SSE B_PRED mode (numeric B_DC..B_HU order → B_DC wins ties).
        let mut best_mode = B_DC_PRED;
        let mut best_sse = i64::MAX;
        for m in 0..NUM_BMODES {
            let mode = m as u8;
            predict_luma4(&mut planes.y, sub, stride, mode);
            let sse = sse_block(&src.y, src_off, src_stride, &planes.y, sub, stride, 4);
            if sse < best_sse {
                best_sse = sse;
                best_mode = mode;
            }
        }

        // Commit the winner's prediction, transform/quantize its residual (DC coded
        // per sub-block, first = 0), then reconstruct in place.
        predict_luma4(&mut planes.y, sub, stride, best_mode);
        let residual = residual_block(
            &src.y,
            src_stride,
            mb_x * 16 + bx,
            mb_y * 16 + by,
            &planes.y,
            sub,
            stride,
        );
        let q = quantize_one(fdct4x4(residual), quant.y1, 0, 3, uses_trellis, plan.probas);
        cand.coeffs[n * 16..n * 16 + 16].copy_from_slice(&q.recon);
        if q.recon.iter().any(|&c| c != 0) {
            transform_one(&q.recon, &mut planes.y, sub, stride);
        }
        cand.imodes[n] = best_mode;
        cand.luma[n] = Block {
            levels: q.levels,
            last: q.last,
        };
        bits += block_token_cost(q.levels, 0, q.last, 3, 0, plan.probas) + I4X4_SUBMODE_COST;
    }

    let mb_src_off = mb_y * 16 * src_stride + mb_x * 16;
    let dist = sse_block(&src.y, mb_src_off, src_stride, &planes.y, y_off, stride, 16);
    (cand, dist, bits)
}

/// Decide whether the macroblock's luma is better coded as sixteen intra-4×4
/// sub-blocks than as one 16×16 block, and if so return the winning i4x4 candidate.
///
/// Both candidates are scored by the unified `cost = RD_DISTO_MULT * sse + lambda *
/// bits`, where the distortion is the post-quant reconstruction SSE against the source
/// and the rate is the true token-tree bit cost ([`block_token_cost`], 1/256-bit
/// units) plus the mode-signaling cost — the same units the whole-block mode decision
/// and the trellis use. Chroma is identical either way, so it is excluded from both
/// sides. `lambda` rises with the luma-AC dequant step (see [`i4x4_lambda`]); using
/// proper post-quant distortion — not raw prediction SSE — is what stops i4x4 from
/// being over-selected on smooth content.
#[expect(
    clippy::too_many_arguments,
    reason = "the i4x4-vs-16×16 decision needs the plane, the 16×16 coeffs/tokens to \
              score against, the source, the grid position and the two top-right-lane \
              edge flags — independent inputs, not a bundle with a natural struct"
)]
fn try_i4x4_luma(
    planes: &mut Planes,
    coeffs: &[i16; 384],
    tokens: &MbTokens,
    src: &SourceYuv,
    mb: (usize, usize),
    has_top: bool,
    is_rightmost: bool,
    plan: QuantPlan<'_>,
) -> Option<I4x4Luma> {
    let quant = plan.quant;
    let (mb_x, mb_y) = mb;
    let y_off = (mb_y * 16 + 1) * planes.y_stride + (mb_x * 16 + 1);

    // 16×16 candidate: reconstruction distortion + true token bits (Y2 + the sixteen
    // AC blocks, first = 1) + the single 16×16 mode signal. Score the perceptual
    // distortion while planes.y still holds the 16×16 reconstruction (the i4x4 search
    // overwrites it next).
    let dist16 = luma16_reconstruction_sse(planes, coeffs, y_off, src, mb_x, mb_y);
    let perceptual16 = luma16_perceptual_disto(dist16, planes, y_off, src, mb_x, mb_y, plan);
    let bits16 = I16X16_MODE_COST + luma16_token_bits(tokens.y2, &tokens.luma, plan.probas);

    // i4x4 candidate (leaves its reconstruction in planes.y; the caller's final
    // reconstruct_mb overwrites it with whichever candidate wins).
    let (cand, dist4, bits4) =
        search_luma_i4x4(planes, y_off, src, mb_x, mb_y, has_top, is_rightmost, plan);
    let perceptual4 = luma16_perceptual_disto(dist4, planes, y_off, src, mb_x, mb_y, plan);

    let lambda = i4x4_lambda(quant.y1.ac.q);
    let cost16 = RD_DISTO_MULT * perceptual16 + lambda * bits16;
    let cost4 = RD_DISTO_MULT * perceptual4 + lambda * bits4;
    (cost4 < cost16).then_some(cand)
}

/// The four whole-block intra modes, in the order the search evaluates them.
/// `DC_PRED` leads so it wins ties (it is the only always-available mode and the
/// strict-`<` comparison keeps the earliest of equal scores).
const WHOLE_BLOCK_MODES: [u8; 4] = [DC_PRED, V_PRED, H_PRED, TM_PRED];

/// Whether a whole-block predictor can run given neighbor availability: `V` needs
/// the top row, `H` the left column, `TM` both; `DC` is always available (its
/// predictor remaps itself at the frame edges).
const fn mode_available(mode: u8, has_top: bool, has_left: bool) -> bool {
    match mode {
        V_PRED => has_top,
        H_PRED => has_left,
        TM_PRED => has_top && has_left,
        _ => true,
    }
}

/// Sum of squared (source − prediction) errors over a `size`×`size` block,
/// accumulated in `i64`. `src` is a row-contiguous source plane read from
/// `src_off`; `pred` is a padded reconstruction plane read from `pred_off`. Each
/// difference is in `-255..=255`, so `d * d` stays within `i32` before widening.
pub(crate) fn sse_block(
    src: &[u8],
    src_off: usize,
    src_stride: usize,
    pred: &[u8],
    pred_off: usize,
    pred_stride: usize,
    size: usize,
) -> i64 {
    work!(SseBlock);
    let mut acc = 0i64;
    for row in 0..size {
        // Contiguous `size`-wide row slices: the bounds are checked once per row (not
        // per column), and the per-element index arithmetic collapses to a walking
        // pointer. That scalar tightening alone roughly halves the runtime at the
        // dominant 16×16 size (measured: `just bench-kernels sse_block`). It is *not*
        // vectorized: on the baseline SSE2 target LLVM emits no packed reduction for
        // this squared-difference sum (`pmaddwd` needs `i16` lanes, `pmulld` is
        // SSE4.1), and reshaping the diff to `i16` does not change that — verified via
        // `--emit asm`. Each difference squares to at most `255*255`, so a whole row
        // (`size <= 16`) sums to at most `16 * 65_025 < i32::MAX`; the per-row `i32`
        // accumulator never overflows and is widened once. Integer addition is
        // associative, so regrouping the block sum by row is bit-identical to the flat
        // `i64` fold — the returned distortion, and every RD decision it feeds, is
        // unchanged.
        let s_row = &src[src_off + row * src_stride..][..size];
        let p_row = &pred[pred_off + row * pred_stride..][..size];
        let row_sse: i32 = s_row
            .iter()
            .zip(p_row)
            .map(|(&s, &p)| {
                let d = i32::from(s) - i32::from(p);
                d * d
            })
            .sum();
        acc += i64::from(row_sse);
    }
    acc
}

/// The pre-optimization [`sse_block`] verbatim: a flat nested loop folding
/// every squared difference straight into the `i64` accumulator. The
/// slice-reduction [`sse_block`] must return this bit for bit (integer addition
/// is associative, so the per-row regrouping cannot change the sum); the
/// `work!` bump is omitted since it is measurement-only here and does not affect
/// the returned value. Compiled only for the equivalence proptest (`test`) and the
/// back-to-back `kernels` microbench (`bench` feature) — never in a real build.
#[cfg(any(test, feature = "bench"))]
pub(crate) fn sse_block_reference(
    src: &[u8],
    src_off: usize,
    src_stride: usize,
    pred: &[u8],
    pred_off: usize,
    pred_stride: usize,
    size: usize,
) -> i64 {
    let mut acc = 0i64;
    for row in 0..size {
        for col in 0..size {
            let s = i32::from(src[src_off + row * src_stride + col]);
            let p = i32::from(pred[pred_off + row * pred_stride + col]);
            let d = s - p;
            acc += i64::from(d * d);
        }
    }
    acc
}

/// Reconstruct the eight chroma blocks into `planes.u` / `planes.v` (which still
/// hold their committed prediction) by adding the inverse-transformed `coeffs` (U
/// blocks `0..4`, V blocks `4..8`), and return the summed U+V post-quant SSE against
/// the source. The scribble is transient — the caller re-predicts / reconstructs the
/// winner afterwards.
fn chroma8_reconstruction_sse(
    planes: &mut Planes,
    coeffs: &[i16; 128],
    uv_off: usize,
    src: &SourceYuv,
    mb_x: usize,
    mb_y: usize,
) -> i64 {
    let stride = planes.uv_stride;
    for n in 0..4 {
        let sub = uv_off + (n % 2) * 4 + (n / 2) * 4 * stride;
        let u_block = &coeffs[n * 16..n * 16 + 16];
        if u_block.iter().any(|&c| c != 0) {
            transform_one(u_block, &mut planes.u, sub, stride);
        }
        let v_block = &coeffs[(4 + n) * 16..(4 + n) * 16 + 16];
        if v_block.iter().any(|&c| c != 0) {
            transform_one(v_block, &mut planes.v, sub, stride);
        }
    }
    let src_stride = src.uv_stride();
    let src_off = mb_y * 8 * src_stride + mb_x * 8;
    sse_block(&src.u, src_off, src_stride, &planes.u, uv_off, stride, 8)
        + sse_block(&src.v, src_off, src_stride, &planes.v, uv_off, stride, 8)
}

/// Choose the 16×16 luma mode by full rate-distortion: for each available mode,
/// predict, transform, (trellis-)quantize and reconstruct, then score the cost
/// `RD_DISTO_MULT * reconstruction_sse + lambda * token_bits`. Returns the winning
/// mode and its quantized encode ([`Luma16Encode`]), leaving the winner's prediction
/// committed in `planes.y` for the i4x4 search / final reconstruct. The winner's
/// coefficients drive both reconstruction and emission, so no re-encode is needed.
fn select_luma16_mode_rd(
    planes: &mut Planes,
    y_off: usize,
    src: &SourceYuv,
    mb: (usize, usize),
    has_top: bool,
    has_left: bool,
    plan: QuantPlan<'_>,
) -> (u8, Luma16Encode) {
    let (mb_x, mb_y) = mb;
    let stride = planes.y_stride;
    let lambda = luma16_lambda(plan.quant.y1.ac.q);
    let mut best_mode = DC_PRED;
    let mut best_cost = i64::MAX;
    let mut best = None;
    for &mode in &WHOLE_BLOCK_MODES {
        if !mode_available(mode, has_top, has_left) {
            continue;
        }
        predict_luma16(&mut planes.y, y_off, stride, mode, has_top, has_left);
        let enc = encode_luma16_residual(src, planes, mb_x, mb_y, plan);
        let bits = I16X16_MODE_COST + luma16_token_bits(enc.y2, &enc.blocks, plan.probas);
        // Reconstruct into planes.y (which holds this candidate's prediction) to score
        // the post-quant distortion; the next iteration re-predicts over it.
        let sse = luma16_reconstruction_sse(planes, &enc.coeffs, y_off, src, mb_x, mb_y);
        let disto = luma16_perceptual_disto(sse, planes, y_off, src, mb_x, mb_y, plan);
        let cost = RD_DISTO_MULT * disto + lambda * bits;
        if cost < best_cost {
            best_cost = cost;
            best_mode = mode;
            best = Some(enc);
        }
    }
    // Re-commit the winner's prediction (overwriting the last candidate's scribble) so
    // downstream reads pure prediction.
    predict_luma16(&mut planes.y, y_off, stride, best_mode, has_top, has_left);
    // DC is always available, so `best` is always Some.
    (
        best_mode,
        best.unwrap_or_else(|| encode_luma16_residual(src, planes, mb_x, mb_y, plan)),
    )
}

/// Choose the shared 8×8 chroma mode (VP8 codes one `uvmode` for both planes) by full
/// rate-distortion over the summed U+V reconstruction, scoring the cost
/// `RD_DISTO_MULT * (u_sse + v_sse) + lambda * token_bits`. Returns the winning mode
/// and its quantized encode ([`Chroma8Encode`]), committing the winner's prediction
/// into both chroma planes.
fn select_chroma8_mode_rd(
    planes: &mut Planes,
    uv_off: usize,
    src: &SourceYuv,
    mb: (usize, usize),
    has_top: bool,
    has_left: bool,
    plan: QuantPlan<'_>,
) -> (u8, Chroma8Encode) {
    let (mb_x, mb_y) = mb;
    let stride = planes.uv_stride;
    let lambda = chroma_lambda(plan.quant.uv.ac.q);
    let mut best_mode = DC_PRED;
    let mut best_cost = i64::MAX;
    let mut best = None;
    for &mode in &WHOLE_BLOCK_MODES {
        if !mode_available(mode, has_top, has_left) {
            continue;
        }
        predict_chroma8(&mut planes.u, uv_off, stride, mode, has_top, has_left);
        predict_chroma8(&mut planes.v, uv_off, stride, mode, has_top, has_left);
        let enc = encode_chroma8_residual(src, planes, mb_x, mb_y, plan);
        let bits = chroma8_token_bits(&enc.blocks, plan.probas);
        let sse = chroma8_reconstruction_sse(planes, &enc.coeffs, uv_off, src, mb_x, mb_y);
        let cost = RD_DISTO_MULT * sse + lambda * bits;
        if cost < best_cost {
            best_cost = cost;
            best_mode = mode;
            best = Some(enc);
        }
    }
    // Commit the winner into both chroma planes.
    predict_chroma8(&mut planes.u, uv_off, stride, best_mode, has_top, has_left);
    predict_chroma8(&mut planes.v, uv_off, stride, best_mode, has_top, has_left);
    (
        best_mode,
        best.unwrap_or_else(|| encode_chroma8_residual(src, planes, mb_x, mb_y, plan)),
    )
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_possible_wrap,
        reason = "test fixtures build pixel/coefficient inputs with the same bounded, \
                  in-range casts the codec uses; the values fit their targets by construction"
    )]

    use super::{
        Effort, FramePlan, FrameTuning, SearchGates, emit_best_partitions, encode_frame,
        encode_frame_impl, plan_frame, residual_block, residual_block_reference, sse_block,
        sse_block_reference,
    };
    use crate::lossy::bool_dec::BoolDecoder;
    use crate::lossy::constants::{COEFFS_PROBA_0, DC_PRED, H_PRED, V_PRED};
    use crate::lossy::decode::{self, Frame};
    use crate::lossy::frame_header::{FrameHeader, KEY_FRAME_HEADER_LEN};

    /// The Balanced/Best Pass-1 search gates (whole-block mode search + trellis, no
    /// intra-4×4), the common shape the plan-level tests drive.
    const FULL_GATES: SearchGates = SearchGates {
        search_modes: true,
        uses_i4x4: false,
        uses_trellis: true,
        sns_strength: FrameTuning::AUTO.sns_strength,
    };

    /// The full Balanced/Best effort: mode search, probability optimization, skip
    /// coding and the in-loop filter all on.
    const BALANCED: Effort = Effort::Full;

    /// The Fast effort: every gate off (fixed DC prediction, default probabilities,
    /// no skip coding, filter off).
    const FAST: Effort = Effort::Fast;

    proptest::proptest! {
        /// The autovectorized `sse_block` is byte-identical to the flat reference
        /// over random source/prediction planes, block sizes and padded strides
        /// (the offsets and strides the real callers use: separate `src`/`pred`
        /// pitches, an arbitrary starting offset). A single differing `i64` here
        /// would move an RD decision and thus the encoded bytes.
        #[test]
        fn sse_block_matches_reference(
            size in 1usize..=16,
            extra_src in 0usize..8,
            extra_pred in 0usize..8,
            src_off in 0usize..32,
            pred_off in 0usize..32,
            seed in proptest::prelude::any::<u64>(),
        ) {
            let src_stride = size + extra_src;
            let pred_stride = size + extra_pred;
            // Buffers large enough for `off + (size-1)*stride + size`.
            let src_len = src_off + (size - 1) * src_stride + size;
            let pred_len = pred_off + (size - 1) * pred_stride + size;
            // Deterministic SplitMix64 fill so the case is reproducible from `seed`.
            let mut st = seed;
            let mut next = || {
                st = st.wrapping_add(0x9E37_79B9_7F4A_7C15);
                let mut z = st;
                z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
                (z ^ (z >> 31)) as u8
            };
            let src: Vec<u8> = (0..src_len).map(|_| next()).collect();
            let pred: Vec<u8> = (0..pred_len).map(|_| next()).collect();
            proptest::prop_assert_eq!(
                sse_block(&src, src_off, src_stride, &pred, pred_off, pred_stride, size),
                sse_block_reference(&src, src_off, src_stride, &pred, pred_off, pred_stride, size),
            );
        }

        /// The slice-mapped `residual_block` is byte-identical to the flat reference
        /// over random source/prediction planes with padded strides and an arbitrary
        /// `(src_x, src_y)` / `pred_off` origin — the coordinates and separate pitches
        /// the real callers pass. A single differing `i16` would move a coefficient
        /// and thus the encoded bytes.
        #[test]
        fn residual_block_matches_reference(
            src_x in 0usize..8,
            src_y in 0usize..8,
            extra_src in 0usize..8,
            extra_pred in 0usize..8,
            pred_off in 0usize..32,
            seed in proptest::prelude::any::<u64>(),
        ) {
            let src_stride = src_x + 4 + extra_src;
            let pred_stride = 4 + extra_pred;
            // Enough rows for the 4×4 block starting at `(src_x, src_y)` / `pred_off`.
            let src_len = (src_y + 4) * src_stride;
            let pred_len = pred_off + 4 * pred_stride;
            let mut st = seed;
            let mut next = || {
                st = st.wrapping_add(0x9E37_79B9_7F4A_7C15);
                let mut z = st;
                z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
                (z ^ (z >> 31)) as u8
            };
            let src: Vec<u8> = (0..src_len).map(|_| next()).collect();
            let pred: Vec<u8> = (0..pred_len).map(|_| next()).collect();
            proptest::prop_assert_eq!(
                residual_block(&src, src_stride, src_x, src_y, &pred, pred_off, pred_stride),
                residual_block_reference(&src, src_stride, src_x, src_y, &pred, pred_off, pred_stride),
            );
        }
    }

    /// Assert the wavefront planner reconstructs and plans one `w`×`h` frame
    /// byte-for-byte identically to the serial reference for effort `gates`.
    /// Identical reconstructed planes across the whole frame imply identical modes,
    /// coefficients and tokens (reconstruction is a deterministic function of the
    /// full plan); the scalar `MbPlan` fields are also compared for the metadata the
    /// emit pass reads directly.
    #[cfg(feature = "rayon")]
    fn assert_wavefront_matches_serial(gates: SearchGates, w: usize, h: usize) {
        use super::{
            MbPlan, PARALLEL_MB_THRESHOLD, Planes, Quantizer, plan_frame_serial, plan_segmentation,
            run_mb_planning,
        };
        use crate::lossy::rgb_to_yuv;

        // Deterministic high-frequency source so many macroblocks pick i4x4.
        let mut rgba = vec![0u8; w * h * 4];
        let mut s: u8 = 0x11;
        for px in rgba.chunks_exact_mut(4) {
            s = s.wrapping_mul(37).wrapping_add(0x53);
            px[0] = s;
            px[1] = s.wrapping_add(40);
            px[2] = s.wrapping_add(80);
            px[3] = 255;
        }
        let src = rgb_to_yuv::from_rgba(&rgba, w, h);
        let (mb_w, mb_h) = (src.mb_w, src.mb_h);
        assert!(
            mb_w * mb_h >= PARALLEL_MB_THRESHOLD,
            "{w}x{h} must exercise the parallel path"
        );
        let seg = plan_segmentation(&src, 40, true, FrameTuning::AUTO);
        let quants = seg.base_q.map(Quantizer::new);

        let mut wave_planes = Planes::new(mb_w, mb_h);
        let par_result = run_mb_planning(
            &mut wave_planes,
            &src,
            &seg.seg_ids,
            &quants,
            mb_w,
            mb_h,
            gates,
            &COEFFS_PROBA_0,
        );
        let mut serial_planes = Planes::new(mb_w, mb_h);
        let ser_result = plan_frame_serial(
            &mut serial_planes,
            &src,
            &seg.seg_ids,
            &quants,
            mb_w,
            mb_h,
            gates,
            &COEFFS_PROBA_0,
        );

        // Compare only the real reconstructed image region (inside the 1-pixel
        // borders, excluding the right/top-right margin). The intra-4×4 top-right lane
        // scribbles a few samples into that margin (and, for interior macroblocks,
        // into the next macroblock's area — which the serial planner then overwrites);
        // those are transient and never feed prediction, the loop filter or the
        // emitted tokens, so the wavefront's leaving them unwritten is byte-irrelevant.
        let region = |plane: &[u8], stride: usize, rows: usize, cols: usize| -> Vec<u8> {
            let mut out = Vec::with_capacity(rows * cols);
            for r in 0..rows {
                let base = (r + 1) * stride + 1;
                out.extend_from_slice(&plane[base..base + cols]);
            }
            out
        };
        let ys = wave_planes.y_stride;
        let uvs = wave_planes.uv_stride;
        assert_eq!(
            region(&wave_planes.y, ys, mb_h * 16, mb_w * 16),
            region(&serial_planes.y, ys, mb_h * 16, mb_w * 16),
            "luma image {w}x{h}"
        );
        assert_eq!(
            region(&wave_planes.u, uvs, mb_h * 8, mb_w * 8),
            region(&serial_planes.u, uvs, mb_h * 8, mb_w * 8),
            "u image {w}x{h}"
        );
        assert_eq!(
            region(&wave_planes.v, uvs, mb_h * 8, mb_w * 8),
            region(&serial_planes.v, uvs, mb_h * 8, mb_w * 8),
            "v image {w}x{h}"
        );
        assert_eq!(par_result.len(), ser_result.len());
        for (i, (a, b)) in par_result.iter().zip(&ser_result).enumerate() {
            let key = |p: &MbPlan| {
                (
                    p.ymode,
                    p.uvmode,
                    p.is_i4x4,
                    p.skippable,
                    p.has_residual,
                    p.segment,
                    p.imodes,
                )
            };
            assert_eq!(
                key(a),
                key(b),
                "MbPlan scalars diverge at MB {i} of {w}x{h}"
            );
        }
    }

    /// The wavefront-parallel planner must be byte-for-byte identical to the serial
    /// reference — the proof that the `rayon` feature never changes encoded output.
    /// Covers both effort shapes (Best → skew-2 diagonals + i4x4 top-right lane;
    /// Balanced → skew-1 diagonals) and spans the wavefront corners (square/wide/tall,
    /// exercising rightmost-column and top-row macroblocks), all above
    /// `PARALLEL_MB_THRESHOLD` so the parallel path actually runs.
    #[cfg(feature = "rayon")]
    #[test]
    fn wavefront_planner_matches_serial_byte_for_byte() {
        let best = SearchGates {
            search_modes: true,
            uses_i4x4: true,
            uses_trellis: true,
            sns_strength: FrameTuning::AUTO.sns_strength,
        };
        let balanced = SearchGates {
            search_modes: true,
            uses_i4x4: false,
            uses_trellis: true,
            sns_strength: FrameTuning::AUTO.sns_strength,
        };
        for &gates in &[best, balanced] {
            for &(w, h) in &[(256usize, 256usize), (272, 256), (256, 272), (512, 256)] {
                assert_wavefront_matches_serial(gates, w, h);
            }
        }
    }

    /// Parse `payload` far enough to recover every macroblock's chosen 16×16 luma
    /// mode and 8×8 chroma mode (raster order), mirroring the control-partition
    /// parse in [`crate::lossy::decode::reconstruct_to_planes`] but collecting the modes
    /// row by row (the decoder reuses one row's worth of `mb_data`). Intra-mode
    /// parsing lives entirely in partition 0 and never touches the token
    /// partitions, so residuals need not be parsed to read the modes.
    fn decoded_mb_modes(payload: &[u8]) -> (usize, usize, Vec<(u8, u8)>) {
        let fh = FrameHeader::parse_key_frame(payload).unwrap();
        let mut frame = Frame::new(fh).unwrap();
        let after_header = &payload[KEY_FRAME_HEADER_LEN..];
        let part0_len = usize::try_from(fh.first_partition_size).unwrap();
        let part0 = &after_header[..part0_len];
        let after_part0 = &after_header[part0_len..];

        let mut br = BoolDecoder::new(part0);
        frame.parse_headers(&mut br);
        frame.parse_partitions(&mut br, after_part0).unwrap();
        frame.parse_quant(&mut br);
        let _update_proba = br.read_flag();
        frame.parse_proba(&mut br);

        let (mb_w, mb_h) = (frame.mb_w, frame.mb_h);
        let mut modes = Vec::with_capacity(mb_w * mb_h);
        for _mb_y in 0..mb_h {
            frame.parse_intra_mode_row(&mut br);
            for mb_x in 0..mb_w {
                let d = &frame.mb_data[mb_x];
                modes.push((d.imodes[0], d.uvmode));
            }
            frame.init_scanline();
        }
        (mb_w, mb_h, modes)
    }

    /// The in-loop filter level the encoder emitted, recovered by parsing the
    /// control-partition filter header (inverse of `header::parse_filter_header`).
    fn decoded_filter_level(payload: &[u8]) -> i32 {
        let fh = FrameHeader::parse_key_frame(payload).unwrap();
        let mut frame = Frame::new(fh).unwrap();
        let part0_len = usize::try_from(fh.first_partition_size).unwrap();
        let part0 = &payload[KEY_FRAME_HEADER_LEN..KEY_FRAME_HEADER_LEN + part0_len];
        let mut br = BoolDecoder::new(part0);
        frame.parse_headers(&mut br);
        frame.filter.level
    }

    /// A `width`×`height` RGBA test image from a deterministic pattern.
    fn image(width: usize, height: usize, f: impl Fn(usize, usize) -> [u8; 3]) -> Vec<u8> {
        let mut buf = Vec::with_capacity(width * height * 4);
        for y in 0..height {
            for x in 0..width {
                let [r, g, b] = f(x, y);
                buf.extend_from_slice(&[r, g, b, 0xff]);
            }
        }
        buf
    }

    /// The load-bearing self-consistency check: the encoder's own reconstruction
    /// must equal the decoder's reconstruction of the encoder's output, byte for
    /// byte, across every padded plane.
    fn assert_self_consistent(rgba: &[u8], w: usize, h: usize, base_q: i32) {
        // Exercises the full Balanced path (mode search + probability optimization
        // + skip consideration + in-loop deblocking filter). The decoder's
        // reconstruction is post-filter, so this pins that the encoder applies the
        // exact same filter the decoder re-derives from the emitted header.
        let (payload, enc_planes) =
            encode_frame_impl(rgba, w, h, base_q, BALANCED, FrameTuning::AUTO);
        let (dec_planes, dw, dh) = decode::reconstruct_to_planes(&payload).unwrap();
        assert_eq!((dw, dh), (w, h), "dimensions");
        assert_eq!(enc_planes.y, dec_planes.y, "luma plane mismatch");
        assert_eq!(enc_planes.u, dec_planes.u, "U plane mismatch");
        assert_eq!(enc_planes.v, dec_planes.v, "V plane mismatch");
    }

    /// Self-consistency under an explicit [`FrameTuning`] (the seam the public
    /// psychovisual knobs thread through): the decoder re-derives every per-segment
    /// quantizer and filter-strength delta from the emitted header, so
    /// `decode(encode) == reconstruction` must hold for any knob setting.
    fn assert_self_consistent_tuned(
        rgba: &[u8],
        w: usize,
        h: usize,
        base_q: i32,
        effort: Effort,
        tuning: FrameTuning,
    ) {
        let (payload, enc) = encode_frame_impl(rgba, w, h, base_q, effort, tuning);
        let (dec, dw, dh) = decode::reconstruct_to_planes(&payload).unwrap();
        let ctx = format!(
            "q{base_q} {effort:?} sns{} seg{} f{} sharp{}",
            tuning.sns_strength, tuning.segments, tuning.filter_strength, tuning.filter_sharpness
        );
        assert_eq!((dw, dh), (w, h), "dimensions [{ctx}]");
        assert_eq!(enc.y_stride, dec.y_stride, "luma stride [{ctx}]");
        // Compare every real luma sample (left/top borders and interior MB boundaries
        // included); the four-column right scratch margin lies beyond the frame width
        // and the i4x4 top-right-lane setup may leave it in a harmless different state,
        // so it is excluded — the same margin-aware check as `assert_self_consistent_best`.
        let stride = enc.y_stride;
        let real_cols = 1 + w.div_ceil(16) * 16;
        for (i, (&a, &b)) in enc.y.iter().zip(&dec.y).enumerate() {
            if i % stride < real_cols {
                assert_eq!(
                    a,
                    b,
                    "luma mismatch at row {}, col {} [{ctx}]",
                    i / stride,
                    i % stride
                );
            }
        }
        assert_eq!(enc.u, dec.u, "U plane mismatch [{ctx}]");
        assert_eq!(enc.v, dec.v, "V plane mismatch [{ctx}]");
    }

    /// A `w`×`h` frame whose left half is flat and right half is a high-contrast
    /// checkerboard — two distinct activity levels, so k-means segmentation and the
    /// per-segment filter deltas genuinely fire under a non-zero SNS strength.
    fn split_activity_image(w: usize, h: usize) -> Vec<u8> {
        image(w, h, |x, _| {
            if x < w / 2 {
                [96, 128, 160]
            } else if x % 2 == 0 {
                [20, 20, 20]
            } else {
                [235, 235, 235]
            }
        })
    }

    #[test]
    fn every_knob_setting_stays_self_consistent() {
        // Sweep each active psychovisual knob across its full range (plus two combined
        // extremes) at both Full and Best on multi-activity content, asserting
        // decode-of-output equals the encoder's own reconstruction for all of them.
        // Segmentation and the per-segment filter only engage when the content and the
        // knobs allow, but self-consistency must hold either way.
        let (w, h) = (48usize, 48usize);
        let rgba = split_activity_image(w, h);
        let base = FrameTuning::AUTO;
        let mut tunings = Vec::new();
        for &sns_strength in &[0u8, 25, 50, 75, 100] {
            tunings.push(FrameTuning {
                sns_strength,
                ..base
            });
        }
        for &segments in &[1u8, 2, 3, 4] {
            tunings.push(FrameTuning { segments, ..base });
        }
        for &filter_strength in &[0u8, 20, 40, 60, 80, 100] {
            tunings.push(FrameTuning {
                filter_strength,
                ..base
            });
        }
        for &filter_sharpness in &[0u8, 1, 3, 5, 7] {
            tunings.push(FrameTuning {
                filter_sharpness,
                ..base
            });
        }
        // Sharp-YUV swaps the chroma planes for the luminance-guided subsampling; the
        // decoder still reconstructs exactly what the encoder coded, so self-consistency
        // must hold with it on (a round-trip check of the whole sharp path).
        tunings.push(FrameTuning {
            sharp_yuv: true,
            ..base
        });
        tunings.push(FrameTuning {
            sns_strength: 100,
            segments: 4,
            filter_strength: 100,
            filter_sharpness: 7,
            sharp_yuv: true,
            passes: 1,
        });
        tunings.push(FrameTuning {
            sns_strength: 80,
            segments: 3,
            filter_strength: 30,
            filter_sharpness: 2,
            sharp_yuv: false,
            passes: 1,
        });
        for tuning in tunings {
            for &effort in &[Effort::Full, Effort::Best] {
                for &q in &[20i32, 70] {
                    assert_self_consistent_tuned(&rgba, w, h, q, effort, tuning);
                }
            }
        }
    }

    #[test]
    fn filter_strength_knob_moves_the_emitted_filter_level() {
        // The filter-strength knob drives the emitted in-loop filter level: 0 disables
        // it, a non-zero knob emits a filter, and a stronger knob raises the level the
        // decoder re-derives — the knob is wired to the bitstream, not inert.
        let (w, h) = (48usize, 48usize);
        let rgba = split_activity_image(w, h);
        let level = |fs: u8| {
            let tuning = FrameTuning {
                filter_strength: fs,
                ..FrameTuning::AUTO
            };
            decoded_filter_level(&encode_frame_impl(&rgba, w, h, 40, BALANCED, tuning).0)
        };
        assert_eq!(level(0), 0, "filter_strength 0 disables the loop filter");
        assert!(level(20) > 0, "a non-zero filter knob emits a filter");
        assert!(
            level(100) > level(20),
            "a stronger filter knob raises the level"
        );
    }

    #[test]
    fn sns_strength_knob_changes_the_encoded_stream() {
        // On multi-activity content the SNS knob engages both segmentation and the
        // perceptual mode decision, so strength 0 (single segment, plain SSE) and a
        // high strength must yield different bytes, and both stay self-consistent.
        let (w, h) = (48usize, 48usize);
        let rgba = split_activity_image(w, h);
        let encode = |sns: u8| {
            let tuning = FrameTuning {
                sns_strength: sns,
                ..FrameTuning::AUTO
            };
            encode_frame_impl(&rgba, w, h, 40, BALANCED, tuning).0
        };
        assert_ne!(
            encode(0),
            encode(100),
            "the SNS knob must change the stream"
        );
        assert_self_consistent_tuned(
            &rgba,
            w,
            h,
            40,
            BALANCED,
            FrameTuning {
                sns_strength: 100,
                ..FrameTuning::AUTO
            },
        );
    }

    #[test]
    fn solid_color_is_self_consistent() {
        // The de-risking spike: a solid 16×16 block (DC-only residual) must decode
        // back to the encoder's reconstruction.
        for &q in &[0, 32, 64, 100, 127] {
            let rgba = image(16, 16, |_, _| [80, 160, 40]);
            assert_self_consistent(&rgba, 16, 16, q);
        }
    }

    #[test]
    fn flat_image_uses_skip_and_stays_self_consistent() {
        // A solid color: every interior macroblock predicts perfectly from its
        // reconstructed neighbors, so its residual quantizes to all-zero and the
        // block is skippable. Balanced must therefore code per-macroblock skip
        // (nb_skip > 0, use_skip true), and decoding that skip-using stream must
        // still equal the encoder's own reconstruction byte for byte — the exact
        // context evolution `skip_mb` writes has to match the decoder's
        // `skip_residuals` or the following macroblocks would desync.
        let (w, h) = (64usize, 64usize);
        let rgba = image(w, h, |_, _| [120, 90, 200]);
        let base_q = 40;

        let FramePlan {
            plans: mb_plans,
            mb_w,
            mb_h,
            ..
        } = plan_frame(
            &rgba,
            w,
            h,
            base_q,
            FULL_GATES,
            true,
            FrameTuning::AUTO,
            &COEFFS_PROBA_0,
        );
        let nb_skip = mb_plans.iter().filter(|p| p.skippable).count();
        assert!(
            nb_skip > 0,
            "a flat image should have skippable macroblocks"
        );
        let skip = super::resolve_skip(&mb_plans, mb_w * mb_h, true);
        assert!(
            skip.use_skip,
            "enough skippable macroblocks should enable use_skip"
        );

        assert_self_consistent(&rgba, w, h, base_q);
    }

    #[test]
    fn gradient_is_self_consistent() {
        // A smooth gradient exercises real AC coefficients and the Y2 path.
        let rgba = image(48, 32, |x, y| {
            let r = (x * 5) as u8;
            let g = (y * 7) as u8;
            let b = ((x + y) * 3) as u8;
            [r, g, b]
        });
        for &q in &[8, 40, 96] {
            assert_self_consistent(&rgba, 48, 32, q);
        }
    }

    #[test]
    fn odd_dimensions_are_self_consistent() {
        // A 17×13 picture (partial edge macroblocks, edge-replicated source).
        let rgba = image(17, 13, |x, y| [(x * 13) as u8, (y * 17) as u8, 128]);
        assert_self_consistent(&rgba, 17, 13, 40);
    }

    #[test]
    fn high_contrast_is_self_consistent() {
        // A checkerboard drives large residuals and many non-zero tokens.
        let rgba = image(32, 32, |x, y| {
            if (x / 4 + y / 4) % 2 == 0 {
                [255, 255, 255]
            } else {
                [0, 0, 0]
            }
        });
        assert_self_consistent(&rgba, 32, 32, 20);
    }

    #[test]
    fn blocky_image_filters_and_stays_self_consistent() {
        // A high-contrast checkerboard: its sharp macroblock-edge steps are exactly
        // what the in-loop deblocking filter smooths, so the Balanced path must
        // choose a non-zero filter level here. Decode-of-output must still equal the
        // encoder's own POST-FILTER reconstruction byte for byte — pinning that the
        // encoder applies precisely the filter the decoder re-derives from the header.
        let (w, h) = (48usize, 48usize);
        let rgba = image(w, h, |x, y| {
            if (x / 4 + y / 4) % 2 == 0 {
                [20, 20, 20]
            } else {
                [220, 220, 220]
            }
        });
        let base_q = 40; // level = 40 * 3 / 8 = 15 > 0
        let (payload, _enc) = encode_frame_impl(&rgba, w, h, base_q, BALANCED, FrameTuning::AUTO);
        assert!(
            decoded_filter_level(&payload) > 0,
            "Balanced must enable the loop filter on blocky content"
        );
        assert_self_consistent(&rgba, w, h, base_q);
    }

    #[test]
    fn fast_emits_filter_level_zero() {
        // The Fast method (apply_filter = false) must code the frame with the loop
        // filter off — filter level 0 — keeping it byte-identical to the pre-filter
        // output, even on blocky content a filtered encode would deblock.
        let (w, h) = (48usize, 48usize);
        let rgba = image(w, h, |x, y| {
            if (x / 4 + y / 4) % 2 == 0 {
                [20, 20, 20]
            } else {
                [220, 220, 220]
            }
        });
        let payload = encode_frame(&rgba, w, h, 40, FAST);
        assert_eq!(
            decoded_filter_level(&payload),
            0,
            "Fast must emit filter level 0"
        );
    }

    #[test]
    fn output_decodes_to_the_right_dimensions() {
        // The wrapped payload decodes through the public image path with correct
        // dimensions and full opacity.
        let rgba = image(24, 20, |x, y| [(x * 10) as u8, (y * 12) as u8, 64]);
        let payload = encode_frame(&rgba, 24, 20, 40, BALANCED);
        let image = decode::decode_frame(&payload).unwrap();
        assert_eq!((image.width(), image.height()), (24, 20));
        assert_eq!(image.as_bytes().len(), 24 * 20 * 4);
    }

    #[test]
    fn encoding_is_deterministic() {
        let rgba = image(32, 24, |x, y| [(x * 8) as u8, (y * 9) as u8, (x ^ y) as u8]);
        let a = encode_frame(&rgba, 32, 24, 50, BALANCED);
        let b = encode_frame(&rgba, 32, 24, 50, BALANCED);
        assert_eq!(a, b, "identical inputs must produce identical bytes");
    }

    #[test]
    fn horizontal_source_selects_horizontal_prediction() {
        // Rows differ, columns are equal: the content varies only *down* the
        // picture. H_PRED fills each row from its reconstructed left neighbor and
        // so matches it; V_PRED fills each column from the row above and does not.
        // Every macroblock that has a left neighbor must therefore pick H_PRED for
        // both luma and chroma — never V_PRED, the *transposed* choice a swapped SSE
        // or predictor wiring would make (a bug the mode-vs-mode self-consistency
        // tests cannot see). The left-edge column has no left neighbor, so H is
        // unavailable and the search falls back to DC, pinning the availability
        // contract too.
        let rgba = image(48, 48, |_x, y| {
            [(y * 7) as u8, (y * 3 + 20) as u8, (y * 11) as u8]
        });
        let payload = encode_frame(&rgba, 48, 48, 16, BALANCED);
        let (mb_w, mb_h, modes) = decoded_mb_modes(&payload);
        assert!(mb_w >= 2 && mb_h >= 2, "test needs an interior macroblock");
        for mb_y in 0..mb_h {
            for mb_x in 0..mb_w {
                let (ymode, uvmode) = modes[mb_y * mb_w + mb_x];
                if mb_x == 0 {
                    // No left neighbor → H unavailable, DC wins.
                    assert_eq!(ymode, DC_PRED, "left-edge luma at ({mb_x},{mb_y})");
                    assert_eq!(uvmode, DC_PRED, "left-edge chroma at ({mb_x},{mb_y})");
                } else {
                    assert_eq!(ymode, H_PRED, "luma at ({mb_x},{mb_y})");
                    assert_eq!(uvmode, H_PRED, "chroma at ({mb_x},{mb_y})");
                }
            }
        }
    }

    #[test]
    fn vertical_source_selects_vertical_prediction() {
        // The transpose of the horizontal case: columns differ, rows are equal, so
        // V_PRED matches and H_PRED does not. Every macroblock with a top neighbor
        // picks V_PRED for both luma and chroma (never H_PRED), pinning the SSE and
        // predictor orientation from the other side. The top row has no top
        // neighbor, so V is unavailable there and the search falls back to DC.
        let rgba = image(48, 48, |x, _y| {
            [(x * 7) as u8, (x * 3 + 20) as u8, (x * 11) as u8]
        });
        let payload = encode_frame(&rgba, 48, 48, 16, BALANCED);
        let (mb_w, mb_h, modes) = decoded_mb_modes(&payload);
        assert!(mb_w >= 2 && mb_h >= 2, "test needs an interior macroblock");
        for mb_y in 0..mb_h {
            for mb_x in 0..mb_w {
                let (ymode, uvmode) = modes[mb_y * mb_w + mb_x];
                if mb_y == 0 {
                    // No top neighbor → V unavailable, DC wins.
                    assert_eq!(ymode, DC_PRED, "top-edge luma at ({mb_x},{mb_y})");
                    assert_eq!(uvmode, DC_PRED, "top-edge chroma at ({mb_x},{mb_y})");
                } else {
                    assert_eq!(ymode, V_PRED, "luma at ({mb_x},{mb_y})");
                    assert_eq!(uvmode, V_PRED, "chroma at ({mb_x},{mb_y})");
                }
            }
        }
    }

    #[test]
    fn fast_skips_mode_search_and_is_self_consistent() {
        // The same directional content the horizontal test feeds the search: rows
        // differ, columns match, so Balanced would pick H_PRED for every interior
        // block. The fast path (search_modes = false) must instead fix DC_PRED
        // everywhere — proving it skipped the search — and its stream (also emitting
        // the default probability table, optimize_probas = false) must still decode
        // back to the encoder's own reconstruction, byte for byte. Fast also codes
        // the filter off (apply_filter = false), so the reconstruction is unfiltered.
        let rgba = image(48, 48, |_x, y| {
            [(y * 7) as u8, (y * 3 + 20) as u8, (y * 11) as u8]
        });
        let (payload, enc_planes) = encode_frame_impl(&rgba, 48, 48, 16, FAST, FrameTuning::AUTO);

        let (dec_planes, dw, dh) = decode::reconstruct_to_planes(&payload).unwrap();
        assert_eq!((dw, dh), (48, 48), "dimensions");
        assert_eq!(enc_planes.y, dec_planes.y, "luma plane mismatch");
        assert_eq!(enc_planes.u, dec_planes.u, "U plane mismatch");
        assert_eq!(enc_planes.v, dec_planes.v, "V plane mismatch");

        let (mb_w, mb_h, modes) = decoded_mb_modes(&payload);
        assert!(mb_w >= 2 && mb_h >= 2, "test needs an interior macroblock");
        for mb_y in 0..mb_h {
            for mb_x in 0..mb_w {
                let (ymode, uvmode) = modes[mb_y * mb_w + mb_x];
                assert_eq!(ymode, DC_PRED, "fast luma at ({mb_x},{mb_y}) is not DC");
                assert_eq!(uvmode, DC_PRED, "fast chroma at ({mb_x},{mb_y}) is not DC");
            }
        }
    }

    /// Encode `rgba` at `base_q` and return `(default_total, optimized_total)`: the
    /// full payload byte counts of the default-probability and optimized-probability
    /// candidates. The encoder emits the smaller; exposing both lets a test prove the
    /// optimizer never grows the frame.
    fn frame_size_totals(rgba: &[u8], width: usize, height: usize, base_q: i32) -> (usize, usize) {
        let FramePlan {
            plans: mb_plans,
            mb_w,
            mb_h,
            seg_params: seg,
            ..
        } = plan_frame(
            rgba,
            width,
            height,
            base_q,
            FULL_GATES,
            true,
            FrameTuning::AUTO,
            &COEFFS_PROBA_0,
        );
        let skip = super::resolve_skip(&mb_plans, mb_w * mb_h, true);
        let filter = super::choose_filter(base_q, true, 60, 0);
        let header = super::HeaderParams {
            base_q,
            filter: &filter,
            segments: seg,
        };
        let (_part0, _token, default_total, optimized_total, _probas) =
            emit_best_partitions(&mb_plans, mb_w, mb_h, header, skip);
        (
            default_total + KEY_FRAME_HEADER_LEN,
            optimized_total + KEY_FRAME_HEADER_LEN,
        )
    }

    /// Deterministic integer pseudo-noise RGBA — high-AC content that produces many
    /// non-zero coefficient tokens, the regime where an optimized probability table
    /// pays for its transmission.
    fn noisy_image(width: usize, height: usize) -> Vec<u8> {
        image(width, height, |x, y| {
            let mut s = (x as u32)
                .wrapping_mul(2_654_435_761)
                .wrapping_add((y as u32).wrapping_mul(40_503))
                .wrapping_add(0x9e37_79b9);
            s ^= s >> 13;
            s = s.wrapping_mul(0x85eb_ca6b);
            s ^= s >> 16;
            [
                (s & 0xff) as u8,
                ((s >> 8) & 0xff) as u8,
                ((s >> 16) & 0xff) as u8,
            ]
        })
    }

    fn decoded_i4x4_flags(payload: &[u8]) -> (usize, usize, Vec<bool>) {
        let fh = FrameHeader::parse_key_frame(payload).unwrap();
        let mut frame = Frame::new(fh).unwrap();
        let after_header = &payload[KEY_FRAME_HEADER_LEN..];
        let part0_len = usize::try_from(fh.first_partition_size).unwrap();
        let part0 = &after_header[..part0_len];
        let after_part0 = &after_header[part0_len..];
        let mut br = BoolDecoder::new(part0);
        frame.parse_headers(&mut br);
        frame.parse_partitions(&mut br, after_part0).unwrap();
        frame.parse_quant(&mut br);
        let _u = br.read_flag();
        frame.parse_proba(&mut br);
        let (mb_w, mb_h) = (frame.mb_w, frame.mb_h);
        let mut flags = Vec::with_capacity(mb_w * mb_h);
        for _ in 0..mb_h {
            frame.parse_intra_mode_row(&mut br);
            for mb_x in 0..mb_w {
                flags.push(frame.mb_data[mb_x].is_i4x4);
            }
            frame.init_scanline();
        }
        (mb_w, mb_h, flags)
    }

    /// Best-effort self-consistency: decode-of-output must equal the encoder's own
    /// post-filter reconstruction, exercising the i4x4 luma path.
    ///
    /// Every real luma sample (plus the left/top borders and all interior macroblock
    /// boundaries) must match byte for byte; the four-column right scratch margin —
    /// which lies *beyond* the frame width, is never part of the decoded image, and
    /// is never read for prediction or filtering — is excluded, since the i4x4
    /// top-right-lane setup may leave it in a different (harmless) state between a
    /// losing candidate evaluation and the decoder's reconstruction. Chroma has no
    /// such margin (`uv_stride = 1 + mb_w*8`), so both planes are compared in full.
    fn assert_self_consistent_best(rgba: &[u8], w: usize, h: usize, base_q: i32) {
        let (payload, enc) = encode_frame_impl(rgba, w, h, base_q, Effort::Best, FrameTuning::AUTO);
        let (dec, dw, dh) = decode::reconstruct_to_planes(&payload).unwrap();
        assert_eq!((dw, dh), (w, h), "dimensions");
        assert_eq!(enc.y_stride, dec.y_stride, "luma stride");
        let stride = enc.y_stride;
        let mb_w = w.div_ceil(16);
        let real_cols = 1 + mb_w * 16; // left border + luma width (excludes the +4 margin)
        for (i, (&a, &b)) in enc.y.iter().zip(&dec.y).enumerate() {
            if i % stride < real_cols {
                assert_eq!(
                    a,
                    b,
                    "luma mismatch at row {}, col {}",
                    i / stride,
                    i % stride
                );
            }
        }
        assert_eq!(enc.u, dec.u, "U plane mismatch");
        assert_eq!(enc.v, dec.v, "V plane mismatch");
    }

    /// Fine vertical stripes (period 2 px): a flat 16×16 predictor fits them poorly
    /// while the 4×4 directional predictors fit the local structure, so the
    /// rate-distortion decision codes several macroblocks as i4x4.
    fn stripe_image(w: usize, h: usize) -> Vec<u8> {
        image(w, h, |x, _| {
            if (x / 2) % 2 == 0 {
                [220, 220, 220]
            } else {
                [20, 20, 20]
            }
        })
    }

    #[test]
    fn best_uses_i4x4_on_detailed_content_and_stays_self_consistent() {
        // Best must code at least one i4x4 macroblock on detailed content (non-vacuity
        // for the whole feature), and decoding that i4x4-using stream must still equal
        // the encoder's own reconstruction byte for byte — pinning the intra_t/intra_l
        // context threading and the first=0 (no-Y2) token path end to end.
        let (w, h) = (64usize, 64usize);
        let rgba = stripe_image(w, h);
        let base_q = 40;
        let payload = encode_frame(&rgba, w, h, base_q, Effort::Best);
        let (_mw, _mh, flags) = decoded_i4x4_flags(&payload);
        assert!(
            flags.iter().any(|&f| f),
            "Best must code at least one i4x4 macroblock on detailed content"
        );
        assert_self_consistent_best(&rgba, w, h, base_q);
    }

    #[test]
    fn best_mixed_i4x4_and_16x16_stays_self_consistent() {
        // A frame with a flat left half (stays 16×16) and detailed stripes on the right
        // (goes i4x4) mixes both macroblock kinds, exercising the mode-context threading
        // across the 16×16→i4x4 and i4x4→16×16 boundaries in one frame.
        let (w, h) = (64usize, 48usize);
        let rgba = image(w, h, |x, _| {
            if x < 32 {
                [100, 100, 100]
            } else if (x / 2) % 2 == 0 {
                [220, 220, 220]
            } else {
                [20, 20, 20]
            }
        });
        let base_q = 40;
        let payload = encode_frame(&rgba, w, h, base_q, Effort::Best);
        let (_mw, _mh, flags) = decoded_i4x4_flags(&payload);
        assert!(
            flags.iter().any(|&f| f),
            "the detailed half should use some i4x4"
        );
        assert!(
            flags.iter().any(|&f| !f),
            "the flat half should keep some 16×16"
        );
        assert_self_consistent_best(&rgba, w, h, base_q);
    }

    #[test]
    fn best_i4x4_is_self_consistent_across_qualities_and_dimensions() {
        // The i4x4 path must stay self-consistent over a spread of quantizers and at
        // odd dimensions (partial edge macroblocks, edge-replicated source).
        for &q in &[8, 30, 64, 100] {
            assert_self_consistent_best(&stripe_image(48, 48), 48, 48, q);
        }
        assert_self_consistent_best(&stripe_image(17, 13), 17, 13, 40);
        assert_self_consistent_best(&stripe_image(16, 16), 16, 16, 40);
    }

    /// Diagonal stripes: no 16×16 predictor (DC/V/H/TM) captures a diagonal, but
    /// the 4×4 diagonal B-modes (`B_LD`/`B_RD`/…) can, so Best codes diagonal
    /// macroblocks as i4x4. There is no 16×16 diagonal mode, so this is the pattern
    /// that most reliably selects i4x4 while its residual can quantize away.
    fn diagonal_image(w: usize, h: usize) -> Vec<u8> {
        // Moderate-contrast diagonal bands: 16×16 predictors miss the diagonal (so
        // i4x4 is selected, even after trellis quantization has thinned the residual)
        // but the per-4×4 diagonal prediction error is small enough to quantize
        // entirely away at a coarse quantizer — the has_residual=false i4x4 case that
        // exposes the loop-filter f_inner bug.
        image(w, h, |x, y| {
            let v = u8::try_from(120 + ((x + y) / 4) % 5 * 16).unwrap_or(120);
            [v, v, v]
        })
    }

    #[test]
    fn best_i4x4_stays_self_consistent_with_the_loop_filter_on() {
        // Regression: apply_loop_filter must thread `plan.is_i4x4` into resolve_finfo
        // (not a hardcoded false). The decoder forces `f_inner = true` for every i4x4
        // macroblock; if the encoder used is_i4x4=false it would instead get
        // `f_inner = has_residual`, so an i4x4 macroblock whose residual quantizes to
        // zero (has_residual=false) under a nonzero filter level would have its inner
        // 4×4 edges filtered by the decoder but not by the encoder's reconstruction —
        // breaking self-consistency. Diagonal content at coarse quantizers (filter
        // level = base_q*3/8 > 0) is where this bites.
        let (w, h) = (96usize, 96usize);
        let rgba = diagonal_image(w, h);
        let mut saw_i4x4 = false;
        for &q in &[80, 100, 110, 120, 127] {
            let payload = encode_frame(&rgba, w, h, q, Effort::Best);
            let (_mw, _mh, flags) = decoded_i4x4_flags(&payload);
            saw_i4x4 |= flags.iter().any(|&f| f);
            assert_self_consistent_best(&rgba, w, h, q);
        }
        assert!(
            saw_i4x4,
            "diagonal content should code some i4x4 macroblock"
        );
    }

    #[test]
    fn rd_mode_decision_is_self_consistent_on_varied_content() {
        // The whole-block (16×16 luma + 8×8 chroma) mode decision is rate-distortion
        // aware: each candidate is transformed, (trellis-)quantized and
        // reconstructed, and the min-RD mode's coefficients drive BOTH the encoder
        // reconstruction and the emitted tokens. This pins that the RD path composes
        // with trellis and stays byte-self-consistent (decode-of-output == our
        // reconstruction) on the banded-photo and AC-rich-noise content the RD delta
        // was measured against, across a spread of quantizers.
        let photo = photo96(96, 96);
        let noisy = noisy96(96, 96);
        for &bq in &[8, 40, 96] {
            assert_self_consistent(&photo, 96, 96, bq);
            assert_self_consistent(&noisy, 96, 96, bq);
        }
    }

    #[test]
    fn rd_mode_decision_selects_a_non_dc_mode_somewhere() {
        // Non-vacuity for the RD search: on directional/banded photo content some
        // interior macroblock must pick a non-DC 16×16 luma mode (the RD score beats
        // the availability-safe DC fallback). A search wired to always return DC — or
        // one whose distortion/rate were wrongly scaled so DC always won — would fail here.
        let rgba = photo96(96, 96);
        let payload = encode_frame(&rgba, 96, 96, 40, BALANCED);
        let (_mw, _mh, modes) = decoded_mb_modes(&payload);
        assert!(
            modes.iter().any(|&(ymode, _uv)| ymode != DC_PRED),
            "the RD mode search should pick a non-DC luma mode on banded photo content"
        );
    }

    #[test]
    fn balanced_never_uses_i4x4_on_detailed_content() {
        // i4x4 is Best-only: on the very stripes that make Best pick i4x4, every
        // Balanced (Full tier) macroblock must stay 16×16, so Balanced output is
        // byte-identical to the pre-i4x4 encoder.
        let (w, h) = (64usize, 64usize);
        let rgba = stripe_image(w, h);
        let payload = encode_frame(&rgba, w, h, 40, Effort::Full);
        let (_mw, _mh, flags) = decoded_i4x4_flags(&payload);
        assert!(flags.iter().all(|&f| !f), "Balanced must never code i4x4");
    }

    #[test]
    fn balanced_output_is_unchanged_by_the_i4x4_feature() {
        // A direct byte-for-byte guard that adding i4x4 (Best-only) did not perturb
        // Balanced: the Full-tier encode of detailed content is identical whether or
        // not the i4x4 gate exists, because Full never sets uses_i4x4. Encoding twice
        // must be deterministic AND contain no i4x4 macroblock.
        let (w, h) = (48usize, 48usize);
        let rgba = stripe_image(w, h);
        let a = encode_frame(&rgba, w, h, 40, Effort::Full);
        let b = encode_frame(&rgba, w, h, 40, Effort::Full);
        assert_eq!(a, b, "Balanced must be deterministic");
        let (_mw, _mh, flags) = decoded_i4x4_flags(&a);
        assert!(flags.iter().all(|&f| !f), "Balanced carries no i4x4");
    }

    #[test]
    fn best_is_deterministic() {
        // The i4x4 search + RD decision must be fully deterministic per (quality).
        let rgba = stripe_image(48, 48);
        let a = encode_frame(&rgba, 48, 48, 50, Effort::Best);
        let b = encode_frame(&rgba, 48, 48, 50, Effort::Best);
        assert_eq!(
            a, b,
            "Best must produce identical bytes for identical input"
        );
    }

    fn chosen_total(rgba: &[u8], w: usize, h: usize, base_q: i32, uses_trellis: bool) -> usize {
        let gates = SearchGates {
            search_modes: true,
            uses_i4x4: false,
            uses_trellis,
            sns_strength: FrameTuning::AUTO.sns_strength,
        };
        let FramePlan {
            plans: mb_plans,
            mb_w,
            mb_h,
            seg_params: seg,
            ..
        } = plan_frame(
            rgba,
            w,
            h,
            base_q,
            gates,
            true,
            FrameTuning::AUTO,
            &COEFFS_PROBA_0,
        );
        let skip = super::resolve_skip(&mb_plans, mb_w * mb_h, true);
        let filter = super::choose_filter(base_q, true, 60, 0);
        let header = super::HeaderParams {
            base_q,
            filter: &filter,
            segments: seg,
        };
        let (_p0, _t, d, o, _probas) = emit_best_partitions(&mb_plans, mb_w, mb_h, header, skip);
        KEY_FRAME_HEADER_LEN + d.min(o)
    }

    /// The AC-rich noise generator the reference sizes were measured against.
    fn noisy96(w: usize, h: usize) -> Vec<u8> {
        image(w, h, |x, y| {
            let n = (x
                .wrapping_mul(2_654_435_761)
                .wrapping_add(y.wrapping_mul(40_503)))
                >> 8;
            [
                (n & 0xff) as u8,
                ((n >> 3) & 0xff) as u8,
                ((x ^ y) * 3) as u8,
            ]
        })
    }

    /// The banded-photo generator the reference sizes were measured against.
    fn photo96(w: usize, h: usize) -> Vec<u8> {
        image(w, h, |x, y| {
            let band = i32::try_from(x / 8 % 3).unwrap_or(0);
            let r = u8::try_from(128 + 40 * band - 30).unwrap_or(0);
            [r, (x * 2 + y) as u8, (200 - y) as u8]
        })
    }

    #[test]
    fn trellis_shrinks_ac_rich_and_photo_frames() {
        // Trellis quantization (the Balanced/Best path) must code the AC-rich noise and
        // the banded photo strictly smaller than round-to-nearest at every quality —
        // the size win is the whole point of the phase. `chosen_total(_, false)` is the
        // pre-trellis round-to-nearest Balanced size (it reproduces the committed
        // reference numbers exactly); `chosen_total(_, true)` is the trellis size. The
        // matching PSNR-within-tolerance guarantee is pinned in `tests/encode.rs` (the
        // in-crate float ban keeps it out of here).
        use crate::lossy::quant::quality_to_base_q;
        let photo = photo96(96, 96);
        let noisy = noisy96(96, 96);
        for &qual in &[50u8, 75, 90] {
            let bq = quality_to_base_q(qual);
            for (name, rgba) in [("photo", &photo), ("noisy", &noisy)] {
                let before = chosen_total(rgba, 96, 96, bq, false);
                let after = chosen_total(rgba, 96, 96, bq, true);
                assert!(
                    after < before,
                    "{name} q{qual}: trellis {after} did not shrink round-to-nearest {before}"
                );
            }
        }
    }

    #[test]
    fn probability_optimization_never_grows_and_shrinks_ac_rich_frames() {
        // Keep-smaller guarantees optimized_total <= default_total for ANY image;
        // an AC-rich noisy frame (many coefficient tokens) must shrink STRICTLY.
        let grad = image(64, 64, |x, y| {
            [(x * 4) as u8, (y * 4) as u8, ((x + y) * 2) as u8]
        });
        let noisy = noisy_image(64, 64);
        let photo = image(96, 96, |x, y| {
            [(x * 2 + y) as u8, (128 + x + y) as u8, (x + y * 3) as u8]
        });
        let cases: [(&[u8], usize, usize); 3] =
            [(&grad, 64, 64), (&noisy, 64, 64), (&photo, 96, 96)];
        for &base_q in &[40i32, 75, 90] {
            for &(rgba, w, h) in &cases {
                let (default_total, optimized_total) = frame_size_totals(rgba, w, h, base_q);
                assert!(
                    optimized_total <= default_total,
                    "q{base_q} {w}x{h}: optimized {optimized_total} > default {default_total}"
                );
            }
        }

        // The AC-rich frame must be strictly smaller with the optimized table.
        let (default_total, optimized_total) = frame_size_totals(&noisy, 64, 64, 40);
        assert!(
            optimized_total < default_total,
            "AC-rich frame did not shrink: optimized {optimized_total} !< default {default_total}"
        );
    }

    // ---- segmentation -------------------------------------------------------

    /// A mixed-complexity image: a flat gray left half and a high-frequency
    /// pseudo-noise right half — the two regions the encoder must split into
    /// distinct quantizer segments (flat keeps fine quant, busy takes coarser).
    fn mixed_image(w: usize, h: usize) -> Vec<u8> {
        image(w, h, |x, y| {
            if x < w / 2 {
                [96, 96, 96]
            } else {
                let mut s = (x as u32)
                    .wrapping_mul(2_654_435_761)
                    .wrapping_add((y as u32).wrapping_mul(40_503))
                    .wrapping_add(0x9e37_79b9);
                s ^= s >> 13;
                s = s.wrapping_mul(0x85eb_ca6b);
                s ^= s >> 16;
                [
                    (s & 0xff) as u8,
                    ((s >> 8) & 0xff) as u8,
                    ((s >> 16) & 0xff) as u8,
                ]
            }
        })
    }

    /// Parse `payload` far enough to recover whether the frame codes segmentation
    /// and every macroblock's decoded segment id (raster order), mirroring the
    /// control-partition parse in `decode::reconstruct_to_planes` but collecting the
    /// per-macroblock `segment` field the segment-id tree fills.
    fn decoded_segments(payload: &[u8]) -> (bool, Vec<u8>) {
        let fh = FrameHeader::parse_key_frame(payload).unwrap();
        let mut frame = Frame::new(fh).unwrap();
        let after_header = &payload[KEY_FRAME_HEADER_LEN..];
        let part0_len = usize::try_from(fh.first_partition_size).unwrap();
        let part0 = &after_header[..part0_len];
        let after_part0 = &after_header[part0_len..];
        let mut br = BoolDecoder::new(part0);
        frame.parse_headers(&mut br);
        frame.parse_partitions(&mut br, after_part0).unwrap();
        frame.parse_quant(&mut br);
        let _update_proba = br.read_flag();
        frame.parse_proba(&mut br);
        let use_segment = frame.segment.use_segment;
        let (mb_w, mb_h) = (frame.mb_w, frame.mb_h);
        let mut segs = Vec::with_capacity(mb_w * mb_h);
        for _mb_y in 0..mb_h {
            frame.parse_intra_mode_row(&mut br);
            for mb_x in 0..mb_w {
                segs.push(frame.mb_data[mb_x].segment);
            }
            frame.init_scanline();
        }
        (use_segment, segs)
    }

    /// The number of distinct segment ids present in a decoded segment map.
    fn distinct_segments(segs: &[u8]) -> usize {
        let mut seen = [false; 4];
        for &s in segs {
            seen[usize::from(s)] = true;
        }
        seen.iter().filter(|&&b| b).count()
    }

    #[test]
    fn mixed_content_uses_multiple_segments_and_stays_self_consistent() {
        // Non-vacuity: on flat-vs-noise content the Balanced encoder must actually
        // partition the macroblocks (use_segment set, >= 2 distinct ids). And the
        // segmented stream must still decode to the encoder's own reconstruction byte
        // for byte — self-consistency holds for ANY valid segment assignment + per-
        // segment quant, so this pins the emitted segment header / map / quantizers as
        // the exact bit-inverse of what the decoder parses.
        let (w, h) = (64usize, 64usize);
        let rgba = mixed_image(w, h);
        let base_q = 40;
        let payload = encode_frame(&rgba, w, h, base_q, BALANCED);
        let (use_segment, segs) = decoded_segments(&payload);
        assert!(use_segment, "mixed content should enable segmentation");
        assert!(
            distinct_segments(&segs) >= 2,
            "expected >= 2 segments, got {} in {segs:?}",
            distinct_segments(&segs)
        );
        assert_self_consistent(&rgba, w, h, base_q);
    }

    #[test]
    fn uniform_image_falls_back_to_a_single_segment() {
        // A flat color has zero luma AC everywhere, so k-means collapses to one
        // cluster and the frame must code use_segment = false (byte-identical to the
        // pre-segmentation encoder) — and still be self-consistent.
        let (w, h) = (32usize, 32usize);
        let rgba = image(w, h, |_, _| [100, 100, 100]);
        let payload = encode_frame(&rgba, w, h, 40, BALANCED);
        let (use_segment, _segs) = decoded_segments(&payload);
        assert!(
            !use_segment,
            "uniform content should fall back to one segment"
        );
        assert_self_consistent(&rgba, w, h, 40);
    }

    #[test]
    fn fast_uses_a_single_segment() {
        // Segmentation is a Full/Best gate: the Fast tier must keep a single segment
        // (use_segment = false) even on mixed content, so Fast stays byte-identical to
        // the pre-segmentation encoder.
        let (w, h) = (64usize, 64usize);
        let rgba = mixed_image(w, h);
        let payload = encode_frame(&rgba, w, h, 40, FAST);
        let (use_segment, _segs) = decoded_segments(&payload);
        assert!(!use_segment, "Fast must not use segmentation");
    }

    #[test]
    fn segmentation_is_deterministic() {
        // The integer k-means (fixed init + fixed iterations) plus per-segment quant
        // must be fully deterministic per (quality) — identical bytes across encodes.
        let rgba = mixed_image(64, 48);
        let a = encode_frame(&rgba, 64, 48, 40, BALANCED);
        let b = encode_frame(&rgba, 64, 48, 40, BALANCED);
        assert_eq!(a, b, "segmentation must be deterministic");
    }

    #[test]
    fn segmented_odd_and_uniform_dimensions_stay_self_consistent() {
        // Segmentation must hold self-consistency at odd dimensions (partial edge
        // macroblocks, edge-replicated source) on both mixed and uniform content.
        assert_self_consistent(&mixed_image(48, 48), 48, 48, 40);
        assert_self_consistent(&image(17, 13, |_, _| [70, 70, 70]), 17, 13, 40);
        assert_self_consistent(
            &image(19, 23, |x, y| [(x * 13) as u8, (y * 17) as u8, 90]),
            19,
            23,
            40,
        );
    }

    /// The chosen frame size (header + smaller partition candidate) for `rgba` with
    /// segmentation on or off, everything else held at the Balanced settings — the
    /// before/after size the segmentation RD win is measured against.
    fn chosen_total_segmented(
        rgba: &[u8],
        w: usize,
        h: usize,
        base_q: i32,
        uses_segments: bool,
    ) -> usize {
        let FramePlan {
            plans: mb_plans,
            mb_w,
            mb_h,
            seg_params: seg,
            ..
        } = plan_frame(
            rgba,
            w,
            h,
            base_q,
            FULL_GATES,
            uses_segments,
            FrameTuning::AUTO,
            &COEFFS_PROBA_0,
        );
        let skip = super::resolve_skip(&mb_plans, mb_w * mb_h, true);
        let filter = super::choose_filter(base_q, true, 60, 0);
        let header = super::HeaderParams {
            base_q,
            filter: &filter,
            segments: seg,
        };
        let (_p0, _t, d, o, _probas) = emit_best_partitions(&mb_plans, mb_w, mb_h, header, skip);
        KEY_FRAME_HEADER_LEN + d.min(o)
    }

    #[test]
    fn segmentation_improves_size_on_mixed_content() {
        // The rate-distortion payoff: on mixed flat-vs-busy content, coarsening the
        // busy segment (where distortion is masked) saves far more than the segment
        // map + slightly-finer flat segment cost, so the segmented frame codes
        // strictly smaller than the single-segment frame at the same base quantizer.
        let rgba = mixed_image(96, 96);
        for &base_q in &[24i32, 40, 64] {
            let segmented = chosen_total_segmented(&rgba, 96, 96, base_q, true);
            let single = chosen_total_segmented(&rgba, 96, 96, base_q, false);
            assert!(
                segmented < single,
                "q{base_q}: segmented {segmented} did not beat single-segment {single}"
            );
        }
    }

    // ---- mutation-kill: golden exact-encode + pure-decision unit tests --------

    /// A 64-bit FNV-1a digest of `bytes` — a compact stand-in for the exact byte
    /// string, so the golden table below can pin every encode's payload without
    /// embedding kilobytes of literals. The encoder is byte-deterministic (pure
    /// integer arithmetic, no float, no platform-dependent behavior — see the
    /// `encoding_is_deterministic*` tests), so any encoder-decision change flips at
    /// least one output byte and therefore this digest.
    fn golden_digest(bytes: &[u8]) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        h
    }

    /// The captured golden: `"<image> <method> <len> <digest>"` per line, in the
    /// exact order the loop below produces them. Regenerate by running this test
    /// (it prints the actual table on mismatch) and pasting the printed block here.
    const GOLDEN_EXPECTED: &str = "\
gradient fast 190 fdbe0b6ce4ec645c
gradient balanced 89 44cc4831e2b014b5
gradient best 113 f507b9cd3a89366e
noisy fast 7975 f970e253b081638d
noisy balanced 3552 0fe948c3ed2a8dc4
noisy best 3158 f86825f9ecc426ef
flat fast 40 66b6a4ee87e184a3
flat balanced 39 2f5a1f125c0a5180
flat best 39 2f5a1f125c0a5180
checker fast 65 a42deaf46567ecd9
checker balanced 56 6b05842252d1046e
checker best 56 6b05842252d1046e
stripe fast 1680 145aef26907ace38
stripe balanced 322 d0284fcbb5c77800
stripe best 118 00cc0a30e8a48b20
diagonal fast 415 46ac66199192122b
diagonal balanced 274 3f18f25e15a3bdcb
diagonal best 337 737aeebdcede7c37
mixed fast 4363 13644d2cc4723cd5
mixed balanced 1953 97d12de896339d85
mixed best 2044 d741e377a717f630
horizontal fast 667 81e58ac967acf023
horizontal balanced 278 4dcd2a5c0b4cb156
horizontal best 213 e58e54c32cc725bf";

    /// Golden exact-encode test — THE high-leverage encoder-decision net. Encodes a
    /// diverse set of fixed images (smooth gradient, AC-rich noise, flat blocks,
    /// high-frequency checker, fine stripes, diagonal bands, mixed flat-vs-noise, a
    /// horizontally-directional ramp) with each `Effort` and pins the exact output
    /// bytes (via `golden_digest` + length). Every encoder decision — 16×16/chroma
    /// RD mode search, trellis vs round-to-nearest, i4x4 vs 16×16, per-macroblock
    /// skip coding, probability-table optimization, k-means segmentation, per-segment
    /// quantizer allocation, and the loop-filter header — feeds these bytes, so a
    /// mutation to any of them flips a digest here.
    #[test]
    fn golden_exact_encode_bytes() {
        let cases: [(&str, Vec<u8>, usize, usize, i32); 8] = [
            (
                "gradient",
                image(48, 32, |x, y| {
                    [(x * 5) as u8, (y * 7) as u8, ((x + y) * 3) as u8]
                }),
                48,
                32,
                40,
            ),
            ("noisy", noisy96(96, 96), 96, 96, 40),
            ("flat", image(64, 64, |_, _| [120, 90, 200]), 64, 64, 40),
            (
                "checker",
                image(48, 48, |x, y| {
                    if (x / 4 + y / 4) % 2 == 0 {
                        [20, 20, 20]
                    } else {
                        [220, 220, 220]
                    }
                }),
                48,
                48,
                40,
            ),
            ("stripe", stripe_image(64, 64), 64, 64, 40),
            ("diagonal", diagonal_image(96, 96), 96, 96, 100),
            ("mixed", mixed_image(96, 96), 96, 96, 40),
            (
                "horizontal",
                image(48, 48, |_x, y| {
                    [(y * 7) as u8, (y * 3 + 20) as u8, (y * 11) as u8]
                }),
                48,
                48,
                16,
            ),
        ];
        let efforts = [
            ("fast", FAST),
            ("balanced", BALANCED),
            ("best", Effort::Best),
        ];
        let mut lines = Vec::new();
        for (name, rgba, w, h, q) in &cases {
            for (ename, effort) in &efforts {
                let payload = encode_frame(rgba, *w, *h, *q, *effort);
                lines.push(format!(
                    "{name} {ename} {} {:016x}",
                    payload.len(),
                    golden_digest(&payload)
                ));
            }
        }
        let actual = lines.join("\n");
        println!("---GOLDEN ACTUAL---\n{actual}\n---END GOLDEN ACTUAL---");
        assert_eq!(actual, GOLDEN_EXPECTED, "golden encode output changed");
    }

    /// Build a minimal [`MbPlan`] carrying only the `skippable` flag `resolve_skip`
    /// reads; the other fields are inert defaults.
    fn skip_plan(skippable: bool) -> super::MbPlan {
        use crate::lossy::tokens::{Block, MbTokens};
        super::MbPlan {
            ymode: 0,
            imodes: [0; 16],
            uvmode: 0,
            is_i4x4: false,
            skippable,
            has_residual: false,
            tokens: MbTokens {
                is_i4x4: false,
                y2: Block::default(),
                luma: [Block::default(); 16],
                chroma: [Block::default(); 8],
            },
            segment: 0,
        }
    }

    #[test]
    fn resolve_skip_probability_and_gate_are_exact() {
        let plans = |n: usize, skip: usize| -> Vec<super::MbPlan> {
            (0..n).map(|i| skip_plan(i < skip)).collect()
        };
        // Case A: 1 of 4 skippable, skip allowed. skip_p = (3*255)/4 = 191, and the
        // gate fires (191 < 250). Pins the `* 255` and `checked_div` arithmetic.
        let a = super::resolve_skip(&plans(4, 1), 4, true);
        assert!(a.use_skip, "A: enough skips should enable use_skip");
        assert_eq!(a.skip_p, 191, "A: skip_p = (3*255)/4");
        // Case B: same populations but skip disallowed — the gate must stay off even
        // though the population would qualify (pins the `consider_skip &&` conjunct).
        let b = super::resolve_skip(&plans(4, 1), 4, false);
        assert!(
            !b.use_skip,
            "B: consider_skip=false must force use_skip off"
        );
        // Case C: 1 of 100 skippable → skip_p = (99*255)/100 = 252 ≥ 250, so the gate
        // must stay off (pins the `nb_skip > 0 &&` conjunct and the `< 250` bound).
        let c = super::resolve_skip(&plans(100, 1), 100, true);
        assert_eq!(c.skip_p, 252, "C: skip_p = (99*255)/100");
        assert!(!c.use_skip, "C: skip_p 252 ≥ 250 must keep use_skip off");
        // Case D: exact-boundary skip_p == 250 (50*255/51). `< 250` must reject it.
        let d = super::resolve_skip(&plans(51, 1), 51, true);
        assert_eq!(d.skip_p, 250, "D: skip_p = (50*255)/51 = 250 exactly");
        assert!(
            !d.use_skip,
            "D: skip_p == 250 is not < 250, so use_skip off"
        );
    }

    #[test]
    fn mode_decision_lambdas_are_exact() {
        // Pure RD-multiplier ladders: `lambda = max(1, (k * q^2) >> shift)`. Exact
        // values pin the multiply/shift/`max(1, ..)` arithmetic; the `q_ac = 0` case
        // pins the clamp's lower branch (the `< 1` boundary, distinct from `== 1`).
        assert_eq!(super::i4x4_lambda(64), 32, "i4x4: 64^2 >> 7");
        assert_eq!(super::i4x4_lambda(0), 1, "i4x4: clamp 0 -> 1");
        assert_eq!(super::luma16_lambda(16), 6, "luma16: 3*16^2 >> 7");
        assert_eq!(super::luma16_lambda(0), 1, "luma16: clamp 0 -> 1");
        assert_eq!(super::chroma_lambda(8), 3, "chroma: 3*8^2 >> 6");
        assert_eq!(super::chroma_lambda(0), 1, "chroma: clamp 0 -> 1");
    }

    #[test]
    fn segment_tree_and_quant_derivation_is_exact() {
        // segment_tree_probs over ids [0,1,2,2,3] → cnt = [1,1,2,1], n01 = 2, n23 = 3:
        // [tree_prob(2,5), tree_prob(1,2), tree_prob(2,3)] = [102, 127, 170]. Pins the
        // count tally (`+= 1`), the `n01`/`n23` sums, and the `tree_prob` argument sum.
        assert_eq!(
            super::segment_tree_probs(&[0, 1, 2, 2, 3]),
            [102, 127, 170],
            "segment tree probabilities"
        );
        // tree_prob(1, 2) = (1*255/2).clamp(1,255) = 127. Pins the `* 255 / total`
        // scaling and the constant-return mutants.
        assert_eq!(super::tree_prob(1, 2), 127, "tree_prob(1,2)");
        // segment_base_qs: 2 segments with mean activity {64, 192} around base_q 40 at
        // sns 100. sns_quant_delta = (activity-128)*100/512: flat 64 -> -12 (q 28),
        // busy 192 -> +12 (q 52). Pins the zero-centered SNS delta end to end.
        assert_eq!(
            super::segment_base_qs(40, 2, [64, 192, 0, 0], 100),
            [28, 52, 40, 40],
            "per-segment SNS base quantizers"
        );
    }

    #[test]
    fn kmeans_assign_and_update_are_exact() {
        // assign_nearest breaks equidistant ties to the LOWEST index (strict `<`):
        // 10 is 5 from both centroid 5 and centroid 15, so it must land in cluster 0.
        let mut assign = [9u8];
        super::assign_nearest(&[10], [5, 15, 0, 0], 2, &mut assign);
        assert_eq!(assign[0], 0, "equidistant tie -> lowest centroid index");
        // update_centroids moves each non-empty centroid to its cluster's integer mean
        // and returns the total displacement: cluster 0 = {10, 20} -> 15, moved from 0.
        let mut cent = [0i64; 4];
        let disp = super::update_centroids(&[10, 20], &[0, 0], &mut cent, 2);
        assert_eq!(cent[0], 15, "centroid 0 -> mean(10,20)");
        assert_eq!(disp, 15, "displacement = |15 - 0|");
    }

    // ---- mutation-kill (round 2): surgical decision-path unit tests ------------

    /// `SplitMix64` byte generator seeded from `seed`, matching the deterministic
    /// fills used by the exact-value kill tests below.
    fn splitmix_bytes(seed: u64, n: usize) -> Vec<u8> {
        let mut st = seed.wrapping_add(1);
        (0..n)
            .map(|_| {
                st = st.wrapping_add(0x9E37_79B9_7F4A_7C15);
                let mut z = st;
                z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
                ((z ^ (z >> 31)) & 0xff) as u8
            })
            .collect()
    }

    #[test]
    fn i16x16_mode_cost_is_exactly_two_coded_bits() {
        // The i16/i4 mode-signaling cost is a fixed `2 * 256`; the `*`->`+` mutant gives
        // 258 and `*`->`/` gives 0, both of which perturb every i4x4-vs-16x16 rate term
        // (and the 16x16 mode-search rate term). Pin the exact constant.
        assert_eq!(super::I16X16_MODE_COST, 512);
    }

    #[test]
    fn mode_available_requires_all_named_neighbors() {
        use crate::lossy::constants::TM_PRED;
        // DC is always available.
        assert!(super::mode_available(DC_PRED, false, false));
        // V needs the top row; H needs the left column.
        assert!(super::mode_available(V_PRED, true, false));
        assert!(!super::mode_available(V_PRED, false, false));
        assert!(super::mode_available(H_PRED, false, true));
        assert!(!super::mode_available(H_PRED, false, false));
        // TM needs BOTH neighbors: the `&&`->`||` mutant would accept a single one.
        assert!(!super::mode_available(TM_PRED, true, false));
        assert!(!super::mode_available(TM_PRED, false, true));
        assert!(super::mode_available(TM_PRED, true, true));
    }

    #[test]
    fn chroma8_reconstruction_transforms_every_dense_block() {
        // The reconstruction-SSE scorer inverse-transforms each chroma block guarded by
        // `any(c != 0)` (skip only the all-zero block). Flipping it to `== 0` (mutants
        // 1969 for U, 1973 for V) would SKIP a fully dense block, leaving it at pure
        // prediction and mis-scoring the mode. Both U and V sub-block 0 are dense here,
        // so the exact summed U+V post-quant SSE pins both guards.
        use super::Planes;
        use crate::lossy::rgb_to_yuv::SourceYuv;
        let mut planes = Planes::new(1, 1);
        let uvs = planes.uv_stride;
        let uv_off = uvs + 1;
        for r in 0..8 {
            for c in 0..8 {
                planes.u[uv_off + r * uvs + c] = 100;
                planes.v[uv_off + r * uvs + c] = 110;
            }
        }
        let mut coeffs = [0i16; 128];
        for k in 0..16 {
            coeffs[k] = 50; // U sub-block 0: dense (no zero coefficient)
            coeffs[64 + k] = 50; // V sub-block 0: dense
        }
        let src = SourceYuv {
            y: vec![0u8; 256],
            u: vec![128u8; 64],
            v: vec![130u8; 64],
            mb_w: 1,
            mb_h: 1,
        };
        let sse = super::chroma8_reconstruction_sse(&mut planes, &coeffs, uv_off, &src, 0, 0);
        assert_eq!(
            sse, 85896,
            "dense U+V blocks must be reconstructed before scoring"
        );
    }

    #[test]
    fn has_residual_is_false_for_zero_coefficient_blocks() {
        // `has_residual = coeffs.any(|c| c != 0)`. A flat frame gives interior
        // macroblocks whose residual quantizes to all-zero, so `has_residual` MUST be
        // false there. The `!= 0`->`== 0` mutant computes `any(c == 0)`, which is true
        // for every real block (each carries at least one zero coefficient), so under
        // the mutant no plan would ever report `has_residual == false`.
        let rgba = image(64, 64, |_, _| [120, 90, 200]);
        let FramePlan {
            plans: mb_plans, ..
        } = plan_frame(
            &rgba,
            64,
            64,
            40,
            FULL_GATES,
            true,
            FrameTuning::AUTO,
            &COEFFS_PROBA_0,
        );
        assert!(
            mb_plans.iter().any(|p| !p.has_residual),
            "a flat frame must contain zero-residual macroblocks"
        );
    }

    #[test]
    fn default_partition_total_is_the_byte_length_sum() {
        // `default_total = part0_d.len() + token_d.len()`. The `+`->`*` mutant makes it a
        // product (hundreds x hundreds), which also flips the keep-smaller comparison
        // that consumes it. Pin the exact sum for a fixed AC-rich frame.
        let noisy = noisy96(96, 96);
        let FramePlan {
            plans: mb_plans,
            mb_w,
            mb_h,
            seg_params: seg,
            ..
        } = plan_frame(
            &noisy,
            96,
            96,
            40,
            FULL_GATES,
            true,
            FrameTuning::AUTO,
            &COEFFS_PROBA_0,
        );
        let skip = super::resolve_skip(&mb_plans, mb_w * mb_h, true);
        let filter = super::choose_filter(40, true, 60, 0);
        let header = super::HeaderParams {
            base_q: 40,
            filter: &filter,
            segments: seg,
        };
        let (_p0, _t, default_total, optimized_total, _probas) =
            emit_best_partitions(&mb_plans, mb_w, mb_h, header, skip);
        assert_eq!(default_total, 4979, "default candidate byte-length sum");
        assert_eq!(optimized_total, 3538, "optimized candidate byte-length sum");
    }

    #[test]
    fn segmentation_spread_threshold_is_strict() {
        // plan_segmentation stays single-segment only when the complexity spread is
        // STRICTLY below `max / SEG_SPREAD_DEN`: `(max - min) * 4 < max`. Two
        // macroblock-complexity levels of exactly 912 and 1216 hit the boundary
        // ((1216 - 912) * 4 == 1216), so the strict `<` proceeds to segment (params
        // Some) while the `<=` mutant collapses to a single segment (params None).
        use crate::lossy::rgb_to_yuv::SourceYuv;
        let amps = [6i32, 6, 8, 8]; // complexities 912, 912, 1216, 1216
        let mut y = vec![0u8; 4 * 16 * 16]; // stride 64, 16 rows, 4 macroblocks wide
        for (k, &amp) in amps.iter().enumerate() {
            for r in 0..16 {
                for lc in 0..16 {
                    let v = (128 + amp * ((lc % 4) as i32)).clamp(0, 255) as u8;
                    y[r * 64 + k * 16 + lc] = v;
                }
            }
        }
        let src = SourceYuv {
            y,
            u: vec![128; 4 * 8 * 8],
            v: vec![128; 4 * 8 * 8],
            mb_w: 4,
            mb_h: 1,
        };
        let seg = super::plan_segmentation(&src, 40, true, FrameTuning::AUTO);
        assert!(
            seg.params.is_some(),
            "two distinct activity levels must segment under the near-best tuning"
        );
    }

    #[test]
    fn skippable_luma_last_index_boundary_is_strict() {
        // `skippable` requires every luma block's `last < 1` (no AC beyond DC-position).
        // A macroblock with an empty Y2, empty chroma, and every luma block `last <= 1`
        // with at least one exactly 1 sits on that boundary: the strict `< 1` keeps it
        // NON-skippable; the `<= 1` mutant would wrongly mark it skippable. Scan a range
        // of qualities over mixed flat-vs-noise content for such a witness (robust to
        // which macroblock/quality the mode/quant choice lands it on — the boundary MB
        // recurs across the whole q=107..=127 band under the tuned trellis lambda).
        let rgba = mixed_image(96, 96);
        let witness = (2..=127)
            .find_map(|q| {
                let FramePlan {
                    plans: mb_plans, ..
                } = plan_frame(
                    &rgba,
                    96,
                    96,
                    q,
                    FULL_GATES,
                    true,
                    FrameTuning::AUTO,
                    &COEFFS_PROBA_0,
                );
                mb_plans.into_iter().find(|p| {
                    p.tokens.y2.last < 0
                        && p.tokens.chroma.iter().all(|b| b.last < 0)
                        && p.tokens.luma.iter().all(|b| b.last <= 1)
                        && p.tokens.luma.iter().any(|b| b.last == 1)
                })
            })
            .expect("mixed content must contain a luma-last==1 boundary macroblock");
        assert!(
            !witness.skippable,
            "a luma last==1 block must not be skippable"
        );
    }

    #[test]
    fn keep_smaller_partition_prefers_default_on_size_ties() {
        // At a size tie (optimized_total == default_total) the strict `<` keeps the
        // DEFAULT candidate; the `<=` mutant switches to the optimized one, whose bytes
        // differ (it transmits probability updates). A 16x16 gradient at q24 hits such a
        // tie, so pinning the exact payload digest pins the default choice.
        let rgba = image(16, 16, |x, y| {
            [(x * 5) as u8, (y * 7) as u8, ((x + y) * 3) as u8]
        });
        let payload = encode_frame(&rgba, 16, 16, 24, BALANCED);
        assert_eq!(
            golden_digest(&payload),
            0x6e07_e15d_6c4e_0924,
            "at a size tie the encoder must keep the default probability table"
        );
    }

    #[test]
    fn i4x4_search_transforms_dense_subblocks() {
        // `search_luma_i4x4` inverse-transforms each sub-block guarded by `any(c != 0)`.
        // At the finest quantizer a high-entropy source keeps DENSE (no-zero) sub-blocks,
        // so the `== 0` mutant would skip transforming them, changing the reconstructed
        // distortion (and, via the cascade into later sub-blocks' predictions, the bits).
        // Pin the exact returned SSE for a fixed random block at base_q 0.
        use super::{Planes, QuantPlan, Quantizer};
        use crate::lossy::constants::COEFFS_PROBA_0;
        use crate::lossy::rgb_to_yuv::SourceYuv;
        let mut planes = Planes::new(1, 1);
        let ys = planes.y_stride;
        let y_off = ys + 1;
        let src = SourceYuv {
            y: splitmix_bytes(0, 256),
            u: vec![128; 64],
            v: vec![128; 64],
            mb_w: 1,
            mb_h: 1,
        };
        let plan = QuantPlan {
            quant: Quantizer::new(0),
            uses_trellis: false,
            sns_strength: 0,
            probas: &COEFFS_PROBA_0,
        };
        let (_cand, dist, _bits) =
            super::search_luma_i4x4(&mut planes, y_off, &src, 0, 0, false, true, plan);
        assert_eq!(
            dist, 115,
            "dense i4x4 sub-blocks must be reconstructed before scoring"
        );
    }

    #[test]
    fn i4x4_vs_16x16_decision_is_strict_at_a_cost_tie() {
        // try_i4x4_luma picks i4x4 only when its RD cost is STRICTLY below the 16x16
        // cost; the `<`->`<=` mutant would pick i4x4 at an exact tie. Construct that tie:
        // for a fixed source at base_q 12 the i4x4 cost is 1_865_022; seed the 16x16
        // prediction (with coeffs = 0, empty tokens) so its distortion is exactly 7280,
        // giving cost16 = 256 * 7280 + 2 * 671 = 1_865_022 == cost4. The strict `<` then
        // keeps 16x16 (None); the `<=` mutant flips to i4x4 (Some).
        use super::{Planes, QuantPlan, Quantizer};
        use crate::lossy::constants::COEFFS_PROBA_0;
        use crate::lossy::rgb_to_yuv::SourceYuv;
        use crate::lossy::tokens::{Block, MbTokens};

        fn isqrt(n: i64) -> i64 {
            let mut x = 0i64;
            while (x + 1) * (x + 1) <= n {
                x += 1;
            }
            x
        }

        let src = SourceYuv {
            y: splitmix_bytes(42, 256),
            u: vec![128; 64],
            v: vec![128; 64],
            mb_w: 1,
            mb_h: 1,
        };
        let mut planes = Planes::new(1, 1);
        let ys = planes.y_stride;
        let y_off = ys + 1;
        // Seed the interior equal to the source (zero distortion), then inject squared
        // per-pixel deltas that sum to EXACTLY 7280 (integer sums of squares only).
        for r in 0..16 {
            for c in 0..16 {
                planes.y[y_off + r * ys + c] = src.y[r * 16 + c];
            }
        }
        let mut remaining: i64 = 7280;
        let mut idx = 0usize;
        while remaining > 0 {
            let (row, col) = (idx / 16, idx % 16);
            idx += 1;
            let s = i32::from(src.y[row * 16 + col]);
            let room = (255 - s).max(s);
            let d = i32::try_from(isqrt(remaining)).unwrap_or(0).min(room);
            let v = if 255 - s >= s { s + d } else { s - d };
            planes.y[y_off + row * ys + col] = v as u8;
            remaining -= i64::from(d) * i64::from(d);
        }

        let coeffs = [0i16; 384];
        let tokens = MbTokens {
            is_i4x4: false,
            y2: Block::default(),
            luma: [Block::default(); 16],
            chroma: [Block::default(); 8],
        };
        let plan = QuantPlan {
            quant: Quantizer::new(12),
            uses_trellis: false,
            sns_strength: 0,
            probas: &COEFFS_PROBA_0,
        };
        let result = super::try_i4x4_luma(
            &mut planes,
            &coeffs,
            &tokens,
            &src,
            (0, 0),
            false,
            true,
            plan,
        );
        assert!(
            result.is_none(),
            "at an exact i4x4/16x16 cost tie the encoder must keep 16x16 (strict <)"
        );
    }

    /// A textured directional gradient: a smooth luma/chroma ramp (`rx`/`ry` slopes,
    /// with the blue and green channels tilted on different axes so chroma competes on
    /// its own) overlaid with a fine two-pixel dither of amplitude `damp`. On this
    /// content several whole-block modes fit within a hair of each other but with
    /// DIFFERENT token-bit costs, so the RD cost arithmetic — not distortion alone —
    /// decides the luma-16 and chroma-8 winners. Chosen so a single flipped `+`/`*` in
    /// the cost/rate expression moves a mode and therefore an output byte.
    fn rd_image(rx: i32, ry: i32, damp: i32) -> Vec<u8> {
        image(48, 48, |x, y| {
            let d = if (x * 7 + y * 13) % 2 == 0 { 0 } else { damp };
            let r = (60 + x as i32 * rx + d).clamp(0, 255) as u8;
            let g = (120 + y as i32 * ry - d).clamp(0, 255) as u8;
            let b = (100 + x as i32 * ry + y as i32 * rx + d).clamp(0, 255) as u8;
            [r, g, b]
        })
    }

    /// Golden exact-encode table for the RD-competition images — the net for the
    /// whole-block RD cost arithmetic: `select_luma16_mode_rd`'s rate `+`->`*` (2010)
    /// and cost `*`->`+` (2014), and `select_chroma8_mode_rd`'s cost `*`->`+` (2060).
    /// Each listed image was verified to move at least one mode (and thus this digest)
    /// under each of those mutations. Regenerate by running this test (it prints the
    /// actual table on mismatch) and pasting the block.
    const RD_GOLDEN_EXPECTED: &str = "\
rd_rx0_ry5_d0_q48 ec3e1d9169c5d34a
rd_rx1_ry0_d0_q8 2dbc60b6031d7af4
rd_rx0_ry2_d0_q24 b6e5b4d5e6bc4ab2
rd_rx0_ry5_d60_q24 ab1c536dec055d08";

    #[test]
    fn rd_cost_arithmetic_golden() {
        // (name, rx, ry, damp, q). q48/q8/q24 span where rate weighting (`lambda`)
        // tips the balance; the first two flip all three mutants, the last two add
        // dedicated chroma (2060) and luma (2010/2014) coverage.
        let cases: [(&str, i32, i32, i32, i32); 4] = [
            ("rd_rx0_ry5_d0_q48", 0, 5, 0, 48),
            ("rd_rx1_ry0_d0_q8", 1, 0, 0, 8),
            ("rd_rx0_ry2_d0_q24", 0, 2, 0, 24),
            ("rd_rx0_ry5_d60_q24", 0, 5, 60, 24),
        ];
        let mut lines = Vec::new();
        for (name, rx, ry, damp, q) in &cases {
            let payload = encode_frame(&rd_image(*rx, *ry, *damp), 48, 48, *q, BALANCED);
            lines.push(format!("{name} {:016x}", golden_digest(&payload)));
        }
        let actual = lines.join("\n");
        println!("---RD GOLDEN ACTUAL---\n{actual}\n---END RD GOLDEN ACTUAL---");
        assert_eq!(
            actual, RD_GOLDEN_EXPECTED,
            "RD cost-arithmetic golden changed"
        );
    }

    #[test]
    fn kmeans_segments_partition_is_exact() {
        // Two crafted four-cluster inputs pin the full k-means: the evenly-spaced
        // centroid seeding (`min + span*k/3`), the six refinement passes, the
        // empty-cluster collapse and the per-segment mean. Input A (min 100) pins the
        // `span * k / 3` seeding arithmetic; input B (large min, tight spread) pins the
        // `span = max - min` computation — a `max + min` seeding pushes the upper seeds
        // past the data and collapses the frame to one cluster, so the exact 4-way
        // split below fails under any span/seed mutation.
        let a = [
            100i64, 105, 110, 1100, 1105, 1110, 2100, 2105, 2110, 3100, 3105, 3110,
        ];
        let (ids_a, cnt_a, seg_a) = super::kmeans_segments(&a, 4);
        assert_eq!(ids_a, vec![0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 3], "A ids");
        assert_eq!(cnt_a, 4, "A count");
        assert_eq!(seg_a, [105, 1105, 2105, 3105], "A segment means");

        let b = [
            1000i64, 1010, 1020, 1100, 1110, 1120, 1200, 1210, 1220, 1300, 1310, 1320,
        ];
        let (ids_b, cnt_b, seg_b) = super::kmeans_segments(&b, 4);
        assert_eq!(ids_b, vec![0, 0, 0, 1, 1, 1, 2, 2, 2, 3, 3, 3], "B ids");
        assert_eq!(cnt_b, 4, "B count");
        assert_eq!(seg_b, [1010, 1110, 1210, 1310], "B segment means");
    }
}
