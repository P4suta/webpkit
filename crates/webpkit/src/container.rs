//! WebP RIFF container framing (bitstream-agnostic).
//!
//! This layer understands only RIFF chunks — `FourCC` tags, little-endian sizes,
//! and the odd-size padding byte. It locates a file's image payload (`VP8L` or
//! `VP8 `) and hands it to a codec layer without interpreting a single bit of the
//! bitstream, and it reads/writes the extended (`VP8X`) header and the animation
//! (`ANIM`/`ANMF`) chunks.
pub mod anim;
pub mod fourcc;
pub mod mux;
pub mod reader;
pub mod scan;
pub mod vp8x;
pub mod writer;

/// Read a 24-bit little-endian value from its three bytes. The single source of
/// truth for u24 decoding shared by `vp8x` (canvas) and `anim` (frame geometry).
pub(crate) fn read_u24_le(a: u8, b: u8, c: u8) -> u32 {
    u32::from(a) | (u32::from(b) << 8) | (u32::from(c) << 16)
}

/// Serialize the low 24 bits of `value` as little-endian bytes. Exact inverse of
/// [`read_u24_le`]; the caller guarantees `value < 1 << 24`.
pub(crate) const fn write_u24_le(value: u32) -> [u8; 3] {
    let [a, b, c, _] = value.to_le_bytes();
    [a, b, c]
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::{read_u24_le, write_u24_le};

    proptest! {
        /// `read_u24_le` inverts `write_u24_le` across the entire 24-bit domain.
        #[test]
        fn u24_round_trips_full_domain(value in 0u32..(1 << 24)) {
            let [a, b, c] = write_u24_le(value);
            prop_assert_eq!(read_u24_le(a, b, c), value);
        }
    }

    #[test]
    fn u24_round_trips_across_the_range() {
        for value in [0u32, 1, 255, 256, 0x00_12_34, 0xFF_FF_FF, (1 << 24) - 1] {
            let [a, b, c] = write_u24_le(value);
            assert_eq!(read_u24_le(a, b, c), value);
        }
    }

    #[test]
    fn write_u24_le_is_little_endian_and_drops_the_high_byte() {
        assert_eq!(write_u24_le(0x00_AB_CD_EF), [0xEF, 0xCD, 0xAB]);
    }
}
