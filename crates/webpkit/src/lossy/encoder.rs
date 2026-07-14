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
use crate::lossy::alpha::compress_alpha;
use crate::lossy::frame;
use crate::lossy::prelude::*;
use crate::lossy::quant::quality_to_base_q;
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

/// The internal [`frame::Effort`] tier the shared [`Effort`] preset encodes at.
///
/// [`Effort::Fast`] disables the whole-block intra-mode search (fixing `DC_PRED`),
/// the coefficient-probability optimization, per-macroblock skip coding and the
/// in-loop deblocking filter — a frame byte-identical to an unfiltered,
/// default-table, DC-only encode. [`Effort::Balanced`] (the `Full` tier) enables
/// them all: the mode search (`DC`/`V`/`H`/`TM`), the optimized table (kept only
/// when it shrinks the frame), skip coding (libwebp `CalcSkipProba`) and deblocking
/// at a level that scales with the base quantizer. [`Effort::Best`] is the `Full`
/// tier plus the intra-4×4 luma search — the only difference, so `Balanced` stays
/// byte-identical.
const fn effort_tier(effort: Effort) -> frame::Effort {
    match effort {
        Effort::Fast => frame::Effort::Fast,
        Effort::Balanced => frame::Effort::Full,
        Effort::Best => frame::Effort::Best,
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
}

impl LossyConfig {
    /// Default configuration: [`Quality::DEFAULT`], [`Effort::Balanced`], no metadata.
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
    // ALPH chunk. Probe without allocating, and only materialize the plane when a
    // non-opaque pixel means an ALPH chunk will actually be written (the common
    // opaque case then allocates nothing here).
    let has_alpha = image::argb_has_alpha(argb);
    // The byte-identical fast path: nothing to carry beyond the opaque image.
    if !has_alpha && metadata.is_empty() {
        return wrap_vp8(&vp8);
    }
    let alph = has_alpha.then(|| {
        let alpha: Vec<u8> = argb.iter().map(|&p| (p >> 24) as u8).collect();
        compress_alpha(&alpha, dims.width(), dims.height())
    });
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
fn encode_vp8_argb(argb: &[u32], dims: Dimensions, config: &LossyConfig) -> Vec<u8> {
    let rgba = image::pack_pixels(PixelLayout::Rgba8, argb);
    let base_q = quality_to_base_q(config.quality.get());
    frame::encode_frame(
        &rgba,
        dims.width() as usize,
        dims.height() as usize,
        base_q,
        effort_tier(config.effort),
    )
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
        Effort, LossyConfig, MetadataPolicy, Quality, effort_tier, encode, encode_image, encode_vp8,
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
    fn effort_defaults_to_balanced_and_builds() {
        // Default is Balanced (the shared `Effort::default()`); the builder is
        // const, chainable and independent of the quality knob.
        assert_eq!(Effort::default(), Effort::Balanced);
        assert_eq!(LossyConfig::new().effort(), Effort::Balanced);
        assert_eq!(LossyConfig::default().effort(), Effort::Balanced);
        assert_eq!(
            LossyConfig::new().with_effort(Effort::Fast).effort(),
            Effort::Fast
        );
        let cfg = LossyConfig::new()
            .with_quality(40)
            .with_effort(Effort::Best);
        assert_eq!(cfg.effort(), Effort::Best);
        assert_eq!(cfg.quality().get(), 40);
    }

    #[test]
    fn efforts_map_to_their_frame_tiers() {
        // Fast drops to the Fast tier (fixed DC prediction, default probabilities, no
        // skip coding, no in-loop filter); Balanced maps to Full (all four whole-block
        // gates on); Best maps to the Best tier (Full plus the intra-4×4 luma search).
        use crate::lossy::frame::Effort as FrameEffort;
        assert_eq!(effort_tier(Effort::Fast), FrameEffort::Fast);
        assert_eq!(effort_tier(Effort::Balanced), FrameEffort::Full);
        assert_eq!(effort_tier(Effort::Best), FrameEffort::Best);
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
