//! Trellis (rate-distortion) coefficient quantization — the Balanced/Best size
//! lever (layer 2: it reads the token-tree probabilities and the entropy cost model).
//!
//! Round-to-nearest quantization ([`crate::lossy::quant::quantize_block`]) picks each
//! coefficient's level independently to minimize its own reconstruction error,
//! ignoring how many bits the token tree will spend coding it. [`trellis_quantize_block`]
//! instead chooses the whole block's levels by a Viterbi search that minimizes
//! `256 * distortion + lambda * rate`, where `distortion` is the squared
//! reconstruction error the decoder will see and `rate` is the exact number of bits
//! [`crate::lossy::tokens::put_coeffs`] would emit (in 1/256-bit units). Coarser quantizers
//! raise `lambda` (see [`trellis_lambda`]), so bits weigh more and small, expensive
//! coefficients are dropped — the biggest single size lever on AC-rich content.
//!
//! # Self-consistency is automatic
//!
//! The result is the same [`Quantized`] shape as round-to-nearest: `recon[j] =
//! level * q`, exactly what the decoder recovers (`token.rs`: `apply_sign(level) *
//! dq`). Any valid level assignment is therefore self-consistent by construction,
//! so the trellis can only change *which* levels (the size/quality trade-off), never
//! the decode identity. The only hard requirements are levels in `0..=MAX_LEVEL`, a
//! correct `last`, and determinism.
//!
//! # The token cost model
//!
//! The VP8 token tree codes, per coded position, a node-0 "more coefficients" bit
//! (only at the top of each run, i.e. when the incoming context is non-zero), a
//! node-1 zero/non-zero bit, and for a non-zero its node-2 one-vs-large decision,
//! large-value tree and sign. The Viterbi state is the outgoing context (`0` after a
//! zero, `1` after `|level| == 1`, `2` after `|level| >= 2`) carried to the next
//! position; the per-position `base` charges node 0 exactly when the incoming
//! context is non-zero, mirroring `put_coeffs`. Trailing zeros after the last
//! non-zero are covered by the terminal EOB (node-0 `false`), so their distortion is
//! added via [`suffix_dist`](self) without any rate.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    reason = "quantized levels (0..=2047) and reconstructed coefficients are stored \
              into i16 with the reference decoder's int16_t wrapping semantics, and \
              the 0..=15 zig-zag positions cast to i32; every value is in range and \
              the casts reproduce the decoder's arithmetic exactly"
)]

use crate::lossy::constants::{BANDS, CAT_3456, CoeffProbas, NUM_PROBAS, Prob, ZIGZAG};
use crate::lossy::prob_opt::bit_cost;
use crate::lossy::quant::{QPair, Quantized};
use crate::lossy::work::work;

/// The largest coefficient magnitude VP8's token tree can code.
const MAX_LEVEL: i32 = 2047;
/// Distortion weight in the rate-distortion score, matching libwebp's
/// `RD_DISTO_MULT`: the squared-error distortion is scaled by 256 so it shares the
/// 1/256-bit unit scale of `rate` before `lambda` weights the two terms. Shared
/// with the whole-block mode decision ([`crate::lossy::frame`]) so trellis, the i16/i4
/// luma decision and the chroma decision all score in the same units.
pub(crate) const RD_DISTO_MULT: i64 = 256;
/// A cost no real path reaches — the Viterbi "unreachable state" sentinel.
const INF: i64 = i64::MAX / 4;

/// Whether the trellis weights its coefficient-domain squared error by the
/// per-frequency [`K_WEIGHT_TRELLIS`] table (libwebp's `kWeightTrellis`) and pairs it
/// with the matching `>> 3` lambda base. When `true` the distortion is
/// `kWeightTrellis[j] * err * err` (low frequencies count for more) scored against the
/// rate with libwebp's `>> 3` lambda; when `false` it is the plain `err * err` with a
/// `>> 6` lambda that stands in for the missing weight table.
const TRELLIS_FREQ_WEIGHTING: bool = true;

/// Right shift turning `7 * q_ac^2` into the trellis `lambda` (libwebp's
/// `lambda_trellis` base is `(7 * q^2) >> 3`). With [`TRELLIS_FREQ_WEIGHTING`] on the
/// per-frequency [`K_WEIGHT_TRELLIS`] weight carries the distortion shaping and the
/// shift is libwebp's `>> 3`; with it off an *unweighted* squared error is balanced
/// against the rate by a larger `>> 6` shift instead (a shift of 5 already costs >1 dB
/// on smooth photo content).
const LAMBDA_SHIFT: u32 = if TRELLIS_FREQ_WEIGHTING { 3 } else { 6 };

/// Per-frequency trellis distortion weights (libwebp `kWeightTrellis`, the `USE_TDISTO`
/// table) indexed by natural (row-major) coefficient position: low frequencies weigh
/// more, so dropping low-frequency energy costs more distortion. A committed constant,
/// consumed only when [`TRELLIS_FREQ_WEIGHTING`] is on.
const K_WEIGHT_TRELLIS: [i64; 16] = [
    30, 27, 19, 11, //
    27, 24, 17, 10, //
    19, 17, 12, 8, //
    11, 10, 8, 6, //
];

/// The per-frequency distortion weight for natural coefficient position `j`: the
/// [`K_WEIGHT_TRELLIS`] entry when weighting is on, else `1` (the plain `err * err`).
const fn trellis_weight(j: usize) -> i64 {
    if TRELLIS_FREQ_WEIGHTING {
        K_WEIGHT_TRELLIS[j]
    } else {
        1
    }
}

/// The rate-distortion multiplier for trellis quantization, derived from a plane's
/// AC dequant step `q_ac`: `lambda = max(1, (7 * q_ac^2) >> LAMBDA_SHIFT)`. Coarser
/// quantizers (larger `q_ac`) give a larger `lambda`, so the entropy `rate` weighs
/// more heavily against the reconstruction distortion and more small coefficients are
/// dropped.
pub(crate) const fn trellis_lambda(q_ac: i32) -> i64 {
    let q = q_ac as i64;
    let l = (7 * q * q) >> LAMBDA_SHIFT;
    if l < 1 { 1 } else { l }
}

/// The outgoing token context a chosen magnitude `m` carries into the next position:
/// `0` for a zero (continuing a run), `1` for `|level| == 1`, `2` for `|level| >= 2`.
const fn ctx_of(m: i32) -> usize {
    if m == 0 {
        0
    } else if m == 1 {
        1
    } else {
        2
    }
}

/// Bits (1/256-bit units) of the large-value tree for a magnitude `m >= 2`, the exact
/// inverse of [`crate::lossy::tokens::put_large_value`] (probabilities `p[3..=10]` plus the
/// hardcoded 159/165/145 literals and the `CAT_3456` category extra bits).
pub(crate) fn large_value_rate(m: i32, p: [Prob; NUM_PROBAS]) -> i64 {
    if m <= 4 {
        let mut c = i64::from(bit_cost(false, p[3]));
        if m == 2 {
            c += i64::from(bit_cost(false, p[4]));
        } else {
            c += i64::from(bit_cost(true, p[4]));
            c += i64::from(bit_cost(m == 4, p[5]));
        }
        c
    } else if m <= 10 {
        let mut c = i64::from(bit_cost(true, p[3])) + i64::from(bit_cost(false, p[6]));
        if m <= 6 {
            c += i64::from(bit_cost(false, p[7]));
            c += i64::from(bit_cost(m == 6, 159));
        } else {
            c += i64::from(bit_cost(true, p[7]));
            let hi = m - 7; // 0..=3
            c += i64::from(bit_cost((hi >> 1) & 1 == 1, 165));
            c += i64::from(bit_cost(hi & 1 == 1, 145));
        }
        c
    } else {
        let mut c = i64::from(bit_cost(true, p[3])) + i64::from(bit_cost(true, p[6]));
        let cat = match m {
            11..=18 => 0usize,
            19..=34 => 1,
            35..=66 => 2,
            _ => 3,
        };
        c += i64::from(bit_cost((cat >> 1) & 1 == 1, p[8]));
        c += i64::from(bit_cost(cat & 1 == 1, p[9 + (cat >> 1)]));
        let extra = m - 3 - (8 << cat);
        let probs = CAT_3456[cat];
        let nbits = probs.len();
        for (i, &prob) in probs.iter().enumerate() {
            c += i64::from(bit_cost((extra >> (nbits - 1 - i)) & 1 == 1, prob));
        }
        c
    }
}

/// Bits to code the value `m` (`m >= 0`) at a position with probability array `p`,
/// EXCLUDING the leading node-0 "more coefficients" bit (charged by the caller from
/// the incoming context). A zero costs the single zero-run bit; a non-zero costs the
/// non-zero node, its one-vs-large magnitude tree and its sign (probability 128 →
/// exactly 256 units).
fn value_rate(m: i32, p: [Prob; NUM_PROBAS]) -> i64 {
    if m == 0 {
        return i64::from(bit_cost(false, p[1]));
    }
    let mut c = i64::from(bit_cost(true, p[1])) + 256; // non-zero node + sign.
    if m == 1 {
        c += i64::from(bit_cost(false, p[2]));
    } else {
        c += i64::from(bit_cost(true, p[2]));
        c += large_value_rate(m, p);
    }
    c
}

/// The exact token-tree bit cost (1/256-bit units) of coding one 4×4 block's
/// quantized `levels` (zig-zag order, positions `first..16`, last non-zero index
/// `last`) with entry context `ctx0`, token `plane` and probability table `bands`.
///
/// A byte-for-byte mirror of [`crate::lossy::tokens::put_coeffs`]: it charges the node-0
/// "more coefficients" bit at the top of each coded position, the node-1 zero-run /
/// non-zero bit, the node-2 one-vs-large decision (plus [`large_value_rate`] for a
/// magnitude `>= 2`), a flat 256-unit sign bit, and the terminal EOB — threading the
/// `(band, ctx)` transitions identically. Because it shares [`value_rate`]'s /
/// [`large_value_rate`]'s primitives with the trellis DP, the RD mode decision in
/// [`crate::lossy::frame`] scores rate in the very same units the trellis optimizes, so the
/// two compose. The result is the true coded length of the block (given `ctx0`), not
/// an estimate.
#[must_use]
pub(crate) fn block_token_cost(
    levels: [i16; 16],
    first: usize,
    last: i32,
    plane: usize,
    ctx0: usize,
    bands: &CoeffProbas,
) -> i64 {
    work!(TokenCostWalk);
    let pb = &bands[plane];
    let mut n = first;
    let mut band = BANDS[n];
    let mut ctx = ctx0;
    let mut cost = 0i64;
    loop {
        let p = pb[band][ctx];
        if n as i32 > last {
            return cost + i64::from(bit_cost(false, p[0])); // EOB / empty block.
        }
        cost += i64::from(bit_cost(true, p[0])); // node 0: more coefficients.
        // Zero run: the first zero reuses the node-0 (band, ctx); each later one is at
        // ctx 0 of the advancing band, exactly as `put_coeffs` walks it.
        while levels[n] == 0 {
            let pz = pb[band][ctx];
            cost += i64::from(bit_cost(false, pz[1]));
            n += 1;
            band = BANDS[n];
            ctx = 0;
        }
        let p = pb[band][ctx];
        cost += i64::from(bit_cost(true, p[1])); // node 1: this position is non-zero.
        let v = i32::from(levels[n]).abs();
        if v == 1 {
            cost += i64::from(bit_cost(false, p[2]));
            ctx = 1;
        } else {
            cost += i64::from(bit_cost(true, p[2]));
            cost += large_value_rate(v, p);
            ctx = 2;
        }
        cost += 256; // sign bit (probability 128 → exactly 256 units).
        n += 1;
        if n == 16 {
            return cost; // full block: no trailing EOB, mirroring `put_coeffs`.
        }
        band = BANDS[n];
    }
}

/// The Viterbi tables over positions `0..16`: per `(position, outgoing context)` the
/// best cumulative rate-distortion cost, the magnitude chosen there, and the incoming
/// context it came from (the backpointer).
struct Viterbi {
    cost: [[i64; 3]; 16],
    mag: [[i32; 3]; 16],
    prev: [[usize; 3]; 16],
}

impl Viterbi {
    /// Reconstruct the winning levels/`recon` by walking the backpointers from the
    /// terminating (last non-zero) node `(best_n, best_out)` down to `first`.
    const fn backtrack(
        &self,
        coeffs: [i16; 16],
        pair: QPair,
        first: usize,
        best_n: usize,
        best_out: usize,
    ) -> Quantized {
        let mut levels = [0i16; 16];
        let mut recon = [0i16; 16];
        let (mut out, mut n) = (best_out, best_n);
        loop {
            let mag = self.mag[n][out];
            let j = ZIGZAG[n];
            let q = if j == 0 { pair.dc.q } else { pair.ac.q };
            let signed = if coeffs[j] < 0 { -mag } else { mag };
            levels[n] = signed as i16;
            recon[j] = (signed * q) as i16;
            if n == first {
                break;
            }
            out = self.prev[n][out];
            n -= 1;
        }
        Quantized {
            levels,
            recon,
            last: best_n as i32,
        }
    }
}

/// Rate-distortion optimal quantization of one 4×4 coefficient block `coeffs`
/// (natural order), starting at zig-zag position `first`, with entry context `ctx0`
/// (the incoming non-zero context the token tree would carry into this block), the
/// plane's `pair` of DC/AC factors, token `plane` (0 i16-AC, 1 i16-DC/Y2, 2 chroma,
/// 3 i4-AC) and probability table `bands`.
///
/// A Viterbi DP over positions `first..16` minimizes `256 * distortion + lambda *
/// rate`; the state is the outgoing token context (`0`/`1`/`2`). Each position tries
/// the nearest level, the nearest minus one, and zero. Returns the same
/// self-consistent [`Quantized`] shape as [`crate::lossy::quant::quantize_block`]:
/// `recon[j] = level * q` and `last` the last non-zero zig-zag index (or `first - 1`
/// when the block trellises to all-zero).
#[must_use]
pub(crate) fn trellis_quantize_block(
    coeffs: [i16; 16],
    pair: QPair,
    first: usize,
    ctx0: usize,
    plane: usize,
    bands: &CoeffProbas,
    lambda: i64,
) -> Quantized {
    let pb = &bands[plane];

    // suffix_dist[n] = squared error of coding positions n..16 all as zero. Used to
    // charge the distortion of the trailing zeros the terminal EOB skips.
    let mut suffix_dist = [0i64; 17];
    for n in (first..16).rev() {
        let j = ZIGZAG[n];
        let c = i64::from(coeffs[j]);
        suffix_dist[n] = suffix_dist[n + 1] + trellis_weight(j) * c * c;
    }

    let mut v = Viterbi {
        cost: [[INF; 3]; 16],
        mag: [[0i32; 3]; 16],
        prev: [[0usize; 3]; 16],
    };
    // Virtual start before `first`: only the entry context ctx0 is reachable.
    let mut prev_cost = [INF; 3];
    prev_cost[ctx0] = 0;
    // The very first node-0 "more" bit is charged once, at `first`, only when the entry
    // context is 0 (otherwise the per-position `base` already charges it).
    let first_extra = if ctx0 == 0 {
        i64::from(bit_cost(true, pb[BANDS[first]][0][0]))
    } else {
        0
    };

    // Best "terminate here" (this position is the last non-zero) across the block.
    let (mut best_total, mut best_n, mut best_out) = (INF, 0usize, 0usize);

    for n in first..16 {
        let j = ZIGZAG[n];
        let factor = if j == 0 { pair.dc } else { pair.ac };
        let (q, abs_coeff) = (factor.q, i32::from(coeffs[j]).abs());
        let l0 = factor.quantize(i32::from(coeffs[j])).abs(); // nearest magnitude.
        debug_assert!(
            l0 <= MAX_LEVEL,
            "quantize clamps the nearest level to MAX_LEVEL"
        );

        // Candidate magnitudes: nearest, nearest-1, and zero — deduplicated. The fixed
        // `[l0, l0-1, 0]` set repeats `0` whenever the nearest level is 0 or 1
        // (l0 == 0 → `[0, -1, 0]`, l0 == 1 → `[1, 0, 0]`), and — since quantization
        // drives most coefficients to zero — that repeat is the common case. Re-scoring
        // the duplicate `0` walks an identical `(m, out)` that can never beat its first
        // evaluation (equal `cand`, strict-`<` update), so it is pure wasted work; drop
        // it. `quantize` clamps to `MAX_LEVEL`, so every distinct candidate is already in
        // `0..=MAX_LEVEL` and needs no per-candidate range test. Depends only on `n`
        // (not the incoming context), so build it once outside the `ci` loop; the order
        // l0 → l0-1 → 0 is preserved so the strict-`<` tie-break at a shared outgoing
        // context stays byte-identical to the full three-candidate walk.
        let mut cands = [l0, 0, 0];
        let mut ncand = 1usize;
        if l0 >= 1 {
            cands[ncand] = l0 - 1;
            ncand += 1;
        }
        if l0 >= 2 {
            cands[ncand] = 0;
            ncand += 1;
        }
        let cands = &cands[..ncand];

        for ci in 0..3 {
            if prev_cost[ci] >= INF {
                continue;
            }
            let p = pb[BANDS[n]][ci];
            let base = if ci > 0 {
                i64::from(bit_cost(true, p[0]))
            } else {
                0
            };
            let extra = if n == first { first_extra } else { 0 };
            for &m in cands {
                work!(TrellisEval);
                let err = i64::from(abs_coeff - m * q);
                let rate = base + extra + value_rate(m, p);
                let cand =
                    prev_cost[ci] + RD_DISTO_MULT * trellis_weight(j) * err * err + lambda * rate;
                let out = ctx_of(m);
                if cand < v.cost[n][out] {
                    v.cost[n][out] = cand;
                    v.mag[n][out] = m;
                    v.prev[n][out] = ci;
                }
            }
        }

        // Terminate at n (n non-zero ⇒ out ∈ {1, 2}): pay the EOB and the trailing
        // zeros' distortion, and keep the global best last-non-zero position.
        for out in [1usize, 2] {
            if v.cost[n][out] >= INF {
                continue;
            }
            let eob = if n == 15 {
                0
            } else {
                i64::from(bit_cost(false, pb[BANDS[n + 1]][out][0]))
            };
            let total = v.cost[n][out] + lambda * eob + RD_DISTO_MULT * suffix_dist[n + 1];
            if total < best_total {
                (best_total, best_n, best_out) = (total, n, out);
            }
        }

        prev_cost = v.cost[n];
    }

    // The all-zero alternative: a single EOB, every coefficient's error retained.
    let empty_total = RD_DISTO_MULT * suffix_dist[first]
        + lambda * i64::from(bit_cost(false, pb[BANDS[first]][ctx0][0]));
    if best_total >= empty_total {
        return Quantized {
            levels: [0; 16],
            recon: [0; 16],
            last: first as i32 - 1,
        };
    }
    v.backtrack(coeffs, pair, first, best_n, best_out)
}

#[cfg(test)]
mod tests {
    use super::{block_token_cost, trellis_lambda, trellis_quantize_block};
    use crate::lossy::constants::{BANDS, COEFFS_PROBA_0, NUM_PROBAS, Prob, ZIGZAG};
    use crate::lossy::prob_opt::bit_cost;
    use crate::lossy::quant::{Quantizer, quantize_block};

    #[test]
    fn block_token_cost_matches_the_hand_walked_token_tree() {
        // An empty block costs exactly one EOB bit at (plane, band[first], ctx0).
        let (plane, first, ctx0) = (0usize, 1usize, 0usize);
        let empty = [0i16; 16];
        let got = block_token_cost(empty, first, first as i32 - 1, plane, ctx0, &COEFFS_PROBA_0);
        let p = COEFFS_PROBA_0[plane][BANDS[first]][ctx0];
        assert_eq!(
            got,
            i64::from(bit_cost(false, p[0])),
            "empty block = one EOB bit"
        );

        // A single unit coefficient at `first`: node-0 "more", node-1 non-zero,
        // node-2 "one", a 256-unit sign, then the terminal EOB at the next band with
        // the outgoing context 1 (a |level|==1 carries context 1).
        let mut levels = [0i16; 16];
        levels[first] = 1;
        let got = block_token_cost(levels, first, first as i32, plane, ctx0, &COEFFS_PROBA_0);
        let p0 = COEFFS_PROBA_0[plane][BANDS[first]][ctx0];
        let eob = COEFFS_PROBA_0[plane][BANDS[first + 1]][1];
        let want = i64::from(bit_cost(true, p0[0]))
            + i64::from(bit_cost(true, p0[1]))
            + i64::from(bit_cost(false, p0[2]))
            + 256
            + i64::from(bit_cost(false, eob[0]));
        assert_eq!(got, want, "one-unit block = more+nonzero+one+sign+EOB");
    }

    #[test]
    fn block_token_cost_charges_more_for_a_busier_block() {
        // A block with several large coefficients must cost strictly more bits than a
        // single unit coefficient — a monotonicity sanity check on the real cost.
        let mut sparse = [0i16; 16];
        sparse[0] = 1;
        let mut busy = [0i16; 16];
        busy[0] = 5;
        busy[1] = -3;
        busy[2] = 17;
        busy[5] = 2;
        let c_sparse = block_token_cost(sparse, 0, 0, 0, 0, &COEFFS_PROBA_0);
        let c_busy = block_token_cost(busy, 0, 5, 0, 0, &COEFFS_PROBA_0);
        assert!(
            c_busy > c_sparse,
            "busy {c_busy} should cost more than sparse {c_sparse}"
        );
    }

    /// The reconstruction contract every trellis result must satisfy: levels are
    /// codable, `recon[j] == level * q` in natural order, and `last` is the last
    /// non-zero zig-zag index (or `first - 1` when empty).
    fn assert_valid(q: Quantizer, first: usize, quantized: &super::Quantized) {
        let (dc, ac) = (q.y1.dc, q.y1.ac);
        let mut want_last = first as i32 - 1;
        for (n, &j) in ZIGZAG.iter().enumerate().skip(first) {
            let level = i32::from(quantized.levels[n]);
            assert!(level.abs() <= 2047, "level {level} at {n} out of range");
            let step = if j == 0 { dc.q } else { ac.q };
            assert_eq!(
                i32::from(quantized.recon[j]),
                level * step,
                "recon[{j}] must equal level*q"
            );
            if level != 0 {
                want_last = n as i32;
            }
        }
        assert_eq!(
            quantized.last, want_last,
            "last must be the last non-zero index"
        );
    }

    #[test]
    fn trellis_result_is_valid_and_self_consistent_in_shape() {
        let q = Quantizer::new(40);
        let mut coeffs = [0i16; 16];
        coeffs[0] = 90;
        coeffs[1] = -50;
        coeffs[4] = 12;
        coeffs[8] = 200;
        let lambda = trellis_lambda(q.y1.ac.q);
        let out = trellis_quantize_block(coeffs, q.y1, 0, 0, 0, &COEFFS_PROBA_0, lambda);
        assert_valid(q, 0, &out);
    }

    #[test]
    fn tiny_coefficients_trellis_toward_zero() {
        // A block of coefficients all far below the quant step: round-to-nearest may
        // keep a stray level, but the rate-distortion trade drops them, so the block
        // trellises empty (last = first - 1).
        let q = Quantizer::new(80); // coarse steps.
        let mut coeffs = [0i16; 16];
        coeffs[1] = 3;
        coeffs[4] = -2;
        coeffs[5] = 4;
        let lambda = trellis_lambda(q.y1.ac.q);
        let out = trellis_quantize_block(coeffs, q.y1, 1, 0, 0, &COEFFS_PROBA_0, lambda);
        assert_valid(q, 1, &out);
        assert_eq!(
            out.last, 0,
            "tiny AC-only block should trellis to empty (last = first-1)"
        );
    }

    #[test]
    fn trellis_never_scores_worse_than_round_to_nearest_and_is_deterministic() {
        // The round-to-nearest assignment is one candidate path in the DP, so the
        // trellis result can never code a larger last index than round-to-nearest on
        // this block, and repeated runs are byte-identical.
        let q = Quantizer::new(60);
        let mut coeffs = [0i16; 16];
        coeffs[0] = 140;
        coeffs[1] = 41;
        coeffs[2] = -39;
        coeffs[8] = 8;
        let rn = quantize_block(coeffs, q.y1.dc, q.y1.ac, 0);
        let lambda = trellis_lambda(q.y1.ac.q);
        let a = trellis_quantize_block(coeffs, q.y1, 0, 0, 0, &COEFFS_PROBA_0, lambda);
        let b = trellis_quantize_block(coeffs, q.y1, 0, 0, 0, &COEFFS_PROBA_0, lambda);
        assert_eq!(a.levels, b.levels, "trellis must be deterministic");
        assert_eq!(a.recon, b.recon);
        assert!(
            a.last <= rn.last,
            "trellis last {} > round-to-nearest {}",
            a.last,
            rn.last
        );
        assert_valid(q, 0, &a);
    }

    #[test]
    fn lambda_grows_with_the_quantizer_step() {
        assert_eq!(trellis_lambda(4).max(1), trellis_lambda(4));
        assert!(trellis_lambda(200) > trellis_lambda(20));
        assert!(trellis_lambda(1) >= 1, "lambda is clamped to at least 1");
    }

    /// `lambda = max(1, (7 * q^2) >> LAMBDA_SHIFT)`, asserted to the exact integer.
    /// Pins the `7 * q * q` product and the shift/clamp so any arithmetic drift in
    /// [`trellis_lambda`] flips a value (`(7 + q) * q` and `7 * q + q`, say, both
    /// diverge from `7 * q * q`), while the `q0/q4` clamp cases anchor the floor.
    #[test]
    fn trellis_lambda_is_exact() {
        // With per-frequency weighting on, LAMBDA_SHIFT is libwebp's `>> 3`:
        // (7*1*1)>>3 = 0 -> clamped to 1; (7*4*4)>>3 = 112>>3 = 14;
        // (7*400)>>3 = 350; (7*10000)>>3 = 8750; (7*40000)>>3 = 35000.
        assert_eq!(trellis_lambda(1), 1);
        assert_eq!(trellis_lambda(4), 14);
        assert_eq!(trellis_lambda(20), 350);
        assert_eq!(trellis_lambda(100), 8750);
        assert_eq!(trellis_lambda(200), 35000);
    }

    /// The large-value probability array used by the exact-rate tests: every entry in
    /// the `p[3..=10]` range distinct and `!= 128`, so a `false`/`true` bit cost never
    /// coincides and a mistaken probability index (e.g. `p[9 + (cat >> 1)]`) shows up
    /// as a changed total.
    const RATE_P: [Prob; NUM_PROBAS] = [128, 128, 128, 40, 50, 60, 70, 80, 90, 100, 110];

    /// [`large_value_rate`] equals the hand-verified token-tree bit cost for a
    /// representative magnitude in **every** category and sub-branch: the `m <= 4`
    /// one/two/three split, the `5..=10` `<= 6` vs `7..=10` hi-bit split, and each
    /// DCT category `11..=18 / 19..=34 / 35..=66 / >= 67` with its `CAT_3456` extra
    /// bits. The golden totals were captured from the real encoder; any mutation to a
    /// probability index, comparison, category boundary, shift or accumulation flips at
    /// least one entry.
    #[test]
    fn large_value_rate_is_exact_across_every_category() {
        use super::large_value_rate;
        let want = [
            (2, 1289),
            (3, 1302),
            (4, 865),
            (5, 1148),
            (6, 1330),
            (7, 1052),
            (8, 1151),
            (9, 1272),
            (10, 1371),
            (11, 1484),
            (12, 1553),
            (18, 1941),
            (19, 1532),
            (25, 1759),
            (34, 2092),
            (35, 1673),
            (50, 1966),
            (66, 2285),
            (67, 2006),
            (100, 2310),
            (300, 3621),
            (700, 4751),
            (2047, 8013),
        ];
        for (m, w) in want {
            assert_eq!(large_value_rate(m, RATE_P), w, "large_value_rate({m})");
        }
    }

    /// [`value_rate`] equals the hand-verified cost for a zero, a `|level| == 1`, and a
    /// spread of large magnitudes: the zero-run bit alone for `m == 0`, and
    /// `non-zero + sign(256) + one-vs-large + large_value_rate` otherwise. Pins the
    /// `+ 256` sign term and both node-2 branches.
    #[test]
    fn value_rate_is_exact() {
        use super::value_rate;
        let want = [
            (0, 256),
            (1, 768),
            (2, 2057),
            (3, 2070),
            (5, 1916),
            (8, 1919),
            (20, 2341),
            (100, 3078),
            (2047, 8781),
        ];
        for (m, w) in want {
            assert_eq!(value_rate(m, RATE_P), w, "value_rate({m})");
        }
    }

    /// [`block_token_cost`] equals the exact token-tree length for blocks that exercise
    /// the zero-run accumulation and the large-value (`|level| >= 2`) branch — the
    /// paths the empty/one-unit test in `..._hand_walked_token_tree` does not reach.
    /// The three totals are golden captures; `-=`/`*=` mutations of either accumulator
    /// flip them.
    #[test]
    fn block_token_cost_exact_for_zero_run_and_large_value() {
        // Two leading zeros then a `3` (large): node0 + two zero-run bits + node1 +
        // node2-large + large_value_rate(3) + sign + EOB.
        let mut zero_run = [0i16; 16];
        zero_run[2] = 3;
        assert_eq!(
            block_token_cost(zero_run, 0, 2, 0, 0, &COEFFS_PROBA_0),
            3893
        );
        // A unit at 0 then a zero then a `-2` (large) at 3.
        let mut mixed = [0i16; 16];
        mixed[0] = 1;
        mixed[3] = -2;
        assert_eq!(block_token_cost(mixed, 0, 3, 0, 0, &COEFFS_PROBA_0), 4636);
        // A single large `4` starting at `first == 1`.
        let mut large = [0i16; 16];
        large[1] = 4;
        assert_eq!(block_token_cost(large, 1, 1, 0, 0, &COEFFS_PROBA_0), 5941);
    }

    /// Build a natural-order coefficient block from `(index, value)` pairs.
    fn block(pairs: &[(usize, i16)]) -> [i16; 16] {
        let mut c = [0i16; 16];
        for &(i, v) in pairs {
            c[i] = v;
        }
        c
    }

    /// Run the trellis with the plane-0 default probabilities and the lambda derived
    /// from the block's own AC step.
    fn trellis(base_q: i32, coeffs: [i16; 16], first: usize, ctx0: usize) -> super::Quantized {
        let q = Quantizer::new(base_q);
        let lambda = trellis_lambda(q.y1.ac.q);
        trellis_quantize_block(coeffs, q.y1, first, ctx0, 0, &COEFFS_PROBA_0, lambda)
    }

    /// The full [`trellis_quantize_block`] result — `levels`, `recon` and `last` — is
    /// pinned byte-for-byte on eight diverse blocks chosen to drive every Viterbi
    /// decision: candidate construction (`l0`, `l0-1`, `0`), the `ci > 0` node-0
    /// charge, the terminate-here EOB (including the `n == 15` special case and a
    /// non-zero riding all the way to position 15), the trailing-zero `suffix_dist`
    /// term, non-zero entry contexts (`ctx0 ∈ {1, 2}`) and a `first > 0` start, plus
    /// the all-zero fallback (`last == first - 1`). The goldens are captures of the
    /// real encoder; any decision-cost mutation that alters a chosen level, its
    /// reconstruction or the last index flips one of these assertions.
    #[test]
    fn trellis_quantize_block_exact_goldens() {
        // (base_q, coeffs, first, ctx0, expected levels, expected recon, expected last)
        struct Case {
            q: i32,
            coeffs: [i16; 16],
            first: usize,
            ctx0: usize,
            levels: [i16; 16],
            recon: [i16; 16],
            last: i32,
        }
        let cases = [
            Case {
                q: 40,
                coeffs: block(&[(0, 90), (1, -50), (4, 12), (8, 200)]),
                first: 0,
                ctx0: 0,
                levels: block(&[(0, 2), (1, -1), (3, 5)]),
                recon: block(&[(0, 74), (1, -44), (8, 220)]),
                last: 3,
            },
            // A non-zero surviving all the way to position 15 (exercises `n == 15`).
            Case {
                q: 20,
                coeffs: block(&[(0, 100), (5, 60), (15, 400)]),
                first: 0,
                ctx0: 0,
                levels: block(&[(0, 5), (4, 2), (15, 17)]),
                recon: block(&[(0, 105), (5, 48), (15, 408)]),
                last: 15,
            },
            Case {
                q: 30,
                coeffs: block(&[(0, 200), (8, 150)]),
                first: 0,
                ctx0: 0,
                levels: block(&[(0, 7), (3, 4)]),
                recon: block(&[(0, 189), (8, 136)]),
                last: 3,
            },
            // first > 0 and a non-zero entry context.
            Case {
                q: 40,
                coeffs: block(&[(1, 90), (4, -50), (8, 30)]),
                first: 1,
                ctx0: 2,
                levels: block(&[(1, 2), (2, -1), (3, 1)]),
                recon: block(&[(1, 88), (4, -44), (8, 44)]),
                last: 3,
            },
            Case {
                q: 40,
                coeffs: block(&[(0, 60), (1, 30), (2, -45), (5, 12)]),
                first: 0,
                ctx0: 1,
                levels: block(&[(0, 2), (1, 1), (5, -1)]),
                recon: block(&[(0, 74), (1, 44), (2, -44)]),
                last: 5,
            },
            Case {
                q: 64,
                coeffs: block(&[(0, 130), (1, 41), (2, -39), (8, 8)]),
                first: 0,
                ctx0: 0,
                levels: block(&[(0, 2)]),
                recon: block(&[(0, 118)]),
                last: 0,
            },
            // Tiny AC-only coefficients trellis to the all-zero fallback.
            Case {
                q: 100,
                coeffs: block(&[(1, 3), (4, -2), (5, 4)]),
                first: 1,
                ctx0: 0,
                levels: [0; 16],
                recon: [0; 16],
                last: 0,
            },
            Case {
                q: 52,
                coeffs: block(&[(0, 200), (1, 55), (3, 48), (7, 40), (12, 35)]),
                first: 0,
                ctx0: 0,
                levels: block(&[(0, 4), (1, 1), (6, 1)]),
                recon: block(&[(0, 188), (1, 56), (3, 56)]),
                last: 6,
            },
        ];
        for (i, c) in cases.iter().enumerate() {
            let out = trellis(c.q, c.coeffs, c.first, c.ctx0);
            assert_eq!(out.levels, c.levels, "case {i} levels");
            assert_eq!(out.recon, c.recon, "case {i} recon");
            assert_eq!(out.last, c.last, "case {i} last");
        }
    }

    /// A low-frequency-AC survival boundary. At `base_q = 17` (DC step 19, AC step 21)
    /// the block `{coeff[0] = 33, coeff[3] = 20}` sits where dropping the AC coefficient
    /// into the trailing-zero `suffix_dist` (DC-only, `last = 0`) trades against coding
    /// it as level 1 (`last = 6`, natural position 3 = zig-zag index 6). Because natural
    /// position 3 carries a high per-frequency [`K_WEIGHT_TRELLIS`] weight, its weighted
    /// distortion outweighs the extra rate, so the trellis keeps it — an exact golden
    /// pinning the weighted objective at the boundary. (The strict-`<` tie-break
    /// *ordering* is exercised across ties by [`trellis_matches_the_brute_force_optimum`],
    /// whose odometer visits candidates in the same `l0 -> l0-1 -> 0` order.)
    #[test]
    fn trellis_low_frequency_ac_survives_the_weighted_objective() {
        let coeffs = block(&[(0, 33), (3, 20)]);
        let out = trellis(17, coeffs, 0, 0);
        assert_eq!(
            out.levels,
            block(&[(0, 2), (6, 1)]),
            "DC level 2 and the low-frequency AC (zig-zag 6) as level 1"
        );
        assert_eq!(
            out.recon,
            block(&[(0, 38), (3, 21)]),
            "recon: DC 2*19 and AC 1*21 at natural position 3"
        );
        assert_eq!(
            out.last, 6,
            "the surviving AC lands the last non-zero at zig-zag 6"
        );
    }

    /// An independent, exhaustive rate-distortion minimizer over the **same**
    /// per-position candidate set the trellis considers (`{l0, l0-1, 0}`), scoring each
    /// full level assignment as `RD_DISTO_MULT * distortion + lambda * rate` where the
    /// rate is the *true* coder [`block_token_cost`] — a byte-for-byte oracle that
    /// shares none of [`trellis_quantize_block`]'s Viterbi bookkeeping (candidate
    /// construction, context threading, per-position EOB, `suffix_dist`, the all-zero
    /// fallback). Blocks are confined so at most a handful of positions carry more than
    /// one candidate, keeping the Cartesian product tiny.
    fn brute_force(
        coeffs: [i16; 16],
        pair: super::QPair,
        first: usize,
        ctx0: usize,
        lambda: i64,
    ) -> super::Quantized {
        let npos = 16 - first;
        let mut cand = [[0i32; 3]; 16];
        let mut ncand = [0usize; 16];
        for k in 0..npos {
            let n = first + k;
            let j = ZIGZAG[n];
            let factor = if j == 0 { pair.dc } else { pair.ac };
            let l0 = factor.quantize(i32::from(coeffs[j])).abs();
            let mut list = [l0, 0, 0];
            let mut c = 1usize;
            if l0 >= 1 {
                list[c] = l0 - 1;
                c += 1;
            }
            if l0 >= 2 {
                list[c] = 0;
                c += 1;
            }
            cand[k] = list;
            ncand[k] = c;
        }
        let mut idx = [0usize; 16];
        let mut best_total = i64::MAX;
        let mut best = super::Quantized {
            levels: [0; 16],
            recon: [0; 16],
            last: first as i32 - 1,
        };
        loop {
            let mut levels = [0i16; 16];
            let mut recon = [0i16; 16];
            let mut last = first as i32 - 1;
            let mut dist = 0i64;
            for k in 0..npos {
                let n = first + k;
                let j = ZIGZAG[n];
                let factor = if j == 0 { pair.dc } else { pair.ac };
                let q = factor.q;
                let mag = cand[k][idx[k]];
                let abs_c = i32::from(coeffs[j]).abs();
                let err = i64::from(abs_c - mag * q);
                dist += super::trellis_weight(j) * err * err;
                let signed = if coeffs[j] < 0 { -mag } else { mag };
                levels[n] = signed as i16;
                recon[j] = (signed * q) as i16;
                if mag != 0 {
                    last = n as i32;
                }
            }
            let rate = block_token_cost(levels, first, last, 0, ctx0, &COEFFS_PROBA_0);
            let total = super::RD_DISTO_MULT * dist + lambda * rate;
            if total < best_total {
                best_total = total;
                best = super::Quantized {
                    levels,
                    recon,
                    last,
                };
            }
            // Odometer: position `first` (k = 0) varies fastest, matching the trellis's
            // `l0 -> l0-1 -> 0` candidate order so the strict-`<` tie-break agrees.
            let mut k = 0;
            loop {
                if k == npos {
                    return best;
                }
                idx[k] += 1;
                if idx[k] < ncand[k] {
                    break;
                }
                idx[k] = 0;
                k += 1;
            }
        }
    }

    /// [`trellis_quantize_block`] equals the independent [`brute_force`] optimum on a
    /// deterministic battery of blocks spanning every quantizer regime, entry context,
    /// `first`, and coefficient shape (sparse, dense, boundary magnitudes that make
    /// `l0-1` or dropping to zero the winner, and non-zeros riding to position 15).
    /// Because the oracle recomputes the objective from scratch, any Viterbi-internal
    /// mutation that changes a chosen level — candidate construction, the `ci > 0`
    /// node-0 charge, the `n == 15` EOB special case, the `suffix_dist`/empty terms, or
    /// a cost accumulation — makes the two disagree on at least one block.
    #[test]
    fn trellis_matches_the_brute_force_optimum() {
        // A small deterministic LCG drives the battery — no `rand`, fully reproducible.
        let mut s: u32 = 0x1234_5678;
        let mut next = || {
            s = s.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            s
        };
        for trial in 0..400u32 {
            let base_q = 8 + (next() % 100) as i32;
            let first = (next() % 2) as usize; // 0 or 1
            let ctx0 = (next() % 3) as usize;
            // Confine non-zeros to the first few positions so the product stays tiny,
            // but let one block reach position 15.
            let span = first + 5 + (next() % 3) as usize; // up to ~7 active positions
            let mut coeffs = [0i16; 16];
            for n in first..16 {
                let active = n < span || (n == 15 && (next() % 4 == 0));
                if active {
                    // Signed magnitudes clustered near quant boundaries.
                    let mag = (next() % 260) as i32 - 130;
                    coeffs[ZIGZAG[n]] = mag as i16;
                }
            }
            let q = Quantizer::new(base_q);
            let lambda = trellis_lambda(q.y1.ac.q);
            let got = trellis_quantize_block(coeffs, q.y1, first, ctx0, 0, &COEFFS_PROBA_0, lambda);
            let want = brute_force(coeffs, q.y1, first, ctx0, lambda);
            assert_eq!(
                got.last, want.last,
                "trial {trial}: last mismatch (q={base_q}, first={first}, ctx0={ctx0})"
            );
            assert_eq!(got.levels, want.levels, "trial {trial}: levels mismatch");
            assert_eq!(got.recon, want.recon, "trial {trial}: recon mismatch");
        }
    }
}
