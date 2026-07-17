//! The `lossless` codec — a pure-Rust WebP **VP8L** (lossless) codec.
//!
//! This crate decodes and encodes the VP8L bitstream used inside WebP files. It
//! forbids `unsafe`, has zero required runtime dependencies, and can build for
//! `no_std` targets (with `alloc`).
//!
//! # Status
//!
//! The codec is developed test-first against the conformance fixtures (see the
//! `webpkit-lossless-conformance` crate and `cargo xtask conformance`). [`decode`] is a
//! complete, bit-exact VP8L decoder (all transforms, meta-Huffman, color cache,
//! LZ77) that also reads the extended (`VP8X`) container and its ICC/Exif/XMP
//! metadata; [`encode`] emits practical VP8L (LZ77 + color cache chosen by an
//! entropy cost model) that both this decoder and libwebp's `dwebp` round-trip.
//!
//! # Public API
//!
//! - Decode: [`decode`] / [`decode_with`] / [`decode_rgba`] (and, with `std`,
//!   [`decode_reader`] and [`Decoder`]) return an [`Image`]; [`IncrementalDecoder`]
//!   accepts pushed bytes and reports [`Progress`].
//! - Animation: [`decode_frames`] returns a lazy [`Frames`] iterator; each
//!   [`Frame`] carries [`FrameMeta`], and [`Frames::composited`] paints frames
//!   onto the canvas honoring [`BlendMode`]/[`DisposalMode`]. Build animations
//!   with the type-state [`crate::AnimationEncoder`].
//! - Encode: [`encode`] / [`encode_to`] take an [`ImageRef`] plus an
//!   [`EncoderConfig`] (effort [`Effort`] and [`Metadata`] to embed);
//!   [`encode_image`] takes a full [`Image`] and preserves its [`Metadata`] by
//!   default (kinder than `cwebp`, which strips it).
//! - Types: [`Dimensions`], [`PixelLayout`], [`Metadata`], [`DecodeOptions`],
//!   [`ImageInfo`].
//!
//! A lossy `VP8` *still* image is rejected with [`Error::UnsupportedFeature`]. An
//! animation's frames may themselves be lossy when a both-codecs [`FrameDecoder`]
//! is supplied via [`crate::anim::decode_frames_with_decoder`] (the umbrella
//! `webpkit` crate does this); the default [`Vp8lFrameDecoder`] rejects a lossy
//! frame the same way.
//! Passing an animation to [`decode`] returns its first composited frame.
//!
//! # Features
//!
//! - `std` *(default)* — enables `std`-backed conveniences and the
//!   [`std::error::Error`] impl on [`Error`].
//! - `alloc` — build for `no_std` targets that provide an allocator.
//! - `rayon` — opt-in encoder parallelism.
//! - `oracle` — **dev/test only.** Links `libwebp-sys` so differential tests can
//!   cross-check against the reference implementation. Never enable in
//!   production builds.

use crate::lossless::prelude::*;
// The bitstream-agnostic shell (RIFF/VP8X container, image model, error type)
// lives in the core shell, shared with the `lossy` (VP8) codec. `image` and
// `container` are pulled in as module paths for the encode/decode glue below.
use crate::{container, image};

pub(crate) mod animation;
pub(crate) mod bit_io;
pub(crate) mod color_cache;
pub(crate) mod constants;
pub(crate) mod decoder;
pub(crate) mod encoder;
pub(crate) mod histogram;
pub(crate) mod huffman;
pub(crate) mod lz77;
pub(crate) mod prelude;
pub(crate) mod transform;
pub(crate) mod vp8l;
pub(crate) mod work;

/// Dev-only thin shims exposing internal numeric kernels to `webpkit-bench`.
///
/// The `kernels` microbench times each kernel against its pre-optimization
/// `*_reference` twin in one back-to-back criterion run (see
/// `docs/benchmarking.md`). The kernels stay `pub(crate)` — a `pub(crate)` item
/// cannot be re-exported outside the crate, so these forwarders build the tile from
/// plain channel arrays (the `TilePixel` struct is not public) and call the private
/// kernel. **Not** part of the stable public API: gated behind the dev-only `bench`
/// feature, which no production build enables.
#[cfg(feature = "bench")]
pub mod bench {
    /// Time-in-isolation shim for the channel-gathering `sweep_blue`.
    #[inline]
    #[must_use]
    pub fn sweep_blue(
        green: &[i8],
        red: &[u8],
        blue_base: &[i32],
        sweep_red: bool,
        channel: &mut alloc::vec::Vec<i8>,
    ) -> i8 {
        crate::lossless::transform::cross_color::bench_sweep_blue(
            green, red, blue_base, sweep_red, channel,
        )
    }

    /// Time-in-isolation shim for the pre-scratch `sweep_blue_reference`.
    #[inline]
    #[must_use]
    pub fn sweep_blue_reference(
        green: &[i8],
        red: &[u8],
        blue_base: &[i32],
        sweep_red: bool,
    ) -> i8 {
        crate::lossless::transform::cross_color::bench_sweep_blue_reference(
            green, red, blue_base, sweep_red,
        )
    }

    /// Time-in-isolation shim for the run-chunked cross-color `inverse_row`.
    #[inline]
    pub fn cross_color_inverse_row(row: &mut [u32], y: usize, bits: u32, tile_data: &[u32]) {
        crate::lossless::transform::cross_color::inverse_row(row, y, bits, tile_data);
    }

    /// Time-in-isolation shim for the pre-hoist `inverse_row_reference`.
    #[inline]
    pub fn cross_color_inverse_row_reference(
        row: &mut [u32],
        y: usize,
        bits: u32,
        tile_data: &[u32],
    ) {
        crate::lossless::transform::cross_color::inverse_row_reference(row, y, bits, tile_data);
    }
}

pub use crate::{Codec, Dimensions, Effort, Error, Image, ImageRef, Metadata, PixelLayout, Result};
pub use animation::{
    AnimInfo, BlendMode, CompositedFrame, CompositedFrames, DisposalMode, Frame, FrameMeta, Frames,
    Vp8lFrameDecoder, decode_frames, decode_frames_with,
};
#[cfg(feature = "std")]
pub use decoder::Decoder;
pub use decoder::{
    DecodeOptions, DecodedFrame, FrameDecoder, FramePayload, ImageInfo, IncrementalDecoder,
    Progress, RowDrain, decode_vp8l,
};
pub use encoder::{EncoderConfig, MetadataPolicy};

/// Decode a WebP lossless (VP8L) file into an [`Image`] (RGBA8 by default).
///
/// # Errors
///
/// An [`Error`] if the input is not a WebP file, uses an unsupported feature
/// (animation / lossy), has no VP8L image, or is a malformed bitstream.
pub fn decode(input: &[u8]) -> Result<Image> {
    decoder::decode_image(input, &DecodeOptions::default())
}

/// Decode a WebP lossless file into an [`Image`] with explicit [`DecodeOptions`]
/// (output layout, pixel limit, metadata).
///
/// # Errors
///
/// The same errors as [`decode`], plus [`Error::LimitExceeded`] when
/// `options.max_pixels` is exceeded.
pub fn decode_with(input: &[u8], options: &DecodeOptions) -> Result<Image> {
    decoder::decode_image(input, options)
}

/// Decode a WebP lossless file to raw RGBA8 pixels and its [`Dimensions`].
///
/// A thin convenience over [`decode`] for callers that only want the bytes.
///
/// # Errors
///
/// The same errors as [`decode`].
pub fn decode_rgba(input: &[u8]) -> Result<(Dimensions, Vec<u8>)> {
    let image = decode(input)?;
    Ok((image.dimensions(), image.into_pixels()))
}

/// Decode the lossless (VP8L) alpha image stream of an `ALPH` chunk.
///
/// Takes the bytes after the chunk's 1-byte header and returns a `width*height`
/// alpha plane that is still spatially filtered (the caller un-filters). Used by
/// the umbrella `webp` crate to composite alpha onto a lossy image.
///
/// # Errors
///
/// Bitstream/truncation errors from the VP8L decoder.
pub fn decode_alpha(payload: &[u8], width: u32, height: u32) -> Result<Vec<u8>> {
    decode_alpha_with(payload, width, height, &DecodeOptions::default())
}

/// Decode the lossless (VP8L) alpha stream of an `ALPH` chunk with explicit
/// [`DecodeOptions`].
///
/// Enforces `options.max_pixels` against the enclosing frame's `width * height`
/// *before* the alpha plane is allocated (symmetric with [`decode_with`], so the
/// umbrella can propagate a decode limit into alpha compositing). The output is the
/// still-filtered `width*height` plane.
///
/// # Errors
///
/// The same errors as [`decode_alpha`], plus [`Error::LimitExceeded`] when
/// `width * height` exceeds `options.max_pixels`.
pub fn decode_alpha_with(
    payload: &[u8],
    width: u32,
    height: u32,
    options: &DecodeOptions,
) -> Result<Vec<u8>> {
    let pixels = u64::from(width) * u64::from(height);
    if let Some(limit) = options.max_pixels.filter(|&limit| pixels > limit) {
        return Err(Error::LimitExceeded { pixels, limit });
    }
    vp8l::decode::decode_alpha_stream(payload, width, height)
}

/// Encode an alpha plane as the lossless (`VP8L`) body of an `ALPH` chunk.
///
/// Takes a `width * height` alpha-byte plane (already spatially filtered by the
/// caller) and returns a HEADERLESS level-0 VP8L stream — the bytes that follow an
/// `ALPH` chunk's 1-byte header when its `method` is lossless. The plane rides in
/// the green lane, exactly the layout [`decode_alpha`] reads back. Used by the
/// `lossy` codec to compress a lossy image's alpha channel.
#[must_use]
pub fn encode_alpha(alpha: &[u8], width: u32, height: u32) -> Vec<u8> {
    vp8l::encode::encode_alpha_stream(alpha, width, height)
}

/// Decode a WebP lossless file read from `reader` into an [`Image`].
///
/// # Errors
///
/// [`Error::Io`] on a read failure, or any [`decode`] error.
#[cfg(feature = "std")]
pub fn decode_reader<R: std::io::Read>(reader: R) -> Result<Image> {
    Decoder::new(reader).decode()
}

/// Encode an [`ImageRef`] into a complete WebP VP8L (lossless) file.
///
/// The output is a bare `RIFF`/`WEBP`/`VP8L` file, or an extended `VP8X` file
/// when `config` carries metadata to embed.
///
/// # Errors
///
/// This is infallible for a valid [`ImageRef`] today, but returns [`Result`] so
/// future encoder options can report failures without a breaking change.
#[expect(
    clippy::unnecessary_wraps,
    reason = "the Result is a deliberate, stable part of the public API so future \
              encoder options can fail without a breaking signature change"
)]
pub fn encode(image: ImageRef<'_>, config: &EncoderConfig) -> Result<Vec<u8>> {
    let mut argb = image::unpack_pixels(image.layout(), image.as_bytes());
    let dims = image.dimensions();
    // Near-lossless is an encode-side ARGB filter applied before the transform
    // search; the resulting pixels are still coded exactly, so alpha-used is read
    // from the (possibly quantized) pixels the encoder actually emits.
    if let Some(level) = config.near_lossless {
        transform::near_lossless::apply(&mut argb, dims.width(), dims.height(), level);
    }
    let has_alpha = image::argb_has_alpha(&argb);
    let payload = encoder::encode_payload(config.effort, dims.width(), dims.height(), &argb);
    Ok(container::writer::wrap(
        &payload,
        dims,
        &config.metadata,
        has_alpha,
    ))
}

/// Encode an [`Image`] into a complete WebP VP8L file.
///
/// It **preserves the image's ICC/Exif/XMP [`Metadata`] by default** — kinder
/// than `cwebp`, whose default strips it.
///
/// The metadata-aware counterpart to [`encode`] (which takes a bare [`ImageRef`]
/// with no metadata and embeds only what `config` carries). The effective
/// metadata is resolved per field, in descending precedence: (1) an explicit
/// value from [`EncoderConfig::with_metadata`]; (2) the image's own metadata,
/// gated by the config's [`MetadataPolicy`] (ICC is inherited under every policy;
/// [`MetadataPolicy::StripPrivate`] drops Exif/XMP); (3) nothing.
///
/// ICC can be *replaced* by `config` but never silently dropped, so a decode →
/// `encode_image` round trip never loses color-correctness.
///
/// # Errors
///
/// The same as [`encode`].
pub fn encode_image(image: &Image, config: &EncoderConfig) -> Result<Vec<u8>> {
    let effective = EncoderConfig {
        effort: config.effort,
        metadata: config.resolve_metadata(image.metadata()),
        policy: config.policy,
        near_lossless: config.near_lossless,
    };
    encode(image.as_image_ref(), &effective)
}

/// Encode an [`ImageRef`] and write the WebP file to `writer`.
///
/// # Errors
///
/// [`Error::Io`] on a write failure, or any [`encode`] error.
#[cfg(feature = "std")]
pub fn encode_to<W: std::io::Write>(
    image: ImageRef<'_>,
    config: &EncoderConfig,
    mut writer: W,
) -> Result<()> {
    let bytes = encode(image, config)?;
    writer.write_all(&bytes)?;
    Ok(())
}

/// Differential hook for the `oracle` integration test (dev-only, hidden): assert
/// the suspend/resume VP8L decoder ([`crate::lossless::vp8l::decode_incr`]) reproduces the
/// one-shot [`decode`] of `webp`'s VP8L payload over every canonical input split.
///
/// It is exposed only under the dev-only `oracle` feature so the (separate,
/// `unsafe`-permitting) oracle test crate can drive the crate-internal streaming
/// decoder over libwebp-authored payloads — which use predictor + cross-color
/// transforms our own encoder does not emit — without leaking the internals into
/// the real public API. Returns `true` when streaming and one-shot agree on both
/// the decoded pixels and the error outcome.
#[cfg(feature = "oracle")]
#[doc(hidden)]
#[must_use]
pub fn __vp8l_stream_equals_one_shot(webp: &[u8]) -> bool {
    let Ok(parsed) = container::reader::parse_container(webp, false) else {
        return false;
    };
    let payload = parsed.vp8l;
    let one_shot = vp8l::decode::decode(payload).map(|d| (d.width, d.height, d.argb));
    vp8l::decode_incr::split_patterns(payload.len())
        .into_iter()
        .all(|splits| {
            let streamed = vp8l::decode_incr::stream_over_splits(payload, &splits)
                .map(|(w, h, _a, px)| (w, h, px));
            match (&one_shot, &streamed) {
                (Ok(a), Ok(b)) => a == b,
                (Err(_), Err(_)) => true,
                _ => false,
            }
        })
}

#[cfg(test)]
mod tests {
    use super::{
        DecodeOptions, Dimensions, Effort, EncoderConfig, Error, Image, ImageRef, Metadata,
        MetadataPolicy, PixelLayout, decode, decode_alpha, decode_alpha_with, decode_rgba,
        decode_with, encode, encode_alpha, encode_image,
    };

    /// Encode RGBA bytes and assert an exact RGBA round-trip.
    fn round_trip(width: u32, height: u32, rgba: &[u8]) {
        let dims = Dimensions::new(width, height).unwrap();
        let img = ImageRef::new(dims, PixelLayout::Rgba8, rgba).unwrap();
        let file = encode(img, &EncoderConfig::default()).unwrap();
        assert_eq!(decode_rgba(&file).unwrap(), (dims, rgba.to_vec()));
    }

    #[test]
    fn decode_rejects_non_webp_input() {
        assert_eq!(decode(&[]).unwrap_err(), Error::Truncated);
        assert_eq!(
            decode(b"not a webp file at all").unwrap_err(),
            Error::NotWebp
        );
    }

    #[test]
    fn round_trips_a_1x1_pixel() {
        round_trip(1, 1, &[10, 20, 30, 40]);
    }

    #[test]
    fn round_trips_a_solid_block() {
        let rgba: Vec<u8> = [12u8, 34, 56, 78]
            .iter()
            .copied()
            .cycle()
            .take(6 * 4)
            .collect();
        round_trip(3, 2, &rgba);
    }

    #[test]
    fn round_trips_a_small_gradient() {
        let (width, height) = (4u32, 3u32);
        let mut rgba = Vec::new();
        for y in 0..height {
            for x in 0..width {
                let red = u8::try_from(x * 40).unwrap();
                let green = u8::try_from(y * 60).unwrap();
                let blue = u8::try_from((x + y) * 20).unwrap();
                rgba.extend_from_slice(&[red, green, blue, 255]);
            }
        }
        round_trip(width, height, &rgba);
    }

    #[test]
    fn image_ref_rejects_length_mismatch() {
        let dims = Dimensions::new(2, 2).unwrap();
        assert_eq!(
            ImageRef::new(dims, PixelLayout::Rgba8, &[0u8; 15]).unwrap_err(),
            Error::PixelBufferMismatch
        );
    }

    #[test]
    fn round_trips_a_non_rgba_layout() {
        // BGRA input, decoded back as BGRA, must reproduce the bytes exactly.
        let bgra = vec![30u8, 20, 10, 255, 60, 50, 40, 200];
        let dims = Dimensions::new(2, 1).unwrap();
        let img = ImageRef::new(dims, PixelLayout::Bgra8, &bgra).unwrap();
        let file = encode(img, &EncoderConfig::default()).unwrap();
        let opts = DecodeOptions::default().layout(PixelLayout::Bgra8);
        assert_eq!(decode_with(&file, &opts).unwrap().as_bytes(), &bgra[..]);
    }

    #[test]
    fn round_trips_metadata_via_vp8x() {
        let rgba = vec![1u8, 2, 3, 255, 4, 5, 6, 255];
        let dims = Dimensions::new(2, 1).unwrap();
        let metadata = Metadata {
            icc_profile: Some(vec![9, 8, 7]),
            xmp: Some(vec![b'<', b'x', b'>']),
            ..Metadata::none()
        };
        let config = EncoderConfig::new().with_metadata(metadata.clone());
        let img = ImageRef::new(dims, PixelLayout::Rgba8, &rgba).unwrap();
        let file = encode(img, &config).unwrap();
        let decoded = decode(&file).unwrap();
        assert_eq!(decoded.metadata(), &metadata);
        assert_eq!(decoded.as_bytes(), &rgba[..]);
    }

    #[test]
    fn encode_image_preserves_metadata_by_default() {
        // The headline anti-silent-drop test: a decode -> encode_image round trip
        // (default config) keeps all three sidecars AND the pixels.
        let rgba = vec![1u8, 2, 3, 255, 4, 5, 6, 255];
        let dims = Dimensions::new(2, 1).unwrap();
        let metadata = Metadata {
            icc_profile: Some(vec![9, 8, 7]),
            exif: Some(vec![1, 1, 2, 3]),
            xmp: Some(vec![b'<', b'x', b'>']),
        };
        let img = Image::from_parts(
            dims,
            PixelLayout::Rgba8,
            rgba.clone(),
            false,
            metadata.clone(),
        );
        let file = encode_image(&img, &EncoderConfig::default()).unwrap();
        let decoded = decode(&file).unwrap();
        assert_eq!(decoded.metadata(), &metadata);
        assert_eq!(decoded.as_bytes(), &rgba[..]);
    }

    #[test]
    fn encode_image_strip_private_keeps_icc_only() {
        let rgba = vec![1u8, 2, 3, 255, 4, 5, 6, 255];
        let dims = Dimensions::new(2, 1).unwrap();
        let metadata = Metadata {
            icc_profile: Some(vec![9, 8, 7]),
            exif: Some(vec![4, 5, 6]),
            xmp: Some(vec![b'<', b'x', b'>']),
        };
        let img = Image::from_parts(dims, PixelLayout::Rgba8, rgba, false, metadata);
        let config = EncoderConfig::default().with_metadata_policy(MetadataPolicy::StripPrivate);
        let decoded = decode(&encode_image(&img, &config).unwrap()).unwrap();
        assert_eq!(
            decoded.metadata().icc_profile.as_deref(),
            Some(&[9, 8, 7][..])
        );
        assert_eq!(decoded.metadata().exif, None);
        assert_eq!(decoded.metadata().xmp, None);
    }

    #[test]
    fn encode_image_config_override_wins() {
        let rgba = vec![1u8, 2, 3, 255, 4, 5, 6, 255];
        let dims = Dimensions::new(2, 1).unwrap();
        let image_meta = Metadata {
            exif: Some(vec![1]),
            ..Metadata::none()
        };
        let img = Image::from_parts(dims, PixelLayout::Rgba8, rgba, false, image_meta);
        let config = EncoderConfig::default().with_metadata(Metadata {
            exif: Some(vec![2]),
            ..Metadata::none()
        });
        let decoded = decode(&encode_image(&img, &config).unwrap()).unwrap();
        assert_eq!(decoded.metadata().exif.as_deref(), Some(&[2][..]));
    }

    #[test]
    fn encode_image_no_metadata_is_byte_identical_to_encode() {
        // Byte-invariance canary: with no metadata, encode_image must produce the
        // exact same bytes as encode, and a bare VP8L file (no VP8X upgrade).
        let rgba = vec![10u8, 20, 30, 255, 40, 50, 60, 255];
        let dims = Dimensions::new(2, 1).unwrap();
        let img = Image::from_parts(
            dims,
            PixelLayout::Rgba8,
            rgba.clone(),
            false,
            Metadata::none(),
        );
        let via_image = encode_image(&img, &EncoderConfig::default()).unwrap();
        let via_ref = encode(
            ImageRef::new(dims, PixelLayout::Rgba8, &rgba).unwrap(),
            &EncoderConfig::default(),
        )
        .unwrap();
        assert_eq!(via_image, via_ref);
        // RIFF/WEBP/VP8L, no VP8X.
        assert_eq!(&via_image[8..12], b"WEBP");
        assert_eq!(&via_image[12..16], b"VP8L");
        assert!(!via_image.windows(4).any(|w| w == b"VP8X"));
    }

    #[test]
    fn decode_alpha_with_rejects_before_alloc() {
        // A huge declared alpha plane (16384x16384) must be rejected by the pixel
        // limit *before* the plane is allocated, so the payload is never touched.
        let opts = DecodeOptions::default().max_pixels(1024);
        assert_eq!(
            decode_alpha_with(&[0u8; 4], 16384, 16384, &opts).unwrap_err(),
            Error::LimitExceeded {
                pixels: 16384 * 16384,
                limit: 1024,
            }
        );
    }

    #[test]
    fn near_lossless_shrinks_a_photo_and_stays_within_bound() {
        // A photographic sample (box-blurred noise) has busy detail near-lossless can
        // quantize; a plain gradient is already smooth and would not shrink. At 64px
        // the small-image guard is cleared, so the pass actually runs.
        use crate::lossless::transform::near_lossless;
        let sample = webpkit_samples::render(webpkit_samples::Content::Photo, 64);
        let dims = Dimensions::new(sample.edge, sample.edge).unwrap();
        let img = ImageRef::new(dims, PixelLayout::Rgba8, &sample.rgba).unwrap();

        let plain = encode(img, &EncoderConfig::default()).unwrap();
        let level = 20;
        let near = encode(
            img,
            &EncoderConfig::default().with_near_lossless(level),
        )
        .unwrap();
        assert!(
            near.len() < plain.len(),
            "near-lossless ({}) must beat plain lossless ({})",
            near.len(),
            plain.len()
        );

        // The decoded pixels must stay within the theoretical per-channel bound of
        // the original — near-lossless is bounded-error, not arbitrary.
        let decoded = decode_rgba(&near).unwrap().1;
        let bound = near_lossless::error_bound(level);
        for (src, dec) in sample.rgba.chunks_exact(4).zip(decoded.chunks_exact(4)) {
            for k in 0..4 {
                assert!(
                    u32::from(src[k].abs_diff(dec[k])) <= bound,
                    "channel {k}: |{} - {}| exceeds bound {bound}",
                    src[k],
                    dec[k]
                );
            }
        }
    }

    #[test]
    fn near_lossless_disabled_is_byte_identical_to_plain() {
        // A None near-lossless config (and level 100, the disabled sentinel) must
        // produce exactly the plain-lossless bytes: the pass is opt-in and inert off.
        let sample = webpkit_samples::render(webpkit_samples::Content::Photo, 64);
        let dims = Dimensions::new(sample.edge, sample.edge).unwrap();
        let img = ImageRef::new(dims, PixelLayout::Rgba8, &sample.rgba).unwrap();
        let plain = encode(img, &EncoderConfig::default()).unwrap();
        let off = encode(img, &EncoderConfig::default().with_near_lossless(100)).unwrap();
        assert_eq!(plain, off);
    }

    #[test]
    fn fast_effort_still_round_trips() {
        let rgba: Vec<u8> = (0..16u8)
            .flat_map(|v| [v, v.wrapping_mul(3), v.wrapping_mul(7), 255])
            .collect();
        let dims = Dimensions::new(4, 4).unwrap();
        let img = ImageRef::new(dims, PixelLayout::Rgba8, &rgba).unwrap();
        let file = encode(img, &EncoderConfig::new().with_effort(Effort::Fast)).unwrap();
        assert_eq!(decode_rgba(&file).unwrap(), (dims, rgba));
    }

    #[test]
    fn encode_alpha_round_trips_through_decode_alpha() {
        // A concrete plane that equals none of the constant-return mutants
        // (`vec![]`, `vec![0]`, `vec![1]`) for either `encode_alpha` or
        // `decode_alpha`: encode to a headerless VP8L stream and read it back.
        let plane = vec![10u8, 90, 200, 5];
        let payload = encode_alpha(&plane, 2, 2);
        assert_eq!(decode_alpha(&payload, 2, 2).unwrap(), plane);
    }

    #[test]
    fn decode_alpha_with_allows_exactly_the_limit() {
        // Boundary for `pixels > limit`: with `width*height == max_pixels` the plane
        // must decode (the `>` is not `>=`). A `>=` mutant would reject it here.
        let plane = vec![10u8, 90, 200, 5];
        let payload = encode_alpha(&plane, 2, 2);
        let opts = DecodeOptions::default().max_pixels(4);
        assert_eq!(decode_alpha_with(&payload, 2, 2, &opts).unwrap(), plane);
    }

    #[cfg(feature = "std")]
    #[test]
    fn encode_to_writes_the_encode_bytes() {
        // The writer path must emit exactly what `encode` returns; an `Ok(())` that
        // skips `write_all` would leave the buffer empty.
        let rgba = vec![10u8, 20, 30, 255, 40, 50, 60, 255];
        let dims = Dimensions::new(2, 1).unwrap();
        let cfg = EncoderConfig::default();
        let mut buf = Vec::new();
        super::encode_to(
            ImageRef::new(dims, PixelLayout::Rgba8, &rgba).unwrap(),
            &cfg,
            &mut buf,
        )
        .unwrap();
        let direct = encode(
            ImageRef::new(dims, PixelLayout::Rgba8, &rgba).unwrap(),
            &cfg,
        )
        .unwrap();
        assert!(!buf.is_empty());
        assert_eq!(buf, direct);
    }

    #[cfg(feature = "bench")]
    #[test]
    fn bench_sweep_blue_shims_return_the_kernel_result() {
        // green channel == 32 == 2^5, so delta(m, 32) == m for every m; with
        // blue_base == 20 the unique cost-zero multiplier is m == 20 (not in the
        // {-1, 0, 1} constant-return mutant set). Both shims must forward it.
        let green = [32i8];
        let red = [0u8];
        let blue_base = [20i32];
        let mut channel = alloc::vec::Vec::new();
        assert_eq!(
            super::bench::sweep_blue(&green, &red, &blue_base, false, &mut channel),
            20
        );
        assert_eq!(
            super::bench::sweep_blue_reference(&green, &red, &blue_base, false),
            20
        );
    }

    #[cfg(feature = "bench")]
    #[test]
    fn bench_cross_color_inverse_row_shims_apply_the_transform() {
        // A single-pixel row under tile code 0x0010_0020 (green_to_red=32,
        // red_to_blue=16) must transform 0xFFF0_4000 -> 0xFF30_4018. A shim replaced
        // by `()` would leave the row untouched.
        let expected = [0xFF30_4018u32];
        let mut row = [0xFFF0_4000u32];
        super::bench::cross_color_inverse_row(&mut row, 0, 0, &[0x0010_0020]);
        assert_eq!(row, expected);
        let mut row_ref = [0xFFF0_4000u32];
        super::bench::cross_color_inverse_row_reference(&mut row_ref, 0, 0, &[0x0010_0020]);
        assert_eq!(row_ref, expected);
    }

    #[cfg(feature = "oracle")]
    #[test]
    fn stream_equals_one_shot_true_on_valid_webp() {
        // A real encode round-trips identically streamed and one-shot: the (Ok, Ok)
        // arm compares equal and the fn returns true. Kills `-> false`, the deleted
        // (Ok, Ok) arm, and `==` -> `!=` (which would flip every split to unequal).
        let rgba = vec![
            10u8, 20, 30, 255, 40, 50, 60, 255, 1, 2, 3, 255, 4, 5, 6, 255,
        ];
        let dims = Dimensions::new(2, 2).unwrap();
        let img = ImageRef::new(dims, PixelLayout::Rgba8, &rgba).unwrap();
        let file = encode(img, &EncoderConfig::default()).unwrap();
        assert!(super::__vp8l_stream_equals_one_shot(&file));
    }

    #[cfg(feature = "oracle")]
    #[test]
    fn stream_equals_one_shot_false_on_non_webp() {
        // Container parse fails -> false. Kills the `-> true` constant.
        assert!(!super::__vp8l_stream_equals_one_shot(
            b"definitely not a webp file"
        ));
    }

    #[cfg(feature = "oracle")]
    #[test]
    fn stream_equals_one_shot_true_when_both_paths_error() {
        // Valid container but a clobbered VP8L signature: both one-shot and streaming
        // decode return Err on every split, so the (Err, Err) arm must yield true.
        // Deleting that arm would route these to `_ => false` and return false.
        let rgba = vec![10u8, 20, 30, 255, 40, 50, 60, 255];
        let dims = Dimensions::new(2, 1).unwrap();
        let img = ImageRef::new(dims, PixelLayout::Rgba8, &rgba).unwrap();
        let mut file = encode(img, &EncoderConfig::default()).unwrap();
        let vp8l = file.windows(4).position(|w| w == b"VP8L").unwrap();
        let sig = vp8l + 8; // 4-byte fourcc + 4-byte chunk size -> payload byte 0
        assert_ne!(file[sig], 0x00); // it is the 0x2f signature
        file[sig] = 0x00; // -> InvalidBitstream in both decode paths
        assert!(super::__vp8l_stream_equals_one_shot(&file));
    }
}
