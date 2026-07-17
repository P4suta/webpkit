//! The shared, hand-written, dependency-free error type for the WebP codecs.
//!
//! [`Error`] is `#[non_exhaustive]`, so new failure modes can be added without a
//! breaking change. It stays `Clone + PartialEq + Eq` (handy in tests and
//! matching) — the `std`-only [`Error::Io`] variant therefore carries an
//! [`IoError`] (a cloneable snapshot of the [`std::io::ErrorKind`] and OS message)
//! rather than the un-cloneable [`std::io::Error`] itself.

/// Which of the two WebP bitstream codecs a bitstream error refers to.
///
/// A closed set: the WebP still-image format carries exactly VP8L (lossless) or
/// VP8 (lossy), so this is deliberately *not* `#[non_exhaustive]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Codec {
    /// VP8L — the lossless codec.
    Lossless,
    /// VP8 — the lossy codec.
    Lossy,
}

impl core::fmt::Display for Codec {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            Self::Lossless => "lossless (VP8L)",
            Self::Lossy => "lossy (VP8)",
        })
    }
}

/// The retained shape of a [`std::io::Error`]: its [`ErrorKind`](std::io::ErrorKind)
/// and the original OS message.
///
/// [`Error`] stores this instead of the raw [`std::io::Error`] because the latter
/// is neither [`Clone`] nor [`Eq`], which [`Error`] must remain. The message is
/// preserved for [`Display`](core::fmt::Display) and this type is the value
/// [`Error::source`](std::error::Error::source) returns for [`Error::Io`].
#[cfg(feature = "std")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IoError {
    kind: std::io::ErrorKind,
    message: alloc::string::String,
}

#[cfg(feature = "std")]
impl IoError {
    /// The [`ErrorKind`](std::io::ErrorKind) of the original I/O error.
    #[must_use]
    pub const fn kind(&self) -> std::io::ErrorKind {
        self.kind
    }
}

#[cfg(feature = "std")]
impl core::fmt::Display for IoError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.message)
    }
}

#[cfg(feature = "std")]
impl core::error::Error for IoError {}

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
    /// A [`crop`](crate::Image::crop) rectangle does not lie fully inside the source
    /// image — distinct from an empty or over-range window, which is an
    /// [`InvalidDimensions`](Self::InvalidDimensions) error.
    CropOutOfBounds,
    /// A well-formed WebP that uses a feature this build does not decode.
    UnsupportedFeature,
    /// The extended (`VP8X`) container or one of its chunks is malformed.
    InvalidContainer,
    /// An animation frame does not fit the canvas, has an odd offset, or exceeds
    /// the `2^24 - 1` ms duration limit — distinct from a plain
    /// [`InvalidDimensions`](Self::InvalidDimensions) size error.
    InvalidFrame,
    /// The image's pixel count exceeds a caller-supplied decode limit; reported
    /// before any large allocation so a hostile header cannot exhaust memory.
    LimitExceeded {
        /// The image's `width * height`.
        pixels: u64,
        /// The configured maximum.
        limit: u64,
    },
    /// An I/O error from a [`std::io::Read`]/[`std::io::Write`] source, retaining
    /// the original [`ErrorKind`](std::io::ErrorKind) and OS message (see [`IoError`]).
    #[cfg(feature = "std")]
    Io(IoError),
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
            Self::CropOutOfBounds => {
                f.write_str("crop rectangle does not fit inside the source image")
            },
            Self::UnsupportedFeature => f.write_str("unsupported WebP feature"),
            Self::InvalidContainer => f.write_str("malformed WebP extended (VP8X) container"),
            Self::InvalidFrame => f.write_str(
                "animation frame does not fit the canvas, has an odd offset, or exceeds the \
                 2^24 ms duration limit",
            ),
            Self::LimitExceeded { pixels, limit } => {
                write!(
                    f,
                    "image has {pixels} pixels, exceeding the limit of {limit}"
                )
            },
            #[cfg(feature = "std")]
            Self::Io(e) => write!(f, "I/O error: {e}"),
        }
    }
}

impl core::error::Error for Error {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            #[cfg(feature = "std")]
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

#[cfg(feature = "std")]
impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Self::Io(IoError {
            kind: err.kind(),
            message: err.to_string(),
        })
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
            Error::CropOutOfBounds,
            Error::UnsupportedFeature,
            Error::InvalidContainer,
            Error::InvalidFrame,
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
    fn codec_display_names_both_bitstreams() {
        // Distinct, non-empty, and each names its bitstream — pins both match arms.
        assert!(Codec::Lossless.to_string().contains("VP8L"));
        assert!(Codec::Lossy.to_string().contains("VP8"));
        assert_ne!(Codec::Lossless.to_string(), Codec::Lossy.to_string());
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
    fn io_error_preserves_kind_and_message() {
        let io = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "disk on fire");
        let err: Error = io.into();
        let Error::Io(inner) = &err else {
            panic!("expected Io");
        };
        assert_eq!(inner.kind(), std::io::ErrorKind::PermissionDenied);
        // Display now carries the OS message, not just the ErrorKind debug.
        assert!(err.to_string().contains("disk on fire"));
    }

    #[test]
    fn io_error_source_chains() {
        use std::error::Error as _;
        let io = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "disk on fire");
        let err: Error = io.into();
        let source = err.source().expect("Io has a source");
        assert!(source.to_string().contains("disk on fire"));
    }

    #[test]
    fn io_error_is_clone_and_eq() {
        let make = || -> Error {
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "disk on fire").into()
        };
        let err = make();
        assert_eq!(err.clone(), err);
        assert_eq!(make(), err);
        let other: Error =
            std::io::Error::new(std::io::ErrorKind::PermissionDenied, "different").into();
        assert_ne!(other, err);
    }

    /// The `std`-gated `Io` arm needs its own Display coverage; the shared list
    /// above is `no_std`-safe and cannot name it.
    #[test]
    fn io_display_is_non_empty() {
        let err: Error = std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "eof").into();
        assert!(!err.to_string().is_empty());
    }
}
