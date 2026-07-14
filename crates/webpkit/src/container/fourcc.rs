//! `FourCC` chunk identifiers.

/// A four-character RIFF chunk identifier (e.g. `RIFF`, `WEBP`, `VP8L`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FourCc(pub [u8; 4]);

impl FourCc {
    /// `RIFF` container magic (first four bytes of every WebP file).
    pub const RIFF: Self = Self(*b"RIFF");
    /// `WEBP` form type (bytes 8..12 of the header).
    pub const WEBP: Self = Self(*b"WEBP");
    /// `VP8L` lossless bitstream chunk.
    pub const VP8L: Self = Self(*b"VP8L");
    /// `VP8X` extended-format feature chunk.
    pub const VP8X: Self = Self(*b"VP8X");
    /// `ICCP` color-profile chunk.
    pub const ICCP: Self = Self(*b"ICCP");
    /// `EXIF` metadata chunk.
    pub const EXIF: Self = Self(*b"EXIF");
    /// `XMP ` metadata chunk (note the trailing space).
    pub const XMP: Self = Self(*b"XMP ");
    /// `ANIM` animation-parameters chunk.
    pub const ANIM: Self = Self(*b"ANIM");
    /// `ANMF` animation-frame chunk.
    pub const ANMF: Self = Self(*b"ANMF");
    /// `VP8 ` lossy bitstream chunk (decoded by the lossy codec).
    pub const VP8: Self = Self(*b"VP8 ");
    /// `ALPH` alpha-channel chunk (accompanies a lossy `VP8 ` image).
    pub const ALPH: Self = Self(*b"ALPH");
}
