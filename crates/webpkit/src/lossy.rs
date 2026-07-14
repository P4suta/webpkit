//! The `lossy` codec — a pure-Rust WebP **VP8** (lossy) codec.
//!
//! This crate decodes and encodes the VP8 bitstream carried in a WebP `VP8 `
//! chunk. It is the lossy sibling of the `lossless` (VP8L, lossless) codec and shares
//! the container and image model with it through the crate's shared shell. Like its
//! sibling it forbids `unsafe`, has zero required runtime dependencies, and targets
//! `no_std` (with `alloc`).
//!
//! # Decoder
//!
//! The full VP8 **key-frame** decode pipeline is implemented and, being the only
//! frame kind a still WebP carries, decodes any lossy WebP image end to end:
//!
//! 1. `bool_dec` — the boolean (range) arithmetic decoder (RFC 6386 §7)
//! 2. `frame_header` — segment / filter / quantizer / token-partition headers
//! 3. `predict` — intra prediction (4×4 / 16×16 luma, 8×8 chroma)
//! 4. `idct` — dequantization + inverse DCT / WHT
//! 5. `loop_filter` — the simple and normal in-loop filters
//! 6. `yuv` — YUV 4:2:0 → RGBA conversion into an [`Image`]
//!
//! Each stage is verified bit-exact against the libwebp C reference (the `oracle`
//! differential path) rather than against this crate's own output: our
//! reconstructed YUV planes equal `WebPDecodeYUV` and our RGBA equals
//! `WebPDecodeRGBA` across a content × size × quality matrix. Inter (non-key)
//! frames do not occur in the WebP still-image format and are out of scope.
//!
//! # Encoder
//!
//! [`encode`] produces a valid `VP8 ` key frame from an [`ImageRef`] at a chosen
//! [`Quality`] and effort [`Effort`]. [`Effort::Fast`] uses fixed 16×16 DC
//! prediction, the default coefficient probabilities, round-to-nearest
//! quantization, a single segment and no loop filter. [`Effort::Balanced`] (the
//! default) and [`Effort::Best`] add rate-distortion whole-block intra mode search
//! (DC/V/H/TM), coefficient-probability optimization, per-macroblock skip coding,
//! the in-loop deblocking filter (a frame-final pass at a level scaled to the
//! quantizer), trellis (rate-distortion) quantization, and segmentation
//! (complexity-clustered per-segment quantizers). [`Effort::Best`] additionally
//! searches 4×4 (`B_PRED`) intra prediction. Because the encoder reconstructs —
//! and filters — with the very same transforms and loop filter the decoder uses,
//! `decode` of its output is byte-identical to its own reconstruction
//! (self-consistency), and it is independently validated to be readable by
//! libwebp's `dwebp`. Non-opaque images carry a lossless `ALPH` alpha plane, and
//! ICC/Exif/XMP [`Metadata`] is embedded via the extended `VP8X` container
//! ([`encode_image`] preserves a source [`Image`]'s metadata by default).

pub(crate) mod alpha;
pub(crate) mod bool_dec;
pub(crate) mod bool_enc;
pub(crate) mod constants;
pub(crate) mod decode;
pub(crate) mod decode_incr;
pub(crate) mod decoder;
pub(crate) mod enc_header;
pub(crate) mod encoder;
pub(crate) mod fdct;
pub(crate) mod frame;
pub(crate) mod frame_header;
pub(crate) mod header;
pub(crate) mod idct;
pub(crate) mod loop_filter;
pub(crate) mod mb;
pub(crate) mod predict;
pub(crate) mod prelude;
pub(crate) mod prob_opt;
pub(crate) mod quant;
pub(crate) mod reconstruct;
pub(crate) mod rgb_to_yuv;
pub(crate) mod token;
pub(crate) mod tokens;
pub(crate) mod trellis;
pub(crate) mod work;
pub(crate) mod yuv;

/// Dev-only thin shims exposing internal numeric kernels to `webpkit-bench`.
///
/// The `kernels` microbench times each kernel against its pre-optimization
/// `*_reference` twin in the same criterion run — the optimized kernel and its
/// reference measured back-to-back, which is how the low-noise A/B avoids
/// criterion's cold-baseline bias (see `docs/benchmarking.md`). The kernels stay
/// `pub(crate)` (a `pub(crate)` item cannot be re-exported outside the crate, and
/// marking them `pub` would trip `unreachable_pub` in every real build); these
/// `#[inline]` forwarders let the bench reach them while the compiler still inlines
/// the kernel body, so the microbench times the kernel, not the shim. **Not** part
/// of the stable public API: gated behind the dev-only `bench` feature, which no
/// production build enables.
#[cfg(feature = "bench")]
pub mod bench {
    /// Time-in-isolation shim for the autovectorized `crate::lossy::frame::sse_block`.
    #[inline]
    #[must_use]
    pub fn sse_block(
        src: &[u8],
        src_off: usize,
        src_stride: usize,
        pred: &[u8],
        pred_off: usize,
        pred_stride: usize,
        size: usize,
    ) -> i64 {
        crate::lossy::frame::sse_block(src, src_off, src_stride, pred, pred_off, pred_stride, size)
    }

    /// Time-in-isolation shim for the flat reference `crate::lossy::frame::sse_block_reference`.
    #[inline]
    #[must_use]
    pub fn sse_block_reference(
        src: &[u8],
        src_off: usize,
        src_stride: usize,
        pred: &[u8],
        pred_off: usize,
        pred_stride: usize,
        size: usize,
    ) -> i64 {
        crate::lossy::frame::sse_block_reference(
            src,
            src_off,
            src_stride,
            pred,
            pred_off,
            pred_stride,
            size,
        )
    }

    /// Time-in-isolation shim for the slice-mapped `crate::lossy::frame::residual_block`.
    #[inline]
    #[must_use]
    pub fn residual_block(
        src: &[u8],
        src_stride: usize,
        src_x: usize,
        src_y: usize,
        pred: &[u8],
        pred_off: usize,
        pred_stride: usize,
    ) -> [i16; 16] {
        crate::lossy::frame::residual_block(
            src,
            src_stride,
            src_x,
            src_y,
            pred,
            pred_off,
            pred_stride,
        )
    }

    /// Time-in-isolation shim for the flat reference `crate::lossy::frame::residual_block_reference`.
    #[inline]
    #[must_use]
    pub fn residual_block_reference(
        src: &[u8],
        src_stride: usize,
        src_x: usize,
        src_y: usize,
        pred: &[u8],
        pred_off: usize,
        pred_stride: usize,
    ) -> [i16; 16] {
        crate::lossy::frame::residual_block_reference(
            src,
            src_stride,
            src_x,
            src_y,
            pred,
            pred_off,
            pred_stride,
        )
    }

    /// Time-in-isolation shim for the gathered-top `crate::lossy::predict::true_motion`.
    #[inline]
    pub fn true_motion(plane: &mut [u8], off: usize, stride: usize, size: usize) {
        crate::lossy::predict::true_motion(plane, off, stride, size);
    }

    /// Time-in-isolation shim for the in-place `crate::lossy::predict::true_motion_reference`.
    #[inline]
    pub fn true_motion_reference(plane: &mut [u8], off: usize, stride: usize, size: usize) {
        crate::lossy::predict::true_motion_reference(plane, off, stride, size);
    }
}

pub use crate::stream::{DecodeOptions, ImageInfo, Progress, RowDrain};
pub use crate::{Codec, Dimensions, Effort, Error, Image, ImageRef, Metadata, PixelLayout, Result};
pub use decoder::IncrementalDecoder;
#[cfg(feature = "std")]
pub use encoder::encode_to;
pub use encoder::{LossyConfig, MetadataPolicy, Quality, encode, encode_image, encode_vp8};
pub use frame_header::FrameHeader;

use crate::lossy::prelude::*;

/// Read the pixel dimensions of a VP8 key-frame bitstream `payload` (the raw
/// contents of a WebP `VP8 ` chunk) without decoding any pixels.
///
/// # Errors
///
/// [`Error::Truncated`] if the header is short, or [`Error::InvalidBitstream`]
/// for a non-key-frame, a bad start code, or out-of-range dimensions.
pub fn peek_dimensions(payload: &[u8]) -> Result<Dimensions> {
    let header = FrameHeader::parse_key_frame(payload)?;
    Dimensions::new(u32::from(header.width), u32::from(header.height)).map_err(|_| {
        Error::InvalidBitstream {
            codec: Codec::Lossy,
        }
    })
}

/// Decode a WebP lossy `VP8` key-frame bitstream `payload` (the raw contents of
/// a WebP `VP8 ` chunk) into an [`Image`].
///
/// Runs the whole pipeline — control-partition parse (headers, intra modes,
/// residual coefficients), intra prediction, inverse DCT/WHT, the in-loop filter
/// and YUV 4:2:0 → RGBA — producing pixels byte-identical to the libwebp
/// reference.
///
/// # Errors
///
/// [`Error::Truncated`] or [`Error::InvalidBitstream`] for a malformed
/// bitstream.
pub fn decode(payload: &[u8]) -> Result<Image> {
    decode_with(payload, &DecodeOptions::default())
}

/// Decode a WebP lossy `VP8` key-frame `payload` into an [`Image`] with explicit
/// [`DecodeOptions`] (output layout, pixel limit).
///
/// Symmetric with [`crate::lossless::decode_with`]: the header dimensions are
/// checked against `options.max_pixels` *before* the reconstruction planes are
/// allocated (in `decode`), so a hostile header cannot exhaust memory. A default
/// [`DecodeOptions`] caps at [`DEFAULT_MAX_PIXELS`](crate::DEFAULT_MAX_PIXELS);
/// call [`DecodeOptions::unbounded`] to lift it for trusted input.
///
/// # Errors
///
/// The same errors as `decode`, plus [`Error::LimitExceeded`] when
/// `options.max_pixels` is exceeded.
pub fn decode_with(payload: &[u8], options: &DecodeOptions) -> Result<Image> {
    check_pixel_limit(payload, options)?;
    let image = decode::decode_frame(payload)?;
    Ok(repack(image, options.layout))
}

/// Decode a baseline VP8 key-frame to native ARGB pixels.
///
/// Returns the frame's dimensions and native `0xAARRGGBB` pixels (alpha `0xff`; a
/// sibling `ALPH` chunk is composited by the umbrella `webp` crate). This is the
/// form the animation compositor consumes.
///
/// # Errors
///
/// The same bitstream errors as `decode`.
pub fn decode_argb(payload: &[u8]) -> Result<(Dimensions, Vec<u32>)> {
    decode_argb_with(payload, &DecodeOptions::default())
}

/// Decode a baseline VP8 key-frame to native ARGB pixels with explicit
/// [`DecodeOptions`].
///
/// Checks `options.max_pixels` before the planes are allocated. The ARGB output is
/// always native `0xAARRGGBB`, so `options.layout` is not consulted (use
/// [`decode_with`] for a byte layout).
///
/// # Errors
///
/// The same errors as [`decode_argb`], plus [`Error::LimitExceeded`] when
/// `options.max_pixels` is exceeded.
pub fn decode_argb_with(payload: &[u8], options: &DecodeOptions) -> Result<(Dimensions, Vec<u32>)> {
    check_pixel_limit(payload, options)?;
    let image = decode::decode_frame(payload)?;
    let argb = crate::image::unpack_pixels(PixelLayout::Rgba8, image.as_bytes());
    Ok((image.dimensions(), argb))
}

/// Reject a key frame whose header dimensions exceed `options.max_pixels` *before*
/// any reconstruction buffer is sized to it — mirroring the `lossless` codec's
/// pre-allocation guard. A cheap header peek (no pixel decode) drives the check.
fn check_pixel_limit(payload: &[u8], options: &DecodeOptions) -> Result<()> {
    let pixels = peek_dimensions(payload)?.pixel_count();
    if let Some(limit) = options.max_pixels.filter(|&limit| pixels > limit) {
        return Err(Error::LimitExceeded { pixels, limit });
    }
    Ok(())
}

/// Repack a freshly decoded (RGBA8) key-frame [`Image`] into `options.layout`
/// (identity for `Rgba8`). A bare `VP8 ` still is opaque and carries no metadata.
fn repack(image: Image, layout: PixelLayout) -> Image {
    if layout == PixelLayout::Rgba8 {
        return image;
    }
    let dims = image.dimensions();
    let has_alpha = image.has_alpha();
    let argb = crate::image::unpack_pixels(PixelLayout::Rgba8, image.as_bytes());
    let bytes = crate::image::pack_pixels(layout, &argb);
    Image::from_parts(dims, layout, bytes, has_alpha, Metadata::none())
}

/// The crate version, as reported by Cargo.
#[must_use]
pub const fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// `(width, height, Y, U, V)`: a reconstructed YUV 4:2:0 frame (Level-A oracle).
#[cfg(feature = "oracle")]
#[doc(hidden)]
pub type YuvPlanes = (u32, u32, Vec<u8>, Vec<u8>, Vec<u8>);

/// Reconstruct `payload` to cropped YUV 4:2:0 planes `(width, height, y, u, v)`,
/// stopping before YUV→RGB. This is the differential oracle's **Level A** hook:
/// VP8 reconstruction is bit-exact per RFC 6386, so these planes must equal
/// libwebp's `WebPDecodeYUV`, isolating reconstruction (boolean decode, intra
/// prediction, IDCT, loop filter) from the color-conversion choice. Compiled
/// only under the `oracle` feature and hidden from docs — not public API, and
/// the in-crate FFI-free (`#![forbid(unsafe_code)]`) test crate cannot itself
/// link libwebp, so the check lives in the integration oracle. Mirrors the `lossless` codec's
/// `__vp8l_stream_equals_one_shot` hook.
#[cfg(feature = "oracle")]
#[doc(hidden)]
#[must_use]
pub fn __reconstruct_yuv(payload: &[u8]) -> Option<YuvPlanes> {
    let (planes, w, h) = decode::reconstruct_to_planes(payload).ok()?;
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    Some((
        u32::try_from(w).ok()?,
        u32::try_from(h).ok()?,
        planes.crop_y(w, h),
        planes.crop_u(cw, ch),
        planes.crop_v(cw, ch),
    ))
}

/// Whether the VP8 key frame in `payload` codes per-macroblock skip
/// (`proba.use_skip`); `None` for a malformed stream. Oracle-only and hidden —
/// not public API — it lets the differential oracle prove a skip test is
/// non-vacuous (the stream truly exercises the per-macroblock skip decode path).
#[cfg(feature = "oracle")]
#[doc(hidden)]
#[must_use]
pub fn __frame_uses_skip(payload: &[u8]) -> Option<bool> {
    decode::frame_uses_skip(payload)
}

/// The in-loop filter level coded in the VP8 key frame in `payload` (`0` disables
/// filtering); `None` for a malformed stream. Oracle-only and hidden — not public
/// API — it lets the differential oracle prove a filtered-encode test is
/// non-vacuous (the stream truly carries a non-zero deblocking filter).
#[cfg(feature = "oracle")]
#[doc(hidden)]
#[must_use]
pub fn __frame_filter_level(payload: &[u8]) -> Option<i32> {
    decode::frame_filter_level(payload)
}

/// Whether any macroblock in the VP8 key frame in `payload` is coded as intra-4×4
/// (`B_PRED`); `None` for a malformed stream. Oracle-only and hidden — not public
/// API — it lets the differential oracle prove an i4x4-encode test is non-vacuous
/// (the stream truly exercises the intra-4×4 luma path).
#[cfg(feature = "oracle")]
#[doc(hidden)]
#[must_use]
pub fn __frame_uses_i4x4(payload: &[u8]) -> Option<bool> {
    decode::frame_uses_i4x4(payload)
}

/// The number of distinct macroblock segments the VP8 key frame in `payload` uses
/// (`1` when segmentation is off); `None` for a malformed stream. Oracle-only and
/// hidden — not public API — it lets the differential oracle prove a segmented-encode
/// test is non-vacuous (the stream truly partitions the macroblocks).
#[cfg(feature = "oracle")]
#[doc(hidden)]
#[must_use]
pub fn __frame_segment_count(payload: &[u8]) -> Option<usize> {
    decode::frame_segment_count(payload)
}

#[cfg(test)]
mod tests {
    use super::{
        Codec, DecodeOptions, Dimensions, Error, ImageRef, LossyConfig, PixelLayout, decode,
        decode_argb, decode_argb_with, decode_with, encode_vp8, peek_dimensions, version,
    };

    /// Build a minimal valid 10-byte VP8 key-frame header for `width`×`height`.
    fn key_frame_header(width: u16, height: u16) -> [u8; 10] {
        let [wl, wh] = width.to_le_bytes();
        let [hl, hh] = height.to_le_bytes();
        // Frame tag (3 bytes, LE): key_frame=0 (bit 0 clear), version=0,
        // show_frame=1, first-partition size 0 => 0x00_00_10. Then the fixed
        // key-frame start code 0x9d 0x01 0x2a and the two 14-bit dimensions.
        [0x10, 0x00, 0x00, 0x9d, 0x01, 0x2a, wl, wh, hl, hh]
    }

    #[test]
    fn peek_dimensions_reads_a_key_frame_size() {
        let header = key_frame_header(320, 240);
        assert_eq!(
            peek_dimensions(&header).unwrap(),
            Dimensions::new(320, 240).unwrap()
        );
    }

    #[test]
    fn decode_reconstructs_a_minimal_key_frame() {
        // A coefficient-free key frame: the empty first partition decodes every
        // boolean as 0, so the luma is B_PRED with all sixteen 4x4 subblocks
        // B_DC_PRED (zero residual) and both chroma planes DC_PRED. The filter
        // level decodes to 0, so no loop filter runs. Predicting from the fixed
        // 127 (top) / 129 (left) borders yields a two-band gray image; with
        // neutral chroma (U=V=128) every pixel is an opaque gray (R==G==B).
        let header = key_frame_header(16, 16);
        let image = decode(&header).unwrap();
        assert_eq!((image.width(), image.height()), (16, 16));
        let px = image.as_bytes();
        assert_eq!(px.len(), 16 * 16 * 4);

        // Every pixel is opaque and gray (neutral chroma => R == G == B).
        for (i, p) in px.chunks_exact(4).enumerate() {
            assert_eq!(p[0], p[1], "pixel {i}: R != G");
            assert_eq!(p[1], p[2], "pixel {i}: G != B");
            assert_eq!(p[3], 0xff, "pixel {i}: alpha not opaque");
        }

        // Top band (pixel rows 0..=3): DC4 = (4*127 + 4*129 + 4) >> 3 = 128,
        // and (Y=128, U=V=128) -> gray 130.
        assert_eq!(&px[0..4], &[130, 130, 130, 255], "row 0 col 0");
        let r3c15 = 3 * 16 * 4 + 15 * 4;
        assert_eq!(&px[r3c15..r3c15 + 4], &[130, 130, 130, 255], "row 3 col 15");

        // Lower band (pixel rows 4..=15): DC4 = (4*128 + 4*129 + 4) >> 3 = 129,
        // and (Y=129, U=V=128) -> gray 132. The band edge sits exactly at row 4.
        let r4c0 = 4 * 16 * 4;
        assert_eq!(&px[r4c0..r4c0 + 4], &[132, 132, 132, 255], "row 4 col 0");
        let r15c15 = 15 * 16 * 4 + 15 * 4;
        assert_eq!(
            &px[r15c15..r15c15 + 4],
            &[132, 132, 132, 255],
            "row 15 col 15"
        );
    }

    #[test]
    fn decode_argb_matches_decode() {
        // The animation-compositor entry point yields the same picture as
        // `decode`, only pre-unpacked to native `0xAARRGGBB` ARGB.
        let header = key_frame_header(16, 16);
        let (dims, argb) = decode_argb(&header).unwrap();
        assert_eq!(dims, Dimensions::new(16, 16).unwrap());
        assert_eq!(argb.len(), 16 * 16);

        // Native ARGB equals `decode`'s RGBA bytes run through the same unpack.
        let rgba = decode(&header).unwrap();
        assert_eq!(
            crate::image::unpack_pixels(PixelLayout::Rgba8, rgba.as_bytes()),
            argb
        );
    }

    #[test]
    fn decode_rejects_a_truncated_header() {
        assert_eq!(decode(&[0u8; 9]).unwrap_err(), Error::Truncated);
    }

    #[test]
    fn decode_with_rejects_before_plane_alloc() {
        // A ~10-byte 16383x16383 key-frame header (first_partition_size=0): both
        // decode_with and decode_argb_with must reject it via the pixel limit
        // *before* reconstruction allocates ~1 GiB of planes, symmetric with
        // `crate::lossless::decode_with`.
        let header = key_frame_header(16383, 16383);
        let opts = DecodeOptions::default().max_pixels(1 << 20);
        let expected = Error::LimitExceeded {
            pixels: 16383 * 16383,
            limit: 1 << 20,
        };
        assert_eq!(decode_with(&header, &opts).unwrap_err(), expected);
        assert_eq!(decode_argb_with(&header, &opts).unwrap_err(), expected);

        // A small header still decodes through decode_with (default = no limit) and
        // matches the bare `decode`.
        let small = key_frame_header(16, 16);
        assert_eq!(
            decode_with(&small, &DecodeOptions::default()).unwrap(),
            decode(&small).unwrap()
        );
    }

    #[test]
    fn decode_with_honors_output_layout() {
        // decode_with must repack into the requested layout (unlike bare `decode`,
        // which is always Rgba8).
        let header = key_frame_header(16, 16);
        let rgba = decode(&header).unwrap();
        let bgra = decode_with(
            &header,
            &DecodeOptions::default().layout(PixelLayout::Bgra8),
        )
        .unwrap();
        let r = rgba.as_bytes();
        let b = bgra.as_bytes();
        assert_eq!([r[0], r[1], r[2], r[3]], [b[2], b[1], b[0], b[3]]);
    }

    #[test]
    fn decode_rejects_a_bad_start_code() {
        let mut header = key_frame_header(16, 16);
        header[3] = 0x00; // corrupt the 0x9d 0x01 0x2a start code
        assert_eq!(
            decode(&header).unwrap_err(),
            Error::InvalidBitstream {
                codec: Codec::Lossy
            }
        );
    }

    #[test]
    fn version_reports_the_cargo_package_version() {
        // `version()` must surface the crate's real Cargo version verbatim — not a
        // placeholder, and never the empty string.
        assert_eq!(version(), env!("CARGO_PKG_VERSION"));
        assert!(!version().is_empty(), "version must not be empty");
    }

    #[test]
    fn decode_with_pixel_limit_is_inclusive_at_the_boundary() {
        // `max_pixels` is an inclusive ceiling: a frame whose pixel count equals the
        // limit exactly must decode, and only a strictly larger count is rejected.
        // The check lives in `check_pixel_limit` (`pixels > limit`), shared by both
        // decode_with and decode_argb_with. A 16x16 frame is exactly 256 pixels.
        let header = key_frame_header(16, 16);
        assert!(
            decode_with(&header, &DecodeOptions::default().max_pixels(256)).is_ok(),
            "256 pixels must pass a limit of exactly 256"
        );
        assert!(
            decode_argb_with(&header, &DecodeOptions::default().max_pixels(256)).is_ok(),
            "256 pixels must pass a limit of exactly 256 (argb)"
        );
        // One under the count is a genuine over-limit rejection.
        assert_eq!(
            decode_with(&header, &DecodeOptions::default().max_pixels(255)).unwrap_err(),
            Error::LimitExceeded {
                pixels: 256,
                limit: 255,
            }
        );
    }

    #[test]
    fn repack_swaps_channels_on_a_colored_frame() {
        // `repack`'s early-return fast path must fire ONLY for the Rgba8 layout; any
        // other layout has to run the unpack/repack. A bare header decodes to a gray
        // frame, which makes a BGRA<->RGBA swap check vacuous (R == B), so encode a
        // saturated colored frame whose decoded pixels have R != B, then prove the
        // Bgra8 decode is the exact channel-swap of the Rgba8 decode.
        let (w, h) = (16u32, 16u32);
        let dims = Dimensions::new(w, h).unwrap();
        let mut rgba = Vec::new();
        for _ in 0..(w * h) {
            rgba.extend_from_slice(&[220, 40, 30, 0xff]);
        }
        let img = ImageRef::new(dims, PixelLayout::Rgba8, &rgba).unwrap();
        let (_dims, payload) = encode_vp8(img, &LossyConfig::new().with_quality(95)).unwrap();

        let rgba_out = decode(&payload).unwrap();
        let bgra_out = decode_with(
            &payload,
            &DecodeOptions::default().layout(PixelLayout::Bgra8),
        )
        .unwrap();
        let r = rgba_out.as_bytes();
        let b = bgra_out.as_bytes();
        // Non-vacuous: the decoded frame really is colored (R != B on pixel 0), so a
        // no-op repack (returning the RGBA bytes unswapped) fails the assertion below.
        assert_ne!(
            r[0], r[2],
            "decoded frame must be colored for this test to bite"
        );
        for (rp, bp) in r.chunks_exact(4).zip(b.chunks_exact(4)) {
            assert_eq!(
                [bp[0], bp[1], bp[2], bp[3]],
                [rp[2], rp[1], rp[0], rp[3]],
                "Bgra8 output is not the channel-swap of Rgba8"
            );
        }
    }
}
