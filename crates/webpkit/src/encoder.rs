//! The unified, type-state [`Encoder`] — the single public encode surface.
//!
//! [`Encoder::lossless`] and [`Encoder::lossy`] return builders that share the
//! effort/metadata knobs but differ in one compile-time way: `quality` exists only
//! on the lossy builder, so `Encoder::lossless().quality(90)` does not compile
//! (quality is meaningless for a bit-exact codec). Each terminal folds into the
//! codec's internal config and calls its encoder.

#[cfg(feature = "alloc")]
use alloc::vec::Vec;
use core::marker::PhantomData;

use crate::lossless::EncoderConfig;
use crate::lossy::{LossyConfig, Quality};
use crate::{Effort, Image, ImageRef, Metadata, MetadataPolicy, Result};

/// Seals the codec marker types so no downstream type can inhabit [`Encoder`].
mod sealed {
    /// Implemented only by the [`super::Lossless`] / [`super::Lossy`] markers.
    pub trait Codec {}
}

/// Type-state marker: a lossless (`VP8L`) [`Encoder`].
#[derive(Debug, Clone, Copy)]
pub struct Lossless(());
/// Type-state marker: a lossy (`VP8 `) [`Encoder`].
#[derive(Debug, Clone, Copy)]
pub struct Lossy(());

impl sealed::Codec for Lossless {}
impl sealed::Codec for Lossy {}

/// A builder that encodes an image into a complete WebP file with one codec.
///
/// Construct it with [`Encoder::lossless`] or [`Encoder::lossy`]; set the shared
/// [`effort`](Encoder::effort) / [`metadata`](Encoder::metadata) /
/// [`metadata_policy`](Encoder::metadata_policy) knobs (and, for lossy only,
/// [`quality`](Encoder::quality)); then call [`encode`](Encoder::encode) (metadata
/// preserved) or [`encode_ref`](Encoder::encode_ref) (bare `ImageRef`).
#[derive(Debug, Clone)]
pub struct Encoder<C> {
    effort: Effort,
    metadata: Metadata,
    policy: MetadataPolicy,
    /// Only consulted by the lossy terminal; the type-state hides its setter on the
    /// lossless builder.
    quality: Quality,
    _codec: PhantomData<C>,
}

impl<C: sealed::Codec> Encoder<C> {
    /// The shared default: [`Effort::Balanced`], no metadata override, the default
    /// [`MetadataPolicy`], and (for lossy) [`Quality`]'s default.
    fn new() -> Self {
        Self {
            effort: Effort::default(),
            metadata: Metadata::none(),
            policy: MetadataPolicy::default(),
            quality: Quality::default(),
            _codec: PhantomData,
        }
    }

    /// Set the effort tier (both codecs share [`Effort`]).
    #[must_use]
    pub const fn effort(mut self, effort: Effort) -> Self {
        self.effort = effort;
        self
    }

    /// Set an explicit ICC/Exif/XMP [`Metadata`] override.
    ///
    /// Under [`encode`](Self::encode) this wins per-field over the image's own
    /// metadata and survives even [`MetadataPolicy::StripPrivate`].
    #[must_use]
    pub fn metadata(mut self, metadata: Metadata) -> Self {
        self.metadata = metadata;
        self
    }

    /// Set the [`MetadataPolicy`] that gates the *inherited* image metadata under
    /// [`encode`](Self::encode) (ICC is always kept; `StripPrivate` drops Exif/XMP).
    #[must_use]
    pub const fn metadata_policy(mut self, policy: MetadataPolicy) -> Self {
        self.policy = policy;
        self
    }
}

impl Encoder<Lossless> {
    /// Start a lossless (`VP8L`) encoder at [`Effort::Balanced`].
    #[must_use]
    pub fn lossless() -> Self {
        Self::new()
    }

    /// Encode `image` into a complete lossless WebP file, **preserving its
    /// ICC/Exif/XMP metadata by default** (kinder than `cwebp`, which strips it).
    ///
    /// The effective metadata is resolved per field: an explicit
    /// [`metadata`](Self::metadata) override wins, else the image's own metadata
    /// gated by [`metadata_policy`](Self::metadata_policy).
    ///
    /// # Errors
    ///
    /// Propagates [`crate::lossless::encode_image`].
    #[cfg(feature = "alloc")]
    pub fn encode(&self, image: &Image) -> Result<Vec<u8>> {
        crate::lossless::encode_image(image, &self.config())
    }

    /// Encode a bare [`ImageRef`] into a complete lossless WebP file, embedding
    /// only this encoder's own [`metadata`](Self::metadata) override (there is no
    /// source image to inherit from).
    ///
    /// # Errors
    ///
    /// Propagates [`crate::lossless::encode`].
    #[cfg(feature = "alloc")]
    pub fn encode_ref(&self, image: ImageRef<'_>) -> Result<Vec<u8>> {
        crate::lossless::encode(image, &self.config())
    }

    /// Encode `image` and write the bytes to `writer` (metadata preserved).
    ///
    /// # Errors
    ///
    /// [`Error::Io`](crate::Error::Io) on a write failure, or any
    /// [`encode`](Self::encode) error.
    #[cfg(feature = "std")]
    pub fn encode_to<W: std::io::Write>(&self, image: &Image, mut writer: W) -> Result<()> {
        writer.write_all(&self.encode(image)?)?;
        Ok(())
    }

    /// The internal lossless config this builder folds into.
    fn config(&self) -> EncoderConfig {
        EncoderConfig::new()
            .with_effort(self.effort)
            .with_metadata(self.metadata.clone())
            .with_metadata_policy(self.policy)
    }
}

impl Encoder<Lossy> {
    /// Start a lossy (`VP8 `) encoder at [`Effort::Balanced`] and the default quality.
    #[must_use]
    pub fn lossy() -> Self {
        Self::new()
    }

    /// Set the encode quality (`0..=100`, clamped). **Lossy only** — this method
    /// does not exist on the lossless builder, so a quality on a lossless encode is
    /// a compile error.
    ///
    /// ```compile_fail
    /// // quality() is not available on a lossless encoder:
    /// let _ = webpkit::Encoder::lossless().quality(90);
    /// ```
    #[must_use]
    pub const fn quality(mut self, quality: u8) -> Self {
        self.quality = Quality::new(quality);
        self
    }

    /// Encode `image` into a complete lossy WebP file, **preserving its
    /// ICC/Exif/XMP metadata by default**. Non-opaque images carry a lossless
    /// `ALPH` alpha plane. See [`Encoder::<Lossless>::encode`] for the metadata
    /// resolution rules.
    ///
    /// # Errors
    ///
    /// Propagates [`crate::lossy::encode_image`].
    #[cfg(feature = "alloc")]
    pub fn encode(&self, image: &Image) -> Result<Vec<u8>> {
        crate::lossy::encode_image(image, &self.config())
    }

    /// Encode a bare [`ImageRef`] into a complete lossy WebP file, embedding only
    /// this encoder's own [`metadata`](Self::metadata) override.
    ///
    /// # Errors
    ///
    /// Propagates [`crate::lossy::encode`].
    #[cfg(feature = "alloc")]
    pub fn encode_ref(&self, image: ImageRef<'_>) -> Result<Vec<u8>> {
        crate::lossy::encode(image, &self.config())
    }

    /// Encode `image` and write the bytes to `writer` (metadata preserved).
    ///
    /// # Errors
    ///
    /// [`Error::Io`](crate::Error::Io) on a write failure, or any
    /// [`encode`](Self::encode) error.
    #[cfg(feature = "std")]
    pub fn encode_to<W: std::io::Write>(&self, image: &Image, mut writer: W) -> Result<()> {
        writer.write_all(&self.encode(image)?)?;
        Ok(())
    }

    /// The internal lossy config this builder folds into.
    fn config(&self) -> LossyConfig {
        LossyConfig::new()
            .with_quality(self.quality.get())
            .with_effort(self.effort)
            .with_metadata(self.metadata.clone())
            .with_metadata_policy(self.policy)
    }
}
