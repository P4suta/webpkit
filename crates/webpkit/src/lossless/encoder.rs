//! Public encoding configuration: the effort [`Effort`] and the [`EncoderConfig`]
//! that also carries the metadata to embed, plus the type-state
//! [`AnimationEncoder`] for building animated (`ANIM`/`ANMF`) files.

use core::marker::PhantomData;

use crate::Effort;
use crate::container::anim::{AnmfFlags, AnmfHeader};
use crate::container::fourcc::FourCc;
use crate::container::vp8x::{Vp8xFlags, Vp8xInfo};
use crate::container::writer::{push_chunk, riff_envelope};
use crate::error::{Error, Result};
use crate::image::{self, Dimensions, ImageRef, Metadata, PixelLayout};
use crate::lossless::animation::{BlendMode, DisposalMode, FrameMeta};
use crate::lossless::prelude::*;
use crate::lossless::vp8l;

/// Encode `argb` (native ARGB, `width * height` pixels, row-major) as a raw VP8L
/// payload at `effort`: [`Effort::Fast`] emits only the literal + subtract-green
/// tiers, [`Effort::Balanced`] runs the full Tier 0/1/2 LZ77 + color-cache search,
/// and [`Effort::Best`] additionally tries the forward-transform families and keeps
/// the smallest. Only `Best` diverges from `Balanced`, so the `Balanced`/`Fast`
/// bytes are unchanged from the earlier tiers.
pub(crate) fn encode_payload(effort: Effort, width: u32, height: u32, argb: &[u32]) -> Vec<u8> {
    match effort {
        Effort::Fast => vp8l::encode::encode_with(width, height, argb, false),
        Effort::Balanced => vp8l::encode::encode(width, height, argb),
        Effort::Best => vp8l::encode::encode_best(width, height, argb),
    }
}

/// How [`crate::lossless::encode_image`] treats the metadata inherited from the source
/// [`crate::Image`] — re-exported from the core shell, the single home of the
/// fold logic ([`Metadata::resolve`]).
pub use crate::image::MetadataPolicy;

/// Configuration for [`crate::lossless::encode`]: the effort method and any sidecar
/// metadata to embed in an extended (`VP8X`) container.
#[derive(Clone, Debug, Default)]
pub struct EncoderConfig {
    pub(crate) effort: Effort,
    pub(crate) metadata: Metadata,
    pub(crate) policy: MetadataPolicy,
}

impl EncoderConfig {
    /// Default configuration: [`Effort::Balanced`], no metadata.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the effort tier.
    #[must_use]
    pub const fn with_effort(mut self, effort: Effort) -> Self {
        self.effort = effort;
        self
    }

    /// Set the metadata to embed (upgrades the output to a `VP8X` container).
    ///
    /// Under [`crate::lossless::encode_image`] this is a per-field *override*: any field set
    /// here wins over the image's own metadata and survives even
    /// [`MetadataPolicy::StripPrivate`]. The policy in
    /// [`with_metadata_policy`](Self::with_metadata_policy) gates only the
    /// *inherited* image metadata, not what is set here.
    #[must_use]
    pub fn with_metadata(mut self, metadata: Metadata) -> Self {
        self.metadata = metadata;
        self
    }

    /// Set the [`MetadataPolicy`] consulted by [`crate::lossless::encode_image`]. Ignored by
    /// [`crate::lossless::encode`], which has no source image to inherit metadata from.
    ///
    /// The policy gates only the *inherited* image metadata; an explicit value set
    /// via [`with_metadata`](Self::with_metadata) still wins over it.
    #[must_use]
    pub const fn with_metadata_policy(mut self, policy: MetadataPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Fold the image's `inherited` metadata, this config's [`MetadataPolicy`], and
    /// any explicit [`with_metadata`](Self::with_metadata) override into the
    /// effective metadata to embed. Delegates to the shared [`Metadata::resolve`]
    /// fold (the config's own metadata is the per-field override).
    pub(crate) fn resolve_metadata(&self, inherited: &Metadata) -> Metadata {
        self.metadata.resolve(inherited, self.policy)
    }
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
/// and the whole file is produced by `finish`, so no [`std::io::Seek`] is needed;
/// each frame is encoded as its own `VP8L` bitstream.
///
/// ```
/// use webpkit::{AnimationEncoder, BlendMode, DisposalMode, Dimensions, FrameMeta, ImageRef, PixelLayout};
/// let canvas = Dimensions::new(2, 2).unwrap();
/// let rgba = [10u8, 20, 30, 255, 40, 50, 60, 255, 70, 80, 90, 255, 100, 110, 120, 255];
/// let meta = FrameMeta::new(0, 0, canvas, 100, BlendMode::Blend, DisposalMode::Keep);
/// let bytes = AnimationEncoder::new(canvas)
///     .with_loop_count(0)
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
            .finish_non_exhaustive()
    }
}

impl AnimationEncoder<Empty> {
    /// Start building an animation with the given canvas size. Defaults:
    /// transparent background, infinite loop, [`Effort::Balanced`].
    #[must_use]
    pub const fn new(canvas: Dimensions) -> Self {
        Self {
            canvas,
            background: 0,
            loop_count: 0,
            effort: Effort::Balanced,
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
    pub const fn with_loop_count(mut self, loop_count: u16) -> Self {
        self.loop_count = loop_count;
        self
    }

    /// Set the advisory background color (RGBA). Note libwebp's decoder — and
    /// ours — ignores it when compositing; it is written for completeness.
    #[must_use]
    pub const fn with_background(mut self, rgba: [u8; 4]) -> Self {
        self.background = PixelLayout::Rgba8.unpack(rgba);
        self
    }

    /// Set the effort [`Effort`] used to encode each frame.
    #[must_use]
    pub const fn with_effort(mut self, effort: Effort) -> Self {
        self.effort = effort;
        self
    }

    /// The number of frames added so far.
    #[must_use]
    pub const fn frame_count(&self) -> u32 {
        self.frame_count
    }

    /// Add a frame. The frame's `image` must match `meta.dimensions`, its offset
    /// must be even and keep the frame within the canvas, and its duration must
    /// fit in 24 bits.
    ///
    /// # Errors
    ///
    /// [`Error::InvalidDimensions`] if the image size disagrees with
    /// `meta.dimensions`, the offset is odd, the frame does not fit inside the
    /// canvas, or the duration exceeds `2^24 - 1` ms.
    pub fn add_frame(
        self,
        image: ImageRef<'_>,
        meta: FrameMeta,
    ) -> Result<AnimationEncoder<HasFrames>> {
        let dims = image.dimensions();
        let fits = meta.x.is_multiple_of(2)
            && meta.y.is_multiple_of(2)
            && dims == meta.dimensions
            && meta.duration_ms < (1 << 24)
            && u64::from(meta.x) + u64::from(dims.width()) <= u64::from(self.canvas.width())
            && u64::from(meta.y) + u64::from(dims.height()) <= u64::from(self.canvas.height());
        if !fits {
            return Err(Error::InvalidDimensions);
        }

        let argb = image::unpack_pixels(image.layout(), image.as_bytes());
        let frame_has_alpha = image::argb_has_alpha(&argb);
        let payload = encode_payload(self.effort, dims.width(), dims.height(), &argb);

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
        push_chunk(&mut frame_body, FourCc::VP8L, &payload);
        let mut frames = self.frames;
        push_chunk(&mut frames, FourCc::ANMF, &frame_body);

        Ok(AnimationEncoder {
            canvas: self.canvas,
            background: self.background,
            loop_count: self.loop_count,
            effort: self.effort,
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
        let flags = Vp8xFlags::for_output(&Metadata::none(), self.has_alpha).with_animation();
        let mut body = Vec::new();
        push_chunk(
            &mut body,
            FourCc::VP8X,
            &Vp8xInfo::build(flags, self.canvas),
        );
        push_chunk(
            &mut body,
            FourCc::ANIM,
            &crate::container::anim::AnimChunk {
                background: self.background,
                loop_count: self.loop_count,
            }
            .build(),
        );
        body.extend_from_slice(&self.frames);
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
mod tests {
    use super::{AnimationEncoder, EncoderConfig, MetadataPolicy, encode_payload};
    use crate::Effort;
    use crate::error::Error;
    use crate::image::{self, Dimensions, ImageRef, Metadata, PixelLayout};
    use crate::lossless::animation::{BlendMode, DisposalMode, FrameMeta, decode_frames};

    #[test]
    fn default_is_balanced_no_metadata() {
        let config = EncoderConfig::new();
        assert_eq!(config.effort, Effort::Balanced);
        assert!(config.metadata.is_empty());
    }

    #[test]
    fn effort_routes_to_the_expected_encoder() {
        // Each effort tier dispatches to its VP8L encoder entry point: Fast to the
        // literal-only path, Balanced to the Tier 2 search, Best to the tiered
        // transform search. Best must never be larger than Balanced (it keeps the
        // Balanced result as its floor).
        let argb = [0xff10_2030u32, 0xff40_5060, 0xff70_8090, 0xffa0_b0c0];
        assert_eq!(
            encode_payload(Effort::Fast, 2, 2, &argb),
            crate::lossless::vp8l::encode::encode_with(2, 2, &argb, false),
        );
        assert_eq!(
            encode_payload(Effort::Balanced, 2, 2, &argb),
            crate::lossless::vp8l::encode::encode(2, 2, &argb),
        );
        assert_eq!(
            encode_payload(Effort::Best, 2, 2, &argb),
            crate::lossless::vp8l::encode::encode_best(2, 2, &argb),
        );
        assert!(
            encode_payload(Effort::Best, 2, 2, &argb).len()
                <= encode_payload(Effort::Balanced, 2, 2, &argb).len()
        );
    }

    #[test]
    fn builder_sets_fields() {
        let config = EncoderConfig::new()
            .with_effort(Effort::Fast)
            .with_metadata(Metadata {
                exif: Some(vec![1, 2, 3]),
                ..Metadata::none()
            });
        assert_eq!(config.effort, Effort::Fast);
        assert_eq!(config.metadata.exif.as_deref(), Some(&[1, 2, 3][..]));
    }

    #[test]
    fn default_policy_is_preserve() {
        assert_eq!(EncoderConfig::new().policy, MetadataPolicy::Preserve);
    }

    #[test]
    fn resolve_metadata_delegates_to_core() {
        // The config forwards its own metadata (as the per-field override) and its
        // policy to the shared `Metadata::resolve` fold; the fold's truth table is
        // owned and exhaustively tested in the core shell.
        let inherited = Metadata {
            icc_profile: Some(vec![1]),
            exif: Some(vec![2]),
            xmp: Some(vec![3]),
        };
        let override_meta = Metadata {
            exif: Some(vec![9]),
            ..Metadata::none()
        };
        let config = EncoderConfig::new()
            .with_metadata(override_meta.clone())
            .with_metadata_policy(MetadataPolicy::StripPrivate);
        assert_eq!(
            config.resolve_metadata(&inherited),
            override_meta.resolve(&inherited, MetadataPolicy::StripPrivate),
        );
    }

    fn meta(dims: Dimensions, x: u32, y: u32, duration: u32) -> FrameMeta {
        FrameMeta {
            x,
            y,
            dimensions: dims,
            duration_ms: duration,
            blend: BlendMode::Blend,
            dispose: DisposalMode::Keep,
        }
    }

    fn frame_bytes(dims: Dimensions, argb: u32) -> Vec<u8> {
        let pixels = vec![argb; usize::try_from(dims.pixel_count()).unwrap()];
        image::pack_pixels(PixelLayout::Rgba8, &pixels)
    }

    #[test]
    fn animation_round_trips_through_decode_frames() {
        let canvas = Dimensions::new(4, 4).unwrap();
        let rgba0 = frame_bytes(canvas, 0xFF00_00FF);
        let rgba1 = frame_bytes(canvas, 0xFF00_FF00);
        let file = AnimationEncoder::new(canvas)
            .with_loop_count(3)
            .with_background([1, 2, 3, 255])
            .add_frame(
                ImageRef::new(canvas, PixelLayout::Rgba8, &rgba0).unwrap(),
                meta(canvas, 0, 0, 100),
            )
            .unwrap()
            .add_frame(
                ImageRef::new(canvas, PixelLayout::Rgba8, &rgba1).unwrap(),
                meta(canvas, 0, 0, 50),
            )
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
    fn frame_count_tracks_added_frames() {
        let canvas = Dimensions::new(2, 2).unwrap();
        let rgba = frame_bytes(canvas, 0xFF00_0000);
        let enc = AnimationEncoder::new(canvas);
        assert_eq!(enc.frame_count(), 0);
        let enc = enc
            .add_frame(
                ImageRef::new(canvas, PixelLayout::Rgba8, &rgba).unwrap(),
                meta(canvas, 0, 0, 40),
            )
            .unwrap();
        assert_eq!(enc.frame_count(), 1);
    }

    #[test]
    fn add_frame_rejects_odd_offset() {
        let canvas = Dimensions::new(8, 8).unwrap();
        let tile = Dimensions::new(2, 2).unwrap();
        let rgba = frame_bytes(tile, 0xFF00_0000);
        let err = AnimationEncoder::new(canvas)
            .add_frame(
                ImageRef::new(tile, PixelLayout::Rgba8, &rgba).unwrap(),
                meta(tile, 1, 0, 40), // odd x
            )
            .unwrap_err();
        assert_eq!(err, Error::InvalidDimensions);
    }

    #[test]
    fn add_frame_rejects_out_of_bounds() {
        let canvas = Dimensions::new(4, 4).unwrap();
        let tile = Dimensions::new(4, 4).unwrap();
        let rgba = frame_bytes(tile, 0xFF00_0000);
        let err = AnimationEncoder::new(canvas)
            .add_frame(
                ImageRef::new(tile, PixelLayout::Rgba8, &rgba).unwrap(),
                meta(tile, 2, 2, 40), // 4x4 at (2,2) overflows the 4x4 canvas
            )
            .unwrap_err();
        assert_eq!(err, Error::InvalidDimensions);
    }

    #[test]
    fn add_frame_rejects_dims_mismatch_and_huge_duration() {
        let canvas = Dimensions::new(4, 4).unwrap();
        let two = Dimensions::new(2, 2).unwrap();
        let rgba = frame_bytes(two, 0xFF00_0000);
        // image is 2x2 but meta claims 4x4.
        let err = AnimationEncoder::new(canvas)
            .add_frame(
                ImageRef::new(two, PixelLayout::Rgba8, &rgba).unwrap(),
                meta(canvas, 0, 0, 40),
            )
            .unwrap_err();
        assert_eq!(err, Error::InvalidDimensions);
        // duration beyond 24 bits.
        let err = AnimationEncoder::new(canvas)
            .add_frame(
                ImageRef::new(two, PixelLayout::Rgba8, &rgba).unwrap(),
                meta(two, 0, 0, 1 << 24),
            )
            .unwrap_err();
        assert_eq!(err, Error::InvalidDimensions);
    }

    #[test]
    fn debug_impl_renders_the_struct_fields() {
        // Kills the `fmt -> Ok(Default::default())` mutant, which would emit an
        // empty string instead of the struct's fields.
        let canvas = Dimensions::new(4, 4).unwrap();
        let enc = AnimationEncoder::new(canvas)
            .with_loop_count(7)
            .with_effort(Effort::Fast);
        let rendered = format!("{enc:?}");
        assert!(rendered.contains("AnimationEncoder"), "got: {rendered}");
        assert!(rendered.contains("canvas"), "got: {rendered}");
        assert!(rendered.contains("frame_count"), "got: {rendered}");
        assert!(rendered.contains("loop_count: 7"), "got: {rendered}");
        assert!(rendered.contains("Fast"), "got: {rendered}");
    }

    #[test]
    fn add_frame_x_bound_uses_sum_not_product() {
        // A 4x2 tile at x=2 on a 6x2 canvas: x + width = 6 <= 6 fits, but the
        // `+ -> *` mutant on the x bound computes x * width = 8 > 6 and rejects.
        let canvas = Dimensions::new(6, 2).unwrap();
        let tile = Dimensions::new(4, 2).unwrap();
        let rgba = frame_bytes(tile, 0xFF00_0000);
        let enc = AnimationEncoder::new(canvas)
            .add_frame(
                ImageRef::new(tile, PixelLayout::Rgba8, &rgba).unwrap(),
                meta(tile, 2, 0, 40),
            )
            .expect("x+width == canvas width must fit");
        assert_eq!(enc.frame_count(), 1);
    }

    #[test]
    fn add_frame_y_bound_uses_sum_not_product() {
        // A 2x4 tile at y=2 on a 2x6 canvas: y + height = 6 <= 6 fits, but the
        // `+ -> *` mutant on the y bound computes y * height = 8 > 6 and rejects.
        let canvas = Dimensions::new(2, 6).unwrap();
        let tile = Dimensions::new(2, 4).unwrap();
        let rgba = frame_bytes(tile, 0xFF00_0000);
        let enc = AnimationEncoder::new(canvas)
            .add_frame(
                ImageRef::new(tile, PixelLayout::Rgba8, &rgba).unwrap(),
                meta(tile, 0, 2, 40),
            )
            .expect("y+height == canvas height must fit");
        assert_eq!(enc.frame_count(), 1);
    }

    #[test]
    fn has_alpha_flag_is_the_or_of_frames() {
        // A single frame carrying a non-opaque pixel must set the VP8X ALPHA flag
        // (0x10). The accumulator starts `false`, so `false || true` sets it, while
        // the `|| -> &&` mutant computes `false && true` and leaves it clear.
        let canvas = Dimensions::new(2, 2).unwrap();
        let translucent = frame_bytes(canvas, 0x8000_00FF); // alpha 0x80
        let file = AnimationEncoder::new(canvas)
            .add_frame(
                ImageRef::new(canvas, PixelLayout::Rgba8, &translucent).unwrap(),
                meta(canvas, 0, 0, 40),
            )
            .unwrap()
            .finish();
        // Layout: RIFF(4) + size(4) + WEBP(4) + "VP8X"(4) + size(4) => flags at 20.
        assert_eq!(&file[12..16], b"VP8X");
        assert_ne!(
            file[20] & 0x10,
            0,
            "ALPHA flag must be set for a translucent frame"
        );

        // Contrast: a fully-opaque frame leaves the ALPHA flag clear (same under
        // both real and mutant, but documents the intended behavior).
        let opaque = frame_bytes(canvas, 0xFF00_00FF);
        let file2 = AnimationEncoder::new(canvas)
            .add_frame(
                ImageRef::new(canvas, PixelLayout::Rgba8, &opaque).unwrap(),
                meta(canvas, 0, 0, 40),
            )
            .unwrap()
            .finish();
        assert_eq!(
            file2[20] & 0x10,
            0,
            "ALPHA flag must be clear for an opaque frame"
        );
    }

    #[cfg(feature = "std")]
    #[test]
    fn finish_to_writes_the_same_bytes_as_finish() {
        // Kills the `finish_to -> Ok(())` mutant, which returns success without
        // writing anything: the sink must receive exactly `finish()`'s bytes.
        let canvas = Dimensions::new(2, 2).unwrap();
        let rgba = frame_bytes(canvas, 0xFF00_00FF);
        let make = || {
            AnimationEncoder::new(canvas)
                .add_frame(
                    ImageRef::new(canvas, PixelLayout::Rgba8, &rgba).unwrap(),
                    meta(canvas, 0, 0, 40),
                )
                .unwrap()
        };
        let expected = make().finish();
        assert!(!expected.is_empty());
        let mut buf = Vec::new();
        make().finish_to(&mut buf).unwrap();
        assert_eq!(buf, expected);
    }
}
