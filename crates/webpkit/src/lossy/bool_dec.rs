//! The VP8 boolean (range) arithmetic decoder — RFC 6386 §7.
//!
//! VP8 codes almost every symbol with a binary arithmetic ("boolean") coder:
//! each bit is decoded against an 8-bit probability that it is zero. This is the
//! lossy counterpart of the `lossless` codec's LSB-first bit reader, and
//! like it this decoder never panics on a short partition — reads past the end
//! yield zero bytes (VP8 partitions are self-terminating; trailing bits are
//! padding), so a truncated stream produces deterministic output instead of a
//! bounds check.
//!
//! This is a direct port of the RFC 6386 §7.3 reference `bool_decoder` to safe
//! integer Rust. Invariants: `range` is renormalized back into `128..=255` after
//! every symbol, and `value` holds the live window into the partition, primed
//! from its first two bytes.

use crate::lossy::constants::Prob;
use crate::lossy::work::work;

/// A boolean-decoder over one VP8 partition's bytes.
pub(crate) struct BoolDecoder<'a> {
    /// The partition bytes; indices at or past the end read as zero (padding).
    input: &'a [u8],
    /// Index of the next byte to pull into `value` during renormalization.
    pos: usize,
    /// The current coding range, renormalized into `128..=255` after each read.
    range: u32,
    /// The live value window, compared against the split point each read.
    value: u32,
    /// Bits shifted into `value` since the last byte load (`0..=7`).
    bit_count: u32,
    /// Latched once a renormalization reads past the end of `input` (i.e. pulls a
    /// padding zero). Purely observational — the decoded value is unaffected
    /// (`value |= 0`), but a row-streaming decoder uses it to tell "consumed real
    /// bytes" from "consumed padding", so it can suspend before committing a
    /// macroblock that ran off the currently-buffered token partition.
    exhausted: bool,
}

/// A resumable snapshot of a [`BoolDecoder`]'s live state, without the borrowed
/// partition slice. A row-streaming decoder persists one of these per token
/// partition across `push` calls and rebuilds the decoder against the (grown)
/// buffer with [`BoolDecoder::resume`]. Everything the coder needs after priming
/// is captured here, so resuming continues bit-for-bit as if it had never paused.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BoolState {
    pos: usize,
    range: u32,
    value: u32,
    bit_count: u32,
    exhausted: bool,
}

impl<'a> BoolDecoder<'a> {
    /// Start decoding at the front of `input` (a control or token partition).
    ///
    /// Per RFC 6386 the decoder primes `value` with the partition's first two
    /// bytes; a partition shorter than that is treated as zero-padded.
    pub(crate) fn new(input: &'a [u8]) -> Self {
        let b0 = input.first().copied().unwrap_or(0);
        let b1 = input.get(1).copied().unwrap_or(0);
        Self {
            input,
            pos: 2,
            range: 255,
            value: (u32::from(b0) << 8) | u32::from(b1),
            bit_count: 0,
            exhausted: false,
        }
    }

    /// Rebuild a decoder over `input` from a previously captured [`BoolState`],
    /// continuing exactly where [`Self::state`] left off. Unlike [`Self::new`]
    /// this does **not** re-prime the first two bytes — those were folded into
    /// `value` when the partition was first opened — so `input` must be the same
    /// partition bytes (possibly extended with newly-arrived tail bytes) and the
    /// decode continues byte-for-byte.
    pub(crate) const fn resume(input: &'a [u8], st: BoolState) -> Self {
        Self {
            input,
            pos: st.pos,
            range: st.range,
            value: st.value,
            bit_count: st.bit_count,
            exhausted: st.exhausted,
        }
    }

    /// Capture the live state for a later [`Self::resume`] (see [`BoolState`]).
    pub(crate) const fn state(&self) -> BoolState {
        BoolState {
            pos: self.pos,
            range: self.range,
            value: self.value,
            bit_count: self.bit_count,
            exhausted: self.exhausted,
        }
    }

    /// Whether any read so far pulled a padding zero past the end of `input`.
    pub(crate) const fn is_exhausted(&self) -> bool {
        self.exhausted
    }

    /// Decode one boolean that is zero with probability `prob / 256`, returning
    /// `true` for a one bit and `false` for a zero bit.
    pub(crate) fn read_bool(&mut self, prob: Prob) -> bool {
        work!(BoolRead);
        // `split` ∈ `1..=range-1`: the boundary between the "0" and "1" sub-ranges.
        let split = 1 + (((self.range - 1) * u32::from(prob)) >> 8);
        let big_split = split << 8;
        let bit = self.value >= big_split;
        if bit {
            self.range -= split;
            self.value -= big_split;
        } else {
            self.range = split;
        }
        // Renormalize: shift `range` back up into `128..=255`, pulling a fresh
        // byte into `value` every 8 shifts (zero past the end of the partition).
        //
        // The bit-at-a-time reference loop shifts `range`/`value` left until
        // `range >= 128`; the shift count is fixed the instant we know `range`, so
        // apply it in one step. `range < 128` here means its top set bit is at
        // position `<= 6`, so `n = leading_zeros - 24 ∈ 1..=7` shifts land it in
        // `128..=255`. Across `n` shifts at most one byte crosses into `value`
        // (`bit_count + n <= 14 < 16`); the reference would OR that byte into the
        // low 8 bits after `8 - bit_count` shifts, leaving it shifted up by the
        // remaining `n - (8 - bit_count) = (bit_count + n) - 8` — the exact position
        // used below. The result is bit-identical to the per-bit loop.
        if self.range < 128 {
            let n = self.range.leading_zeros() - 24;
            work!(BoolRenorm, u64::from(n));
            self.range <<= n;
            self.value <<= n;
            let new_bit_count = self.bit_count + n;
            if new_bit_count >= 8 {
                let shift = new_bit_count - 8;
                match self.input.get(self.pos) {
                    Some(&b) => self.value |= u32::from(b) << shift,
                    // Past the end: `value |= 0` is a no-op, so the decoded value
                    // is unchanged; only latch that we've entered padding.
                    None => self.exhausted = true,
                }
                self.pos += 1;
                self.bit_count = shift;
            } else {
                self.bit_count = new_bit_count;
            }
        }
        bit
    }

    /// Decode one boolean at equal (`1/2`) probability — a raw bit.
    pub(crate) fn read_flag(&mut self) -> bool {
        self.read_bool(128)
    }

    /// Decode `n` raw bits (each at `prob = 128`) as an unsigned value, MSB
    /// first. VP8 only ever reads small fields this way, so `n <= 24`.
    pub(crate) fn read_literal(&mut self, n: u32) -> u32 {
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | u32::from(self.read_flag());
        }
        v
    }

    /// Decode an `n`-bit magnitude followed by a sign flag (the RFC "signed
    /// value" form), returning the negated magnitude when the sign bit is set.
    pub(crate) fn read_signed(&mut self, n: u32) -> i32 {
        // `n` is always small in VP8, so the magnitude fits an `i32` unscathed.
        let magnitude = i32::try_from(self.read_literal(n)).unwrap_or(i32::MAX);
        self.apply_sign(magnitude)
    }

    /// Apply a trailing sign flag to an already-decoded `magnitude`, negating it
    /// when the sign bit is set. VP8 codes coefficient signs this way, after the
    /// magnitude has been read from the token tree.
    pub(crate) fn apply_sign(&mut self, magnitude: i32) -> i32 {
        if self.read_flag() {
            -magnitude
        } else {
            magnitude
        }
    }
}

#[cfg(test)]
mod tests {
    use super::BoolDecoder;

    #[test]
    fn first_flag_is_the_top_value_bit() {
        // `value` is primed with `(b0 << 8) | b1`; at prob 128 the split point is
        // `128 << 8 = 32768`, so the first flag is one iff `b0`'s top bit is set.
        assert!(BoolDecoder::new(&[0x80, 0x00]).read_flag());
        assert!(!BoolDecoder::new(&[0x7f, 0xff]).read_flag());
    }

    #[test]
    fn renormalization_reads_high_probability_ones() {
        // `[0xff, 0x00, ..]` at prob 128 yields three consecutive one bits,
        // exercising the renormalization shift (hand-computed from RFC 6386 §7.3:
        // each read leaves range 127->254 and subtracts the split from value).
        let mut d = BoolDecoder::new(&[0xff, 0x00, 0x00, 0x00]);
        assert_eq!(
            (d.read_flag(), d.read_flag(), d.read_flag()),
            (true, true, true)
        );
    }

    #[test]
    fn nontrivial_probability_split_is_hand_computed() {
        // A decoder-only KAT at prob != 128 that pins the split formula
        // `split = 1 + (((range - 1) * prob) >> 8)` and the `big_split = split << 8`
        // comparison. A `range * prob` or a dropped `-1` would move `split` by a
        // whole unit and flip these decisions; prob=128 (where split == 128)
        // cannot expose that, so use prob=150 with a distinct primed value.
        //
        // Input [0xC0, 0x00, ..]: value primes to 0xC000 = 49152, range = 255.
        //   read_bool(150):
        //     split = 1 + ((254*150) >> 8) = 1 + (38100 >> 8) = 1 + 148 = 149
        //     big_split = 149 << 8 = 38144; 49152 >= 38144  => TRUE
        //     range = 255-149 = 106; value = 49152-38144 = 11008
        //     renorm once (106<128): range = 212, value = 22016
        //   read_bool(150):
        //     split = 1 + ((211*150) >> 8) = 1 + (31650 >> 8) = 1 + 123 = 124
        //     big_split = 124 << 8 = 31744; 22016 >= 31744  => FALSE
        let mut d = BoolDecoder::new(&[0xc0, 0x00, 0x00, 0x00]);
        assert!(d.read_bool(150));
        assert!(!d.read_bool(150));
    }

    #[test]
    fn short_partition_reads_are_zero_padded() {
        // A documented edge: an empty/short partition must decode deterministically
        // (bytes at or past the end read as zero) without a bounds panic.
        let mut d = BoolDecoder::new(&[]);
        assert_eq!(d.read_literal(16), 0);
        assert!(!d.read_flag());
    }

    #[test]
    fn exhausted_latches_only_after_reading_past_the_end() {
        // The `exhausted` latch must fire exactly when a renormalization pulls a
        // byte past the partition end — the streaming decoder's signal to suspend —
        // while leaving the decoded value unchanged (padding is zero).
        let mut within = BoolDecoder::new(&[0x80, 0x00, 0x00, 0x00]);
        within.read_flag();
        assert!(
            !within.is_exhausted(),
            "a read within bounds must not latch"
        );

        // A one-byte partition: repeated high-probability ones keep pulling bytes,
        // so the decoder soon reads past the end and latches, yet never panics.
        let mut short = BoolDecoder::new(&[0xff]);
        for _ in 0..8 {
            short.read_flag();
        }
        assert!(
            short.is_exhausted(),
            "reading past the end must latch exhausted"
        );
    }
}
