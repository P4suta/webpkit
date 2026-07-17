//! JPEG/GIF/TIFF/BMP decode via the `image` crate, behind the `formats` feature.
//!
//! `image` is decode-only here (`default-features = false` plus the four format
//! decoders), and never its `webp` feature — that would pull a competing WebP
//! codec into our own tool. A decoded frame becomes a [`webpkit::Image`] through
//! the `TryFrom<&DynamicImage>` conversion the `webpkit/image` feature provides.
//! GIF loop count comes from the `gif` crate directly — `image` does not surface it.

use image::{AnimationDecoder as _, ImageFormat, codecs::gif::GifDecoder};
use webpkit::Image;

use crate::{error::CliError, format::InputFormat};

/// One composited animation frame: its pixels and its display duration.
pub(crate) struct AnimFrame {
    /// The full-canvas RGBA frame.
    pub(crate) image: Image,
    /// How long to show it, in milliseconds.
    pub(crate) duration_ms: u32,
}

/// Decode a still image (or a GIF's first frame) into a [`webpkit::Image`].
///
/// # Errors
///
/// [`CliError::Format`] if the bytes are not the format they were taken to be, or
/// [`CliError::Codec`] if the decoded dimensions exceed what WebP can represent.
pub(crate) fn read_still(bytes: &[u8], format: InputFormat) -> Result<Image, CliError> {
    let dynamic = image::load_from_memory_with_format(bytes, image_format(format)?)
        .map_err(|err| CliError::Format(format!("decoding {format:?}: {err}")))?;
    Image::try_from(&dynamic).map_err(CliError::from)
}

/// Decode every frame of a GIF, composited to the full canvas, for animation.
///
/// The `image` crate hands back full-canvas frames (disposal already applied), so
/// each frame maps to a canvas-sized `ANMF` with no offset.
///
/// # Errors
///
/// [`CliError::Format`] if the GIF is malformed, or [`CliError::Codec`] if a frame
/// exceeds WebP's dimension ceiling.
pub(crate) fn read_gif_frames(bytes: &[u8]) -> Result<Vec<AnimFrame>, CliError> {
    let decoder = GifDecoder::new(std::io::Cursor::new(bytes))
        .map_err(|err| CliError::Format(format!("reading GIF: {err}")))?;
    let frames = decoder
        .into_frames()
        .collect_frames()
        .map_err(|err| CliError::Format(format!("decoding GIF frames: {err}")))?;
    frames
        .into_iter()
        .map(|frame| {
            let (numer, denom) = frame.delay().numer_denom_ms();
            let duration_ms = numer / denom.max(1);
            let dynamic = image::DynamicImage::ImageRgba8(frame.into_buffer());
            Ok(AnimFrame {
                image: Image::try_from(&dynamic).map_err(CliError::from)?,
                duration_ms,
            })
        })
        .collect()
}

/// A GIF's Netscape loop count, in WebP `ANIM` convention: `0` loops forever,
/// `n` plays `n` times. The `image` crate decodes GIF frames but never surfaces
/// this, so it is read from the `gif` crate directly. A GIF with no loop extension
/// (the crate's `Finite(0)` default) maps to forever, as it always has.
///
/// # Errors
///
/// [`CliError::Format`] if the bytes are not a decodable GIF.
pub(crate) fn read_gif_loop_count(bytes: &[u8]) -> Result<u16, CliError> {
    let mut options = gif::DecodeOptions::new();
    // Only the Netscape loop extension is wanted, not pixels; the GIF spec places it
    // before the first frame, so decoding frame data is skipped.
    options.skip_frame_decoding(true);
    let mut reader = options
        .read_info(std::io::Cursor::new(bytes))
        .map_err(|err| CliError::Format(format!("reading GIF: {err}")))?;
    // Advance to the first frame so the loop extension preceding it is parsed.
    reader
        .next_frame_info()
        .map_err(|err| CliError::Format(format!("reading GIF: {err}")))?;
    Ok(match reader.repeat() {
        gif::Repeat::Infinite => 0,
        gif::Repeat::Finite(count) => count,
    })
}

/// The `image` crate's format tag for one of our decodable input formats.
fn image_format(format: InputFormat) -> Result<ImageFormat, CliError> {
    match format {
        InputFormat::Jpeg => Ok(ImageFormat::Jpeg),
        InputFormat::Gif => Ok(ImageFormat::Gif),
        InputFormat::Tiff => Ok(ImageFormat::Tiff),
        InputFormat::Bmp => Ok(ImageFormat::Bmp),
        // The still/animation dispatch never routes the codec-native formats here.
        InputFormat::Png | InputFormat::Ppm | InputFormat::Pam | InputFormat::Raw => Err(
            CliError::Format(format!("{format:?} is not an image-crate format")),
        ),
    }
}
