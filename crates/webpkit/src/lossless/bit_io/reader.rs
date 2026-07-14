//! LSB-first bit reader over an in-memory byte slice.

/// A little-endian, least-significant-bit-first reader over a byte slice.
///
/// The VP8L bitstream is consumed LSB-first: the first bit read from a byte is
/// its bit 0, and an `n`-bit value returned by [`BitReader::read_bits`] carries
/// the first bit read in its least-significant position. Multi-byte values are
/// little-endian (`[0x34, 0x12]` read as 16 bits yields `0x1234`).
///
/// Reads past the end of the buffer yield zero bits and latch [`BitReader::is_eos`]
/// to `true`, mirroring libwebp's zero-padding so a truncated stream is detected
/// at the end of decoding rather than by panicking mid-symbol.
pub(crate) struct BitReader<'a> {
    buf: &'a [u8],
    /// Bit accumulator; the next unconsumed bit sits at position `bit_pos`.
    val: u64,
    /// Index of the next byte in `buf` not yet loaded into `val`.
    pos: usize,
    /// Count of bits already consumed from the low end of `val` (kept `< 8`
    /// after each refill).
    bit_pos: u32,
    /// Real (buffer-backed) bits not yet consumed. When a read exceeds this the
    /// stream is exhausted and the surplus bits are zero-padded.
    bits_left: u64,
    /// Latched once a read has run past the end of `buf`.
    eos: bool,
    /// Absolute count of bits consumed since the start of `buf` (a monotonic
    /// odometer). Independent of the read math; it only records how far the
    /// cursor has advanced so a position can be captured and later restored with
    /// [`BitReader::new_at`] in O(1).
    abs_bits: u64,
}

impl<'a> BitReader<'a> {
    /// Create a reader positioned at the first bit of `buf`.
    pub(crate) fn new(buf: &'a [u8]) -> Self {
        let prime = buf.len().min(8);
        let mut val = 0u64;
        for (i, &byte) in buf.iter().take(prime).enumerate() {
            val |= u64::from(byte) << (8 * i);
        }
        Self {
            buf,
            val,
            pos: prime,
            bit_pos: 0,
            bits_left: buf.len() as u64 * 8,
            eos: false,
            abs_bits: 0,
        }
    }

    /// Create a reader positioned `bit_offset` bits into `buf`, in O(1).
    ///
    /// This is semantically identical to [`BitReader::new`] followed by
    /// `consume(bit_offset)` — every internal field (accumulator, byte cursor,
    /// intra-byte position, remaining real bits, EOS latch, and odometer) matches
    /// bit-for-bit — but it primes the accumulator directly instead of walking
    /// byte by byte, so seeking to a saved [`BitReader::bit_position`] costs the
    /// same regardless of how far in it is. `bit_offset` is expected to be a
    /// previously committed boundary (`<= buf.len() * 8`); the arithmetic
    /// saturates rather than overflowing if it is not.
    pub(crate) fn new_at(buf: &'a [u8], bit_offset: u64) -> Self {
        // Split the offset into whole bytes already passed and the leftover
        // intra-byte position, mirroring what `consume` leaves behind.
        let start = usize::try_from(bit_offset / 8)
            .unwrap_or(usize::MAX)
            .min(buf.len());
        let bit_pos = u32::try_from(bit_offset % 8).unwrap_or(0);

        // Prime the accumulator from `buf[start..]` exactly as `new` primes from
        // `buf[0..]`: up to eight little-endian bytes.
        let loaded = (buf.len() - start).min(8);
        let mut val = 0u64;
        for (i, &byte) in buf[start..].iter().take(loaded).enumerate() {
            val |= u64::from(byte) << (8 * i);
        }

        let total_bits = buf.len() as u64 * 8;
        Self {
            buf,
            val,
            pos: start + loaded,
            bit_pos,
            bits_left: total_bits.saturating_sub(bit_offset),
            eos: false,
            abs_bits: bit_offset,
        }
    }

    /// Return the next `n` bits (`n <= 24`) LSB-first **without** consuming them.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "the accumulator is masked to n <= 24 bits; the high bits are unconsumed future bits"
    )]
    pub(crate) fn peek_bits(&self, n: u32) -> u32 {
        debug_assert!(n <= 24, "peek_bits supports up to 24 bits per call");
        let mask = (1u32 << n).wrapping_sub(1);
        (self.val >> self.bit_pos) as u32 & mask
    }

    /// Advance the cursor by `n` bits, refilling the accumulator and latching
    /// [`BitReader::is_eos`] if this consumes past the real input.
    pub(crate) fn consume(&mut self, n: u32) {
        self.abs_bits += u64::from(n);
        if u64::from(n) > self.bits_left {
            self.eos = true;
            self.bits_left = 0;
        } else {
            self.bits_left -= u64::from(n);
        }
        self.bit_pos += n;
        while self.bit_pos >= 8 {
            self.val >>= 8;
            if self.pos < self.buf.len() {
                self.val |= u64::from(self.buf[self.pos]) << 56;
                self.pos += 1;
            }
            self.bit_pos -= 8;
        }
    }

    /// Read `n` bits (`n <= 24`) LSB-first and advance the cursor.
    pub(crate) fn read_bits(&mut self, n: u32) -> u32 {
        let result = self.peek_bits(n);
        self.consume(n);
        result
    }

    /// Read a single bit LSB-first.
    pub(crate) fn read_bit(&mut self) -> u32 {
        self.read_bits(1)
    }

    /// Whether a read has run past the end of the input (zero-padded thereafter).
    pub(crate) const fn is_eos(&self) -> bool {
        self.eos
    }

    /// The absolute number of bits consumed since the start of `buf`.
    ///
    /// Pairs with [`BitReader::new_at`]: capturing this value and later seeking a
    /// (possibly grown, append-only) buffer to it reconstructs the identical bit
    /// window in O(1).
    pub(crate) const fn bit_position(&self) -> u64 {
        self.abs_bits
    }
}

#[cfg(test)]
mod tests {
    use super::BitReader;

    #[test]
    fn reads_a_whole_byte() {
        // 0x2F is the VP8L signature byte.
        let mut r = BitReader::new(&[0x2F]);
        assert_eq!(r.read_bits(8), 0x2F);
        assert!(!r.is_eos());
    }

    #[test]
    fn assembles_bits_lsb_first_within_a_byte() {
        // 0xAC = 1010_1100; LSB-first the bits stream out as 0,0,1,1,0,1,0,1.
        let mut r = BitReader::new(&[0xAC]);
        assert_eq!(r.read_bits(2), 0b00); // bits 0,1
        assert_eq!(r.read_bits(3), 0b011); // bits 2,3,4 -> 1,1,0 => 0b011
        assert_eq!(r.read_bits(3), 0b101); // bits 5,6,7 -> 1,0,1 => 0b101
    }

    #[test]
    fn multi_byte_values_are_little_endian() {
        let mut r = BitReader::new(&[0x34, 0x12]);
        assert_eq!(r.read_bits(16), 0x1234);
    }

    #[test]
    fn reads_across_a_byte_boundary() {
        // Low nibble of byte0, then 8 bits spanning byte0-high + byte1-low.
        let mut r = BitReader::new(&[0xF0, 0x0F]);
        assert_eq!(r.read_bits(4), 0x0); // low nibble of 0xF0
        assert_eq!(r.read_bits(8), 0xFF); // high nibble 0xF | low nibble of 0x0F
        assert_eq!(r.read_bits(4), 0x0); // high nibble of 0x0F
    }

    #[test]
    fn decodes_a_vp8l_header_prefix() {
        // signature 0x2F, then 14-bit (width-1), 14-bit (height-1), alpha, version.
        // 8 + 14 + 14 + 1 + 3 = 40 bits = 5 bytes, all zero except the signature,
        // describing a 1x1 image with alpha_is_used=0 and version=0.
        let bytes = [0x2F, 0x00, 0x00, 0x00, 0x00];
        let mut r = BitReader::new(&bytes);
        assert_eq!(r.read_bits(8), 0x2F);
        assert_eq!(r.read_bits(14) + 1, 1); // width
        assert_eq!(r.read_bits(14) + 1, 1); // height
        assert_eq!(r.read_bit(), 0); // alpha_is_used
        assert_eq!(r.read_bits(3), 0); // version
        assert!(!r.is_eos());
    }

    #[test]
    fn past_end_yields_zero_and_latches_eos() {
        let mut r = BitReader::new(&[0xFF]);
        assert_eq!(r.read_bits(8), 0xFF);
        assert!(!r.is_eos());
        assert_eq!(r.read_bits(8), 0x00); // past the end
        assert!(r.is_eos());
    }

    #[test]
    fn works_on_an_empty_buffer() {
        let mut r = BitReader::new(&[]);
        assert_eq!(r.read_bits(4), 0);
        assert!(r.is_eos());
    }

    /// A tuple of every observable internal field, so `new_at` can be compared
    /// against `new(..).consume(..)` in one shot.
    fn snapshot(r: &BitReader<'_>) -> (u64, usize, u32, u64, bool, u64) {
        (r.val, r.pos, r.bit_pos, r.bits_left, r.eos, r.abs_bits)
    }

    #[test]
    fn new_at_matches_new_then_consume_exhaustively() {
        // Buffers straddling the 8-byte prime window (below, at, above), and
        // every offset on each — hitting off == 0, off == len*8, byte-aligned
        // boundaries, and mid-byte positions deterministically.
        for len in [0usize, 1, 2, 3, 7, 8, 9, 16, 20] {
            let buf: Vec<u8> = (0..len)
                .map(|i| u8::try_from((i * 37 + 5) & 0xff).unwrap_or(0))
                .collect();
            for off in 0..=(len as u64 * 8) {
                let at_offset = BitReader::new_at(&buf, off);
                let mut walked = BitReader::new(&buf);
                walked.consume(u32::try_from(off).unwrap());
                assert_eq!(
                    snapshot(&at_offset),
                    snapshot(&walked),
                    "field mismatch at len={len} off={off}"
                );

                // The two readers must also stay in lock-step across subsequent
                // reads that cross the prime window and run off the end.
                let mut a = at_offset;
                let mut b = walked;
                for n in [1u32, 3, 8, 13, 24, 7, 5] {
                    assert_eq!(
                        a.read_bits(n),
                        b.read_bits(n),
                        "read mismatch at len={len} off={off} n={n}"
                    );
                    assert_eq!(a.bit_position(), b.bit_position());
                    assert_eq!(a.is_eos(), b.is_eos());
                }
            }
        }
    }
}

#[cfg(test)]
mod proptests {
    use super::BitReader;
    use proptest::prelude::*;

    /// Arbitrary buffer paired with a committed bit offset (`0..=len*8`).
    fn buf_and_offset() -> impl Strategy<Value = (Vec<u8>, u64)> {
        proptest::collection::vec(any::<u8>(), 0..=64).prop_flat_map(|buf| {
            let max = buf.len() as u64 * 8;
            (Just(buf), 0..=max)
        })
    }

    proptest! {
        /// O(1) `new_at` must be indistinguishable from the O(N) walk of
        /// `new(..)` then `consume(off)` — every internal field agrees, and a
        /// following stream of reads agrees bit-for-bit (including the EOS latch
        /// and the odometer). This proptest is the sole guard that makes the
        /// O(1) seek safe.
        #[test]
        fn new_at_equals_new_then_consume(
            (buf, off) in buf_and_offset(),
            reads in proptest::collection::vec(1u32..=24, 0..64),
        ) {
            let at_offset = BitReader::new_at(&buf, off);
            let mut walked = BitReader::new(&buf);
            walked.consume(u32::try_from(off).unwrap());

            prop_assert_eq!(at_offset.val, walked.val, "val");
            prop_assert_eq!(at_offset.pos, walked.pos, "pos");
            prop_assert_eq!(at_offset.bit_pos, walked.bit_pos, "bit_pos");
            prop_assert_eq!(at_offset.bits_left, walked.bits_left, "bits_left");
            prop_assert_eq!(at_offset.eos, walked.eos, "eos");
            prop_assert_eq!(at_offset.abs_bits, walked.abs_bits, "abs_bits");

            let mut a = at_offset;
            let mut b = walked;
            for &n in &reads {
                prop_assert_eq!(a.read_bits(n), b.read_bits(n));
                prop_assert_eq!(a.bit_position(), b.bit_position());
                prop_assert_eq!(a.is_eos(), b.is_eos());
            }
        }
    }
}
