//! The public lossy-encode API (the encoder counterpart of [`crate::lossy::decoder`]).
//!
//! [`encode`] turns an [`ImageRef`] into a complete lossy WebP file (a bare
//! `RIFF`/`WEBP`/`VP8 ` container, upgrading to the extended `VP8X` form only for
//! alpha or embedded metadata); [`encode_image`] is its metadata-aware counterpart
//! that inherits an [`Image`]'s sidecar [`Metadata`]; [`encode_vp8`] exposes the
//! raw `VP8 ` payload for callers that assemble their own container. The knobs are
//! [`Quality`], the effort [`Effort`], and the ICC/Exif/XMP [`Metadata`] to embed
//! (governed by a [`MetadataPolicy`]).

use crate::container::writer::{wrap_vp8, wrap_vp8_extended};
use crate::image::{self, Image, Metadata, PixelLayout};
use crate::lossy::frame;
use crate::lossy::prelude::*;
use crate::lossy::quant::quality_to_base_q;
use crate::lossy::tuning::LossyTuning;
use crate::{Dimensions, Effort, Error, ImageRef, Result};

/// The largest side a lossy VP8 frame can express (14-bit dimension fields).
const MAX_VP8_DIMENSION: u32 = 16383;

/// A validated encode quality in `0..=100` (higher = larger and closer to the
/// source). The default is `75`, matching libwebp's default.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Quality(u8);

impl Quality {
    /// The default quality, `75`.
    pub const DEFAULT: Self = Self(75);

    /// Build a quality, clamping values above `100` down to `100`.
    #[must_use]
    pub const fn new(q: u8) -> Self {
        Self(if q > 100 { 100 } else { q })
    }

    /// The quality value in `0..=100`.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

impl Default for Quality {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// The single validated lossy parameter surface: a [`Quality`] plus its
/// [`LossyTuning`] psychovisual/RD knobs.
///
/// This is the one place a lossy quality is validated (through the [`Quality`]
/// newtype) and paired with its tuning, so every consumer — the [`Encoder`](crate::Encoder)
/// terminal and the animation codec ([`AnimCodec::Lossy`](crate::AnimCodec)) — shares one
/// validation story. Build from [`LossyParams::new`] (a quality, near-best tuning) and
/// override with [`with_quality`](Self::with_quality) / [`with_tuning`](Self::with_tuning).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LossyParams {
    quality: Quality,
    tuning: LossyTuning,
}

impl LossyParams {
    /// Build params at `quality` (`0..=100`, clamped by [`Quality`]) with the near-best
    /// [`LossyTuning::default`] knobs.
    #[must_use]
    pub fn new(quality: u8) -> Self {
        Self {
            quality: Quality::new(quality),
            tuning: LossyTuning::default(),
        }
    }

    /// Set the quality (`0..=100`, clamped).
    #[must_use]
    pub const fn with_quality(mut self, quality: u8) -> Self {
        self.quality = Quality::new(quality);
        self
    }

    /// Set the [`LossyTuning`] knobs.
    #[must_use]
    pub const fn with_tuning(mut self, tuning: LossyTuning) -> Self {
        self.tuning = tuning;
        self
    }

    /// The validated [`Quality`].
    #[must_use]
    pub const fn quality(self) -> Quality {
        self.quality
    }

    /// The [`LossyTuning`] knobs.
    #[must_use]
    pub const fn tuning(self) -> LossyTuning {
        self.tuning
    }
}

impl Default for LossyParams {
    /// [`Quality::DEFAULT`] with the near-best [`LossyTuning::default`] knobs.
    fn default() -> Self {
        Self {
            quality: Quality::DEFAULT,
            tuning: LossyTuning::default(),
        }
    }
}

/// The internal [`frame::Effort`] tier the shared [`Effort`] preset encodes an
/// image of `pixels` at.
///
/// An explicit [`Effort::level`] maps onto the three frame tiers: low levels to
/// `Fast`, mid levels to `Full`, the top levels to `Best`. [`Effort::AUTO`] picks a
/// tier from the frame size — small frames can afford the exhaustive `Best` search,
/// mid frames take the `Full` search, and large frames drop to `Fast` to bound
/// encode cost. The byte output for a given resolved tier is unchanged:
///
/// `Fast` disables the whole-block intra-mode search (fixing `DC_PRED`), the
/// coefficient-probability optimization, per-macroblock skip coding and the in-loop
/// deblocking filter. `Full` enables them all: the mode search (`DC`/`V`/`H`/`TM`),
/// the optimized table (kept only when it shrinks the frame), skip coding (libwebp
/// `CalcSkipProba`) and deblocking. `Best` is `Full` plus the intra-4×4 luma search.
const fn effort_tier(effort: Effort, pixels: u64) -> frame::Effort {
    match effort.explicit_level() {
        Some(level) => tier_for_level(level),
        None => auto_tier(pixels),
    }
}

/// Map an explicit effort level (`0..=9`) onto a frame search tier.
const fn tier_for_level(level: u8) -> frame::Effort {
    match level {
        0..=2 => frame::Effort::Fast,
        3..=7 => frame::Effort::Full,
        _ => frame::Effort::Best,
    }
}

/// Choose a frame search tier for an [`Effort::AUTO`] frame of `pixels`: the smaller
/// the frame, the deeper the affordable search.
///
/// Up to ~1 MP the default runs the full [`Best`](frame::Effort::Best) search
/// (intra-4×4 + trellis + segments), which measures as a clean rate-distortion win
/// over `cwebp` default shaping — smaller at matched quality with the reconstruction
/// staying at or above `cwebp`. Between ~1 MP and ~16 MP it steps down to
/// [`Full`](frame::Effort::Full) (whole-block mode search + trellis + segments, no
/// per-macroblock intra-4×4), so a full-resolution photo still gets real
/// rate-distortion shaping rather than the round-to-nearest
/// [`Fast`](frame::Effort::Fast) path; only genuinely huge scans fall back to `Fast`
/// to bound encode time and working-set memory.
const fn auto_tier(pixels: u64) -> frame::Effort {
    if pixels <= 1 << 20 {
        frame::Effort::Best // <= ~1 MP: deepest search, incl. intra-4x4
    } else if pixels <= 1 << 24 {
        frame::Effort::Full // <= ~16 MP: mode search + trellis + segments
    } else {
        frame::Effort::Fast // huge scans: bound encode time / memory
    }
}

/// How [`encode_image`] treats the metadata inherited from the source [`Image`] —
/// re-exported from the core shell, the single home of the fold logic
/// ([`Metadata::resolve`]).
pub use crate::image::MetadataPolicy;

/// Configuration for [`encode`] and [`encode_image`].
///
/// Carries the [`Quality`], the effort [`Effort`], and any sidecar [`Metadata`]
/// to embed in an extended (`VP8X`) container (governed by a [`MetadataPolicy`]).
#[derive(Clone, Debug, Default)]
pub struct LossyConfig {
    quality: Quality,
    effort: Effort,
    metadata: Metadata,
    policy: MetadataPolicy,
    tuning: LossyTuning,
}

impl LossyConfig {
    /// Default configuration: [`Quality::DEFAULT`], [`Effort::AUTO`], no metadata, and
    /// the near-best [`LossyTuning::default`] knobs.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the encode quality (clamped to `0..=100`).
    #[must_use]
    pub const fn with_quality(mut self, quality: u8) -> Self {
        self.quality = Quality::new(quality);
        self
    }

    /// Set the effort [`Effort`].
    #[must_use]
    pub const fn with_effort(mut self, effort: Effort) -> Self {
        self.effort = effort;
        self
    }

    /// Set the metadata to embed (upgrades the output to a `VP8X` container).
    ///
    /// Under [`encode_image`] this is a per-field *override*: any field set here
    /// wins over the image's own metadata and survives even
    /// [`MetadataPolicy::StripPrivate`]. The policy in
    /// [`with_metadata_policy`](Self::with_metadata_policy) gates only the
    /// *inherited* image metadata, not what is set here.
    #[must_use]
    pub fn with_metadata(mut self, metadata: Metadata) -> Self {
        self.metadata = metadata;
        self
    }

    /// Set the [`MetadataPolicy`] consulted by [`encode_image`]. Ignored by
    /// [`encode`], which has no source image to inherit metadata from.
    ///
    /// The policy gates only the *inherited* image metadata; an explicit value set
    /// via [`with_metadata`](Self::with_metadata) still wins over it.
    #[must_use]
    pub const fn with_metadata_policy(mut self, policy: MetadataPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Set the psychovisual [`LossyTuning`] knobs (SNS strength, segment count, filter
    /// strength/sharpness, and the typed placeholders for the sharp-YUV / lossy-alpha /
    /// rate-control subsystems). Defaults to [`LossyTuning::default`], the near-best
    /// `cwebp`-parity baseline.
    #[must_use]
    pub const fn with_tuning(mut self, tuning: LossyTuning) -> Self {
        self.tuning = tuning;
        self
    }

    /// The configured quality.
    #[must_use]
    pub const fn quality(&self) -> Quality {
        self.quality
    }

    /// The configured effort [`Effort`].
    #[must_use]
    pub const fn effort(&self) -> Effort {
        self.effort
    }

    /// The configured sidecar [`Metadata`] override.
    #[must_use]
    pub const fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    /// The configured [`MetadataPolicy`].
    #[must_use]
    pub const fn policy(&self) -> MetadataPolicy {
        self.policy
    }

    /// The configured psychovisual [`LossyTuning`] knobs.
    #[must_use]
    pub const fn tuning(&self) -> &LossyTuning {
        &self.tuning
    }

    /// Fold the image's `inherited` metadata, this config's [`MetadataPolicy`], and
    /// any explicit [`with_metadata`](Self::with_metadata) override into the
    /// effective metadata to embed. Delegates to the shared [`Metadata::resolve`]
    /// fold (the config's own metadata is the per-field override).
    pub(crate) fn resolve_metadata(&self, inherited: &Metadata) -> Metadata {
        self.metadata.resolve(inherited, self.policy)
    }
}

/// Encode `image` into a complete lossy WebP file.
///
/// A fully-opaque image with no embedded metadata produces a bare
/// `RIFF`/`WEBP`/`VP8 ` container, byte-identical to an alpha-free encode. When any
/// pixel is non-opaque the alpha plane is carried **losslessly** in a sibling
/// `ALPH` chunk and the file upgrades to the extended (`VP8X` + `ALPH` + `VP8 `)
/// form. Any ICC/Exif/XMP [`Metadata`] set via [`LossyConfig::with_metadata`] is
/// emitted as `ICCP`/`EXIF`/`XMP ` chunks, also upgrading to the extended form.
///
/// This takes a bare [`ImageRef`], so there is no source image to inherit metadata
/// from; only the config's explicit metadata is embedded. Use [`encode_image`] to
/// preserve an [`Image`]'s own sidecar metadata.
///
/// # Errors
///
/// [`Error::InvalidDimensions`] if a side exceeds the lossy VP8 maximum of
/// `16383`. The signature is fallible so future options can report failures
/// without a breaking change.
pub fn encode(image: ImageRef<'_>, config: &LossyConfig) -> Result<Vec<u8>> {
    let dims = image.dimensions();
    check_dimensions(dims)?;
    let argb = image::unpack_pixels(image.layout(), image.as_bytes());
    // No source image to inherit from: only an explicit config override applies.
    let metadata = config.resolve_metadata(&Metadata::none());
    Ok(assemble(&argb, dims, config, &metadata))
}

/// Encode an [`Image`] into a complete lossy WebP file, **preserving its
/// ICC/Exif/XMP [`Metadata`] by default** — kinder than `cwebp`, whose default
/// strips it.
///
/// The metadata-aware counterpart to [`encode`] (which takes a bare [`ImageRef`]
/// with no metadata and embeds only what `config` carries). The effective metadata
/// is resolved per field, in descending precedence: (1) an explicit value from
/// [`LossyConfig::with_metadata`]; (2) the image's own metadata, gated by the
/// config's [`MetadataPolicy`] (ICC is inherited under every policy;
/// [`MetadataPolicy::StripPrivate`] drops Exif/XMP); (3) nothing.
///
/// ICC can be *replaced* by `config` but never silently dropped, so a decode →
/// `encode_image` round trip never loses color-correctness.
///
/// # Errors
///
/// The same as [`encode`].
pub fn encode_image(image: &Image, config: &LossyConfig) -> Result<Vec<u8>> {
    let dims = image.dimensions();
    check_dimensions(dims)?;
    let argb = image::unpack_pixels(image.layout(), image.as_bytes());
    let metadata = config.resolve_metadata(image.metadata());
    Ok(assemble(&argb, dims, config, &metadata))
}

/// Assemble the container from native-ARGB `argb`, the effective `metadata`, and
/// the alpha lane. The single seam both [`encode`] and [`encode_image`] share.
///
/// A fully-opaque frame with empty metadata stays a bare `VP8 ` file (byte-
/// identical to the pre-metadata encoder); alpha and/or metadata upgrade it to the
/// extended `VP8X` form, whose chunk order and flags [`wrap_vp8_extended`] sets.
fn assemble(argb: &[u32], dims: Dimensions, config: &LossyConfig, metadata: &Metadata) -> Vec<u8> {
    let vp8 = encode_vp8_argb(argb, dims, config);
    // Alpha is the top byte of each native-ARGB pixel; an all-opaque plane needs no
    // ALPH chunk. `alph_chunk` probes without allocating and only materializes the
    // plane when a non-opaque pixel means an ALPH chunk will actually be written.
    let alph = crate::lossy::alpha::alph_chunk(argb, dims, alpha_tuning(*config.tuning()));
    // The byte-identical fast path: nothing to carry beyond the opaque image.
    if alph.is_none() && metadata.is_empty() {
        return wrap_vp8(&vp8);
    }
    wrap_vp8_extended(&vp8, alph.as_deref(), dims, metadata)
}

/// Encode `image` into a raw `VP8 ` key-frame payload (no container), returning it
/// with the frame [`Dimensions`].
///
/// The low-level seam a container assembler uses; it carries no alpha (see
/// [`encode`] for the alpha-aware container).
///
/// # Errors
///
/// [`Error::InvalidDimensions`] if a side exceeds `16383`.
pub fn encode_vp8(image: ImageRef<'_>, config: &LossyConfig) -> Result<(Dimensions, Vec<u8>)> {
    let dims = image.dimensions();
    check_dimensions(dims)?;
    let argb = image::unpack_pixels(image.layout(), image.as_bytes());
    Ok((dims, encode_vp8_argb(&argb, dims, config)))
}

/// Encode already-unpacked native-ARGB `pixels` into a raw `VP8 ` key-frame
/// payload (the opaque RGB; alpha is handled separately by [`encode`]).
///
/// The crate-internal seam the animation encoder ([`crate::AnimationEncoder`]) uses
/// to encode a lossy `ANMF` frame's `VP8 ` sub-chunk.
pub(crate) fn encode_vp8_argb(argb: &[u32], dims: Dimensions, config: &LossyConfig) -> Vec<u8> {
    let tuning = *config.tuning();
    // `-exact` off (the default) preserves the RGB under fully-transparent pixels;
    // clearing it flattens those macroblocks for a smaller file. Borrow the source
    // untouched on the default path so the byte output is unchanged.
    let cleaned: Vec<u32>;
    let argb = if tuning.exact() {
        argb
    } else {
        cleaned = clear_hidden_rgb(argb);
        &cleaned
    };
    let rgba = image::pack_pixels(PixelLayout::Rgba8, argb);
    let base_q = biased_base_q(quality_to_base_q(config.quality.get()), tuning);
    let pixels = u64::from(dims.width()) * u64::from(dims.height());
    frame::encode_frame_tuned(
        &rgba,
        dims.width() as usize,
        dims.height() as usize,
        base_q,
        effort_tier(config.effort, pixels),
        frame_tuning(tuning),
    )
}

/// The largest VP8 base quantizer index (the 7-bit `y_ac_qi` field).
const MAX_BASE_Q: i32 = 127;
/// Divisor shaping the `jpeg_like` quality falloff: the base index is coarsened by a
/// fraction of its distance from the coarsest index, so a high-quality (fine) frame is
/// biased more than a low-quality one — a JPEG-like size curve.
const JPEG_LIKE_FALLOFF_DIV: i32 = 16;

/// Apply the RD/rate tuning knobs as a base-quantizer bias. Both neutral values
/// (`jpeg_like == false`, `partition_limit == 0`) return `base_q` unchanged, so the
/// default encode is byte-identical; a non-neutral value coarsens the quantizer (a
/// smaller file), and the emitted quant index keeps the stream self-consistent.
fn biased_base_q(base_q: i32, tuning: LossyTuning) -> i32 {
    let mut q = base_q;
    if tuning.jpeg_like() {
        // Ceil-divide the headroom so any frame finer than the coarsest index is biased.
        q += (MAX_BASE_Q - base_q + JPEG_LIKE_FALLOFF_DIV - 1) / JPEG_LIKE_FALLOFF_DIV;
    }
    let limit = i32::from(tuning.partition_limit());
    if limit > 0 {
        // A rate cap: coarsen proportionally to drop high-frequency coefficients.
        q += (limit * MAX_BASE_Q) / 100;
    }
    q.clamp(0, MAX_BASE_Q)
}

/// Clear the RGB channels of fully-transparent (`alpha == 0`) pixels, leaving opaque
/// and partially-transparent pixels untouched (`-exact` off). Flattening the hidden
/// RGB lets those macroblocks compress smaller; the alpha plane is unaffected.
fn clear_hidden_rgb(argb: &[u32]) -> Vec<u32> {
    argb.iter()
        .map(|&p| if p >> 24 == 0 { 0 } else { p })
        .collect()
}

/// Project the public [`LossyTuning`] onto the internal [`frame::FrameTuning`] the
/// frame encoder consumes (the active psychovisual knobs, the sharp-YUV chroma
/// strategy, and the multi-pass entropy-refinement count). The RD/rate knobs that bias
/// the base quantizer (`jpeg_like`/`partition_limit`) are folded in earlier by
/// [`biased_base_q`], and the alpha knobs ride their own [`alpha_tuning`] seam, so they
/// do not cross this one.
const fn frame_tuning(tuning: LossyTuning) -> frame::FrameTuning {
    frame::FrameTuning {
        sns_strength: tuning.sns_strength(),
        segments: tuning.segments(),
        filter_strength: tuning.filter_strength(),
        filter_sharpness: tuning.filter_sharpness(),
        sharp_yuv: tuning.sharp_yuv(),
        passes: tuning.pass(),
    }
}

/// Project the public [`LossyTuning`] onto the internal [`alpha::AlphaTuning`] the
/// `ALPH` search consumes (the level-quantization quality and stored-plane bounds).
/// The default tuning yields the always-lossless, exhaustive search — byte-identical
/// to the prior output.
const fn alpha_tuning(tuning: LossyTuning) -> crate::lossy::alpha::AlphaTuning {
    crate::lossy::alpha::AlphaTuning {
        quality: tuning.alpha_q(),
        method: tuning.alpha_method(),
        filter: tuning.alpha_filter(),
    }
}

/// Reject a frame whose either side exceeds the 14-bit VP8 dimension field.
const fn check_dimensions(dims: Dimensions) -> Result<()> {
    if dims.width() > MAX_VP8_DIMENSION || dims.height() > MAX_VP8_DIMENSION {
        Err(Error::InvalidDimensions)
    } else {
        Ok(())
    }
}

/// Encode `image` and write the lossy WebP file to `writer`.
///
/// # Errors
///
/// [`Error::Io`] on a write failure, or any [`encode`] error.
#[cfg(feature = "std")]
pub fn encode_to<W: std::io::Write>(
    image: ImageRef<'_>,
    config: &LossyConfig,
    mut writer: W,
) -> Result<()> {
    let bytes = encode(image, config)?;
    writer.write_all(&bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        Effort, LossyConfig, LossyParams, LossyTuning, MetadataPolicy, Quality, effort_tier,
        encode, encode_image, encode_vp8,
    };
    use crate::alpha::{AlphaCompression, parse_header, unfilter};
    use crate::container::fourcc::FourCc;
    use crate::container::reader::{ImageChunk, chunks, locate_image_with_alpha};
    use crate::container::writer::wrap_vp8;
    use crate::image::{Dimensions, Image, ImageRef, Metadata, PixelLayout};

    /// The bytes of the first `id` chunk in `file`, or `None` when it is absent —
    /// the reader-side inverse of the writer's ICCP/EXIF/XMP emission (our own
    /// walker, since `parse_container` rejects lossy `VP8 ` files).
    fn chunk_bytes(file: &[u8], id: FourCc) -> Option<Vec<u8>> {
        chunks(file)
            .unwrap()
            .filter_map(Result::ok)
            .find(|c| c.id == id)
            .map(|c| c.data.to_vec())
    }

    /// A tiny opaque RGBA image and its metadata fixture (odd-length ICC to also
    /// exercise the RIFF pad byte not being counted in the chunk size).
    fn meta_fixture() -> (Metadata, Vec<u8>, Dimensions) {
        let metadata = Metadata {
            icc_profile: Some(b"icc-bytes".to_vec()), // 9 bytes: odd -> pad
            exif: Some(b"exif-bytes".to_vec()),
            xmp: Some(b"<x:xmpmeta/>".to_vec()),
        };
        (metadata, solid(16, 16), Dimensions::new(16, 16).unwrap())
    }

    /// A pattern value narrowed into a byte (no lossy cast).
    fn byte(v: u32) -> u8 {
        u8::try_from(v & 0xff).unwrap_or(0)
    }

    /// A solid-color RGBA `ImageRef` buffer.
    fn solid(width: u32, height: u32) -> Vec<u8> {
        let mut buf = Vec::new();
        for _ in 0..width * height {
            buf.extend_from_slice(&[100, 150, 200, 255]);
        }
        buf
    }

    /// Recover the source alpha plane from an encoded file's `ALPH` chunk, the
    /// exact inverse the umbrella crate applies on decode (parse header,
    /// decompress raw/VP8L, un-filter).
    fn recover_alpha(file: &[u8], width: u32, height: u32) -> Vec<u8> {
        let located = locate_image_with_alpha(file).unwrap();
        let alph = located
            .alpha
            .expect("an alpha image must carry an ALPH chunk");
        let (w, h) = (width as usize, height as usize);
        let (header, data) = parse_header(alph).unwrap();
        let mut plane = match header.compression {
            AlphaCompression::None => data[..w * h].to_vec(),
            AlphaCompression::Lossless => {
                crate::lossless::decode_alpha(data, width, height).unwrap()
            },
        };
        unfilter(header.filter, &mut plane, w, h);
        plane
    }

    /// A `w`×`h` RGBA checkerboard of two saturated colors — high-frequency chroma edges,
    /// exactly the content sharp-YUV's luminance-guided subsampling changes.
    fn chroma_edges(w: u32, h: u32) -> Vec<u8> {
        let mut buf = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let px = if (x + y) % 2 == 0 {
                    [220, 30, 40, 255]
                } else {
                    [20, 200, 60, 255]
                };
                buf.extend_from_slice(&px);
            }
        }
        buf
    }

    #[test]
    fn sharp_yuv_defaults_off_and_is_byte_identical_to_explicit_off() {
        // The byte-stability contract: the default encode and an explicit sharp_yuv=false
        // are the same code path (plain box chroma), so they must be byte-for-byte equal —
        // and both equal the pre-P4 output (which the byte-stable goldens pin). Turning
        // sharp_yuv ON must actually change the bytes, proving the knob is wired, not inert.
        let dims = Dimensions::new(24, 24).unwrap();
        let pixels = chroma_edges(24, 24);
        let img = ImageRef::new(dims, PixelLayout::Rgba8, &pixels).unwrap();

        let default = encode_vp8(img, &LossyConfig::new()).unwrap().1;
        let explicit_off = encode_vp8(
            img,
            &LossyConfig::new().with_tuning(LossyTuning::new().with_sharp_yuv(false)),
        )
        .unwrap()
        .1;
        assert_eq!(
            default, explicit_off,
            "default must be byte-identical to an explicit sharp_yuv=false"
        );

        let sharp_on = encode_vp8(
            img,
            &LossyConfig::new().with_tuning(LossyTuning::new().with_sharp_yuv(true)),
        )
        .unwrap()
        .1;
        assert_ne!(
            default, sharp_on,
            "sharp_yuv=true must change the output on chroma edges (knob is live)"
        );
    }
    /// A deterministic `w`×`h` RGBA noise field (a splitmix walk over the RGB bytes) —
    /// AC-rich content whose many borderline coefficients let a refinement pass cross a
    /// rate-distortion decision boundary and shrink the frame.
    fn noise(w: u32, h: u32, seed: u64) -> Vec<u8> {
        let mut s = seed;
        let mut v = Vec::new();
        for _ in 0..w * h {
            for _ in 0..3 {
                s = s
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                v.push(byte(u32::try_from(s >> 33 & 0xff).unwrap_or(0)));
            }
            v.push(255);
        }
        v
    }

    #[test]
    fn multi_pass_is_byte_identical_at_one_and_refines_above_one() {
        // The byte-stability contract for `-pass`: `pass = 1` (the default) is the
        // single-pass encode, so it must equal an untuned default byte-for-byte; a
        // higher pass count re-plans against the converging probability model, which
        // must change the output on AC-rich content and — being self-consistent by
        // construction — still decode. A noise field at a moderate quality gives the
        // trellis / mode search enough borderline decisions for the refined table to
        // bite (seed 4 at q70 crosses a boundary and shrinks the frame).
        let dims = Dimensions::new(32, 32).unwrap();
        let pixels = noise(32, 32, 4);
        let img = ImageRef::new(dims, PixelLayout::Rgba8, &pixels).unwrap();
        let at = |pass: u8| {
            encode_vp8(
                img,
                &LossyConfig::new()
                    .with_quality(70)
                    .with_tuning(LossyTuning::new().with_pass(pass)),
            )
            .unwrap()
            .1
        };
        let default = encode_vp8(img, &LossyConfig::new().with_quality(70))
            .unwrap()
            .1;
        assert_eq!(
            default,
            at(1),
            "pass=1 must be byte-identical to the default"
        );

        let refined = at(4);
        assert_ne!(default, refined, "pass>1 must refine (change) the output");
        assert!(
            refined.len() <= default.len(),
            "a refinement pass must never grow the file: {} vs {}",
            refined.len(),
            default.len()
        );
        assert!(
            crate::lossy::decode(&refined).is_ok(),
            "the multi-pass stream must still decode"
        );
    }

    #[test]
    fn lossy_params_validates_quality_and_carries_tuning() {
        // The single validation story: quality flows through the `Quality` newtype, and
        // the tuning rides alongside it.
        assert_eq!(
            LossyParams::new(200).quality().get(),
            100,
            "quality clamps to 100"
        );
        let params = LossyParams::new(60)
            .with_tuning(LossyTuning::new().with_jpeg_like(true))
            .with_quality(70);
        assert_eq!(params.quality().get(), 70);
        assert!(params.tuning().jpeg_like());
        assert_eq!(LossyParams::default().quality().get(), 75);
        assert_eq!(LossyParams::default().tuning(), LossyTuning::default());
    }

    #[test]
    fn rd_knobs_default_off_and_change_output_when_set() {
        // Byte-stability contract for the RD knobs: an explicit *neutral* tuning is the
        // same code path as the default, so it must be byte-for-byte equal; a non-neutral
        // `jpeg_like` / `partition_limit` biases the quantizer and must change the output,
        // and the biased stream must still decode (it stays self-consistent).
        let dims = Dimensions::new(24, 24).unwrap();
        let pixels = chroma_edges(24, 24);
        let img = ImageRef::new(dims, PixelLayout::Rgba8, &pixels).unwrap();
        let at = |t: LossyTuning| {
            encode_vp8(img, &LossyConfig::new().with_quality(80).with_tuning(t))
                .unwrap()
                .1
        };
        let base = at(LossyTuning::new());
        let neutral = LossyTuning::new()
            .with_jpeg_like(false)
            .with_partition_limit(0)
            .with_exact(true);
        assert_eq!(base, at(neutral), "neutral RD knobs must be byte-identical");

        for tuning in [
            LossyTuning::new().with_jpeg_like(true),
            LossyTuning::new().with_partition_limit(50),
        ] {
            let bytes = at(tuning);
            assert_ne!(base, bytes, "a non-neutral RD knob must change the output");
            assert!(
                crate::lossy::decode(&bytes).is_ok(),
                "the biased stream must still decode"
            );
        }
    }

    #[test]
    fn exact_defaults_to_preserve_and_clearing_changes_output() {
        // A checkerboard of fully-transparent (alpha 0) pixels with non-zero hidden RGB
        // and opaque pixels. `exact` (the default) preserves that hidden RGB, so it is
        // byte-identical; clearing it (`exact=false`) zeroes those channels and must
        // change the VP8 stream, which must still decode.
        let (w, h) = (16u32, 16u32);
        let mut pixels = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let a = if (x + y) % 2 == 0 { 0 } else { 255 };
                pixels.extend_from_slice(&[byte(x * 13), byte(y * 7), 200, a]);
            }
        }
        let dims = Dimensions::new(w, h).unwrap();
        let img = ImageRef::new(dims, PixelLayout::Rgba8, &pixels).unwrap();
        let at = |t: LossyTuning| {
            encode_vp8(img, &LossyConfig::new().with_quality(85).with_tuning(t))
                .unwrap()
                .1
        };
        let default = at(LossyTuning::new());
        assert_eq!(
            default,
            at(LossyTuning::new().with_exact(true)),
            "exact=true is the default (byte-identical)"
        );
        let cleared = at(LossyTuning::new().with_exact(false));
        assert_ne!(
            default, cleared,
            "clearing hidden RGB (exact=false) must change the VP8 output"
        );
        assert!(crate::lossy::decode(&cleared).is_ok());
    }

    #[test]
    fn opaque_image_stays_a_bare_vp8_container() {
        // A fully-opaque image must encode to the exact bytes of an alpha-free
        // encode: a bare RIFF/WEBP/VP8 file with no VP8X or ALPH chunk.
        let dims = Dimensions::new(24, 16).unwrap();
        let pixels = solid(24, 16);
        let img = ImageRef::new(dims, PixelLayout::Rgba8, &pixels).unwrap();
        let cfg = LossyConfig::new();
        let file = encode(img, &cfg).unwrap();
        let (_dims, payload) = encode_vp8(img, &cfg).unwrap();
        assert_eq!(
            file,
            wrap_vp8(&payload),
            "opaque encode must be byte-identical"
        );
        assert_eq!(&file[12..16], b"VP8 ");
        assert!(!file.windows(4).any(|w| w == b"VP8X"));
        assert!(!file.windows(4).any(|w| w == b"ALPH"));
    }

    #[test]
    fn non_opaque_image_upgrades_to_extended_and_alpha_round_trips_byte_exact() {
        // A diagonal alpha gradient with fully-transparent and fully-opaque corners.
        // Alpha is LOSSLESS: the recovered plane must equal the source byte-for-byte.
        let (w, h) = (20u32, 16u32);
        let mut pixels = Vec::new();
        let mut source_alpha = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let a = byte(((x + y) * 255) / (w + h - 2));
                source_alpha.push(a);
                pixels.extend_from_slice(&[byte(x * 12), byte(y * 15), 128, a]);
            }
        }
        let dims = Dimensions::new(w, h).unwrap();
        let img = ImageRef::new(dims, PixelLayout::Rgba8, &pixels).unwrap();
        let file = encode(img, &LossyConfig::new().with_quality(90)).unwrap();
        // Extended container: VP8X + ALPH + VP8.
        assert_eq!(&file[12..16], b"VP8X");
        assert!(file.windows(4).any(|c| c == b"ALPH"));
        match locate_image_with_alpha(&file).unwrap().image {
            ImageChunk::Lossy(_) => {},
            ImageChunk::Lossless(_) => panic!("expected a lossy VP8 image"),
        }
        assert_eq!(recover_alpha(&file, w, h), source_alpha);
    }

    #[test]
    fn encode_is_deterministic_with_alpha() {
        let (w, h) = (12u32, 12u32);
        let mut pixels = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let a = byte(x * y);
                pixels.extend_from_slice(&[byte(x), byte(y), 64, a]);
            }
        }
        let dims = Dimensions::new(w, h).unwrap();
        let img = ImageRef::new(dims, PixelLayout::Rgba8, &pixels).unwrap();
        let cfg = LossyConfig::new();
        assert_eq!(encode(img, &cfg).unwrap(), encode(img, &cfg).unwrap());
    }

    #[test]
    fn quality_clamps_and_defaults() {
        assert_eq!(Quality::default().get(), 75);
        assert_eq!(Quality::new(200).get(), 100);
        assert_eq!(Quality::new(0).get(), 0);
        assert_eq!(LossyConfig::new().quality().get(), 75);
        assert_eq!(LossyConfig::new().with_quality(150).quality().get(), 100);
    }

    #[test]
    fn effort_defaults_to_auto_and_builds() {
        // Default is AUTO (the shared `Effort::default()`); the builder is const,
        // chainable and independent of the quality knob.
        assert_eq!(Effort::default(), Effort::AUTO);
        assert_eq!(LossyConfig::new().effort(), Effort::AUTO);
        assert_eq!(LossyConfig::default().effort(), Effort::AUTO);
        assert_eq!(
            LossyConfig::new().with_effort(Effort::level(0)).effort(),
            Effort::level(0)
        );
        let cfg = LossyConfig::new()
            .with_quality(40)
            .with_effort(Effort::level(9));
        assert_eq!(cfg.effort(), Effort::level(9));
        assert_eq!(cfg.quality().get(), 40);
    }

    #[test]
    fn efforts_map_to_their_frame_tiers() {
        // Explicit levels bucket onto the three frame tiers (low -> Fast, mid ->
        // Full, top -> Best); AUTO picks a tier by frame size (small -> Best, mid ->
        // Full, large -> Fast). Each tier: Fast fixes DC prediction with no filter,
        // Full turns the four whole-block gates on, Best adds the intra-4×4 search.
        use crate::lossy::frame::Effort as FrameEffort;
        let px = 16 * 16;
        assert_eq!(effort_tier(Effort::level(0), px), FrameEffort::Fast);
        assert_eq!(effort_tier(Effort::level(2), px), FrameEffort::Fast);
        assert_eq!(effort_tier(Effort::level(3), px), FrameEffort::Full);
        assert_eq!(effort_tier(Effort::level(7), px), FrameEffort::Full);
        assert_eq!(effort_tier(Effort::level(9), px), FrameEffort::Best);
        // AUTO by size: up to ~1 MP earns the deepest Best search, a full-resolution
        // photo (~1-16 MP) earns Full, and only a genuinely huge scan falls to Fast.
        assert_eq!(effort_tier(Effort::AUTO, 64 * 64), FrameEffort::Best);
        assert_eq!(effort_tier(Effort::AUTO, 512 * 512), FrameEffort::Best);
        assert_eq!(effort_tier(Effort::AUTO, 1024 * 1024), FrameEffort::Best);
        assert_eq!(effort_tier(Effort::AUTO, 4000 * 4000), FrameEffort::Full);
        assert_eq!(effort_tier(Effort::AUTO, 6000 * 6000), FrameEffort::Fast);
    }

    #[test]
    fn encode_produces_a_decodable_lossy_webp() {
        let dims = Dimensions::new(24, 16).unwrap();
        let pixels = solid(24, 16);
        let img = ImageRef::new(dims, PixelLayout::Rgba8, &pixels).unwrap();
        let file = encode(img, &LossyConfig::new()).unwrap();
        // A RIFF/WEBP/VP8 container.
        assert_eq!(&file[0..4], b"RIFF");
        assert_eq!(&file[8..12], b"WEBP");
        assert_eq!(&file[12..16], b"VP8 ");
        // Round-trips through the umbrella-free decoder path (the raw payload).
        let (_dims, payload) = encode_vp8(img, &LossyConfig::new()).unwrap();
        let decoded = crate::lossy::decode(&payload).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (24, 16));
    }

    #[test]
    fn rejects_oversized_dimensions() {
        // 16384 exceeds the 14-bit VP8 dimension field; construction is valid but
        // lossy encode must reject it.
        let dims = Dimensions::new(16384, 1).unwrap();
        let pixels = vec![0u8; 16384 * 4];
        let img = ImageRef::new(dims, PixelLayout::Rgba8, &pixels).unwrap();
        assert!(encode_vp8(img, &LossyConfig::new()).is_err());
    }

    #[test]
    fn honors_non_rgba_layouts() {
        // A BGRA source must convert correctly: encode then decode and confirm the
        // dimensions (self-consistency of the color path is covered in `frame`).
        let dims = Dimensions::new(16, 16).unwrap();
        let mut pixels = Vec::new();
        for _ in 0..16 * 16 {
            pixels.extend_from_slice(&[200, 150, 100, 255]); // B, G, R, A
        }
        let img = ImageRef::new(dims, PixelLayout::Bgra8, &pixels).unwrap();
        let (_dims, payload) = encode_vp8(img, &LossyConfig::new()).unwrap();
        let decoded = crate::lossy::decode(&payload).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (16, 16));
    }

    #[test]
    fn metadata_free_opaque_encode_is_byte_identical_to_bare_vp8() {
        // Byte-invariance canary: with no metadata and no alpha, `encode` must be
        // byte-for-byte a bare `VP8 ` file (no `VP8X` container wrapping).
        let dims = Dimensions::new(24, 16).unwrap();
        let pixels = solid(24, 16);
        let img = ImageRef::new(dims, PixelLayout::Rgba8, &pixels).unwrap();
        let cfg = LossyConfig::new();
        let file = encode(img, &cfg).unwrap();
        let (_dims, payload) = encode_vp8(img, &cfg).unwrap();
        assert_eq!(
            file,
            wrap_vp8(&payload),
            "opaque metadata-free encode must be bare VP8"
        );
        assert!(!file.windows(4).any(|w| w == b"VP8X"));
    }

    #[test]
    fn config_metadata_upgrades_opaque_to_vp8x_and_survives_byte_exact() {
        // ICC + Exif + XMP on an OPAQUE image: no ALPH, but the file upgrades to the
        // extended form and every metadata chunk round-trips byte-exact.
        let (metadata, pixels, dims) = meta_fixture();
        let img = ImageRef::new(dims, PixelLayout::Rgba8, &pixels).unwrap();
        let cfg = LossyConfig::new().with_metadata(metadata.clone());
        let file = encode(img, &cfg).unwrap();
        assert_eq!(
            &file[12..16],
            b"VP8X",
            "metadata must force the extended form"
        );
        assert!(
            !file.windows(4).any(|w| w == b"ALPH"),
            "opaque: no ALPH chunk"
        );
        assert_eq!(chunk_bytes(&file, FourCc::ICCP), metadata.icc_profile);
        assert_eq!(chunk_bytes(&file, FourCc::EXIF), metadata.exif);
        assert_eq!(chunk_bytes(&file, FourCc::XMP), metadata.xmp);
        // The VP8X flags advertise exactly the chunks present.
        let vp8x = locate_image_with_alpha(&file).unwrap().vp8x.unwrap();
        assert!(vp8x.flags.has_icc() && vp8x.flags.has_exif() && vp8x.flags.has_xmp());
        assert!(!vp8x.flags.has_alpha());
        // The image still decodes to the right shape.
        let decoded = crate::lossy::decode(vp8_of(&file)).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (16, 16));
    }

    #[test]
    fn encode_image_preserves_metadata_by_default_with_alpha() {
        // A decode -> encode_image round trip (default Preserve) keeps all three
        // sidecars AND emits the ALPH chunk for the non-opaque image: the full
        // VP8X + ICCP + ALPH + VP8 + EXIF + XMP file.
        let (metadata, _opaque, dims) = meta_fixture();
        // A non-trivial alpha ramp so the ALPH chunk is genuinely present.
        let mut pixels = Vec::new();
        for y in 0..16u32 {
            for x in 0..16u32 {
                pixels.extend_from_slice(&[byte(x * 9), byte(y * 11), 100, byte((x + y) * 8)]);
            }
        }
        let img = Image::from_parts(dims, PixelLayout::Rgba8, pixels, true, metadata.clone());
        let file = encode_image(&img, &LossyConfig::new().with_quality(90)).unwrap();
        assert_eq!(&file[12..16], b"VP8X");
        assert!(file.windows(4).any(|w| w == b"ALPH"), "must carry ALPH");
        assert_eq!(chunk_bytes(&file, FourCc::ICCP), metadata.icc_profile);
        assert_eq!(chunk_bytes(&file, FourCc::EXIF), metadata.exif);
        assert_eq!(chunk_bytes(&file, FourCc::XMP), metadata.xmp);
    }

    #[test]
    fn encode_image_strip_private_keeps_icc_only() {
        let (metadata, pixels, dims) = meta_fixture();
        let img = Image::from_parts(dims, PixelLayout::Rgba8, pixels, false, metadata.clone());
        let cfg = LossyConfig::new().with_metadata_policy(MetadataPolicy::StripPrivate);
        let file = encode_image(&img, &cfg).unwrap();
        assert_eq!(chunk_bytes(&file, FourCc::ICCP), metadata.icc_profile);
        assert_eq!(chunk_bytes(&file, FourCc::EXIF), None);
        assert_eq!(chunk_bytes(&file, FourCc::XMP), None);
    }

    #[test]
    fn encode_image_no_metadata_is_byte_identical_to_bare_vp8() {
        // Byte-invariance: an opaque metadata-free `Image` through `encode_image`
        // produces the exact bytes of the bare-VP8 `encode` path.
        let dims = Dimensions::new(16, 16).unwrap();
        let pixels = solid(16, 16);
        let img = Image::from_parts(
            dims,
            PixelLayout::Rgba8,
            pixels.clone(),
            false,
            Metadata::none(),
        );
        let via_image = encode_image(&img, &LossyConfig::new()).unwrap();
        let via_ref = encode(
            ImageRef::new(dims, PixelLayout::Rgba8, &pixels).unwrap(),
            &LossyConfig::new(),
        )
        .unwrap();
        assert_eq!(via_image, via_ref);
        assert_eq!(&via_image[12..16], b"VP8 ");
        assert!(!via_image.windows(4).any(|w| w == b"VP8X"));
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
        let config = LossyConfig::new()
            .with_metadata(override_meta.clone())
            .with_metadata_policy(MetadataPolicy::StripPrivate);
        assert_eq!(
            config.resolve_metadata(&inherited),
            override_meta.resolve(&inherited, MetadataPolicy::StripPrivate),
        );
    }

    #[test]
    fn metadata_encode_is_deterministic() {
        let (metadata, pixels, dims) = meta_fixture();
        let img = ImageRef::new(dims, PixelLayout::Rgba8, &pixels).unwrap();
        let cfg = LossyConfig::new().with_metadata(metadata);
        assert_eq!(encode(img, &cfg).unwrap(), encode(img, &cfg).unwrap());
    }

    #[test]
    fn check_dimensions_accepts_the_exact_maximum_and_rejects_one_over() {
        // The 14-bit VP8 dimension field maxes out at 16383, which must be VALID on
        // either side (the guard is `> MAX`, not `>= MAX`); 16384 is the first
        // rejected value. Pins both boundary comparisons in `check_dimensions`
        // (`> -> >=` would reject 16383; `> -> ==` would accept 16384).
        assert!(
            super::check_dimensions(Dimensions::new(16383, 1).unwrap()).is_ok(),
            "max width is valid"
        );
        assert!(
            super::check_dimensions(Dimensions::new(1, 16383).unwrap()).is_ok(),
            "max height is valid"
        );
        assert!(
            super::check_dimensions(Dimensions::new(16383, 16383).unwrap()).is_ok(),
            "max square is valid"
        );
        assert!(
            super::check_dimensions(Dimensions::new(16384, 1).unwrap()).is_err(),
            "one past max width is rejected"
        );
        assert!(
            super::check_dimensions(Dimensions::new(1, 16384).unwrap()).is_err(),
            "one past max height is rejected"
        );
    }

    #[cfg(feature = "std")]
    #[test]
    fn encode_to_writes_the_same_bytes_as_encode() {
        // `encode_to` must funnel the exact `encode` bytes into the writer; a body
        // replaced by `Ok(())` writes nothing, leaving the buffer empty.
        let dims = Dimensions::new(16, 16).unwrap();
        let pixels = solid(16, 16);
        let img = ImageRef::new(dims, PixelLayout::Rgba8, &pixels).unwrap();
        let cfg = LossyConfig::new();
        let mut buf = Vec::new();
        super::encode_to(img, &cfg, &mut buf).unwrap();
        assert_eq!(
            buf,
            encode(img, &cfg).unwrap(),
            "encode_to must write exactly the encode() bytes"
        );
        assert!(!buf.is_empty(), "encode_to must not write an empty buffer");
    }

    /// The raw `VP8 ` payload of an (extended) lossy file.
    fn vp8_of(file: &[u8]) -> &[u8] {
        match locate_image_with_alpha(file).unwrap().image {
            ImageChunk::Lossy(payload) => payload,
            ImageChunk::Lossless(_) => panic!("expected a lossy VP8 image"),
        }
    }
}
