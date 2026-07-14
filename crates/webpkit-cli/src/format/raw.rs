//! Raw row-major pixel I/O (the codec's native currency).

use webpkit::lossless::{Dimensions, Image, Metadata, PixelLayout};

use crate::error::CliError;

/// Dimensions and byte order for interpreting a raw pixel buffer.
#[derive(Debug, Clone, Copy)]
pub struct RawParams {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// Byte order of each 4-byte pixel.
    pub layout: PixelLayout,
}

/// Interpret `bytes` as raw row-major pixels per `params`.
///
/// # Errors
///
/// [`CliError::RawConfig`] if the buffer length does not equal
/// `width * height * 4`, or [`CliError::Codec`] for out-of-range dimensions.
pub fn read(bytes: &[u8], params: RawParams) -> Result<Image, CliError> {
    let dims = Dimensions::new(params.width, params.height)?;
    let expected = params.width as usize * params.height as usize * 4;
    if bytes.len() != expected {
        return Err(CliError::RawConfig(format!(
            "raw input is {} bytes but {}x{} needs {expected}",
            bytes.len(),
            params.width,
            params.height,
        )));
    }
    let alpha_offset = match params.layout {
        PixelLayout::Argb8 => 0,
        PixelLayout::Rgba8 | PixelLayout::Bgra8 => 3,
    };
    let has_alpha = bytes.chunks_exact(4).any(|px| px[alpha_offset] != 0xff);
    Ok(Image::from_parts(
        dims,
        params.layout,
        bytes.to_vec(),
        has_alpha,
        Metadata::none(),
    ))
}
