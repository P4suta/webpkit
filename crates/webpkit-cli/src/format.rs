//! Image-file encodings the CLI reads and writes: PNG, netpbm (PPM/PAM), raw,
//! and — behind the `formats` feature — JPEG/GIF/TIFF/BMP via the `image` crate.
//!
//! The codec itself only speaks raw RGBA/ARGB/BGRA, so this layer is what makes
//! the tools accept `.png` inputs and emit `.png` outputs. PNG keeps its own
//! decoder for metadata fidelity the `image` crate cannot give; the four extra
//! input formats route through `image_input`.

#[cfg(feature = "formats")]
pub(crate) mod image_input;
pub(crate) mod png;
pub(crate) mod ppm;
pub(crate) mod raw;

use clap::ValueEnum;
use webpkit::{Image, Metadata, PixelLayout};

use crate::{error::CliError, format::raw::RawParams};

/// An image encoding the CLI can read as encoder input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum InputFormat {
    /// PNG (any color type; normalized to RGBA8).
    Png,
    /// Netpbm binary PPM (`P6`, RGB).
    Ppm,
    /// Netpbm binary PAM (`P7`, RGBA).
    Pam,
    /// JPEG (decoded to RGBA8; needs the `formats` feature).
    Jpeg,
    /// GIF (first frame as a still; whole-file animation is a separate path).
    Gif,
    /// TIFF (decoded to RGBA8; needs the `formats` feature).
    Tiff,
    /// BMP (decoded to RGBA8; needs the `formats` feature).
    Bmp,
    /// Raw row-major pixels; requires `--width`/`--height`/`--layout`.
    Raw,
}

/// An image encoding the CLI can write as decoder output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum OutputFormat {
    /// PNG, RGBA8.
    Png,
    /// Netpbm binary PPM (`P6`, RGB; alpha dropped).
    Ppm,
    /// Netpbm binary PAM (`P7`, RGBA).
    Pam,
    /// Raw row-major pixels in the requested `--layout`.
    Raw,
}

impl InputFormat {
    /// Resolve the input format: an explicit choice wins, else the file
    /// extension, else the leading magic bytes, else [`InputFormat::Raw`].
    #[must_use]
    pub(crate) fn resolve(explicit: Option<Self>, extension: Option<&str>, bytes: &[u8]) -> Self {
        explicit
            .or_else(|| extension.and_then(Self::from_extension))
            .or_else(|| Self::sniff(bytes))
            .unwrap_or(Self::Raw)
    }

    fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            "png" => Some(Self::Png),
            "ppm" => Some(Self::Ppm),
            "pam" => Some(Self::Pam),
            "jpg" | "jpeg" => Some(Self::Jpeg),
            "gif" => Some(Self::Gif),
            "tif" | "tiff" => Some(Self::Tiff),
            "bmp" => Some(Self::Bmp),
            "raw" | "rgba" | "argb" | "bgra" => Some(Self::Raw),
            _ => None,
        }
    }

    /// Whether these bytes are a GIF, by magic (`GIF87a`/`GIF89a`). The GIF sniff
    /// stands alone because the `webp` tool routes a GIF to the animation encoder,
    /// not the still path — direction is decided before a format is even resolved.
    #[must_use]
    pub(crate) fn is_gif(bytes: &[u8]) -> bool {
        bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")
    }

    fn sniff(bytes: &[u8]) -> Option<Self> {
        if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(Self::Png)
        } else if bytes.starts_with(b"P6") {
            Some(Self::Ppm)
        } else if bytes.starts_with(b"P7") {
            Some(Self::Pam)
        } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
            Some(Self::Jpeg)
        } else if Self::is_gif(bytes) {
            Some(Self::Gif)
        } else if bytes.starts_with(b"II*\x00") || bytes.starts_with(b"MM\x00*") {
            Some(Self::Tiff)
        } else if bytes.starts_with(b"BM") {
            Some(Self::Bmp)
        } else {
            None
        }
    }
}

impl OutputFormat {
    /// Resolve the output format: an explicit choice wins, else the `-o`
    /// extension, else [`OutputFormat::Png`] (the dwebp default).
    #[must_use]
    pub(crate) fn resolve(explicit: Option<Self>, extension: Option<&str>) -> Self {
        explicit
            .or_else(|| extension.and_then(Self::from_extension))
            .unwrap_or(Self::Png)
    }

    fn from_extension(ext: &str) -> Option<Self> {
        match ext {
            "png" => Some(Self::Png),
            "ppm" => Some(Self::Ppm),
            "pam" => Some(Self::Pam),
            "raw" | "rgba" | "argb" | "bgra" => Some(Self::Raw),
            _ => None,
        }
    }
}

/// Decode `bytes` in the given `format` into an [`Image`].
///
/// `raw` supplies the dimensions/layout required by [`InputFormat::Raw`].
///
/// # Errors
///
/// [`CliError::Format`] if a PNG/netpbm stream is malformed, or
/// [`CliError::RawConfig`] if raw parameters are missing or inconsistent.
pub(crate) fn read_image(
    bytes: &[u8],
    format: InputFormat,
    raw: Option<RawParams>,
) -> Result<Image, CliError> {
    match format {
        InputFormat::Png => png::read(bytes),
        InputFormat::Ppm | InputFormat::Pam => ppm::read(bytes),
        InputFormat::Jpeg | InputFormat::Gif | InputFormat::Tiff | InputFormat::Bmp => {
            read_via_image(bytes, format)
        },
        InputFormat::Raw => {
            let params = raw.ok_or_else(|| {
                CliError::RawConfig(
                    "raw input requires --width and --height (or use a PNG/PPM/PAM file)"
                        .to_owned(),
                )
            })?;
            raw::read(bytes, params)
        },
    }
}

/// Decode a JPEG/GIF/TIFF/BMP still via the `image` crate. A GIF yields its first
/// frame here; the animation path (`webp` → animated WebP) is separate.
#[cfg(feature = "formats")]
fn read_via_image(bytes: &[u8], format: InputFormat) -> Result<Image, CliError> {
    image_input::read_still(bytes, format)
}

/// Without the `formats` feature the four `image`-crate formats are recognized but
/// not decodable, so the error names the missing capability rather than misparsing.
#[cfg(not(feature = "formats"))]
fn read_via_image(_bytes: &[u8], format: InputFormat) -> Result<Image, CliError> {
    Err(CliError::Format(format!(
        "{format:?} input needs the `formats` feature, which this build was compiled without"
    )))
}

/// Encode an [`Image`] into the given output `format`, returning file bytes.
///
/// `metadata` is embedded only by formats that support it (PNG); netpbm and raw
/// ignore it.
///
/// # Errors
///
/// [`CliError::Format`] if PNG encoding fails.
pub(crate) fn write_image(
    image: &Image,
    format: OutputFormat,
    metadata: &Metadata,
) -> Result<Vec<u8>, CliError> {
    match format {
        OutputFormat::Png => png::write(image, metadata),
        OutputFormat::Ppm => Ok(ppm::write_ppm(image)),
        OutputFormat::Pam => Ok(ppm::write_pam(image)),
        OutputFormat::Raw => Ok(image.as_bytes().to_vec()),
    }
}

/// Return an image's pixels as RGBA8, reordering from its stored layout.
#[must_use]
pub(crate) fn to_rgba8(image: &Image) -> Vec<u8> {
    let src = image.as_bytes();
    match image.layout() {
        PixelLayout::Rgba8 => src.to_vec(),
        PixelLayout::Argb8 => reorder(src, [1, 2, 3, 0]),
        PixelLayout::Bgra8 => reorder(src, [2, 1, 0, 3]),
    }
}

/// Reorder 4-byte pixels by picking source indices `order` into RGBA slots.
fn reorder(src: &[u8], order: [usize; 4]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len());
    for px in src.chunks_exact(4) {
        out.extend_from_slice(&[px[order[0]], px[order[1]], px[order[2]], px[order[3]]]);
    }
    out
}
