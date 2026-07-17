//! BMP/TIFF encode via the `image` crate, behind the `formats` feature.
//!
//! The decoder reconstructs RGBA8; the `image` crate's BMP and TIFF encoders take
//! that directly through the `TryFrom<&Image>` conversion the `webpkit/image`
//! feature provides. ICC/Exif/XMP are dropped here — neither encoder carries
//! WebP's sidecar chunks, so PNG stays the metadata-preserving output.

use image::{DynamicImage, ImageFormat};
use webpkit::Image;

use crate::{error::CliError, format::OutputFormat};

/// Encode an [`Image`] as a BMP or TIFF file, returning the file bytes.
///
/// # Errors
///
/// [`CliError::Codec`] if the image's pixel buffer is inconsistent, or
/// [`CliError::Format`] if the `image` crate's encoder fails.
pub(crate) fn write(image: &Image, format: OutputFormat) -> Result<Vec<u8>, CliError> {
    let rgba = image::RgbaImage::try_from(image).map_err(CliError::from)?;
    let dynamic = DynamicImage::ImageRgba8(rgba);
    let mut out = std::io::Cursor::new(Vec::new());
    dynamic
        .write_to(&mut out, image_format(format)?)
        .map_err(|err| CliError::Format(format!("encoding {format:?}: {err}")))?;
    Ok(out.into_inner())
}

/// The `image` crate's format tag for a BMP/TIFF output format.
fn image_format(format: OutputFormat) -> Result<ImageFormat, CliError> {
    match format {
        OutputFormat::Bmp => Ok(ImageFormat::Bmp),
        OutputFormat::Tiff => Ok(ImageFormat::Tiff),
        // The write dispatch never routes the codec-native outputs here.
        OutputFormat::Png | OutputFormat::Ppm | OutputFormat::Pam | OutputFormat::Raw => Err(
            CliError::Format(format!("{format:?} is not an image-crate output format")),
        ),
    }
}

#[cfg(test)]
mod tests {
    use webpkit::{Dimensions, Image, PixelLayout};

    use super::write;
    use crate::format::OutputFormat;

    fn image_2x2() -> Image {
        Image::new(
            Dimensions::new(2, 2).unwrap(),
            PixelLayout::Rgba8,
            (0..16).collect(),
        )
        .unwrap()
    }

    /// BMP/TIFF output is real, not a placeholder: the `image` crate reads each
    /// encoding back to the exact RGBA pixels, alpha included.
    #[test]
    fn pixels_round_trip_through_bmp_and_tiff() {
        for format in [OutputFormat::Bmp, OutputFormat::Tiff] {
            let bytes = write(&image_2x2(), format).unwrap();
            let back = image::load_from_memory(&bytes).unwrap().to_rgba8();
            assert_eq!(back.width(), 2, "{format:?}");
            assert_eq!(back.height(), 2, "{format:?}");
            assert_eq!(
                back.into_raw(),
                (0..16).collect::<Vec<u8>>(),
                "{format:?} must round-trip pixels"
            );
        }
    }
}
