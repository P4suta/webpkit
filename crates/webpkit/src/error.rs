//! The shared, hand-written, dependency-free error type for the WebP codecs.
//!
//! [`Error`] is `#[non_exhaustive]`, so new failure modes can be added without a
//! breaking change. It stays `Clone + PartialEq + Eq` (handy in tests and
//! matching) — the `std`-only [`Error::Io`] variant therefore carries a
//! [`std::io::ErrorKind`] (which is `Copy + Eq`) rather than the un-cloneable
//! [`std::io::Error`].

/// Which of the two WebP bitstream codecs a bitstream error refers to.
///
/// A closed set: the WebP still-image format carries exactly VP8L (lossless) or
/// VP8 (lossy), so this is deliberately *not* `#[non_exhaustive]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    /// VP8L — the lossless codec.
    Lossless,
    /// VP8 — the lossy codec.
    Lossy,
}

/// Errors returned by the WebP codecs (VP8L lossless, VP8 lossy) and this shell crate.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// The input is not a well-formed WebP file (bad `RIFF`/`WEBP` magic).
    NotWebp,
    /// The input ended in the middle of a header, chunk, or bitstream.
    Truncated,
    /// The file contains no image chunk (VP8L or VP8) that this crate can decode.
    MissingImage,
    /// The input is not a well-formed WebP bitstream; `codec` names which
    /// bitstream (VP8L lossless or VP8 lossy) failed to parse — a bad Huffman
    /// stream / transform for `Lossless`, or a bad frame tag / key-frame start
    /// code / dimensions for `Lossy`.
    InvalidBitstream {
        /// Which codec's bitstream was malformed.
        codec: Codec,
    },
    /// A requested width or height is `0` or exceeds the VP8L maximum of `16384`
    /// (the crate-internal `MAX_DIMENSION`).
    InvalidDimensions,
    /// The pixel buffer length does not equal `width * height * 4` bytes.
    PixelBufferMismatch,
    /// A well-formed WebP that uses a feature this build does not decode.
    UnsupportedFeature,
    /// The extended (`VP8X`) container or one of its chunks is malformed.
    InvalidContainer,
    /// The image's pixel count exceeds a caller-supplied decode limit; reported
    /// before any large allocation so a hostile header cannot exhaust memory.
    LimitExceeded {
        /// The image's `width * height`.
        pixels: u64,
        /// The configured maximum.
        limit: u64,
    },
    /// An I/O error from a [`std::io::Read`]/[`std::io::Write`] source. The
    /// [`std::io::ErrorKind`] is retained; the original message is not.
    #[cfg(feature = "std")]
    Io(std::io::ErrorKind),
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotWebp => f.write_str("not a WebP file (bad RIFF/WEBP magic)"),
            Self::Truncated => f.write_str("unexpected end of input"),
            Self::MissingImage => f.write_str("no decodable image chunk (VP8L or VP8)"),
            Self::InvalidBitstream {
                codec: Codec::Lossless,
            } => f.write_str("invalid WebP VP8L (lossless) bitstream"),
            Self::InvalidBitstream {
                codec: Codec::Lossy,
            } => f.write_str("invalid WebP VP8 (lossy) bitstream"),
            Self::InvalidDimensions => f.write_str("image dimensions out of range (1..=16384)"),
            Self::PixelBufferMismatch => {
                f.write_str("pixel buffer length does not match width * height * 4")
            },
            Self::UnsupportedFeature => f.write_str("unsupported WebP feature"),
            Self::InvalidContainer => f.write_str("malformed WebP extended (VP8X) container"),
            Self::LimitExceeded { pixels, limit } => {
                write!(
                    f,
                    "image has {pixels} pixels, exceeding the limit of {limit}"
                )
            },
            #[cfg(feature = "std")]
            Self::Io(kind) => write!(f, "I/O error: {kind:?}"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for Error {}

#[cfg(feature = "std")]
impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err.kind())
    }
}

/// Result alias for WebP operations.
pub type Result<T> = core::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::{Codec, Error};

    #[test]
    fn display_is_non_empty_for_every_variant() {
        let variants = [
            Error::NotWebp,
            Error::Truncated,
            Error::MissingImage,
            Error::InvalidBitstream {
                codec: Codec::Lossless,
            },
            Error::InvalidBitstream {
                codec: Codec::Lossy,
            },
            Error::InvalidDimensions,
            Error::PixelBufferMismatch,
            Error::UnsupportedFeature,
            Error::InvalidContainer,
            Error::LimitExceeded {
                pixels: 100,
                limit: 10,
            },
        ];
        for variant in variants {
            assert!(!variant.to_string().is_empty());
        }
    }

    #[test]
    fn limit_exceeded_reports_both_numbers() {
        let msg = Error::LimitExceeded {
            pixels: 4096,
            limit: 1024,
        }
        .to_string();
        assert!(msg.contains("4096") && msg.contains("1024"));
    }

    #[test]
    fn io_error_maps_to_its_kind() {
        use std::io::{Error as IoError, ErrorKind};
        let err: Error = IoError::from(ErrorKind::UnexpectedEof).into();
        assert_eq!(err, Error::Io(ErrorKind::UnexpectedEof));
    }
}
