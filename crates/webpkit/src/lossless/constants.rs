//! VP8L bitstream constants and lookup tables, transcribed from libwebp 1.6.0.
//!
//! These are the single source of truth shared by the decoder and encoder; the
//! two must agree on every value, so the tables are pinned with compile-time
//! length assertions and anchor tests rather than trusted by eye.

/// VP8L signature byte (first byte of the lossless bitstream).
pub(crate) const VP8L_MAGIC_BYTE: u8 = 0x2f;
/// Bit width of the (width-1) and (height-1) fields in the VP8L header.
///
/// The largest side the header can express is `1 << VP8L_IMAGE_SIZE_BITS`
/// (16384). That bound is the shared [`crate::MAX_DIMENSION`] used to
/// validate [`crate::Dimensions`]; the two are pinned equal by a test below.
pub(crate) const VP8L_IMAGE_SIZE_BITS: u32 = 14;
/// Bit width of the version field (must decode to 0).
pub(crate) const VP8L_VERSION_BITS: u32 = 3;

/// Number of literal (green/red/blue/alpha channel value) codes.
pub(crate) const NUM_LITERAL_CODES: usize = 256;
/// Number of LZ77 length codes appended to the green alphabet.
pub(crate) const NUM_LENGTH_CODES: usize = 24;
/// Number of distance codes.
pub(crate) const NUM_DISTANCE_CODES: usize = 40;
/// Number of code-length codes (the meta-alphabet used to transmit code lengths).
pub(crate) const CODE_LENGTH_CODES: usize = 19;

/// Maximum Huffman code length permitted by the format.
pub(crate) const MAX_ALLOWED_CODE_LENGTH: usize = 15;
/// The implicit "previous length" before any explicit length has been read.
pub(crate) const DEFAULT_CODE_LENGTH: u32 = 8;

/// Root-table index width used by the two-level Huffman decode tables.
pub(crate) const HUFFMAN_TABLE_BITS: u32 = 8;

/// Maximum color-cache size, in bits (cache holds `1 << bits` entries).
pub(crate) const MAX_CACHE_BITS: u32 = 11;
/// Color-cache hash multiplier (`(HASH_MUL * argb) >> (32 - cache_bits)`).
pub(crate) const HASH_MUL: u32 = 0x1e35_a7bd;

/// Number of distance codes that map through the 2-D near-neighbor plane table.
pub(crate) const CODE_TO_PLANE_CODES: usize = 120;

/// Largest back-reference distance the encoder may emit, in pixels (libwebp
/// `WINDOW_SIZE`). Bounding distances here keeps the transmitted plane code
/// `<= WINDOW_SIZE + CODE_TO_PLANE_CODES = 1 << 20`, so its prefix symbol is
/// `<= 39` (a valid [`NUM_DISTANCE_CODES`] index) with `<= 18` extra bits — well
/// under the 24-bit-per-call limit of the bit writer. Without this cap a
/// 16384×16384 image could reference distances that overflow the distance
/// alphabet. The `120` is [`CODE_TO_PLANE_CODES`] (asserted in the tests).
pub(crate) const WINDOW_SIZE: u32 = (1 << 20) - 120;
/// Largest back-reference length the encoder may emit, in pixels. This is the
/// maximum value the green length codes (symbols `256..280`) can express:
/// `prefix_encode(4096)` yields length symbol 23 (the last one) with 10 extra
/// bits. Longer runs are split across successive copies.
pub(crate) const MAX_COPY_LENGTH: u32 = 4096;
/// Shortest back-reference the encoder will emit (libwebp `MIN_LENGTH`). Shorter
/// matches rarely beat coding the pixels as literals once the length + distance
/// symbols and their extra bits are paid for.
pub(crate) const MIN_MATCH: u32 = 4;
/// Upper bound on hash-chain hops per match search. A cap trades a little
/// compression for bounded time; it never affects correctness (a shorter or
/// missed match is still a valid, decodable choice).
pub(crate) const MAX_CHAIN: u32 = 64;
/// Maximum bit width of the LZ77 pixel-pair hash table (libwebp `HASH_BITS`).
/// The match finder uses `min(HASH_BITS_LZ77, ceil_log2(pixels))` so small
/// images do not allocate the full `1 << 18` table.
pub(crate) const HASH_BITS_LZ77: u32 = 18;

/// Opaque black (`0xAARRGGBB`); predictor modes 0/14/15 and the image border seed.
pub(crate) const ARGB_BLACK: u32 = 0xff00_0000;

/// Transform type id for the spatial predictor transform.
pub(crate) const PREDICTOR_TRANSFORM: u32 = 0;
/// Transform type id for the cross-color transform.
pub(crate) const CROSS_COLOR_TRANSFORM: u32 = 1;
/// Transform type id for the subtract-green transform.
pub(crate) const SUBTRACT_GREEN_TRANSFORM: u32 = 2;
/// Transform type id for the color-indexing (palette) transform.
pub(crate) const COLOR_INDEXING_TRANSFORM: u32 = 3;

/// Bias added to the transmitted tile-size field: `tile_bits = read_bits(3) + 2`.
pub(crate) const MIN_TRANSFORM_BITS: u32 = 2;
/// Bit width of the transmitted transform tile-size field.
pub(crate) const NUM_TRANSFORM_BITS: u32 = 3;

/// Ceil-divide `size` by `2^bits` — the number of tiles/entries needed to cover a
/// dimension at the given subsampling level (libwebp `VP8LSubSampleSize`).
#[must_use]
pub(crate) const fn subsample_size(size: u32, bits: u32) -> u32 {
    (size + (1 << bits) - 1) >> bits
}

/// Number of Huffman codes per meta-code (green+length, red, blue, alpha, dist).
pub(crate) const HUFFMAN_CODES_PER_META_CODE: usize = 5;

/// Alphabet size per Huffman code within a group. Index 0 (green) is
/// `NUM_LITERAL_CODES + NUM_LENGTH_CODES`; a color cache extends it at runtime by
/// `1 << cache_bits`.
pub(crate) const ALPHABET_SIZE: [usize; HUFFMAN_CODES_PER_META_CODE] = [
    NUM_LITERAL_CODES + NUM_LENGTH_CODES,
    NUM_LITERAL_CODES,
    NUM_LITERAL_CODES,
    NUM_LITERAL_CODES,
    NUM_DISTANCE_CODES,
];

/// Transmission order of the code-length codes (a fixed permutation).
pub(crate) const CODE_LENGTH_CODE_ORDER: [u8; CODE_LENGTH_CODES] = [
    17, 18, 0, 1, 2, 3, 4, 5, 16, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
];

/// Extra-bit counts for the repeat codes 16, 17, 18.
pub(crate) const CODE_LENGTH_EXTRA_BITS: [u8; 3] = [2, 3, 7];
/// Repeat base offsets for the repeat codes 16, 17, 18.
pub(crate) const CODE_LENGTH_REPEAT_OFFSETS: [u8; 3] = [3, 3, 11];

/// Distance plane table: `plane_code` (1-based, `<= 120`) maps to a packed
/// `(yoffset << 4) | (8 - xoffset)` near-neighbor offset. Values are byte-exact
/// from libwebp `src/dec/vp8l_dec.c`.
pub(crate) const CODE_TO_PLANE: [u8; CODE_TO_PLANE_CODES] = [
    0x18, 0x07, 0x17, 0x19, 0x28, 0x06, 0x27, 0x29, 0x16, 0x1a, //
    0x26, 0x2a, 0x38, 0x05, 0x37, 0x39, 0x15, 0x1b, 0x36, 0x3a, //
    0x25, 0x2b, 0x48, 0x04, 0x47, 0x49, 0x14, 0x1c, 0x35, 0x3b, //
    0x46, 0x4a, 0x24, 0x2c, 0x58, 0x45, 0x4b, 0x34, 0x3c, 0x03, //
    0x57, 0x59, 0x13, 0x1d, 0x56, 0x5a, 0x23, 0x2d, 0x44, 0x4c, //
    0x55, 0x5b, 0x33, 0x3d, 0x68, 0x02, 0x67, 0x69, 0x12, 0x1e, //
    0x66, 0x6a, 0x22, 0x2e, 0x54, 0x5c, 0x43, 0x4d, 0x65, 0x6b, //
    0x32, 0x3e, 0x78, 0x01, 0x77, 0x79, 0x53, 0x5d, 0x11, 0x1f, //
    0x64, 0x6c, 0x42, 0x4e, 0x76, 0x7a, 0x21, 0x2f, 0x75, 0x7b, //
    0x31, 0x3f, 0x63, 0x6d, 0x52, 0x5e, 0x00, 0x74, 0x7c, 0x41, //
    0x4f, 0x10, 0x20, 0x62, 0x6e, 0x30, 0x73, 0x7d, 0x51, 0x5f, //
    0x40, 0x72, 0x7e, 0x61, 0x6f, 0x50, 0x71, 0x7f, 0x60, 0x70, //
];

const _: () = assert!(CODE_TO_PLANE.len() == CODE_TO_PLANE_CODES);
const _: () = assert!(CODE_LENGTH_CODE_ORDER.len() == CODE_LENGTH_CODES);

#[cfg(test)]
mod tests {
    use crate::MAX_DIMENSION;

    use super::{
        CODE_LENGTH_CODE_ORDER, CODE_TO_PLANE, HASH_MUL, VP8L_IMAGE_SIZE_BITS, subsample_size,
    };

    #[test]
    fn max_dimension_is_16384() {
        // The width/height fields store `dimension - 1` in 14 bits, so the
        // largest expressible side is 2^14 — which must equal the shared
        // `crate::MAX_DIMENSION` that validates `Dimensions`.
        assert_eq!(MAX_DIMENSION, 16384);
        assert_eq!(1u32 << VP8L_IMAGE_SIZE_BITS, MAX_DIMENSION);
    }

    #[test]
    fn code_to_plane_anchors() {
        // The two most common back-references: straight up and one to the left.
        assert_eq!(CODE_TO_PLANE[0], 0x18); // yoffset 1, xoffset 0 -> distance = width
        assert_eq!(CODE_TO_PLANE[1], 0x07); // yoffset 0, xoffset 1 -> distance = 1
        assert_eq!(CODE_TO_PLANE.len(), 120);
    }

    #[test]
    fn code_to_plane_table_bytes_are_pinned() {
        // Corruption guard. The table's *correctness* is proven by the libwebp
        // conformance goldens (a wrong table fails decode); this order-sensitive
        // FNV-1a hash pins its exact 120 bytes so an accidental edit fails fast
        // and locally. Regenerate the constant only alongside an intentional change.
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325; // FNV-1a offset basis
        for &byte in &CODE_TO_PLANE {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3); // FNV-1a prime
        }
        assert_eq!(
            hash, 11_597_027_981_418_168_485,
            "CODE_TO_PLANE bytes changed"
        );
    }

    #[test]
    fn code_length_code_order_is_a_permutation_of_0_18() {
        let mut seen = [false; 19];
        for &c in &CODE_LENGTH_CODE_ORDER {
            seen[c as usize] = true;
        }
        assert!(
            seen.iter().all(|&s| s),
            "must cover every code-length code 0..=18"
        );
    }

    #[test]
    fn hash_multiplier_is_exact() {
        assert_eq!(HASH_MUL, 0x1e35_a7bd);
    }

    #[test]
    fn lz77_window_and_length_limits() {
        use super::{MAX_COPY_LENGTH, WINDOW_SIZE};
        // The window is one 20-bit span minus the 120 near-neighbor plane codes,
        // so the largest plane code (distance + 120) is exactly 1 << 20.
        assert_eq!(WINDOW_SIZE, 1_048_456);
        assert_eq!(
            u64::from(WINDOW_SIZE) + super::CODE_TO_PLANE_CODES as u64,
            1 << 20
        );
        // 4096 is the last length the 24 green length codes can express.
        assert_eq!(MAX_COPY_LENGTH, 4096);
    }

    #[test]
    fn subsample_size_ceil_divides() {
        assert_eq!(subsample_size(5, 1), 3);
        assert_eq!(subsample_size(1, 3), 1);
        assert_eq!(subsample_size(16, 0), 16);
        assert_eq!(subsample_size(17, 4), 2);
    }
}
