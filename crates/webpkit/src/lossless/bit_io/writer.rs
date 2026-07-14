//! LSB-first bit writer over an in-memory byte buffer.
//!
//! This is the exact inverse of [`crate::lossless::bit_io::reader::BitReader`]: bits are
//! emitted least-significant-bit-first within each byte and multi-byte values are
//! little-endian, so anything written here reads back identically. The encoder
//! serializes the entire VP8L payload through this one primitive, so its bit
//! order and the reader's must stay in lockstep — verified by round-trip tests.

use crate::lossless::prelude::*;

/// A little-endian, least-significant-bit-first bit writer.
///
/// Bits accumulate low-to-high in `acc`; whenever a full byte is buffered it is
/// flushed to `bytes`. [`BitWriter::into_bytes`] (or [`BitWriter::align`]) pads
/// any trailing partial byte with zero bits, matching the reader's zero-padding
/// at end of stream.
pub(crate) struct BitWriter {
    /// Completed output bytes, in stream order.
    bytes: Vec<u8>,
    /// Bit accumulator; the next bit to emit sits at bit position `nbits`.
    acc: u64,
    /// Count of valid bits currently buffered in `acc` (kept `< 8` after each
    /// write via the flush loop).
    nbits: u32,
}

impl BitWriter {
    /// Create an empty writer.
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            bytes: Vec::new(),
            acc: 0,
            nbits: 0,
        }
    }

    /// Append the low `n` bits (`n <= 24`) of `value`, least-significant first.
    ///
    /// `n == 0` is a strict no-op (writes nothing), which is what lets the
    /// encoder emit a single-symbol Huffman code as zero bits per occurrence.
    /// Bits of `value` above bit `n - 1` are ignored (masked off).
    pub(crate) fn write_bits(&mut self, value: u32, n: u32) {
        debug_assert!(n <= 24, "write_bits supports up to 24 bits per call");
        if n == 0 {
            return;
        }
        let mask = (1u64 << n) - 1;
        self.acc |= (u64::from(value) & mask) << self.nbits;
        self.nbits += n;
        while self.nbits >= 8 {
            // The low byte holds the oldest 8 buffered bits; no truncating cast.
            self.bytes.push(self.acc.to_le_bytes()[0]);
            self.acc >>= 8;
            self.nbits -= 8;
        }
    }

    /// Flush any partial byte, zero-padding to the next byte boundary.
    pub(crate) fn align(&mut self) {
        if self.nbits > 0 {
            self.bytes.push(self.acc.to_le_bytes()[0]);
            self.acc = 0;
            self.nbits = 0;
        }
    }

    /// Finish writing: flush the trailing partial byte (zero-padded) and return
    /// the accumulated bytes.
    #[must_use]
    pub(crate) fn into_bytes(mut self) -> Vec<u8> {
        self.align();
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::BitWriter;
    use crate::lossless::bit_io::reader::BitReader;

    #[test]
    fn writes_a_single_byte() {
        // 0x2f is the VP8L signature byte.
        let mut w = BitWriter::new();
        w.write_bits(0x2f, 8);
        assert_eq!(w.into_bytes(), vec![0x2f]);
    }

    #[test]
    fn writes_multibyte_little_endian() {
        let mut w = BitWriter::new();
        w.write_bits(0x1234, 16);
        assert_eq!(w.into_bytes(), vec![0x34, 0x12]);
    }

    #[test]
    fn zero_width_write_is_a_strict_no_op() {
        let mut w = BitWriter::new();
        w.write_bits(0, 0);
        w.write_bits(0xffff_ffff, 0); // value is irrelevant for n == 0
        w.write_bits(1, 1);
        assert_eq!(w.into_bytes(), vec![0x01]);
    }

    #[test]
    fn align_pads_partial_byte_with_zeros() {
        let mut w = BitWriter::new();
        w.write_bits(0b101, 3);
        // Aligning zero-fills bits 3..8 -> 0b0000_0101.
        assert_eq!(w.into_bytes(), vec![0b0000_0101]);
    }

    /// Deterministic `(value, width)` pairs from a linear-congruential generator,
    /// so the sequence is reproducible without a dependency. Each `value` is
    /// already masked to its `width` (1..=24) bits.
    fn lcg_pairs(count: usize) -> Vec<(u32, u32)> {
        let mut state = 0x1234_5678_9abc_def0u64;
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let bytes = state.to_le_bytes();
            let n = u32::from(bytes[7] % 24) + 1; // 1..=24
            let raw = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            let mask = (1u32 << n) - 1;
            out.push((raw & mask, n));
        }
        out
    }

    #[test]
    fn reader_round_trips_an_lcg_sequence() {
        let pairs = lcg_pairs(500);
        let mut w = BitWriter::new();
        for &(value, n) in &pairs {
            w.write_bits(value, n);
        }
        let bytes = w.into_bytes();
        let mut r = BitReader::new(&bytes);
        for &(value, n) in &pairs {
            assert_eq!(r.read_bits(n), value);
        }
        // We read back exactly what we wrote; padding is never over-read.
        assert!(!r.is_eos());
    }

    #[test]
    fn matches_decode_bitbuf_byte_for_byte() {
        // The decoder's hand-rolled test oracle (vp8l::decode tests), replicated
        // verbatim, must produce byte-identical output to BitWriter.
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

        // Widths stay <= 24, so BitBuf's u32 accumulator never overflows.
        let pairs = lcg_pairs(500);
        let mut oracle = BitBuf::default();
        let mut w = BitWriter::new();
        for &(value, n) in &pairs {
            oracle.put(value, n);
            w.write_bits(value, n);
        }
        assert_eq!(w.into_bytes(), oracle.finish());
    }
}

#[cfg(test)]
mod proptests {
    use super::BitWriter;
    use crate::lossless::bit_io::reader::BitReader;
    use proptest::prelude::*;

    proptest! {
        /// Anything written LSB-first must read back bit-for-bit, and reading
        /// exactly the written bits never runs into the alignment padding.
        #[test]
        fn writer_reader_round_trip(
            pairs in proptest::collection::vec((0u32..=0x00ff_ffff, 1u32..=24), 0..200)
        ) {
            let normalized: Vec<(u32, u32)> = pairs
                .iter()
                .map(|&(value, n)| {
                    let mask = (1u32 << n) - 1;
                    (value & mask, n)
                })
                .collect();

            let mut w = BitWriter::new();
            for &(value, n) in &normalized {
                w.write_bits(value, n);
            }
            let bytes = w.into_bytes();

            let mut r = BitReader::new(&bytes);
            for &(value, n) in &normalized {
                prop_assert_eq!(r.read_bits(n), value);
            }
            prop_assert!(!r.is_eos());
        }
    }
}
