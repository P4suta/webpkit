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

use crate::container::anim::{AnimChunk, AnmfFlags, AnmfHeader};
use crate::container::fourcc::FourCc;
use crate::container::vp8x::{Vp8xFlags, Vp8xInfo};
use crate::container::writer::{push_chunk, riff_envelope};
use crate::image;
use crate::lossless::EncoderConfig;
use crate::lossless::encoder::encode_payload;
use crate::lossy::{LossyConfig, LossyParams, LossyTuning, Quality};
use crate::{
    BlendMode, Dimensions, DisposalMode, Effort, Error, FrameMeta, Image, ImageRef, Metadata,
    MetadataPolicy, PixelLayout, Result,
};

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
    /// Only consulted by the lossless terminal; the type-state hides its setter on
    /// the lossy builder (near-lossless is a VP8L-only preprocessing step).
    near_lossless: Option<u8>,
    /// Only consulted by the lossy terminal; the type-state hides its setter on the
    /// lossless builder (the psychovisual knobs are VP8-only).
    tuning: LossyTuning,
    _codec: PhantomData<C>,
}

impl<C: sealed::Codec> Encoder<C> {
    /// The shared default: [`Effort::AUTO`], no metadata override, the default
    /// [`MetadataPolicy`], and (for lossy) [`Quality`]'s default.
    fn new() -> Self {
        Self {
            effort: Effort::default(),
            metadata: Metadata::none(),
            policy: MetadataPolicy::default(),
            quality: Quality::default(),
            near_lossless: None,
            tuning: LossyTuning::new(),
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
    /// Start a lossless (`VP8L`) encoder at [`Effort::AUTO`].
    #[must_use]
    pub fn lossless() -> Self {
        Self::new()
    }

    /// Enable near-lossless preprocessing at `level` (`0..=100`, lower = stronger
    /// quantization; `100` is a no-op). **Lossless only** — this method does not
    /// exist on the lossy builder, so a near-lossless request on a lossy encode is a
    /// compile error.
    ///
    /// A lossy encode-side filter that snaps the low bits of pixels in busy regions
    /// to a coarser grid, trading a bounded per-channel error for a smaller VP8L
    /// payload; the bitstream stays exact, so the file decodes with no special
    /// support.
    ///
    /// ```compile_fail
    /// // near_lossless() is not available on a lossy encoder:
    /// let _ = webpkit::Encoder::lossy().near_lossless(60);
    /// ```
    #[must_use]
    pub const fn near_lossless(mut self, level: u8) -> Self {
        self.near_lossless = Some(level);
        self
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
    /// Any error from the lossless encode.
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
    /// Any error from the lossless encode.
    #[cfg(feature = "alloc")]
    pub fn encode_ref(&self, image: ImageRef<'_>) -> Result<Vec<u8>> {
        crate::lossless::encode(image, &self.config())
    }

    /// Encode `image` and write the bytes to `writer` (metadata preserved).
    ///
    /// # Errors
    ///
    /// [`Error::Io`] on a write failure, or any
    /// [`encode`](Self::encode) error.
    #[cfg(feature = "std")]
    pub fn encode_to<W: std::io::Write>(&self, image: &Image, mut writer: W) -> Result<()> {
        writer.write_all(&self.encode(image)?)?;
        Ok(())
    }

    /// The internal lossless config this builder folds into.
    fn config(&self) -> EncoderConfig {
        let config = EncoderConfig::new()
            .with_effort(self.effort)
            .with_metadata(self.metadata.clone())
            .with_metadata_policy(self.policy);
        match self.near_lossless {
            Some(level) => config.with_near_lossless(level),
            None => config,
        }
    }
}

impl Encoder<Lossy> {
    /// Start a lossy (`VP8 `) encoder at [`Effort::AUTO`] and the default quality.
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

    /// Set the psychovisual [`LossyTuning`] knobs (SNS strength, segment count, filter
    /// strength/sharpness). **Lossy only** — this method does not exist on the
    /// lossless builder. Defaults to [`LossyTuning::default`], the near-best
    /// `cwebp`-parity baseline.
    ///
    /// ```compile_fail
    /// // tuning() is not available on a lossless encoder:
    /// let _ = webpkit::Encoder::lossless().tuning(webpkit::LossyTuning::new());
    /// ```
    #[must_use]
    pub const fn tuning(mut self, tuning: LossyTuning) -> Self {
        self.tuning = tuning;
        self
    }

    /// Set both the quality and the [`LossyTuning`] from a single validated
    /// [`LossyParams`] — the same surface [`AnimCodec::Lossy`] carries. Equivalent to
    /// calling [`quality`](Self::quality) and [`tuning`](Self::tuning) together, so one
    /// validation story flows from the params to the encode.
    #[must_use]
    pub const fn params(mut self, params: LossyParams) -> Self {
        self.quality = params.quality();
        self.tuning = params.tuning();
        self
    }

    /// Encode `image` into a complete lossy WebP file, **preserving its
    /// ICC/Exif/XMP metadata by default**. Non-opaque images carry a lossless
    /// `ALPH` alpha plane. See [`Encoder::<Lossless>::encode`] for the metadata
    /// resolution rules.
    ///
    /// # Errors
    ///
    /// Any error from the lossy encode.
    #[cfg(feature = "alloc")]
    pub fn encode(&self, image: &Image) -> Result<Vec<u8>> {
        crate::lossy::encode_image(image, &self.config())
    }

    /// Encode a bare [`ImageRef`] into a complete lossy WebP file, embedding only
    /// this encoder's own [`metadata`](Self::metadata) override.
    ///
    /// # Errors
    ///
    /// Any error from the lossy encode.
    #[cfg(feature = "alloc")]
    pub fn encode_ref(&self, image: ImageRef<'_>) -> Result<Vec<u8>> {
        crate::lossy::encode(image, &self.config())
    }

    /// Encode `image` and write the bytes to `writer` (metadata preserved).
    ///
    /// # Errors
    ///
    /// [`Error::Io`] on a write failure, or any
    /// [`encode`](Self::encode) error.
    #[cfg(feature = "std")]
    pub fn encode_to<W: std::io::Write>(&self, image: &Image, mut writer: W) -> Result<()> {
        writer.write_all(&self.encode(image)?)?;
        Ok(())
    }

    /// Search encode quality to meet a byte / PSNR [`RateTarget`](crate::RateTarget), returning the best
    /// encode and how the search reached it ([`RateSearch`](crate::RateSearch)).
    ///
    /// Rate control is the inverse of a plain encode — "which quality fits this
    /// budget / clears this floor?" — so it is a deterministic integer bisection over
    /// `0..=100` at this builder's fixed [`effort`](Self::effort) and
    /// [`tuning`](Self::tuning) (the builder's own `quality` is ignored, since quality
    /// is the axis being searched). A [`tuning`](Self::tuning) with a higher
    /// [`pass`](LossyTuning::pass) sharpens each probe's size, so a multi-pass encode
    /// converges the search too. Metadata is preserved exactly as in
    /// [`encode`](Self::encode).
    ///
    /// # Errors
    ///
    /// Any error from the underlying encode or decode, or [`Error::InvalidDimensions`]
    /// when `target` sets no bound.
    #[cfg(feature = "alloc")]
    pub fn rate_control(
        &self,
        image: &Image,
        target: crate::RateTarget,
    ) -> Result<crate::RateSearch> {
        crate::lossy::rate::search(image, &self.config(), target)
    }

    /// Search quality for the largest quality whose encode fits within `max_bytes`
    /// (convenience over [`rate_control`](Self::rate_control) with a
    /// [`RateTarget::size`](crate::RateTarget::size)).
    ///
    /// # Errors
    ///
    /// As [`rate_control`](Self::rate_control).
    #[cfg(feature = "alloc")]
    pub fn target_size(&self, image: &Image, max_bytes: usize) -> Result<crate::RateSearch> {
        self.rate_control(image, crate::RateTarget::size(max_bytes))
    }

    /// Search quality for the smallest quality whose reconstruction PSNR meets
    /// `min_psnr_centidb` (dB × 100 — a fixed-point centidecibel floor, so `42.5 dB`
    /// is `4250`). Convenience over [`rate_control`](Self::rate_control) with a
    /// [`RateTarget::psnr`](crate::RateTarget::psnr).
    ///
    /// # Errors
    ///
    /// As [`rate_control`](Self::rate_control).
    #[cfg(feature = "alloc")]
    pub fn target_psnr(&self, image: &Image, min_psnr_centidb: u32) -> Result<crate::RateSearch> {
        self.rate_control(image, crate::RateTarget::psnr(min_psnr_centidb))
    }

    /// The internal lossy config this builder folds into.
    fn config(&self) -> LossyConfig {
        LossyConfig::new()
            .with_quality(self.quality.get())
            .with_effort(self.effort)
            .with_metadata(self.metadata.clone())
            .with_metadata_policy(self.policy)
            .with_tuning(self.tuning)
    }
}

/// Which codec encodes an animation frame. WebP `ANMF` frames may mix codecs, so
/// this is both the encoder-level default and a per-frame override
/// ([`AnimationEncoder::add_frame_with`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
#[non_exhaustive]
pub enum AnimCodec {
    /// Lossless `VP8L` frame (default), carrying its own alpha.
    #[default]
    Lossless,
    /// Lossy `VP8 ` key-frame at the given [`LossyParams`] (quality + tuning); a
    /// non-opaque frame also gets a sibling `ALPH` alpha chunk (lossless unless the
    /// params lower `alpha_q`).
    Lossy {
        /// The validated lossy quality and psychovisual/RD tuning for this frame.
        params: LossyParams,
    },
}

/// Encode one animation frame's sub-chunk bytes — the bare chunk sequence that
/// follows the 16-byte `ANMF` header (an optional `ALPH`, then the `VP8L`/`VP8 `
/// image chunk) — for a `dims`-sized native-ARGB frame under `codec`.
///
/// The single home of a frame's payload layout, shared by
/// [`AnimationEncoder::add_frame_with`] and the animation optimizer
/// ([`AnimationOptimizer`](crate::AnimationOptimizer)), which measures both codecs'
/// bytes here to keep the smaller. Sub-chunk order follows the still extended order
/// (`ALPH` before the image chunk): our decoder is order-independent, but libwebp's
/// demux expects `ALPH` first.
pub(crate) fn encode_frame_payload(
    effort: Effort,
    dims: Dimensions,
    argb: &[u32],
    codec: AnimCodec,
) -> Vec<u8> {
    let mut body = Vec::new();
    match codec {
        AnimCodec::Lossless => {
            let payload = encode_payload(effort, dims.width(), dims.height(), argb);
            push_chunk(&mut body, FourCc::VP8L, &payload);
        },
        AnimCodec::Lossy { params } => {
            let tuning = params.tuning();
            let cfg = LossyConfig::new()
                .with_quality(params.quality().get())
                .with_effort(effort)
                .with_tuning(tuning);
            let vp8 = crate::lossy::encoder::encode_vp8_argb(argb, dims, &cfg);
            // The frame's ALPH search follows the params' alpha knobs; the default
            // params keep the always-lossless, exhaustive search (byte-identical).
            let alpha_tuning = crate::lossy::alpha::AlphaTuning {
                quality: tuning.alpha_q(),
                method: tuning.alpha_method(),
                filter: tuning.alpha_filter(),
            };
            if let Some(alph) = crate::lossy::alpha::alph_chunk(argb, dims, alpha_tuning) {
                push_chunk(&mut body, FourCc::ALPH, &alph);
            }
            push_chunk(&mut body, FourCc::VP8, &vp8);
        },
    }
    body
}

/// Type-state marker: an [`AnimationEncoder`] with no frames yet. `finish` is not
/// available in this state, so an empty animation cannot be built.
pub enum Empty {}

/// Type-state marker: an [`AnimationEncoder`] with at least one frame. `finish`
/// is available only in this state.
pub enum HasFrames {}

/// A type-state builder for animated (`ANIM`/`ANMF`) WebP files.
///
/// [`finish`](AnimationEncoder::finish) is only callable once at least one frame
/// has been added, so an empty animation is a compile error. Frames are buffered
/// and the whole file is produced by `finish`, so no [`std::io::Seek`] is needed.
/// Each frame is encoded eagerly with the encoder's current [`AnimCodec`] — the
/// default is lossless `VP8L`; [`codec`](AnimationEncoder::codec) switches to lossy
/// `VP8 ` (non-opaque frames carry a lossless `ALPH` chunk) and
/// [`add_frame_with`](AnimationEncoder::add_frame_with) overrides it per frame.
/// ICC/Exif/XMP [`Metadata`] set via
/// [`metadata`](AnimationEncoder::metadata) is embedded by `finish`.
///
/// ```
/// use webpkit::{AnimationEncoder, BlendMode, DisposalMode, Dimensions, FrameMeta, ImageRef, PixelLayout};
/// let canvas = Dimensions::new(2, 2).unwrap();
/// let rgba = [10u8, 20, 30, 255, 40, 50, 60, 255, 70, 80, 90, 255, 100, 110, 120, 255];
/// let meta = FrameMeta::new(0, 0, canvas, 100, BlendMode::Blend, DisposalMode::Keep);
/// let bytes = AnimationEncoder::new(canvas)
///     .loop_count(0)
///     .add_frame(ImageRef::new(canvas, PixelLayout::Rgba8, &rgba).unwrap(), meta)
///     .unwrap()
///     .finish();
/// assert!(webpkit::decode_frames(&bytes).is_ok());
/// ```
///
/// The same builder encodes lossy frames — `webpkit::decode_frames` reads both:
///
/// ```
/// use webpkit::{AnimCodec, AnimationEncoder, BlendMode, DisposalMode, Dimensions, FrameMeta, ImageRef, LossyParams, PixelLayout};
/// let canvas = Dimensions::new(2, 2).unwrap();
/// let rgba = [10u8, 20, 30, 255, 40, 50, 60, 255, 70, 80, 90, 255, 100, 110, 120, 255];
/// let meta = FrameMeta::new(0, 0, canvas, 100, BlendMode::Blend, DisposalMode::Keep);
/// let bytes = AnimationEncoder::new(canvas)
///     .codec(AnimCodec::Lossy { params: LossyParams::new(80) })
///     .add_frame(ImageRef::new(canvas, PixelLayout::Rgba8, &rgba).unwrap(), meta)
///     .unwrap()
///     .finish();
/// assert!(webpkit::decode_frames(&bytes).is_ok());
/// ```
///
/// Calling `finish` before adding a frame does not compile:
///
/// ```compile_fail
/// use webpkit::{AnimationEncoder, Dimensions};
/// let bytes = AnimationEncoder::new(Dimensions::new(2, 2).unwrap()).finish();
/// ```
pub struct AnimationEncoder<S = Empty> {
    canvas: Dimensions,
    background: u32,
    loop_count: u16,
    effort: Effort,
    /// Default codec for [`add_frame`](AnimationEncoder::add_frame).
    codec: AnimCodec,
    /// ICC/Exif/XMP metadata embedded by [`finish`](AnimationEncoder::finish).
    metadata: Metadata,
    /// Accumulated `ANMF` chunk bytes (each a full framed chunk).
    frames: Vec<u8>,
    frame_count: u32,
    has_alpha: bool,
    _state: PhantomData<S>,
}

impl<S> core::fmt::Debug for AnimationEncoder<S> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("AnimationEncoder")
            .field("canvas", &self.canvas)
            .field("frame_count", &self.frame_count)
            .field("loop_count", &self.loop_count)
            .field("effort", &self.effort)
            .field("codec", &self.codec)
            .finish_non_exhaustive()
    }
}

impl AnimationEncoder<Empty> {
    /// Start building an animation with the given canvas size. Defaults:
    /// transparent background, infinite loop, [`Effort::AUTO`], lossless
    /// frames, no metadata.
    #[must_use]
    pub const fn new(canvas: Dimensions) -> Self {
        Self {
            canvas,
            background: 0,
            loop_count: 0,
            effort: Effort::AUTO,
            codec: AnimCodec::Lossless,
            metadata: Metadata::none(),
            frames: Vec::new(),
            frame_count: 0,
            has_alpha: false,
            _state: PhantomData,
        }
    }
}

impl<S> AnimationEncoder<S> {
    /// Set the loop count (`0` = loop forever).
    #[must_use]
    pub const fn loop_count(mut self, loop_count: u16) -> Self {
        self.loop_count = loop_count;
        self
    }

    /// Set the advisory background color (RGBA). Note libwebp's decoder — and
    /// ours — ignores it when compositing; it is written for completeness.
    #[must_use]
    pub const fn background(mut self, rgba: [u8; 4]) -> Self {
        self.background = PixelLayout::Rgba8.unpack(rgba);
        self
    }

    /// Set the effort [`Effort`] used to encode each frame.
    #[must_use]
    pub const fn effort(mut self, effort: Effort) -> Self {
        self.effort = effort;
        self
    }

    /// Set the default [`AnimCodec`] for subsequently added frames — lossless
    /// `VP8L` (the default) or lossy `VP8 ` at a quality, matching the
    /// [`add_frame_with`](Self::add_frame_with) per-frame override. A non-opaque
    /// lossy frame carries a lossless `ALPH` alpha chunk.
    #[must_use]
    pub const fn codec(mut self, codec: AnimCodec) -> Self {
        self.codec = codec;
        self
    }

    /// Embed ICC/Exif/XMP [`Metadata`] in the finished file (upgrades the `VP8X`
    /// flags and emits `ICCP`/`EXIF`/`XMP ` chunks).
    #[must_use]
    pub fn metadata(mut self, metadata: Metadata) -> Self {
        self.metadata = metadata;
        self
    }

    /// The number of frames added so far.
    #[must_use]
    pub const fn frame_count(&self) -> u32 {
        self.frame_count
    }

    /// Add a frame with the encoder's current default [`AnimCodec`]. The frame's
    /// `image` must match `meta.dimensions`, its offset must be even and keep the
    /// frame within the canvas, and its duration must fit in 24 bits.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`] if the image size disagrees with `meta.dimensions`,
    /// the offset is odd, the frame does not fit inside the canvas, or the duration
    /// exceeds `2^24 - 1` ms.
    pub fn add_frame(
        self,
        image: ImageRef<'_>,
        meta: FrameMeta,
    ) -> Result<AnimationEncoder<HasFrames>> {
        let codec = self.codec;
        self.add_frame_with(image, meta, codec)
    }

    /// Add a frame encoded with an explicit [`AnimCodec`], overriding the encoder
    /// default for this frame only (WebP animations may mix codecs).
    ///
    /// # Errors
    ///
    /// [`Error::InvalidFrame`] under the same conditions as
    /// [`add_frame`](Self::add_frame).
    pub fn add_frame_with(
        self,
        image: ImageRef<'_>,
        meta: FrameMeta,
        codec: AnimCodec,
    ) -> Result<AnimationEncoder<HasFrames>> {
        let dims = image.dimensions();
        let fits = meta.x.is_multiple_of(2)
            && meta.y.is_multiple_of(2)
            && dims == meta.dimensions
            && meta.duration_ms < (1 << 24)
            && u64::from(meta.x) + u64::from(dims.width()) <= u64::from(self.canvas.width())
            && u64::from(meta.y) + u64::from(dims.height()) <= u64::from(self.canvas.height());
        if !fits {
            return Err(Error::InvalidFrame);
        }

        let argb = image::unpack_pixels(image.layout(), image.as_bytes());
        let frame_has_alpha = image::argb_has_alpha(&argb);

        let flags = AnmfFlags::from_parts(
            matches!(meta.blend, BlendMode::Overwrite),
            matches!(meta.dispose, DisposalMode::Background),
        );
        let header = AnmfHeader {
            x: meta.x,
            y: meta.y,
            dims,
            duration_ms: meta.duration_ms,
            flags,
        };
        let mut frame_body = header.build().to_vec();
        frame_body.extend_from_slice(&encode_frame_payload(self.effort, dims, &argb, codec));

        let mut frames = self.frames;
        push_chunk(&mut frames, FourCc::ANMF, &frame_body);

        Ok(AnimationEncoder {
            canvas: self.canvas,
            background: self.background,
            loop_count: self.loop_count,
            effort: self.effort,
            codec: self.codec,
            metadata: self.metadata,
            frames,
            frame_count: self.frame_count + 1,
            has_alpha: self.has_alpha || frame_has_alpha,
            _state: PhantomData,
        })
    }
}

impl AnimationEncoder<HasFrames> {
    /// Finish the animation, returning the complete WebP file bytes.
    #[must_use]
    pub fn finish(self) -> Vec<u8> {
        let flags = Vp8xFlags::for_output(&self.metadata, self.has_alpha).with_animation();
        let mut body = Vec::new();
        push_chunk(
            &mut body,
            FourCc::VP8X,
            &Vp8xInfo::build(flags, self.canvas),
        );
        if let Some(icc) = &self.metadata.icc_profile {
            push_chunk(&mut body, FourCc::ICCP, icc);
        }
        push_chunk(
            &mut body,
            FourCc::ANIM,
            &AnimChunk {
                background: self.background,
                loop_count: self.loop_count,
            }
            .build(),
        );
        body.extend_from_slice(&self.frames);
        if let Some(exif) = &self.metadata.exif {
            push_chunk(&mut body, FourCc::EXIF, exif);
        }
        if let Some(xmp) = &self.metadata.xmp {
            push_chunk(&mut body, FourCc::XMP, xmp);
        }
        riff_envelope(&body)
    }

    /// Finish the animation and write the WebP file to `writer`.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] on a write failure.
    #[cfg(feature = "std")]
    pub fn finish_to<W: std::io::Write>(self, mut writer: W) -> Result<()> {
        writer.write_all(&self.finish())?;
        Ok(())
    }
}

#[cfg(test)]
mod anim_tests {
    use super::{AnimCodec, AnimationEncoder, LossyParams};
    use crate::container::vp8x::Vp8xInfo;
    use crate::image::{self, Dimensions, ImageRef, Metadata, PixelLayout};
    use crate::{BlendMode, DisposalMode, Effort, Error, FrameMeta, decode_frames};

    fn meta(dims: Dimensions, x: u32, y: u32, duration: u32) -> FrameMeta {
        FrameMeta::new(x, y, dims, duration, BlendMode::Blend, DisposalMode::Keep)
    }

    fn frame_bytes(dims: Dimensions, argb: u32) -> Vec<u8> {
        let pixels = vec![argb; usize::try_from(dims.pixel_count()).unwrap()];
        image::pack_pixels(PixelLayout::Rgba8, &pixels)
    }

    fn image_ref(dims: Dimensions, rgba: &[u8]) -> ImageRef<'_> {
        ImageRef::new(dims, PixelLayout::Rgba8, rgba).unwrap()
    }

    /// Parse a flat RIFF chunk list (`fourcc`, `payload`), honoring the pad byte.
    fn chunks(mut data: &[u8]) -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        while data.len() >= 8 {
            let fourcc = String::from_utf8_lossy(&data[0..4]).to_string();
            let size =
                usize::try_from(u32::from_le_bytes([data[4], data[5], data[6], data[7]])).unwrap();
            let payload = data[8..8 + size].to_vec();
            out.push((fourcc, payload));
            data = &data[8 + size + (size & 1)..];
        }
        out
    }

    /// The top-level chunks of a WebP file (past the 12-byte RIFF/WEBP header).
    fn top_chunks(file: &[u8]) -> Vec<(String, Vec<u8>)> {
        chunks(&file[12..])
    }

    fn fourccs(chs: &[(String, Vec<u8>)]) -> Vec<String> {
        chs.iter().map(|(f, _)| f.clone()).collect()
    }

    /// The sub-chunk fourccs inside each `ANMF` frame body (past the 16-byte header).
    fn anmf_subchunks(file: &[u8]) -> Vec<Vec<String>> {
        top_chunks(file)
            .iter()
            .filter(|(f, _)| f == "ANMF")
            .map(|(_, body)| fourccs(&chunks(&body[16..])))
            .collect()
    }

    #[test]
    fn lossless_animation_bytes_unchanged() {
        // The default codec + empty metadata must reproduce the pre-change layout:
        // exactly VP8X, ANIM, ANMF, ANMF, every frame image a VP8L, no metadata or
        // ALPH chunks. Adding the `codec`/`metadata` fields must not shift output.
        let canvas = Dimensions::new(4, 4).unwrap();
        let rgba0 = frame_bytes(canvas, 0xFF00_00FF);
        let rgba1 = frame_bytes(canvas, 0xFF00_FF00);
        let file = AnimationEncoder::new(canvas)
            .loop_count(0)
            .add_frame(image_ref(canvas, &rgba0), meta(canvas, 0, 0, 100))
            .unwrap()
            .add_frame(image_ref(canvas, &rgba1), meta(canvas, 0, 0, 50))
            .unwrap()
            .finish();

        assert_eq!(&file[12..16], b"VP8X");
        assert_eq!(
            fourccs(&top_chunks(&file)),
            ["VP8X", "ANIM", "ANMF", "ANMF"]
        );
        for sub in anmf_subchunks(&file) {
            assert_eq!(sub, ["VP8L"]);
        }
    }

    #[test]
    fn animation_round_trips_through_decode_frames() {
        let canvas = Dimensions::new(4, 4).unwrap();
        let rgba0 = frame_bytes(canvas, 0xFF00_00FF);
        let rgba1 = frame_bytes(canvas, 0xFF00_FF00);
        let file = AnimationEncoder::new(canvas)
            .loop_count(3)
            .background([1, 2, 3, 255])
            .add_frame(image_ref(canvas, &rgba0), meta(canvas, 0, 0, 100))
            .unwrap()
            .add_frame(image_ref(canvas, &rgba1), meta(canvas, 0, 0, 50))
            .unwrap()
            .finish();

        let frames = decode_frames(&file).unwrap();
        let info = frames.anim_info();
        assert_eq!(info.canvas, canvas);
        assert_eq!(info.loop_count, 3);
        assert_eq!(info.background_rgba, [1, 2, 3, 255]);
        let decoded: Vec<_> = frames.map(Result::unwrap).collect();
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].image().as_bytes(), rgba0);
        assert_eq!(decoded[0].meta().duration_ms, 100);
        assert_eq!(decoded[1].image().as_bytes(), rgba1);
        assert_eq!(decoded[1].meta().duration_ms, 50);
    }

    #[test]
    fn lossy_animation_round_trips_through_umbrella_decoder() {
        let canvas = Dimensions::new(8, 8).unwrap();
        let rgba0 = frame_bytes(canvas, 0xFF20_4060);
        let rgba1 = frame_bytes(canvas, 0xFF80_A0C0);
        let file = AnimationEncoder::new(canvas)
            .codec(AnimCodec::Lossy {
                params: LossyParams::new(90),
            })
            .add_frame(image_ref(canvas, &rgba0), meta(canvas, 0, 0, 40))
            .unwrap()
            .add_frame(image_ref(canvas, &rgba1), meta(canvas, 0, 0, 60))
            .unwrap()
            .finish();

        for sub in anmf_subchunks(&file) {
            assert!(sub.contains(&"VP8 ".to_string()), "expected VP8 : {sub:?}");
            assert!(
                !sub.contains(&"VP8L".to_string()),
                "unexpected VP8L: {sub:?}"
            );
        }

        let frames = decode_frames(&file).unwrap();
        assert_eq!(frames.anim_info().canvas, canvas);
        let decoded: Vec<_> = frames.map(Result::unwrap).collect();
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[0].meta().duration_ms, 40);
        assert_eq!(decoded[1].meta().duration_ms, 60);
        for (src, frame) in [&rgba0, &rgba1].into_iter().zip(&decoded) {
            let dec = frame.image().as_bytes();
            let mut total = 0u64;
            let mut count = 0u64;
            for (s, d) in src.chunks_exact(4).zip(dec.chunks_exact(4)) {
                for k in 0..3 {
                    total += u64::from(s[k].abs_diff(d[k]));
                    count += 1;
                }
            }
            assert!(
                total <= 30 * count,
                "mean-abs-error too high: {total}/{count}"
            );
        }
    }

    #[test]
    fn lossy_frame_with_alpha_emits_alph_and_round_trips_alpha() {
        let canvas = Dimensions::new(4, 4).unwrap();
        // A per-pixel alpha ramp in the top byte over a fixed RGB.
        let argb: Vec<u32> = (0..16u32).map(|i| ((i * 15) << 24) | 0x0011_2233).collect();
        let rgba = image::pack_pixels(PixelLayout::Rgba8, &argb);
        let file = AnimationEncoder::new(canvas)
            .codec(AnimCodec::Lossy {
                params: LossyParams::new(90),
            })
            .add_frame(image_ref(canvas, &rgba), meta(canvas, 0, 0, 40))
            .unwrap()
            .finish();

        // ALPH must precede VP8 inside the frame body.
        let sub = anmf_subchunks(&file);
        assert_eq!(sub[0], ["ALPH", "VP8 "]);
        // VP8X ALPHA flag set.
        assert_ne!(file[20] & 0x10, 0, "ALPHA flag must be set");

        let frames = decode_frames(&file).unwrap();
        let decoded: Vec<_> = frames.map(Result::unwrap).collect();
        let dec = decoded[0].image().as_bytes();
        let recovered: Vec<u8> = dec.chunks_exact(4).map(|px| px[3]).collect();
        let expected: Vec<u8> = argb
            .iter()
            .map(|&p| u8::try_from(p >> 24).unwrap())
            .collect();
        assert_eq!(
            recovered, expected,
            "alpha is lossless and must round-trip exactly"
        );
    }

    #[test]
    fn animation_carries_metadata_chunks_and_flags() {
        let canvas = Dimensions::new(4, 4).unwrap();
        let rgba = frame_bytes(canvas, 0xFF00_00FF);
        // Odd-length ICC to also exercise the RIFF pad byte.
        let icc = vec![1u8, 2, 3];
        let exif = vec![9u8, 8, 7, 6];
        let xmp = vec![5u8, 4, 3, 2, 1];
        let metadata = Metadata {
            icc_profile: Some(icc.clone()),
            exif: Some(exif.clone()),
            xmp: Some(xmp.clone()),
        };
        let file = AnimationEncoder::new(canvas)
            .metadata(metadata)
            .add_frame(image_ref(canvas, &rgba), meta(canvas, 0, 0, 40))
            .unwrap()
            .finish();

        let top = top_chunks(&file);
        assert_eq!(
            fourccs(&top),
            ["VP8X", "ICCP", "ANIM", "ANMF", "EXIF", "XMP "]
        );
        let payload = |name: &str| {
            top.iter()
                .find(|(f, _)| f == name)
                .map(|(_, p)| p.clone())
                .unwrap()
        };
        assert_eq!(payload("ICCP"), icc);
        assert_eq!(payload("EXIF"), exif);
        assert_eq!(payload("XMP "), xmp);

        let flags = Vp8xInfo::parse(&payload("VP8X")).unwrap().flags;
        assert!(flags.has_icc());
        assert!(flags.has_exif());
        assert!(flags.has_xmp());
        assert!(flags.is_animated());
    }

    #[test]
    fn animation_metadata_survives_probe_animation() {
        let canvas = Dimensions::new(6, 6).unwrap();
        let rgba0 = frame_bytes(canvas, 0xFF11_2233);
        let rgba1 = frame_bytes(canvas, 0xFF44_5566);
        let metadata = Metadata {
            icc_profile: Some(vec![1, 2, 3]),
            exif: Some(vec![4, 5]),
            xmp: Some(vec![6, 7, 8]),
        };
        let file = AnimationEncoder::new(canvas)
            .codec(AnimCodec::Lossy {
                params: LossyParams::new(80),
            })
            .loop_count(2)
            .metadata(metadata)
            .add_frame(image_ref(canvas, &rgba0), meta(canvas, 0, 0, 30))
            .unwrap()
            .add_frame(image_ref(canvas, &rgba1), meta(canvas, 0, 0, 30))
            .unwrap()
            .finish();

        let info = crate::probe_animation(&file).unwrap();
        assert_eq!(info.canvas, canvas);
        assert_eq!(info.frame_count, 2);
        assert_eq!(info.loop_count, 2);
    }

    #[test]
    fn mixed_codec_frames_decode() {
        let canvas = Dimensions::new(8, 8).unwrap();
        let rgba0 = frame_bytes(canvas, 0xFF10_2030);
        let rgba1 = frame_bytes(canvas, 0xFF40_5060);
        let file = AnimationEncoder::new(canvas)
            .codec(AnimCodec::Lossless)
            .add_frame(image_ref(canvas, &rgba0), meta(canvas, 0, 0, 40))
            .unwrap()
            .add_frame_with(
                image_ref(canvas, &rgba1),
                meta(canvas, 0, 0, 40),
                AnimCodec::Lossy {
                    params: LossyParams::new(80),
                },
            )
            .unwrap()
            .finish();

        let sub = anmf_subchunks(&file);
        assert_eq!(sub[0], ["VP8L"]);
        assert!(
            sub[1].contains(&"VP8 ".to_string()),
            "frame 1 must be lossy: {:?}",
            sub[1]
        );
        let frames = decode_frames(&file).unwrap();
        assert_eq!(frames.map(Result::unwrap).count(), 2);
    }

    #[test]
    fn lossy_default_via_encoder_level_lossy() {
        // `.codec(Lossy(75))` then plain `add_frame` calls (no override) must both be
        // lossy; kills a mutant that ignores `self.codec` in `add_frame`.
        let canvas = Dimensions::new(8, 8).unwrap();
        let rgba = frame_bytes(canvas, 0xFF30_6090);
        let file = AnimationEncoder::new(canvas)
            .codec(AnimCodec::Lossy {
                params: LossyParams::new(75),
            })
            .add_frame(image_ref(canvas, &rgba), meta(canvas, 0, 0, 40))
            .unwrap()
            .add_frame(image_ref(canvas, &rgba), meta(canvas, 0, 0, 40))
            .unwrap()
            .finish();
        for sub in anmf_subchunks(&file) {
            assert!(
                sub.contains(&"VP8 ".to_string()),
                "expected lossy frame: {sub:?}"
            );
        }
    }

    #[test]
    fn frame_count_tracks_added_frames() {
        let canvas = Dimensions::new(2, 2).unwrap();
        let rgba = frame_bytes(canvas, 0xFF00_0000);
        let enc = AnimationEncoder::new(canvas);
        assert_eq!(enc.frame_count(), 0);
        let enc = enc
            .add_frame(image_ref(canvas, &rgba), meta(canvas, 0, 0, 40))
            .unwrap();
        assert_eq!(enc.frame_count(), 1);
    }

    #[test]
    fn add_frame_rejects_odd_offset() {
        let canvas = Dimensions::new(8, 8).unwrap();
        let tile = Dimensions::new(2, 2).unwrap();
        let rgba = frame_bytes(tile, 0xFF00_0000);
        let err = AnimationEncoder::new(canvas)
            .add_frame(image_ref(tile, &rgba), meta(tile, 1, 0, 40))
            .unwrap_err();
        assert_eq!(err, Error::InvalidFrame);
    }

    #[test]
    fn add_frame_rejects_out_of_bounds() {
        let canvas = Dimensions::new(4, 4).unwrap();
        let tile = Dimensions::new(4, 4).unwrap();
        let rgba = frame_bytes(tile, 0xFF00_0000);
        let err = AnimationEncoder::new(canvas)
            .add_frame(image_ref(tile, &rgba), meta(tile, 2, 2, 40))
            .unwrap_err();
        assert_eq!(err, Error::InvalidFrame);
    }

    #[test]
    fn add_frame_rejects_dims_mismatch_and_huge_duration() {
        let canvas = Dimensions::new(4, 4).unwrap();
        let two = Dimensions::new(2, 2).unwrap();
        let rgba = frame_bytes(two, 0xFF00_0000);
        // image is 2x2 but meta claims 4x4.
        let err = AnimationEncoder::new(canvas)
            .add_frame(image_ref(two, &rgba), meta(canvas, 0, 0, 40))
            .unwrap_err();
        assert_eq!(err, Error::InvalidFrame);
        // duration beyond 24 bits.
        let err = AnimationEncoder::new(canvas)
            .add_frame(image_ref(two, &rgba), meta(two, 0, 0, 1 << 24))
            .unwrap_err();
        assert_eq!(err, Error::InvalidFrame);
    }

    #[test]
    fn add_frame_x_bound_uses_sum_not_product() {
        let canvas = Dimensions::new(6, 2).unwrap();
        let tile = Dimensions::new(4, 2).unwrap();
        let rgba = frame_bytes(tile, 0xFF00_0000);
        let enc = AnimationEncoder::new(canvas)
            .add_frame(image_ref(tile, &rgba), meta(tile, 2, 0, 40))
            .expect("x+width == canvas width must fit");
        assert_eq!(enc.frame_count(), 1);
    }

    #[test]
    fn add_frame_y_bound_uses_sum_not_product() {
        let canvas = Dimensions::new(2, 6).unwrap();
        let tile = Dimensions::new(2, 4).unwrap();
        let rgba = frame_bytes(tile, 0xFF00_0000);
        let enc = AnimationEncoder::new(canvas)
            .add_frame(image_ref(tile, &rgba), meta(tile, 0, 2, 40))
            .expect("y+height == canvas height must fit");
        assert_eq!(enc.frame_count(), 1);
    }

    #[test]
    fn has_alpha_flag_is_the_or_of_frames() {
        let canvas = Dimensions::new(2, 2).unwrap();
        let translucent = frame_bytes(canvas, 0x8000_00FF); // alpha 0x80
        let file = AnimationEncoder::new(canvas)
            .add_frame(image_ref(canvas, &translucent), meta(canvas, 0, 0, 40))
            .unwrap()
            .finish();
        assert_eq!(&file[12..16], b"VP8X");
        assert_ne!(
            file[20] & 0x10,
            0,
            "ALPHA flag must be set for a translucent frame"
        );

        let opaque = frame_bytes(canvas, 0xFF00_00FF);
        let file2 = AnimationEncoder::new(canvas)
            .add_frame(image_ref(canvas, &opaque), meta(canvas, 0, 0, 40))
            .unwrap()
            .finish();
        assert_eq!(
            file2[20] & 0x10,
            0,
            "ALPHA flag must be clear for an opaque frame"
        );
    }

    #[test]
    fn debug_impl_renders_the_struct_fields() {
        let canvas = Dimensions::new(4, 4).unwrap();
        let enc = AnimationEncoder::new(canvas)
            .loop_count(7)
            .effort(Effort::level(0))
            .codec(AnimCodec::Lossy {
                params: LossyParams::new(80),
            });
        let rendered = format!("{enc:?}");
        assert!(rendered.contains("AnimationEncoder"), "got: {rendered}");
        assert!(rendered.contains("canvas"), "got: {rendered}");
        assert!(rendered.contains("frame_count"), "got: {rendered}");
        assert!(rendered.contains("loop_count: 7"), "got: {rendered}");
        assert!(rendered.contains("Level(0)"), "got: {rendered}");
        assert!(rendered.contains("codec"), "got: {rendered}");
        assert!(rendered.contains("Lossy"), "got: {rendered}");
    }

    #[cfg(feature = "std")]
    #[test]
    fn finish_to_writes_the_same_bytes_as_finish() {
        let canvas = Dimensions::new(2, 2).unwrap();
        let rgba = frame_bytes(canvas, 0xFF00_00FF);
        let make = || {
            AnimationEncoder::new(canvas)
                .add_frame(image_ref(canvas, &rgba), meta(canvas, 0, 0, 40))
                .unwrap()
        };
        let expected = make().finish();
        assert!(!expected.is_empty());
        let mut buf = Vec::new();
        make().finish_to(&mut buf).unwrap();
        assert_eq!(buf, expected);
    }
}
