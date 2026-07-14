//! The VP8X extended-format chunk: a feature-flag byte plus the 24-bit canvas
//! dimensions. Present only in extended files (metadata / ICC / animation).

use super::{read_u24_le, write_u24_le};
use crate::error::{Error, Result};
use crate::image::{Dimensions, Metadata};

/// Byte length of a VP8X chunk payload (flags + 3 reserved + 2×u24 canvas).
pub const VP8X_PAYLOAD_LEN: usize = 10;

/// The VP8X flags byte. Masks are fixed by the WebP container spec.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Vp8xFlags(pub u8);

impl Vp8xFlags {
    const ICC: u8 = 0x20;
    const ALPHA: u8 = 0x10;
    const EXIF: u8 = 0x08;
    const XMP: u8 = 0x04;
    const ANIMATION: u8 = 0x02;

    /// Whether the ICC-profile flag is set.
    #[must_use]
    pub const fn has_icc(self) -> bool {
        self.0 & Self::ICC != 0
    }
    /// Whether the alpha flag is set (advisory for lossless).
    #[must_use]
    pub const fn has_alpha(self) -> bool {
        self.0 & Self::ALPHA != 0
    }
    /// Whether the Exif-metadata flag is set.
    #[must_use]
    pub const fn has_exif(self) -> bool {
        self.0 & Self::EXIF != 0
    }
    /// Whether the XMP-metadata flag is set.
    #[must_use]
    pub const fn has_xmp(self) -> bool {
        self.0 & Self::XMP != 0
    }
    /// Whether the animation flag is set.
    #[must_use]
    pub const fn is_animated(self) -> bool {
        self.0 & Self::ANIMATION != 0
    }

    /// Compose the flags for an output file from its metadata and alpha usage.
    #[must_use]
    pub const fn for_output(metadata: &Metadata, has_alpha: bool) -> Self {
        let mut bits = 0u8;
        if metadata.icc_profile.is_some() {
            bits |= Self::ICC;
        }
        if has_alpha {
            bits |= Self::ALPHA;
        }
        if metadata.exif.is_some() {
            bits |= Self::EXIF;
        }
        if metadata.xmp.is_some() {
            bits |= Self::XMP;
        }
        Self(bits)
    }

    /// Set the animation flag (an animated file always uses the extended form).
    #[must_use]
    pub const fn with_animation(self) -> Self {
        Self(self.0 | Self::ANIMATION)
    }
}

/// A parsed VP8X chunk: its feature flags and canvas size.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Vp8xInfo {
    /// The extended-format feature flags.
    pub flags: Vp8xFlags,
    /// The declared canvas dimensions.
    pub canvas: Dimensions,
}

impl Vp8xInfo {
    /// Parse a 10-byte VP8X payload. The three reserved bytes are ignored (per
    /// libwebp's tolerance); an out-of-range canvas is rejected.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidContainer`] if the length is wrong or the canvas exceeds
    /// the VP8L dimension range.
    pub fn parse(data: &[u8]) -> Result<Self> {
        if data.len() != VP8X_PAYLOAD_LEN {
            return Err(Error::InvalidContainer);
        }
        let flags = Vp8xFlags(data[0]);
        // Canvas dimensions are stored minus-one as 24-bit little-endian.
        let width = read_u24_le(data[4], data[5], data[6]) + 1;
        let height = read_u24_le(data[7], data[8], data[9]) + 1;
        let canvas = Dimensions::new(width, height).map_err(|_| Error::InvalidContainer)?;
        Ok(Self { flags, canvas })
    }

    /// Serialize this info into a 10-byte VP8X payload.
    #[must_use]
    pub const fn build(flags: Vp8xFlags, canvas: Dimensions) -> [u8; VP8X_PAYLOAD_LEN] {
        let w = write_u24_le(canvas.width() - 1);
        let h = write_u24_le(canvas.height() - 1);
        [flags.0, 0, 0, 0, w[0], w[1], w[2], h[0], h[1], h[2]]
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::{Vp8xFlags, Vp8xInfo};
    use crate::error::Error;
    use crate::image::{Dimensions, Metadata};

    proptest! {
        /// `build` then `parse` reproduces any flags byte and any in-range canvas.
        #[test]
        fn build_parse_round_trips(flags in any::<u8>(), w in 1u32..=16384, h in 1u32..=16384) {
            let info = Vp8xInfo {
                flags: Vp8xFlags(flags),
                canvas: Dimensions::new(w, h).unwrap(),
            };
            let parsed = Vp8xInfo::parse(&Vp8xInfo::build(info.flags, info.canvas)).unwrap();
            prop_assert_eq!(parsed, info);
        }
    }

    #[test]
    fn flags_for_output_and_accessors() {
        // ICC + EXIF present, no alpha/xmp.
        let metadata = Metadata {
            icc_profile: Some(vec![1]),
            exif: Some(vec![2]),
            ..Metadata::none()
        };
        let f = Vp8xFlags::for_output(&metadata, false);
        assert!(f.has_icc() && f.has_exif());
        assert!(!f.has_alpha() && !f.has_xmp() && !f.is_animated());
        assert_eq!(f.0, 0x20 | 0x08);
        // Accessors on a raw flags byte (alpha + animation).
        let raw = Vp8xFlags(0x10 | 0x02);
        assert!(raw.has_alpha() && raw.is_animated());
    }

    #[test]
    fn accessors_read_only_their_own_bit() {
        // Every accessor is false on an all-clear byte: pins `& 0` semantics so a
        // `-> true`, `& -> |`, or `& -> ^` mutation (all of which would report the
        // bit as set) is caught.
        let none = Vp8xFlags(0);
        assert!(!none.has_icc());
        assert!(!none.has_alpha());
        assert!(!none.has_exif());
        assert!(!none.has_xmp());
        assert!(!none.is_animated());
        // And true when only that one bit is set.
        assert!(Vp8xFlags(0x20).has_icc());
        assert!(Vp8xFlags(0x10).has_alpha());
        assert!(Vp8xFlags(0x08).has_exif());
        assert!(Vp8xFlags(0x04).has_xmp());
        assert!(Vp8xFlags(0x02).is_animated());
    }

    #[test]
    fn with_animation_sets_the_bit_and_preserves_others() {
        // OR the animation bit in, keeping the existing flags (kills `| -> &`).
        assert_eq!(Vp8xFlags(0x20).with_animation().0, 0x22);
        // Already-animated stays animated (kills `| -> ^`, which would clear it).
        assert_eq!(Vp8xFlags(0x02).with_animation().0, 0x02);
    }

    #[test]
    fn parse_build_round_trips() {
        let canvas = Dimensions::new(640, 480).unwrap();
        let metadata = Metadata {
            icc_profile: Some(vec![1]),
            xmp: Some(vec![2]),
            ..Metadata::none()
        };
        let flags = Vp8xFlags::for_output(&metadata, true); // icc + alpha + xmp
        let bytes = Vp8xInfo::build(flags, canvas);
        let info = Vp8xInfo::parse(&bytes).unwrap();
        assert_eq!(info.flags, flags);
        assert_eq!(info.canvas, canvas);
    }

    #[test]
    fn build_stores_canvas_minus_one() {
        let bytes = Vp8xInfo::build(Vp8xFlags::default(), Dimensions::new(1, 1).unwrap());
        // width-1 = 0, height-1 = 0 across the six canvas bytes.
        assert_eq!(&bytes[4..10], &[0, 0, 0, 0, 0, 0]);
        let bytes = Vp8xInfo::build(Vp8xFlags::default(), Dimensions::new(256, 2).unwrap());
        assert_eq!(&bytes[4..7], &[255, 0, 0]); // width-1 = 255
        assert_eq!(&bytes[7..10], &[1, 0, 0]); // height-1 = 1
    }

    #[test]
    fn parse_rejects_bad_length_and_oversized_canvas() {
        assert_eq!(
            Vp8xInfo::parse(&[0u8; 9]).unwrap_err(),
            Error::InvalidContainer
        );
        // Canvas width-1 = 0xFFFFFF -> width 0x1000000 > 16384 -> rejected.
        let bad = [0u8, 0, 0, 0, 0xff, 0xff, 0xff, 0, 0, 0];
        assert_eq!(Vp8xInfo::parse(&bad).unwrap_err(), Error::InvalidContainer);
    }

    #[test]
    fn parse_ignores_reserved_bytes() {
        let mut bytes = Vp8xInfo::build(Vp8xFlags(0x10), Dimensions::new(8, 8).unwrap());
        bytes[1] = 0xff; // reserved bytes set; tolerated
        bytes[2] = 0xff;
        bytes[3] = 0xff;
        let info = Vp8xInfo::parse(&bytes).unwrap();
        assert!(info.flags.has_alpha());
        assert_eq!(info.canvas, Dimensions::new(8, 8).unwrap());
    }
}
