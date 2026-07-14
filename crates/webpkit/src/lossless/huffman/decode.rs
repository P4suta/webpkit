//! Two-level canonical Huffman table build + symbol decode, ported from
//! libwebp 1.6.0 `src/utils/huffman_utils.c` (`BuildHuffmanTable`) and
//! `src/dec/vp8l_dec.c` (`ReadHuffmanCode`, `ReadHuffmanCodeLengths`).

use crate::lossless::bit_io::reader::BitReader;
use crate::lossless::constants::{
    CODE_LENGTH_CODE_ORDER, CODE_LENGTH_CODES, CODE_LENGTH_EXTRA_BITS, CODE_LENGTH_REPEAT_OFFSETS,
    DEFAULT_CODE_LENGTH, HUFFMAN_TABLE_BITS, MAX_ALLOWED_CODE_LENGTH,
};

use crate::lossless::prelude::*;

/// Root-table index width for the code-length-code (meta) table.
const LENGTHS_TABLE_BITS: u32 = 7;

/// One entry of a Huffman lookup table.
///
/// A root entry with `bits <= root_bits` is a leaf: consume `bits` and emit
/// `value` (`bits == 0` is a trivial single-symbol code). A root entry with
/// `bits > root_bits` is a pointer: `value` is the offset from the root base to a
/// second-level table, and `bits - root_bits` is that table's index width.
#[derive(Clone, Copy, Default)]
pub(crate) struct HuffmanCode {
    pub(crate) bits: u8,
    pub(crate) value: u16,
}

/// A built canonical Huffman decode table (root + appended second-level tables).
pub(crate) struct HuffmanTable {
    arena: Vec<HuffmanCode>,
    root_bits: u32,
}

/// Advance a bit-reversed code key by one, for canonical assignment.
const fn get_next_key(key: u32, len: u32) -> u32 {
    let mut step = 1u32 << (len - 1);
    while key & step != 0 {
        step >>= 1;
    }
    if step != 0 {
        (key & (step - 1)) + step
    } else {
        key
    }
}

/// Strided fill: write `code` to `table[end - step]`, `table[end - 2*step]`, ...
/// down to `table[0]`.
fn replicate_value(table: &mut [HuffmanCode], step: usize, end: usize, code: HuffmanCode) {
    let mut e = end;
    loop {
        e -= step;
        table[e] = code;
        if e == 0 {
            break;
        }
    }
}

/// Compute the index width of the next second-level table for length `len`.
fn next_table_bit_size(count: &[i32], mut len: usize, root_bits: usize) -> usize {
    let mut left = 1i32 << (len - root_bits);
    while len < MAX_ALLOWED_CODE_LENGTH {
        left -= count[len];
        if left <= 0 {
            break;
        }
        len += 1;
        left <<= 1;
    }
    len - root_bits
}

impl HuffmanTable {
    /// Build a decode table from per-symbol `code_lengths` (0 = unused symbol).
    ///
    /// Returns `None` for an over- or under-subscribed code, or an all-zero code.
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_possible_wrap,
        clippy::needless_range_loop,
        clippy::too_many_lines,
        reason = "values are bounded by the format (symbols < 2^16, lengths <= 15); the `len` \
                  loop index drives bit-shift arithmetic, so an iterator does not fit; and this \
                  is a faithful single-function port of libwebp BuildHuffmanTable"
    )]
    pub(crate) fn build(code_lengths: &[u32], root_bits: u32) -> Option<Self> {
        let root_bits_usize = root_bits as usize;
        let mut count = [0i32; MAX_ALLOWED_CODE_LENGTH + 1];
        for &cl in code_lengths {
            if cl as usize > MAX_ALLOWED_CODE_LENGTH {
                return None;
            }
            count[cl as usize] += 1;
        }
        if count[0] as usize == code_lengths.len() {
            return None; // all lengths zero
        }

        // Offsets into the sorted-by-(length, symbol) table.
        let mut offset = [0i32; MAX_ALLOWED_CODE_LENGTH + 1];
        for len in 1..MAX_ALLOWED_CODE_LENGTH {
            if count[len] > (1i32 << len) {
                return None; // over-subscribed
            }
            offset[len + 1] = offset[len] + count[len];
        }

        // Counting sort of used symbols by (length, symbol).
        let num_used: usize = code_lengths.iter().filter(|&&c| c > 0).count();
        let mut sorted = vec![0u16; num_used.max(1)];
        {
            let mut cursor = offset;
            for (symbol, &cl) in code_lengths.iter().enumerate() {
                if cl > 0 {
                    sorted[cursor[cl as usize] as usize] = symbol as u16;
                    cursor[cl as usize] += 1;
                }
            }
        }

        let root_size = 1usize << root_bits;

        // Single-symbol code: a 0-bit leaf replicated across the whole root table.
        if num_used == 1 {
            let code = HuffmanCode {
                bits: 0,
                value: sorted[0],
            };
            return Some(Self {
                arena: vec![code; root_size],
                root_bits,
            });
        }

        let mut arena = vec![HuffmanCode::default(); root_size];
        let mask = (root_size - 1) as u32;
        let mut key = 0u32;
        let mut symbol_idx = 0usize;
        let mut num_nodes = 1i32;
        let mut num_open = 1i32;
        let mut table_base = 0usize;
        let mut table_size = root_size;
        let mut low: i64 = -1;

        // Root table: code lengths 1..=root_bits.
        let mut step = 2usize;
        for len in 1..=root_bits_usize {
            num_open <<= 1;
            num_nodes += num_open;
            num_open -= count[len];
            if num_open < 0 {
                return None;
            }
            while count[len] > 0 {
                let code = HuffmanCode {
                    bits: len as u8,
                    value: sorted[symbol_idx],
                };
                symbol_idx += 1;
                replicate_value(&mut arena[key as usize..], step, table_size, code);
                key = get_next_key(key, len as u32);
                count[len] -= 1;
            }
            step <<= 1;
        }

        // Second-level tables: code lengths root_bits+1 ..= 15.
        step = 2;
        for len in (root_bits_usize + 1)..=MAX_ALLOWED_CODE_LENGTH {
            num_open <<= 1;
            num_nodes += num_open;
            num_open -= count[len];
            if num_open < 0 {
                return None;
            }
            while count[len] > 0 {
                if i64::from(key & mask) != low {
                    table_base += table_size;
                    let table_bits = next_table_bit_size(&count, len, root_bits_usize);
                    table_size = 1usize << table_bits;
                    arena.resize(table_base + table_size, HuffmanCode::default());
                    low = i64::from(key & mask);
                    arena[low as usize] = HuffmanCode {
                        bits: (table_bits + root_bits_usize) as u8,
                        value: (table_base as u32 - (key & mask)) as u16,
                    };
                }
                let code = HuffmanCode {
                    bits: (len - root_bits_usize) as u8,
                    value: sorted[symbol_idx],
                };
                symbol_idx += 1;
                replicate_value(
                    &mut arena[table_base + (key >> root_bits) as usize..],
                    step,
                    table_size,
                    code,
                );
                key = get_next_key(key, len as u32);
                count[len] -= 1;
            }
            step <<= 1;
        }

        if num_nodes != 2 * num_used as i32 - 1 {
            return None; // under-subscribed
        }
        Some(Self { arena, root_bits })
    }

    /// Decode one symbol, consuming its bits from `br`.
    pub(crate) fn read_symbol(&self, br: &mut BitReader<'_>) -> u16 {
        let root_index = br.peek_bits(self.root_bits) as usize;
        let entry = self.arena[root_index];
        if u32::from(entry.bits) <= self.root_bits {
            br.consume(u32::from(entry.bits));
            return entry.value;
        }
        // Second-level table.
        br.consume(self.root_bits);
        let extra_bits = u32::from(entry.bits) - self.root_bits;
        let sub_index = root_index + entry.value as usize + br.peek_bits(extra_bits) as usize;
        let leaf = self.arena[sub_index];
        br.consume(u32::from(leaf.bits));
        leaf.value
    }
}

/// Read one prefix code (simple or normal form) over `alphabet_size` symbols.
pub(crate) fn read_huffman_code(
    br: &mut BitReader<'_>,
    alphabet_size: usize,
) -> Option<HuffmanTable> {
    let mut code_lengths = vec![0u32; alphabet_size];
    if br.read_bits(1) != 0 {
        // Simple code: 1 or 2 explicit symbols, each with implied length 1.
        let num_symbols = br.read_bits(1) + 1;
        let first_bits = if br.read_bits(1) == 0 { 1 } else { 8 };
        let sym0 = br.read_bits(first_bits) as usize;
        if sym0 >= alphabet_size {
            return None;
        }
        code_lengths[sym0] = 1;
        if num_symbols == 2 {
            let sym1 = br.read_bits(8) as usize;
            if sym1 >= alphabet_size {
                return None;
            }
            code_lengths[sym1] = 1;
        }
    } else {
        // Normal code: read the code-length-code lengths, then the code lengths.
        let num_code_lengths = (br.read_bits(4) + 4) as usize;
        if num_code_lengths > CODE_LENGTH_CODES {
            return None;
        }
        let mut cl_code_lengths = [0u32; CODE_LENGTH_CODES];
        for &order in CODE_LENGTH_CODE_ORDER.iter().take(num_code_lengths) {
            cl_code_lengths[order as usize] = br.read_bits(3);
        }
        code_lengths = read_code_lengths(br, cl_code_lengths, alphabet_size)?;
    }
    HuffmanTable::build(&code_lengths, HUFFMAN_TABLE_BITS)
}

/// Decode the per-symbol code lengths using the code-length-code table.
fn read_code_lengths(
    br: &mut BitReader<'_>,
    cl_code_lengths: [u32; CODE_LENGTH_CODES],
    num_symbols: usize,
) -> Option<Vec<u32>> {
    let table = HuffmanTable::build(&cl_code_lengths, LENGTHS_TABLE_BITS)?;
    let mut code_lengths = vec![0u32; num_symbols];

    let mut max_symbol = if br.read_bits(1) != 0 {
        let length_nbits = 2 + 2 * br.read_bits(3);
        (2 + br.read_bits(length_nbits)) as usize
    } else {
        num_symbols
    };
    if max_symbol > num_symbols {
        return None;
    }

    let mut prev_len = DEFAULT_CODE_LENGTH;
    let mut symbol = 0usize;
    while symbol < num_symbols {
        if max_symbol == 0 {
            break;
        }
        max_symbol -= 1;
        let code_len = table.read_symbol(br);
        if br.is_eos() {
            return None;
        }
        if code_len < 16 {
            code_lengths[symbol] = u32::from(code_len);
            symbol += 1;
            if code_len != 0 {
                prev_len = u32::from(code_len);
            }
        } else {
            let slot = (code_len - 16) as usize;
            let extra = u32::from(CODE_LENGTH_EXTRA_BITS[slot]);
            let base = u32::from(CODE_LENGTH_REPEAT_OFFSETS[slot]);
            let repeat = (br.read_bits(extra) + base) as usize;
            if symbol + repeat > num_symbols {
                return None;
            }
            let length = if code_len == 16 { prev_len } else { 0 };
            for _ in 0..repeat {
                code_lengths[symbol] = length;
                symbol += 1;
            }
        }
    }
    Some(code_lengths)
}

#[cfg(test)]
mod tests {
    use super::{
        HuffmanTable, get_next_key, next_table_bit_size, read_code_lengths, read_huffman_code,
    };
    use crate::lossless::bit_io::reader::BitReader;
    use crate::lossless::constants::{
        CODE_LENGTH_CODES, HUFFMAN_TABLE_BITS, MAX_ALLOWED_CODE_LENGTH,
    };

    /// Reverse the low `len` bits of `v` (canonical codes are read LSB-first).
    fn reverse_bits(v: u32, len: u32) -> u32 {
        let mut out = 0;
        for i in 0..len {
            out |= ((v >> i) & 1) << (len - 1 - i);
        }
        out
    }

    /// Independently assign standard MSB-first canonical codes for `lengths`,
    /// returning `codes[symbol]` (only meaningful for used symbols).
    fn canonical_codes(lengths: &[u32]) -> Vec<u32> {
        let max_len = *lengths.iter().max().unwrap() as usize;
        let mut count = vec![0u32; max_len + 1];
        for &l in lengths {
            if l > 0 {
                count[l as usize] += 1;
            }
        }
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

    #[test]
    fn single_symbol_code_consumes_no_bits() {
        // Symbol 5 is the only used symbol -> a 0-bit trivial code.
        let mut lengths = vec![0u32; 10];
        lengths[5] = 1;
        let table = HuffmanTable::build(&lengths, HUFFMAN_TABLE_BITS).unwrap();
        let mut br = BitReader::new(&[0xFF]);
        assert_eq!(table.read_symbol(&mut br), 5);
        assert!(!br.is_eos()); // consumed nothing
    }

    #[test]
    fn two_symbol_code_decodes_lsb_first() {
        // Symbols 0 and 1, both length 1.
        let table = HuffmanTable::build(&[1, 1], HUFFMAN_TABLE_BITS).unwrap();
        let mut br0 = BitReader::new(&[0x00]);
        assert_eq!(table.read_symbol(&mut br0), 0);
        let mut br1 = BitReader::new(&[0x01]);
        assert_eq!(table.read_symbol(&mut br1), 1);
    }

    #[test]
    fn multi_length_code_matches_independent_canonical() {
        // A valid prefix code: 1/2 + 1/4 + 1/8 + 1/8 = 1.
        let lengths = [1u32, 2, 3, 3];
        let table = HuffmanTable::build(&lengths, HUFFMAN_TABLE_BITS).unwrap();
        let codes = canonical_codes(&lengths);
        for (sym, &len) in lengths.iter().enumerate() {
            // The bits appear in the stream LSB-first as the reversed canonical code.
            let reversed = reverse_bits(codes[sym], len);
            let bytes = (reversed | (0xFFFF_FFFF << len)).to_le_bytes();
            let mut br = BitReader::new(&bytes);
            assert_eq!(
                table.read_symbol(&mut br) as usize,
                sym,
                "symbol {sym} (len {len}) must round-trip"
            );
        }
    }

    #[test]
    fn rejects_over_subscribed_code() {
        // Three symbols of length 1 cannot form a prefix code (max 2).
        assert!(HuffmanTable::build(&[1, 1, 1], HUFFMAN_TABLE_BITS).is_none());
    }

    #[test]
    fn rejects_all_zero_code() {
        assert!(HuffmanTable::build(&[0, 0, 0], HUFFMAN_TABLE_BITS).is_none());
    }

    #[test]
    fn build_creates_sub_table_when_first_key_equals_sentinel() {
        // Guards `low` starting at -1 (not 1). A single length-1 leaf takes half
        // the code space and leaves the build key at exactly 1 entering the
        // second level, so the first length-9 code has `key & root_mask == 1`.
        // Were `low` initialised to 1, that first comparison would suppress the
        // sub-table and symbol 1 would mis-decode (it resolves to 2 instead).
        let mut lengths = vec![0u32; 257];
        lengths[0] = 1; // length-1 leaf: 1/2 of the space
        for l in lengths.iter_mut().skip(1) {
            *l = 9; // 256 length-9 leaves fill the other 1/2
        }
        let table = HuffmanTable::build(&lengths, HUFFMAN_TABLE_BITS).unwrap();
        let codes = canonical_codes(&lengths);
        let reversed = reverse_bits(codes[1], 9);
        assert_eq!(reversed, 1, "symbol 1 must be the key==1 length-9 code");
        let bytes = (reversed | (0xFFFF_FFFF << 9)).to_le_bytes();
        let mut br = BitReader::new(&bytes);
        assert_eq!(table.read_symbol(&mut br), 1);
    }

    /// A 2-symbol code-length-code (symbols 0 and 1, each length 1). Reading one
    /// bit yields code-length symbol 0 (a `0` bit) or 1 (a `1` bit).
    fn two_symbol_cl_code() -> [u32; CODE_LENGTH_CODES] {
        let mut cl = [0u32; CODE_LENGTH_CODES];
        cl[0] = 1;
        cl[1] = 1;
        cl
    }

    #[test]
    fn read_code_lengths_rejects_only_above_alphabet() {
        // Guards `max_symbol > num_symbols` (not `<`). Control word encodes
        // max_symbol == 2, which is BELOW the 4-symbol alphabet and must be
        // accepted, filling two symbols then leaving the rest zero.
        let cl = two_symbol_cl_code();
        let buf = [0xC1u8, 0, 0, 0];
        let mut br = BitReader::new(&buf);
        assert_eq!(read_code_lengths(&mut br, cl, 4), Some(vec![1, 1, 0, 0]));
    }

    #[test]
    fn read_code_lengths_length_nbits_x1() {
        // Control word x == 1, so length_nbits = 2 + 2*1 = 4 and max_symbol = 2.
        // Guards the `+` in `2 + 2*x` (→ `-` gives nbits 0) and the `*` in
        // `2 + 2 * read_bits` (→ max_symbol 2*0 = 0 stops immediately).
        let cl = two_symbol_cl_code();
        let buf = [3u8, 1, 0, 0];
        let mut br = BitReader::new(&buf);
        assert_eq!(read_code_lengths(&mut br, cl, 3), Some(vec![1, 0, 0]));
    }

    #[test]
    fn read_code_lengths_length_nbits_x2() {
        // Control word x == 2, so length_nbits = 2 + 2*2 = 6. Guards `2 + 2*x`
        // against `2 * (2*x)` (nbits 8) and `2 * x` against `2 / x` (nbits 3).
        let cl = two_symbol_cl_code();
        let buf = [5u8, 4, 0, 0];
        let mut br = BitReader::new(&buf);
        assert_eq!(read_code_lengths(&mut br, cl, 3), Some(vec![1, 0, 0]));
    }

    #[test]
    fn read_code_lengths_countdown_stops_at_max_symbol() {
        // max_symbol == 2 over a 3-symbol alphabet: the `-= 1` countdown must
        // reach zero and stop after two symbols (symbol 2 stays zero). Guards
        // `-= 1` against `/= 1` / `+= 1` (which never reach zero and would read a
        // third symbol → `[0, 0, 1]`), and the `+` in `2 + 2*x` (→ `[1, 0, 0]`).
        let cl = two_symbol_cl_code();
        let buf = [1u8, 1, 0, 0];
        let mut br = BitReader::new(&buf);
        assert_eq!(read_code_lengths(&mut br, cl, 3), Some(vec![0, 0, 0]));
    }

    #[test]
    fn read_code_lengths_max_symbol_base_offset() {
        // read_bits(length_nbits) == 1 so max_symbol = 2 + 1 = 3 == alphabet,
        // reading all three symbols. Guards the `+` in `2 + read_bits` (→ `2 - 1`
        // = 1 would stop after one symbol, giving `[0, 0, 0]`).
        let cl = two_symbol_cl_code();
        let buf = [17u8, 1, 0, 0];
        let mut br = BitReader::new(&buf);
        assert_eq!(read_code_lengths(&mut br, cl, 3), Some(vec![0, 0, 1]));
    }

    #[test]
    fn get_next_key_advances_bit_reversed_key() {
        // `get_next_key` clears the run of set bits below `len` and sets the next
        // free bit, advancing a bit-reversed canonical key by one. Asserting exact
        // results guards the `key & step != 0` scan: the `!= 0` -> `== 0` mutant
        // inverts the loop, which never enters here (bit `len-1` is set) and returns
        // the key unmasked-plus-`step` instead. Both inputs have bit `len-1` set, so
        // the mutant terminates (with a wrong value) rather than merely hanging.
        assert_eq!(get_next_key(0b100, 3), 0b010, "0b100 -> 0b010");
        assert_eq!(get_next_key(0b110, 3), 0b001, "0b110 -> 0b001");
    }

    #[test]
    fn next_table_bit_size_computes_sub_table_width() {
        // Two length-9 sub-codes here: one leaf at length 10 and six at length 11
        // (root_bits = 8). The scan descends one level (10 -> 11) before the slots
        // are exhausted, so the second-level table needs 3 index bits. The `len +=
        // 1` -> `len *= 1` mutant freezes `len` at 10: `left` then grows unbounded
        // until it wraps negative and breaks, returning width 2 instead of 3 (a fast
        // divergent result, not a hang, for this particular count vector).
        let mut count = [0i32; MAX_ALLOWED_CODE_LENGTH + 1];
        count[10] = 1;
        count[11] = 6;
        assert_eq!(next_table_bit_size(&count, 10, 8), 3);
    }

    #[test]
    fn read_huffman_code_accepts_maximum_code_length_count() {
        // Guards `num_code_lengths > CODE_LENGTH_CODES` (not `==` / `>=`).
        // The stream encodes num_code_lengths == 19 (its maximum, == the array
        // size), which is valid and must not be rejected: normal code, count 19,
        // a 2-symbol code-length-code, then two length-1 symbols over a 2-symbol
        // alphabet.
        let buf = [30u8, 72, 0, 0, 0, 0, 0, 128, 1, 0, 0];
        let mut br = BitReader::new(&buf);
        let table = read_huffman_code(&mut br, 2).expect("num_code_lengths == 19 must be accepted");
        let mut b0 = BitReader::new(&[0x00u8]);
        assert_eq!(table.read_symbol(&mut b0), 0);
        let mut b1 = BitReader::new(&[0x01u8]);
        assert_eq!(table.read_symbol(&mut b1), 1);
    }
}
