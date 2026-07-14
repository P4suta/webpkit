//! Canonical Huffman code assignment and the encoder's single bit-reversal site.
//!
//! VP8L prefix codes are transmitted MSB-first in the *canonical* sense (shorter
//! codes numerically precede longer ones, and within a length symbols are ordered
//! by index), yet the bitstream itself is read least-significant-bit-first. The
//! decoder ([`crate::lossless::huffman::decode`]) reconciles this by building a
//! *bit-reversed* lookup table; the encoder reconciles it here, in exactly one
//! place, by reversing each canonical code before it is written LSB-first.
//!
//! Keeping bit-reversal confined to [`reverse_bits`] (called only from
//! [`emit_codes`]) means the encode/decode bit convention has a single source of
//! truth. [`canonical_codes`] mirrors the standard "count → `next_code` → assign"
//! construction that the decoder's build implements implicitly, and is the
//! encoder's promotion of the `canonical_codes` test oracle in
//! `crate::lossless::huffman::decode`.
//!
//! # The single-symbol trap
//!
//! [`crate::lossless::huffman::decode::HuffmanTable::build`] represents an alphabet with a
//! single used symbol as a **zero-bit leaf**: decoding it consumes no bits. A
//! length-limited build cannot express "length 0" for a used symbol (0 means
//! *unused*), so such a symbol arrives here with length 1. Emitting one bit per
//! occurrence would immediately desynchronise the decoder, which expects zero.
//! [`emit_codes`] therefore returns `(0, 0)` for every symbol whenever the
//! alphabet has at most one used symbol (libwebp's
//! `ClearHuffmanTreeIfOnlyOneSymbol`), and [`crate::lossless::bit_io::writer::BitWriter`]
//! treats a zero-width write as a strict no-op.

use crate::lossless::prelude::*;

/// Reverse the low `len` bits of `v`, discarding higher bits.
///
/// Canonical codes are computed MSB-first but the VP8L bitstream is LSB-first, so
/// each code is reversed exactly once, here, before being written. `reverse_bits`
/// is an involution on values that fit in `len` bits, and `len == 0` yields `0`.
#[must_use]
pub(crate) const fn reverse_bits(v: u32, len: u32) -> u32 {
    let mut out = 0u32;
    let mut i = 0u32;
    while i < len {
        out |= ((v >> i) & 1) << (len - 1 - i);
        i += 1;
    }
    out
}

/// Assign standard MSB-first canonical codes for the per-symbol `lengths`.
///
/// Returns `codes[symbol]` for every position; entries for unused symbols
/// (length 0) are `0` and carry no meaning. Codes are assigned in increasing
/// numeric order by `(length, symbol-index)`, so the result is prefix-free and
/// complete exactly when `lengths` is (the caller guarantees validity; this
/// function does not check it). This is the exact convention the decoder's
/// bit-reversed table build reproduces.
#[must_use]
pub(crate) fn canonical_codes(lengths: &[u32]) -> Vec<u32> {
    let max_len = lengths.iter().copied().max().unwrap_or(0) as usize;
    let mut count = vec![0u32; max_len + 1];
    for &l in lengths {
        if l > 0 {
            count[l as usize] += 1;
        }
    }
    // Smallest code of each length: next[len] = (next[len-1's total] ) << 1.
    let mut next = vec![0u32; max_len + 2];
    let mut code = 0u32;
    for len in 1..=max_len {
        code = (code + count[len - 1]) << 1;
        next[len] = code;
    }
    let mut codes = vec![0u32; lengths.len()];
    for (sym, &l) in lengths.iter().enumerate() {
        if l > 0 {
            codes[sym] = next[l as usize];
            next[l as usize] += 1;
        }
    }
    codes
}

/// Produce the `(lsb-first code, emit-length)` pair to write for each symbol.
///
/// For a normal alphabet (two or more used symbols) each used symbol maps to
/// `(reverse_bits(canonical_code, length), length)` and each unused symbol to
/// `(0, 0)`. When at most one symbol is used the whole alphabet collapses to
/// `(0, 0)` — the decoder builds a zero-bit leaf, so every occurrence must emit
/// zero bits (see the module-level "single-symbol trap"). The returned vector is
/// parallel to `lengths`.
#[must_use]
pub(crate) fn emit_codes(lengths: &[u32]) -> Vec<(u32, u32)> {
    let used = lengths.iter().filter(|&&l| l > 0).count();
    if used <= 1 {
        // ClearHuffmanTreeIfOnlyOneSymbol: a lone (or absent) symbol is a
        // zero-bit leaf on decode, so it must occupy zero bits on encode.
        return vec![(0, 0); lengths.len()];
    }
    let canon = canonical_codes(lengths);
    lengths
        .iter()
        .zip(canon.iter())
        .map(|(&len, &code)| {
            if len > 0 {
                (reverse_bits(code, len), len)
            } else {
                (0, 0)
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{canonical_codes, emit_codes, reverse_bits};
    use crate::lossless::bit_io::reader::BitReader;
    use crate::lossless::bit_io::writer::BitWriter;
    use crate::lossless::constants::HUFFMAN_TABLE_BITS;
    use crate::lossless::huffman::decode::HuffmanTable;

    #[test]
    fn reverse_bits_known_values() {
        assert_eq!(reverse_bits(0b1, 1), 0b1);
        assert_eq!(reverse_bits(0b10, 2), 0b01);
        assert_eq!(reverse_bits(0b110, 3), 0b011);
        assert_eq!(reverse_bits(0b1011, 4), 0b1101);
        // High bits beyond `len` are ignored.
        assert_eq!(reverse_bits(0b1111_0010, 3), 0b010);
        // Zero-length reversal is empty.
        assert_eq!(reverse_bits(0x7, 0), 0);
    }

    #[test]
    fn reverse_bits_is_an_involution() {
        for len in 1u32..=15 {
            let mask = (1u32 << len) - 1;
            for v in [0u32, 1, mask, mask ^ 1, 0xA5A5_A5A5 & mask] {
                assert_eq!(reverse_bits(reverse_bits(v, len), len), v);
            }
        }
    }

    #[test]
    fn canonical_codes_assigns_standard_order() {
        // Classic 1/2 + 1/4 + 1/8 + 1/8 code.
        assert_eq!(canonical_codes(&[1, 2, 3, 3]), vec![0, 2, 6, 7]);
        // Unused symbols (length 0) keep a placeholder 0.
        assert_eq!(canonical_codes(&[2, 0, 1, 2]), vec![2, 0, 0, 3]);
    }

    #[test]
    fn emit_codes_single_symbol_is_all_zero_bits() {
        let mut lengths = vec![0u32; 10];
        lengths[5] = 1; // the one used symbol
        let codes = emit_codes(&lengths);
        assert!(
            codes.iter().all(|&pair| pair == (0, 0)),
            "a single-symbol alphabet must emit zero bits per occurrence"
        );

        // And it decodes back with no bits consumed.
        let table = HuffmanTable::build(&lengths, HUFFMAN_TABLE_BITS)
            .expect("single-symbol code builds a zero-bit leaf");
        let (code, emit_len) = codes[5];
        let mut w = BitWriter::new();
        w.write_bits(code, emit_len); // no-op
        let bytes = w.into_bytes();
        assert!(bytes.is_empty(), "no bits should be emitted");
        let mut br = BitReader::new(&bytes);
        assert_eq!(table.read_symbol(&mut br), 5);
        assert!(!br.is_eos(), "reading a zero-bit leaf consumes nothing");
    }

    #[test]
    fn emit_codes_empty_alphabet_is_all_zero_bits() {
        assert_eq!(emit_codes(&[0, 0, 0]), vec![(0, 0), (0, 0), (0, 0)]);
    }

    #[test]
    fn emit_codes_two_equal_length_symbols() {
        // Two symbols of length 1: sym0 -> 0, sym1 -> 1, each one bit.
        assert_eq!(emit_codes(&[1, 1]), vec![(0, 1), (1, 1)]);
    }

    #[test]
    fn emit_codes_matches_reversed_canonical() {
        let lengths = [1u32, 2, 3, 3];
        let canon = canonical_codes(&lengths);
        let emitted = emit_codes(&lengths);
        for (sym, &len) in lengths.iter().enumerate() {
            assert_eq!(emitted[sym], (reverse_bits(canon[sym], len), len));
        }
    }

    /// Serialize every used symbol of `lengths` through `emit_codes` and assert
    /// the decoder's `HuffmanTable` reads each one back. Trailing zero padding
    /// from `into_bytes` is harmless because the code is prefix-free.
    fn assert_round_trips(lengths: &[u32]) {
        let table = HuffmanTable::build(lengths, HUFFMAN_TABLE_BITS)
            .expect("test length vectors are valid prefix codes");
        let codes = emit_codes(lengths);
        for (sym, &len) in lengths.iter().enumerate() {
            if len == 0 {
                continue;
            }
            let (code, emit_len) = codes[sym];
            let mut w = BitWriter::new();
            w.write_bits(code, emit_len);
            let bytes = w.into_bytes();
            let mut br = BitReader::new(&bytes);
            assert_eq!(
                table.read_symbol(&mut br) as usize,
                sym,
                "symbol {sym} (len {len}) must round-trip"
            );
        }
    }

    #[test]
    fn round_trips_representative_codes() {
        assert_round_trips(&[1, 1]);
        assert_round_trips(&[1, 2, 2]);
        assert_round_trips(&[1, 2, 3, 3]);
        assert_round_trips(&[2, 2, 2, 2]);
        assert_round_trips(&[1, 2, 4, 4, 4, 4]);
        assert_round_trips(&[3, 3, 3, 3, 3, 3, 3, 3]);
        // Sparse alphabet with gaps of unused symbols.
        assert_round_trips(&[0, 1, 0, 2, 0, 2]);
        // Single used symbol amid unused ones (zero-bit leaf).
        assert_round_trips(&[0, 0, 1, 0]);
    }

    #[test]
    fn canonical_codes_skips_unused_symbols() {
        // Two used length-1 symbols separated by two unused (length-0) slots.
        // The length guard must ignore length-0 symbols: if it admitted them,
        // the second unused slot would consume `next[0]` and be assigned 1
        // instead of the placeholder 0, yielding `[0, 0, 1, 1]`.
        assert_eq!(canonical_codes(&[1, 0, 1, 0]), vec![0, 0, 1, 0]);
    }
}

#[cfg(test)]
mod proptests {
    use super::{emit_codes, reverse_bits};
    use crate::lossless::bit_io::reader::BitReader;
    use crate::lossless::bit_io::writer::BitWriter;
    use crate::lossless::constants::HUFFMAN_TABLE_BITS;
    use crate::lossless::huffman::decode::HuffmanTable;
    use proptest::prelude::*;

    /// Deterministically build a *complete* Huffman length vector with `n` used
    /// symbols (`n >= 2`) from `seed` by repeatedly splitting a random leaf of a
    /// growing binary tree. Kraft equality holds by construction, so the result
    /// is always accepted by `HuffmanTable::build`; only leaves shallower than 15
    /// are split, so every depth stays within `MAX_ALLOWED_CODE_LENGTH`.
    fn build_complete_lengths(n: usize, seed: u64) -> Vec<u32> {
        let mut state = seed ^ 0x9e37_79b9_7f4a_7c15;
        let mut depths = vec![1u32, 1u32];
        while depths.len() < n {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let candidates: Vec<usize> = (0..depths.len()).filter(|&i| depths[i] < 15).collect();
            let idx = usize::try_from(state % candidates.len() as u64)
                .expect("modulus is < candidates.len(), which fits usize");
            let pick = candidates[idx];
            depths[pick] += 1;
            depths.push(depths[pick]);
        }
        depths
    }

    proptest! {
        /// Reversing the low `len` bits twice is the identity, and the result
        /// never sets any bit at or above `len`.
        #[test]
        fn reverse_bits_involution_over_len(len in 1u32..=15, raw in any::<u32>()) {
            let mask = (1u32 << len) - 1;
            let v = raw & mask;
            prop_assert_eq!(reverse_bits(reverse_bits(v, len), len), v);
            prop_assert_eq!(reverse_bits(v, len) & !mask, 0);
        }

        /// Every symbol of a random complete canonical code, emitted LSB-first,
        /// is decoded back to itself by the decoder's `HuffmanTable`.
        #[test]
        fn canonical_codes_round_trip_through_decoder(
            n in 2usize..=64,
            seed in any::<u64>(),
        ) {
            let lengths = build_complete_lengths(n, seed);
            let table = HuffmanTable::build(&lengths, HUFFMAN_TABLE_BITS)
                .expect("a complete code always builds");
            let codes = emit_codes(&lengths);
            for (sym, &len) in lengths.iter().enumerate() {
                let (code, emit_len) = codes[sym];
                // Every symbol is used here, so the emit length equals the code
                // length (no single-symbol collapse).
                prop_assert_eq!(emit_len, len);
                let mut w = BitWriter::new();
                w.write_bits(code, emit_len);
                let bytes = w.into_bytes();
                let mut br = BitReader::new(&bytes);
                prop_assert_eq!(table.read_symbol(&mut br) as usize, sym);
            }
        }
    }
}
