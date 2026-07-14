//! The VP8 boolean (range) arithmetic **encoder** — RFC 6386 §7.3.
//!
//! The exact inverse of [`crate::lossy::bool_dec::BoolDecoder`]: a binary range coder
//! that writes each symbol against an 8-bit probability that it is zero. Every
//! compressed VP8 partition the encoder emits — the control partition (headers +
//! intra modes) and the token partition (DCT coefficients) — is produced through
//! one of these. Transcribed from the RFC 6386 §7.3 reference `bool_encoder`,
//! with `add_one_to_output` carry propagation and a four-byte flush. A round-trip
//! test pins it against the already-correct decoder.

use crate::lossy::prelude::*;

/// A VP8 boolean (range) encoder writing one compressed partition.
pub(crate) struct BoolEncoder {
    /// Emitted partition bytes; a carry rewrites already-written tail bytes.
    out: Vec<u8>,
    /// The coding range, mirroring the decoder's `range` (renormalized `>= 128`).
    range: u32,
    /// The low end of the coding interval (the RFC `bottom`), pending output.
    value: u32,
    /// Shifts remaining before the next output byte is emitted.
    bit_count: i32,
}

#[allow(
    clippy::new_without_default,
    reason = "a Default impl could not reproduce the RFC init state \
              (range = 255, bit_count = 24) as a meaningful empty value"
)]
impl BoolEncoder {
    /// A fresh encoder over an empty partition (RFC `init_bool_encoder`).
    #[must_use]
    pub(crate) const fn new() -> Self {
        Self {
            out: Vec::new(),
            range: 255,
            value: 0,
            bit_count: 24,
        }
    }

    /// Propagate a carry into the already-written bytes (`add_one_to_output`):
    /// walk back turning `0xff` into `0` until a byte can be incremented.
    fn add_one_to_output(&mut self) {
        for byte in self.out.iter_mut().rev() {
            if *byte == 0xff {
                *byte = 0;
            } else {
                *byte += 1;
                return;
            }
        }
    }

    /// Encode one boolean that is zero with probability `prob / 256` — the exact
    /// inverse of `BoolDecoder::read_bool` (RFC `write_bool`). `split` is the
    /// same `1 + (((range - 1) * prob) >> 8)` the decoder uses.
    pub(crate) fn put_bool(&mut self, prob: u8, bit: bool) {
        let split = 1 + (((self.range - 1) * u32::from(prob)) >> 8);
        if bit {
            self.value += split;
            self.range -= split;
        } else {
            self.range = split;
        }
        while self.range < 128 {
            self.range <<= 1;
            if self.value & 0x8000_0000 != 0 {
                self.add_one_to_output();
            }
            self.value <<= 1;
            self.bit_count -= 1;
            if self.bit_count == 0 {
                self.out.push(self.value.to_be_bytes()[0]);
                self.value &= 0x00ff_ffff;
                self.bit_count = 8;
            }
        }
    }

    /// Encode one equal-probability raw bit (inverse of `read_flag`).
    pub(crate) fn put_flag(&mut self, bit: bool) {
        self.put_bool(128, bit);
    }

    /// Encode `n` bits MSB-first at prob 128 (inverse of `read_literal`, whose
    /// loop is `v = (v << 1) | flag`, so the first flag carries the top bit).
    pub(crate) fn put_literal(&mut self, n: u32, value: u32) {
        let mut i = n;
        while i > 0 {
            i -= 1;
            self.put_flag((value >> i) & 1 == 1);
        }
    }

    /// Encode an `n`-bit magnitude then a sign flag (inverse of `read_signed`).
    pub(crate) fn put_signed(&mut self, n: u32, value: i32) {
        self.put_literal(n, value.unsigned_abs());
        self.put_flag(value < 0);
    }

    /// Flush the residual interval (RFC `flush_bool_encoder`): a possible final
    /// carry, then four bytes of `bottom` written from the top.
    fn flush(&mut self) {
        let bit_count = self.bit_count;
        let mut v = self.value;
        if v & (1u32 << (32 - bit_count)) != 0 {
            self.add_one_to_output();
        }
        v <<= bit_count & 7;
        for _ in 0..(bit_count >> 3) {
            v <<= 8;
        }
        for _ in 0..4 {
            self.out.push(v.to_be_bytes()[0]);
            v <<= 8;
        }
    }

    /// Flush and return the encoded partition bytes.
    #[must_use]
    pub(crate) fn finish(mut self) -> Vec<u8> {
        self.flush();
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::BoolEncoder;
    use crate::lossy::bool_dec::BoolDecoder;

    #[test]
    fn encoder_round_trips_through_the_decoder() {
        // Self-validates the BoolEncoder against the already-correct decoder: a
        // scripted mix of skewed probabilities, MSB-first literals and signed
        // values must read back symbol-for-symbol in the same order they were
        // written. Because each symbol carries a distinct value, a dropped or
        // duplicated renormalization byte, a wrongly propagated carry, or an MSB/LSB
        // swap in the literal codec would surface as a mismatch. This is the
        // primary validation of the encoder (the inverse of `read_*`).
        let bools = [
            (10u8, true),
            (200, false),
            (128, true),
            (1, true),
            (255, false),
            (128, false),
            (64, true),
            (192, true),
            (37, false),
            (222, true),
            (150, true),
            (3, false),
        ];
        let mut enc = BoolEncoder::new();
        for &(p, b) in &bools {
            enc.put_bool(p, b);
        }
        enc.put_literal(12, 0x0abc);
        enc.put_literal(3, 0b101);
        enc.put_signed(9, -173);
        enc.put_signed(5, 7);
        enc.put_signed(4, 0);
        enc.put_flag(true);
        let bytes = enc.finish();

        let mut dec = BoolDecoder::new(&bytes);
        for &(p, b) in &bools {
            assert_eq!(dec.read_bool(p), b, "bool at prob {p}");
        }
        assert_eq!(dec.read_literal(12), 0x0abc);
        assert_eq!(dec.read_literal(3), 0b101);
        assert_eq!(dec.read_signed(9), -173);
        assert_eq!(dec.read_signed(5), 7);
        assert_eq!(dec.read_signed(4), 0);
        assert!(dec.read_flag());
    }

    #[test]
    fn state_and_resume_are_bit_identical_at_every_boundary() {
        // The streaming decoder snapshots a token partition's `BoolState` and
        // rebuilds the decoder with `resume` against the (grown) buffer. This pins
        // that round-trip: from any symbol boundary, a resumed decoder continues
        // bit-for-bit, so re-decoding a suspended macroblock reads identical bits.
        let probs = [
            128u8, 20, 230, 5, 128, 200, 64, 190, 128, 7, 255, 100, 33, 128, 250, 3, 177, 96,
        ];
        let bits = [
            true, false, true, true, false, true, false, false, true, true, false, true, true,
            false, true, false, true, false,
        ];
        let mut enc = BoolEncoder::new();
        for (&p, &b) in probs.iter().zip(&bits) {
            enc.put_bool(p, b);
        }
        let bytes = enc.finish();

        // Reference: one decoder over the whole partition.
        let reference: Vec<bool> = {
            let mut d = BoolDecoder::new(&bytes);
            probs.iter().map(|&p| d.read_bool(p)).collect()
        };
        assert_eq!(reference, bits);

        // From every boundary, snapshot and resume; the tail must match exactly.
        for cut in 0..=probs.len() {
            let mut a = BoolDecoder::new(&bytes);
            for &p in &probs[..cut] {
                a.read_bool(p);
            }
            let mut b = BoolDecoder::resume(&bytes, a.state());
            for (i, &p) in probs[cut..].iter().enumerate() {
                assert_eq!(
                    b.read_bool(p),
                    reference[cut + i],
                    "cut {cut}, tail symbol {i}"
                );
            }
        }
    }

    #[test]
    fn read_signed_decodes_a_set_sign_bit_as_negative() {
        // A genuine negative value: a non-zero magnitude with the sign flag SET
        // must decode as negative, not positive and not -0. Encode magnitude 5
        // over n=4 bits (0101) followed by a set sign -> -5.
        let mut enc = BoolEncoder::new();
        enc.put_signed(4, -5);
        let bytes = enc.finish();
        assert_eq!(BoolDecoder::new(&bytes).read_signed(4), -5);

        // The mirror boundary: a non-zero magnitude with a CLEAR sign stays
        // positive (so the sign flag, not the magnitude bits, controls the sign).
        let mut enc_pos = BoolEncoder::new();
        enc_pos.put_signed(6, 21);
        let bytes_pos = enc_pos.finish();
        assert_eq!(BoolDecoder::new(&bytes_pos).read_signed(6), 21);

        // And the largest magnitude representable in the field round-trips too.
        let mut enc_max = BoolEncoder::new();
        enc_max.put_signed(4, -15);
        let bytes_max = enc_max.finish();
        assert_eq!(BoolDecoder::new(&bytes_max).read_signed(4), -15);
    }

    #[test]
    fn signed_zero_emits_a_clear_sign_flag() {
        // `put_signed` writes the magnitude then a sign flag = `value < 0`. For a
        // value of 0 that flag must be CLEAR (a `< 0 -> <= 0` mutation would set it).
        // A round-trip decode alone cannot see this (both signs decode +0 to 0), so
        // read the magnitude and the sign flag back separately and pin the flag.
        let mut enc = BoolEncoder::new();
        enc.put_signed(5, 0);
        let bytes = enc.finish();
        let mut dec = BoolDecoder::new(&bytes);
        assert_eq!(dec.read_literal(5), 0, "zero magnitude");
        assert!(!dec.read_flag(), "sign flag for +0 must be clear");
    }

    #[test]
    fn mixed_probability_literal_survives_renormalization() {
        // A multi-symbol KAT spanning several byte loads: a wide 24-bit literal at
        // prob 128 interleaved with strongly-skewed bools (prob 20 / 230 / 5)
        // forces the decoder to pull fresh bytes mid-stream, so an ill-timed byte
        // load or a lost carry would corrupt a later symbol. Round-trips exactly.
        let mut enc = BoolEncoder::new();
        enc.put_bool(20, true);
        enc.put_literal(24, 0x00ab_cdef);
        enc.put_bool(230, false);
        enc.put_bool(5, true);
        enc.put_literal(16, 0xbeef);
        enc.put_bool(96, true);
        enc.put_signed(11, -1000);
        let bytes = enc.finish();

        let mut dec = BoolDecoder::new(&bytes);
        assert!(dec.read_bool(20));
        assert_eq!(dec.read_literal(24), 0x00ab_cdef);
        assert!(!dec.read_bool(230));
        assert!(dec.read_bool(5));
        assert_eq!(dec.read_literal(16), 0xbeef);
        assert!(dec.read_bool(96));
        assert_eq!(dec.read_signed(11), -1000);
    }
}
