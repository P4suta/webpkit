//! LZ77 back-reference decode helpers: prefix codes with extra bits, and the
//! 2-D distance / plane-code mapping. Shared with the encoder (which uses the
//! inverse mappings), so both agree by construction.
use crate::lossless::bit_io::reader::BitReader;
use crate::lossless::constants::{CODE_TO_PLANE, CODE_TO_PLANE_CODES};

/// Decode a length or distance value from its prefix `symbol`, reading any extra
/// bits from `br`. Length and distance share this scheme (libwebp
/// `GetCopyLength` == `GetCopyDistance`).
///
/// Symbols `0..=3` are the literal values `1..=4` with no extra bits; larger
/// symbols encode `offset + extra + 1` where `offset` and the extra-bit count
/// grow geometrically.
pub(crate) fn read_prefix_value(symbol: u32, br: &mut BitReader<'_>) -> u32 {
    if symbol < 4 {
        return symbol + 1;
    }
    let extra_bits = (symbol - 2) >> 1;
    let offset = (2 + (symbol & 1)) << extra_bits;
    offset + br.read_bits(extra_bits) + 1
}

/// Map a 1-based distance `plane_code` to a pixel distance for an image of width
/// `xsize`.
///
/// Plane codes `> 120` are literal distances (`plane_code - 120`); codes `1..=120`
/// index [`CODE_TO_PLANE`], whose packed byte gives a near-neighbor
/// `(yoffset, xoffset)` combined as `yoffset * xsize + xoffset`, clamped to `>= 1`.
#[allow(
    clippy::cast_possible_wrap,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_lossless,
    reason = "xsize <= 16384 (14-bit) and the computed distance is a small non-negative value; \
              `as` (not From) is required to stay a const fn"
)]
pub(crate) const fn plane_code_to_distance(xsize: u32, plane_code: u32) -> u32 {
    if plane_code as usize > CODE_TO_PLANE_CODES {
        return plane_code - CODE_TO_PLANE_CODES as u32;
    }
    let dc = CODE_TO_PLANE[(plane_code - 1) as usize] as i32;
    let yoffset = dc >> 4;
    let xoffset = 8 - (dc & 0x0f);
    let dist = yoffset * xsize as i32 + xoffset;
    if dist >= 1 { dist as u32 } else { 1 }
}

/// Encode a length or distance `value` (`>= 1`) into its prefix `symbol`, the
/// number of `extra_bits`, and the `extra_value` those bits carry. This is the
/// exact inverse of [`read_prefix_value`] (libwebp `VP8LPrefixEncode`): feeding
/// the returned `symbol` and `extra_value` back through `read_prefix_value`
/// reproduces `value`.
///
/// Values `1..=4` are symbols `0..=3` with no extra bits (the `symbol < 4` fast
/// path); larger values split into the top two set bits of `value - 1` (the
/// `symbol`) and the remaining low bits (`extra_value`).
pub(crate) const fn prefix_encode(value: u32) -> (u32, u32, u32) {
    if value <= 4 {
        return (value - 1, 0, 0);
    }
    let d = value - 1;
    let highest_bit = d.ilog2();
    let second_highest_bit = (d >> (highest_bit - 1)) & 1;
    let extra_bits = highest_bit - 1;
    let extra_value = d & ((1 << extra_bits) - 1);
    (
        2 * highest_bit + second_highest_bit,
        extra_bits,
        extra_value,
    )
}

/// Map a pixel `dist` (`>= 1`) to the smallest 1-based distance `plane_code` that
/// [`plane_code_to_distance`] turns back into `dist` for an image of width
/// `xsize` (libwebp `VP8LDistanceToPlaneCode`).
///
/// The forward table is scanned in order so the smallest — hence
/// cheapest-to-code — plane code wins; distances with no near-neighbor encoding
/// fall back to the literal plane code `dist + 120`. Because the search reuses
/// the decoder's own [`plane_code_to_distance`], the round trip holds by
/// construction.
///
/// This O(120) scan is retained only as the byte-exact reference oracle for
/// [`PlaneCodeMap`], which the encoder builds once per width and looks up in
/// O(log n); `plane_code_map_matches_reference` pins the two equal.
#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    reason = "CODE_TO_PLANE_CODES is 120 and every emitted distance is bounded by \
              WINDOW_SIZE, so both narrowing casts are value-preserving"
)]
pub(crate) fn distance_to_plane_code(xsize: u32, dist: u32) -> u32 {
    for plane_code in 1..=CODE_TO_PLANE_CODES as u32 {
        if plane_code_to_distance(xsize, plane_code) == dist {
            return plane_code;
        }
    }
    dist + CODE_TO_PLANE_CODES as u32
}

/// A per-width reverse map from a pixel distance to its distance `plane_code`,
/// precomputed once so the encoder replaces [`distance_to_plane_code`]'s O(120)
/// linear rescan — run once per copy, and once per candidate per DP position —
/// with an O(log n) lookup over the 120 near-neighbor distances.
///
/// `xsize` is fixed within each parse/emit, so the reachable near-neighbor
/// distances (and the plane code each maps to) are fixed too. The 120 `(distance,
/// plane_code)` pairs are held sorted by `(distance, plane_code)` in a fixed
/// stack array — no heap allocation, so building one per call stays cheap. A
/// lookup takes the FIRST pair with a matching distance; because equal distances
/// are ordered by ascending plane code, that first pair carries the smallest
/// plane code, reproducing the forward scan's first-match winner exactly. A
/// distance with no near-neighbor encoding matches none and falls back to the
/// literal plane code `dist + 120`, byte-identical to the scan.
pub(crate) struct PlaneCodeMap {
    /// The 120 `(distance, plane_code)` pairs, sorted by `(distance, plane_code)`.
    entries: [(u32, u32); CODE_TO_PLANE_CODES],
}

impl PlaneCodeMap {
    /// Build the reverse lookup for width `xsize`.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "CODE_TO_PLANE_CODES is 120, so each plane code fits u32"
    )]
    pub(crate) fn new(xsize: u32) -> Self {
        let mut entries = [(0u32, 0u32); CODE_TO_PLANE_CODES];
        for (index, entry) in entries.iter_mut().enumerate() {
            let plane_code = index as u32 + 1;
            *entry = (plane_code_to_distance(xsize, plane_code), plane_code);
        }
        // Sorting by `(distance, plane_code)` puts the smallest plane code first
        // within each equal-distance run, so the first match a lookup finds is the
        // winner the ascending forward scan returns.
        entries.sort_unstable();
        Self { entries }
    }

    /// The distance `plane_code` for `dist` — identical to
    /// [`distance_to_plane_code`] for the same `xsize`.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "CODE_TO_PLANE_CODES is 120 and every emitted distance is bounded \
                  by WINDOW_SIZE, so the miss-fallback cast is value-preserving"
    )]
    pub(crate) fn plane_code(&self, dist: u32) -> u32 {
        // First index whose distance is `>= dist`; the array is sorted so that is
        // the smallest-plane-code entry for `dist` when one exists.
        let index = self.entries.partition_point(|&(d, _)| d < dist);
        match self.entries.get(index) {
            Some(&(d, plane_code)) if d == dist => plane_code,
            _ => dist + CODE_TO_PLANE_CODES as u32,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PlaneCodeMap, distance_to_plane_code, plane_code_to_distance, prefix_encode,
        read_prefix_value,
    };
    use crate::lossless::bit_io::reader::BitReader;
    use crate::lossless::bit_io::writer::BitWriter;
    use crate::lossless::constants::{MAX_COPY_LENGTH, WINDOW_SIZE};
    use proptest::prelude::*;

    #[test]
    fn small_symbols_are_literal_with_no_extra_bits() {
        // Symbols 0..=3 decode to 1..=4 and consume no extra bits.
        for sym in 0..4u32 {
            let mut br = BitReader::new(&[0xFF]); // no bits should be consumed
            assert_eq!(read_prefix_value(sym, &mut br), sym + 1);
            assert!(!br.is_eos());
        }
    }

    #[test]
    fn symbol_four_reads_one_extra_bit() {
        // sym=4: extra=1, offset=(2+0)<<1=4, value = 4 + extra + 1.
        let mut br0 = BitReader::new(&[0x00]);
        assert_eq!(read_prefix_value(4, &mut br0), 5); // extra bit = 0
        let mut br1 = BitReader::new(&[0x01]);
        assert_eq!(read_prefix_value(4, &mut br1), 6); // extra bit = 1
    }

    #[test]
    fn symbol_five_uses_odd_offset() {
        // sym=5: extra=1, offset=(2+1)<<1=6, value = 6 + extra + 1.
        let mut br0 = BitReader::new(&[0x00]);
        assert_eq!(read_prefix_value(5, &mut br0), 7);
        let mut br1 = BitReader::new(&[0x01]);
        assert_eq!(read_prefix_value(5, &mut br1), 8);
    }

    #[test]
    fn plane_code_anchors() {
        // plane 1 (CODE_TO_PLANE[0] = 0x18): yoffset 1, xoffset 0 -> straight up.
        assert_eq!(plane_code_to_distance(100, 1), 100);
        // plane 2 (CODE_TO_PLANE[1] = 0x07): yoffset 0, xoffset 1 -> one to the left.
        assert_eq!(plane_code_to_distance(100, 2), 1);
    }

    #[test]
    fn plane_code_above_120_is_linear() {
        assert_eq!(plane_code_to_distance(100, 121), 1);
        assert_eq!(plane_code_to_distance(100, 200), 80);
    }

    #[test]
    fn plane_code_distance_never_zero() {
        // Every plane code over a range of widths must yield a distance >= 1.
        for xsize in [1u32, 2, 16, 100, 1000, 16384] {
            for pc in 1..=200u32 {
                assert!(plane_code_to_distance(xsize, pc) >= 1);
            }
        }
    }

    #[test]
    fn prefix_encode_known_values() {
        // Values 1..=4 are the zero-extra-bit fast path (symbols 0..=3).
        assert_eq!(prefix_encode(1), (0, 0, 0));
        assert_eq!(prefix_encode(4), (3, 0, 0));
        // Symbol 4 uses an even offset, symbol 5 an odd one; each has 1 extra bit.
        assert_eq!(prefix_encode(5), (4, 1, 0));
        assert_eq!(prefix_encode(6), (4, 1, 1));
        assert_eq!(prefix_encode(7), (5, 1, 0));
        assert_eq!(prefix_encode(8), (5, 1, 1));
        // The largest length the green length codes can express is symbol 23.
        assert_eq!(prefix_encode(MAX_COPY_LENGTH), (23, 10, 1023));
    }

    #[test]
    fn prefix_symbol_bounds_fit_the_alphabets() {
        // Every length up to the cap stays within the 24 green length codes.
        for value in [1u32, 2, 4, 5, 255, 256, MAX_COPY_LENGTH] {
            let (symbol, extra_bits, _) = prefix_encode(value);
            assert!(symbol <= 23, "length symbol {symbol} for value {value}");
            assert!(extra_bits <= 10);
        }
        // The largest plane code (window + 120) is the last distance symbol, 39.
        let (symbol, extra_bits, _) = prefix_encode(WINDOW_SIZE + 120);
        assert_eq!(symbol, 39);
        assert!(extra_bits <= 18);
    }

    #[test]
    fn plane_code_map_matches_reference_exhaustive() {
        // Sweep a band of distances around every reachable near-neighbor multiple
        // (`k * xsize`, `k <= 15`, x-offset within `-7..=8`) plus far misses, and
        // assert the O(log n) map equals the O(120) reference scan bit-for-bit at
        // every width — the tiny (1x1), tiny-width, and large (14-bit) extremes.
        for xsize in [1u32, 2, 3, 4, 7, 8, 16, 100, 1000, 16384] {
            let map = PlaneCodeMap::new(xsize);
            let mut dists = alloc::vec![1u32, 2, 100, 100_000, WINDOW_SIZE];
            for k in 0..=16i64 {
                for off in -16..=16i64 {
                    let d = k * i64::from(xsize) + off;
                    if let Ok(d) = u32::try_from(d)
                        && d >= 1
                    {
                        dists.push(d);
                    }
                }
            }
            for &dist in &dists {
                assert_eq!(
                    map.plane_code(dist),
                    distance_to_plane_code(xsize, dist),
                    "mismatch at xsize={xsize} dist={dist}"
                );
            }
        }
    }

    #[test]
    fn distance_to_plane_code_known_values() {
        // Straight up (distance = width) is plane code 1; one to the left is 2.
        assert_eq!(distance_to_plane_code(100, 100), 1);
        assert_eq!(distance_to_plane_code(100, 1), 2);
        // A distance with no near-neighbor encoding falls back to `dist + 120`.
        assert_eq!(distance_to_plane_code(100, 100_000), 100_120);
    }

    /// Round-trip a value through `prefix_encode` and the real decoder read,
    /// exercising the actual `BitWriter` -> `BitReader` -> `read_prefix_value`
    /// path (not just the algebra).
    fn round_trip_prefix(value: u32) -> u32 {
        let (symbol, extra_bits, extra_value) = prefix_encode(value);
        let mut bw = BitWriter::new();
        bw.write_bits(extra_value, extra_bits);
        let bytes = bw.into_bytes();
        let mut br = BitReader::new(&bytes);
        read_prefix_value(symbol, &mut br)
    }

    proptest! {
        #[test]
        fn prefix_encode_inverts_read_prefix_value(value in 1u32..=1_048_576) {
            let (symbol, extra_bits, _) = prefix_encode(value);
            // Extra bits always fit a single BitWriter call; symbols fit the
            // widest (distance) alphabet.
            prop_assert!(extra_bits <= 24);
            prop_assert!(symbol < 40);
            prop_assert_eq!(round_trip_prefix(value), value);
        }

        #[test]
        fn distance_to_plane_code_inverts_plane_code_to_distance(
            xsize in prop_oneof![
                Just(1u32), Just(2u32), Just(16u32),
                Just(100u32), Just(1000u32), Just(16384u32),
            ],
            dist in 1u32..=WINDOW_SIZE,
        ) {
            let plane_code = distance_to_plane_code(xsize, dist);
            prop_assert_eq!(plane_code_to_distance(xsize, plane_code), dist);
            // The plane code must be codable by the 40 distance symbols.
            let (symbol, extra_bits, _) = prefix_encode(plane_code);
            prop_assert!(symbol < 40);
            prop_assert!(extra_bits <= 24);
        }

        /// The memoized [`PlaneCodeMap`] returns byte-identical plane codes to the
        /// [`distance_to_plane_code`] reference scan for every width/distance —
        /// the mechanical proof the O(log n) lookup is behavior-neutral, sampling
        /// the miss branch (far distances) as well as near-neighbor hits.
        #[test]
        fn plane_code_map_matches_reference(
            xsize in prop_oneof![
                Just(1u32), Just(2u32), Just(3u32), Just(16u32),
                Just(100u32), Just(1000u32), Just(16384u32),
            ],
            dist in 1u32..=WINDOW_SIZE,
        ) {
            let map = PlaneCodeMap::new(xsize);
            prop_assert_eq!(map.plane_code(dist), distance_to_plane_code(xsize, dist));
        }
    }
}
