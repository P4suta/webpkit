//! Public image model: validated [`Dimensions`], the byte-order [`PixelLayout`],
//! sidecar [`Metadata`], and the owned [`Image`] / borrowed [`ImageRef`]
//! containers.
//!
//! Internally pixels are native `u32` ARGB (`0xAARRGGBB`); the conversion to and
//! from a caller's byte layout happens only here, so the rest of the codec never
//! deals with channel order.

use crate::error::{Error, Result};
use crate::prelude::*;

/// Largest image side any WebP bitstream can express, in pixels.
///
/// The VP8L header stores each side as `dimension - 1` in 14 bits, so a valid
/// dimension is `1..=MAX_DIMENSION` (`1 << 14` = 16384). The lossy `VP8` format
/// tops out slightly lower (16383); callers that need the tighter bound check it
/// themselves. This is the single source of truth for [`Dimensions`] validation.
pub const MAX_DIMENSION: u32 = 1 << 14;

/// A validated image size: both sides lie in `1..=16384`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Dimensions {
    width: u32,
    height: u32,
}

impl Dimensions {
    /// Create dimensions, validating both sides are in `1..=16384`.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidDimensions`] if either side is `0` or exceeds `16384`.
    pub fn new(width: u32, height: u32) -> Result<Self> {
        let valid = |side: u32| (1..=MAX_DIMENSION).contains(&side);
        if valid(width) && valid(height) {
            Ok(Self { width, height })
        } else {
            Err(Error::InvalidDimensions)
        }
    }

    /// The width in pixels.
    #[must_use]
    pub const fn width(self) -> u32 {
        self.width
    }

    /// The height in pixels.
    #[must_use]
    pub const fn height(self) -> u32 {
        self.height
    }

    /// The total pixel count (`width * height`), widened so it never overflows.
    #[must_use]
    pub fn pixel_count(self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }
}

/// The byte order of a pixel buffer at the API boundary.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum PixelLayout {
    /// `R, G, B, A` (the default).
    #[default]
    Rgba8,
    /// `A, R, G, B`.
    Argb8,
    /// `B, G, R, A`.
    Bgra8,
}

impl PixelLayout {
    /// Pack a native ARGB pixel (`0xAARRGGBB`) into this layout's four bytes.
    #[must_use]
    pub const fn pack(self, argb: u32) -> [u8; 4] {
        // Native `0xAARRGGBB` little-endian bytes are `[B, G, R, A]`.
        let [b, g, r, a] = argb.to_le_bytes();
        match self {
            Self::Rgba8 => [r, g, b, a],
            Self::Argb8 => [a, r, g, b],
            Self::Bgra8 => [b, g, r, a],
        }
    }

    /// Unpack this layout's four bytes into a native ARGB pixel (`0xAARRGGBB`).
    #[must_use]
    pub const fn unpack(self, px: [u8; 4]) -> u32 {
        let (r, g, b, a) = match self {
            Self::Rgba8 => (px[0], px[1], px[2], px[3]),
            Self::Argb8 => (px[1], px[2], px[3], px[0]),
            Self::Bgra8 => (px[2], px[1], px[0], px[3]),
        };
        u32::from_le_bytes([b, g, r, a])
    }

    /// Byte offset of the alpha lane within a 4-byte pixel: `Rgba8`/`Bgra8` -> 3,
    /// `Argb8` -> 0.
    #[must_use]
    pub const fn alpha_byte_offset(self) -> usize {
        match self {
            Self::Rgba8 | Self::Bgra8 => 3,
            Self::Argb8 => 0,
        }
    }
}

/// Pack native ARGB pixels into a byte buffer in `layout` order.
#[must_use]
pub fn pack_pixels(layout: PixelLayout, argb: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(argb.len() * 4);
    for &pixel in argb {
        out.extend_from_slice(&layout.pack(pixel));
    }
    out
}

/// Unpack a `layout`-ordered byte buffer into native ARGB pixels.
#[must_use]
pub fn unpack_pixels(layout: PixelLayout, bytes: &[u8]) -> Vec<u32> {
    bytes
        .chunks_exact(4)
        .map(|c| layout.unpack([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// Whether any pixel in a native-ARGB buffer is non-opaque.
#[must_use]
pub fn argb_has_alpha(argb: &[u32]) -> bool {
    argb.iter().any(|&p| p >> 24 != 0xff)
}

/// Optional sidecar metadata carried by a WebP extended (`VP8X`) container.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Metadata {
    /// ICC color profile (`ICCP` chunk).
    pub icc_profile: Option<Vec<u8>>,
    /// Exif metadata (`EXIF` chunk).
    pub exif: Option<Vec<u8>>,
    /// XMP metadata (`XMP ` chunk).
    pub xmp: Option<Vec<u8>>,
}

impl Metadata {
    /// Empty metadata (no ICC/Exif/XMP).
    #[must_use]
    pub const fn none() -> Self {
        Self {
            icc_profile: None,
            exif: None,
            xmp: None,
        }
    }

    /// Whether no metadata is present (so a bare `VP8L` file suffices).
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.icc_profile.is_none() && self.exif.is_none() && self.xmp.is_none()
    }

    /// Fold `self` (a per-field *override*), the `inherited` source metadata, and a
    /// [`MetadataPolicy`] into the effective metadata to embed. Per field: an
    /// override value in `self` wins; otherwise the inherited value is kept, gated
    /// by the policy. ICC is never gated — it is inherited (or replaced) but never
    /// dropped; only the privacy-bearing Exif/XMP sidecars are gated.
    #[must_use]
    pub fn resolve(&self, inherited: &Self, policy: MetadataPolicy) -> Self {
        let keep_private = matches!(policy, MetadataPolicy::Preserve);
        Self {
            icc_profile: self
                .icc_profile
                .clone()
                .or_else(|| inherited.icc_profile.clone()),
            exif: self.exif.clone().or_else(|| {
                if keep_private {
                    inherited.exif.clone()
                } else {
                    None
                }
            }),
            xmp: self.xmp.clone().or_else(|| {
                if keep_private {
                    inherited.xmp.clone()
                } else {
                    None
                }
            }),
        }
    }
}

/// How an encoder treats the metadata inherited from a source [`Image`].
///
/// The ICC color profile is preserved under *every* policy — a WebP we emit never
/// silently loses color-correctness — so a policy governs only the
/// privacy-bearing Exif/XMP sidecars.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[non_exhaustive]
pub enum MetadataPolicy {
    /// Preserve all inherited metadata: ICC, Exif, and XMP. The default — kinder
    /// than `cwebp`, which strips metadata by default.
    #[default]
    Preserve,
    /// Preserve the ICC color profile but strip the privacy-bearing Exif and XMP
    /// sidecars (they can embed GPS, timestamps, and device IDs).
    StripPrivate,
}

/// A decoded image: pixels in a chosen [`PixelLayout`] plus size, alpha, and
/// sidecar [`Metadata`].
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Image {
    dims: Dimensions,
    layout: PixelLayout,
    pixels: Vec<u8>,
    has_alpha: bool,
    metadata: Metadata,
}

impl Image {
    /// Assemble an image from already-validated parts (internal constructor).
    #[must_use]
    pub const fn from_parts(
        dims: Dimensions,
        layout: PixelLayout,
        pixels: Vec<u8>,
        has_alpha: bool,
        metadata: Metadata,
    ) -> Self {
        Self {
            dims,
            layout,
            pixels,
            has_alpha,
            metadata,
        }
    }

    /// Attach (or replace) the sidecar metadata, builder-style — used to surface
    /// `VP8X` container metadata recovered alongside a decoded image so a
    /// decode → re-encode round trip preserves it.
    #[must_use]
    pub fn with_metadata(mut self, metadata: Metadata) -> Self {
        self.metadata = metadata;
        self
    }

    /// The image dimensions.
    #[must_use]
    pub const fn dimensions(&self) -> Dimensions {
        self.dims
    }

    /// The width in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.dims.width()
    }

    /// The height in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.dims.height()
    }

    /// The byte order of [`Self::as_bytes`].
    #[must_use]
    pub const fn layout(&self) -> PixelLayout {
        self.layout
    }

    /// The pixel bytes in [`Self::layout`] order (`width * height * 4` bytes).
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.pixels
    }

    /// Consume the image and return its pixel bytes.
    #[must_use]
    pub fn into_pixels(self) -> Vec<u8> {
        self.pixels
    }

    /// Whether any pixel is non-opaque.
    #[must_use]
    pub const fn has_alpha(&self) -> bool {
        self.has_alpha
    }

    /// The sidecar metadata (empty if the source was a bare `VP8L` file).
    #[must_use]
    pub const fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    /// Borrow this image as an [`ImageRef`] for re-encoding.
    #[must_use]
    pub fn as_image_ref(&self) -> ImageRef<'_> {
        ImageRef {
            dims: self.dims,
            layout: self.layout,
            pixels: &self.pixels,
        }
    }

    /// Overwrite the alpha lane of every pixel from a `width * height` alpha plane
    /// (one byte per pixel), and set [`Self::has_alpha`] from the plane.
    ///
    /// # Errors
    ///
    /// [`Error::PixelBufferMismatch`] if `alpha.len() != width * height`.
    pub fn apply_alpha_plane(&mut self, alpha: &[u8]) -> Result<()> {
        if alpha.len() as u64 != self.dims.pixel_count() {
            return Err(Error::PixelBufferMismatch);
        }
        let off = self.layout.alpha_byte_offset();
        for (px, &a) in self.pixels.chunks_exact_mut(4).zip(alpha) {
            px[off] = a;
        }
        self.has_alpha = alpha.iter().any(|&a| a != 0xff);
        Ok(())
    }
}

/// A borrowed view of pixels to encode: size, layout, and a byte slice.
#[derive(Clone, Copy, Debug)]
pub struct ImageRef<'a> {
    dims: Dimensions,
    layout: PixelLayout,
    pixels: &'a [u8],
}

impl<'a> ImageRef<'a> {
    /// Borrow `pixels` (in `layout` order) as an image of `dims`.
    ///
    /// # Errors
    ///
    /// [`Error::PixelBufferMismatch`] if `pixels.len() != width * height * 4`.
    pub fn new(dims: Dimensions, layout: PixelLayout, pixels: &'a [u8]) -> Result<Self> {
        if pixels.len() as u64 != dims.pixel_count() * 4 {
            return Err(Error::PixelBufferMismatch);
        }
        Ok(Self {
            dims,
            layout,
            pixels,
        })
    }

    /// The image dimensions.
    #[must_use]
    pub const fn dimensions(self) -> Dimensions {
        self.dims
    }

    /// The byte order of [`Self::as_bytes`].
    #[must_use]
    pub const fn layout(self) -> PixelLayout {
        self.layout
    }

    /// The borrowed pixel bytes.
    #[must_use]
    pub const fn as_bytes(self) -> &'a [u8] {
        self.pixels
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::{
        Dimensions, Image, Metadata, MetadataPolicy, PixelLayout, pack_pixels, unpack_pixels,
    };

    proptest! {
        /// For every layout, `pack`/`unpack` are mutual inverses in both
        /// directions (the single channel-order boundary of the whole codec).
        #[test]
        fn pixel_layout_pack_unpack_round_trips(
            argb in any::<u32>(),
            px in any::<[u8; 4]>(),
            layout in prop_oneof![
                Just(PixelLayout::Rgba8),
                Just(PixelLayout::Argb8),
                Just(PixelLayout::Bgra8),
            ],
        ) {
            prop_assert_eq!(layout.unpack(layout.pack(argb)), argb);
            prop_assert_eq!(layout.pack(layout.unpack(px)), px);
        }
    }
    use crate::error::Error;

    #[test]
    fn dimensions_validate_range() {
        assert!(Dimensions::new(0, 4).is_err());
        assert!(Dimensions::new(4, 0).is_err());
        assert!(Dimensions::new(16385, 1).is_err());
        let d = Dimensions::new(16384, 2).unwrap();
        assert_eq!((d.width(), d.height()), (16384, 2));
        assert_eq!(d.pixel_count(), 32768);
    }

    #[test]
    fn layout_pack_unpack_round_trips() {
        // A distinctive ARGB pixel: A=0x11, R=0x22, G=0x33, B=0x44.
        let argb = 0x1122_3344u32;
        for layout in [PixelLayout::Rgba8, PixelLayout::Argb8, PixelLayout::Bgra8] {
            assert_eq!(layout.unpack(layout.pack(argb)), argb);
        }
        // Byte order is exactly as documented.
        assert_eq!(PixelLayout::Rgba8.pack(argb), [0x22, 0x33, 0x44, 0x11]);
        assert_eq!(PixelLayout::Argb8.pack(argb), [0x11, 0x22, 0x33, 0x44]);
        assert_eq!(PixelLayout::Bgra8.pack(argb), [0x44, 0x33, 0x22, 0x11]);
    }

    #[test]
    fn buffer_pack_unpack_round_trips() {
        let argb: Vec<u32> = (0..64u32).map(|v| v.wrapping_mul(0x0104_5197)).collect();
        for layout in [PixelLayout::Rgba8, PixelLayout::Argb8, PixelLayout::Bgra8] {
            let bytes = pack_pixels(layout, &argb);
            assert_eq!(bytes.len(), argb.len() * 4);
            assert_eq!(unpack_pixels(layout, &bytes), argb);
        }
    }

    #[test]
    fn image_ref_checks_buffer_length() {
        let dims = Dimensions::new(2, 2).unwrap();
        assert_eq!(
            super::ImageRef::new(dims, PixelLayout::Rgba8, &[0u8; 15]).unwrap_err(),
            Error::PixelBufferMismatch
        );
        assert!(super::ImageRef::new(dims, PixelLayout::Rgba8, &[0u8; 16]).is_ok());
    }

    #[test]
    fn argb_has_alpha_reads_the_high_byte() {
        use super::argb_has_alpha;
        // Alpha is the high byte (`>> 24`): an all-opaque buffer has none.
        assert!(!argb_has_alpha(&[0xFF00_0000, 0xFFAA_BBCC]));
        // A non-opaque pixel is detected (pins `!=`, not `==`).
        assert!(argb_has_alpha(&[0xFF00_0000, 0x0000_0000]));
        // A pixel opaque in the high byte but 0xFF in the low byte is still opaque
        // — kills `>> -> <<`, which would inspect the low byte instead.
        assert!(!argb_has_alpha(&[0xFF00_00FF]));
    }

    #[test]
    fn image_reports_dimensions_and_pixels_verbatim() {
        let dims = Dimensions::new(2, 3).unwrap();
        let pixels: Vec<u8> = (0..24).collect(); // 2 * 3 * 4
        let img = Image::from_parts(
            dims,
            PixelLayout::Rgba8,
            pixels.clone(),
            false,
            Metadata::none(),
        );
        assert_eq!(img.width(), 2);
        assert_eq!(img.height(), 3); // kills `height -> 1`
        assert_eq!(img.as_bytes(), &pixels[..]);
        assert_eq!(img.into_pixels(), pixels); // kills `into_pixels -> vec![..]`
    }

    #[test]
    fn metadata_emptiness() {
        assert!(Metadata::none().is_empty());
        let with_icc = Metadata {
            icc_profile: Some(vec![1, 2, 3]),
            ..Metadata::none()
        };
        assert!(!with_icc.is_empty());
    }

    /// A source carrying all three sidecars.
    fn all_three() -> Metadata {
        Metadata {
            icc_profile: Some(vec![10]),
            exif: Some(vec![20]),
            xmp: Some(vec![30]),
        }
    }

    /// The single source of truth for the `Metadata::resolve` fold: policy
    /// (Preserve/StripPrivate) × field (icc/exif/xmp) × override presence. The
    /// encoder configs delegate here, so their tests only check the delegation.
    #[test]
    fn resolve_truth_table() {
        // Preserve (no override): every inherited field is kept as-is.
        let inherited = all_three();
        assert_eq!(
            Metadata::none().resolve(&inherited, MetadataPolicy::Preserve),
            inherited,
        );

        // StripPrivate (no override): ICC kept, Exif/XMP dropped.
        let stripped = Metadata::none().resolve(&inherited, MetadataPolicy::StripPrivate);
        assert_eq!(stripped.icc_profile.as_deref(), Some(&[10][..]));
        assert_eq!(stripped.exif, None);
        assert_eq!(stripped.xmp, None);

        // Override wins per-slot under any policy: exif override beats inherited.
        let exif_override = Metadata {
            exif: Some(vec![2]),
            ..Metadata::none()
        };
        assert_eq!(
            exif_override
                .resolve(&inherited, MetadataPolicy::Preserve)
                .exif
                .as_deref(),
            Some(&[2][..]),
        );

        // An explicit override survives StripPrivate, but a non-overridden private
        // slot is still dropped; ICC is never gated.
        let exif99 = Metadata {
            exif: Some(vec![99]),
            ..Metadata::none()
        };
        let resolved = exif99.resolve(&inherited, MetadataPolicy::StripPrivate);
        assert_eq!(resolved.exif.as_deref(), Some(&[99][..]));
        assert_eq!(resolved.xmp, None);
        assert_eq!(resolved.icc_profile.as_deref(), Some(&[10][..]));

        // ICC: a `None` override never nulls the inherited profile; a `Some`
        // override replaces it.
        let icc_only = Metadata {
            icc_profile: Some(vec![10]),
            ..Metadata::none()
        };
        assert_eq!(
            Metadata::none()
                .resolve(&icc_only, MetadataPolicy::Preserve)
                .icc_profile
                .as_deref(),
            Some(&[10][..]),
        );
        let icc_replace = Metadata {
            icc_profile: Some(vec![77]),
            ..Metadata::none()
        };
        assert_eq!(
            icc_replace
                .resolve(&icc_only, MetadataPolicy::Preserve)
                .icc_profile
                .as_deref(),
            Some(&[77][..]),
        );

        // A present-but-empty blob (`Some(vec![])`) is a real value: the `.or_else`
        // short-circuits on any `Some`, so it wins and is not normalized to `None`,
        // under either policy — and it still upgrades to VP8X (non-empty).
        let empty_exif = Metadata {
            exif: Some(vec![]),
            ..Metadata::none()
        };
        let e_preserve = empty_exif.resolve(&Metadata::none(), MetadataPolicy::Preserve);
        assert_eq!(e_preserve.exif, Some(vec![]));
        assert!(!e_preserve.is_empty());
        assert_eq!(
            empty_exif
                .resolve(&Metadata::none(), MetadataPolicy::StripPrivate)
                .exif,
            Some(vec![]),
        );
        // A present-but-empty *inherited* value survives under Preserve.
        let inherited_empty_xmp = Metadata {
            xmp: Some(vec![]),
            ..Metadata::none()
        };
        assert_eq!(
            Metadata::none()
                .resolve(&inherited_empty_xmp, MetadataPolicy::Preserve)
                .xmp,
            Some(vec![]),
        );
    }

    #[test]
    fn image_accessors_and_borrow() {
        let dims = Dimensions::new(2, 1).unwrap();
        let img = Image::from_parts(
            dims,
            PixelLayout::Rgba8,
            vec![1, 2, 3, 255, 4, 5, 6, 0],
            true,
            Metadata::none(),
        );
        assert_eq!((img.width(), img.height()), (2, 1));
        assert!(img.has_alpha());
        assert_eq!(img.layout(), PixelLayout::Rgba8);
        let borrowed = img.as_image_ref();
        assert_eq!(borrowed.as_bytes(), img.as_bytes());
        assert_eq!(borrowed.dimensions(), dims);
    }

    /// The alpha lane lands at the layout's byte offset (`3`/`0`/`3`), the other
    /// three channels are untouched, and a mixed plane flips `has_alpha` on.
    #[test]
    fn apply_alpha_plane_writes_alpha_lane() {
        for (layout, off) in [
            (PixelLayout::Rgba8, 3usize),
            (PixelLayout::Argb8, 0usize),
            (PixelLayout::Bgra8, 3usize),
        ] {
            assert_eq!(layout.alpha_byte_offset(), off);
            let dims = Dimensions::new(2, 2).unwrap();
            // Four fully-opaque pixels with distinct non-alpha channels.
            let bases = [10u8, 14, 18, 22];
            let mut pixels = vec![0u8; 16];
            for (px, &base) in pixels.chunks_exact_mut(4).zip(bases.iter()) {
                px[0] = base;
                px[1] = base + 1;
                px[2] = base + 2;
                px[3] = base + 3;
                px[off] = 0xff; // opaque alpha lane
            }
            let original = pixels.clone();
            let mut img = Image::from_parts(dims, layout, pixels, false, Metadata::none());
            let plane = [0x00u8, 0x80, 0xff, 0x40];
            img.apply_alpha_plane(&plane).unwrap();
            for (i, (px, orig)) in img
                .as_bytes()
                .chunks_exact(4)
                .zip(original.chunks_exact(4))
                .enumerate()
            {
                assert_eq!(px[off], plane[i], "alpha lane at offset {off}");
                for (b, (&got, &want)) in px.iter().zip(orig).enumerate() {
                    if b != off {
                        assert_eq!(got, want, "channel {b} untouched");
                    }
                }
            }
            assert!(img.has_alpha(), "mixed plane flips has_alpha on");
        }
    }

    /// An all-`0xff` plane still overwrites the lane but leaves `has_alpha` off.
    #[test]
    fn apply_alpha_plane_all_opaque_keeps_flag_false() {
        for layout in [PixelLayout::Rgba8, PixelLayout::Argb8, PixelLayout::Bgra8] {
            let dims = Dimensions::new(2, 2).unwrap();
            let mut img = Image::from_parts(dims, layout, vec![0u8; 16], false, Metadata::none());
            img.apply_alpha_plane(&[0xffu8; 4]).unwrap();
            assert!(!img.has_alpha());
            let off = layout.alpha_byte_offset();
            for px in img.as_bytes().chunks_exact(4) {
                assert_eq!(px[off], 0xff);
            }
        }
    }

    /// A plane whose length is not `width * height` is rejected.
    #[test]
    fn apply_alpha_plane_length_mismatch() {
        let dims = Dimensions::new(2, 2).unwrap();
        let mut img = Image::from_parts(
            dims,
            PixelLayout::Rgba8,
            vec![0u8; 16],
            false,
            Metadata::none(),
        );
        assert_eq!(
            img.apply_alpha_plane(&[0u8; 3]).unwrap_err(),
            Error::PixelBufferMismatch
        );
        assert_eq!(
            img.apply_alpha_plane(&[0u8; 5]).unwrap_err(),
            Error::PixelBufferMismatch
        );
    }
}
