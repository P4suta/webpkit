//! Encoder cost model: a symbol-frequency histogram over the five VP8L Huffman
//! codes plus a deterministic bit-cost estimate.
//!
//! The estimate lets the encoder choose a color-cache size and reference
//! strategy by comparing candidates *without* materializing each stream. All
//! arithmetic is integer / fixed-point, so an estimate is identical on every
//! platform (a codec that emits golden files must never depend on float
//! rounding) and the crate stays `no_std`.

use crate::lossless::constants::{NUM_DISTANCE_CODES, NUM_LENGTH_CODES, NUM_LITERAL_CODES};
use crate::lossless::prelude::*;

/// Fractional bits carried by [`fixed_log2`] and the entropy accumulator.
const LOG2_FRAC_BITS: u32 = 16;

/// A per-group symbol histogram: one bin per symbol of each of the five VP8L
/// codes, plus the running total of raw length/distance extra bits.
///
/// The green code is variable width (`280 + (1 << cache_bits)`); the other four
/// are fixed by the format. The bins double as the input to the Huffman code
/// builder and the [`Self::estimate_bits`] cost model, so a single walk of the
/// token stream feeds both.
#[cfg_attr(test, derive(PartialEq, Eq, Debug, Clone))]
pub(crate) struct Histogram {
    green: Vec<u32>,
    red: [u32; NUM_LITERAL_CODES],
    blue: [u32; NUM_LITERAL_CODES],
    alpha: [u32; NUM_LITERAL_CODES],
    dist: [u32; NUM_DISTANCE_CODES],
    extra_bits: u64,
}

impl Histogram {
    /// A zeroed histogram whose green alphabet holds `green_alphabet` symbols
    /// (`280` with no cache, `280 + (1 << cache_bits)` with one).
    pub(crate) fn new(green_alphabet: usize) -> Self {
        Self {
            green: vec![0; green_alphabet],
            red: [0; NUM_LITERAL_CODES],
            blue: [0; NUM_LITERAL_CODES],
            alpha: [0; NUM_LITERAL_CODES],
            dist: [0; NUM_DISTANCE_CODES],
            extra_bits: 0,
        }
    }

    /// Zero every bin (keeping the green `Vec`'s allocation) so a scratch
    /// histogram can be refilled instead of reallocated. Because a zero-count
    /// bin contributes nothing to [`Self::estimate_bits`], a max-size green
    /// (`280 + (1 << MAX_CACHE_BITS)`) reset once and reused for every candidate
    /// cache size yields identical estimates to a per-size exact histogram.
    pub(crate) fn reset(&mut self) {
        self.green.iter_mut().for_each(|b| *b = 0);
        self.red = [0; NUM_LITERAL_CODES];
        self.blue = [0; NUM_LITERAL_CODES];
        self.alpha = [0; NUM_LITERAL_CODES];
        self.dist = [0; NUM_DISTANCE_CODES];
        self.extra_bits = 0;
    }

    /// Clone this histogram with its green channel truncated to `green_alphabet`
    /// symbols. The reused max-size scratch in `best_cache_bits` zero-pads green
    /// to the `MAX_CACHE_BITS` alphabet; the bins past a smaller candidate's
    /// alphabet are always zero, so truncating to that candidate's green length
    /// yields a histogram bin-identical to a fresh per-size build — the emitted
    /// prefix code (whose full form RLE-encodes every green length) is therefore
    /// unchanged. `green_alphabet` must not exceed the current green length.
    pub(crate) fn snapshot_truncated(&self, green_alphabet: usize) -> Self {
        Self {
            green: self.green[..green_alphabet].to_vec(),
            red: self.red,
            blue: self.blue,
            alpha: self.alpha,
            dist: self.dist,
            extra_bits: self.extra_bits,
        }
    }

    /// Count one ARGB literal across the green/red/blue/alpha channels.
    pub(crate) fn add_literal(&mut self, argb: u32) {
        self.green[((argb >> 8) & 0xff) as usize] += 1;
        self.red[((argb >> 16) & 0xff) as usize] += 1;
        self.blue[(argb & 0xff) as usize] += 1;
        self.alpha[((argb >> 24) & 0xff) as usize] += 1;
    }

    /// Count one back-reference length symbol (a green code `256..280`) and its
    /// extra bits.
    pub(crate) fn add_length(&mut self, length_symbol: u32, extra_bits: u32) {
        self.green[NUM_LITERAL_CODES + length_symbol as usize] += 1;
        self.extra_bits += u64::from(extra_bits);
    }

    /// Count one back-reference distance symbol and its extra bits.
    pub(crate) fn add_distance(&mut self, dist_symbol: u32, extra_bits: u32) {
        self.dist[dist_symbol as usize] += 1;
        self.extra_bits += u64::from(extra_bits);
    }

    /// Count one color-cache reference (a green code `>= 280`).
    pub(crate) fn add_cache(&mut self, key: u16) {
        self.green[NUM_LITERAL_CODES + NUM_LENGTH_CODES + key as usize] += 1;
    }

    /// The green channel bins (literals, length codes, then cache keys).
    pub(crate) fn green(&self) -> &[u32] {
        &self.green
    }
    /// The red channel bins.
    pub(crate) const fn red(&self) -> &[u32] {
        &self.red
    }
    /// The blue channel bins.
    pub(crate) const fn blue(&self) -> &[u32] {
        &self.blue
    }
    /// The alpha channel bins.
    pub(crate) const fn alpha(&self) -> &[u32] {
        &self.alpha
    }
    /// The distance channel bins.
    pub(crate) const fn dist(&self) -> &[u32] {
        &self.dist
    }

    /// Element-wise accumulate `other` into `self`. Both must share the same green
    /// alphabet length (same `cache_bits`).
    pub(crate) fn add_assign(&mut self, other: &Self) {
        assert_eq!(
            self.green.len(),
            other.green.len(),
            "green alphabets must match"
        );
        for (a, b) in self.green.iter_mut().zip(&other.green) {
            *a += *b;
        }
        for (a, b) in self.red.iter_mut().zip(&other.red) {
            *a += *b;
        }
        for (a, b) in self.blue.iter_mut().zip(&other.blue) {
            *a += *b;
        }
        for (a, b) in self.alpha.iter_mut().zip(&other.alpha) {
            *a += *b;
        }
        for (a, b) in self.dist.iter_mut().zip(&other.dist) {
            *a += *b;
        }
        self.extra_bits += other.extra_bits;
    }

    /// `estimate_bits` of the element-wise SUM of `self` and `other`, computed
    /// without allocating the merged histogram — the greedy merge-cost query.
    pub(crate) fn merged_estimate_bits(&self, other: &Self) -> u64 {
        let channels = [
            channel_bits_pair(&self.green, &other.green),
            channel_bits_pair(&self.red, &other.red),
            channel_bits_pair(&self.blue, &other.blue),
            channel_bits_pair(&self.alpha, &other.alpha),
            channel_bits_pair(&self.dist, &other.dist),
        ];
        channels.into_iter().map(|(bits, _used)| bits).sum::<u64>()
            + channels
                .into_iter()
                .map(|(_bits, used)| header_bits(used))
                .sum::<u64>()
            + self.extra_bits
            + other.extra_bits
    }

    /// Estimate the encoded size, in bits, of this token distribution.
    ///
    /// The pixel-data term is the Shannon entropy of each channel
    /// (`Σ n·log2(total/n)`, the ideal prefix-code cost) plus the raw extra
    /// bits; a small per-channel term approximates the code-length header so a
    /// channel that uses many symbols is not judged for free. The estimate is a
    /// close lower bound on the real Huffman output — good enough to rank
    /// candidates, and a single-symbol channel scores exactly `0` (matching the
    /// zero-bit trap), so a solid image never prefers a back-reference.
    pub(crate) fn estimate_bits(&self) -> u64 {
        let channels = [
            channel_bits(&self.green),
            channel_bits(&self.red),
            channel_bits(&self.blue),
            channel_bits(&self.alpha),
            channel_bits(&self.dist),
        ];
        channels.into_iter().map(|(bits, _used)| bits).sum::<u64>()
            + channels
                .into_iter()
                .map(|(_bits, used)| header_bits(used))
                .sum::<u64>()
            + self.extra_bits
    }
}

/// Shannon cost (whole bits) and number of used symbols of one channel.
fn channel_bits(bins: &[u32]) -> (u64, u64) {
    let total: u64 = bins.iter().map(|&n| u64::from(n)).sum();
    if total == 0 {
        return (0, 0);
    }
    // Σ n·log2(total/n) = total·log2(total) − Σ n·log2(n), computed in fixed point.
    let mut acc = total * fixed_log2(total);
    let mut used = 0u64;
    for &n in bins {
        if n > 0 {
            let n64 = u64::from(n);
            acc -= n64 * fixed_log2(n64);
            used += 1;
        }
    }
    (acc >> LOG2_FRAC_BITS, used)
}

/// Shannon cost (whole bits) of a channel from its total count and the counts of
/// just its used (nonzero) symbols. Σ n·log2(total/n) = total·log2(total) − Σ n·log2(n),
/// fixed point — the O(used) sibling of [`channel_bits`], for the predictor's
/// per-tile mode-entropy scoring (avoids a 256-bin scan per mode).
pub(crate) fn shannon_bits(total: u64, used_counts: &[u32]) -> u64 {
    if total == 0 {
        return 0;
    }
    let mut acc = total * fixed_log2(total);
    for &n in used_counts {
        let n64 = u64::from(n);
        acc -= n64 * fixed_log2(n64);
    }
    acc >> LOG2_FRAC_BITS
}

/// Shannon cost (whole bits) and used-symbol count of the element-wise SUM of two
/// equal-length bin slices — the pairwise sibling of [`channel_bits`].
fn channel_bits_pair(a: &[u32], b: &[u32]) -> (u64, u64) {
    debug_assert_eq!(a.len(), b.len());
    let total: u64 = a
        .iter()
        .zip(b)
        .map(|(&x, &y)| u64::from(x) + u64::from(y))
        .sum();
    if total == 0 {
        return (0, 0);
    }
    let mut acc = total * fixed_log2(total);
    let mut used = 0u64;
    for (&x, &y) in a.iter().zip(b) {
        let s = u64::from(x) + u64::from(y);
        if s > 0 {
            acc -= s * fixed_log2(s);
            used += 1;
        }
    }
    (acc >> LOG2_FRAC_BITS, used)
}

/// Approximate the code-length header cost of a channel with `used` symbols. A
/// single-symbol (or empty) channel is transmitted as a zero-bit code, so it
/// costs ~nothing; otherwise each used symbol carries a small length field.
const fn header_bits(used: u64) -> u64 {
    if used <= 1 { 0 } else { 24 + used * 3 }
}

/// `log2(x)` for `x >= 1`, scaled by `2^LOG2_FRAC_BITS`, using only integer
/// arithmetic (a bit-by-bit fractional refinement). Deterministic across
/// platforms and `no_std`-safe.
pub(crate) fn fixed_log2(x: u64) -> u64 {
    if x <= 1 {
        return 0;
    }
    let int_part = x.ilog2(); // floor(log2(x))
    // Normalize the mantissa to [2^FRAC, 2^(FRAC+1)) i.e. [1, 2) in fixed point.
    let mut mantissa = if int_part <= LOG2_FRAC_BITS {
        x << (LOG2_FRAC_BITS - int_part)
    } else {
        x >> (int_part - LOG2_FRAC_BITS)
    };
    let mut frac = 0u64;
    let mut bit = 1u64 << (LOG2_FRAC_BITS - 1);
    let one = 1u64 << LOG2_FRAC_BITS;
    while bit != 0 {
        // Square the mantissa (still fixed point); it lands in [1, 4).
        mantissa = (mantissa * mantissa) >> LOG2_FRAC_BITS;
        if mantissa >= 2 * one {
            mantissa >>= 1; // renormalise to [1, 2)
            frac |= bit; // this fractional bit of log2 is set
        }
        bit >>= 1;
    }
    (u64::from(int_part) << LOG2_FRAC_BITS) | frac
}

#[cfg(test)]
mod tests {
    use super::{Histogram, LOG2_FRAC_BITS, channel_bits, fixed_log2, header_bits, shannon_bits};
    use crate::lossless::constants::{NUM_LENGTH_CODES, NUM_LITERAL_CODES};
    use proptest::prelude::*;

    #[test]
    fn fixed_log2_known_values() {
        let one = 1u64 << LOG2_FRAC_BITS;
        assert_eq!(fixed_log2(1), 0);
        assert_eq!(fixed_log2(2), one); // log2(2) = 1
        assert_eq!(fixed_log2(4), 2 * one); // log2(4) = 2
        assert_eq!(fixed_log2(256), 8 * one); // log2(256) = 8
        // log2(3) ≈ 1.5849625; in Q16 that rounds to 1.5849625 * 65536 = 103872.
        let approx = fixed_log2(3);
        assert!(approx.abs_diff(103_872) <= 2, "log2(3): {approx} vs 103872");
    }

    #[test]
    fn fixed_log2_is_monotone() {
        let mut prev = 0;
        for x in 1u64..=4096 {
            let v = fixed_log2(x);
            assert!(v >= prev, "log2 must not decrease at {x}");
            prev = v;
        }
    }

    #[test]
    fn single_symbol_channel_costs_zero() {
        // A channel with one used symbol is a zero-bit code: the solid-image
        // non-regression guarantee, proven at the cost-model level.
        let bins = {
            let mut b = vec![0u32; 256];
            b[42] = 1000;
            b
        };
        assert_eq!(channel_bits(&bins), (0, 1));
    }

    #[test]
    fn uniform_two_symbol_channel_costs_one_bit_each() {
        let bins = vec![5u32, 5];
        // 10 symbols, 1 bit each -> 10 bits of pixel data.
        assert_eq!(channel_bits(&bins).0, 10);
    }

    #[test]
    fn skewed_beats_uniform() {
        // A skewed distribution must estimate cheaper than a uniform one of the
        // same mass — the property that makes the cache/strategy choice sane.
        let uniform = channel_bits(&[8u32, 8, 8, 8]).0;
        let skewed = channel_bits(&[29u32, 1, 1, 1]).0;
        assert!(skewed < uniform, "skewed {skewed} !< uniform {uniform}");
    }

    #[test]
    fn literal_and_reference_bins_land_in_the_right_place() {
        let mut h = Histogram::new(NUM_LITERAL_CODES + NUM_LENGTH_CODES + 8);
        h.add_literal(0xAA_11_22_33); // green byte = 0x22
        h.add_length(3, 2);
        h.add_distance(5, 4);
        h.add_cache(7);
        assert_eq!(h.green()[0x22], 1);
        assert_eq!(h.green()[NUM_LITERAL_CODES + 3], 1);
        assert_eq!(h.green()[NUM_LITERAL_CODES + NUM_LENGTH_CODES + 7], 1);
        assert_eq!(h.dist()[5], 1);
        assert_eq!(h.red()[0x11], 1);
        assert_eq!(h.blue()[0x33], 1);
        assert_eq!(h.alpha()[0xAA], 1);
    }

    #[test]
    fn add_length_accumulates_extra_bits_additively() {
        // `+=` vs `*=`: one length token carrying 5 extra bits must contribute
        // exactly those 5 bits. A single green length bin is a zero-bit code
        // (used == 1) and every other channel is empty, so estimate_bits equals
        // the raw extra-bits total. `extra_bits` starts at 0, so `0 *= 5` would
        // wrongly yield 0.
        let mut h = Histogram::new(NUM_LITERAL_CODES + NUM_LENGTH_CODES);
        h.add_length(0, 5);
        assert_eq!(h.estimate_bits(), 5);
    }

    #[test]
    fn header_bits_multi_symbol_channel_cost() {
        // 0/1-symbol channels are free (zero-bit codes); multi-symbol channels
        // pay 24 + used*3. Exact values pin every arithmetic/relational mutant.
        assert_eq!(header_bits(0), 0);
        assert_eq!(header_bits(1), 0);
        assert_eq!(header_bits(2), 30); // 24 + 2*3
        assert_eq!(header_bits(5), 39); // 24 + 5*3
    }

    #[test]
    fn fixed_log2_large_argument_uses_high_branch() {
        let one = 1u64 << LOG2_FRAC_BITS;
        // x = 3 * 2^18 = 786432 has floor(log2 x) = 19 > LOG2_FRAC_BITS (16), so
        // the mantissa normalizes via the `x >> (int_part - LOG2_FRAC_BITS)`
        // branch (never reached by the <=4096 monotone test). log2(786432) =
        // 18 + log2(3) ≈ 19.585. The `-`→`+` mutant shifts by 35, collapsing the
        // mantissa to 0 and returning exactly 19<<16.
        let v = fixed_log2(786_432);
        assert!(v > 19 * one, "expected > 19.0 in Q16, got {v}");
        assert!(v < 20 * one, "expected < 20.0 in Q16, got {v}");
        assert!(v.abs_diff(1_283_520) <= 8, "log2(786432): {v} vs ~1283520");
    }

    proptest! {
        /// `shannon_bits(total, used_counts)` — fed the total and just the nonzero
        /// bins — must equal the pixel-data term of the full-slice `channel_bits`.
        #[test]
        fn shannon_bits_matches_channel_bits(bins in prop::collection::vec(0u32..=5000, 0..=300)) {
            let total: u64 = bins.iter().map(|&n| u64::from(n)).sum();
            let used: Vec<u32> = bins.iter().copied().filter(|&n| n > 0).collect();
            prop_assert_eq!(shannon_bits(total, &used), channel_bits(&bins).0);
        }

        /// The entropy estimate is a lower bound on a real fixed-length coding:
        /// `n` symbols over an alphabet of `k` used values never estimate above
        /// `n · ceil(log2 k)` bits (before headers/extra).
        #[test]
        fn entropy_is_a_sane_lower_bound(counts in prop::collection::vec(1u32..=50, 2..=16)) {
            let k = counts.len() as u64;
            let n: u64 = counts.iter().map(|&c| u64::from(c)).sum();
            let ceil_log2_k = u64::from(u64::BITS - (k - 1).leading_zeros());
            let (bits, used) = channel_bits(&counts);
            prop_assert_eq!(used, k);
            prop_assert!(bits <= n * ceil_log2_k, "{bits} > {n}*{ceil_log2_k}");
        }

        /// merged_estimate_bits equals building the merge then estimating, and is
        /// symmetric — the greedy merge cost is exact and order-independent.
        #[test]
        fn merged_estimate_bits_consistent_and_symmetric(
            greens_a in prop::collection::vec(0u32..8, 0..40),
            greens_b in prop::collection::vec(0u32..8, 0..40),
        ) {
            let alphabet = NUM_LITERAL_CODES + NUM_LENGTH_CODES + 4;
            let mut a = Histogram::new(alphabet);
            let mut b = Histogram::new(alphabet);
            for &g in &greens_a {
                a.add_literal(g << 8);
                a.add_length(g % 24, g % 3);
            }
            for &g in &greens_b {
                b.add_literal(g << 8);
                b.add_cache(u16::try_from(g % 4).unwrap());
                b.add_distance(g % 40, g % 5);
            }
            let mut merged = a.clone();
            merged.add_assign(&b);
            prop_assert_eq!(a.merged_estimate_bits(&b), merged.estimate_bits());
            prop_assert_eq!(a.merged_estimate_bits(&b), b.merged_estimate_bits(&a));
        }
    }
}
