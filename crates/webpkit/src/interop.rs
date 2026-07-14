//! Interop with the [`image`] crate, behind the `image` feature.
//!
//! Fallible conversions between [`image::DynamicImage`] and the codec's [`Image`]
//! so an `image`-based pipeline can hand pixels to webpkit and take them back. The
//! conversions are [`TryFrom`] (not [`From`]) because webpkit validates each side
//! to at most [`MAX_DIMENSION`](crate::MAX_DIMENSION), which an arbitrary `image`
//! buffer may exceed.
//!
//! The dependency is optional: a default build pulls in no `image` crate and this
//! module does not exist — the zero-dependency baseline is unchanged.

use crate::error::{Error, Result};
use crate::image::{Dimensions, Image, Metadata, PixelLayout, pack_pixels, unpack_pixels};

/// Convert an [`image::DynamicImage`] into a webpkit [`Image`] (RGBA8).
///
/// The source is materialized as RGBA8 and `has_alpha` is set from a scan of the
/// resulting alpha lane. Sidecar metadata (ICC/Exif/XMP) is not carried — a
/// `DynamicImage` does not model it — so attach any with [`Image::with_metadata`].
///
/// # Errors
///
/// [`Error::InvalidDimensions`] if either side is `0` or exceeds
/// [`MAX_DIMENSION`](crate::MAX_DIMENSION).
///
/// # Examples
///
/// ```
/// let dynamic = image::DynamicImage::new_rgba8(4, 4);
/// let img = webpkit::Image::try_from(&dynamic)?;
/// let webp = webpkit::Encoder::lossless().encode(&img)?;
/// assert_eq!(webpkit::decode(&webp)?.dimensions(), img.dimensions());
/// # Ok::<(), webpkit::Error>(())
/// ```
impl TryFrom<&image::DynamicImage> for Image {
    type Error = Error;

    fn try_from(src: &image::DynamicImage) -> Result<Self> {
        let dims = Dimensions::new(src.width(), src.height())?;
        let rgba = src.to_rgba8().into_raw();
        let has_alpha = rgba.chunks_exact(4).any(|px| px[3] != 0xff);
        Ok(Self::from_parts(
            dims,
            PixelLayout::Rgba8,
            rgba,
            has_alpha,
            Metadata::none(),
        ))
    }
}

/// Convert a webpkit [`Image`] into an [`image::RgbaImage`], repacking to RGBA8
/// from whatever [`PixelLayout`] the image stores.
///
/// # Errors
///
/// [`Error::PixelBufferMismatch`] only if the image's internal
/// `pixels.len() == width * height * 4` invariant is violated — unreachable for an
/// `Image` built through the public API, but surfaced rather than panicked.
impl TryFrom<&Image> for image::RgbaImage {
    type Error = Error;

    fn try_from(src: &Image) -> Result<Self> {
        let (w, h) = (src.width(), src.height());
        let rgba = if src.layout() == PixelLayout::Rgba8 {
            src.as_bytes().to_vec()
        } else {
            pack_pixels(
                PixelLayout::Rgba8,
                &unpack_pixels(src.layout(), src.as_bytes()),
            )
        };
        Self::from_raw(w, h, rgba).ok_or(Error::PixelBufferMismatch)
    }
}

#[cfg(test)]
mod tests {
    use crate::image::{Dimensions, Image, Metadata, PixelLayout};

    #[test]
    fn dynamic_image_round_trips_through_webpkit_image() {
        // A gradient RGBA `DynamicImage` -> webpkit `Image` -> `RgbaImage` must
        // preserve dimensions and every byte.
        let mut src = image::RgbaImage::new(3, 2);
        for (i, px) in src.pixels_mut().enumerate() {
            let v = u8::try_from(i * 10 % 256).unwrap();
            *px = image::Rgba([v, v.wrapping_add(1), v.wrapping_add(2), 0x80]);
        }
        let dynamic = image::DynamicImage::ImageRgba8(src.clone());

        let img = Image::try_from(&dynamic).unwrap();
        assert_eq!((img.width(), img.height()), (3, 2));
        assert!(img.has_alpha(), "alpha 0x80 must be detected as non-opaque");

        let back = image::RgbaImage::try_from(&img).unwrap();
        assert_eq!(back.dimensions(), (3, 2));
        assert_eq!(back.into_raw(), src.into_raw());
    }

    #[test]
    fn rejects_dimensions_past_the_webp_maximum() {
        // `image` permits sizes webpkit does not; the conversion must reject a side
        // over `MAX_DIMENSION` with `InvalidDimensions` rather than build an image a
        // codec cannot represent. (16385 x 1 is one past the limit.)
        let oversized = image::DynamicImage::new_rgba8(crate::MAX_DIMENSION + 1, 1);
        assert!(matches!(
            Image::try_from(&oversized),
            Err(crate::Error::InvalidDimensions)
        ));
    }

    #[test]
    fn non_rgba_layout_is_repacked_to_rgba_on_export() {
        // An `Image` stored in a non-RGBA layout must still export correct RGBA byte
        // order (repack path), not leak its internal byte order.
        let dims = Dimensions::new(1, 1).unwrap();
        // BGRA bytes for opaque pure red (R=255): stored as [B,G,R,A] = [0,0,255,255].
        let img = Image::from_parts(
            dims,
            PixelLayout::Bgra8,
            vec![0, 0, 255, 255],
            false,
            Metadata::none(),
        );
        let rgba = image::RgbaImage::try_from(&img).unwrap();
        assert_eq!(rgba.into_raw(), vec![255, 0, 0, 255], "must emit RGBA red");
    }
}
