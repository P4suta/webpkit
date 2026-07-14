//! Prefix-code serialization — the exact inverse of the decoder's
//! `read_huffman_code` / `read_code_lengths` (see [`crate::lossless::huffman::decode`]).
//!
//! A VP8L prefix code is transmitted in one of two forms:
//!
//! * **Simple** — up to two literal symbols, each with an implied code length
//!   of one. Only usable when the alphabet has `<= 2` used symbols and both
//!   symbol indices fit in 8 bits (`< 256`).
//! * **Normal (full)** — the per-symbol code lengths are themselves run-length
//!   encoded into *code-length-code* tokens (RLE codes 16/17/18 plus literal
//!   lengths 0..=15), and that token stream is transmitted with its own small
//!   Huffman code over the 19-symbol code-length alphabet.
//!
//! The single-used-symbol trap (trap #1 in the plan) is handled entirely in
//! [`crate::lossless::huffman::canonical::emit_codes`]: an alphabet with `<= 1` used symbol
//! emits **zero bits** per occurrence, and [`crate::lossless::bit_io::writer::BitWriter`]'s
//! `write_bits(_, 0)` is a strict no-op, so a lone symbol costs nothing here too.

use crate::lossless::bit_io::writer::BitWriter;
use crate::lossless::constants::{
    CODE_LENGTH_CODE_ORDER, CODE_LENGTH_CODES, CODE_LENGTH_EXTRA_BITS, DEFAULT_CODE_LENGTH,
};
use crate::lossless::huffman::build::build_code_lengths;
use crate::lossless::huffman::canonical::emit_codes;
use crate::lossless::prelude::*;

/// Smallest count of code-length-code lengths the format can transmit: the field
/// is sent as `read_bits(4) + 4`, so trimming trailing zeros never drops below 4.
const MIN_CODE_LENGTH_CODES: usize = 4;

/// Length limit for the code-length (meta) Huffman code. The decoder reads it
/// with a 7-bit root table and transmits each length in 3 bits, so every meta
/// code length must be `<= 7`.
const CODE_LENGTH_CODE_LIMIT: u32 = 7;

/// Largest symbol index expressible by the simple form's 8-bit symbol field.
const SIMPLE_SYMBOL_LIMIT: u32 = 256;

/// One run-length-encoded code-length token (libwebp `HuffmanTreeToken`).
struct Token {
    /// Code-length-code symbol: a literal length (`0..=15`) or a repeat code
    /// (`16` = repeat previous, `17`/`18` = repeat zero).
    code: u32,
    /// Extra bits carried by a repeat code; always `0` for a literal length.
    extra: u32,
}

/// Serialize one prefix code (simple or normal form) for `lengths`, the exact
/// inverse of [`crate::lossless::huffman::decode::read_huffman_code`].
///
/// `lengths` holds one code length per alphabet symbol (`0` = unused); values
/// must be valid VP8L code lengths (`<= 15`), as produced by
/// [`build_code_lengths`]. The simple form is chosen when at most two symbols are
/// used and both indices are `< 256`; otherwise the normal (full) form is used.
pub(crate) fn write_huffman_code(bw: &mut BitWriter, lengths: &[u32]) {
    let mut count = 0usize;
    let mut sym = [0u32; 2];
    for (index, &len) in lengths.iter().enumerate() {
        if len != 0 {
            if count < 2 {
                // Alphabet sizes are far below `u32::MAX`; the fallback can never
                // trigger and would only steer this symbol to the full form.
                sym[count] = u32::try_from(index).unwrap_or(u32::MAX);
            }
            count += 1;
            if count == 3 {
                break; // three used symbols already forces the full form
            }
        }
    }

    if count <= 2 && sym[0] < SIMPLE_SYMBOL_LIMIT && sym[1] < SIMPLE_SYMBOL_LIMIT {
        write_simple_code(bw, count, sym[0], sym[1]);
    } else {
        write_full_code(bw, lengths);
    }
}

/// Write the simple form: 1 or 2 explicit symbols, each with implied length 1.
///
/// `count == 0` (an all-zero alphabet) is written as a single symbol `0`, so the
/// decoder still builds a valid one-symbol (zero-bit) code.
fn write_simple_code(bw: &mut BitWriter, count: usize, sym0: u32, sym1: u32) {
    bw.write_bits(1, 1); // simple form
    bw.write_bits(u32::from(count == 2), 1); // num_symbols - 1 (0 for 0 or 1 symbols)
    if sym0 <= 1 {
        bw.write_bits(0, 1); // first symbol fits in 1 bit
        bw.write_bits(sym0, 1);
    } else {
        bw.write_bits(1, 1); // first symbol needs 8 bits
        bw.write_bits(sym0, 8);
    }
    if count == 2 {
        bw.write_bits(sym1, 8);
    }
}

/// Write the normal (full) form: RLE the code lengths into tokens, transmit the
/// meta Huffman code over the 19-symbol code-length alphabet, then the tokens.
fn write_full_code(bw: &mut BitWriter, lengths: &[u32]) {
    bw.write_bits(0, 1); // normal (full) form

    let tokens = compress_code_lengths(lengths);

    let mut hist = [0u32; CODE_LENGTH_CODES];
    for token in &tokens {
        hist[token.code as usize] += 1;
    }
    let cl_lengths = build_code_lengths(&hist, CODE_LENGTH_CODE_LIMIT);

    // Trim trailing code-length codes whose length is zero (in transmission
    // order), keeping at least the format minimum.
    let mut codes_to_store = CODE_LENGTH_CODES;
    while codes_to_store > MIN_CODE_LENGTH_CODES
        && cl_lengths[usize::from(CODE_LENGTH_CODE_ORDER[codes_to_store - 1])] == 0
    {
        codes_to_store -= 1;
    }

    // `codes_to_store - 4` is in `0..=15`, so the conversion never fails.
    bw.write_bits(
        u32::try_from(codes_to_store - MIN_CODE_LENGTH_CODES).unwrap_or(0),
        4,
    );
    for &order in CODE_LENGTH_CODE_ORDER.iter().take(codes_to_store) {
        bw.write_bits(cl_lengths[usize::from(order)], 3);
    }

    let cl_codes = emit_codes(&cl_lengths);

    // write_trimmed_length = 0: the encoder skips the max_symbol optimization, so
    // every token is transmitted (the decoder defaults max_symbol to num_symbols).
    bw.write_bits(0, 1);

    for token in &tokens {
        let (code, emit_len) = cl_codes[token.code as usize];
        bw.write_bits(code, emit_len);
        if token.code >= 16 {
            let width = u32::from(CODE_LENGTH_EXTRA_BITS[(token.code - 16) as usize]);
            bw.write_bits(token.extra, width);
        }
    }
}

/// Run-length encode `lengths` into code-length-code tokens (libwebp
/// `VP8LCreateCompressedHuffmanTree`). The RLE previous-value seed is
/// [`DEFAULT_CODE_LENGTH`] (`8`), matching the decoder.
#[must_use]
fn compress_code_lengths(lengths: &[u32]) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut prev_value = DEFAULT_CODE_LENGTH;
    let mut i = 0usize;
    while i < lengths.len() {
        let value = lengths[i];
        let mut k = i + 1;
        while k < lengths.len() && lengths[k] == value {
            k += 1;
        }
        // Run length is bounded by the alphabet size, so it always fits in u32.
        let runs = u32::try_from(k - i).unwrap_or(u32::MAX);
        if value == 0 {
            code_repeated_zeros(runs, &mut tokens);
        } else {
            code_repeated_values(runs, value, prev_value, &mut tokens);
            prev_value = value;
        }
        i = k;
    }
    tokens
}

/// Encode a run of `repetitions` zero code lengths (libwebp `CodeRepeatedZeros`).
fn code_repeated_zeros(mut repetitions: u32, tokens: &mut Vec<Token>) {
    while repetitions >= 1 {
        if repetitions < 3 {
            for _ in 0..repetitions {
                tokens.push(Token { code: 0, extra: 0 });
            }
            return;
        }
        if repetitions < 11 {
            tokens.push(Token {
                code: 17,
                extra: repetitions - 3,
            });
            return;
        }
        if repetitions < 139 {
            tokens.push(Token {
                code: 18,
                extra: repetitions - 11,
            });
            return;
        }
        tokens.push(Token {
            code: 18,
            extra: 0x7f, // 138 zeros
        });
        repetitions -= 138;
    }
}

/// Encode a run of `repetitions` copies of a non-zero `value` (libwebp
/// `CodeRepeatedValues`). When `value` differs from `prev_value` the first copy
/// is emitted as a literal before any repeat code can be used.
fn code_repeated_values(
    mut repetitions: u32,
    value: u32,
    prev_value: u32,
    tokens: &mut Vec<Token>,
) {
    if value != prev_value {
        tokens.push(Token {
            code: value,
            extra: 0,
        });
        repetitions -= 1;
    }
    while repetitions >= 1 {
        if repetitions < 3 {
            for _ in 0..repetitions {
                tokens.push(Token {
                    code: value,
                    extra: 0,
                });
            }
            return;
        }
        if repetitions < 7 {
            tokens.push(Token {
                code: 16,
                extra: repetitions - 3,
            });
            return;
        }
        tokens.push(Token {
            code: 16,
            extra: 3, // 6 repeats
        });
        repetitions -= 6;
    }
}

#[cfg(test)]
mod tests {
    use super::write_huffman_code;
    use crate::lossless::bit_io::reader::BitReader;
    use crate::lossless::bit_io::writer::BitWriter;
    use crate::lossless::huffman::canonical::emit_codes;
    use crate::lossless::huffman::decode::read_huffman_code;

    /// A distinctive bit pattern appended after the serialized code so the
    /// round-trip can prove the reader consumed *exactly* the written bits.
    const SENTINEL: u32 = 0b1_0110;
    /// Bit width of [`SENTINEL`].
    const SENTINEL_BITS: u32 = 5;

    /// Serialize `lengths`, append the canonical code for every used symbol in
    /// ascending order, then decode it back and assert each symbol reappears and
    /// the sentinel survives (i.e. bit accounting is exact).
    fn round_trip(lengths: &[u32]) {
        let mut bw = BitWriter::new();
        write_huffman_code(&mut bw, lengths);
        let codes = emit_codes(lengths);
        for (sym, &len) in lengths.iter().enumerate() {
            if len != 0 {
                let (code, emit_len) = codes[sym];
                bw.write_bits(code, emit_len);
            }
        }
        bw.write_bits(SENTINEL, SENTINEL_BITS);
        let bytes = bw.into_bytes();

        let mut br = BitReader::new(&bytes);
        let table = read_huffman_code(&mut br, lengths.len())
            .expect("write_huffman_code must emit a decodable prefix code");
        for (sym, &len) in lengths.iter().enumerate() {
            if len != 0 {
                assert_eq!(
                    table.read_symbol(&mut br) as usize,
                    sym,
                    "symbol {sym} must round-trip"
                );
            }
        }
        assert_eq!(
            br.read_bits(SENTINEL_BITS),
            SENTINEL,
            "reader must consume exactly the bits the writer produced"
        );
    }

    #[test]
    fn single_symbol_costs_zero_bits_per_occurrence() {
        // One used symbol -> simple form, and emit_codes yields (0, 0).
        let mut lengths = vec![0u32; 32];
        lengths[5] = 1;
        round_trip(&lengths);
    }

    #[test]
    fn two_symbols_one_bit_first_symbol() {
        // Symbol 1 (<=1 -> 1-bit field) and symbol 5; simple form.
        let mut lengths = vec![0u32; 16];
        lengths[1] = 1;
        lengths[5] = 1;
        round_trip(&lengths);
    }

    #[test]
    fn two_symbols_eight_bit_first_symbol() {
        // Both symbols > 1 and < 256 -> simple form, 8-bit symbol fields.
        let mut lengths = vec![0u32; 256];
        lengths[3] = 1;
        lengths[200] = 1;
        round_trip(&lengths);
    }

    #[test]
    fn symbol_at_or_above_256_forces_full_form() {
        // Two used symbols but one index >= 256 cannot use the simple form.
        let mut lengths = vec![0u32; 280];
        lengths[5] = 1;
        lengths[260] = 1;
        round_trip(&lengths);
    }

    #[test]
    fn mixed_lengths_use_the_full_form() {
        // A valid complete prefix code: 1/2 + 1/4 + 1/8 + 1/8 = 1.
        round_trip(&[1, 2, 3, 3]);
    }

    #[test]
    fn green_alphabet_with_trailing_zeros() {
        // 280-symbol green alphabet: a complete code over the first four symbols,
        // then a long trailing zero run (exercises the code-18 138-zero loop).
        let mut lengths = vec![0u32; 280];
        lengths[..4].copy_from_slice(&[1, 2, 3, 3]);
        round_trip(&lengths);
    }

    #[test]
    fn all_length_eight_is_a_single_token_kind() {
        // 256 symbols all length 8 -> a complete code whose RLE is nothing but
        // code-16 tokens, so the meta code has a single used symbol (0, 0).
        round_trip(&[8u32; 256]);
    }

    #[test]
    fn all_zero_alphabet_decodes_symbol_zero_with_no_bits() {
        let lengths = [0u32; 40];
        let mut bw = BitWriter::new();
        write_huffman_code(&mut bw, &lengths);
        bw.write_bits(SENTINEL, SENTINEL_BITS); // sentinel right after the code
        let bytes = bw.into_bytes();

        let mut br = BitReader::new(&bytes);
        let table = read_huffman_code(&mut br, lengths.len()).expect("simple code");
        assert_eq!(table.read_symbol(&mut br), 0);
        // The sentinel is intact only if read_symbol consumed zero bits.
        assert_eq!(br.read_bits(SENTINEL_BITS), SENTINEL);
    }

    /// The decoder's private `BitBuf` test oracle, replicated so we can compare
    /// byte-for-byte against its `put_simple_code`.
    #[derive(Default)]
    struct BitBuf {
        bytes: Vec<u8>,
        acc: u32,
        n: u32,
    }
    impl BitBuf {
        fn put(&mut self, value: u32, bits: u32) {
            self.acc |= value << self.n;
            self.n += bits;
            while self.n >= 8 {
                self.bytes.push((self.acc & 0xff) as u8);
                self.acc >>= 8;
                self.n -= 8;
            }
        }
        fn finish(mut self) -> Vec<u8> {
            if self.n > 0 {
                self.bytes.push((self.acc & 0xff) as u8);
            }
            self.bytes
        }
    }

    /// Verbatim copy of `vp8l::decode::tests::put_simple_code`.
    fn put_simple_code(b: &mut BitBuf, symbol: u32) {
        b.put(1, 1); // simple code
        b.put(0, 1); // num_symbols - 1 == 0 (one symbol)
        if symbol <= 1 {
            b.put(0, 1); // first_symbol_len_code: 1-bit value
            b.put(symbol, 1);
        } else {
            b.put(1, 1); // 8-bit value
            b.put(symbol, 8);
        }
    }

    #[test]
    fn single_symbol_at_index_256_forces_full_form() {
        // Boundary for `sym[0] < SIMPLE_SYMBOL_LIMIT` (256): index 256 does NOT
        // fit the simple form's 8-bit field, so the full form must be used. The
        // `<` -> `<=` mutant would take the simple form and write 256 in 8 bits,
        // which the writer masks to 0 -> the decoded symbol becomes 0, not 256.
        let mut lengths = vec![0u32; 257];
        lengths[256] = 1;
        round_trip(&lengths);
    }

    #[test]
    fn second_symbol_at_index_256_forces_full_form() {
        // Boundary for `sym[1] < SIMPLE_SYMBOL_LIMIT` (256): the first symbol is
        // < 256 (so the guard reaches the second comparison) and the second is
        // exactly 256. The `<` -> `<=` mutant on the second comparison would pick
        // the simple form and truncate 256 to 0 in its 8-bit field, so the second
        // symbol would decode as 0 instead of 256.
        let mut lengths = vec![0u32; 257];
        lengths[5] = 1;
        lengths[256] = 1;
        round_trip(&lengths);
    }

    #[test]
    fn simple_single_symbol_matches_decoder_oracle() {
        // For a one-symbol code, write_huffman_code must be byte-identical to the
        // decoder's own put_simple_code, for both the 1-bit and 8-bit branches.
        for symbol in [0u32, 1, 5, 200] {
            let mut lengths = vec![0u32; 256];
            lengths[symbol as usize] = 1;

            let mut bw = BitWriter::new();
            write_huffman_code(&mut bw, &lengths);
            let ours = bw.into_bytes();

            let mut oracle = BitBuf::default();
            put_simple_code(&mut oracle, symbol);
            assert_eq!(ours, oracle.finish(), "symbol {symbol}");
        }
    }
}

#[cfg(test)]
mod proptests {
    use super::write_huffman_code;
    use crate::lossless::bit_io::reader::BitReader;
    use crate::lossless::bit_io::writer::BitWriter;
    use crate::lossless::huffman::build::build_code_lengths;
    use crate::lossless::huffman::canonical::emit_codes;
    use crate::lossless::huffman::decode::read_huffman_code;
    use proptest::prelude::*;

    proptest! {
        /// For any histogram, the code lengths produced by `build_code_lengths`
        /// serialize (in either form) to a stream the decoder reconstructs, and
        /// every used symbol round-trips through its canonical code.
        #[test]
        fn any_built_code_round_trips(
            weights in proptest::collection::vec(0u32..12, 1..300usize)
        ) {
            let lengths = build_code_lengths(&weights, 15);

            let mut bw = BitWriter::new();
            write_huffman_code(&mut bw, &lengths);
            let codes = emit_codes(&lengths);
            for (sym, &len) in lengths.iter().enumerate() {
                if len != 0 {
                    let (code, emit_len) = codes[sym];
                    bw.write_bits(code, emit_len);
                }
            }
            let bytes = bw.into_bytes();

            let mut br = BitReader::new(&bytes);
            let table = read_huffman_code(&mut br, lengths.len());
            prop_assert!(table.is_some());
            let table = table.expect("checked is_some above");
            for (sym, &len) in lengths.iter().enumerate() {
                if len != 0 {
                    prop_assert_eq!(table.read_symbol(&mut br) as usize, sym);
                }
            }
        }
    }
}
