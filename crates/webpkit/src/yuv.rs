//! The native YUV 4:2:0 output of the lossy (`VP8`) decoder: [`YuvImage`].
//!
//! A lossy WebP reconstructs to YUV 4:2:0 planes before the YUV→RGB conversion
//! [`decode`](crate::decode) performs. [`decode_yuv`](crate::decode_yuv) stops one
//! step earlier and hands back those planes — the byte-exact form libwebp's
//! `WebPDecodeYUV` returns — for callers that consume YUV directly (video
//! pipelines, `dwebp -yuv`/`-pgm`). Only the lossy codec has this native form;
//! lossless (`VP8L`) and animation have no YUV representation.

use crate::Dimensions;
use crate::prelude::*;

/// A reconstructed lossy (`VP8`) frame as native **YUV 4:2:0** planes.
///
/// The luma plane [`y`](Self::y) is `width × height` bytes, row-major. The chroma
/// planes [`u`](Self::u) and [`v`](Self::v) are each `⌈width/2⌉ × ⌈height/2⌉` —
/// one sample per 2×2 luma block ([`chroma_width`](Self::chroma_width) ×
/// [`chroma_height`](Self::chroma_height)). All three are packed with no row
/// padding, matching libwebp's `WebPDecodeYUV`.
///
/// Produced only by [`decode_yuv`](crate::decode_yuv).
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct YuvImage {
    dims: Dimensions,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

impl YuvImage {
    /// Assemble a [`YuvImage`] from already-cropped, packed planes. Internal: the
    /// decoder is the only source of a bit-exact 4:2:0 plane triple.
    pub(crate) const fn from_planes(dims: Dimensions, y: Vec<u8>, u: Vec<u8>, v: Vec<u8>) -> Self {
        Self { dims, y, u, v }
    }

    /// The luma [`Dimensions`] (`width × height`); the chroma planes are half in
    /// each axis, rounded up.
    #[must_use]
    pub const fn dimensions(&self) -> Dimensions {
        self.dims
    }

    /// The luma width in pixels.
    #[must_use]
    pub const fn width(&self) -> u32 {
        self.dims.width()
    }

    /// The luma height in pixels.
    #[must_use]
    pub const fn height(&self) -> u32 {
        self.dims.height()
    }

    /// The chroma-plane width, `⌈width/2⌉` (4:2:0 subsampling).
    #[must_use]
    pub const fn chroma_width(&self) -> u32 {
        self.dims.width().div_ceil(2)
    }

    /// The chroma-plane height, `⌈height/2⌉` (4:2:0 subsampling).
    #[must_use]
    pub const fn chroma_height(&self) -> u32 {
        self.dims.height().div_ceil(2)
    }

    /// The packed luma plane, `width × height` bytes in row-major order.
    #[must_use]
    pub fn y(&self) -> &[u8] {
        &self.y
    }

    /// The packed U (blue-difference) chroma plane,
    /// `chroma_width × chroma_height` bytes.
    #[must_use]
    pub fn u(&self) -> &[u8] {
        &self.u
    }

    /// The packed V (red-difference) chroma plane,
    /// `chroma_width × chroma_height` bytes.
    #[must_use]
    pub fn v(&self) -> &[u8] {
        &self.v
    }
}
