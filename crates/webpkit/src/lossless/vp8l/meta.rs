//! Meta-Huffman entropy-image planning (forward/emit side).
//!
//! Partitions the coded image into a grid of `2^bits`-sized blocks, builds one
//! symbol histogram per block, then greedily merges blocks into a small number of
//! Huffman groups that minimize the total estimated bits — the same
//! [`Histogram::estimate_bits`] cost model that already chooses the color-cache
//! size. The resulting [`MetaPlan`] drives the multi-group emit in
//! [`crate::lossless::vp8l::encode`]; the decoder's `read_huffman_codes` / `select_group`
//! read exactly what we write (this module produces, never consumes).

use alloc::collections::BinaryHeap;
use core::cmp::Reverse;

use crate::lossless::constants::{ALPHABET_SIZE, MIN_TRANSFORM_BITS, subsample_size};
use crate::lossless::histogram::Histogram;
use crate::lossless::prelude::*;
use crate::lossless::vp8l::backref::{Resolved, Token, resolve};
use crate::lossless::work::work;

/// The decoder's precision field is `read_bits(3) + 2`, so `bits` is in `2..=9`.
const MAX_META_BITS: u32 = 9;
/// Hard cap on the initial entropy-block grid, bounding the O(n^2) greedy merge.
const MAX_ENTROPY_BLOCKS: u32 = 256;
/// Group ids are packed `group << 8` into the entropy image's green byte, so they
/// stay < 256 (the decoder itself supports up to 65535 via green+red).
const MAX_GROUPS: usize = 256;

/// A resolved meta-Huffman plan: the entropy precision, the per-block dense group
/// id (emitted as `group << 8`), and each group's summed histogram (the input to
/// its five prefix codes).
#[cfg_attr(test, derive(PartialEq, Debug))]
pub(crate) struct MetaPlan {
    pub(crate) bits: u32,
    pub(crate) entropy_xsize: u32,
    pub(crate) groups: Vec<u16>,
    pub(crate) group_histograms: Vec<Histogram>,
}

/// Plan a meta-Huffman grouping for one token stream, or `None` when it cannot
/// help: a giant image whose grid overflows even at the coarsest precision, a
/// single-block grid, `<= 1` non-empty block, or a merge that collapses to one
/// group.
#[expect(
    clippy::similar_names,
    reason = "`entropy_xsize`/`entropy_ysize` are the standard grid-dimension names \
              (`entropy_xsize` is also the emitted `MetaPlan` field); the x/y pair is \
              intentional and clearer than renaming one side"
)]
pub(crate) fn plan(
    tokens: &[Token],
    pixels: &[u32],
    width: u32,
    ysize: u32,
    cache_bits: u32,
) -> Option<MetaPlan> {
    let bits = choose_bits(width, ysize)?;
    let entropy_xsize = subsample_size(width, bits);
    let entropy_ysize = subsample_size(ysize, bits);
    let num_blocks = (entropy_xsize * entropy_ysize) as usize;
    if num_blocks <= 1 {
        return None;
    }
    let cache_codes = if cache_bits > 0 {
        1usize << cache_bits
    } else {
        0
    };
    let green_alphabet = ALPHABET_SIZE[0] + cache_codes;
    let block_histograms = build_block_histograms(
        tokens,
        pixels,
        width,
        cache_bits,
        bits,
        entropy_xsize,
        green_alphabet,
        num_blocks,
    );
    greedy_cluster(block_histograms, bits, entropy_xsize)
}

/// The finest precision whose block grid stays within [`MAX_ENTROPY_BLOCKS`], or
/// `None` if even `bits = 9` overflows it (a very large image).
fn choose_bits(width: u32, ysize: u32) -> Option<u32> {
    let mut bits = MIN_TRANSFORM_BITS;
    loop {
        let blocks =
            u64::from(subsample_size(width, bits)) * u64::from(subsample_size(ysize, bits));
        if blocks <= u64::from(MAX_ENTROPY_BLOCKS) {
            return Some(bits);
        }
        if bits >= MAX_META_BITS {
            return None;
        }
        bits += 1;
    }
}

/// One histogram per entropy block, each unit bucketed by its START-position block
/// (byte-identical to the decoder's `select_group`); a copy lands wholly in its
/// start block, empty blocks keep a zero histogram.
#[expect(
    clippy::too_many_arguments,
    reason = "the block grid geometry (bits, entropy_xsize, green_alphabet, num_blocks) \
              is pre-derived once by `plan`; threading it in avoids recomputing it here"
)]
#[expect(
    clippy::cast_possible_truncation,
    reason = "`pos < width * ysize <= 2^28`, so `pos as u32` is value-preserving (the \
              same narrowing the decoder's `decode_one` does at decode.rs:504)"
)]
fn build_block_histograms(
    tokens: &[Token],
    pixels: &[u32],
    width: u32,
    cache_bits: u32,
    bits: u32,
    entropy_xsize: u32,
    green_alphabet: usize,
    num_blocks: usize,
) -> Vec<Histogram> {
    let mut hists: Vec<Histogram> = (0..num_blocks)
        .map(|_| Histogram::new(green_alphabet))
        .collect();
    resolve(tokens, pixels, cache_bits, width, |pos, unit| {
        let x = pos as u32 % width;
        let y = pos as u32 / width;
        let block = ((y >> bits) * entropy_xsize + (x >> bits)) as usize;
        match unit {
            Resolved::Literal(argb) => hists[block].add_literal(argb),
            Resolved::Copy {
                length_symbol,
                length_extra,
                dist_symbol,
                dist_extra,
            } => {
                hists[block].add_length(length_symbol, length_extra.1);
                hists[block].add_distance(dist_symbol, dist_extra.1);
            },
            Resolved::Cache(key) => hists[block].add_cache(key),
        }
    });
    hists
}

/// Greedily merge non-empty block histograms into groups that minimize total
/// estimated bits, then relabel to dense ids. `None` if <= 1 group results.
#[expect(
    clippy::needless_range_loop,
    reason = "the pairwise-delta recompute walks index `j` to address the flattened \
              upper-triangular `delta[lo * n + hi]` matrix alongside `active`/`hist`; a \
              plain iterator does not express the triangular addressing"
)]
#[expect(
    clippy::cast_possible_truncation,
    reason = "cluster count `n <= num_blocks <= MAX_ENTROPY_BLOCKS` (256), so both the \
              dense group ids and the `i`/`j` heap indices fit `u16` value-preservingly"
)]
fn greedy_cluster(
    block_histograms: Vec<Histogram>,
    bits: u32,
    entropy_xsize: u32,
) -> Option<MetaPlan> {
    let num_blocks = block_histograms.len();
    // Seed one cluster per NON-EMPTY block (a block is empty iff its green channel
    // is all zero — every unit type increments some green bin). `cluster_of[b]`
    // tracks the surviving cluster of block b (None for empty blocks -> group 0).
    let mut cluster_of: Vec<Option<usize>> = vec![None; num_blocks];
    let mut hist: Vec<Histogram> = Vec::new();
    let mut src_block: Vec<usize> = Vec::new();
    for (b, h) in block_histograms.into_iter().enumerate() {
        if h.green().iter().all(|&n| n == 0) {
            continue;
        }
        cluster_of[b] = Some(hist.len());
        src_block.push(b);
        hist.push(h);
    }
    let n = hist.len();
    if n <= 1 {
        return None;
    }

    let mut est: Vec<u64> = hist.iter().map(Histogram::estimate_bits).collect();
    let mut active: Vec<bool> = vec![true; n];
    let mut active_count = n;
    // Upper-triangular pairwise merge deltas (delta = merged - sep; negative saves).
    let mut delta: Vec<i64> = vec![0; n * n];
    for i in 0..n {
        for j in (i + 1)..n {
            delta[i * n + j] = merge_delta(&hist[i], &hist[j], est[i], est[j]);
        }
    }
    // Lazy-deletion min-heap over every active pair's `(delta, i, j)` (i < j). The
    // heap orders by `(delta, i, j)` ascending — exactly the full-triangle scan's
    // lexicographic tie-break — so its smallest CURRENT entry is the very pair the
    // reference merges. A merge only pushes the O(n) recomputed column-`a` pairs;
    // entries it makes stale (an overwritten delta or a retired endpoint) are
    // skipped on pop. The whole clustering is thus O(n^2 log n) rather than the
    // per-merge full-row rescan the naive cached-minima form degrades to. Indices
    // are `u16` (n <= 256) to keep each O(n^2) entry at 16 bytes, not 24.
    let mut heap: BinaryHeap<Reverse<(i64, u16, u16)>> = BinaryHeap::with_capacity(n * n / 2);
    for i in 0..n {
        for j in (i + 1)..n {
            heap.push(Reverse((delta[i * n + j], i as u16, j as u16)));
        }
    }

    loop {
        // Pop the lexicographically smallest CURRENT pair, discarding stale entries
        // (a retired endpoint, or a delta a later merge overwrote). Every active
        // pair still carries an entry with its live delta, so the first entry that
        // validates is the true `(delta, i, j)` minimum.
        let best = loop {
            let Some(Reverse((d, pi, pj))) = heap.pop() else {
                break None;
            };
            work!(ClusterScan);
            let (i, j) = (usize::from(pi), usize::from(pj));
            if !active[i] || !active[j] || delta[i * n + j] != d {
                continue;
            }
            break Some((d, i, j));
        };
        let Some((d, a, b)) = best else { break };
        // Stop once no merge helps AND we are within the group cap; otherwise force
        // the least-harmful merge until the cap is met.
        if d >= 0 && active_count <= MAX_GROUPS {
            break;
        }
        // Merge b into a (a < b). split_at_mut avoids the aliasing borrow.
        let (left, right) = hist.split_at_mut(b);
        left[a].add_assign(&right[0]);
        est[a] = left[a].estimate_bits();
        active[b] = false;
        active_count -= 1;
        for slot in &mut cluster_of {
            if *slot == Some(b) {
                *slot = Some(a);
            }
        }
        // Only row/col a changed; recompute its deltas against every active cluster
        // and push the fresh entries (the pre-merge ones are now stale).
        for j in 0..n {
            if j == a || !active[j] {
                continue;
            }
            let (lo, hi) = if a < j { (a, j) } else { (j, a) };
            let merged_delta = merge_delta(&hist[lo], &hist[hi], est[lo], est[hi]);
            delta[lo * n + hi] = merged_delta;
            heap.push(Reverse((merged_delta, lo as u16, hi as u16)));
        }
    }

    // Relabel surviving clusters to dense ids 0..k-1, ordered by smallest member
    // block (deterministic; guarantees the max id appears -> decoder num_groups = k).
    let mut actives: Vec<usize> = (0..n).filter(|&i| active[i]).collect();
    actives.sort_by_key(|&i| src_block[i]);
    let k = actives.len();
    if k <= 1 {
        return None;
    }
    let mut new_id: Vec<u16> = vec![0; n];
    for (id, &old) in actives.iter().enumerate() {
        new_id[old] = id as u16;
    }
    let mut groups: Vec<u16> = vec![0; num_blocks];
    for (b, slot) in cluster_of.iter().enumerate() {
        if let Some(c) = *slot {
            groups[b] = new_id[c];
        }
    }
    let mut tagged: Vec<(u16, Histogram)> = hist
        .into_iter()
        .enumerate()
        .filter(|(old, _)| active[*old])
        .map(|(old, h)| (new_id[old], h))
        .collect();
    tagged.sort_by_key(|&(id, _)| id);
    let group_histograms: Vec<Histogram> = tagged.into_iter().map(|(_, h)| h).collect();

    Some(MetaPlan {
        bits,
        entropy_xsize,
        groups,
        group_histograms,
    })
}

/// Merge cost of two clusters: bits of the merged histogram minus the separate
/// bits (negative means merging saves, chiefly by dropping one group's channel
/// headers). Estimate bits are bounded far below `i64::MAX`.
#[expect(
    clippy::cast_possible_wrap,
    reason = "estimate bits are Shannon costs bounded far below `i64::MAX`, so the \
              `u64 as i64` casts never wrap (the same cast lz77.rs uses for its \
              distance arithmetic)"
)]
fn merge_delta(a: &Histogram, b: &Histogram, est_a: u64, est_b: u64) -> i64 {
    work!(ClusterEstimate);
    a.merged_estimate_bits(b) as i64 - est_a as i64 - est_b as i64
}

/// Reference full-scan clustering: the pre-optimization body of
/// [`greedy_cluster`], kept verbatim so a proptest can pin the incremental
/// per-row-minima fast path to byte-identical output.
#[cfg(test)]
#[expect(
    clippy::needless_range_loop,
    reason = "mirrors greedy_cluster: the triangular delta addressing is index-based"
)]
#[expect(
    clippy::cast_possible_truncation,
    reason = "dense group ids are `< MAX_GROUPS` (256), so `id as u16` is value-preserving"
)]
fn greedy_cluster_reference(
    block_histograms: Vec<Histogram>,
    bits: u32,
    entropy_xsize: u32,
) -> Option<MetaPlan> {
    let num_blocks = block_histograms.len();
    let mut cluster_of: Vec<Option<usize>> = vec![None; num_blocks];
    let mut hist: Vec<Histogram> = Vec::new();
    let mut src_block: Vec<usize> = Vec::new();
    for (b, h) in block_histograms.into_iter().enumerate() {
        if h.green().iter().all(|&n| n == 0) {
            continue;
        }
        cluster_of[b] = Some(hist.len());
        src_block.push(b);
        hist.push(h);
    }
    let n = hist.len();
    if n <= 1 {
        return None;
    }

    let mut est: Vec<u64> = hist.iter().map(Histogram::estimate_bits).collect();
    let mut active: Vec<bool> = vec![true; n];
    let mut active_count = n;
    let mut delta: Vec<i64> = vec![0; n * n];
    for i in 0..n {
        for j in (i + 1)..n {
            delta[i * n + j] = merge_delta(&hist[i], &hist[j], est[i], est[j]);
        }
    }

    loop {
        let mut best: Option<(i64, usize, usize)> = None;
        for i in 0..n {
            if !active[i] {
                continue;
            }
            for j in (i + 1)..n {
                if !active[j] {
                    continue;
                }
                let d = delta[i * n + j];
                if best.is_none_or(|(bd, _, _)| d < bd) {
                    best = Some((d, i, j));
                }
            }
        }
        let Some((d, a, b)) = best else { break };
        if d >= 0 && active_count <= MAX_GROUPS {
            break;
        }
        let (left, right) = hist.split_at_mut(b);
        left[a].add_assign(&right[0]);
        est[a] = left[a].estimate_bits();
        active[b] = false;
        active_count -= 1;
        for slot in &mut cluster_of {
            if *slot == Some(b) {
                *slot = Some(a);
            }
        }
        for j in 0..n {
            if j == a || !active[j] {
                continue;
            }
            let (lo, hi) = if a < j { (a, j) } else { (j, a) };
            delta[lo * n + hi] = merge_delta(&hist[lo], &hist[hi], est[lo], est[hi]);
        }
    }

    let mut actives: Vec<usize> = (0..n).filter(|&i| active[i]).collect();
    actives.sort_by_key(|&i| src_block[i]);
    let k = actives.len();
    if k <= 1 {
        return None;
    }
    let mut new_id: Vec<u16> = vec![0; n];
    for (id, &old) in actives.iter().enumerate() {
        new_id[old] = id as u16;
    }
    let mut groups: Vec<u16> = vec![0; num_blocks];
    for (b, slot) in cluster_of.iter().enumerate() {
        if let Some(c) = *slot {
            groups[b] = new_id[c];
        }
    }
    let mut tagged: Vec<(u16, Histogram)> = hist
        .into_iter()
        .enumerate()
        .filter(|(old, _)| active[*old])
        .map(|(old, h)| (new_id[old], h))
        .collect();
    tagged.sort_by_key(|&(id, _)| id);
    let group_histograms: Vec<Histogram> = tagged.into_iter().map(|(_, h)| h).collect();

    Some(MetaPlan {
        bits,
        entropy_xsize,
        groups,
        group_histograms,
    })
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::cast_possible_wrap,
        reason = "the estimate-bits assertions compare small entropy costs via i64; the u64 \
                  bit counts are far below i64::MAX so the cast cannot wrap"
    )]

    use super::{
        ALPHABET_SIZE, Histogram, MAX_GROUPS, build_block_histograms, choose_bits, greedy_cluster,
        greedy_cluster_reference, merge_delta, plan, subsample_size,
    };
    use crate::lossless::vp8l::backref::parse;
    use crate::lossless::vp8l::encode::RefModel;
    use proptest::prelude::*;

    /// A valid (width, height, pixels) image: `pixels.len()` == width * height.
    fn image() -> impl Strategy<Value = (u32, u32, Vec<u32>)> {
        (1u32..12, 1u32..12).prop_flat_map(|(w, h)| {
            prop::collection::vec(any::<u32>(), (w * h) as usize).prop_map(move |px| (w, h, px))
        })
    }

    /// A (width, height, pixels) image spanning the clustering edge cases: pure
    /// noise, a low-entropy palette (alpha present and absent), and solid fills —
    /// the inputs whose block histograms drive the greedy merge sequence.
    fn clustering_image() -> impl Strategy<Value = (u32, u32, Vec<u32>)> {
        const PALETTE: [u32; 4] = [0xff00_0000, 0xffff_ffff, 0x8012_3456, 0x00ab_cdef];
        (1u32..16, 1u32..16).prop_flat_map(|(w, h)| {
            let n = (w * h) as usize;
            prop_oneof![
                prop::collection::vec(any::<u32>(), n),
                prop::collection::vec(0usize..4, n)
                    .prop_map(|ix| ix.into_iter().map(|i| PALETTE[i]).collect::<Vec<u32>>()),
                any::<u32>().prop_map(move |c| vec![c; n]),
            ]
            .prop_map(move |px| (w, h, px))
        })
    }

    #[test]
    fn plan_none_for_single_pixel() {
        let pixels = [0xff00_0000u32];
        let tokens = parse(&pixels, true);
        assert!(plan(&tokens, &pixels, 1, 1, 0).is_none());
    }

    #[test]
    fn plan_none_for_solid_image() {
        let pixels = vec![0xff11_2233u32; 64];
        let tokens = parse(&pixels, true);
        assert!(plan(&tokens, &pixels, 8, 8, 0).is_none());
    }

    #[test]
    fn choose_bits_picks_finest_within_block_cap() {
        // 8x8 fits at the coarsest precision: ceil(8/4)^2 = 2*2 = 4 <= 256 -> bits 2.
        assert_eq!(choose_bits(8, 8), Some(2));
        // 128x128 forces exactly one step up: at bits 2 the grid is 32*32 = 1024 > 256
        // (so the loop must NOT stop), and at bits 3 it is 16*16 = 256 <= 256 -> bits 3.
        // This one assertion pins four choose_bits mutants: `*`->`+` (sum 32+32 = 64
        // <= 256 would wrongly stop at 2), `*`->`/` (32/32 = 1 <= 256 would too),
        // `>=`->`<` (would return None at bits 2), and `+=`->`-=` (would drive `bits`
        // below 0 and panic on the shift/underflow instead of ever reaching 3).
        assert_eq!(choose_bits(128, 128), Some(3));
    }

    #[test]
    fn merge_delta_is_merged_minus_separate() {
        // Two DIFFERENT non-empty histograms: each estimates > 0 bits, and their
        // merged estimate differs from the separate sum by a value that is neither 0
        // nor 1. `merged_estimate_bits` is computed here directly (it is not the
        // mutated function), so it is a faithful independent oracle for `merge_delta`.
        let mut a = Histogram::new(ALPHABET_SIZE[0]);
        let mut b = Histogram::new(ALPHABET_SIZE[0]);
        for g in 0u32..20 {
            a.add_literal(g << 8);
            b.add_literal((g + 40) << 8);
        }
        let est_a = a.estimate_bits();
        let est_b = b.estimate_bits();
        let expected = a.merged_estimate_bits(&b) as i64 - est_a as i64 - est_b as i64;
        // Guarantee the const/sign mutants all diverge: `-> 0` and `-> 1` differ
        // because expected is neither; the two `-`->`+` mutants differ because they
        // add 2*est_a resp. 2*est_b, both strictly positive.
        assert!(est_a > 0 && est_b > 0, "both operands must cost bits");
        assert!(
            expected != 0 && expected != 1,
            "delta {expected} must be distinct from the constant mutants"
        );
        assert_eq!(merge_delta(&a, &b, est_a, est_b), expected);
    }

    #[test]
    fn active_count_decrement_gates_forced_merge() {
        // 256 non-empty blocks, of which ONLY blocks 0 and 1 are worth merging.
        let mut blocks = Vec::with_capacity(256);
        // Blocks 0 and 1: identical two-symbol histograms. Merging them replaces two
        // multi-symbol channel headers with one, so the merge delta is negative.
        for _ in 0..2 {
            let mut h = Histogram::new(ALPHABET_SIZE[0]);
            h.add_literal(0x0000_0100); // green symbol 1
            h.add_literal(0x0000_0200); // green symbol 2
            blocks.push(h);
        }
        // Blocks 2..256: distinct single-symbol histograms. A one-symbol channel is a
        // zero-bit code, so each estimates 0 bits and every remaining merge (among
        // them, or with the 0/1 cluster) only adds bits -> delta >= 0.
        for s in 2u32..256 {
            let mut h = Histogram::new(ALPHABET_SIZE[0]);
            h.add_literal(s << 8);
            blocks.push(h);
        }
        assert_eq!(blocks.len(), 256);
        // Real: the single beneficial merge fires (256 -> 255 active), then every
        // delta is >= 0 while active_count (255) <= MAX_GROUPS (256), so clustering
        // stops at exactly 255 groups. The `-=`->`+=` mutant instead GROWS
        // active_count past MAX_GROUPS after that first merge, so the
        // `d >= 0 && active_count <= MAX_GROUPS` stop can never hold again and it
        // force-merges every cluster into one -> greedy_cluster returns None.
        let clustered = greedy_cluster(blocks, 2, 16)
            .expect("256 non-empty blocks with one beneficial merge yield 255 groups");
        assert!(clustered.group_histograms.len() <= MAX_GROUPS);
        assert_eq!(clustered.group_histograms.len(), 255);
    }

    proptest! {
        /// The incremental per-row-minima `greedy_cluster` returns a byte-identical
        /// plan to the full-scan `greedy_cluster_reference` across noise, palette,
        /// and solid images at every entropy precision and cache size — the same
        /// merge sequence, groups, and group histograms.
        #[test]
        fn greedy_cluster_matches_reference(
            (w, h, pixels) in clustering_image(),
            bits in 2u32..=4,
            cache_bits in 0u32..4,
        ) {
            let entropy_xsize = subsample_size(w, bits);
            let num_blocks = (entropy_xsize * subsample_size(h, bits)) as usize;
            let cache_codes = if cache_bits > 0 { 1usize << cache_bits } else { 0 };
            let green_alphabet = ALPHABET_SIZE[0] + cache_codes;
            let tokens = parse(&pixels, true);
            let blocks = build_block_histograms(
                &tokens, &pixels, w, cache_bits, bits, entropy_xsize, green_alphabet, num_blocks,
            );
            let fast = greedy_cluster(blocks.clone(), bits, entropy_xsize);
            let reference = greedy_cluster_reference(blocks, bits, entropy_xsize);
            prop_assert_eq!(fast, reference);
        }

        /// The per-block histograms partition the single whole-image histogram:
        /// their element-wise sum equals RefModel::histogram (same cache replay).
        #[test]
        fn block_histograms_sum_to_single((w, h, pixels) in image(), cache_bits in 0u32..4) {
            let bits = 2u32;
            let entropy_xsize = subsample_size(w, bits);
            let num_blocks = (entropy_xsize * subsample_size(h, bits)) as usize;
            let cache_codes = if cache_bits > 0 { 1usize << cache_bits } else { 0 };
            let green_alphabet = ALPHABET_SIZE[0] + cache_codes;
            let tokens = parse(&pixels, true);
            let blocks = build_block_histograms(
                &tokens, &pixels, w, cache_bits, bits, entropy_xsize, green_alphabet, num_blocks,
            );
            let mut sum = Histogram::new(green_alphabet);
            for b in &blocks {
                sum.add_assign(b);
            }
            let model = RefModel::new(&tokens, &pixels, w);
            prop_assert_eq!(sum, model.histogram(cache_bits));
        }

        /// Planning is deterministic: identical inputs give an identical plan.
        #[test]
        fn plan_is_deterministic((w, h, pixels) in image(), cache_bits in 0u32..3) {
            let tokens = parse(&pixels, true);
            prop_assert_eq!(plan(&tokens, &pixels, w, h, cache_bits), plan(&tokens, &pixels, w, h, cache_bits));
        }

        /// When a plan exists, group ids are dense 0..k-1 (the max id appears, so
        /// the decoder's num_groups = max+1 = k), all in-range, and <= MAX_GROUPS.
        #[test]
        fn plan_groups_are_dense((w, h, pixels) in image(), cache_bits in 0u32..3) {
            let tokens = parse(&pixels, true);
            if let Some(p) = plan(&tokens, &pixels, w, h, cache_bits) {
                let k = p.group_histograms.len();
                prop_assert!((2..=MAX_GROUPS).contains(&k));
                let maxid = p.groups.iter().copied().max().unwrap();
                prop_assert_eq!(usize::from(maxid) + 1, k);
                prop_assert!(p.groups.iter().all(|&g| usize::from(g) < k));
                prop_assert_eq!(p.groups.len(), (p.entropy_xsize * subsample_size(h, p.bits)) as usize);
            }
        }
    }
}
