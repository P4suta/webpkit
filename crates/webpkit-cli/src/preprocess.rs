//! Pixel preprocessing applied between decode-to-buffer and encode: crop, then
//! resize.
//!
//! These are not codec knobs. In libwebp `-crop`/`-resize` are `cwebp` tool-side
//! pixel operations run before the encoder ever sees the image, so this layer does
//! the same thing in the same place — on a decoded RGBA buffer. Crop is exact byte
//! selection; resize resamples with the `image` crate's high-quality filter.
//!
//! Two entry points. [`Pipeline::project`] maps input dimensions to output
//! dimensions with no pixels at all, so an out-of-bounds crop can be refused from a
//! header alone; [`Pipeline::apply`] runs the ops on a real [`Image`]. The two must
//! agree, and a test pins that they do.
//!
//! We are deliberately **not** byte-identical to libwebp's rescaler: libwebp uses
//! its own fixed-point resampler, and matching it bit-for-bit is a non-goal. What
//! matches is the output *dimensions*; the pixels are ours.

use webpkit::{Dimensions, Image};

use crate::error::CliError;

/// The resampling filter used for `--resize`, named in the help and docs.
///
/// Lanczos3 is the highest-quality general-purpose choice the `image` crate
/// offers. It is not libwebp's resampler — see the module docs — so the pixels
/// differ even where the dimensions match.
#[cfg(feature = "formats")]
const RESIZE_FILTER: image::imageops::FilterType = image::imageops::FilterType::Lanczos3;

/// A crop rectangle in source-pixel coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Crop {
    /// Left edge (pixels from the left).
    pub(crate) x: u32,
    /// Top edge (pixels from the top).
    pub(crate) y: u32,
    /// Width of the region.
    pub(crate) width: u32,
    /// Height of the region.
    pub(crate) height: u32,
}

impl Crop {
    /// Parse a `x,y,w,h` spec (four comma-separated non-negative integers).
    ///
    /// # Errors
    ///
    /// [`CliError::Usage`] if the spec is not exactly four integers.
    pub(crate) fn parse(spec: &str) -> Result<Self, CliError> {
        let parts = four_ints(spec).ok_or_else(|| {
            CliError::Usage(format!(
                "`--crop` takes `x,y,width,height` (four integers), got `{spec}`"
            ))
        })?;
        Ok(Self {
            x: parts[0],
            y: parts[1],
            width: parts[2],
            height: parts[3],
        })
    }

    /// The dimensions the crop yields from `input`, validating that the rectangle
    /// lies fully inside it — the projection used to refuse a bad crop early.
    fn project(self, input: Dimensions) -> Result<Dimensions, CliError> {
        if self.width == 0 || self.height == 0 {
            return Err(CliError::Usage(
                "`--crop` width and height must be non-zero".to_owned(),
            ));
        }
        // Widen to u64 so `x + width` cannot overflow before the bounds check.
        let right = u64::from(self.x) + u64::from(self.width);
        let bottom = u64::from(self.y) + u64::from(self.height);
        if right > u64::from(input.width()) || bottom > u64::from(input.height()) {
            return Err(CliError::Usage(format!(
                "crop region {}x{}+{}+{} does not fit in the {}x{} image",
                self.width,
                self.height,
                self.x,
                self.y,
                input.width(),
                input.height(),
            )));
        }
        Dimensions::new(self.width, self.height).map_err(CliError::from)
    }
}

/// A resize target. A `0` on either axis is derived from the other to preserve the
/// source aspect ratio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Resize {
    /// Target width, or `0` to derive from the height.
    pub(crate) width: u32,
    /// Target height, or `0` to derive from the width.
    pub(crate) height: u32,
}

impl Resize {
    /// Parse a resize spec: `WxH`, or `W,H` (either axis may be `0`).
    ///
    /// # Errors
    ///
    /// [`CliError::Usage`] if the spec is not two integers, or both are `0`.
    pub(crate) fn parse(spec: &str) -> Result<Self, CliError> {
        let (w, h) = spec
            .split_once(['x', 'X', ','])
            .and_then(|(a, b)| Some((parse_u32(a)?, parse_u32(b)?)))
            .ok_or_else(|| {
                CliError::Usage(format!(
                    "`--resize` takes `WxH` (use 0 on one axis to keep aspect), got `{spec}`"
                ))
            })?;
        Self::new(w, h)
    }

    /// Build from a width and height, rejecting the all-zero case.
    ///
    /// # Errors
    ///
    /// [`CliError::Usage`] if both axes are `0`.
    pub(crate) fn new(width: u32, height: u32) -> Result<Self, CliError> {
        if width == 0 && height == 0 {
            return Err(CliError::Usage(
                "`--resize` needs at least one non-zero dimension".to_owned(),
            ));
        }
        Ok(Self { width, height })
    }

    /// The output dimensions from `input`, resolving a `0` axis to keep aspect.
    ///
    /// A derived axis is `round(other * input_other / input_this)`, clamped to at
    /// least 1 — the same aspect-preserving rule libwebp applies for a `0` side.
    fn project(self, input: Dimensions) -> Result<Dimensions, CliError> {
        let (iw, ih) = (u64::from(input.width()), u64::from(input.height()));
        let width = if self.width == 0 {
            scale(iw, u64::from(self.height), ih)
        } else {
            u64::from(self.width)
        };
        let height = if self.height == 0 {
            scale(ih, u64::from(self.width), iw)
        } else {
            u64::from(self.height)
        };
        let clamp = |v: u64| u32::try_from(v.min(u64::from(webpkit::MAX_DIMENSION))).unwrap_or(1);
        Dimensions::new(clamp(width), clamp(height)).map_err(CliError::from)
    }
}

/// `round(a * b / c)` in u64, clamped to at least 1 so a derived axis never
/// collapses to zero.
fn scale(a: u64, b: u64, c: u64) -> u64 {
    if c == 0 {
        return 1;
    }
    ((a * b + c / 2) / c).max(1)
}

/// One preprocessing step. Crop always precedes resize in a [`Pipeline`].
#[derive(Debug, Clone, Copy)]
enum Stage {
    /// Select a sub-rectangle.
    Crop(Crop),
    /// Resample to new dimensions.
    Resize(Resize),
}

/// An ordered crop-then-resize preprocessing pipeline.
#[derive(Debug, Default)]
pub(crate) struct Pipeline {
    stages: Vec<Stage>,
}

impl Pipeline {
    /// Build from an optional crop and an optional resize, ordered crop-before-resize.
    #[must_use]
    pub(crate) fn new(crop: Option<Crop>, resize: Option<Resize>) -> Self {
        let mut stages = Vec::new();
        if let Some(crop) = crop {
            stages.push(Stage::Crop(crop));
        }
        if let Some(resize) = resize {
            stages.push(Stage::Resize(resize));
        }
        Self { stages }
    }

    /// Whether the pipeline is a no-op (no crop and no resize).
    #[must_use]
    pub(crate) const fn is_empty(&self) -> bool {
        self.stages.is_empty()
    }

    /// The output dimensions from an input size, without touching a single pixel.
    ///
    /// This is what lets `--crop 0,0,9999,9999 small.png` fail from the header
    /// alone, and what `--dry-run`-style reporting can print before any decode.
    ///
    /// # Errors
    ///
    /// [`CliError::Usage`] if a crop lies outside the (projected) image.
    pub(crate) fn project(&self, input: Dimensions) -> Result<Dimensions, CliError> {
        let mut dims = input;
        for stage in &self.stages {
            dims = match stage {
                Stage::Crop(crop) => crop.project(dims)?,
                Stage::Resize(resize) => resize.project(dims)?,
            };
        }
        Ok(dims)
    }

    /// Run the pipeline over a decoded image.
    ///
    /// Validates crops against the real dimensions (so `apply` and [`Self::project`]
    /// agree), then crops by exact byte selection and resizes with the `image`
    /// crate. Metadata is carried through.
    ///
    /// # Errors
    ///
    /// [`CliError::Usage`] for an out-of-bounds crop, or [`CliError::Format`] if a
    /// resize build fails (only when the `formats` feature is off, or on an
    /// impossible buffer).
    pub(crate) fn apply(&self, mut image: Image) -> Result<Image, CliError> {
        for stage in &self.stages {
            image = match stage {
                Stage::Crop(crop) => apply_crop(&image, *crop)?,
                Stage::Resize(resize) => apply_resize(&image, *resize)?,
            };
        }
        Ok(image)
    }
}

/// Crop `image` to `crop` by copying the sub-rectangle's rows, preserving the
/// image's pixel layout and metadata and recomputing `has_alpha`.
fn apply_crop(image: &Image, crop: Crop) -> Result<Image, CliError> {
    let out_dims = crop.project(image.dimensions())?;
    let (src_w, x, y, w, h) = (
        image.width() as usize,
        crop.x as usize,
        crop.y as usize,
        crop.width as usize,
        crop.height as usize,
    );
    let src = image.as_bytes();
    let row_bytes = w * 4;
    let mut out = Vec::with_capacity(row_bytes * h);
    for ry in y..y + h {
        let start = (ry * src_w + x) * 4;
        out.extend_from_slice(&src[start..start + row_bytes]);
    }
    Ok(Image::new(out_dims, image.layout(), out)?.with_metadata(image.metadata().clone()))
}

/// Resize `image` to the projected target with the `image` crate's Lanczos3 filter.
#[cfg(feature = "formats")]
fn apply_resize(image: &Image, resize: Resize) -> Result<Image, CliError> {
    let out_dims = resize.project(image.dimensions())?;
    let metadata = image.metadata().clone();
    let rgba = image::RgbaImage::try_from(image).map_err(CliError::from)?;
    let resized =
        image::imageops::resize(&rgba, out_dims.width(), out_dims.height(), RESIZE_FILTER);
    let dynamic = image::DynamicImage::ImageRgba8(resized);
    Ok(Image::try_from(&dynamic)?.with_metadata(metadata))
}

/// Without the `formats` feature there is no resampler, so `--resize` is refused
/// rather than silently ignored.
#[cfg(not(feature = "formats"))]
fn apply_resize(_image: &Image, _resize: Resize) -> Result<Image, CliError> {
    Err(CliError::Usage(
        "`--resize` needs the `formats` feature (the `image` crate's resampler), \
         which this build was compiled without"
            .to_owned(),
    ))
}

/// Parse a base-10 `u32`, trimming surrounding whitespace.
fn parse_u32(text: &str) -> Option<u32> {
    text.trim().parse().ok()
}

/// Parse exactly four comma-separated `u32`s.
fn four_ints(spec: &str) -> Option<[u32; 4]> {
    let mut it = spec.split(',');
    let a = parse_u32(it.next()?)?;
    let b = parse_u32(it.next()?)?;
    let c = parse_u32(it.next()?)?;
    let d = parse_u32(it.next()?)?;
    if it.next().is_some() {
        return None;
    }
    Some([a, b, c, d])
}

#[cfg(test)]
mod tests {
    use webpkit::{Dimensions, Image, PixelLayout};

    use super::{Crop, Pipeline, Resize};

    fn dims(w: u32, h: u32) -> Dimensions {
        Dimensions::new(w, h).unwrap()
    }

    /// A solid-color image so crop/resize outputs are easy to reason about.
    fn solid(w: u32, h: u32, px: [u8; 4]) -> Image {
        let pixels = px.repeat((w * h) as usize);
        Image::new(dims(w, h), PixelLayout::Rgba8, pixels).unwrap()
    }

    #[test]
    fn crop_projection_matches_apply() {
        let crop = Crop {
            x: 2,
            y: 1,
            width: 4,
            height: 3,
        };
        let pipeline = Pipeline::new(Some(crop), None);
        let projected = pipeline.project(dims(10, 8)).unwrap();
        let out = pipeline.apply(solid(10, 8, [1, 2, 3, 255])).unwrap();
        assert_eq!((projected.width(), projected.height()), (4, 3));
        assert_eq!((out.width(), out.height()), (4, 3));
        assert_eq!(out.as_bytes().len(), 4 * 3 * 4);
    }

    #[test]
    fn out_of_bounds_crop_is_refused_by_projection() {
        let pipeline = Pipeline::new(
            Some(Crop {
                x: 0,
                y: 0,
                width: 9999,
                height: 9999,
            }),
            None,
        );
        assert!(pipeline.project(dims(16, 16)).is_err());
    }

    #[test]
    fn resize_zero_axis_preserves_aspect() {
        // 100x50, ask for width 40, height 0 -> height derived to keep 2:1 => 20.
        let by_width = Resize::new(40, 0).unwrap();
        assert_eq!(by_width.project(dims(100, 50)).unwrap(), dims(40, 20));
        // width 0, height 20 -> width derived => 40.
        let by_height = Resize::new(0, 20).unwrap();
        assert_eq!(by_height.project(dims(100, 50)).unwrap(), dims(40, 20));
    }

    #[test]
    fn crop_precedes_resize_in_projection() {
        // Crop to 8x8 out of 16x16, then resize the crop to 4x4.
        let pipeline = Pipeline::new(
            Some(Crop {
                x: 0,
                y: 0,
                width: 8,
                height: 8,
            }),
            Some(Resize {
                width: 4,
                height: 4,
            }),
        );
        assert_eq!(pipeline.project(dims(16, 16)).unwrap(), dims(4, 4));
    }

    #[test]
    fn resize_needs_a_non_zero_dimension() {
        assert!(Resize::new(0, 0).is_err());
        assert!(Resize::parse("0x0").is_err());
    }

    #[test]
    fn crop_spec_parses_four_ints() {
        assert_eq!(
            Crop::parse("2,1,4,3").unwrap(),
            Crop {
                x: 2,
                y: 1,
                width: 4,
                height: 3
            }
        );
        assert!(Crop::parse("2,1,4").is_err());
        assert!(Crop::parse("2,1,4,3,9").is_err());
    }

    #[test]
    fn resize_spec_accepts_x_and_comma() {
        assert_eq!(
            Resize::parse("640x480").unwrap(),
            Resize {
                width: 640,
                height: 480
            }
        );
        assert_eq!(
            Resize::parse("640,480").unwrap(),
            Resize {
                width: 640,
                height: 480
            }
        );
    }
}
