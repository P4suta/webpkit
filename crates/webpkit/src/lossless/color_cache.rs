//! VP8L color cache: a hash-indexed table of recently emitted ARGB pixels.
//!
//! Every produced pixel (literal, back-reference copy, or cache hit) is inserted
//! into the cache in output order. The decoder and encoder must insert in
//! identical order or the indices desynchronise, so this lives in one place.

use crate::lossless::constants::HASH_MUL;
use crate::lossless::prelude::*;

/// A direct-mapped ARGB cache of `1 << bits` entries.
pub(crate) struct ColorCache {
    entries: Vec<u32>,
    bits: u32,
}

impl ColorCache {
    /// Create an empty cache with `1 << bits` slots (`bits` in `1..=11`).
    pub(crate) fn new(bits: u32) -> Self {
        debug_assert!((1..=crate::lossless::constants::MAX_CACHE_BITS).contains(&bits));
        Self {
            entries: alloc_zeroed(1usize << bits),
            bits,
        }
    }

    /// Reset to an empty cache of `1 << bits` slots, reusing the existing backing
    /// allocation (which must already hold at least `1 << bits` entries). This
    /// lets one max-size scratch cache serve every candidate cache size without
    /// reallocating; only the `1 << bits` live slots are cleared (higher slots are
    /// never indexed at this `bits`).
    pub(crate) fn reset(&mut self, bits: u32) {
        debug_assert!((1..=crate::lossless::constants::MAX_CACHE_BITS).contains(&bits));
        debug_assert!(self.entries.len() >= 1usize << bits);
        self.bits = bits;
        self.entries[..1usize << bits]
            .iter_mut()
            .for_each(|e| *e = 0);
    }

    /// Hash an ARGB value to its slot index for a cache of `bits` bits.
    pub(crate) const fn index(argb: u32, bits: u32) -> usize {
        (HASH_MUL.wrapping_mul(argb) >> (32 - bits)) as usize
    }

    /// Insert a produced pixel into its hashed slot.
    pub(crate) fn insert(&mut self, argb: u32) {
        let i = Self::index(argb, self.bits);
        self.entries[i] = argb;
    }

    /// Fetch the pixel stored at a cache index (the value of a color-cache code).
    ///
    /// `index` comes from an untrusted color-cache code; a bounds-checked read
    /// keeps a hostile code from panicking (the parser sizes the cache to the
    /// Huffman alphabet, so a valid stream never takes the fallback), matching the
    /// zero-fill fallback style of the rest of the decoder.
    pub(crate) fn get(&self, index: usize) -> u32 {
        self.entries.get(index).copied().unwrap_or(0)
    }
}

/// Allocate a zero-filled `u32` buffer of `len` entries.
fn alloc_zeroed(len: usize) -> Vec<u32> {
    vec![0u32; len]
}

#[cfg(test)]
mod tests {
    use super::ColorCache;

    #[test]
    fn insert_then_get_round_trips() {
        let mut cache = ColorCache::new(11);
        let argb = 0xAABB_CCDD;
        cache.insert(argb);
        assert_eq!(cache.get(ColorCache::index(argb, 11)), argb);
    }

    #[test]
    fn index_is_deterministic() {
        let argb = 0x1234_5678;
        assert_eq!(ColorCache::index(argb, 10), ColorCache::index(argb, 10));
    }

    #[test]
    fn index_matches_hash_formula() {
        // (0x1e35a7bd * 1) >> 31 == 0 because the product's top bit is clear.
        assert_eq!(ColorCache::index(1, 1), 0);
        // General formula check for a second value / width.
        let argb = 0x00FF_00FFu32;
        let expected = (0x1e35_a7bdu32.wrapping_mul(argb) >> (32 - 8)) as usize;
        assert_eq!(ColorCache::index(argb, 8), expected);
    }

    #[test]
    fn cache_size_follows_bits() {
        let cache = ColorCache::new(4);
        // Index is always in range for the declared width.
        for v in 0..1000u32 {
            let i = ColorCache::index(v.wrapping_mul(0x9E37), 4);
            assert!(i < (1 << 4));
            let _ = cache.get(i);
        }
    }
}
