//! Coefficient-probability optimization (encoder-only, layer 2).
//!
//! Once the encoder has quantized every macroblock it knows the exact
//! distribution of the boolean decisions its token tree will code.
//! [`optimize_probas`] turns those per-node counts ([`CoeffStats`]) into an
//! improved probability table and, for each of the `4 × 8 × 3 × 11` main-tree
//! nodes, decides with an integer bit-cost model whether transmitting an updated
//! probability pays for itself against simply keeping the [`COEFFS_PROBA_0`]
//! default. The caller ([`crate::lossy::frame`]) then emits whichever of {defaults,
//! optimized} yields the smaller payload, so this can only shrink the frame: it
//! never changes a single reconstructed pixel, and the decoder already applies
//! the updates in `header::parse_proba`.
//!
//! All arithmetic is integer (the crate forbids floating point): the cost model
//! reads the fixed-point [`ENTROPY_COST`] table (1/256-bit units).

use crate::lossy::constants::{
    COEFFS_PROBA_0, COEFFS_UPDATE_PROBA, CoeffProbas, CoeffStats, CoeffUpdateFlags, ENTROPY_COST,
    NUM_BANDS, NUM_CTX, NUM_PROBAS, NUM_TYPES,
};
use crate::lossy::work::work;

/// The fixed-point cost (1/256-bit units) of coding `bit` at probability
/// `prob / 256`, from the [`ENTROPY_COST`] table. `prob` is the probability that
/// the coded bit is zero, so a one costs the complementary `256 - prob` entry.
pub(crate) fn bit_cost(bit: bool, prob: u8) -> u32 {
    let idx = if bit {
        256 - usize::from(prob)
    } else {
        usize::from(prob)
    };
    u32::from(ENTROPY_COST[idx])
}

/// Decide one node from its observed `[bit0, bit1]` counts, its default
/// probability and the node's update-flag probability: return the probability to
/// transmit and whether to send an update. The empirically-optimal probability is
/// sent only when its coding saving outweighs the update flag plus the 8-bit
/// literal (`8 * 256` units); otherwise the default is kept verbatim.
fn optimize_node(c0: u64, c1: u64, default_p: u8, update_proba: u8) -> (u8, bool) {
    let total = c0 + c1;
    if total == 0 {
        return (default_p, false);
    }
    // Rounded empirical probability of bit == 0, clamped to the legal 1..=255.
    // The clamp bounds the value to a valid u8, so the narrowing cast is lossless
    // (it states that directly rather than via a `try_from(..).unwrap_or(..)` whose
    // error arm is unreachable).
    let ratio = ((c0 << 8) + (total >> 1)) / total;
    let new_p = ratio.clamp(1, 255) as u8;

    let cost_default =
        c0 * u64::from(bit_cost(false, default_p)) + c1 * u64::from(bit_cost(true, default_p));
    let cost_new = c0 * u64::from(bit_cost(false, new_p)) + c1 * u64::from(bit_cost(true, new_p));
    // The update-flag decision itself: 0 keeps the default, 1 plus an 8-bit
    // literal transmits the new probability.
    let cost_keep = u64::from(bit_cost(false, update_proba));
    let cost_update = u64::from(bit_cost(true, update_proba)) + 8 * 256;

    if cost_new + cost_update < cost_default + cost_keep {
        (new_p, true)
    } else {
        (default_p, false)
    }
}

/// Build an improved coefficient-probability table and the matching update flags
/// from the frame's observed token statistics. Each node independently either
/// keeps its [`COEFFS_PROBA_0`] default (flag clear) or transmits an optimized
/// probability (flag set), whichever the integer bit-cost model in
/// [`optimize_node`] prefers.
pub(crate) fn optimize_probas(stats: &CoeffStats) -> (CoeffProbas, CoeffUpdateFlags) {
    let mut probas = COEFFS_PROBA_0;
    let mut updated = CoeffUpdateFlags::default();
    for t in 0..NUM_TYPES {
        for b in 0..NUM_BANDS {
            for c in 0..NUM_CTX {
                for p in 0..NUM_PROBAS {
                    work!(ProbaOptNode);
                    let [c0, c1] = stats[t][b][c][p];
                    let (prob, upd) = optimize_node(
                        c0,
                        c1,
                        COEFFS_PROBA_0[t][b][c][p],
                        COEFFS_UPDATE_PROBA[t][b][c][p],
                    );
                    probas[t][b][c][p] = prob;
                    updated[t][b][c][p] = upd;
                }
            }
        }
    }
    (probas, updated)
}

#[cfg(test)]
mod tests {
    use super::{optimize_node, optimize_probas};
    use crate::lossy::constants::{
        COEFFS_PROBA_0, CoeffStats, NUM_BANDS, NUM_CTX, NUM_PROBAS, NUM_TYPES,
    };

    // The exact-value tests below pin `optimize_node`'s integer arithmetic. The
    // constants are derived by hand from `ENTROPY_COST`: bit_cost(bit, p) reads
    // ENTROPY_COST[if bit { 256 - p } else { p }], and E[128]=256, E[192]=106,
    // E[64]=512, E[194]=102, E[62]=524, E[250]=9, E[255]=1.

    #[test]
    fn optimize_node_pins_rounded_probability_and_update() {
        // 3000 zeros / 1000 ones. Empirical prob of bit0 rounds to
        //   ((3000 << 8) + 4000/2) / 4000 = 770000 / 4000 = 192.
        // Against a 50/50 default (128) the saving clears the update overhead, so
        // the optimizer transmits exactly 192. This pins the whole ratio
        // expression (`<< 8`, `+ total/2`, `/ total`) and the cost_default terms.
        assert_eq!(optimize_node(3000, 1000, 128, 128), (192, true));
    }

    #[test]
    fn optimize_node_keeps_default_until_saving_beats_overhead() {
        // 1000 zeros / 1000 ones optimizes to the 128 default exactly, so there is
        // no coding saving to offset the 8-bit literal + update flag: the node must
        // keep its default and stay unflagged. Any mutation that shrinks cost_new
        // (dropping either factor of `c0 * ..` / `c1 * ..` on line 51) would make
        // the update spuriously worthwhile and flip the flag to true.
        assert_eq!(optimize_node(1000, 1000, 128, 128), (128, false));
    }

    #[test]
    fn optimize_node_update_boundary_is_strict() {
        // A hand-tuned exact tie: 256 zeros (new_p clamps to 255) against default
        // 250 with update_proba 128 gives
        //   cost_new + cost_update = 256*1 + (256 + 2048) = 2560
        //   cost_default + cost_keep = 256*9 + 256          = 2560.
        // The decision is strictly `<`, so an equal cost must KEEP the default
        // (250, false); relaxing the comparison to `<=` would transmit (255, true).
        assert_eq!(optimize_node(256, 0, 250, 128), (250, false));
    }

    #[test]
    fn all_zero_stats_keep_every_default() {
        // With no observed tokens every node must fall back to the defaults and
        // transmit no update.
        let stats = Box::<CoeffStats>::default();
        let (probas, updated) = optimize_probas(&stats);
        assert_eq!(probas, COEFFS_PROBA_0, "empty stats must keep the defaults");
        for plane in &updated {
            for band in plane {
                for ctx in band {
                    for &u in ctx {
                        assert!(!u, "no node may be updated on empty stats");
                    }
                }
            }
        }
    }

    #[test]
    fn a_skewed_node_is_updated_toward_its_empirical_probability() {
        // One node overwhelmingly codes bit == 0: its probability of a zero should
        // optimize near 255 and be worth transmitting; every other node is left at
        // its default and unflagged.
        let mut stats = Box::<CoeffStats>::default();
        stats[0][1][0][0] = [10_000, 5];
        let (probas, updated) = optimize_probas(&stats);
        assert!(updated[0][1][0][0], "the skewed node must be updated");
        assert!(
            probas[0][1][0][0] >= 250,
            "empirical prob of bit0 near 255, got {}",
            probas[0][1][0][0]
        );
        for t in 0..NUM_TYPES {
            for b in 0..NUM_BANDS {
                for c in 0..NUM_CTX {
                    for p in 0..NUM_PROBAS {
                        if (t, b, c, p) != (0, 1, 0, 0) {
                            assert_eq!(
                                probas[t][b][c][p], COEFFS_PROBA_0[t][b][c][p],
                                "untouched node ({t},{b},{c},{p}) changed"
                            );
                            assert!(!updated[t][b][c][p], "node ({t},{b},{c},{p}) updated");
                        }
                    }
                }
            }
        }
    }
}
