//! The uncompressed VP8 frame header.
//!
//! Every VP8 frame begins with a 3-byte little-endian *frame tag* (frame type,
//! version, show-frame flag, first-partition size). A key frame follows it with a
//! fixed 3-byte start code and two 16-bit fields packing the 14-bit width/height
//! and a 2-bit upscale factor each. This layout is fixed bytes — no arithmetic
//! coding — so it is parsed and tested here without the boolean decoder, and
//! independently of any reference implementation (RFC 6386 §9.1, §19.1).

use crate::{Codec, Error, Result};

/// The fixed 3-byte start code that follows the frame tag of a key frame.
const KEY_FRAME_START_CODE: [u8; 3] = [0x9d, 0x01, 0x2a];
/// Total length of a key frame's uncompressed header: tag + start code + dims.
pub(crate) const KEY_FRAME_HEADER_LEN: usize = 10;
/// Mask selecting the 14-bit dimension field (the top 2 bits carry the scale).
const DIMENSION_MASK: u16 = 0x3fff;

/// The parsed uncompressed header of a VP8 key frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FrameHeader {
    /// Whether this is a key frame. A stand-alone WebP lossy image always is;
    /// inter frames are only meaningful inside a video and are rejected here.
    pub key_frame: bool,
    /// Bitstream version (0..=3), selecting the reconstruction / loop filter.
    pub version: u8,
    /// Whether the frame is meant to be displayed.
    pub show_frame: bool,
    /// Size in bytes of the first (control) partition that follows this header.
    pub first_partition_size: u32,
    /// Frame width in pixels (1..=16383).
    pub width: u16,
    /// Frame height in pixels (1..=16383).
    pub height: u16,
    /// Horizontal upscale factor (0..=3) stored in the top 2 bits of the width.
    pub x_scale: u8,
    /// Vertical upscale factor (0..=3) stored in the top 2 bits of the height.
    pub y_scale: u8,
}

impl FrameHeader {
    /// Parse the 10-byte uncompressed header of a VP8 **key frame** from the front
    /// of `payload` (the raw contents of a WebP `VP8 ` chunk).
    ///
    /// # Errors
    ///
    /// [`Error::Truncated`] if fewer than 10 bytes are present, or
    /// [`Error::InvalidBitstream`] for an inter frame, a wrong start code, or a
    /// zero width/height.
    pub fn parse_key_frame(payload: &[u8]) -> Result<Self> {
        let head = payload
            .get(..KEY_FRAME_HEADER_LEN)
            .ok_or(Error::Truncated)?;

        // Frame tag: 3 bytes little-endian. All sub-fields live in bytes 0..3, so
        // extract them per byte to avoid any truncating cast.
        let b0 = head[0];
        let key_frame = b0 & 0x01 == 0; // bit 0: 0 => key frame
        if !key_frame {
            return Err(Error::InvalidBitstream {
                codec: Codec::Lossy,
            });
        }
        let version = (b0 >> 1) & 0x07; // bits 1..=3
        let show_frame = (b0 >> 4) & 0x01 == 1; // bit 4
        // bits 5..=23: 3 high bits of b0, then b1, then b2.
        let first_partition_size =
            (u32::from(b0) >> 5) | (u32::from(head[1]) << 3) | (u32::from(head[2]) << 11);

        if head[3..6] != KEY_FRAME_START_CODE {
            return Err(Error::InvalidBitstream {
                codec: Codec::Lossy,
            });
        }

        // Width / height: 16-bit little-endian, low 14 bits size, top 2 bits scale.
        let width_field = u16::from(head[6]) | (u16::from(head[7]) << 8);
        let height_field = u16::from(head[8]) | (u16::from(head[9]) << 8);
        let width = width_field & DIMENSION_MASK;
        let height = height_field & DIMENSION_MASK;
        // The scale is bits 14..=15, i.e. the top 2 bits of the high byte.
        let x_scale = head[7] >> 6;
        let y_scale = head[9] >> 6;
        if width == 0 || height == 0 {
            return Err(Error::InvalidBitstream {
                codec: Codec::Lossy,
            });
        }

        Ok(Self {
            key_frame,
            version,
            show_frame,
            first_partition_size,
            width,
            height,
            x_scale,
            y_scale,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::{Codec, Error};

    use super::{FrameHeader, KEY_FRAME_START_CODE};

    /// Assemble a key-frame header from explicit fields (no truncating casts).
    fn header(tag: [u8; 3], width: u16, height: u16) -> [u8; 10] {
        let [wl, wh] = width.to_le_bytes();
        let [hl, hh] = height.to_le_bytes();
        [
            tag[0],
            tag[1],
            tag[2],
            KEY_FRAME_START_CODE[0],
            KEY_FRAME_START_CODE[1],
            KEY_FRAME_START_CODE[2],
            wl,
            wh,
            hl,
            hh,
        ]
    }

    #[test]
    fn parses_a_minimal_key_frame() {
        // tag 0x10: key_frame=0, version=0, show_frame=1, first-partition size 0.
        let bytes = header([0x10, 0x00, 0x00], 16, 16);
        let h = FrameHeader::parse_key_frame(&bytes).unwrap();
        assert!(h.key_frame && h.show_frame);
        assert_eq!((h.version, h.first_partition_size), (0, 0));
        assert_eq!((h.width, h.height), (16, 16));
        assert_eq!((h.x_scale, h.y_scale), (0, 0));
    }

    #[test]
    fn decodes_tag_subfields() {
        // Build tag with version=3, show_frame=1, first_partition_size=100.
        // tag = (100 << 5) | (1 << 4) | (3 << 1) = 3200 | 16 | 6 = 3222 = 0x0C96.
        let bytes = header([0x96, 0x0c, 0x00], 640, 480);
        let h = FrameHeader::parse_key_frame(&bytes).unwrap();
        assert_eq!(h.version, 3);
        assert!(h.show_frame);
        assert_eq!(h.first_partition_size, 100);
        assert_eq!((h.width, h.height), (640, 480));
    }

    #[test]
    fn extracts_the_two_bit_scale_from_the_dimension_fields() {
        // width field = 16 | (x_scale=1 << 14); height field = 16 | (y_scale=2 << 14).
        let bytes = header([0x10, 0x00, 0x00], 16 | (1 << 14), 16 | (2 << 14));
        let h = FrameHeader::parse_key_frame(&bytes).unwrap();
        assert_eq!((h.width, h.height), (16, 16));
        assert_eq!((h.x_scale, h.y_scale), (1, 2));
    }

    #[test]
    fn decodes_a_large_first_partition_size_across_all_three_tag_bytes() {
        // The first-partition size is 19 bits spanning the top 3 bits of tag byte 0,
        // all of byte 1, and all of byte 2 (`b0>>5 | head[1]<<3 | head[2]<<11`). A
        // value large enough to occupy byte 2 (>= 2048) proves the top byte really
        // shifts LEFT into position 11..18 — a `head[2] << 11 -> >> 11` mutation
        // would zero those bits (head[2] is a u8, so `>> 11` is always 0) and lose
        // the high part of the size.
        let fps = 300_000u32; // 0x493E0: needs bits up to 18, so head[2] != 0.
        let tag = ((1u32 << 4) | (fps << 5)).to_le_bytes();
        let bytes = header([tag[0], tag[1], tag[2]], 320, 240);
        let h = FrameHeader::parse_key_frame(&bytes).unwrap();
        assert_eq!(
            h.first_partition_size, fps,
            "19-bit size across all 3 tag bytes"
        );
        assert_eq!((h.width, h.height), (320, 240));
    }

    #[test]
    fn rejects_an_inter_frame() {
        let bytes = header([0x11, 0x00, 0x00], 16, 16); // bit 0 set => inter frame
        assert_eq!(
            FrameHeader::parse_key_frame(&bytes).unwrap_err(),
            Error::InvalidBitstream {
                codec: Codec::Lossy
            }
        );
    }

    #[test]
    fn rejects_a_wrong_start_code() {
        let mut bytes = header([0x10, 0x00, 0x00], 16, 16);
        bytes[4] = 0x00; // corrupt the middle start-code byte
        assert_eq!(
            FrameHeader::parse_key_frame(&bytes).unwrap_err(),
            Error::InvalidBitstream {
                codec: Codec::Lossy
            }
        );
    }

    #[test]
    fn rejects_zero_dimensions() {
        let bytes = header([0x10, 0x00, 0x00], 0, 16);
        assert_eq!(
            FrameHeader::parse_key_frame(&bytes).unwrap_err(),
            Error::InvalidBitstream {
                codec: Codec::Lossy
            }
        );
    }

    #[test]
    fn rejects_a_truncated_header() {
        assert_eq!(
            FrameHeader::parse_key_frame(&[0u8; 9]).unwrap_err(),
            Error::Truncated
        );
    }
}
