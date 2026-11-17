//! Deterministic algorithmic-work counters: a fixed array of process-global
//! integer atomics, one slot per instrumented codec hot path.
//!
//! Counts are a pure function of codec input (they tally control-flow events,
//! never wall-clock time), so a [`snapshot`] is a byte-reproducible integer
//! ledger across platforms, toolchains, AND optimization levels — unlike timing
//! or peak memory. This module is the third, "work-cost" measurement plane
//! documented in `docs/benchmarking.md`.
//!
//! It is compiled only behind the crate's optional `work-count` feature and is
//! absent from every default/production build. Because the counters are
//! process-global statics, the driver (xtask `work`) MUST measure one codec
//! operation at a time: reset, run, snapshot. Totals are order-independent
//! (`fetch_add`), so an in-encode `rayon` region is fine as long as the snapshot
//! is read after it joins.

use core::sync::atomic::{AtomicU64, Ordering};

/// One instrumented hot path.
///
/// The discriminant order is load-bearing: it fixes both the index into the
/// backing `SLOTS` array and each snapshot value's mapping to the stable
/// [`field_names`] keys written to `corpus/work.json`. Only ever APPEND new
/// variants — never reorder or remove, or the ledger schema shifts under the
/// drift gate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(usize)]
pub enum Counter {
    // ---- lossless codec ----
    /// `RefModel::histogram` full-image token walk.
    HistogramPass,
    /// `backref::resolve` token walk (weighted by tokens walked).
    ResolveWalk,
    /// A single histogram build inside `best_cache_bits`' cache-size sweep.
    CacheBitsBuild,
    /// A hash-chain hop in the LZ77 match finder.
    MatchHop,
    /// A byte compared while extending an LZ77 match (weighted by bytes).
    MatchCompare,
    /// A node relaxation in the optimal-parse dynamic program.
    OptimalDpStep,
    /// A pairwise-delta comparison in the meta-Huffman greedy clusterer.
    ClusterScan,
    /// A merged-histogram cost estimate in the clusterer.
    ClusterEstimate,
    /// One predictor (mode x tile) residual pass in the forward transform.
    PredictorModeEval,
    /// One cross-color candidate-multiplier evaluation in the forward transform.
    CrossColorEval,
    /// A `ColorCache` allocation inside a histogram/resolve hot loop.
    ColorCacheAlloc,
    /// A `Histogram` allocation inside a histogram hot loop.
    HistogramAlloc,
    // ---- lossy codec ----
    /// A `fdct4x4` forward-transform call.
    FdctCall,
    /// A `quantize_one` (round-to-nearest or trellis) quantization call.
    QuantizeCall,
    /// A candidate evaluation in the trellis quantizer (weighted by candidates).
    TrellisEval,
    /// A `block_token_cost` token walk in a rate estimate.
    TokenCostWalk,
    /// A `sse_block` sum-of-squared-error block comparison.
    SseBlock,
    /// A luma intra-prediction fill (`predict_luma16` / `predict_luma4`).
    PredictLuma,
    /// A chroma intra-prediction fill (`predict_chroma8`).
    PredictChroma,
    /// A source-luma fdct done only to derive the segmentation complexity metric.
    MbComplexityFdct,
    /// A nearest-centroid comparison in the segmentation k-means.
    KmeansCompare,
    /// A node iteration in the coefficient-probability optimizer.
    ProbaOptNode,
    /// A whole-frame token-partition walk during emission.
    TokenPartitionWalk,
    // ---- lossy codec (DECODE) ----
    // These name the operation, not the phase; which pass populates them is
    // fixed by the driver's reset→run→snapshot discipline (the encoder resets
    // before an encode, the decoder before a decode). Decode-only primitives
    // (`bool_read`, `bool_renorm`, `coeff_token`, `upsample_row`) stay zero in
    // encode rows; the two shared kernels (`idct_call`, `loop_filter_edge`) also
    // fire during the encoder's own reconstruction, so they enrich encode rows
    // too. Intra prediction is already covered by `PredictLuma`/`PredictChroma`.
    /// A `bool_dec::read_bool` boolean decode — the hottest decode primitive.
    BoolRead,
    /// One renormalization shift in `read_bool` (weighted by shift iterations).
    BoolRenorm,
    /// A `token::get_coeffs` 4×4 coefficient-block token walk.
    CoeffToken,
    /// An `idct::transform_one` / `transform_wht` inverse-transform call
    /// (shared: also the encoder's reconstruction).
    IdctCall,
    /// A `loop_filter` per-edge kernel application (`do_filter2/4/6`)
    /// (shared: also the encoder's frame-final deblock).
    LoopFilterEdge,
    /// A `yuv::upsample_one_row` fancy-upsample row (decode color conversion).
    UpsampleRow,
    // ---- lossless codec (DECODE) ----
    // Append-only despite the semantic grouping: these follow the lossy decode
    // slots because their discriminants were added later. Moving them beside the
    // older lossless slots would silently remap every committed ledger value.
    /// One byte-window shift while refilling the VP8L LSB-first bit reader.
    Vp8lBitRefill,
    /// One VP8L Huffman-table symbol lookup (header, nested, or pixel stream).
    Vp8lHuffmanSymbol,
    /// A VP8L Huffman lookup that descends from the root into a second-level table.
    Vp8lHuffmanSecondLevel,
    /// A meta-Huffman entropy-image group selection for one coded unit.
    Vp8lEntropySelect,
    /// One literal pixel committed by the VP8L pixel loop.
    Vp8lLiteralPixel,
    /// One VP8L LZ77 back-reference unit committed.
    Vp8lBackrefUnit,
    /// One pixel copied by a VP8L LZ77 back-reference (weighted by copy length).
    Vp8lBackrefPixel,
    /// One VP8L color-cache lookup.
    Vp8lCacheLookup,
    /// One VP8L color-cache insertion (weighted by inserted pixels).
    Vp8lCacheInsert,
    /// One pixel reconstructed by the VP8L predictor inverse.
    Vp8lPredictorPixel,
    /// One pixel processed by the VP8L cross-color inverse.
    Vp8lCrossColorPixel,
    /// One pixel processed by the VP8L subtract-green inverse.
    Vp8lSubtractGreenPixel,
    /// One output pixel expanded by the VP8L palette inverse.
    Vp8lPalettePixel,
    /// One decoded VP8L ARGB pixel packed into the requested byte layout.
    Vp8lPackPixel,
}

/// Number of counter slots — equal to the ledger's integer-field count.
pub const N: usize = Counter::Vp8lPackPixel as usize + 1;

// A const-initializable seed for the fixed atomic array. `AtomicU64` has
// interior mutability, so the const-item lint fires; it is used only to
// build the `static` array, never read through, so the warning is spurious.
#[expect(
    clippy::declare_interior_mutable_const,
    reason = "array-initializer seed for the static SLOTS array; never read through this const"
)]
const ZERO: AtomicU64 = AtomicU64::new(0);

/// The process-global counter slots, indexed by [`Counter`] discriminant.
static SLOTS: [AtomicU64; N] = [ZERO; N];

impl Counter {
    #[inline]
    fn bump_in(self, slots: &[AtomicU64; N]) {
        slots[self as usize].fetch_add(1, Ordering::Relaxed);
    }

    #[inline]
    fn add_in(self, slots: &[AtomicU64; N], n: u64) {
        slots[self as usize].fetch_add(n, Ordering::Relaxed);
    }

    /// Add 1 to this counter's slot.
    #[inline]
    pub fn bump(self) {
        self.bump_in(&SLOTS);
    }

    /// Add `n` to this counter's slot (for cost-weighted events such as an
    /// inner loop's trip count).
    #[inline]
    pub fn add(self, n: u64) {
        self.add_in(&SLOTS, n);
    }
}

fn reset_slots(slots: &[AtomicU64; N]) {
    for slot in slots {
        slot.store(0, Ordering::Relaxed);
    }
}

fn snapshot_slots(slots: &[AtomicU64; N]) -> [u64; N] {
    let mut out = [0u64; N];
    for (dst, slot) in out.iter_mut().zip(slots.iter()) {
        *dst = slot.load(Ordering::Relaxed);
    }
    out
}

/// Zero every counter slot; call immediately before a measured codec operation.
pub fn reset() {
    reset_slots(&SLOTS);
}

/// Read every counter slot in [`Counter`] discriminant order.
///
/// Call after the measured operation (and after any in-encode parallel region has joined).
/// The returned array aligns element-for-element with [`field_names`].
#[must_use]
pub fn snapshot() -> [u64; N] {
    snapshot_slots(&SLOTS)
}

/// The stable, lowercase field name of each slot in [`Counter`] order.
///
/// These drive the `corpus/work.json` JSON keys. Kept adjacent to the enum so
/// that appending a `Counter` variant without a name here fails to compile
/// (the array length must equal [`N`]).
#[must_use]
pub const fn field_names() -> [&'static str; N] {
    [
        // lossless codec
        "histogram_pass",
        "resolve_walk",
        "cache_bits_build",
        "match_hop",
        "match_compare",
        "optimal_dp_step",
        "cluster_scan",
        "cluster_estimate",
        "predictor_mode_eval",
        "cross_color_eval",
        "color_cache_alloc",
        "histogram_alloc",
        // lossy codec
        "fdct_call",
        "quantize_call",
        "trellis_eval",
        "token_cost_walk",
        "sse_block",
        "predict_luma",
        "predict_chroma",
        "mb_complexity_fdct",
        "kmeans_compare",
        "proba_opt_node",
        "token_partition_walk",
        // lossy codec (DECODE)
        "bool_read",
        "bool_renorm",
        "coeff_token",
        "idct_call",
        "loop_filter_edge",
        "upsample_row",
        // lossless codec (DECODE)
        "vp8l_bit_refill",
        "vp8l_huffman_symbol",
        "vp8l_huffman_second_level",
        "vp8l_entropy_select",
        "vp8l_literal_pixel",
        "vp8l_backref_unit",
        "vp8l_backref_pixel",
        "vp8l_cache_lookup",
        "vp8l_cache_insert",
        "vp8l_predictor_pixel",
        "vp8l_cross_color_pixel",
        "vp8l_subtract_green_pixel",
        "vp8l_palette_pixel",
        "vp8l_pack_pixel",
    ]
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::AtomicU64;

    use super::{Counter, N, field_names, reset_slots, snapshot_slots};

    #[test]
    fn field_names_align_with_slot_count() {
        assert_eq!(N, 43);
        assert_eq!(field_names().len(), N);
    }

    #[test]
    fn vp8l_decode_field_names_align_with_discriminants() {
        for (counter, name) in [
            (Counter::Vp8lBitRefill, "vp8l_bit_refill"),
            (Counter::Vp8lHuffmanSymbol, "vp8l_huffman_symbol"),
            (Counter::Vp8lHuffmanSecondLevel, "vp8l_huffman_second_level"),
            (Counter::Vp8lEntropySelect, "vp8l_entropy_select"),
            (Counter::Vp8lLiteralPixel, "vp8l_literal_pixel"),
            (Counter::Vp8lBackrefUnit, "vp8l_backref_unit"),
            (Counter::Vp8lBackrefPixel, "vp8l_backref_pixel"),
            (Counter::Vp8lCacheLookup, "vp8l_cache_lookup"),
            (Counter::Vp8lCacheInsert, "vp8l_cache_insert"),
            (Counter::Vp8lPredictorPixel, "vp8l_predictor_pixel"),
            (Counter::Vp8lCrossColorPixel, "vp8l_cross_color_pixel"),
            (Counter::Vp8lSubtractGreenPixel, "vp8l_subtract_green_pixel"),
            (Counter::Vp8lPalettePixel, "vp8l_palette_pixel"),
            (Counter::Vp8lPackPixel, "vp8l_pack_pixel"),
        ] {
            assert_eq!(field_names()[counter as usize], name);
        }
    }

    // Exercise the counter primitives on local slots.
    // Other all-feature tests legitimately use the process-global work ledger in parallel.
    #[test]
    fn bump_add_and_reset_hit_the_right_slots() {
        let slots = core::array::from_fn(|_| AtomicU64::new(0));
        reset_slots(&slots);
        Counter::HistogramPass.bump_in(&slots);
        Counter::HistogramPass.bump_in(&slots);
        Counter::MatchCompare.add_in(&slots, 40);
        let snap = snapshot_slots(&slots);
        assert_eq!(snap[Counter::HistogramPass as usize], 2);
        assert_eq!(snap[Counter::MatchCompare as usize], 40);
        assert_eq!(snap[Counter::TokenPartitionWalk as usize], 0);

        reset_slots(&slots);
        assert_eq!(snapshot_slots(&slots), [0u64; N]);
    }
}
