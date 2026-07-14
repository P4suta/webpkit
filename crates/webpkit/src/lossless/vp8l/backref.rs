//! LZ77 back-reference production: the encoder-side counterpart to the decoder
//! pixel loop in [`crate::lossless::vp8l::decode`].
//!
//! [`parse`] turns a pixel stream into a cache-agnostic [`Token`] list (literals
//! and copies) using a hash-chain match finder with one-step lazy matching.
//! [`resolve`] then walks that list once, applying an optional color cache and
//! computing the prefix symbols for each emitted unit — the single place the
//! per-pixel cache-insertion order lives, so the histogram pass and the emit
//! pass (both in [`crate::lossless::vp8l::encode`]) can never disagree with the decoder.
#![allow(
    clippy::cast_possible_truncation,
    reason = "cache keys are < 1<<cache_bits (<= 2047, fit u16), copy lengths are \
              <= MAX_COPY_LENGTH and distances <= WINDOW_SIZE, and pixel positions are \
              bounded by width*height (each dimension <= 2^14, so the count is <= 2^28 \
              < u32::MAX); every narrowing cast here is therefore value-preserving"
)]

use crate::lossless::color_cache::ColorCache;
use crate::lossless::constants::{
    HASH_BITS_LZ77, MAX_CHAIN, MAX_COPY_LENGTH, MIN_MATCH, NUM_DISTANCE_CODES, NUM_LITERAL_CODES,
    WINDOW_SIZE,
};
use crate::lossless::histogram::{Histogram, fixed_log2};
use crate::lossless::lz77::{PlaneCodeMap, prefix_encode};
use crate::lossless::prelude::*;
use crate::lossless::work::work;

/// A parsed reference: either a literal pixel or a back-reference copy. Cache
/// references are not decided here — they are applied later by [`resolve`] once a
/// cache size is chosen, so the parse is independent of the cache.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Token {
    /// A single ARGB pixel coded literally.
    Literal(u32),
    /// Copy `length` pixels from `distance` pixels back (`distance >= 1`).
    Copy {
        /// Run length in pixels (`MIN_MATCH..=MAX_COPY_LENGTH`).
        length: u32,
        /// Backward distance in pixels (`1..=WINDOW_SIZE`).
        distance: u32,
    },
}

/// A token resolved to the concrete symbols the bitstream carries, with the
/// color cache applied. Extra-bit fields are `(value, bit_count)`.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Resolved {
    /// A literal ARGB pixel (green/red/blue/alpha symbols).
    Literal(u32),
    /// A back-reference: a green length code plus a distance code, each with
    /// their prefix extra bits.
    Copy {
        /// Length prefix symbol (green code `256 + length_symbol`).
        length_symbol: u32,
        /// Length `(extra_value, extra_bits)`.
        length_extra: (u32, u32),
        /// Distance prefix symbol.
        dist_symbol: u32,
        /// Distance `(extra_value, extra_bits)`.
        dist_extra: (u32, u32),
    },
    /// A color-cache reference (green code `280 + key`).
    Cache(u16),
}

/// Parse `pixels` into tokens. With `use_lz77` cleared every pixel becomes a
/// literal (the baseline that keeps solid images at zero pixel bits); with it
/// set, a hash-chain finder emits greedy matches refined by one-step lazy
/// lookahead.
pub(crate) fn parse(pixels: &[u32], use_lz77: bool) -> Vec<Token> {
    if !use_lz77 || pixels.len() < MIN_MATCH as usize {
        return pixels.iter().map(|&p| Token::Literal(p)).collect();
    }
    let chain = HashChain::build(pixels);
    parse_with_chain(pixels, &chain)
}

/// Greedy LZ77 [`parse`] that also hands back the [`HashChain`] it built, so a
/// following [`parse_optimal`] over the same pixels can reuse it instead of
/// rebuilding the identical (pure-function-of-`pixels`) chain. The chain is a pure
/// function of `pixels`, so whether the DP reuses this one or builds its own it
/// sees a bit-identical structure and emits byte-identical tokens; sharing merely
/// elides the redundant second `O(n)` build.
pub(crate) fn parse_lz77(pixels: &[u32]) -> (Vec<Token>, HashChain) {
    let chain = HashChain::build(pixels);
    // For inputs too short to hold any match, `parse_with_chain` still yields the
    // all-literal stream (the loop finds nothing); the tiny chain is returned
    // unused so callers get a uniform shape.
    let tokens = if pixels.len() < MIN_MATCH as usize {
        pixels.iter().map(|&p| Token::Literal(p)).collect()
    } else {
        parse_with_chain(pixels, &chain)
    };
    (tokens, chain)
}

/// The greedy one-step-lazy match loop of [`parse`], factored out so the same code
/// serves both the standalone [`parse`] (which builds its own chain) and
/// [`parse_lz77`] (which returns the chain for reuse). Given a prebuilt `chain`,
/// this is a pure function of `pixels`, so both entry points yield identical tokens.
fn parse_with_chain(pixels: &[u32], chain: &HashChain) -> Vec<Token> {
    let n = pixels.len();
    let mut tokens = Vec::new();
    let mut i = 0usize;
    // Invariant: `here` is the match at position `i`. Coding a literal advances by
    // one, so the lazy lookahead's `find(i + 1)` becomes the next iteration's
    // `find(i)`: carry it instead of recomputing. `find` is a pure function of the
    // chain, so the carried value is bit-identical to a fresh call — the token
    // stream (hence the emitted bytes) is unchanged, only the redundant second
    // `find` per position is removed.
    let mut here = chain.find(pixels, i);
    while i < n {
        let ahead = if i + 1 < n {
            chain.find(pixels, i + 1)
        } else {
            None
        };
        // Lazy: if the next position begins a strictly longer match, defer by
        // coding a literal here so the longer copy can be taken next.
        let matched = here.filter(|&(_, len)| ahead.is_none_or(|(_, next)| next <= len));
        if let Some((distance, length)) = matched {
            tokens.push(Token::Copy {
                length: length as u32,
                distance: distance as u32,
            });
            i += length;
            here = chain.find(pixels, i); // jumped past the lookahead; recompute
        } else {
            tokens.push(Token::Literal(pixels[i]));
            i += 1;
            here = ahead; // reuse the lookahead as the new current match
        }
    }
    tokens
}

/// Walk `tokens` in output order, applying a `cache_bits`-wide color cache
/// (disabled when `0`) and resolving each unit to its bitstream symbols, driving
/// `sink` once per emitted unit.
///
/// Every produced pixel is inserted into the cache in output order — literals,
/// each copied pixel, and cache hits — mirroring the decoder exactly. A literal
/// whose color already sits in its cache slot becomes a [`Resolved::Cache`]
/// (a fresh zero slot legitimately matches a black pixel).
pub(crate) fn resolve(
    tokens: &[Token],
    pixels: &[u32],
    cache_bits: u32,
    width: u32,
    mut sink: impl FnMut(usize, Resolved),
) {
    work!(ResolveWalk, tokens.len() as u64);
    let mut cache = (cache_bits > 0).then(|| {
        work!(ColorCacheAlloc);
        ColorCache::new(cache_bits)
    });
    // `width` is constant across the walk, so the distance -> plane-code reverse
    // map is built once instead of rescanning the 120-entry table per copy.
    let plane_map = PlaneCodeMap::new(width);
    let mut pos = 0usize;
    for &token in tokens {
        match token {
            Token::Literal(argb) => {
                let unit = cache.as_mut().map_or(Resolved::Literal(argb), |cache| {
                    let key = ColorCache::index(argb, cache_bits);
                    let unit = if cache.get(key) == argb {
                        Resolved::Cache(key as u16)
                    } else {
                        Resolved::Literal(argb)
                    };
                    cache.insert(argb);
                    unit
                });
                sink(pos, unit);
                pos += 1;
            },
            Token::Copy { length, distance } => {
                let (length_symbol, length_bits, length_value) = prefix_encode(length);
                let plane_code = plane_map.plane_code(distance);
                let (dist_symbol, dist_bits, dist_value) = prefix_encode(plane_code);
                sink(
                    pos,
                    Resolved::Copy {
                        length_symbol,
                        length_extra: (length_value, length_bits),
                        dist_symbol,
                        dist_extra: (dist_value, dist_bits),
                    },
                );
                if let Some(cache) = cache.as_mut() {
                    for pixel in &pixels[pos..pos + length as usize] {
                        cache.insert(*pixel);
                    }
                }
                pos += length as usize;
            },
        }
    }
}

/// Sentinel for "no earlier position" in the hash chain.
const NONE: u32 = u32::MAX;
/// Pair-hash multipliers (libwebp `kHashMultiplierLo` / `Hi`).
const HASH_MUL_LO: u32 = 0x5bd1_e996;
const HASH_MUL_HI: u32 = 0xc6a4_a793;

/// A backward-linked hash chain over pixel pairs: `prev[i]` is the most recent
/// earlier position whose pixel pair hashes the same as position `i`.
pub(crate) struct HashChain {
    prev: Vec<u32>,
}

impl HashChain {
    /// Build the chain in a single left-to-right pass.
    fn build(pixels: &[u32]) -> Self {
        let n = pixels.len();
        let hash_bits = effective_hash_bits(n);
        let mut head = vec![NONE; 1usize << hash_bits];
        let mut prev = vec![NONE; n];
        for (i, slot) in prev.iter_mut().enumerate() {
            let h = pair_hash(pixels, i, hash_bits);
            *slot = head[h];
            head[h] = i as u32;
        }
        Self { prev }
    }

    /// Find the longest exact match for the pixels at `i` among earlier
    /// positions within the window, or `None` if none reaches [`MIN_MATCH`].
    fn find(&self, pixels: &[u32], i: usize) -> Option<(usize, usize)> {
        let max_len = (MAX_COPY_LENGTH as usize).min(pixels.len() - i);
        if max_len < MIN_MATCH as usize {
            return None;
        }
        let window_min = i.saturating_sub(WINDOW_SIZE as usize);
        let (mut best_len, mut best_dist) = (0usize, 0usize);
        // `pixels[i + best_len]` — the right side of the quick-reject guard. It
        // depends only on `best_len`, which changes solely on a strict
        // improvement, so hoist the load and refresh it there instead of
        // re-indexing every hop. `best_len` starts at 0, so this is `pixels[i]`.
        let mut guard = pixels[i];
        let mut candidate = self.prev[i];
        let mut hops = 0u32;
        while candidate != NONE && candidate as usize >= window_min && hops < MAX_CHAIN {
            work!(MatchHop);
            let pos = candidate as usize;
            // Quick reject (byte-invariant): to strictly beat `best_len`, this
            // position must at least match at index `best_len`. If it does not,
            // its match is `<= best_len`, so it cannot improve the winner and the
            // full `match_length` scan is skipped. `best_len < max_len` here (the
            // loop breaks once it reaches the cap) and `pos < i`, so both reads
            // are in bounds. This preserves the selected `(best_dist, best_len)`
            // exactly — only wasted comparisons vanish.
            if pixels[pos + best_len] == guard {
                let len = match_length(pixels, pos, i, max_len);
                if len > best_len {
                    best_len = len;
                    best_dist = i - pos;
                    if best_len >= max_len {
                        break; // cannot improve on the capped length
                    }
                    guard = pixels[i + best_len]; // best_len < max_len, so in bounds
                }
            }
            candidate = self.prev[pos];
            hops += 1;
        }
        (best_len >= MIN_MATCH as usize).then_some((best_dist, best_len))
    }

    /// Collect the *staircase* of improving matches at `i`: walking the chain
    /// most-recent-first (so nearest distance first), each time [`match_length`]
    /// reaches a strictly longer length than any seen so far, push
    /// `(dist = i - pos, len)` into `out`. This reuses [`Self::find`]'s exact
    /// `window_min` / [`MAX_CHAIN`] / [`MAX_COPY_LENGTH`] / [`match_length`] logic;
    /// where `find` keeps only the single longest match, this keeps the whole
    /// improving spectrum — from a short copy at a *near* distance to a long copy
    /// at a *far* one. Because the chain is newest-first, the first position to
    /// reach a given length is the nearest (hence cheapest plane-code distance)
    /// for that length, so the near/far tradeoff the greedy parse cannot express
    /// is handed to the DP. Yields at most [`MAX_CHAIN`] points, each of length
    /// `>= MIN_MATCH`, in ascending-length order.
    ///
    /// `memo` carries per-distance match lengths from the previous DP position so
    /// [`length_at`] can shortcut the scan on runs; it changes only *how* each
    /// length is computed, never the value, so the pushed candidate set is
    /// identical to a scan-only walk.
    fn find_candidates(
        &self,
        pixels: &[u32],
        i: usize,
        out: &mut Vec<(u32, u32)>,
        memo: &mut ShiftMemo,
    ) {
        out.clear();
        let max_len = (MAX_COPY_LENGTH as usize).min(pixels.len() - i);
        if max_len < MIN_MATCH as usize {
            return;
        }
        let window_min = i.saturating_sub(WINDOW_SIZE as usize);
        let mut best_len = 0usize;
        // Hoisted quick-reject guard `pixels[i + best_len]`, refreshed only on a
        // strict improvement — see `find`. `best_len` starts at 0, so `pixels[i]`.
        let mut guard = pixels[i];
        let mut candidate = self.prev[i];
        let mut hops = 0u32;
        while candidate != NONE && candidate as usize >= window_min && hops < MAX_CHAIN {
            work!(MatchHop);
            let pos = candidate as usize;
            // Same byte-invariant quick reject as `find`: a position that fails to
            // match at index `best_len` cannot exceed `best_len`, so it can never
            // be a strictly-improving staircase point — skip the full scan. The
            // pushed candidate set is therefore identical.
            if pixels[pos + best_len] == guard {
                let dist = (i - pos) as u32;
                let len = length_at(memo, pixels, pos, i, max_len, dist);
                if len > best_len {
                    best_len = len;
                    if len >= MIN_MATCH as usize {
                        out.push((dist, len as u32));
                    }
                    if best_len >= max_len {
                        break; // cannot improve on the capped length
                    }
                    guard = pixels[i + best_len]; // best_len < max_len, so in bounds
                }
            }
            candidate = self.prev[pos];
            hops += 1;
        }
    }
}

/// A per-distance shift memo for the backward optimal-parse DP: it carries, from
/// one position to the next, the capped match length found at each distance so
/// [`length_at`] can derive most lengths in O(1) instead of re-scanning a run.
///
/// `read` holds the `(distance, capped_len)` pairs recorded at position `i + 1`;
/// `write` accumulates them for the position `i` currently being processed. The
/// two are swapped by [`Self::advance`] once per position. Double-buffering (not a
/// position-tagged map) means a hit in `read` is *structurally* the `i + 1` value
/// and never a stale one — the invariant the byte-identity of the DP rests on.
/// The set of distinct distances probed per position is `<= MAX_CHAIN`, so the
/// buffers stay tiny, and `Vec::clear` keeps their capacity (no per-position
/// allocation churn), which is why this is a plain association list, not a map.
#[derive(Default)]
struct ShiftMemo {
    /// `(distance, capped_len)` recorded at position `i + 1`.
    read: Vec<(u32, u32)>,
    /// `(distance, capped_len)` being recorded at position `i`.
    write: Vec<(u32, u32)>,
}

impl ShiftMemo {
    /// The capped match length recorded for `dist` at the previous (`i + 1`)
    /// position, if that distance was probed there.
    fn child(&self, dist: u32) -> Option<u32> {
        self.read
            .iter()
            .find(|&&(d, _)| d == dist)
            .map(|&(_, len)| len)
    }

    /// Record the capped match length for `dist` at the current position, for the
    /// next (`i - 1`) position to shift from.
    fn record(&mut self, dist: u32, len: u32) {
        self.write.push((dist, len));
    }

    /// Advance to the next position: this position's records become the previous
    /// ones, and the write buffer is emptied (retaining capacity).
    fn advance(&mut self) {
        core::mem::swap(&mut self.read, &mut self.write);
        self.write.clear();
    }
}

/// The capped match length at `(src, dst)` — identical to what a full
/// [`match_length`] scan over `(src, dst, max_len)` returns — derived in O(1)
/// from the same distance one position later when the memo has it, else scanned.
///
/// The recurrence is exact backward: with `d = dst - src` fixed,
/// `cap_i(d) = 0` if `pixels[src] != pixels[dst]`, else
/// `min(max_len, 1 + cap_{i+1}(d))`. The `min` resolves the capped child exactly
/// (walking backward, `max_len` grows by at most one per step), so the memoized
/// value equals a full scan bit for bit. Every call records its result for the
/// next position, so a persistent distance (solid / gradient / tiled runs)
/// collapses the per-position `O(len)` scan to a single pixel comparison.
fn length_at(
    memo: &mut ShiftMemo,
    pixels: &[u32],
    src: usize,
    dst: usize,
    max_len: usize,
    distance: u32,
) -> usize {
    let len = match memo.child(distance) {
        // Shift from the same distance one position ahead: one honest pixel
        // comparison (the `k = 0` test the scan would also start with).
        Some(child) if pixels[src] == pixels[dst] => {
            work!(MatchCompare, 1);
            (child as usize + 1).min(max_len)
        },
        // `pixels[src] != pixels[dst]`: a full scan would stop at `k = 0` and
        // return 0 (counting nothing), so short-circuit to the same.
        Some(_) => 0,
        // First time this distance is seen at this run of positions: scan.
        None => match_length(pixels, src, dst, max_len),
    };
    memo.record(distance, len as u32);
    len
}

/// Length of the exact match of the pixels at `dst` against those at `src`
/// (`src < dst`), capped at `max_len`. Overlapping matches (`dst - src < len`)
/// extend naturally against the known source, reproducing run-length fills.
fn match_length(pixels: &[u32], src: usize, dst: usize, max_len: usize) -> usize {
    let mut len = 0usize;
    while len < max_len && pixels[src + len] == pixels[dst + len] {
        len += 1;
    }
    work!(MatchCompare, len as u64);
    len
}

/// Hash a pixel pair `(pixels[i], pixels[i + 1])` into `hash_bits` bits.
fn pair_hash(pixels: &[u32], i: usize, hash_bits: u32) -> usize {
    let a = pixels[i];
    let b = pixels.get(i + 1).copied().unwrap_or(0);
    let key = a
        .wrapping_mul(HASH_MUL_LO)
        .wrapping_add(b.wrapping_mul(HASH_MUL_HI));
    (key >> (32 - hash_bits)) as usize
}

/// Hash-table width for `n` pixels: `ceil(log2 n)`, capped at [`HASH_BITS_LZ77`],
/// so small images do not allocate the full table. Using `next_power_of_two`
/// keeps this correct (and overflow-free) for every `n`, including the tiny
/// values the caller never actually passes.
fn effective_hash_bits(n: usize) -> u32 {
    let ceil_log2 = usize::BITS - n.next_power_of_two().leading_zeros() - 1;
    ceil_log2.clamp(1, HASH_BITS_LZ77)
}

/// Fixed-point fractional bits carried by the [`CostModel`], matching the
/// `LOG2_FRAC_BITS` of [`fixed_log2`] (Q16). A raw extra bit therefore costs
/// exactly `1 << COST_FRAC_BITS`.
const COST_FRAC_BITS: u32 = 16;

/// A per-symbol self-information cost table (Q16 bits) derived from a pass-1
/// symbol histogram — the entropy the *greedy* parse's distribution assigns to
/// each symbol, used to price literals and copies during the optimal DP parse.
///
/// It is intentionally **cache-agnostic** (built from the histogram at
/// `cache_bits = 0`): the color cache is applied later by [`resolve`], exactly as
/// it is for the greedy stream, so pricing here mirrors what the emit pass sees
/// before the cache decision. All arithmetic is integer / fixed-point, so the DP
/// is deterministic on every platform.
pub(crate) struct CostModel {
    /// Green code costs: literals `0..256`, then length symbols at
    /// `NUM_LITERAL_CODES + length_symbol`.
    green: Vec<u64>,
    /// Red / blue / alpha literal-channel costs.
    red: [u64; NUM_LITERAL_CODES],
    blue: [u64; NUM_LITERAL_CODES],
    alpha: [u64; NUM_LITERAL_CODES],
    /// Distance-symbol costs.
    dist: [u64; NUM_DISTANCE_CODES],
}

impl CostModel {
    /// Build the cost table from a pass-1 [`Histogram`].
    ///
    /// Per channel: `cost[s] = fixed_log2(total) - fixed_log2(count[s])`, the Q16
    /// self-information of symbol `s`. An unseen symbol (`count == 0`) scores
    /// `fixed_log2(total)` (the maximum, still finite), so a copy that introduces
    /// a never-before-seen length/distance is discouraged but not forbidden. A
    /// wholly empty channel (`total == 0`, e.g. no copies at all) is filled with
    /// the neutral `fixed_log2(alphabet_len)` so copies stay priceable.
    pub(crate) fn from_histogram(h: &Histogram) -> Self {
        Self {
            green: channel_costs(h.green()),
            red: channel_costs_arr(h.red()),
            blue: channel_costs_arr(h.blue()),
            alpha: channel_costs_arr(h.alpha()),
            dist: channel_costs_arr(h.dist()),
        }
    }

    /// Q16-bit cost of coding `argb` as a literal: the sum of its four channel
    /// self-information (green byte, red byte, blue byte, alpha byte).
    fn literal_cost(&self, argb: u32) -> u64 {
        self.green[((argb >> 8) & 0xff) as usize]
            + self.red[((argb >> 16) & 0xff) as usize]
            + self.blue[(argb & 0xff) as usize]
            + self.alpha[((argb >> 24) & 0xff) as usize]
    }

    /// Q16-bit cost of a back-reference: the length prefix symbol and distance
    /// prefix symbol self-information, plus their raw extra bits (each an exact
    /// `1 << COST_FRAC_BITS`).
    fn copy_cost(
        &self,
        length_symbol: u32,
        length_bits: u32,
        dist_symbol: u32,
        dist_bits: u32,
    ) -> u64 {
        self.green[NUM_LITERAL_CODES + length_symbol as usize]
            + (u64::from(length_bits) << COST_FRAC_BITS)
            + self.dist[dist_symbol as usize]
            + (u64::from(dist_bits) << COST_FRAC_BITS)
    }
}

/// Self-information (Q16 bits) of every symbol in a variable-length channel.
fn channel_costs(counts: &[u32]) -> Vec<u64> {
    let total: u64 = counts.iter().map(|&c| u64::from(c)).sum();
    if total == 0 {
        return vec![fixed_log2(counts.len() as u64); counts.len()];
    }
    let total_log2 = fixed_log2(total);
    counts
        .iter()
        .map(|&c| total_log2.saturating_sub(fixed_log2(u64::from(c))))
        .collect()
}

/// Self-information (Q16 bits) of every symbol in a fixed-size channel; `N` is
/// inferred from the destination array and must equal `counts.len()`.
fn channel_costs_arr<const N: usize>(counts: &[u32]) -> [u64; N] {
    debug_assert_eq!(counts.len(), N);
    let total: u64 = counts.iter().map(|&c| u64::from(c)).sum();
    if total == 0 {
        return [fixed_log2(N as u64); N];
    }
    let total_log2 = fixed_log2(total);
    core::array::from_fn(|s| total_log2.saturating_sub(fixed_log2(u64::from(counts[s]))))
}

/// The optimal-parse decision at one position: code a literal, or take a copy of
/// `len` at `dist`. Recorded per position by the backward DP and replayed forward.
#[derive(Clone, Copy)]
enum Choice {
    Literal,
    Copy { len: u32, dist: u32 },
}

/// Cost-model-driven near-optimal LZ77 parse: a backward dynamic program that,
/// at each position, picks the literal-or-copy choice minimizing the total Q16
/// self-information of the remaining stream, then reconstructs the token list
/// forward.
///
/// The cost model is a pass-1 histogram of the already-computed `greedy_tokens`
/// (the same cache-agnostic distribution the greedy stream emits), so the DP
/// prices exactly what the encoder will code. The near/far distance tradeoff is
/// supplied by [`HashChain::find_candidates`]; the length/distance caps,
/// window, and overlap (RLE) handling are inherited from [`match_length`] via
/// that method. The result is a valid cache-agnostic [`Token`] stream — the same
/// `Literal` / `Copy` shape [`resolve`] already handles — so it round-trips by
/// construction, and (wired as a self-floored extra candidate) can only ever
/// replace the greedy stream when it is strictly smaller.
pub(crate) fn parse_optimal(
    pixels: &[u32],
    width: u32,
    greedy_tokens: &[Token],
    chain: &HashChain,
) -> Vec<Token> {
    let n = pixels.len();
    if n < MIN_MATCH as usize {
        return greedy_tokens.to_vec();
    }
    // Backward pass: the `cost`/`chain`/`cands` scratch and the cost model exist
    // only to fill `choice`. Scope them so they drop before the forward
    // reconstruction allocates the token list, keeping the `cost` (8·n bytes) and
    // hash chain off the memory high-water mark during that final allocation. Only
    // `choice` escapes, so the recorded decisions — and the emitted tokens — are
    // unchanged.
    let choice = {
        // Pass-1 histogram of the greedy stream, cache-agnostic (cache_bits = 0) —
        // reusing the encoder's own model so the DP prices the identical
        // distribution the emit pass sees before any cache decision.
        let histogram =
            crate::lossless::vp8l::encode::RefModel::new(greedy_tokens, pixels, width).histogram(0);
        let cost_model = CostModel::from_histogram(&histogram);
        // `chain` is threaded in by the caller (built once by the greedy parse). It
        // is a pure function of `pixels`, so a shared chain is bit-identical to one
        // built here, leaving the DP — and its tokens — unchanged.
        // `width` is fixed across the DP, so build the distance -> plane-code reverse
        // map once rather than rescanning the 120-entry table for every candidate.
        let plane_map = PlaneCodeMap::new(width);

        // cost[i] = minimal Q16 cost to code pixels[i..]; cost[n] = 0.
        let mut cost = vec![0u64; n + 1];
        let mut choice = vec![Choice::Literal; n];
        let mut cands: Vec<(u32, u32)> = Vec::new();
        // Carries each position's per-distance match lengths to the next (`i - 1`)
        // iteration so `find_candidates` can shift them instead of re-scanning
        // runs. Empty on the first iteration (`i = n - 1`), so it starts by scanning.
        let mut memo = ShiftMemo::default();
        for i in (0..n).rev() {
            work!(OptimalDpStep);
            // Literal is always available and seeds the minimum (its earliest-wins
            // tiebreak keeps ties -> literal, matching the greedy floor).
            let mut best = cost_model
                .literal_cost(pixels[i])
                .saturating_add(cost[i + 1]);
            let mut best_choice = Choice::Literal;
            chain.find_candidates(pixels, i, &mut cands, &mut memo);
            for &(dist, len) in &cands {
                let (length_symbol, length_bits, _) = prefix_encode(len);
                let plane_code = plane_map.plane_code(dist);
                let (dist_symbol, dist_bits, _) = prefix_encode(plane_code);
                let c = cost_model
                    .copy_cost(length_symbol, length_bits, dist_symbol, dist_bits)
                    .saturating_add(cost[i + len as usize]);
                if c < best {
                    best = c;
                    best_choice = Choice::Copy { len, dist };
                }
            }
            cost[i] = best;
            choice[i] = best_choice;
            // This position's records become the previous ones for `i - 1`.
            memo.advance();
        }
        choice
    };

    // Forward reconstruction from the recorded choices.
    let mut tokens = Vec::new();
    let mut i = 0usize;
    while i < n {
        match choice[i] {
            Choice::Literal => {
                tokens.push(Token::Literal(pixels[i]));
                i += 1;
            },
            Choice::Copy { len, dist } => {
                tokens.push(Token::Copy {
                    length: len,
                    distance: dist,
                });
                i += len as usize;
            },
        }
    }
    tokens
}

#[cfg(test)]
mod tests {
    use super::{Resolved, Token, parse, parse_lz77, parse_optimal, resolve};
    use crate::lossless::color_cache::ColorCache;
    use proptest::prelude::*;

    /// Reproduce a token list the way the decoder would (forward element-wise
    /// copies, overlap-safe) — the oracle that `parse` is lossless.
    fn decode_tokens(tokens: &[Token]) -> Vec<u32> {
        let mut out = Vec::new();
        for &token in tokens {
            match token {
                Token::Literal(argb) => out.push(argb),
                Token::Copy { length, distance } => {
                    let start = out.len() - distance as usize;
                    for k in 0..length as usize {
                        out.push(out[start + k]);
                    }
                },
            }
        }
        out
    }

    /// A straightforward `parse` loop that recomputes `find(i + 1)` on every step
    /// instead of carrying it. The production [`parse`] must produce
    /// byte-identical tokens for every input; this reference pins that
    /// equivalence mechanically (the shared [`HashChain`] guarantees the match
    /// finder is unchanged, so only the loop that consumes it is under test).
    fn reference_parse(pixels: &[u32], use_lz77: bool) -> Vec<Token> {
        use crate::lossless::constants::MIN_MATCH;
        if !use_lz77 || pixels.len() < MIN_MATCH as usize {
            return pixels.iter().map(|&p| Token::Literal(p)).collect();
        }
        let chain = super::HashChain::build(pixels);
        let n = pixels.len();
        let mut tokens = Vec::new();
        let mut i = 0usize;
        while i < n {
            let matched = chain.find(pixels, i).filter(|&(_, len)| {
                !(i + 1 < n
                    && chain
                        .find(pixels, i + 1)
                        .is_some_and(|(_, next)| next > len))
            });
            if let Some((distance, length)) = matched {
                tokens.push(Token::Copy {
                    length: length as u32,
                    distance: distance as u32,
                });
                i += length;
            } else {
                tokens.push(Token::Literal(pixels[i]));
                i += 1;
            }
        }
        tokens
    }

    /// The pre-hoist `find` verbatim — it re-indexes and reloads
    /// `pixels[i + best_len]` on every chain hop. The optimized
    /// [`super::HashChain::find`] hoists that guard load and refreshes it only on a
    /// strict improvement; this reference pins the byte-identity of the selected
    /// `(best_dist, best_len)` mechanically. Reading the private `chain.prev` and
    /// calling `match_length` is legal from this child module.
    fn find_reference(
        chain: &super::HashChain,
        pixels: &[u32],
        i: usize,
    ) -> Option<(usize, usize)> {
        use crate::lossless::constants::{MAX_CHAIN, MAX_COPY_LENGTH, MIN_MATCH, WINDOW_SIZE};
        let max_len = (MAX_COPY_LENGTH as usize).min(pixels.len() - i);
        if max_len < MIN_MATCH as usize {
            return None;
        }
        let window_min = i.saturating_sub(WINDOW_SIZE as usize);
        let (mut best_len, mut best_dist) = (0usize, 0usize);
        let mut candidate = chain.prev[i];
        let mut hops = 0u32;
        while candidate != super::NONE && candidate as usize >= window_min && hops < MAX_CHAIN {
            let pos = candidate as usize;
            if pixels[pos + best_len] == pixels[i + best_len] {
                let len = super::match_length(pixels, pos, i, max_len);
                if len > best_len {
                    best_len = len;
                    best_dist = i - pos;
                    if best_len >= max_len {
                        break;
                    }
                }
            }
            candidate = chain.prev[pos];
            hops += 1;
        }
        (best_len >= MIN_MATCH as usize).then_some((best_dist, best_len))
    }

    #[test]
    fn literal_only_parse_is_all_literals() {
        let pixels = [1u32, 2, 3, 4, 5];
        let tokens = parse(&pixels, false);
        assert_eq!(tokens.len(), 5);
        assert!(tokens.iter().all(|t| matches!(t, Token::Literal(_))));
        assert_eq!(decode_tokens(&tokens), pixels);
    }

    #[test]
    fn repeated_tile_produces_a_copy() {
        // A 4-pixel tile repeated four times: the tail must become a copy.
        let tile = [10u32, 20, 30, 40];
        let pixels: Vec<u32> = tile.iter().cycle().take(16).copied().collect();
        let tokens = parse(&pixels, true);
        assert!(
            tokens.iter().any(|t| matches!(t, Token::Copy { .. })),
            "a repeated tile must emit at least one copy"
        );
        assert_eq!(decode_tokens(&tokens), pixels);
    }

    #[test]
    fn solid_run_becomes_a_dist_one_copy() {
        let pixels = [7u32; 12];
        let tokens = parse(&pixels, true);
        assert!(
            tokens
                .iter()
                .any(|t| matches!(t, Token::Copy { distance: 1, .. })),
            "a solid run must exploit a distance-1 (RLE) copy"
        );
        assert_eq!(decode_tokens(&tokens), pixels);
    }

    #[test]
    fn resolve_emits_a_cache_hit_for_a_repeated_color() {
        let color = 0x1122_3344u32;
        let pixels = [color, color];
        let tokens = vec![Token::Literal(color), Token::Literal(color)];
        let mut units = Vec::new();
        resolve(&tokens, &pixels, 8, 2, |_pos, r| units.push(r));
        assert!(matches!(units[0], Resolved::Literal(_)));
        match units[1] {
            Resolved::Cache(key) => {
                assert_eq!(usize::from(key), ColorCache::index(color, 8));
            },
            other => panic!("second identical pixel must be a cache hit, got {other:?}"),
        }
    }

    #[test]
    fn resolve_treats_a_fresh_zero_slot_as_a_hit() {
        // A zero-initialized slot legitimately matches a black (0) pixel; the
        // decoder replays the same zero cache, so this is correct and cheap.
        let pixels = [0u32];
        let tokens = vec![Token::Literal(0)];
        let mut units = Vec::new();
        resolve(&tokens, &pixels, 4, 1, |_pos, r| units.push(r));
        assert!(matches!(units[0], Resolved::Cache(0)));
    }

    #[test]
    fn resolve_passes_unit_start_pos() {
        // literal a ; copy(len 3, dist 1) replicating a ; literal b
        let a = 0xff_00_00_00u32;
        let b = 0xff_11_22_33u32;
        let pixels = [a, a, a, a, b];
        let tokens = [
            Token::Literal(a),
            Token::Copy {
                length: 3,
                distance: 1,
            },
            Token::Literal(b),
        ];
        let mut starts = Vec::new();
        resolve(&tokens, &pixels, 0, 5, |pos, _unit| starts.push(pos));
        assert_eq!(starts, vec![0, 1, 4]);
    }

    /// Deterministic repetition-prone pixels from a seed (small palette so LZ77
    /// matches and RLE runs are common). Shared by the parse proptests.
    fn seeded_pixels(seed: u64, len: usize, palette: u32) -> Vec<u32> {
        let mut state = seed | 1;
        (0..len)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                (state >> 40) as u32 % palette
            })
            .collect()
    }

    /// The pre-memo `find_candidates` verbatim — a scan-only staircase walk with
    /// no shift memo. The memoized [`super::HashChain::find_candidates`] must push
    /// a byte-identical candidate vector at every position; this reference pins
    /// that equivalence mechanically, mirroring [`reference_parse`]. Reading the
    /// private `chain.prev` and calling the private `match_length` is legal from
    /// this child module.
    fn find_candidates_reference(
        chain: &super::HashChain,
        pixels: &[u32],
        i: usize,
        out: &mut Vec<(u32, u32)>,
    ) {
        use crate::lossless::constants::{MAX_CHAIN, MAX_COPY_LENGTH, MIN_MATCH, WINDOW_SIZE};
        out.clear();
        let max_len = (MAX_COPY_LENGTH as usize).min(pixels.len() - i);
        if max_len < MIN_MATCH as usize {
            return;
        }
        let window_min = i.saturating_sub(WINDOW_SIZE as usize);
        let mut best_len = 0usize;
        let mut candidate = chain.prev[i];
        let mut hops = 0u32;
        while candidate != super::NONE && candidate as usize >= window_min && hops < MAX_CHAIN {
            let pos = candidate as usize;
            if pixels[pos + best_len] == pixels[i + best_len] {
                let len = super::match_length(pixels, pos, i, max_len);
                if len > best_len {
                    best_len = len;
                    if len >= MIN_MATCH as usize {
                        out.push(((i - pos) as u32, len as u32));
                    }
                    if best_len >= max_len {
                        break;
                    }
                }
            }
            candidate = chain.prev[pos];
            hops += 1;
        }
    }

    /// Drive the REAL backward DP lifecycle (one persistent [`super::ShiftMemo`],
    /// advanced each step, `i` from `n - 1` down to `0` — exactly as
    /// `parse_optimal` does) over inputs long enough to hit the `MAX_COPY_LENGTH`
    /// (4096) ceiling, and assert the memoized candidate vector equals the
    /// scan-only reference at every position. This is the delicate case: the
    /// recurrence must resolve a *capped* child exactly. Covers distance-1 solid
    /// runs, a short period (distance = period), and distinct pixels (memo misses).
    #[test]
    fn find_candidates_matches_reference_across_the_cap() {
        let solid = vec![7u32; 5000];
        let periodic: Vec<u32> = (0u32..5000).map(|k| k % 3).collect();
        let distinct: Vec<u32> = (0u32..5000).collect();
        for pixels in [&solid, &periodic, &distinct] {
            let chain = super::HashChain::build(pixels);
            let mut memo = super::ShiftMemo::default();
            let (mut got, mut want) = (Vec::new(), Vec::new());
            for i in (0..pixels.len()).rev() {
                chain.find_candidates(pixels, i, &mut got, &mut memo);
                find_candidates_reference(&chain, pixels, i, &mut want);
                assert_eq!(got, want, "candidate mismatch at position {i}");
                memo.advance();
            }
        }
    }

    /// A `parse_optimal` variant that builds its own [`super::HashChain`]
    /// internally, where the production [`parse_optimal`] takes one from the
    /// caller. Handing the DP a freshly built chain is the oracle that reusing
    /// `parse_lz77`'s chain changes nothing.
    fn parse_optimal_reference(pixels: &[u32], width: u32, greedy: &[Token]) -> Vec<Token> {
        let chain = super::HashChain::build(pixels);
        parse_optimal(pixels, width, greedy, &chain)
    }

    /// Explicit edge cases for the shared-chain path: empty, 1x1, solid, a tiny
    /// palette-like run, a copy-heavy tile, and pixels with a non-zero alpha byte.
    /// For each, feeding the DP the greedy parse's chain must match rebuilding it.
    #[test]
    fn parse_optimal_shared_chain_edge_cases() {
        let empty: Vec<u32> = Vec::new();
        let one = vec![0x1122_3344u32];
        let solid = vec![7u32; 40];
        let palette: Vec<u32> = (0u32..40).map(|k| k % 3).collect();
        let tile: Vec<u32> = [10u32, 20, 30, 40]
            .iter()
            .cycle()
            .take(40)
            .copied()
            .collect();
        let alpha: Vec<u32> = (0u32..40).map(|k| 0xff00_0000 | (k % 5)).collect();
        for pixels in [&empty, &one, &solid, &palette, &tile, &alpha] {
            for width in [1u32, 2, 5, 8] {
                let (greedy, chain) = parse_lz77(pixels);
                let shared = parse_optimal(pixels, width, &greedy, &chain);
                let reference = parse_optimal_reference(pixels, width, &greedy);
                assert_eq!(shared, reference, "shared-chain mismatch at width {width}");
            }
        }
    }

    #[test]
    fn literal_cost_is_the_exact_four_channel_sum() {
        // `literal_cost` reads the four channel bytes of `argb` at the exact shifts
        // green=(>>8), red=(>>16), blue=(&0xff), alpha=(>>24), masks each to a byte,
        // and sums the four table entries. Distinct per-index table values make any
        // wrong shift/mask/operator land on a different sum. argb = 0x1122_3344:
        // green byte 0x33, red 0x22, blue 0x44, alpha 0x11.
        let cm = super::CostModel {
            green: (0u64..280).map(|i| 100 + i).collect(),
            red: core::array::from_fn(|i| 200 + i as u64),
            blue: core::array::from_fn(|i| 300 + i as u64),
            alpha: core::array::from_fn(|i| 400 + i as u64),
            dist: core::array::from_fn(|i| 20_000 + i as u64),
        };
        // green[0x33] + red[0x22] + blue[0x44] + alpha[0x11]
        // = 151 + 234 + 368 + 417 = 1170.
        assert_eq!(cm.literal_cost(0x1122_3344), 1170);
    }

    #[test]
    fn copy_cost_is_the_exact_symbol_and_extra_bit_sum() {
        // `copy_cost` = green[NUM_LITERAL_CODES + length_symbol] + length_bits<<16
        //             + dist[dist_symbol] + dist_bits<<16 (COST_FRAC_BITS = 16).
        // Distinct per-index tables pin every operator, shift, and index offset.
        let cm = super::CostModel {
            green: (0u64..280).map(|i| 10_000 + i).collect(),
            red: [0; 256],
            blue: [0; 256],
            alpha: [0; 256],
            dist: core::array::from_fn(|i| 20_000 + i as u64),
        };
        // green[256 + 5] + (3<<16) + dist[7] + (2<<16)
        // = 10_261 + 196_608 + 20_007 + 131_072 = 357_948.
        assert_eq!(cm.copy_cost(5, 3, 7, 2), 357_948);
    }

    #[test]
    fn channel_costs_switches_on_a_zero_total() {
        // Nonzero total takes the self-information branch: for [1, 3], total = 4,
        // total_log2 = fixed_log2(4) = 2<<16, so symbol 0 (count 1) costs
        // 131_072 - fixed_log2(1) = 131_072. A zero total takes the neutral
        // fixed_log2(len) fill instead (fixed_log2(2) = 1<<16 = 65_536). The two
        // branches are distinct, so `== 0` cannot become `!= 0`.
        assert_eq!(super::channel_costs(&[1, 3])[0], 131_072);
        assert_eq!(super::channel_costs(&[0, 0]), vec![65_536, 65_536]);
    }

    #[test]
    fn pair_hash_mixes_both_pixels_of_the_pair() {
        // The hash folds pixels[i] AND pixels[i + 1]; using pixels[i] twice (an
        // `i + 1` -> `i` index slip) gives a different bucket. For [1, 2] at 8 bits
        // the real bucket is 0xE9 = 233; the both-equal fold would give 0x22 = 34.
        assert_eq!(super::pair_hash(&[1u32, 2], 0, 8), 233);
    }

    #[test]
    fn effective_hash_bits_is_clamped_ceil_log2() {
        // usize::BITS(64) - leading_zeros(next_power_of_two(16) = 16 -> 59) - 1 = 4.
        // A `+`/`/` for either `-` shifts this off 4; the huge/tiny result then
        // clamps differently (a `-` -> `+` on the first term saturates to 18).
        assert_eq!(super::effective_hash_bits(16), 4);
        // The upper clamp: a 2^20-pixel image caps at HASH_BITS_LZ77 = 18.
        assert_eq!(super::effective_hash_bits(1usize << 20), 18);
    }

    #[test]
    fn length_at_short_circuits_on_a_pixel_mismatch() {
        // A memo hit records a capped length for `distance`, but it is only valid
        // when the two pixels actually match: `pixels[src] == pixels[dst]`. When
        // they differ a full scan stops at k = 0 and returns 0, so `length_at` must
        // ignore the memoized child and return 0 — the match-guard cannot be `true`.
        let pixels = [10u32, 20, 30, 40];
        let mut memo = super::ShiftMemo::default();
        memo.record(1, 5); // capped length 5 for distance 1 at the "i + 1" position
        memo.advance(); // child(1) is now Some(5)
        // src = 0 (10) != dst = 1 (20): despite the memo hit, the length is 0.
        assert_eq!(super::length_at(&mut memo, &pixels, 0, 1, 4, 1), 0);
    }

    #[test]
    fn find_returns_none_without_an_earlier_match() {
        // Five distinct pixels: no pixel pair ever repeats, so no position has a
        // back-reference reaching MIN_MATCH. `find` must report `None` everywhere —
        // a stub returning `Some((_, 0))`, or an inverted final `>=`, would not.
        let pixels = [10u32, 20, 30, 40, 50];
        let chain = super::HashChain::build(&pixels);
        for i in 0..pixels.len() {
            assert_eq!(chain.find(&pixels, i), None, "position {i}");
        }
    }

    #[test]
    fn find_selects_the_nearest_longest_match() {
        // A solid run: at position 1 the match extends over the whole remaining run
        // at distance 1 (RLE), capped only by the remaining length (7). The exact
        // `(distance, length)` pins the finder against length-0 / fixed-tuple stubs
        // and an inverted final `>=` (which would drop this length-7 match).
        let pixels = [7u32; 8];
        let chain = super::HashChain::build(&pixels);
        assert_eq!(chain.find(&pixels, 1), Some((1, 7)));
    }

    #[test]
    fn find_stops_at_the_max_chain_hop_limit() {
        // The only length-4 match source (`p0`) sits at hash-chain depth 65, behind
        // 64 "decoy" positions that share the (1, 2) pair hash but match just length
        // 2 (their third pixel is 5, not 3). MAX_CHAIN = 64 stops the walk exactly
        // one hop short of `p0`, so the real finder reports `None`. Any finder that
        // takes one hop more (`hops <= MAX_CHAIN`) or drops the cap entirely
        // (`hops *= 1`, frozen at 0) reaches `p0` and returns `Some`.
        let mut pixels = vec![1u32, 2, 3, 4]; // p0: the length-4 match source
        for _ in 0..64 {
            pixels.extend_from_slice(&[1, 2, 5]); // 64 decoys, all pair (1, 2)
        }
        pixels.extend_from_slice(&[1, 2, 3, 4]); // current block matches p0
        let i = pixels.len() - 4;
        let chain = super::HashChain::build(&pixels);
        assert_eq!(chain.find(&pixels, i), None);
    }

    #[test]
    fn find_candidates_stops_at_the_max_chain_hop_limit() {
        // The staircase analogue of `find_stops_at_the_max_chain_hop_limit`: the
        // only length-4 match source (`p0`) sits at hash-chain depth 65, behind 64
        // "decoy" positions that share the (1, 2) pair hash but match just length 2
        // (their third pixel is 5, not 3) — below MIN_MATCH, so no decoy is ever a
        // pushed staircase point. MAX_CHAIN = 64 stops the walk exactly one hop
        // short of `p0`, so the real `find_candidates` pushes nothing (an empty
        // spectrum). A finder that takes one hop more (`hops <= MAX_CHAIN`) reaches
        // `p0` and pushes the single length-4 candidate, so `<` and `<=` diverge on
        // the exact emitted candidate set.
        let mut pixels = vec![1u32, 2, 3, 4]; // p0: the length-4 match source
        for _ in 0..64 {
            pixels.extend_from_slice(&[1, 2, 5]); // 64 decoys, all pair (1, 2)
        }
        pixels.extend_from_slice(&[1, 2, 3, 4]); // current block matches p0
        let i = pixels.len() - 4;
        let chain = super::HashChain::build(&pixels);
        let mut memo = super::ShiftMemo::default();
        let mut out = Vec::new();
        chain.find_candidates(&pixels, i, &mut out, &mut memo);
        assert!(
            out.is_empty(),
            "the staircase must be empty at the hop limit, got {out:?}"
        );
    }

    #[test]
    fn parse_optimal_replaces_a_suboptimal_greedy_stream() {
        // Greedy takes Copy{4,3} then Copy{5,4}; the cost DP finds the cheaper split
        // (three extra literals then a single Copy{6,4}). The DP result is therefore
        // NOT the greedy token list, so the `n < MIN_MATCH` early-out must stay
        // `< 4`: turning it into `n > 4` (returning greedy verbatim) is caught.
        let pixels = [1u32, 0, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1];
        let greedy = parse(&pixels, true);
        let chain = super::HashChain::build(&pixels);
        let optimal = parse_optimal(&pixels, 8, &greedy, &chain);
        assert_eq!(
            optimal,
            vec![
                Token::Literal(1),
                Token::Literal(0),
                Token::Literal(1),
                Token::Literal(1),
                Token::Literal(0),
                Token::Literal(1),
                Token::Copy {
                    length: 6,
                    distance: 4,
                },
            ]
        );
        assert_ne!(optimal, greedy);
    }

    #[test]
    fn parse_optimal_uses_a_strict_less_than_decision() {
        // At width 1 the DP keeps Copy{4,3} then a trailing literal. Pin the exact
        // stream: a `< -> <=` / `== ` / `>` flip in the copy-vs-best comparison, or a
        // literal seed that adds cost[i] instead of cost[i + 1], all change it.
        let pixels = [1u32, 0, 1, 1, 0, 1, 1, 1];
        let greedy = parse(&pixels, true);
        let chain = super::HashChain::build(&pixels);
        let optimal = parse_optimal(&pixels, 1, &greedy, &chain);
        assert_eq!(
            optimal,
            vec![
                Token::Literal(1),
                Token::Literal(0),
                Token::Literal(1),
                Token::Copy {
                    length: 4,
                    distance: 3,
                },
                Token::Literal(1),
            ]
        );
    }

    #[test]
    fn parse_optimal_tiebreak_direction_is_pinned() {
        // A longer input where a `< -> <=` tiebreak flip changes the chosen copy
        // length (7 here). Pinning the exact optimal token stream asserts the
        // tiebreak direction, not merely that the result round-trips.
        let pixels = [
            1u32, 0, 1, 1, 0, 1, 1, 1, 0, 1, 1, 1, 0, 0, 1, 0, 0, 0, 1, 0,
        ];
        let greedy = parse(&pixels, true);
        let chain = super::HashChain::build(&pixels);
        let optimal = parse_optimal(&pixels, 1, &greedy, &chain);
        assert_eq!(
            optimal,
            vec![
                Token::Literal(1),
                Token::Literal(0),
                Token::Literal(1),
                Token::Literal(1),
                Token::Literal(0),
                Token::Literal(1),
                Token::Copy {
                    length: 7,
                    distance: 4,
                },
                Token::Literal(0),
                Token::Literal(1),
                Token::Literal(0),
                Token::Copy {
                    length: 4,
                    distance: 4,
                },
            ]
        );
    }

    #[test]
    fn parse_optimal_early_out_is_below_min_match() {
        // The `n < MIN_MATCH(4)` early-out returns the greedy list verbatim. A
        // 4-pixel run (n == 4, one past the cut) fed a hand-built copy-bearing token
        // list proves the boundary is `< 4`, not `<= 4` / `== 4`: at n == 4 the DP
        // runs and emits four literals, never the early return's verbatim copy list.
        let a = 0x1234_5678u32;
        let pixels = [a, a, a, a];
        let chain = super::HashChain::build(&pixels);
        let fake_greedy = vec![
            Token::Literal(a),
            Token::Copy {
                length: 3,
                distance: 1,
            },
        ];
        let optimal = parse_optimal(&pixels, 1, &fake_greedy, &chain);
        assert_eq!(optimal, vec![Token::Literal(a); 4]);
    }

    proptest! {
        /// `parse` is lossless: decoding its tokens reproduces the pixels, for
        /// both strategies, over arbitrary (repetition-prone) data.
        #[test]
        fn parse_round_trips(
            seed in any::<u64>(),
            len in 1usize..=200,
            palette in 1u32..=6,
        ) {
            let pixels = seeded_pixels(seed, len, palette);
            prop_assert_eq!(&decode_tokens(&parse(&pixels, false)), &pixels);
            prop_assert_eq!(&decode_tokens(&parse(&pixels, true)), &pixels);
        }

        /// The guard-hoisted `find` returns the byte-identical `(best_dist,
        /// best_len)` to the pre-hoist reference at every position — the mechanical
        /// proof that hoisting the quick-reject guard load is behavior-neutral.
        /// Since `find` also backs the greedy `parse`, this pins its byte-identity
        /// directly (both `parse` and `reference_parse` share the hoisted `find`).
        /// Small palettes make long overlapping runs — where the guard is refreshed
        /// most often — frequent; `len == 1` covers the 1x1 / too-short cases.
        #[test]
        fn find_matches_reference(
            seed in any::<u64>(),
            len in 1usize..=400,
            palette in 1u32..=4,
        ) {
            let pixels = seeded_pixels(seed, len, palette);
            let chain = super::HashChain::build(&pixels);
            for i in 0..pixels.len() {
                prop_assert_eq!(
                    chain.find(&pixels, i),
                    find_reference(&chain, &pixels, i),
                    "find mismatch at position {}", i
                );
            }
        }

        /// The production `parse` (lazy-lookahead carry) yields byte-identical
        /// tokens to the straightforward reference for both strategies — the
        /// mechanical proof that the carry optimization is behavior-neutral.
        #[test]
        fn parse_matches_reference(
            seed in any::<u64>(),
            len in 1usize..=200,
            palette in 1u32..=6,
        ) {
            let pixels = seeded_pixels(seed, len, palette);
            prop_assert_eq!(parse(&pixels, false), reference_parse(&pixels, false));
            prop_assert_eq!(parse(&pixels, true), reference_parse(&pixels, true));
        }

        /// The cost-model-driven optimal parse is lossless: decoding the tokens it
        /// emits reproduces the pixels exactly, for arbitrary widths and
        /// repetition-prone data. `width` only shifts the distance plane-code
        /// pricing (steering which copies the DP prefers) — the reconstructed token
        /// stream is a valid `Literal`/`Copy` list regardless, so this pins the
        /// round-trip that makes wiring it as a self-floored candidate safe.
        #[test]
        fn optimal_parse_round_trips(
            seed in any::<u64>(),
            len in 1usize..=200,
            palette in 1u32..=6,
            width in 1u32..=16,
        ) {
            let pixels = seeded_pixels(seed, len, palette);
            let greedy = parse(&pixels, true);
            let chain = super::HashChain::build(&pixels);
            let optimal = parse_optimal(&pixels, width, &greedy, &chain);
            prop_assert_eq!(&decode_tokens(&optimal), &pixels);
        }

        /// Sharing the greedy parse's [`super::HashChain`] with the DP yields
        /// byte-identical tokens to letting the DP build its own. The chain is a
        /// pure function of the pixels, so the shared chain (the fast path) and a
        /// freshly built one (the reference) must agree at every position — this
        /// pins that sharing the chain is behavior-neutral.
        #[test]
        fn parse_optimal_shared_chain_matches_reference(
            seed in any::<u64>(),
            len in 1usize..=200,
            palette in 1u32..=6,
            width in 1u32..=16,
        ) {
            let pixels = seeded_pixels(seed, len, palette);
            let (greedy, chain) = parse_lz77(&pixels);
            let shared = parse_optimal(&pixels, width, &greedy, &chain);
            let reference = parse_optimal_reference(&pixels, width, &greedy);
            prop_assert_eq!(shared, reference);
        }

        /// The memoized `find_candidates` pushes a byte-identical candidate vector
        /// to the scan-only reference at every position, driven through the real
        /// backward memo lifecycle. Since `parse_optimal`'s tokens are a pure
        /// function of these vectors (the `CostModel` is unchanged), identical
        /// vectors at every position prove identical tokens, hence identical bytes.
        /// Small palettes make long overlapping runs — the memo's target regime —
        /// frequent.
        #[test]
        fn find_candidates_matches_reference(
            seed in any::<u64>(),
            len in 1usize..=400,
            palette in 1u32..=4,
        ) {
            let pixels = seeded_pixels(seed, len, palette);
            let chain = super::HashChain::build(&pixels);
            let mut memo = super::ShiftMemo::default();
            let (mut got, mut want) = (Vec::new(), Vec::new());
            for i in (0..pixels.len()).rev() {
                chain.find_candidates(&pixels, i, &mut got, &mut memo);
                find_candidates_reference(&chain, &pixels, i, &mut want);
                prop_assert_eq!(&got, &want, "candidate mismatch at position {}", i);
                memo.advance();
            }
        }
    }
}
