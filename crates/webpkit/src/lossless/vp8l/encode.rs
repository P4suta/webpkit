//! VP8L payload encoder (Tier 0/1/2): literals, an optional subtract-green
//! transform, LZ77 back-references, and a color cache.
//!
//! This is the exact producer for [`crate::lossless::vp8l::decode::decode`]. For each of
//! the two tiers — Tier 0 (raw pixels) and Tier 1 (subtract-green) — the encoder
//! parses the pixels into [`Token`]s once, uses the [`Histogram`] cost model to
//! pick a color-cache size for each reference strategy, then materializes a
//! small set of candidates and keeps the smallest by actual bytes. Because the
//! all-literal / no-cache candidate is always in the set, a solid image still
//! collapses to an empty pixel-data section and the encoder never does worse
//! than the plain Tier 0/1 baseline.
//!
//! Bit-exactness rests on traps handled elsewhere and relied on here:
//! [`crate::lossless::huffman::canonical::emit_codes`] returns `(0, 0)` for any alphabet
//! with `<= 1` used symbol (a single-valued channel costs zero bits per pixel);
//! [`crate::lossless::transform::subtract_green::forward`] subtracts per channel so
//! borrows never propagate from blue into red; and [`resolve`] is the one place
//! the color cache is applied, so the histogram and emit passes insert pixels in
//! the identical order the decoder does.
#![expect(
    clippy::cast_possible_truncation,
    reason = "channel indices are masked to 8 bits, prefix symbols index alphabets \
              bounded by the format, and pixel counts fit the validated 14-bit VP8L \
              dimensions, so every narrowing cast here is value-preserving"
)]

use crate::lossless::bit_io::writer::BitWriter;
use crate::lossless::color_cache::ColorCache;
use crate::lossless::constants::{
    ALPHABET_SIZE, COLOR_INDEXING_TRANSFORM, CROSS_COLOR_TRANSFORM, MAX_CACHE_BITS,
    MIN_TRANSFORM_BITS, NUM_LENGTH_CODES, NUM_LITERAL_CODES, NUM_TRANSFORM_BITS,
    PREDICTOR_TRANSFORM, SUBTRACT_GREEN_TRANSFORM, VP8L_IMAGE_SIZE_BITS, VP8L_MAGIC_BYTE,
    VP8L_VERSION_BITS, subsample_size,
};
use crate::lossless::histogram::Histogram;
use crate::lossless::huffman::build::build_code_lengths;
use crate::lossless::huffman::canonical::emit_codes;
use crate::lossless::huffman::serialize::write_huffman_code;
use crate::lossless::lz77::{PlaneCodeMap, prefix_encode};
use crate::lossless::prelude::*;
use crate::lossless::transform::{cross_color, palette, predictor, subtract_green};
use crate::lossless::vp8l::backref::{Resolved, Token, parse, parse_lz77, parse_optimal, resolve};
use crate::lossless::vp8l::meta::{self, MetaPlan};
use crate::lossless::work::work;

/// Maximum Huffman code length for the five main channel codes. VP8L caps code
/// lengths at 15 bits; kept in lockstep with the decoder's
/// [`crate::lossless::constants::MAX_ALLOWED_CODE_LENGTH`] by the assertion below.
const MAX_MAIN_CODE_LENGTH: u32 = 15;
const _: () = assert!(crate::lossless::constants::MAX_ALLOWED_CODE_LENGTH == 15);

/// The 2-bit transform-type field width in the VP8L transform header.
const TRANSFORM_TYPE_BITS: u32 = 2;
/// Width of the transmitted color-cache-bits field.
const CACHE_BITS_FIELD_WIDTH: u32 = 4;
/// Width of the transmitted `num_colors - 1` field of a color-indexing transform.
const NUM_COLORS_FIELD_WIDTH: u32 = 8;

/// One transform to write into the VP8L transform list, carrying whatever nested
/// sub-image the decoder reads back for it. Written in vec order by
/// [`write_transforms`]; the decoder applies the inverses in reverse.
///
/// Predictor/cross-color carry a per-tile sub-image (`subsample_size(width, bits)`
/// wide); color-indexing carries the `num_colors`-wide color map. Subtract-green
/// is pointwise and carries no sub-image.
enum TransformPlan {
    /// Spatial predictor: per-tile mode sub-image (mode packed as `(m & 0xf) << 8`).
    Predictor { bits: u32, tile_data: Vec<u32> },
    /// Cross-color decorrelation: per-tile multiplier-code sub-image.
    CrossColor { bits: u32, tile_data: Vec<u32> },
    /// Subtract-green: pointwise, no sub-image.
    SubtractGreen,
    /// Color indexing (palette): a `num_colors`-wide delta-coded color-map sub-image.
    ColorIndexing { num_colors: u32, colormap: Vec<u32> },
}

/// Encode `argb` (native `0xAARRGGBB`, row-major, `width * height` pixels) as a
/// raw VP8L payload beginning at the `0x2f` signature.
///
/// Both tiers are optimized independently and the smaller result is returned; on
/// a tie Tier 0 wins. The output is a valid VP8L bitstream that round-trips
/// through [`crate::lossless::vp8l::decode::decode`].
#[must_use]
pub(crate) fn encode(width: u32, height: u32, argb: &[u32]) -> Vec<u8> {
    encode_with(width, height, argb, true)
}

/// Encode as [`encode`], but with `use_lz77` cleared (the `Fast` method) the
/// encoder skips the LZ77 / color-cache search and emits only the literal +
/// subtract-green tiers — faster, slightly larger.
#[must_use]
pub(crate) fn encode_with(width: u32, height: u32, argb: &[u32], use_lz77: bool) -> Vec<u8> {
    // Alpha-used is a header advisory computed once; subtract-green leaves the
    // alpha channel untouched, so both tiers share the same value.
    let alpha_used = argb.iter().any(|&p| p >> 24 != 0xff);
    let tier0 = encode_stream(width, height, alpha_used, argb, false, use_lz77);
    let tier1 = encode_stream(width, height, alpha_used, argb, true, use_lz77);
    if tier1.len() < tier0.len() {
        tier1
    } else {
        tier0
    }
}

/// Byte length of the fixed VP8L stream header: the `0x2f` signature (8 bits),
/// two 14-bit dimension fields, the 1-bit alpha-used advisory and the 3-bit
/// version — exactly 40 bits, a whole 5 bytes. Because it is byte-aligned the bit
/// writer starts the transform list at byte 5, so a *headerless* level-0 stream is
/// byte-for-byte the tail of a normal stream past this prefix.
const VP8L_HEADER_BYTES: usize = 5;
const _: () = assert!(
    8 + 2 * VP8L_IMAGE_SIZE_BITS as usize + 1 + VP8L_VERSION_BITS as usize == VP8L_HEADER_BYTES * 8
);

/// Encode an alpha plane as a HEADERLESS level-0 VP8L stream — the lossless
/// (`method = 1`) body of an `ALPH` chunk, i.e. the bytes AFTER its 1-byte header.
///
/// Each alpha byte rides in the GREEN lane of a synthetic ARGB image; the other
/// three lanes are a single constant (`0`), so red/blue/alpha collapse to zero-bit
/// single-symbol codes and only the alpha (green) channel costs anything. The same
/// Tier 0/1 LZ77 + color-cache search as [`encode`] runs — the 5-byte header is a
/// constant prefix that never perturbs the candidate ranking — and dropping that
/// header yields exactly what [`crate::lossless::vp8l::decode::decode_alpha_stream`] reads
/// back: it drives the image-stream decoder directly and extracts `(argb >> 8)`.
///
/// Even if the search were to pick the subtract-green tier, the round-trip still
/// holds: subtract-green leaves the green lane untouched, so the recovered green is
/// the source alpha regardless.
#[must_use]
pub(crate) fn encode_alpha_stream(alpha: &[u8], width: u32, height: u32) -> Vec<u8> {
    let argb: Vec<u32> = alpha.iter().map(|&a| u32::from(a) << 8).collect();
    let mut full = encode(width, height, &argb);
    full.split_off(VP8L_HEADER_BYTES)
}

/// Optimize and encode one tier. When `use_subtract_green` is set the pixels are
/// copied and transformed in place; otherwise the caller's slice is borrowed.
///
/// The pixels are parsed once per reference strategy; the cost model picks a
/// cache size for each; then up to three candidates are materialized and the
/// smallest kept. The `(literal, no-cache)` candidate is always first, so this
/// never regresses the plain literal baseline.
fn encode_stream(
    width: u32,
    height: u32,
    alpha_used: bool,
    argb: &[u32],
    use_subtract_green: bool,
    use_lz77: bool,
) -> Vec<u8> {
    // Borrow for Tier 0; own a transformed copy for Tier 1. `owned` is only
    // read on the branch that initializes it, so Tier 0 allocates nothing extra.
    let owned;
    let pixels: &[u32] = if use_subtract_green {
        let mut copy = argb.to_vec();
        subtract_green::forward(&mut copy);
        owned = copy;
        &owned
    } else {
        argb
    };

    // This tier's transform list: a lone subtract-green, or nothing. Both leave
    // the working width equal to the image width, so `header_width == model.width`.
    let sg = [TransformPlan::SubtractGreen];
    let plans: &[TransformPlan] = if use_subtract_green { &sg } else { &[] };

    if !use_lz77 {
        // Fast method: the literal, no-cache baseline only (per tier).
        let tokens_literal = parse(pixels, false);
        let model_literal = RefModel::new(&tokens_literal, pixels, width);
        return emit_stream(
            width,
            height,
            alpha_used,
            plans,
            0,
            &model_literal,
            &model_literal.histogram(0),
        );
    }
    // Full LZ77 + color-cache search; the pixels are already at the image width,
    // so the header and working widths coincide.
    search_lz77(width, height, alpha_used, plans, width, pixels)
}

/// Full LZ77 + color-cache candidate search over already-transformed `pixels`
/// laid out at `working_width` (which a color-indexing transform reduces below
/// `header_width`; every other family leaves them equal), returning the smallest
/// stream. Parses both reference strategies, lets the cost model pick a cache
/// size for each, materializes up to three candidates, and keeps the smallest.
/// The `(literal, no-cache)` candidate is always first, so this never regresses
/// the plain literal baseline for the given `plans`.
fn search_lz77(
    header_width: u32,
    header_height: u32,
    alpha_used: bool,
    plans: &[TransformPlan],
    working_width: u32,
    pixels: &[u32],
) -> Vec<u8> {
    let emit = |cache_bits, model: &RefModel<'_>, histogram: &Histogram| {
        emit_stream(
            header_width,
            header_height,
            alpha_used,
            plans,
            cache_bits,
            model,
            histogram,
        )
    };
    // Each candidate's parse + model live only until its stream is folded into
    // `best`, then drop — so the peak never holds every full-size token stream at
    // once. The fold order (literal@0, literal@cache, lz77, dp) is unchanged, so
    // the selected bytes are identical; only the memory high-water mark shrinks.
    //
    // Baseline: all literals, no cache — the non-regression floor. Dropped before
    // the LZ77/DP work below, which never needs the literal stream. The cache
    // search hands back the winning size's histogram, reused by that candidate's
    // emit (the @0 emit still builds its own, since the winner may be > 0).
    let mut best = {
        let tokens_literal = parse(pixels, false);
        let model_literal = RefModel::new(&tokens_literal, pixels, working_width);
        let (cache_literal, hist_literal) = best_cache_bits(&model_literal);
        let mut best = emit(0, &model_literal, &model_literal.histogram(0));
        if cache_literal > 0 {
            keep_smaller(
                &mut best,
                emit(cache_literal, &model_literal, &hist_literal),
            );
        }
        best
    };
    // The greedy LZ77 stream is kept alive only as the DP seed. Its hash chain is
    // carried alongside so the DP reuses it instead of rebuilding the same
    // pure-function-of-pixels chain (byte-identical either way).
    let (tokens_lz77, chain) = parse_lz77(pixels);
    {
        let model_lz77 = RefModel::new(&tokens_lz77, pixels, working_width);
        let (cache_lz77, hist_lz77) = best_cache_bits(&model_lz77);
        keep_smaller(&mut best, emit(cache_lz77, &model_lz77, &hist_lz77));
    }
    // Cost-model-driven optimal parse: a self-floored extra candidate (chosen only
    // when strictly smaller by real bytes), seeded from the greedy stream. Once the
    // DP has consumed it, the greedy stream and its chain are no longer needed here.
    let tokens_dp = parse_optimal(pixels, working_width, &tokens_lz77, &chain);
    drop(tokens_lz77);
    drop(chain);
    {
        let model_dp = RefModel::new(&tokens_dp, pixels, working_width);
        let (cache_dp, hist_dp) = best_cache_bits(&model_dp);
        keep_smaller(&mut best, emit(cache_dp, &model_dp, &hist_dp));
    }
    best
}

/// [`search_lz77`] plus a meta-Huffman candidate: it reproduces the single-group
/// candidate set (so it self-floors) and, when [`meta::plan`] finds a beneficial
/// grouping on the LZ77 model, offers a multi-group stream too. Best-only.
fn search_lz77_best(
    header_width: u32,
    header_height: u32,
    alpha_used: bool,
    plans: &[TransformPlan],
    working_width: u32,
    pixels: &[u32],
) -> Vec<u8> {
    let emit = |cache_bits, model: &RefModel<'_>, histogram: &Histogram| {
        emit_stream(
            header_width,
            header_height,
            alpha_used,
            plans,
            cache_bits,
            model,
            histogram,
        )
    };
    // The literal candidate's parse + model live only until its stream is folded,
    // then drop before the LZ77/DP/meta work — which never needs the literal
    // stream. Fold order is unchanged, so the selected bytes are identical. The
    // cache search's winning histogram is reused by that candidate's emit (the @0
    // emit still builds its own, since the winner may be > 0).
    let mut best = {
        let tokens_literal = parse(pixels, false);
        let model_literal = RefModel::new(&tokens_literal, pixels, working_width);
        let (cache_literal, hist_literal) = best_cache_bits(&model_literal);
        let mut best = emit(0, &model_literal, &model_literal.histogram(0));
        if cache_literal > 0 {
            keep_smaller(
                &mut best,
                emit(cache_literal, &model_literal, &hist_literal),
            );
        }
        best
    };
    // The greedy stream and its model live on: seed for the DP and input to the
    // meta-Huffman shot below. Its hash chain is carried only as far as the DP,
    // which reuses it instead of rebuilding the same chain (byte-identical either
    // way).
    let (tokens_lz77, chain) = parse_lz77(pixels);
    let model_lz77 = RefModel::new(&tokens_lz77, pixels, working_width);
    let (cache_lz77, hist_lz77) = best_cache_bits(&model_lz77);
    keep_smaller(&mut best, emit(cache_lz77, &model_lz77, &hist_lz77));
    // Cost-model-driven optimal parse: a self-floored extra candidate (single
    // group), seeded from the greedy stream.
    let tokens_dp = parse_optimal(pixels, working_width, &tokens_lz77, &chain);
    drop(chain);
    let model_dp = RefModel::new(&tokens_dp, pixels, working_width);
    let (cache_dp, hist_dp) = best_cache_bits(&model_dp);
    keep_smaller(&mut best, emit(cache_dp, &model_dp, &hist_dp));
    let ysize = pixels.len() as u32 / working_width;
    if let Some(plan) = meta::plan(&tokens_lz77, pixels, working_width, ysize, cache_lz77) {
        keep_smaller(
            &mut best,
            emit_stream_meta(
                header_width,
                header_height,
                alpha_used,
                plans,
                cache_lz77,
                &model_lz77,
                &plan,
            ),
        );
    }
    // The same meta-Huffman shot on the DP stream — also self-floored.
    if let Some(plan_dp) = meta::plan(&tokens_dp, pixels, working_width, ysize, cache_dp) {
        keep_smaller(
            &mut best,
            emit_stream_meta(
                header_width,
                header_height,
                alpha_used,
                plans,
                cache_dp,
                &model_dp,
                &plan_dp,
            ),
        );
    }
    best
}

/// Tile-bits candidates for the predictor/cross-color families. [`keep_smaller`]
/// ranks by REAL emitted bytes, so widening this array can only shrink the result.
const TRANSFORM_TILE_BITS_SWEEP: [u32; 4] = [2, 3, 4, 5];
const _: () = {
    let mut i = 0;
    while i < TRANSFORM_TILE_BITS_SWEEP.len() {
        assert!(
            TRANSFORM_TILE_BITS_SWEEP[i] >= MIN_TRANSFORM_BITS
                && TRANSFORM_TILE_BITS_SWEEP[i] < MIN_TRANSFORM_BITS + (1 << NUM_TRANSFORM_BITS)
        );
        i += 1;
    }
};

/// One independent `Effort::Best` candidate family. Evaluated in the fixed order
/// [`build_best_tasks`] emits, so [`reduce_task_minima`] is deterministic and the
/// serial and rayon evaluators agree byte-for-byte.
#[derive(Clone, Copy)]
enum BestTask {
    Floor,
    Palette,
    Predictor(u32),
    CrossColor(u32),
}

/// The canonical candidate task order: floor, palette, then predictor and
/// cross-color once per swept tile size.
fn build_best_tasks() -> Vec<BestTask> {
    let mut tasks = vec![BestTask::Floor, BestTask::Palette];
    tasks.extend(
        TRANSFORM_TILE_BITS_SWEEP
            .iter()
            .map(|&b| BestTask::Predictor(b)),
    );
    tasks.extend(
        TRANSFORM_TILE_BITS_SWEEP
            .iter()
            .map(|&b| BestTask::CrossColor(b)),
    );
    tasks
}

/// Evaluate one task into its ordered candidate streams. The four `*_streams`
/// helpers hold the family logic once, shared by the serial and rayon evaluators.
fn run_best_task(
    task: BestTask,
    width: u32,
    height: u32,
    alpha_used: bool,
    argb: &[u32],
) -> Vec<Vec<u8>> {
    match task {
        BestTask::Floor => floor_streams(width, height, alpha_used, argb),
        BestTask::Palette => palette_streams(width, height, alpha_used, argb),
        BestTask::Predictor(bits) => predictor_streams(width, height, alpha_used, argb, bits),
        BestTask::CrossColor(bits) => cross_color_streams(width, height, alpha_used, argb, bits),
    }
}

/// The Tier 0/1/2 floor (always first, so it seeds `best` and wins ties) plus the
/// two no-transform meta-Huffman shots.
fn floor_streams(width: u32, height: u32, alpha_used: bool, argb: &[u32]) -> Vec<Vec<u8>> {
    let floor = encode_with(width, height, argb, true);
    let raw_meta = search_lz77_best(width, height, alpha_used, &[], width, argb);
    let mut sg = argb.to_vec();
    subtract_green::forward(&mut sg);
    let sg_meta = search_lz77_best(
        width,
        height,
        alpha_used,
        &[TransformPlan::SubtractGreen],
        width,
        &sg,
    );
    vec![floor, raw_meta, sg_meta]
}

/// The palette family (empty when the image has > 256 distinct colors).
fn palette_streams(width: u32, height: u32, alpha_used: bool, argb: &[u32]) -> Vec<Vec<u8>> {
    if let Some(p) = palette::forward(argb, width) {
        let working_width = subsample_size(width, p.bits);
        let plans = [TransformPlan::ColorIndexing {
            num_colors: p.num_colors,
            colormap: p.colormap,
        }];
        vec![search_lz77_best(
            width,
            height,
            alpha_used,
            &plans,
            working_width,
            &p.bundled,
        )]
    } else {
        Vec::new()
    }
}

/// The predictor family at one tile size: predictor alone, then predictor +
/// subtract-green (spatial before pointwise).
fn predictor_streams(
    width: u32,
    height: u32,
    alpha_used: bool,
    argb: &[u32],
    bits: u32,
) -> Vec<Vec<u8>> {
    let (residual, tile_data) = predictor::forward(argb, width, height, bits);
    let alone = search_lz77_best(
        width,
        height,
        alpha_used,
        &[TransformPlan::Predictor {
            bits,
            tile_data: tile_data.clone(),
        }],
        width,
        &residual,
    );
    let mut residual_sg = residual;
    subtract_green::forward(&mut residual_sg);
    let with_sg = search_lz77_best(
        width,
        height,
        alpha_used,
        &[
            TransformPlan::Predictor { bits, tile_data },
            TransformPlan::SubtractGreen,
        ],
        width,
        &residual_sg,
    );
    vec![alone, with_sg]
}

/// The cross-color family at one tile size: cross-color alone, then cross-color +
/// subtract-green.
fn cross_color_streams(
    width: u32,
    height: u32,
    alpha_used: bool,
    argb: &[u32],
    bits: u32,
) -> Vec<Vec<u8>> {
    let (stored, tile_data) = cross_color::forward(argb, width, height, bits);
    let alone = search_lz77_best(
        width,
        height,
        alpha_used,
        &[TransformPlan::CrossColor {
            bits,
            tile_data: tile_data.clone(),
        }],
        width,
        &stored,
    );
    let mut stored_sg = stored;
    subtract_green::forward(&mut stored_sg);
    let with_sg = search_lz77_best(
        width,
        height,
        alpha_used,
        &[
            TransformPlan::CrossColor { bits, tile_data },
            TransformPlan::SubtractGreen,
        ],
        width,
        &stored_sg,
    );
    vec![alone, with_sg]
}

/// Fold one family's ordered candidate streams into that family's minimum by REAL
/// emitted bytes (tie -> the earliest stream), or `None` for an empty family (only
/// palette, when the image exceeds 256 colors). This is [`keep_smaller`] applied
/// left-to-right, so folding per-family minima and then folding those across
/// families yields the identical winner as flattening every stream and folding
/// once — but never holds more than one family's streams live at a time.
fn keep_smallest(streams: Vec<Vec<u8>>) -> Option<Vec<u8>> {
    let mut streams = streams.into_iter();
    let mut best = streams.next()?;
    for candidate in streams {
        keep_smaller(&mut best, candidate);
    }
    Some(best)
}

/// Fold the per-family minima (already in canonical task order) into the single
/// smallest stream, ties resolving to the earliest family — empty families are
/// skipped, so the seed is the first non-empty family's minimum (the floor
/// family, always present), and the floor still wins global ties exactly as when
/// every stream was flattened and folded together.
fn reduce_task_minima(minima: impl IntoIterator<Item = Option<Vec<u8>>>) -> Vec<u8> {
    minima
        .into_iter()
        .flatten()
        .reduce(|mut best, candidate| {
            keep_smaller(&mut best, candidate);
            best
        })
        .unwrap_or_default()
}

/// Run every task serially (default) or across a rayon thread pool (feature
/// `rayon`), folding each family to its own minimum and then across families, so
/// the peak never holds every candidate stream at once — only the running best
/// plus the family currently being evaluated.
///
/// Both paths preserve task order: the serial map folds families in emission
/// order, and `Vec::into_par_iter().map().collect()` is an
/// `IndexedParallelIterator`, so `minima[i]` is always
/// `keep_smallest(run_best_task(tasks[i], ..))` regardless of scheduling — hence
/// rayon-on output equals rayon-off byte-for-byte.
#[cfg(not(feature = "rayon"))]
fn evaluate_best_tasks(
    tasks: Vec<BestTask>,
    width: u32,
    height: u32,
    alpha_used: bool,
    argb: &[u32],
) -> Vec<u8> {
    // Serial: `reduce_task_minima` drives the lazy map, so each family's streams
    // are produced, folded to one, and dropped before the next family runs.
    reduce_task_minima(
        tasks
            .into_iter()
            .map(|t| keep_smallest(run_best_task(t, width, height, alpha_used, argb))),
    )
}
#[cfg(feature = "rayon")]
fn evaluate_best_tasks(
    tasks: Vec<BestTask>,
    width: u32,
    height: u32,
    alpha_used: bool,
    argb: &[u32],
) -> Vec<u8> {
    use rayon::prelude::*;
    // Parallel: each worker folds its family to a single stream, so the collected
    // vec holds one stream per non-empty family (not every candidate); `collect`
    // keeps task order, so the fold below resolves ties identically to serial.
    let minima: Vec<Option<Vec<u8>>> = tasks
        .into_par_iter()
        .map(|t| keep_smallest(run_best_task(t, width, height, alpha_used, argb)))
        .collect();
    reduce_task_minima(minima)
}

/// Tier 3 ("Best") encode: return the SMALLEST stream among the Tier 0/1/2 floor
/// and each forward-transform family (palette, predictor, cross-color). The floor
/// is always in the candidate set, so Best never regresses [`encode`].
///
/// Palette is mutually exclusive with the spatial transforms (it reduces the
/// working width; the others do not), so the families are evaluated as separate
/// candidates rather than combined into one stream. When a pointwise
/// subtract-green rides on a spatial transform, the plans are built — and the
/// forwards applied — in the mandated `PALETTE -> PREDICTOR -> CROSS_COLOR ->
/// SUBTRACT_GREEN` order, which the decoder inverts in reverse.
#[must_use]
pub(crate) fn encode_best(width: u32, height: u32, argb: &[u32]) -> Vec<u8> {
    let alpha_used = argb.iter().any(|&p| p >> 24 != 0xff);
    evaluate_best_tasks(build_best_tasks(), width, height, alpha_used, argb)
}

/// Replace `best` with `candidate` when the candidate is strictly smaller.
fn keep_smaller(best: &mut Vec<u8>, candidate: Vec<u8>) {
    if candidate.len() < best.len() {
        *best = candidate;
    }
}

/// One back-reference's cache-independent bitstream cost: the length and
/// distance prefix symbols and their extra-bit widths. Computed once per copy by
/// [`RefModel::new`] so the per-cache-size cost passes never recompute
/// `prefix_encode` / the O(120) `distance_to_plane_code` scan.
#[derive(Clone, Copy)]
struct CopyCost {
    length_symbol: u32,
    length_bits: u32,
    dist_symbol: u32,
    dist_bits: u32,
}

/// The cost model for one parsed token stream, reused across every candidate
/// cache size and by the final emit.
///
/// A back-reference's length/distance symbols and extra bits do not depend on
/// the color cache, so [`Self::new`] resolves them once into [`CopyCost`]s; only
/// the literal-vs-cache decision and the per-pixel cache-insert order vary with
/// the cache size, and those are replayed per candidate in [`Self::histogram`].
/// This hoists the `prefix_encode` / `distance_to_plane_code` work the old
/// per-cache-size `accumulate` recomputed for all `1..=MAX_CACHE_BITS + 1`
/// candidates (times two strategies, times two tiers).
pub(crate) struct RefModel<'a> {
    /// The parsed token stream (borrowed for the model's lifetime).
    tokens: &'a [Token],
    /// The pixels the tokens were parsed from, for the copies' cache inserts.
    pixels: &'a [u32],
    /// Image width, for the header and the distance plane-code mapping.
    width: u32,
    /// The resolved cost of each `Copy` token, in token order.
    copies: Vec<CopyCost>,
}

impl<'a> RefModel<'a> {
    /// Resolve the cache-independent cost of every back-reference in one pass.
    pub(crate) fn new(tokens: &'a [Token], pixels: &'a [u32], width: u32) -> Self {
        // `width` is fixed, so resolve every copy's distance through one reverse
        // map instead of the per-copy 120-entry rescan.
        let plane_map = PlaneCodeMap::new(width);
        let copies = tokens
            .iter()
            .filter_map(|&token| match token {
                Token::Copy { length, distance } => {
                    let (length_symbol, length_bits, _) = prefix_encode(length);
                    let plane_code = plane_map.plane_code(distance);
                    let (dist_symbol, dist_bits, _) = prefix_encode(plane_code);
                    Some(CopyCost {
                        length_symbol,
                        length_bits,
                        dist_symbol,
                        dist_bits,
                    })
                },
                Token::Literal(_) => None,
            })
            .collect();
        Self {
            tokens,
            pixels,
            width,
            copies,
        }
    }

    /// Build the symbol histogram for a `cache_bits`-wide cache (`0` = disabled).
    ///
    /// Copies contribute their pre-resolved [`CopyCost`]s (no `prefix_encode` /
    /// `distance_to_plane_code` recomputation); only the literals' cache
    /// decisions and the copies' cache inserts are replayed here, in the exact
    /// output order [`resolve`] uses (literal: check-then-insert; copy:
    /// `pixels[pos..pos + length]`). The result is therefore bit-identical to
    /// walking every unit through `resolve` — pinned by the
    /// `ref_model_matches_accumulate` proptest — so the histogram feeding the
    /// Huffman builder can never disagree with the [`emit_tokens`] pass.
    pub(crate) fn histogram(&self, cache_bits: u32) -> Histogram {
        let cache_codes = if cache_bits > 0 {
            1usize << cache_bits
        } else {
            0
        };
        work!(HistogramAlloc);
        let mut histogram = Histogram::new(ALPHABET_SIZE[0] + cache_codes);
        let mut cache = (cache_bits > 0).then(|| {
            work!(ColorCacheAlloc);
            ColorCache::new(cache_bits)
        });
        self.accumulate_into(cache_bits, &mut histogram, cache.as_mut());
        histogram
    }

    /// Fill an already-zeroed `histogram` from the token stream under a
    /// `cache_bits`-wide cache (`cache` is `Some` iff `cache_bits > 0`, and must
    /// be reset to `cache_bits` on entry). This is the single walk shared by the
    /// allocating [`Self::histogram`] and the buffer-reusing [`best_cache_bits`]
    /// sweep; both therefore produce byte-identical bins.
    fn accumulate_into(
        &self,
        cache_bits: u32,
        histogram: &mut Histogram,
        mut cache: Option<&mut ColorCache>,
    ) {
        work!(HistogramPass);
        let mut pos = 0usize;
        let mut next_copy = 0usize;
        for &token in self.tokens {
            match token {
                Token::Literal(argb) => {
                    match cache.as_deref_mut() {
                        None => histogram.add_literal(argb),
                        Some(cache) => {
                            let key = ColorCache::index(argb, cache_bits);
                            if cache.get(key) == argb {
                                histogram.add_cache(key as u16);
                            } else {
                                histogram.add_literal(argb);
                            }
                            cache.insert(argb);
                        },
                    }
                    pos += 1;
                },
                Token::Copy { length, .. } => {
                    let cost = self.copies[next_copy];
                    next_copy += 1;
                    histogram.add_length(cost.length_symbol, cost.length_bits);
                    histogram.add_distance(cost.dist_symbol, cost.dist_bits);
                    if let Some(cache) = cache.as_deref_mut() {
                        for pixel in &self.pixels[pos..pos + length as usize] {
                            cache.insert(*pixel);
                        }
                    }
                    pos += length as usize;
                },
            }
        }
    }
}

/// Choose the color-cache size (`0` = disabled) that minimizes the model's
/// estimated encoded size, returning it alongside the winning size's histogram.
/// The scan is ascending from `0` and updates only on a strict improvement, so
/// ties prefer the smaller size (a cache that does not help is left off). This
/// ordering is load-bearing: changing it would change the selected cache size
/// and hence the emitted bytes.
///
/// The returned histogram is a truncated snapshot of the scratch at the winning
/// size, bin-identical to [`RefModel::histogram`] for `best_bits`; handing it
/// back lets [`emit_coded_pixels`] skip re-accumulating the same bins — pinned by
/// `best_cache_bits_histogram_matches_fresh`.
fn best_cache_bits(model: &RefModel<'_>) -> (u32, Histogram) {
    // Reuse one max-size scratch histogram + cache across all MAX_CACHE_BITS + 1
    // candidates instead of allocating a fresh pair per size (a fresh-per-size pass
    // would do ~12 heap allocations per model, times three models, times two tiers,
    // times every Best transform family). The green bins are zero-padded to the largest
    // alphabet; since `estimate_bits` skips zero-count bins, the reused estimate
    // is byte-identical to a per-size exact histogram — pinned by
    // `best_cache_bits_reuse_matches_fresh`.
    let mut hist = Histogram::new(ALPHABET_SIZE[0] + (1usize << MAX_CACHE_BITS));
    let mut cache = ColorCache::new(MAX_CACHE_BITS);

    let mut best_bits = 0;
    work!(CacheBitsBuild);
    hist.reset();
    model.accumulate_into(0, &mut hist, None);
    let mut best_cost = hist.estimate_bits();
    // Snapshot the winning scratch, truncated to that size's green alphabet
    // (`ALPHABET_SIZE[0] + (1 << bits)`, or `ALPHABET_SIZE[0]` for no cache): the
    // bins past it are always zero, so this equals a fresh per-size histogram.
    let mut best_hist = hist.snapshot_truncated(ALPHABET_SIZE[0]);
    for bits in 1..=MAX_CACHE_BITS {
        work!(CacheBitsBuild);
        hist.reset();
        cache.reset(bits);
        model.accumulate_into(bits, &mut hist, Some(&mut cache));
        let cost = hist.estimate_bits();
        if cost < best_cost {
            best_cost = cost;
            best_bits = bits;
            best_hist = hist.snapshot_truncated(ALPHABET_SIZE[0] + (1usize << bits));
        }
    }
    (best_bits, best_hist)
}

/// Serialize a level-0 VP8L payload: the 5-byte header (carrying the ORIGINAL
/// image dimensions), the `plans` transform list, the color-cache header, the
/// single-group meta-Huffman bit, and the coded pixel data.
///
/// The coded pixels are laid out at the model's WORKING width (`model.width`),
/// which a color-indexing transform reduces below `header_width`; every other
/// transform family leaves them equal. The cost model supplies the symbol
/// histogram and [`emit_tokens`] re-walks the same tokens through [`resolve`].
fn emit_stream(
    header_width: u32,
    header_height: u32,
    alpha_used: bool,
    plans: &[TransformPlan],
    cache_bits: u32,
    model: &RefModel<'_>,
    histogram: &Histogram,
) -> Vec<u8> {
    let mut bw = BitWriter::new();
    write_header(&mut bw, header_width, header_height, alpha_used);
    write_transforms(&mut bw, plans, header_width, header_height);
    write_color_cache(&mut bw, cache_bits);
    bw.write_bits(0, 1); // no meta-Huffman
    emit_coded_pixels(&mut bw, cache_bits, model, histogram);
    bw.into_bytes()
}

/// Level-0 multi-group (meta-Huffman) stream: like [`emit_stream`] but writes the
/// meta bit = 1, the 3-bit precision, the entropy sub-image, and one Huffman group
/// per cluster. The single-group [`emit_stream`] is untouched, so Balanced/Fast
/// bytes are unaffected.
fn emit_stream_meta(
    header_width: u32,
    header_height: u32,
    alpha_used: bool,
    plans: &[TransformPlan],
    cache_bits: u32,
    model: &RefModel<'_>,
    plan: &MetaPlan,
) -> Vec<u8> {
    let mut bw = BitWriter::new();
    write_header(&mut bw, header_width, header_height, alpha_used);
    write_transforms(&mut bw, plans, header_width, header_height);
    write_color_cache(&mut bw, cache_bits);
    bw.write_bits(1, 1); // meta-Huffman present
    bw.write_bits(plan.bits - MIN_TRANSFORM_BITS, NUM_TRANSFORM_BITS); // inverse of read_bits(3)+2
    let entropy_pixels: Vec<u32> = plan.groups.iter().map(|&g| u32::from(g) << 8).collect();
    emit_subimage(&mut bw, plan.entropy_xsize, &entropy_pixels);
    emit_coded_pixels_meta(&mut bw, cache_bits, model, plan);
    bw.into_bytes()
}

/// Emit one Huffman group per cluster (all five channel codes, in group-id order,
/// exactly as the decoder reads them) followed by the group-selected pixel tokens.
fn emit_coded_pixels_meta(
    bw: &mut BitWriter,
    cache_bits: u32,
    model: &RefModel<'_>,
    plan: &MetaPlan,
) {
    let prefix_sets: Vec<[Prefix; 5]> = plan
        .group_histograms
        .iter()
        .map(|h| {
            [
                Prefix::from_histogram(h.green()),
                Prefix::from_histogram(h.red()),
                Prefix::from_histogram(h.blue()),
                Prefix::from_histogram(h.alpha()),
                Prefix::from_histogram(h.dist()),
            ]
        })
        .collect();
    for set in &prefix_sets {
        for prefix in set {
            write_huffman_code(bw, &prefix.lengths);
        }
    }
    emit_tokens_meta(
        bw,
        model.tokens,
        model.pixels,
        cache_bits,
        model.width,
        &prefix_sets,
        plan,
    );
}

/// Emit every token's bits, selecting the group's prefix set by the unit's
/// start-position block — byte-identical selection to the decoder's `select_group`.
fn emit_tokens_meta(
    bw: &mut BitWriter,
    tokens: &[Token],
    pixels: &[u32],
    cache_bits: u32,
    width: u32,
    prefix_sets: &[[Prefix; 5]],
    plan: &MetaPlan,
) {
    resolve(tokens, pixels, cache_bits, width, |pos, unit| {
        let x = pos as u32 % width;
        let y = pos as u32 / width;
        let block = ((y >> plan.bits) * plan.entropy_xsize + (x >> plan.bits)) as usize;
        let [green, red, blue, alpha, dist] = &prefix_sets[plan.groups[block] as usize];
        match unit {
            Resolved::Literal(argb) => {
                green.emit(bw, ((argb >> 8) & 0xff) as usize);
                red.emit(bw, ((argb >> 16) & 0xff) as usize);
                blue.emit(bw, (argb & 0xff) as usize);
                alpha.emit(bw, ((argb >> 24) & 0xff) as usize);
            },
            Resolved::Copy {
                length_symbol,
                length_extra,
                dist_symbol,
                dist_extra,
            } => {
                green.emit(bw, NUM_LITERAL_CODES + length_symbol as usize);
                bw.write_bits(length_extra.0, length_extra.1);
                dist.emit(bw, dist_symbol as usize);
                bw.write_bits(dist_extra.0, dist_extra.1);
            },
            Resolved::Cache(key) => {
                green.emit(bw, NUM_LITERAL_CODES + NUM_LENGTH_CODES + usize::from(key));
            },
        }
    });
}

/// Emit the entropy-coded tail shared by every image stream: the five channel
/// Huffman codes followed by the pixel tokens. The pixel width comes from the
/// model (the working width), and the caller has already written the color-cache
/// header (plus, for level-0 streams, the meta-Huffman bit) ahead of this. The
/// `histogram` must be the model's histogram at `cache_bits` (as returned by
/// [`RefModel::histogram`] or [`best_cache_bits`]); the caller supplies it so the
/// bins built for the cache-size search can be reused instead of re-accumulated.
fn emit_coded_pixels(
    bw: &mut BitWriter,
    cache_bits: u32,
    model: &RefModel<'_>,
    histogram: &Histogram,
) {
    let prefixes = [
        Prefix::from_histogram(histogram.green()),
        Prefix::from_histogram(histogram.red()),
        Prefix::from_histogram(histogram.blue()),
        Prefix::from_histogram(histogram.alpha()),
        Prefix::from_histogram(histogram.dist()),
    ];
    for prefix in &prefixes {
        write_huffman_code(bw, &prefix.lengths);
    }
    emit_tokens(
        bw,
        model.tokens,
        model.pixels,
        cache_bits,
        model.width,
        &prefixes,
    );
}

/// Emit a nested (`is_level0 = false`) sub-image of `width`-wide `pixels`: the
/// color-cache header (always absent) then the coded pixels, with NO transform
/// list and NO meta-Huffman bit — exactly what [`crate::lossless::vp8l::decode`] reads for a
/// transform's tile grid or a palette's color map. The height is implicit in
/// `pixels.len() / width`. The pixels are coded as literals with no color cache.
fn emit_subimage(bw: &mut BitWriter, width: u32, pixels: &[u32]) {
    write_color_cache(bw, 0);
    let tokens = parse(pixels, false);
    let model = RefModel::new(&tokens, pixels, width);
    emit_coded_pixels(bw, 0, &model, &model.histogram(0));
}

/// Write the fixed VP8L header: signature, `width - 1`, `height - 1`, the
/// alpha-used advisory bit, and the (always-zero) version field.
fn write_header(bw: &mut BitWriter, width: u32, height: u32, alpha_used: bool) {
    bw.write_bits(u32::from(VP8L_MAGIC_BYTE), 8);
    bw.write_bits(width - 1, VP8L_IMAGE_SIZE_BITS);
    bw.write_bits(height - 1, VP8L_IMAGE_SIZE_BITS);
    bw.write_bits(u32::from(alpha_used), 1);
    bw.write_bits(0, VP8L_VERSION_BITS);
}

/// Write the transform list: for each plan (in vec order) a present bit, its
/// 2-bit type code, and any header + nested sub-image, then the terminating "no
/// more transforms" bit. `width` is the ORIGINAL image width, used to size the
/// predictor/cross-color tile grid via [`subsample_size`]. `_height` completes
/// the transform's logical geometry (mirroring the decoder's `ysize`) but every
/// sub-image's row count is implicit in its emitted slice length, so no arm reads
/// it.
fn write_transforms(bw: &mut BitWriter, plans: &[TransformPlan], width: u32, _height: u32) {
    for plan in plans {
        bw.write_bits(1, 1); // one transform present
        match plan {
            TransformPlan::Predictor { bits, tile_data } => {
                bw.write_bits(PREDICTOR_TRANSFORM, TRANSFORM_TYPE_BITS);
                bw.write_bits(*bits - MIN_TRANSFORM_BITS, NUM_TRANSFORM_BITS);
                emit_subimage(bw, subsample_size(width, *bits), tile_data);
            },
            TransformPlan::CrossColor { bits, tile_data } => {
                bw.write_bits(CROSS_COLOR_TRANSFORM, TRANSFORM_TYPE_BITS);
                bw.write_bits(*bits - MIN_TRANSFORM_BITS, NUM_TRANSFORM_BITS);
                emit_subimage(bw, subsample_size(width, *bits), tile_data);
            },
            TransformPlan::SubtractGreen => {
                bw.write_bits(SUBTRACT_GREEN_TRANSFORM, TRANSFORM_TYPE_BITS);
            },
            TransformPlan::ColorIndexing {
                num_colors,
                colormap,
            } => {
                bw.write_bits(COLOR_INDEXING_TRANSFORM, TRANSFORM_TYPE_BITS);
                bw.write_bits(*num_colors - 1, NUM_COLORS_FIELD_WIDTH);
                emit_subimage(bw, *num_colors, colormap);
            },
        }
    }
    bw.write_bits(0, 1); // no (further) transforms
}

/// Write the color-cache header: a present bit and, if set, the 4-bit size.
fn write_color_cache(bw: &mut BitWriter, cache_bits: u32) {
    if cache_bits > 0 {
        bw.write_bits(1, 1);
        bw.write_bits(cache_bits, CACHE_BITS_FIELD_WIDTH);
    } else {
        bw.write_bits(0, 1);
    }
}

/// Emit every token's bits, matching the decoder's read order exactly.
fn emit_tokens(
    bw: &mut BitWriter,
    tokens: &[Token],
    pixels: &[u32],
    cache_bits: u32,
    width: u32,
    prefixes: &[Prefix; 5],
) {
    let [green, red, blue, alpha, dist] = prefixes;
    resolve(tokens, pixels, cache_bits, width, |_pos, unit| match unit {
        Resolved::Literal(argb) => {
            green.emit(bw, ((argb >> 8) & 0xff) as usize);
            red.emit(bw, ((argb >> 16) & 0xff) as usize);
            blue.emit(bw, (argb & 0xff) as usize);
            alpha.emit(bw, ((argb >> 24) & 0xff) as usize);
        },
        Resolved::Copy {
            length_symbol,
            length_extra,
            dist_symbol,
            dist_extra,
        } => {
            green.emit(bw, NUM_LITERAL_CODES + length_symbol as usize);
            bw.write_bits(length_extra.0, length_extra.1);
            dist.emit(bw, dist_symbol as usize);
            bw.write_bits(dist_extra.0, dist_extra.1);
        },
        Resolved::Cache(key) => {
            green.emit(bw, NUM_LITERAL_CODES + NUM_LENGTH_CODES + usize::from(key));
        },
    });
}

/// The prefix code for one channel: per-symbol lengths (for serialization) plus
/// the LSB-first `(code, emit_len)` pairs used to emit each symbol.
struct Prefix {
    /// Per-symbol code lengths (`0` = unused), sized to the channel alphabet.
    lengths: Vec<u32>,
    /// LSB-first emit `(code, len)` per symbol. `len == 0` for a `<= 1`-symbol
    /// alphabet, so a single-valued channel costs zero bits per pixel.
    codes: Vec<(u32, u32)>,
}

impl Prefix {
    /// Build the length-limited (`<= 15`) prefix code for one channel histogram.
    fn from_histogram(histogram: &[u32]) -> Self {
        let lengths = build_code_lengths(histogram, MAX_MAIN_CODE_LENGTH);
        let codes = emit_codes(&lengths);
        Self { lengths, codes }
    }

    /// Emit `symbol`'s code; a strict no-op for a single-symbol alphabet, whose
    /// entries are `(0, 0)` and `write_bits(_, 0)` writes nothing.
    fn emit(&self, bw: &mut BitWriter, symbol: usize) {
        let (code, len) = self.codes[symbol];
        bw.write_bits(code, len);
    }
}

#[cfg(test)]
mod tests {
    use super::{
        RefModel, TransformPlan, best_cache_bits, emit_stream, emit_stream_meta, encode,
        encode_best, encode_stream,
    };
    use crate::lossless::transform::predictor;
    use crate::lossless::vp8l::backref::parse;
    use crate::lossless::vp8l::decode::decode;
    use crate::lossless::vp8l::meta;

    /// Pack channels into a native ARGB pixel (`0xAARRGGBB`).
    fn argb(a: u32, r: u32, g: u32, b: u32) -> u32 {
        (a << 24) | (r << 16) | (g << 8) | b
    }

    /// Encode then decode and assert an exact round-trip.
    fn assert_round_trip(width: u32, height: u32, pixels: &[u32]) {
        let payload = encode(width, height, pixels);
        let decoded = decode(&payload).expect("our encoder must emit decodable VP8L");
        assert_eq!((decoded.width, decoded.height), (width, height));
        assert_eq!(decoded.argb.as_slice(), pixels);
    }

    /// `alpha_used` as the encoder computes it (used to drive `encode_stream`).
    fn alpha_used(pixels: &[u32]) -> bool {
        pixels.iter().any(|&p| p >> 24 != 0xff)
    }

    /// The all-literal, no-cache, no-transform baseline stream.
    fn literal_baseline(width: u32, height: u32, pixels: &[u32]) -> Vec<u8> {
        let tokens = parse(pixels, false);
        let model = RefModel::new(&tokens, pixels, width);
        emit_stream(
            width,
            height,
            alpha_used(pixels),
            &[],
            0,
            &model,
            &model.histogram(0),
        )
    }

    #[test]
    fn round_trips_a_1x1_pixel() {
        assert_round_trip(1, 1, &[argb(255, 50, 100, 200)]);
    }

    #[test]
    fn solid_image_has_empty_pixel_data() {
        // Every channel is a single-symbol code, so the pixel-data section is
        // empty regardless of pixel count. LZ77/cache must not inflate this: the
        // 1x1 and 4x4 payloads differ only in the 14-bit dimension fields and
        // must have identical length.
        let color = argb(255, 10, 20, 30);
        let small = encode(1, 1, &[color]);
        let large = encode(4, 4, &[color; 16]);
        assert_eq!(small.len(), large.len());
        assert_round_trip(1, 1, &[color]);
        assert_round_trip(4, 4, &[color; 16]);
    }

    #[test]
    fn round_trips_a_gradient() {
        let pixels: Vec<u32> = (0..64u32).map(|v| argb(255, v * 2, v, 255 - v)).collect();
        assert_round_trip(8, 8, &pixels);
    }

    #[test]
    fn round_trips_all_transparent_preserving_rgb() {
        // Alpha is 0 everywhere (so alpha_used is set) but the distinct RGB must
        // survive: the encoder never discards fully-transparent color data.
        let pixels: Vec<u32> = (0..16u32).map(|v| argb(0, v, 63 - v, v * 3)).collect();
        assert!(alpha_used(&pixels));
        assert_round_trip(4, 4, &pixels);
    }

    #[test]
    fn round_trips_a_single_row() {
        let pixels: Vec<u32> = (0..20u32).map(|v| argb(255, v, 100, 200 - v)).collect();
        assert_round_trip(20, 1, &pixels);
    }

    #[test]
    fn round_trips_a_single_column() {
        let pixels: Vec<u32> = (0..20u32).map(|v| argb(255, 5, v, v)).collect();
        assert_round_trip(1, 20, &pixels);
    }

    #[test]
    fn tier1_chosen_when_subtract_green_shrinks() {
        // Grayscale ramp (r == g == b): subtract-green zeroes red and blue into
        // single-symbol codes, leaving only green to code, so Tier 1 is smaller.
        let pixels: Vec<u32> = (0..32u32).map(|v| argb(255, v, v, v)).collect();
        let au = alpha_used(&pixels);
        let tier0 = encode_stream(32, 1, au, &pixels, false, true);
        let tier1 = encode_stream(32, 1, au, &pixels, true, true);
        assert!(
            tier1.len() < tier0.len(),
            "subtract-green must shrink a grayscale ramp"
        );
        assert_eq!(encode(32, 1, &pixels), tier1);
        assert_round_trip(32, 1, &pixels);
    }

    #[test]
    fn tier0_chosen_when_only_green_varies() {
        // Only green varies; red/blue/alpha are constant. Subtract-green would
        // spread green's variation into red and blue, enlarging the stream, so
        // Tier 0 wins (also exercising the tie-goes-to-Tier-0 `<=` boundary).
        let pixels: Vec<u32> = (0..32u32).map(|v| argb(255, 0x40, v, 0x80)).collect();
        let au = alpha_used(&pixels);
        let tier0 = encode_stream(32, 1, au, &pixels, false, true);
        let tier1 = encode_stream(32, 1, au, &pixels, true, true);
        assert!(tier0.len() <= tier1.len());
        assert_eq!(encode(32, 1, &pixels), tier0);
        assert_round_trip(32, 1, &pixels);
    }

    #[test]
    fn lz77_shrinks_a_repeating_pattern() {
        // A 4-color tile repeated 64 times: back-references must beat literals.
        let tile = [
            argb(255, 10, 20, 30),
            argb(255, 40, 50, 60),
            argb(255, 70, 80, 90),
            argb(255, 100, 110, 120),
        ];
        let pixels: Vec<u32> = tile.iter().cycle().take(256).copied().collect();
        let full = encode(64, 4, &pixels);
        let baseline = literal_baseline(64, 4, &pixels);
        assert!(
            full.len() < baseline.len(),
            "LZ77 must shrink a repeating pattern: {} vs {}",
            full.len(),
            baseline.len()
        );
        assert_round_trip(64, 4, &pixels);
    }

    #[test]
    fn cost_model_enables_cache_for_a_scattered_palette() {
        // Twelve colors scattered with no runs (mirroring the color_cache_scatter
        // fixture): a cache hit codes a whole pixel with one green symbol, so the
        // cost model must turn the cache on — the path dwebp validates at gen time.
        let pixels: Vec<u32> = (0..1024u32)
            .map(|i| {
                let (x, y) = (i % 32, i / 32);
                let c = (x.wrapping_mul(5).wrapping_add(y.wrapping_mul(11))) % 12;
                argb(255, c * 20 + 8, c * 13 + 17, c * 7 + 29)
            })
            .collect();
        let tokens = parse(&pixels, false);
        let (bits, _) = best_cache_bits(&RefModel::new(&tokens, &pixels, 32));
        assert!(
            bits > 0,
            "cost model must enable a cache for a scattered palette"
        );
        assert_round_trip(32, 32, &pixels);
    }

    #[test]
    fn forced_cache_stream_round_trips() {
        // Force a cache-coded stream (two alternating colors) and confirm the
        // decoder's cache path replays it exactly. This exercises the encoder's
        // cache emission and insert order independent of the cost model's choice.
        let a = argb(255, 1, 2, 3);
        let b = argb(255, 250, 240, 230);
        let pixels: Vec<u32> = (0..40u32).map(|i| if i % 2 == 0 { a } else { b }).collect();
        let tokens = parse(&pixels, false);
        let model = RefModel::new(&tokens, &pixels, 40);
        let stream = emit_stream(40, 1, false, &[], 6, &model, &model.histogram(6));
        let decoded = decode(&stream).expect("a cache-coded stream must decode");
        assert_eq!((decoded.width, decoded.height), (40, 1));
        assert_eq!(decoded.argb, pixels);
    }

    #[test]
    fn predictor_subimage_stream_round_trips() {
        // Hand-build a level-0 stream carrying a predictor transform whose per-tile
        // mode grid is emitted as a nested (is_level0 = false) sub-image, then
        // decode it back to the source. This proves `emit_stream` +
        // `write_transforms` + `emit_subimage` reproduce the decoder's nested read
        // order end-to-end, before the tiered `encode_best` driver exists.
        let width = 4u32;
        let height = 4u32;
        let source: Vec<u32> = (0..16u32)
            .map(|v| argb(255, v * 4, v * 2, 100 + v))
            .collect();
        // bits = 2 -> a single 4x4 tile covers the image; `forward` picks the mode.
        let bits = 2u32;
        let (residual, tile_data) = predictor::forward(&source, width, height, bits);

        // The coded pixels are the residual; the header carries the ORIGINAL dims,
        // and the predictor tile grid rides along as the nested sub-image.
        let tokens = parse(&residual, false);
        let model = RefModel::new(&tokens, &residual, width);
        let plans = [TransformPlan::Predictor { bits, tile_data }];
        let stream = emit_stream(
            width,
            height,
            alpha_used(&source),
            &plans,
            0,
            &model,
            &model.histogram(0),
        );

        let decoded = decode(&stream).expect("a predictor sub-image stream must decode");
        assert_eq!((decoded.width, decoded.height), (width, height));
        assert_eq!(decoded.argb, source);
    }

    #[test]
    fn never_regresses_the_literal_baseline() {
        // Whatever the input, the chosen stream is never larger than the plain
        // literal baseline that is always among the candidates.
        let cases: [(u32, u32, Vec<u32>); 3] = [
            (
                8,
                8,
                (0..64u32).map(|v| argb(255, v, v * 3, v * 7)).collect(),
            ),
            (16, 1, vec![argb(255, 5, 5, 5); 16]),
            (
                4,
                4,
                (0..16u32).map(|v| argb(255, v % 3, v % 3, v % 3)).collect(),
            ),
        ];
        for (w, h, pixels) in cases {
            assert!(encode(w, h, &pixels).len() <= literal_baseline(w, h, &pixels).len());
            assert_round_trip(w, h, &pixels);
        }
    }

    /// Encode with the Tier 3 `Best` driver, decode, and assert an exact
    /// round-trip through the transform families it may emit.
    fn assert_best_round_trip(width: u32, height: u32, pixels: &[u32]) {
        let payload = encode_best(width, height, pixels);
        let decoded = decode(&payload).expect("encode_best must emit decodable VP8L");
        assert_eq!((decoded.width, decoded.height), (width, height));
        assert_eq!(decoded.argb.as_slice(), pixels);
    }

    #[test]
    fn best_never_regresses_the_floor() {
        // The Tier 0/1/2 floor is always a candidate, so Best is never larger than
        // Balanced (`encode`) for any input.
        let cases: [(u32, u32, Vec<u32>); 3] = [
            (
                8,
                8,
                (0..64u32).map(|v| argb(255, v, v * 3, v * 7)).collect(),
            ),
            (16, 1, vec![argb(255, 5, 5, 5); 16]),
            (
                4,
                4,
                (0..16u32).map(|v| argb(255, v % 3, v % 3, v % 3)).collect(),
            ),
        ];
        for (w, h, pixels) in cases {
            assert!(encode_best(w, h, &pixels).len() <= encode(w, h, &pixels).len());
            assert_best_round_trip(w, h, &pixels);
        }
    }

    #[test]
    fn best_palette_beats_the_literal_baseline_on_a_small_palette() {
        // A 16x16 field of eight scattered colors. Palette coding packs 4-bit
        // indices two-per-byte (bits = 1) plus a tiny color map, which is far
        // smaller than coding every pixel's four channels as a literal.
        let colors = [
            argb(255, 10, 20, 30),
            argb(255, 200, 40, 60),
            argb(255, 70, 220, 90),
            argb(255, 100, 110, 240),
            argb(255, 33, 66, 99),
            argb(255, 240, 240, 10),
            argb(255, 15, 250, 250),
            argb(255, 250, 15, 130),
        ];
        let pixels: Vec<u32> = (0..256usize)
            .map(|i| colors[(i * 7 + i / 16) % colors.len()])
            .collect();
        let best = encode_best(16, 16, &pixels);
        let baseline = literal_baseline(16, 16, &pixels);
        assert!(
            best.len() < baseline.len(),
            "palette Best must beat the literal baseline: {} vs {}",
            best.len(),
            baseline.len()
        );
        assert_best_round_trip(16, 16, &pixels);
    }

    #[test]
    fn best_predictor_shrinks_a_smooth_gradient() {
        // A smooth 16x16 planar gradient `v = (x + y) * 8`: the gradient predictor
        // (mode 12) predicts every interior pixel exactly, collapsing the residual
        // to zero, which the subtract-green / LZ77 Balanced floor cannot match.
        let pixels: Vec<u32> = (0..256u32)
            .map(|i| {
                let v = (i % 16 + i / 16) * 8;
                argb(255, v, v, v)
            })
            .collect();
        let best = encode_best(16, 16, &pixels);
        let balanced = encode(16, 16, &pixels);
        assert!(
            best.len() < balanced.len(),
            "predictor Best must shrink a smooth gradient below Balanced: {} vs {}",
            best.len(),
            balanced.len()
        );
        assert_best_round_trip(16, 16, &pixels);
    }

    #[test]
    fn best_cross_color_helps_a_correlated_image() {
        // Green ramps 0..126 (kept below 128 so the signed multiplier stays
        // positive), with red = green / 2 and blue = green / 4. Cross-color cancels
        // red and blue to zero (green_to_red = 16, green_to_blue = 8), leaving only
        // green to code — smaller than the subtract-green Balanced floor, which
        // instead spreads green's variation into red and blue.
        let pixels: Vec<u32> = (0..64u32)
            .map(|i| {
                let g = i * 2;
                argb(255, g >> 1, g, g >> 2)
            })
            .collect();
        let best = encode_best(8, 8, &pixels);
        let balanced = encode(8, 8, &pixels);
        assert!(
            best.len() < balanced.len(),
            "cross-color Best must beat Balanced on a correlated image: {} vs {}",
            best.len(),
            balanced.len()
        );
        assert_best_round_trip(8, 8, &pixels);
    }

    /// A 16x16 image whose top half is red-dominant and bottom half blue-dominant
    /// yields >=2 Huffman groups; `emit_stream_meta` round-trips it exactly.
    #[test]
    fn emit_meta_two_groups_round_trips() {
        let mut pixels = Vec::with_capacity(256);
        for y in 0..16u32 {
            for x in 0..16u32 {
                pixels.push(if y < 8 {
                    argb(255, (x * 16 + y) & 0xff, x & 7, 0)
                } else {
                    argb(255, 0, x & 7, (x * 16 + (y - 8)) & 0xff)
                });
            }
        }
        let tokens = crate::lossless::vp8l::backref::parse(&pixels, true);
        let plan =
            meta::plan(&tokens, &pixels, 16, 16, 0).expect("regional image should plan >=2 groups");
        assert!(
            plan.group_histograms.len() >= 2,
            "expected >=2 groups, got {}",
            plan.group_histograms.len()
        );
        let model = RefModel::new(&tokens, &pixels, 16);
        let bytes = emit_stream_meta(16, 16, alpha_used(&pixels), &[], 0, &model, &plan);
        let decoded = decode(&bytes).expect("meta stream must decode");
        assert_eq!((decoded.width, decoded.height), (16, 16));
        assert_eq!(decoded.argb.as_slice(), pixels.as_slice());
    }

    #[test]
    fn alpha_stream_round_trips_headerless() {
        // A headerless VP8L alpha stream carries the plane in the green lane and must
        // decode byte-for-byte through `decode_alpha_stream`, which drives the
        // image-stream decoder directly (no 5-byte header) and extracts the green
        // lane. Covers a flat run, a ramp, and a scattered pattern.
        use crate::lossless::vp8l::decode::decode_alpha_stream;
        let cases: [(u32, u32, Vec<u8>); 3] = [
            (4, 4, vec![0x80u8; 16]),
            (8, 2, (0..16u32).map(|v| (v * 15) as u8).collect()),
            (
                5,
                5,
                (0..25u32)
                    .map(|v| (v.wrapping_mul(37) ^ 0x5a) as u8)
                    .collect(),
            ),
        ];
        for (w, h, alpha) in cases {
            let stream = super::encode_alpha_stream(&alpha, w, h);
            let decoded = decode_alpha_stream(&stream, w, h).expect("alpha stream must decode");
            assert_eq!(decoded, alpha, "{w}x{h} alpha round-trip");
        }
    }

    #[test]
    fn encode_best_output_is_byte_stable() {
        // Golden FNV-1a-64 of `encode_best`'s exact output. The Effort::Best (Tier3)
        // path has NO other byte oracle — `xtask corpus-sweep` and
        // `encode_output_is_byte_stable` both re-encode with the default Balanced
        // method — so this pins the predictor/cross-color/palette forward transforms
        // and their (integer, deterministic) selection heuristics against silent
        // byte drift. The scattered case exercises the palette family, the gradient
        // the predictor family.
        const EXPECTED: [u64; 4] = [
            0xaa74_a8dc_ab4f_7a97,
            0x0e16_a604_b5f2_a267,
            0x2401_f6da_e437_c4b3,
            0xa5c5_1ec6_3637_64c7,
        ];
        let got: [u64; 4] =
            byte_stability_cases().map(|(w, h, pixels)| fnv1a64(&encode_best(w, h, &pixels)));
        assert_eq!(
            got, EXPECTED,
            "encode_best output bytes drifted from the committed golden"
        );
    }

    #[test]
    fn task_minima_fold_is_index_canonical() {
        // Folding each family to its own minimum (`keep_smallest`) and then across
        // families (`reduce_task_minima`) reproduces the flat fold exactly: ties
        // keep the earliest stream within a family and the earliest family across
        // them, a strictly shorter later candidate wins, and an empty family folds
        // to `None` and is skipped. The family shapes mirror `run_best_task` output.
        use super::{keep_smallest, reduce_task_minima};
        // Within-family tie -> earliest stream.
        assert_eq!(
            keep_smallest(vec![vec![3u8; 4], vec![4u8; 4]]),
            Some(vec![3u8; 4])
        );
        // An empty family (e.g. no palette on a > 256-color image) folds to None.
        assert_eq!(keep_smallest(Vec::new()), None);
        let families: Vec<Vec<Vec<u8>>> = vec![
            vec![vec![0u8; 5], vec![1u8; 5]], // family min = [0; 5] (tie -> first)
            Vec::new(),                       // empty family, skipped
            vec![vec![2u8; 5]],
            vec![vec![9u8; 3]], // strictly smaller -> global winner
        ];
        let minima: Vec<Option<Vec<u8>>> = families.into_iter().map(keep_smallest).collect();
        assert_eq!(minima[1], None);
        assert_eq!(reduce_task_minima(minima), vec![9u8; 3]);
        // Cross-family tie resolves to the earliest family's minimum.
        let ties: Vec<Vec<Vec<u8>>> = vec![vec![vec![7u8; 4]], vec![vec![8u8; 4]]];
        let ties_minima: Vec<Option<Vec<u8>>> = ties.into_iter().map(keep_smallest).collect();
        assert_eq!(reduce_task_minima(ties_minima), vec![7u8; 4]);
    }

    #[cfg(feature = "rayon")]
    #[test]
    fn serial_and_parallel_evaluation_agree() {
        use super::{
            build_best_tasks, evaluate_best_tasks, keep_smallest, reduce_task_minima, run_best_task,
        };
        for (w, h, pixels) in byte_stability_cases() {
            let alpha = pixels.iter().any(|&p| p >> 24 != 0xff);
            // Independent serial reference: fold each family's own minimum in task
            // order (the serial `evaluate_best_tasks` is not compiled under rayon).
            let serial = reduce_task_minima(
                build_best_tasks()
                    .into_iter()
                    .map(|t| keep_smallest(run_best_task(t, w, h, alpha, &pixels))),
            );
            // The rayon evaluator (feature on) must match the serial reference and
            // the public entry point, byte-for-byte.
            let parallel = evaluate_best_tasks(build_best_tasks(), w, h, alpha, &pixels);
            assert_eq!(
                serial, parallel,
                "rayon evaluation must equal serial, byte-for-byte"
            );
            assert_eq!(parallel, encode_best(w, h, &pixels));
        }
    }

    /// FNV-1a-64 of `bytes` — integer-only and deterministic, mirroring the
    /// `xtask corpus-sweep` byte oracle. Used to pin the encoder's exact output.
    fn fnv1a64(bytes: &[u8]) -> u64 {
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        for &b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x100_0000_01b3);
        }
        h
    }

    /// The four canonical byte-stability inputs, one per encoder path: a solid
    /// block (literal), a gradient (subtract-green), a scattered 12-color field
    /// (color cache), and a repeating 4-color tile (LZ77). Each is an input other
    /// tests already prove round-trips, so this only pins the exact bytes.
    fn byte_stability_cases() -> [(u32, u32, Vec<u32>); 4] {
        let solid = vec![argb(255, 10, 20, 30); 16];
        let gradient: Vec<u32> = (0..64u32).map(|v| argb(255, v * 2, v, 255 - v)).collect();
        let scattered: Vec<u32> = (0..1024u32)
            .map(|i| {
                let (x, y) = (i % 32, i / 32);
                let c = (x * 5 + y * 11) % 12;
                argb(255, c * 20 + 8, c * 13 + 17, c * 7 + 29)
            })
            .collect();
        let tile = [
            argb(255, 10, 20, 30),
            argb(255, 40, 50, 60),
            argb(255, 70, 80, 90),
            argb(255, 100, 110, 120),
        ];
        let repeating: Vec<u32> = tile.iter().cycle().take(256).copied().collect();
        [
            (4, 4, solid),
            (8, 8, gradient),
            (32, 32, scattered),
            (64, 4, repeating),
        ]
    }

    #[test]
    fn search_lz77_returns_the_literal_cache_winner() {
        // A scattered 16-color field (a PRNG over a fixed palette): color-cache-
        // coding every pixel as a literal — the colors are all frequent, so each
        // gets a short cache code — strictly beats both the greedy-LZ77 and
        // optimal-DP candidates, whose cache-unaware matches cost more than the
        // cache hits they displace. The winning stream is therefore exactly the
        // literal model's cache candidate, produced ONLY by the `if cache_literal >
        // 0` branch (both in `search_lz77` and `search_lz77_best`). Mutating that
        // guard to `==`/`<` stops the branch firing and the search falls back to the
        // strictly larger LZ77/DP candidate, breaking the exact-bytes assertions.
        use super::{search_lz77, search_lz77_best};
        use crate::lossless::vp8l::backref::{parse_lz77, parse_optimal};
        let mut state = 3u64 | 1;
        let pixels: Vec<u32> = (0..256usize)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let c = (state >> 40) as u32 % 16;
                argb(255, c * 20 + 8, c * 13 + 17, c * 7 + 29)
            })
            .collect();
        let (w, h) = (16u32, 16u32);
        let au = alpha_used(&pixels);
        let tokens_lit = parse(&pixels, false);
        let model_lit = RefModel::new(&tokens_lit, &pixels, w);
        let (bits, hist) = best_cache_bits(&model_lit);
        assert!(bits > 0, "the cost model must enable a cache here");
        let lit_cache = emit_stream(w, h, au, &[], bits, &model_lit, &hist);
        // The cache genuinely helps: the literal @cache candidate is smaller than
        // the literal @0 baseline the mutated guard would leave as `best`.
        let lit_zero = emit_stream(w, h, au, &[], 0, &model_lit, &model_lit.histogram(0));
        assert!(
            lit_cache.len() < lit_zero.len(),
            "the cache must shrink the stream"
        );
        // ...and both the greedy-LZ77 and optimal-DP candidates are strictly larger,
        // so the literal-cache candidate is the UNIQUE winner: no other branch of
        // the search reproduces its exact bytes.
        let (tokens_lz, chain) = parse_lz77(&pixels);
        let model_lz = RefModel::new(&tokens_lz, &pixels, w);
        let (b_lz, h_lz) = best_cache_bits(&model_lz);
        assert!(emit_stream(w, h, au, &[], b_lz, &model_lz, &h_lz).len() > lit_cache.len());
        let tokens_dp = parse_optimal(&pixels, w, &tokens_lz, &chain);
        let model_dp = RefModel::new(&tokens_dp, &pixels, w);
        let (b_dp, h_dp) = best_cache_bits(&model_dp);
        assert!(emit_stream(w, h, au, &[], b_dp, &model_dp, &h_dp).len() > lit_cache.len());
        // Both the Balanced (`search_lz77`) and Best (`search_lz77_best`) searches
        // must therefore return exactly the literal-cache candidate.
        assert_eq!(search_lz77(w, h, au, &[], w, &pixels), lit_cache);
        assert_eq!(search_lz77_best(w, h, au, &[], w, &pixels), lit_cache);
        assert_round_trip(w, h, &pixels);
    }

    #[test]
    fn search_lz77_best_meta_shot_shrinks_a_bimodal_image() {
        // A 64x32 image whose top half is a single flat color and whose bottom half
        // is a high-entropy multi-channel field: two region-specialized Huffman
        // groups beat one shared code by a wide margin, so the meta-Huffman
        // candidate is the strict winner of `search_lz77_best`. That shot is only
        // reached because `ysize = pixels.len() / working_width` recovers the true
        // height; mutating `/` (to `%` -> 0, or `*` -> a huge value) makes
        // `meta::plan` see a degenerate geometry and return `None`, dropping the
        // winning candidate so the result stops beating the meta-free `search_lz77`.
        use super::{search_lz77, search_lz77_best};
        let (w, h) = (64u32, 32u32);
        let mut pixels = Vec::with_capacity((w * h) as usize);
        for y in 0..h {
            for x in 0..w {
                pixels.push(if y < h / 2 {
                    argb(255, 10, 20, 30)
                } else {
                    argb(255, (x * 3) & 0xff, (x * 7 + y) & 0xff, (y * 5) & 0xff)
                });
            }
        }
        let au = alpha_used(&pixels);
        let with_meta = search_lz77_best(w, h, au, &[], w, &pixels);
        let no_meta = search_lz77(w, h, au, &[], w, &pixels);
        assert!(
            with_meta.len() < no_meta.len(),
            "meta-Huffman must shrink the bimodal image: {} vs {}",
            with_meta.len(),
            no_meta.len()
        );
        let decoded = decode(&with_meta).expect("meta-winning stream must decode");
        assert_eq!((decoded.width, decoded.height), (w, h));
        assert_eq!(decoded.argb.as_slice(), pixels.as_slice());
    }

    #[test]
    fn emit_meta_varying_alpha_round_trips() {
        // Like `emit_meta_two_groups_round_trips`, but every pixel carries a
        // different alpha value, so the alpha channel is genuinely multi-symbol
        // within each group. That makes the meta-emit's alpha extraction
        // `((argb >> 24) & 0xff)` load-bearing: corrupting the shift (`>>`->`<<`
        // forces 0) or the mask (`&`->`|` forces 0xff, `&`->`^` flips the byte)
        // emits the wrong alpha symbol and the round-trip breaks.
        let mut pixels = Vec::with_capacity(256);
        for y in 0..16u32 {
            for x in 0..16u32 {
                let a = 1 + ((x * 5 + y * 3) & 0x3f);
                pixels.push(if y < 8 {
                    argb(a, (x * 16 + y) & 0xff, x & 7, 0)
                } else {
                    argb(a, 0, x & 7, (x * 16 + (y - 8)) & 0xff)
                });
            }
        }
        // The alpha channel must actually vary, else the mutation would be invisible.
        assert!(pixels.iter().any(|&p| p >> 24 != pixels[0] >> 24));
        let tokens = crate::lossless::vp8l::backref::parse(&pixels, true);
        let plan =
            meta::plan(&tokens, &pixels, 16, 16, 0).expect("regional image should plan >=2 groups");
        assert!(plan.group_histograms.len() >= 2);
        let model = RefModel::new(&tokens, &pixels, 16);
        let bytes = emit_stream_meta(16, 16, alpha_used(&pixels), &[], 0, &model, &plan);
        let decoded = decode(&bytes).expect("meta stream must decode");
        assert_eq!((decoded.width, decoded.height), (16, 16));
        assert_eq!(decoded.argb.as_slice(), pixels.as_slice());
    }

    #[test]
    fn emit_meta_cache_reference_uses_the_correct_symbol() {
        // The color-cache branch of `emit_tokens_meta` codes a cache reference as the
        // green symbol `NUM_LITERAL_CODES + NUM_LENGTH_CODES + key` (= 280 + key).
        // Flipping the first `+` to `-` emits `NUM_LITERAL_CODES - NUM_LENGTH_CODES +
        // key` (= 232 + key) — a green LITERAL symbol — so the decoder reads a
        // different unit and the stream stops round-tripping. Random images seldom
        // exercise the meta cache path, so pin it deterministically: a 16x16 image
        // whose top and bottom halves use DISJOINT palettes (>=2 Huffman groups), with
        // every color duplicated in place so an all-literal parse under an 8-bit color
        // cache scores a guaranteed hit on the second pixel of every pair. Encoded via
        // the meta path with that cache, an exact round-trip catches the wrong cache
        // symbol on every run, at any case count.
        use crate::lossless::vp8l::backref::{Resolved, resolve};
        let top = [
            argb(255, 10, 20, 30),
            argb(255, 200, 40, 60),
            argb(255, 70, 220, 90),
            argb(255, 100, 110, 240),
        ];
        let bottom = [
            argb(255, 33, 66, 99),
            argb(255, 240, 240, 10),
            argb(255, 15, 250, 250),
            argb(255, 250, 15, 130),
        ];
        let mut pixels = Vec::with_capacity(256);
        for y in 0..16u32 {
            for x in 0..16u32 {
                let palette = if y < 8 { &top } else { &bottom };
                // Duplicated pairs (0,0,1,1,2,2,3,3,...): every odd x repeats its
                // predecessor, so an all-literal parse makes it a cache hit.
                pixels.push(palette[((x / 2) % 4) as usize]);
            }
        }
        let cache_bits = 8u32;
        // All-literal parse: adjacent duplicates become cache hits (an LZ77 parse would
        // fold them into a copy), so the cache branch of the meta emit is exercised.
        let tokens = parse(&pixels, false);
        // The cache branch must genuinely fire, else the mutation would be invisible.
        let mut cache_hit_count = 0usize;
        resolve(&tokens, &pixels, cache_bits, 16, |_pos, unit| {
            if matches!(unit, Resolved::Cache(_)) {
                cache_hit_count += 1;
            }
        });
        assert!(
            cache_hit_count > 0,
            "the meta cache branch must be exercised"
        );
        let plan = meta::plan(&tokens, &pixels, 16, 16, cache_bits)
            .expect("two disjoint-palette halves must plan >=2 groups");
        assert!(
            plan.group_histograms.len() >= 2,
            "expected >=2 groups, got {}",
            plan.group_histograms.len()
        );
        let model = RefModel::new(&tokens, &pixels, 16);
        let bytes = emit_stream_meta(16, 16, alpha_used(&pixels), &[], cache_bits, &model, &plan);
        let decoded = decode(&bytes).expect("meta cache stream must decode");
        assert_eq!((decoded.width, decoded.height), (16, 16));
        assert_eq!(decoded.argb.as_slice(), pixels.as_slice());
    }

    #[test]
    fn cross_color_streams_are_nonempty_and_round_trip() {
        // The cross-color family must yield its two ordered candidates (cross-color
        // alone, and cross-color + subtract-green), each a valid stream that decodes
        // back to the source. A function-replacement mutant returning `vec![]`
        // produces no candidates — caught by the length assert; the decodes pin that
        // both streams are correct.
        use super::cross_color_streams;
        let pixels: Vec<u32> = (0..64u32)
            .map(|i| {
                let g = i * 2;
                argb(255, g >> 1, g, g >> 2)
            })
            .collect();
        let streams = cross_color_streams(8, 8, alpha_used(&pixels), &pixels, 3);
        assert_eq!(
            streams.len(),
            2,
            "cross-color family must emit two candidates"
        );
        for stream in &streams {
            let decoded = decode(stream).expect("each cross-color stream must decode");
            assert_eq!((decoded.width, decoded.height), (8, 8));
            assert_eq!(decoded.argb.as_slice(), pixels.as_slice());
        }
    }

    #[test]
    fn cross_color_transform_tile_bits_field_round_trips() {
        // A level-0 stream carrying a cross-color transform at tile bits = 2 over a
        // 32-wide image, so the tile grid is `subsample_size(32, 2) = 8` wide. The
        // field is written as `bits - MIN_TRANSFORM_BITS` (= 0) and the decoder
        // reads back `read_bits(3) + 2`. Corrupting the subtraction changes the
        // decoded tile bits and thus the grid width the decoder reads the sub-image
        // at (`-`->`+` writes 4 -> reads bits 6 -> width `subsample_size(32,6)=1`;
        // `-`->`/` writes 1 -> reads bits 3 -> width `subsample_size(32,3)=4`), so
        // the tile sub-image is misparsed and the round-trip fails.
        use crate::lossless::transform::cross_color;
        let width = 32u32;
        let height = 8u32;
        // Left half is red ~= green/2, right half is red ~= green: cross-color picks
        // a DIFFERENT per-tile multiplier for the two regions, so the tile sub-image
        // carries real (multi-symbol) pixel bits and its grid width is load-bearing.
        let source: Vec<u32> = (0..(width * height))
            .map(|i| {
                let x = i % width;
                let g = (i * 2) & 0x7f;
                if x < 16 {
                    argb(255, g >> 1, g, g >> 2)
                } else {
                    argb(255, g, g, 0)
                }
            })
            .collect();
        let bits = 2u32;
        let (stored, tile_data) = cross_color::forward(&source, width, height, bits);
        assert!(
            tile_data
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                > 1,
            "the tile grid must carry >1 distinct multiplier for the field to matter"
        );
        let tokens = parse(&stored, false);
        let model = RefModel::new(&tokens, &stored, width);
        let plans = [TransformPlan::CrossColor { bits, tile_data }];
        let stream = emit_stream(
            width,
            height,
            alpha_used(&source),
            &plans,
            0,
            &model,
            &model.histogram(0),
        );
        let decoded = decode(&stream).expect("a cross-color sub-image stream must decode");
        assert_eq!((decoded.width, decoded.height), (width, height));
        assert_eq!(decoded.argb, source);
    }

    #[test]
    fn encode_output_is_byte_stable() {
        // Golden FNV-1a-64 of `encode`'s exact output bytes, captured from the
        // pre-perf-refactor build. The RefModel / lazy-carry changes are designed
        // to be byte-invariant, so these must never move; a mismatch here
        // localizes an accidental output change faster than the full corpus sweep.
        const EXPECTED: [u64; 4] = [
            0xaa74_a8dc_ab4f_7a97,
            0xa071_c300_9622_384f,
            0x8055_bafc_bbc8_6673,
            0x2ac6_6ad5_3623_697f,
        ];
        for ((w, h, pixels), &expected) in byte_stability_cases().into_iter().zip(&EXPECTED) {
            assert_eq!(
                fnv1a64(&encode(w, h, &pixels)),
                expected,
                "encode({w}x{h}) output bytes drifted from the committed golden"
            );
        }
    }
}

#[cfg(test)]
mod proptests {
    use super::{RefModel, best_cache_bits, emit_stream_meta, encode, encode_best, encode_with};
    use crate::lossless::constants::{ALPHABET_SIZE, MAX_CACHE_BITS};
    use crate::lossless::histogram::Histogram;
    use crate::lossless::vp8l::backref::{Resolved, Token, parse, resolve};
    use crate::lossless::vp8l::decode::decode;
    use crate::lossless::vp8l::meta;
    use proptest::prelude::*;

    /// `alpha_used` as the encoder computes it (the header advisory bit).
    fn alpha_used(pixels: &[u32]) -> bool {
        pixels.iter().any(|&p| p >> 24 != 0xff)
    }

    /// A straightforward per-cache-size histogram builder: it walks every unit
    /// through `resolve`, recomputing each copy's prefix symbols. The production
    /// [`RefModel::histogram`] hoists that cache-independent work but must yield a
    /// bit-identical histogram; `ref_model_matches_accumulate` pins the identity.
    fn reference_accumulate(
        tokens: &[Token],
        pixels: &[u32],
        cache_bits: u32,
        width: u32,
    ) -> Histogram {
        let cache_codes = if cache_bits > 0 {
            1usize << cache_bits
        } else {
            0
        };
        let mut histogram = Histogram::new(ALPHABET_SIZE[0] + cache_codes);
        resolve(tokens, pixels, cache_bits, width, |_pos, unit| match unit {
            Resolved::Literal(argb) => histogram.add_literal(argb),
            Resolved::Copy {
                length_symbol,
                length_extra,
                dist_symbol,
                dist_extra,
            } => {
                histogram.add_length(length_symbol, length_extra.1);
                histogram.add_distance(dist_symbol, dist_extra.1);
            },
            Resolved::Cache(key) => histogram.add_cache(key),
        });
        histogram
    }

    /// Deterministic repetition-prone pixels from a seed (small palette keeps the
    /// LZ77 and color-cache paths well exercised).
    fn seeded_pixels(seed: u64, count: usize, palette: u32) -> Vec<u32> {
        let mut state = seed | 1;
        (0..count)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                (state >> 40) as u32 % palette
            })
            .collect()
    }

    /// One structure-bearing image per `variant`, so the Best proptest exercises
    /// the palette, predictor, and cross-color emits rather than random noise
    /// (where the floor always wins and no transform is emitted):
    /// `0` a scattered small palette, `1` a smooth grayscale gradient, `2` a
    /// green<->red/blue correlated ramp.
    fn structured_pixels(variant: u8, seed: u64, width: u32, height: u32) -> Vec<u32> {
        let count = (width * height) as usize;
        match variant % 3 {
            0 => {
                let palette = [
                    0xff10_2030u32,
                    0xff20_4030,
                    0xff70_a0c0,
                    0xffa0_3060,
                    0xff30_c090,
                    0xfff0_f010,
                ];
                seeded_pixels(seed, count, palette.len() as u32)
                    .into_iter()
                    .map(|idx| palette[idx as usize])
                    .collect()
            },
            1 => (0..count as u32)
                .map(|i| {
                    let v = ((i % width + i / width) * 4) & 0xff;
                    0xff00_0000 | (v << 16) | (v << 8) | v
                })
                .collect(),
            _ => (0..count as u32)
                .map(|i| {
                    // Keep green < 128 so the signed cross-color multiplier stays
                    // positive and the red = g/2, blue = g/4 correlation cancels.
                    let g = (i * 2) & 0x7f;
                    0xff00_0000 | ((g >> 1) << 16) | (g << 8) | (g >> 2)
                })
                .collect(),
        }
    }

    proptest! {
        /// Any small image survives a full encode -> decode round-trip
        /// unchanged; strategy, cache-size, and tier selection never break the
        /// identity. A small palette keeps LZ77 and cache paths well exercised.
        #[test]
        fn encode_decode_round_trip(
            width in 1u32..=16,
            height in 1u32..=16,
            seed in any::<u64>(),
            palette in 1u32..=8,
        ) {
            let count = (width * height) as usize;
            let pixels = seeded_pixels(seed, count, palette);

            let payload = encode(width, height, &pixels);
            let decoded = decode(&payload).expect("encoder must emit decodable VP8L");
            prop_assert_eq!(decoded.width, width);
            prop_assert_eq!(decoded.height, height);
            prop_assert_eq!(&decoded.argb, &pixels);

            // The `Fast` method (`use_lz77` cleared) skips the LZ77 / color-cache
            // search and emits only the literal + subtract-green tiers; it must
            // still be exactly lossless.
            let fast = encode_with(width, height, &pixels, false);
            let decoded_fast = decode(&fast).expect("Fast encoder must emit decodable VP8L");
            prop_assert_eq!(decoded_fast.width, width);
            prop_assert_eq!(decoded_fast.height, height);
            prop_assert_eq!(decoded_fast.argb, pixels);
        }

        /// The Tier 3 `Best` driver round-trips exactly over palette, gradient,
        /// and correlated images — the transform families it emits (color
        /// indexing, predictor, cross-color, each possibly with subtract-green)
        /// are all inverted losslessly by the decoder.
        #[test]
        fn encode_best_round_trip(
            width in 1u32..=16,
            height in 1u32..=16,
            seed in any::<u64>(),
            variant in 0u8..3,
        ) {
            let pixels = structured_pixels(variant, seed, width, height);
            let payload = encode_best(width, height, &pixels);
            let decoded = decode(&payload).expect("encode_best must emit decodable VP8L");
            prop_assert_eq!(decoded.width, width);
            prop_assert_eq!(decoded.height, height);
            prop_assert_eq!(decoded.argb, pixels);
        }

        /// encode_best is deterministic across repeated calls — a tautology
        /// serially, but under the rayon CI job it hammers the work-stealer.
        #[test]
        fn encode_best_is_repeatable(width in 1u32..=16, height in 1u32..=16, seed in any::<u64>(), variant in 0u8..3) {
            let pixels = structured_pixels(variant, seed, width, height);
            let a = encode_best(width, height, &pixels);
            for _ in 0..3 {
                prop_assert_eq!(&encode_best(width, height, &pixels), &a);
            }
        }

        /// Whenever the planner finds a grouping, the multi-group emit decodes back
        /// to the source — the meta emit is correct for every plan that arises.
        #[test]
        fn meta_emit_decodes_to_source(
            width in 2u32..=16, height in 2u32..=16, seed in any::<u64>(), variant in 0u8..3,
        ) {
            let pixels = structured_pixels(variant, seed, width, height);
            let tokens = crate::lossless::vp8l::backref::parse(&pixels, true);
            if let Some(plan) = meta::plan(&tokens, &pixels, width, height, 0) {
                let model = RefModel::new(&tokens, &pixels, width);
                let bytes = emit_stream_meta(width, height, alpha_used(&pixels), &[], 0, &model, &plan);
                let decoded = decode(&bytes).expect("meta emit must decode");
                prop_assert_eq!(decoded.width, width);
                prop_assert_eq!(decoded.height, height);
                prop_assert_eq!(decoded.argb, pixels);
            }
        }

        /// `RefModel::histogram` (hoisted cache-independent bins) is bit-identical
        /// to a `resolve`-driven `accumulate` at every cache size, for both parse
        /// strategies — the mechanical proof the hoist is behavior-neutral, so the
        /// histogram (hence the emitted bytes) cannot move.
        #[test]
        fn ref_model_matches_accumulate(
            width in 1u32..=16,
            seed in any::<u64>(),
            len in 1usize..=200,
            palette in 1u32..=8,
            use_lz77 in any::<bool>(),
        ) {
            let pixels = seeded_pixels(seed, len, palette);
            let tokens = parse(&pixels, use_lz77);
            let model = RefModel::new(&tokens, &pixels, width);
            for cache_bits in 0..=MAX_CACHE_BITS {
                prop_assert_eq!(
                    model.histogram(cache_bits),
                    reference_accumulate(&tokens, &pixels, cache_bits, width),
                    "histogram mismatch at cache_bits={}",
                    cache_bits
                );
            }
        }

        /// The buffer-reusing `best_cache_bits` chooses exactly the size a fresh
        /// per-size histogram sweep would. The reused scratch is a max-size
        /// zero-padded histogram, and `estimate_bits` skips zero-count bins, so the
        /// estimate (hence the selected cache size, hence the emitted bytes) is
        /// unchanged by the allocation-eliding refactor.
        #[test]
        fn best_cache_bits_reuse_matches_fresh(
            width in 1u32..=16,
            seed in any::<u64>(),
            len in 1usize..=200,
            palette in 1u32..=8,
            use_lz77 in any::<bool>(),
        ) {
            let pixels = seeded_pixels(seed, len, palette);
            let tokens = parse(&pixels, use_lz77);
            let model = RefModel::new(&tokens, &pixels, width);
            // Fresh per-size sweep: a freshly allocated histogram each iteration.
            let mut fresh_bits = 0;
            let mut best = model.histogram(0).estimate_bits();
            for bits in 1..=MAX_CACHE_BITS {
                let cost = model.histogram(bits).estimate_bits();
                if cost < best {
                    best = cost;
                    fresh_bits = bits;
                }
            }
            prop_assert_eq!(best_cache_bits(&model).0, fresh_bits);
        }

        /// The histogram `best_cache_bits` hands back (a truncated snapshot of the
        /// reused max-size scratch at the winning size) is bin-identical to a fresh
        /// `RefModel::histogram` at that same size — so [`emit_coded_pixels`]
        /// reusing it instead of rebuilding cannot move a single emitted byte.
        #[test]
        fn best_cache_bits_histogram_matches_fresh(
            width in 1u32..=16,
            seed in any::<u64>(),
            len in 1usize..=200,
            palette in 1u32..=8,
            use_lz77 in any::<bool>(),
        ) {
            let pixels = seeded_pixels(seed, len, palette);
            let tokens = parse(&pixels, use_lz77);
            let model = RefModel::new(&tokens, &pixels, width);
            let (bits, hist) = best_cache_bits(&model);
            prop_assert_eq!(hist, model.histogram(bits));
        }
    }
}
