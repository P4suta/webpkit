//! Shared streaming decode vocabulary: the [`DecodeOptions`] builder, the
//! [`ImageInfo`] header summary, the push-based [`Progress`] events, and the
//! zero-copy [`RowDrain`] row view.
//!
//! These types are bitstream-agnostic — they describe *how* a decode is driven
//! and *what* it reports, not the pixels themselves — so they live in the core shell
//! where every codec's streaming decoder can share one set: the `lossless` codec (VP8L) drives
//! them today and the `lossy` codec (VP8) row-streaming decoder will reuse the same
//! vocabulary. The decoders that *emit* this progress (e.g. the `lossless` codec's
//! `IncrementalDecoder`) live in the codec crates; only the vocabulary is shared.

use crate::container::anim::FrameMeta;
use crate::error::Result;
use crate::image::{Dimensions, PixelLayout};
use crate::prelude::*;

/// The located sub-chunk payloads of one `ANMF` animation frame, handed to a
/// [`FrameDecoder`]. Exactly one of `vp8l`/`vp8` is `Some` for a well-formed frame.
#[derive(Clone, Copy, Debug)]
pub struct FramePayload<'a> {
    /// The frame's `VP8L` (lossless) bitstream payload, if it is a lossless frame.
    pub vp8l: Option<&'a [u8]>,
    /// The frame's `VP8 ` (lossy) bitstream payload, if it is a lossy frame.
    pub vp8: Option<&'a [u8]>,
    /// A sibling `ALPH` chunk payload (including its 1-byte header), for a lossy
    /// frame that carries a separate alpha plane.
    pub alph: Option<&'a [u8]>,
    /// The frame rectangle's dimensions, from the `ANMF` header.
    pub dims: Dimensions,
}

/// A decoded animation frame's native-ARGB pixels plus its compositing-relevant
/// alpha flag (libwebp keys the compositor on the declared/`ALPH`-present flag,
/// not a pixel scan).
#[derive(Clone, Debug)]
pub struct DecodedFrame {
    /// The frame's native `0xAARRGGBB` pixels, `dims.width * dims.height` long.
    pub argb: Vec<u32>,
    /// Whether the frame declares alpha (drives the blend/key-frame logic).
    pub alpha_used: bool,
}

/// Decodes one located animation frame ([`FramePayload`]) into pixels.
///
/// This is the seam that lets the codec-agnostic animation walker decode frames of
/// either codec without depending on them: a bare `lossless` codec supplies a
/// decoder that handles `VP8L` and rejects `VP8 `, while the umbrella `webpkit`
/// crate supplies one that handles both (compositing an `ALPH` plane onto a lossy
/// frame). A frame walker is generic over the concrete implementor (each a
/// zero-sized unit struct), so the decoder is chosen at compile time with no
/// dynamic dispatch.
pub trait FrameDecoder: core::fmt::Debug + Sync {
    /// Decode `frame` into native-ARGB pixels. The returned `argb` must be
    /// `frame.dims.width * frame.dims.height` long.
    ///
    /// # Errors
    ///
    /// A bitstream/container error, [`Error::UnsupportedFeature`](crate::Error::UnsupportedFeature)
    /// for a frame codec this decoder does not handle, or
    /// [`Error::LimitExceeded`](crate::Error::LimitExceeded) past `options.max_pixels`.
    fn decode_frame(
        &self,
        frame: FramePayload<'_>,
        options: &DecodeOptions,
    ) -> Result<DecodedFrame>;
}

/// Options controlling a decode.
///
/// Build one with [`DecodeOptions::new`] / [`Default`] and the consuming builder
/// methods ([`layout`](Self::layout), [`max_pixels`](Self::max_pixels),
/// [`read_metadata`](Self::read_metadata)); the fields are private so new options
/// can be added without a breaking change (reinforced by `#[non_exhaustive]`).
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct DecodeOptions {
    /// The requested output pixel byte order. See [`layout`](Self::layout).
    pub(crate) layout: PixelLayout,
    /// The opt-in `width * height` cap. See [`max_pixels`](Self::max_pixels).
    pub(crate) max_pixels: Option<u64>,
    /// Whether to extract ICC/Exif/XMP metadata. See [`read_metadata`](Self::read_metadata).
    pub(crate) read_metadata: bool,
}

impl Default for DecodeOptions {
    fn default() -> Self {
        Self {
            layout: PixelLayout::Rgba8,
            max_pixels: None,
            read_metadata: true,
        }
    }
}

impl DecodeOptions {
    /// Default options: RGBA output, no pixel limit, metadata read.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the output byte order.
    #[must_use]
    pub const fn layout(mut self, layout: PixelLayout) -> Self {
        self.layout = layout;
        self
    }

    /// Cap the decoded canvas at `max` pixels (`width * height`) — the guard
    /// against a hostile header that claims a huge image to exhaust memory.
    ///
    /// An image whose pixel count exceeds `max` is rejected with
    /// [`Error::LimitExceeded`](crate::error::Error::LimitExceeded) *before* any
    /// pixel or canvas buffer is allocated. **The default is no cap** (`None`): a
    /// plain [`decode`](crate::decode) accepts an arbitrarily large image, bounded
    /// only by the per-side dimension limit ([`MAX_DIMENSION`](crate::MAX_DIMENSION),
    /// ≈268 Mpx ≈ 1 GiB of RGBA). **Always set this when decoding untrusted input.**
    #[must_use]
    pub const fn max_pixels(mut self, max: u64) -> Self {
        self.max_pixels = Some(max);
        self
    }

    /// Whether to extract ICC/Exif/XMP metadata (default `true`).
    #[must_use]
    pub const fn read_metadata(mut self, read: bool) -> Self {
        self.read_metadata = read;
        self
    }
}

/// A summary of an image, obtainable before a full decode.
///
/// `#[non_exhaustive]`: construct one with [`ImageInfo::new`] so future header
/// fields can be added without a breaking change (the fields stay `pub` to read).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ImageInfo {
    /// The image dimensions (for an animation, the canvas size).
    pub dimensions: Dimensions,
    /// Whether alpha is used, combining the VP8L header advisory bit with any
    /// VP8X alpha flag.
    pub has_alpha: bool,
    /// Whether the file carries ICC/Exif/XMP metadata.
    pub has_metadata: bool,
    /// Whether the file is an animation (use `decode_frames` for the frames; a
    /// one-shot `decode` returns the first composited frame).
    pub is_animated: bool,
}

impl ImageInfo {
    /// Assemble an [`ImageInfo`] header summary. The sole constructor (the struct
    /// is `#[non_exhaustive]`), so the codec crates that peek a header build one
    /// here rather than with a struct literal.
    #[must_use]
    pub const fn new(
        dimensions: Dimensions,
        has_alpha: bool,
        has_metadata: bool,
        is_animated: bool,
    ) -> Self {
        Self {
            dimensions,
            has_alpha,
            has_metadata,
            is_animated,
        }
    }
}

/// Progress reported by a push-based decoder's `push`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum Progress {
    /// More bytes are required before anything can be reported.
    NeedMoreInput,
    /// The header is now known (reported once, before the pixels are complete).
    HeaderReady(ImageInfo),
    /// Output rows `first_row..first_row + count` (0-based) were finalized on this
    /// push and are available via the decoder's `drain_rows` (still images only).
    /// One `push` can finalize a burst of rows at once.
    RowsDecoded {
        /// 0-based index of the first newly-finalized row.
        first_row: u32,
        /// Number of newly-finalized rows.
        count: u32,
    },
    /// One animation frame was decoded and composited onto the persistent canvas
    /// (animations only). The [`FrameMeta`] describes the frame just completed.
    /// Reported once per frame, after [`Self::HeaderReady`] and before the single
    /// terminal [`Self::Finished`]; one `push` completes at most one frame.
    FrameComplete(FrameMeta),
    /// The whole image has been decoded; retrieve it with the decoder's
    /// `into_image`.
    Finished,
}

/// A zero-copy borrow of contiguous finalized rows, yielded by a decoder's
/// `drain_rows`.
///
/// The bytes are packed in the decoder's requested [`PixelLayout`], `width * 4`
/// per row. Draining is a **non-consuming early view**: each row is yielded once
/// (successive `drain_rows` calls return only newly finalized rows). The retained
/// bytes are freed on the next `push` to bound memory for a pure-streaming
/// consumer, but the decoder's `into_image` still returns the complete image
/// (re-decoding any freed rows from the buffer).
#[derive(Clone, Copy, Debug)]
pub struct RowDrain<'a> {
    /// 0-based output-row index of the first row in this batch.
    pub first_row: u32,
    /// Number of rows in this batch.
    pub rows: u32,
    /// Image width in pixels (each row is `width * 4` bytes).
    pub width: u32,
    /// The byte order of [`Self::as_bytes`].
    pub layout: PixelLayout,
    /// The packed row bytes, `rows * width * 4` long.
    bytes: &'a [u8],
}

impl<'a> RowDrain<'a> {
    /// A view over `bytes` (`rows * width * 4` packed bytes in `layout`), whose
    /// first row is output-row `first_row`.
    #[must_use]
    pub const fn new(
        first_row: u32,
        rows: u32,
        width: u32,
        layout: PixelLayout,
        bytes: &'a [u8],
    ) -> Self {
        Self {
            first_row,
            rows,
            width,
            layout,
            bytes,
        }
    }
}

impl RowDrain<'_> {
    /// All drained rows as one packed byte slice (`rows * width * 4` bytes).
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8] {
        self.bytes
    }

    /// The `i`-th row of this batch as `width * 4` packed bytes.
    ///
    /// # Panics
    ///
    /// If `i >= rows`.
    #[must_use]
    pub fn row(&self, i: u32) -> &[u8] {
        let row_bytes = self.width as usize * 4;
        let start = i as usize * row_bytes;
        &self.bytes[start..start + row_bytes]
    }
}

#[cfg(test)]
mod tests {
    use super::{DecodeOptions, ImageInfo};
    use crate::image::{Dimensions, PixelLayout};

    #[test]
    fn image_info_new_is_the_construction_path() {
        // `ImageInfo` is `#[non_exhaustive]`, so `ImageInfo::new` is the only way
        // to build one — the codec crates rely on this staying stable.
        let dims = Dimensions::new(3, 4).unwrap();
        let info = ImageInfo::new(dims, true, false, true);
        assert_eq!(info.dimensions, dims);
        assert!(info.has_alpha);
        assert!(!info.has_metadata);
        assert!(info.is_animated);
    }

    #[test]
    fn decode_options_builder_round_trips_and_defaults_to_no_limit() {
        // `#[non_exhaustive]`: the builder (not a struct literal) is the construction
        // path, and the default carries no pixel limit.
        assert_eq!(DecodeOptions::default().max_pixels, None);
        let opts = DecodeOptions::new()
            .layout(PixelLayout::Bgra8)
            .max_pixels(1024)
            .read_metadata(false);
        assert_eq!(opts.layout, PixelLayout::Bgra8);
        assert_eq!(opts.max_pixels, Some(1024));
        assert!(!opts.read_metadata);
    }

    #[test]
    fn row_drain_row_slices_by_width_times_four() {
        use super::RowDrain;
        // 2 rows x width 3 -> 12 bytes/row, distinct content so a wrong stride or
        // offset is visible. Kills the `* -> +/-`, `* -> /`, and `+ -> -/*`
        // arithmetic mutants in `row`, plus its `-> Vec::leak(..)` body swaps.
        let bytes: Vec<u8> = (0..24).collect();
        let drain = RowDrain::new(5, 2, 3, PixelLayout::Rgba8, &bytes);
        assert_eq!(drain.row(0), &(0u8..12).collect::<Vec<u8>>()[..]);
        assert_eq!(drain.row(1), &(12u8..24).collect::<Vec<u8>>()[..]);
        assert_eq!(drain.first_row, 5);
        assert_eq!(drain.rows, 2);
    }
}
