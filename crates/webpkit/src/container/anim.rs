//! The animation chunks: `ANIM` (global parameters) and the `ANMF` frame header.
//!
//! This module handles only the fixed byte layouts — the u32/u24 fields and the
//! flags byte — mirroring [`super::vp8x`]. Frame *pixel* data (the nested `VP8L`
//! sub-chunk) is decoded a layer up in the codec's animation module; the semantic
//! meaning of the flag bits (blend / dispose) is interpreted there too. Keeping
//! the flags as raw bits here preserves the one-directional layering (`container`
//! never depends on a codec's public composition layer).

use super::{read_u24_le, write_u24_le};
use crate::error::{Error, Result};
use crate::image::Dimensions;

/// Byte length of an `ANIM` chunk payload (background u32 + loop count u16).
pub const ANIM_PAYLOAD_LEN: usize = 6;
/// Byte length of the fixed `ANMF` frame header that precedes the frame data.
pub const ANMF_HEADER_LEN: usize = 16;

/// The `ANIM` chunk: canvas-wide animation parameters.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AnimChunk {
    /// Background color as native ARGB (`0xAARRGGBB`). The chunk stores it as a
    /// little-endian `[B, G, R, A]` u32, which read as LE **is** native ARGB, so
    /// no channel swap is needed.
    pub background: u32,
    /// Loop count; `0` means loop forever.
    pub loop_count: u16,
}

impl AnimChunk {
    /// Parse a 6-byte `ANIM` payload.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidContainer`] if the payload length is not exactly 6.
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() != ANIM_PAYLOAD_LEN {
            return Err(Error::InvalidContainer);
        }
        Ok(Self {
            background: u32::from_le_bytes([data[0], data[1], data[2], data[3]]),
            loop_count: u16::from_le_bytes([data[4], data[5]]),
        })
    }

    /// Serialize into a 6-byte `ANIM` payload.
    #[must_use]
    pub const fn build(self) -> [u8; ANIM_PAYLOAD_LEN] {
        let bg = self.background.to_le_bytes();
        let lc = self.loop_count.to_le_bytes();
        [bg[0], bg[1], bg[2], bg[3], lc[0], lc[1]]
    }
}

/// The `ANMF` frame flags byte. Bit 1 is the blending method, bit 0 the disposal
/// method (the top six bits are reserved). Semantics are interpreted a layer up.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AnmfFlags(pub u8);

impl AnmfFlags {
    /// Bit 1 set means "do not blend" (overwrite the rectangle).
    const BLEND: u8 = 0x02;
    /// Bit 0 set means "dispose to background" after the frame is shown.
    const DISPOSE: u8 = 0x01;

    /// Whether the frame must overwrite (rather than alpha-blend over) the canvas.
    #[must_use]
    pub const fn do_not_blend(self) -> bool {
        self.0 & Self::BLEND != 0
    }
    /// Whether the frame's rectangle is cleared to background after display.
    #[must_use]
    pub const fn dispose_background(self) -> bool {
        self.0 & Self::DISPOSE != 0
    }
    /// Compose the flags byte from its two boolean methods.
    #[must_use]
    pub const fn from_parts(do_not_blend: bool, dispose_background: bool) -> Self {
        let mut bits = 0u8;
        if do_not_blend {
            bits |= Self::BLEND;
        }
        if dispose_background {
            bits |= Self::DISPOSE;
        }
        Self(bits)
    }
}

/// The fixed 16-byte `ANMF` frame header (offset, size, duration, flags) that
/// precedes a frame's pixel sub-chunks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AnmfHeader {
    /// Pixel X offset of the frame's top-left corner (always even).
    pub x: u32,
    /// Pixel Y offset of the frame's top-left corner (always even).
    pub y: u32,
    /// Frame dimensions (stored on the wire minus one).
    pub dims: Dimensions,
    /// Display duration in milliseconds (stored as a 24-bit field).
    pub duration_ms: u32,
    /// Blend / dispose flags.
    pub flags: AnmfFlags,
}

impl AnmfHeader {
    /// Parse the 16-byte header at the front of an `ANMF` payload. The remaining
    /// bytes (`data[ANMF_HEADER_LEN..]`) are the frame's sub-chunks.
    ///
    /// The X/Y offsets are stored halved (the wire value is the pixel offset / 2)
    /// and the dimensions minus one, matching the container spec.
    ///
    /// # Errors
    ///
    /// [`Error::Truncated`] if fewer than 16 bytes are present, or
    /// [`Error::InvalidContainer`] if the frame dimensions are out of range.
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < ANMF_HEADER_LEN {
            return Err(Error::Truncated);
        }
        let x = read_u24_le(data[0], data[1], data[2]) * 2;
        let y = read_u24_le(data[3], data[4], data[5]) * 2;
        let width = read_u24_le(data[6], data[7], data[8]) + 1;
        let height = read_u24_le(data[9], data[10], data[11]) + 1;
        let duration_ms = read_u24_le(data[12], data[13], data[14]);
        let flags = AnmfFlags(data[15]);
        let dims = Dimensions::new(width, height).map_err(|_| Error::InvalidContainer)?;
        Ok(Self {
            x,
            y,
            dims,
            duration_ms,
            flags,
        })
    }

    /// Serialize into the 16-byte frame header. Assumes even `x`/`y` and a
    /// `duration_ms` that fits in 24 bits (the encoder validates both); only the
    /// low bits are stored otherwise.
    #[must_use]
    pub const fn build(self) -> [u8; ANMF_HEADER_LEN] {
        let x = write_u24_le(self.x / 2);
        let y = write_u24_le(self.y / 2);
        let width = write_u24_le(self.dims.width() - 1);
        let height = write_u24_le(self.dims.height() - 1);
        let dur = write_u24_le(self.duration_ms);
        [
            x[0],
            x[1],
            x[2],
            y[0],
            y[1],
            y[2],
            width[0],
            width[1],
            width[2],
            height[0],
            height[1],
            height[2],
            dur[0],
            dur[1],
            dur[2],
            self.flags.0,
        ]
    }
}

/// How a frame combines with the canvas underneath it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum BlendMode {
    /// Alpha-blend the frame over the canvas (`ANMF` blend bit clear).
    #[default]
    Blend,
    /// Overwrite the frame's rectangle, ignoring what is underneath (bit set).
    Overwrite,
}

/// What happens to a frame's rectangle after the frame has been displayed.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum DisposalMode {
    /// Leave the canvas as-is for the next frame (`ANMF` dispose bit clear).
    #[default]
    Keep,
    /// Clear the frame's rectangle before the next frame (bit set). libwebp
    /// clears to transparent, not to the background color.
    Background,
}

/// A single frame's placement and timing within the animation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FrameMeta {
    /// Pixel X offset of the frame's top-left corner (always even).
    pub x: u32,
    /// Pixel Y offset of the frame's top-left corner (always even).
    pub y: u32,
    /// The frame's own dimensions (may be smaller than the canvas).
    pub dimensions: Dimensions,
    /// Display duration in milliseconds.
    pub duration_ms: u32,
    /// How the frame combines with the canvas.
    pub blend: BlendMode,
    /// What happens to the frame's rectangle afterwards.
    pub dispose: DisposalMode,
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::{ANIM_PAYLOAD_LEN, ANMF_HEADER_LEN, AnimChunk, AnmfFlags, AnmfHeader};
    use crate::error::Error;
    use crate::image::Dimensions;

    proptest! {
        #[test]
        fn anim_chunk_parse_build_round_trips(background in any::<u32>(), loop_count in any::<u16>()) {
            let anim = AnimChunk {
                background,
                loop_count,
            };
            prop_assert_eq!(AnimChunk::parse(&anim.build()).unwrap(), anim);
        }

        /// Offsets are stored halved, so any even offset round-trips exactly.
        #[test]
        fn anmf_header_parse_build_round_trips(
            x_half in 0u32..(1 << 23),
            y_half in 0u32..(1 << 23),
            w in 1u32..=16384,
            h in 1u32..=16384,
            duration_ms in 0u32..(1 << 24),
            flags in any::<u8>(),
        ) {
            let header = AnmfHeader {
                x: x_half * 2,
                y: y_half * 2,
                dims: Dimensions::new(w, h).unwrap(),
                duration_ms,
                flags: AnmfFlags(flags),
            };
            prop_assert_eq!(AnmfHeader::parse(&header.build()).unwrap(), header);
        }
    }

    #[test]
    fn anim_chunk_round_trips() {
        let anim = AnimChunk {
            background: 0x80_11_22_33,
            loop_count: 5,
        };
        assert_eq!(AnimChunk::parse(&anim.build()).unwrap(), anim);
    }

    #[test]
    fn anim_background_is_stored_bgra_little_endian() {
        // Native ARGB 0xAARRGGBB -> LE bytes [BB, GG, RR, AA] = spec's [B,G,R,A].
        // A=0x80, R=0x12, G=0x34, B=0x56.
        let anim = AnimChunk {
            background: 0x80_12_34_56,
            loop_count: 0,
        };
        let bytes = anim.build();
        assert_eq!(&bytes[0..4], &[0x56, 0x34, 0x12, 0x80]);
    }

    #[test]
    fn anim_parse_rejects_wrong_length() {
        assert_eq!(
            AnimChunk::parse(&[0u8; ANIM_PAYLOAD_LEN - 1]).unwrap_err(),
            Error::InvalidContainer
        );
        assert_eq!(
            AnimChunk::parse(&[0u8; ANIM_PAYLOAD_LEN + 1]).unwrap_err(),
            Error::InvalidContainer
        );
    }

    #[test]
    fn anmf_flags_bit_positions() {
        assert_eq!(AnmfFlags::from_parts(false, false).0, 0x00);
        assert_eq!(AnmfFlags::from_parts(true, false).0, 0x02); // blend bit
        assert_eq!(AnmfFlags::from_parts(false, true).0, 0x01); // dispose bit
        assert_eq!(AnmfFlags::from_parts(true, true).0, 0x03);
        assert!(AnmfFlags(0x02).do_not_blend() && !AnmfFlags(0x02).dispose_background());
        assert!(AnmfFlags(0x01).dispose_background() && !AnmfFlags(0x01).do_not_blend());
    }

    #[test]
    fn anmf_header_round_trips_with_doubled_offsets() {
        let header = AnmfHeader {
            x: 20,
            y: 8,
            dims: Dimensions::new(64, 48).unwrap(),
            duration_ms: 100,
            flags: AnmfFlags::from_parts(false, true),
        };
        let bytes = header.build();
        // Offsets are stored halved: x=20 -> wire 10, y=8 -> wire 4.
        assert_eq!(&bytes[0..3], &[10, 0, 0]);
        assert_eq!(&bytes[3..6], &[4, 0, 0]);
        // Dimensions are stored minus one: 64 -> 63, 48 -> 47.
        assert_eq!(&bytes[6..9], &[63, 0, 0]);
        assert_eq!(&bytes[9..12], &[47, 0, 0]);
        assert_eq!(AnmfHeader::parse(&bytes).unwrap(), header);
    }

    #[test]
    fn anmf_parse_reads_only_the_header_and_ignores_trailing_frame_data() {
        let header = AnmfHeader {
            x: 0,
            y: 0,
            dims: Dimensions::new(2, 2).unwrap(),
            duration_ms: 40,
            flags: AnmfFlags(0),
        };
        let mut bytes = header.build().to_vec();
        bytes.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]); // pretend frame data
        assert_eq!(AnmfHeader::parse(&bytes).unwrap(), header);
    }

    #[test]
    fn anmf_parse_rejects_short_header() {
        assert_eq!(
            AnmfHeader::parse(&[0u8; ANMF_HEADER_LEN - 1]).unwrap_err(),
            Error::Truncated
        );
    }
}
