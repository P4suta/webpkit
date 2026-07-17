//! RGBA → YUV 4:2:0 color conversion (the encoder counterpart of [`crate::lossy::yuv`]).
//!
//! The decoder's [`crate::lossy::yuv`] uses libwebp's *studio-swing* YUV→RGB
//! coefficients (`19077`/`26149`/…, Y in `16..=235`), so the forward path here
//! uses the matching studio-swing `VP8RGBToY`/`VP8RGBToU`/`VP8RGBToV`
//! coefficients — a full-range matrix would make the encode/decode round-trip
//! drift. Chroma is 4:2:0 box-downsampled: each chroma sample sums the covering
//! 2×2 luma-site RGB and the `>> (YUV_FIX + 2)` descale folds in the `/4`.
//!
//! The source is extended to whole macroblocks by replicating the last row and
//! column (VP8's edge-extension), so partial edge macroblocks have real samples.
//! Integer, deterministic, float-free. Exact cwebp parity (its default non-box
//! filter) is a quality refinement left for later; only the *decode* of our own
//! output must reproduce the source within PSNR, which any reasonable forward
//! conversion satisfies.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "every converted sample is clamped to 0..=255 before the u8 cast; \
              this reproduces the reference encoder's byte handling exactly"
)]

use crate::lossy::prelude::*;

/// Fixed-point precision of the RGB→YUV matrix (libwebp `YUV_FIX`).
pub(crate) const YUV_FIX: u32 = 16;
/// Rounding added to the luma accumulator (libwebp `YUV_HALF`).
const YUV_HALF: i32 = 1 << (YUV_FIX - 1);
/// Rounding added to a chroma accumulator that already sums 4 samples: the
/// `>> (YUV_FIX + 2)` descale divides by both the fixed-point scale and the 4
/// box samples, so the round-to-nearest bias is `YUV_HALF << 2`.
const UV_ROUND: i32 = YUV_HALF << 2;
/// The `128` chroma center, pre-shifted for the `>> (YUV_FIX + 2)` descale.
const UV_OFFSET: i32 = 128 << (YUV_FIX + 2);
/// Studio-swing `VP8RGBToU` matrix row (libwebp): the blue-difference chroma weights
/// applied to red/green/blue. Shared with [`crate::lossy::sharp_yuv`], which folds them
/// into a signed chroma delta.
pub(crate) const U_COEFF: [i32; 3] = [-9719, -19081, 28800];
/// Studio-swing `VP8RGBToV` matrix row (libwebp): the red-difference chroma weights.
pub(crate) const V_COEFF: [i32; 3] = [28800, -24116, -4684];

/// The source luma/chroma planes for one frame, padded to whole macroblocks.
pub(crate) struct SourceYuv {
    /// Luma plane, row-major, `mb_w*16` × `mb_h*16`.
    pub(crate) y: Vec<u8>,
    /// U chroma plane, row-major, `mb_w*8` × `mb_h*8`.
    pub(crate) u: Vec<u8>,
    /// V chroma plane.
    pub(crate) v: Vec<u8>,
    /// Frame width in macroblocks.
    pub(crate) mb_w: usize,
    /// Frame height in macroblocks.
    pub(crate) mb_h: usize,
}

impl SourceYuv {
    /// Row stride of the luma plane (`mb_w * 16`).
    pub(crate) const fn y_stride(&self) -> usize {
        self.mb_w * 16
    }

    /// Row stride of the chroma planes (`mb_w * 8`).
    pub(crate) const fn uv_stride(&self) -> usize {
        self.mb_w * 8
    }
}

/// libwebp `VP8RGBToY`: studio-swing luma of one pixel (result in `16..=235`).
pub(crate) fn rgb_to_y(r: i32, g: i32, b: i32) -> u8 {
    let luma = 16839 * r + 33059 * g + 6420 * b;
    ((luma + YUV_HALF + (16 << YUV_FIX)) >> YUV_FIX).clamp(0, 255) as u8
}

/// libwebp `VP8ClipUV` for a 4-sample chroma accumulator: add the center and
/// rounding, descale by `YUV_FIX + 2` (which also divides by 4), clamp.
fn clip_uv_sum(acc: i32) -> u8 {
    ((acc + UV_ROUND + UV_OFFSET) >> (YUV_FIX + 2)).clamp(0, 255) as u8
}

/// libwebp `VP8RGBToU` over a summed 2×2 RGB block.
pub(crate) fn rgb_to_u(sr: i32, sg: i32, sb: i32) -> u8 {
    clip_uv_sum(U_COEFF[0] * sr + U_COEFF[1] * sg + U_COEFF[2] * sb)
}

/// libwebp `VP8RGBToV` over a summed 2×2 RGB block.
pub(crate) fn rgb_to_v(sr: i32, sg: i32, sb: i32) -> u8 {
    clip_uv_sum(V_COEFF[0] * sr + V_COEFF[1] * sg + V_COEFF[2] * sb)
}

/// Convert an RGBA source (`rgba`, 4 bytes/pixel, row-major `width` × `height`,
/// alpha ignored) into padded YUV 4:2:0 [`SourceYuv`] planes.
#[must_use]
#[expect(
    clippy::many_single_char_names,
    reason = "r/g/b are the conventional color-channel names and x/y the pixel \
              coordinates; longer names would obscure the conversion"
)]
pub(crate) fn from_rgba(rgba: &[u8], width: usize, height: usize) -> SourceYuv {
    let mb_w = width.div_ceil(16);
    let mb_h = height.div_ceil(16);
    let y_stride = mb_w * 16;
    let uv_stride = mb_w * 8;
    let (y_h, uv_h) = (mb_h * 16, mb_h * 8);

    // Fetch a clamped RGB pixel (edge-replicated past the real picture bounds).
    let rgb = |x: usize, y: usize| -> (i32, i32, i32) {
        let cx = x.min(width - 1);
        let cy = y.min(height - 1);
        let i = (cy * width + cx) * 4;
        (
            i32::from(rgba[i]),
            i32::from(rgba[i + 1]),
            i32::from(rgba[i + 2]),
        )
    };

    let mut y = vec![0u8; y_stride * y_h];
    for py in 0..y_h {
        for px in 0..y_stride {
            let (r, g, b) = rgb(px, py);
            y[py * y_stride + px] = rgb_to_y(r, g, b);
        }
    }

    let mut u = vec![0u8; uv_stride * uv_h];
    let mut v = vec![0u8; uv_stride * uv_h];
    for cy in 0..uv_h {
        for cx in 0..uv_stride {
            let mut sr = 0;
            let mut sg = 0;
            let mut sb = 0;
            for dy in 0..2 {
                for dx in 0..2 {
                    let (r, g, b) = rgb(2 * cx + dx, 2 * cy + dy);
                    sr += r;
                    sg += g;
                    sb += b;
                }
            }
            u[cy * uv_stride + cx] = rgb_to_u(sr, sg, sb);
            v[cy * uv_stride + cx] = rgb_to_v(sr, sg, sb);
        }
    }

    SourceYuv {
        y,
        u,
        v,
        mb_w,
        mb_h,
    }
}

#[cfg(test)]
mod tests {
    use super::from_rgba;

    /// Build a solid `width`×`height` RGBA buffer of one color.
    fn solid(width: usize, height: usize, rgb: [u8; 3]) -> Vec<u8> {
        let mut buf = Vec::with_capacity(width * height * 4);
        for _ in 0..width * height {
            buf.extend_from_slice(&[rgb[0], rgb[1], rgb[2], 0xff]);
        }
        buf
    }

    #[test]
    fn neutral_gray_maps_to_studio_swing() {
        // r=g=b=128: Y = (56318*128 + 32768 + (16<<16)) >> 16 = 126 (studio swing),
        // U = V = 128 (the luma-difference chroma of a gray is exactly centered).
        let src = from_rgba(&solid(16, 16, [128, 128, 128]), 16, 16);
        assert!(src.y.iter().all(|&p| p == 126), "gray luma is 126");
        assert!(src.u.iter().all(|&p| p == 128), "gray U centered");
        assert!(src.v.iter().all(|&p| p == 128), "gray V centered");
    }

    #[test]
    fn black_and_white_hit_the_studio_pedestal_and_ceiling() {
        // Black -> Y 16 (the 16 pedestal), white -> Y 235 (the studio ceiling).
        let black = from_rgba(&solid(16, 16, [0, 0, 0]), 16, 16);
        assert!(black.y.iter().all(|&p| p == 16), "black luma is 16");
        let white = from_rgba(&solid(16, 16, [255, 255, 255]), 16, 16);
        assert!(white.y.iter().all(|&p| p == 235), "white luma is 235");
        // Both are neutral chroma.
        assert!(black.u.iter().all(|&p| p == 128) && white.u.iter().all(|&p| p == 128));
    }

    #[test]
    fn pure_primaries_push_chroma_off_center() {
        // Pure blue lifts U well above 128 (blue-difference); pure red lifts V.
        let blue = from_rgba(&solid(16, 16, [0, 0, 255]), 16, 16);
        assert!(blue.u[0] > 200, "blue U high, got {}", blue.u[0]);
        let red = from_rgba(&solid(16, 16, [255, 0, 0]), 16, 16);
        assert!(red.v[0] > 200, "red V high, got {}", red.v[0]);
    }

    #[test]
    fn chroma_rounding_bias_lands_on_the_nearest_level() {
        // The chroma descale is `(acc + UV_ROUND + UV_OFFSET) >> (YUV_FIX + 2)`,
        // dividing by 2^18, so the round-to-nearest bias UV_ROUND must be 2^17
        // (= YUV_HALF << 2). A very dark blue [0,0,2] sums sb = 8 and gives a U
        // accumulator that lands just past the .5 boundary: the correct bias rounds
        // it UP to 129, whereas the mutated `YUV_HALF >> 2` (= 8192) rounds it down
        // to 128. Pin the exact rounded level.
        let src = from_rgba(&solid(16, 16, [0, 0, 2]), 16, 16);
        assert!(
            src.u.iter().all(|&p| p == 129),
            "dark-blue U rounds to 129, got {}",
            src.u[0]
        );
    }

    #[test]
    fn planes_are_padded_to_whole_macroblocks() {
        // A 17×13 picture rounds up to 2×1 macroblocks: luma 32×16, chroma 16×8.
        let src = from_rgba(&solid(17, 13, [64, 200, 32]), 17, 13);
        assert_eq!((src.mb_w, src.mb_h), (2, 1));
        assert_eq!(src.y.len(), 32 * 16);
        assert_eq!(src.u.len(), 16 * 8);
        assert_eq!(src.y_stride(), 32);
        assert_eq!(src.uv_stride(), 16);
        // Edge replication: the padded columns/rows carry the same color, so the
        // whole (uniform) plane is constant.
        let first = src.y[0];
        assert!(src.y.iter().all(|&p| p == first), "edge-extended uniformly");
    }
}
