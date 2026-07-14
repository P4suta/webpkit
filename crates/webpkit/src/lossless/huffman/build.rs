//! Length-limited canonical Huffman code-length assignment for the encoder.
//!
//! Given a symbol frequency histogram this produces a per-symbol *code length*
//! vector (0 = unused) whose lengths form a complete prefix code no deeper than a
//! caller-supplied limit. Only the multiset of lengths matters downstream:
//! [`crate::lossless::huffman::canonical`] turns lengths into canonical codes and the
//! decoder's [`crate::lossless::huffman::decode::HuffmanTable::build`] rebuilds the same
//! table from lengths alone, so this module never assigns codes itself.
//!
//! The depth limit (15 for the main channel codes, 7 for the code-length meta
//! alphabet) is enforced with libwebp's `count_min` clamp: build an optimal tree,
//! and if it is too deep raise a floor on every used weight and rebuild. Raising
//! the floor flattens the frequency distribution; once every weight is clamped to
//! the same value the tree is as balanced as possible (depth `ceil(log2 n)`),
//! which is well within both limits for our alphabets (`n <= 280`), so the loop is
//! guaranteed to terminate.

use crate::lossless::prelude::*;

/// Sentinel child/marker for a leaf node (a real symbol index is always small).
const NONE: usize = usize::MAX;

/// One node of the Huffman work tree, stored in a flat pool addressed by index.
///
/// A leaf has `symbol` set to its histogram index and both children `NONE`; an
/// internal node has `symbol == NONE` and `left`/`right` pointing into the pool.
struct Node {
    /// Total (possibly `count_min`-clamped) frequency under this node.
    count: u64,
    /// Left child pool index, or [`NONE`] for a leaf.
    left: usize,
    /// Right child pool index, or [`NONE`] for a leaf.
    right: usize,
    /// Histogram index for a leaf, or [`NONE`] for an internal node.
    symbol: usize,
}

/// Locate the two smallest active nodes, returning their *positions in `active`*.
///
/// Ordering is by `(count, pool index)`, so ties break deterministically on the
/// creation order of nodes (leaves precede internal nodes). `active` must hold at
/// least two entries.
fn two_minima(active: &[usize], pool: &[Node]) -> (usize, usize) {
    let cost = |idx: usize| (pool[idx].count, idx);
    let (mut min1, mut min2) = if cost(active[1]) < cost(active[0]) {
        (1usize, 0usize)
    } else {
        (0usize, 1usize)
    };
    for (pos, &idx) in active.iter().enumerate().skip(2) {
        let key = cost(idx);
        if key < cost(active[min1]) {
            min2 = min1;
            min1 = pos;
        } else if key < cost(active[min2]) {
            min2 = pos;
        }
    }
    (min1, min2)
}

/// Walk the tree rooted at `root` and record each leaf's depth as its code length.
///
/// Returns a vector of length `alphabet_size` (0 for symbols not in the tree).
fn assign_depths(pool: &[Node], root: usize, alphabet_size: usize) -> Vec<u32> {
    let mut lengths = vec![0u32; alphabet_size];
    let mut stack = vec![(root, 0u32)];
    while let Some((idx, depth)) = stack.pop() {
        let node = &pool[idx];
        if node.symbol == NONE {
            stack.push((node.left, depth + 1));
            stack.push((node.right, depth + 1));
        } else {
            lengths[node.symbol] = depth;
        }
    }
    lengths
}

/// Build an optimal Huffman tree over the used symbols with every weight floored
/// at `count_min`, and return per-symbol depths (code lengths).
///
/// Requires at least two used symbols (the caller handles the 0/1 cases); with
/// fewer, no meaningful tree exists.
fn huffman_code_lengths(histogram: &[u32], count_min: u64) -> Vec<u32> {
    let mut pool: Vec<Node> = Vec::new();
    for (symbol, &freq) in histogram.iter().enumerate() {
        if freq != 0 {
            pool.push(Node {
                count: u64::from(freq).max(count_min),
                left: NONE,
                right: NONE,
                symbol,
            });
        }
    }
    debug_assert!(pool.len() >= 2, "caller must handle the 0/1-symbol cases");

    let mut active: Vec<usize> = (0..pool.len()).collect();
    while active.len() > 1 {
        let (pos_a, pos_b) = two_minima(&active, &pool);
        let (a, b) = (active[pos_a], active[pos_b]);
        let parent = pool.len();
        pool.push(Node {
            count: pool[a].count + pool[b].count,
            left: a,
            right: b,
            symbol: NONE,
        });
        // Drop both merged roots (highest position first to keep the other valid)
        // and add the parent. Selection is order-independent, so `swap_remove`'s
        // reshuffling does not affect determinism.
        let (hi, lo) = (pos_a.max(pos_b), pos_a.min(pos_b));
        active.swap_remove(hi);
        active.swap_remove(lo);
        active.push(parent);
    }

    assign_depths(&pool, active[0], histogram.len())
}

/// Assign length-limited canonical code lengths for `histogram`.
///
/// Returns a vector the same length as `histogram`; entry `i` is the code length
/// for symbol `i` (0 for an unused symbol, i.e. `histogram[i] == 0`). Used
/// symbols receive lengths in `1..=limit`, and when more than one symbol is used
/// the lengths form a *complete* prefix code (Kraft equality holds).
///
/// A lone used symbol is given length 1 (0 means "unused", so it cannot be used
/// here); the serializer collapses such a single-symbol alphabet to zero emitted
/// bits per occurrence, matching the decoder's 0-bit leaf.
///
/// `limit` must be large enough to express a balanced tree over the used symbols
/// (`limit >= ceil(log2 n)`); the encoder always passes 15 or 7, which is ample
/// for the VP8L alphabets (`n <= 280`).
#[must_use]
pub(crate) fn build_code_lengths(histogram: &[u32], limit: u32) -> Vec<u32> {
    let num_used = histogram.iter().filter(|&&freq| freq != 0).count();
    let mut lengths = vec![0u32; histogram.len()];
    match num_used {
        0 => return lengths,
        1 => {
            if let Some(idx) = histogram.iter().position(|&freq| freq != 0) {
                lengths[idx] = 1;
            }
            return lengths;
        },
        _ => {},
    }

    // Optimal tree first; raise the weight floor and retry while too deep.
    let mut count_min = 1u64;
    loop {
        let candidate = huffman_code_lengths(histogram, count_min);
        if candidate.iter().copied().max().unwrap_or(0) <= limit {
            return candidate;
        }
        count_min <<= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::build_code_lengths;
    use crate::lossless::constants::HUFFMAN_TABLE_BITS;
    use crate::lossless::huffman::decode::HuffmanTable;

    /// The longest code length present (0 for an all-unused alphabet).
    fn max_length(lengths: &[u32]) -> u32 {
        lengths.iter().copied().max().unwrap_or(0)
    }

    /// The decoder must be able to rebuild a table from these lengths (this proves
    /// the lengths are a complete, prefix-free code). Only meaningful when at
    /// least one symbol is used.
    fn assert_decoder_accepts(lengths: &[u32]) {
        assert!(
            HuffmanTable::build(lengths, HUFFMAN_TABLE_BITS).is_some(),
            "decoder must accept lengths {lengths:?}"
        );
    }

    /// For a complete prefix code over more than one symbol, Kraft's inequality is
    /// an equality: `sum 2^(maxlen - len) == 2^maxlen`.
    fn assert_kraft_when_multi(lengths: &[u32]) {
        let used: Vec<u32> = lengths.iter().copied().filter(|&l| l != 0).collect();
        if used.len() <= 1 {
            return;
        }
        let max_len = used.iter().copied().max().unwrap();
        let sum: u64 = used.iter().map(|&l| 1u64 << (max_len - l)).sum();
        assert_eq!(
            sum,
            1u64 << max_len,
            "Kraft equality must hold for {lengths:?}"
        );
    }

    #[test]
    fn empty_alphabet_is_all_zero() {
        assert_eq!(build_code_lengths(&[0, 0, 0], 15), vec![0, 0, 0]);
        assert!(build_code_lengths(&[], 15).is_empty());
    }

    #[test]
    fn single_used_symbol_gets_length_one() {
        let mut hist = vec![0u32; 10];
        hist[3] = 42;
        let lengths = build_code_lengths(&hist, 15);
        let mut expected = vec![0u32; 10];
        expected[3] = 1;
        assert_eq!(lengths, expected);
        // A single-symbol alphabet still builds (as a 0-bit leaf in the decoder).
        assert_decoder_accepts(&lengths);
    }

    #[test]
    fn two_used_symbols_each_get_length_one() {
        let lengths = build_code_lengths(&[5, 9], 15);
        assert_eq!(lengths, vec![1, 1]);
        assert_kraft_when_multi(&lengths);
        assert_decoder_accepts(&lengths);
    }

    #[test]
    fn several_histograms_produce_decodable_codes() {
        let cases: [&[u32]; 5] = [
            &[1, 1, 1, 1],
            &[10, 1, 1, 1, 1],
            &[100, 50, 25, 12, 6, 3, 1],
            &[3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3],
            &[0, 7, 0, 0, 2, 5, 0, 9, 1, 0],
        ];
        for hist in cases {
            let lengths = build_code_lengths(hist, 15);
            assert_eq!(lengths.len(), hist.len());
            for (i, &l) in lengths.iter().enumerate() {
                assert_eq!(l == 0, hist[i] == 0, "used-ness must match for {hist:?}");
            }
            assert!(max_length(&lengths) <= 15, "must respect the 15-bit limit");
            assert_kraft_when_multi(&lengths);
            assert_decoder_accepts(&lengths);
        }
    }

    #[test]
    fn fibonacci_weights_are_depth_limited() {
        // Fibonacci frequencies yield a maximally unbalanced ("caterpillar")
        // Huffman tree whose natural depth is ~num_symbols - 1, far past 15. The
        // count_min clamp must flatten it to <= 15 while staying decodable.
        let mut hist = Vec::new();
        let (mut a, mut b) = (1u32, 1u32);
        for _ in 0..40 {
            hist.push(a);
            let next = a.saturating_add(b);
            a = b;
            b = next;
        }
        let lengths = build_code_lengths(&hist, 15);
        assert!(max_length(&lengths) <= 15, "count_min must cap depth at 15");
        assert_kraft_when_multi(&lengths);
        assert_decoder_accepts(&lengths);
    }

    #[test]
    fn skewed_nineteen_entries_respect_limit_seven() {
        // The 19-symbol code-length meta alphabet, skewed enough that the
        // unconstrained tree would exceed 7, encoded under the 7-bit meta limit.
        let hist: [u32; 19] = [
            1000, 500, 250, 120, 60, 30, 15, 8, 4, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1,
        ];
        let lengths = build_code_lengths(&hist, 7);
        assert!(
            max_length(&lengths) <= 7,
            "must respect the 7-bit meta limit"
        );
        assert_kraft_when_multi(&lengths);
        assert_decoder_accepts(&lengths);
    }
}

#[cfg(test)]
mod proptests {
    use super::build_code_lengths;
    use crate::lossless::constants::HUFFMAN_TABLE_BITS;
    use crate::lossless::huffman::decode::HuffmanTable;
    use proptest::prelude::*;

    proptest! {
        /// Whatever the histogram, the output is well-formed: unused symbols get
        /// length 0, used symbols land in 1..=15, more-than-one-symbol alphabets
        /// satisfy Kraft equality, and the decoder rebuilds a table from them.
        #[test]
        fn produces_valid_length_limited_codes(
            freqs in proptest::collection::vec(0u32..=4096, 1..=280)
        ) {
            let lengths = build_code_lengths(&freqs, 15);
            prop_assert_eq!(lengths.len(), freqs.len());

            let mut used = 0usize;
            for (i, &l) in lengths.iter().enumerate() {
                if freqs[i] == 0 {
                    prop_assert_eq!(l, 0, "unused symbol must have length 0");
                } else {
                    prop_assert!((1..=15).contains(&l), "used length must be in 1..=15");
                    used += 1;
                }
            }

            if used > 1 {
                let max_len = lengths.iter().copied().max().unwrap_or(0);
                let sum: u64 = lengths
                    .iter()
                    .filter(|&&l| l != 0)
                    .map(|&l| 1u64 << (max_len - l))
                    .sum();
                prop_assert_eq!(sum, 1u64 << max_len, "Kraft equality must hold");
            }

            if used >= 1 {
                prop_assert!(
                    HuffmanTable::build(&lengths, HUFFMAN_TABLE_BITS).is_some(),
                    "decoder must accept the generated lengths"
                );
            }
        }
    }
}
