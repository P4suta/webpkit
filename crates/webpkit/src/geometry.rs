//! The shared crop/resize geometry engine, used by both the encode and decode
//! tool-side paths.
//!
//! Resizing is a bit-exact port of libwebp's `WebPRescaler` (`utils/rescaler.c` +
//! the generic `dsp/rescaler.c` import/export path): a fixed-point,
//! [`forbid(unsafe_code)`](crate), zero-dependency resampler whose output is
//! byte-identical to `cwebp`/`dwebp`. The `RFIX = 32` fixed-point shift, the
//! `fx`/`fy`/`fxy` factor derivation, the import-then-export phase ordering, and the
//! per-channel `u32`/`u64` accumulation all mirror the reference exactly, because an
//! off-by-one in the rounding or the phase order is a visible pixel difference.
//!
//! `resize` follows `WebPPictureRescale`'s ARGB path: it black-mattes (premultiplies
//! the color channels by alpha), rescales all four channels interleaved, then removes
//! the premultiplication — so translucent edges interpolate the way libwebp's do.
//! `crop` is an exact byte-window copy. Both are integer-only and deterministic.

use crate::error::{Error, Result};
use crate::image::{Dimensions, Image, argb_has_alpha, pack_pixels, unpack_pixels};
use crate::prelude::*;

/// Fixed-point precision of the rescaler's multiplies (`WEBP_RESCALER_RFIX`).
const RFIX: u32 = 32;
/// `1.0` in `RFIX` fixed-point (`WEBP_RESCALER_ONE`); does not fit in `u32`.
const ONE: u64 = 1 << RFIX;
/// Half an LSB, the rounding bias of [`mult_fix`] (`ROUNDER`).
const ROUNDER: u64 = ONE >> 1;

/// `WEBP_RESCALER_FRAC(numer, denom)`: `numer / denom` in `RFIX` fixed-point,
/// truncated to `u32` (so an exact `1.0` wraps to `0`, matching the reference — the
/// callers that can hit it special-case the result).
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "denom is a positive dimension/increment; the u32 truncation is \
              WEBP_RESCALER_FRAC's own cast (an exact 1.0 wraps to 0 by design)"
)]
fn frac(numer: u32, denom: i32) -> u32 {
    (((u64::from(numer)) << RFIX) / denom as u64) as u32
}

/// `MULT_FIX(x, y)`: rounded fixed-point multiply, `(x * y + ROUNDER) >> RFIX`.
fn mult_fix(x: u32, y: u32) -> u64 {
    (u64::from(x) * u64::from(y) + ROUNDER) >> RFIX
}

/// `MULT_FIX_FLOOR(x, y)`: truncating fixed-point multiply, `(x * y) >> RFIX`.
fn mult_fix_floor(x: u32, y: u32) -> u64 {
    (u64::from(x) * u64::from(y)) >> RFIX
}

//------------------------------------------------------------------------------
// Alpha black-matting (libwebp dsp/alpha_processing.c WebPMultARGBRow_C)

/// Fixed-point precision of the alpha-weight multiply (`MFIX`).
const MFIX: u32 = 24;
/// Rounding bias for [`mult_alpha`] (`HALF`).
const M_HALF: u32 = 1 << (MFIX - 1);
/// `(1 << MFIX) / 255`, the forward premultiply scale per alpha unit (`KINV_255`).
const KINV_255: u32 = (1 << MFIX) / 255;

/// `Mult(x, scale)`: `(x * scale + HALF) >> MFIX`, the per-channel alpha weight.
#[allow(
    clippy::cast_possible_truncation,
    reason = "in-domain the result is <= 255 (24-bit precision guarantees it), so \
              the u32 cast never truncates a live bit — mirrors Mult()'s return"
)]
fn mult_alpha(x: u8, scale: u32) -> u32 {
    (((u64::from(x) * u64::from(scale)) + u64::from(M_HALF)) >> MFIX) as u32
}

/// `GetScale(a, inverse)`: the premultiply (`false`) or un-premultiply (`true`)
/// scale for a non-zero, non-opaque alpha `a`.
const fn alpha_scale(a: u32, inverse: bool) -> u32 {
    if inverse {
        (255_u32 << MFIX) / a
    } else {
        a * KINV_255
    }
}

/// `WebPMultARGBRow_C` for one native `0xAARRGGBB` pixel: black-matte
/// (`inverse == false`) or restore (`inverse == true`) the color channels by alpha.
/// Opaque and fully-transparent pixels are handled exactly as the reference.
#[allow(
    clippy::cast_possible_truncation,
    reason = "each `as u8` takes the low byte of a color lane, exactly as \
              WebPMultARGBRow_C's `Mult(argb >> k, scale)` does"
)]
fn mult_argb(argb: u32, inverse: bool) -> u32 {
    if argb < 0xff00_0000 {
        if argb <= 0x00ff_ffff {
            0
        } else {
            let alpha = (argb >> 24) & 0xff;
            let scale = alpha_scale(alpha, inverse);
            let mut out = argb & 0xff00_0000;
            out |= mult_alpha(argb as u8, scale);
            out |= mult_alpha((argb >> 8) as u8, scale) << 8;
            out |= mult_alpha((argb >> 16) as u8, scale) << 16;
            out
        }
    } else {
        argb
    }
}

//------------------------------------------------------------------------------
// WebPRescaler port (utils/rescaler.c + dsp/rescaler.c generic path)

/// A single-plane fixed-point rescaler over `num_channels` interleaved `u8` lanes,
/// a direct port of libwebp's `WebPRescaler`. Owns its output plane and the two
/// `u32` work rows; driven by [`rescale_plane`].
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    reason = "a line-for-line port of libwebp's rescaler: the int/uint32/uint64 casts \
              and their wrap/truncation are load-bearing to the reference's fixed-point \
              math, and every product is widened to u64 before it can overflow"
)]
struct Rescaler {
    x_expand: bool,
    y_expand: bool,
    num_channels: usize,
    fx_scale: u32,
    fy_scale: u32,
    fxy_scale: u32,
    y_accum: i32,
    y_add: i32,
    y_sub: i32,
    x_add: i32,
    x_sub: i32,
    src_width: i32,
    dst_width: i32,
    dst_height: i32,
    dst_y: i32,
    irow: Vec<u32>,
    frow: Vec<u32>,
    dst: Vec<u8>,
    dst_stride: usize,
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_possible_wrap,
    clippy::similar_names,
    clippy::many_single_char_names,
    reason = "a line-for-line port of libwebp's rescaler: the int/uint32/uint64 casts \
              and their wrap/truncation are load-bearing to the reference's fixed-point \
              math, every product is widened to u64 before it can overflow, and the \
              fx/fy/fxy factor and A/B/I/J blend names are the reference's own"
)]
impl Rescaler {
    /// `WebPRescalerInit`: derive the increments and fixed-point factors for a
    /// `src_width x src_height` -> `dst_width x dst_height` rescale of `num_channels`
    /// interleaved lanes, and allocate the output plane and work rows.
    fn new(
        src_width: i32,
        src_height: i32,
        dst_width: i32,
        dst_height: i32,
        num_channels: usize,
    ) -> Self {
        let x_expand = src_width < dst_width;
        let y_expand = src_height < dst_height;

        let (x_add, x_sub) = if x_expand {
            (dst_width - 1, src_width - 1)
        } else {
            (src_width, dst_width)
        };
        // fx_scale is used by the shrink import only.
        let fx_scale = if x_expand { 0 } else { frac(1, x_sub) };

        let (y_add, y_sub) = if y_expand {
            (src_height - 1, dst_height - 1)
        } else {
            (src_height, dst_height)
        };
        let y_accum = if y_expand { y_sub } else { y_add };

        let (fy_scale, fxy_scale) = if y_expand {
            (frac(1, x_add), 0)
        } else {
            let num = u64::from(dst_height as u32) << RFIX;
            let den = u64::from(x_add as u32) * u64::from(y_add as u32);
            let ratio = num / den;
            // A ratio that does not fit u32 (== ONE) is special-cased to 0 in export.
            let fxy = if ratio == u64::from(ratio as u32) {
                ratio as u32
            } else {
                0
            };
            (frac(1, y_sub), fxy)
        };

        let work_len = num_channels * dst_width as usize;
        let dst_stride = num_channels * dst_width as usize;
        Self {
            x_expand,
            y_expand,
            num_channels,
            fx_scale,
            fy_scale,
            fxy_scale,
            y_accum,
            y_add,
            y_sub,
            x_add,
            x_sub,
            src_width,
            dst_width,
            dst_height,
            dst_y: 0,
            irow: vec![0; work_len],
            frow: vec![0; work_len],
            dst: vec![0; dst_stride * dst_height as usize],
            dst_stride,
        }
    }

    /// True once every output row has been written.
    const fn output_done(&self) -> bool {
        self.dst_y >= self.dst_height
    }

    /// True while an output row is ready to export (`WebPRescalerHasPendingOutput`).
    const fn has_pending_output(&self) -> bool {
        !self.output_done() && self.y_accum <= 0
    }

    /// `WebPRescalerImport`: consume rows from `src` (starting at its first byte,
    /// advancing `src_stride` per row) until one output row is ready or `num_lines`
    /// are taken. Returns the number of rows consumed.
    fn import(&mut self, num_lines: i32, src: &[u8], src_stride: usize) -> i32 {
        let mut total = 0;
        let mut off = 0;
        while total < num_lines && !self.has_pending_output() {
            if self.y_expand {
                core::mem::swap(&mut self.irow, &mut self.frow);
            }
            self.import_row(&src[off..]);
            if !self.y_expand {
                let n = self.num_channels * self.dst_width as usize;
                for x in 0..n {
                    self.irow[x] = self.irow[x].wrapping_add(self.frow[x]);
                }
            }
            off += src_stride;
            total += 1;
            self.y_accum -= self.y_sub;
        }
        total
    }

    /// `WebPRescalerImportRow`: fill `frow` from one source row.
    fn import_row(&mut self, src: &[u8]) {
        if self.x_expand {
            self.import_row_expand(src);
        } else {
            self.import_row_shrink(src);
        }
    }

    /// `WebPRescalerImportRowExpand_C`: horizontal bilinear expansion into `frow`.
    fn import_row_expand(&mut self, src: &[u8]) {
        let x_stride = self.num_channels;
        let x_out_max = self.dst_width as usize * self.num_channels;
        for channel in 0..x_stride {
            let mut x_in = channel;
            let mut x_out = channel;
            let mut accum = self.x_add;
            let mut left = u32::from(src[x_in]);
            let mut right = if self.src_width > 1 {
                u32::from(src[x_in + x_stride])
            } else {
                left
            };
            x_in += x_stride;
            loop {
                self.frow[x_out] = right
                    .wrapping_mul(self.x_add as u32)
                    .wrapping_add(left.wrapping_sub(right).wrapping_mul(accum as u32));
                x_out += x_stride;
                if x_out >= x_out_max {
                    break;
                }
                accum -= self.x_sub;
                if accum < 0 {
                    left = right;
                    x_in += x_stride;
                    right = u32::from(src[x_in]);
                    accum += self.x_add;
                }
            }
        }
    }

    /// `WebPRescalerImportRowShrink_C`: horizontal area-average shrink into `frow`.
    fn import_row_shrink(&mut self, src: &[u8]) {
        let x_stride = self.num_channels;
        let x_out_max = self.dst_width as usize * self.num_channels;
        for channel in 0..x_stride {
            let mut x_in = channel;
            let mut x_out = channel;
            let mut sum: u32 = 0;
            let mut accum: i32 = 0;
            while x_out < x_out_max {
                let mut base: u32 = 0;
                accum += self.x_add;
                while accum > 0 {
                    accum -= self.x_sub;
                    base = u32::from(src[x_in]);
                    sum = sum.wrapping_add(base);
                    x_in += x_stride;
                }
                let frac_part = base.wrapping_mul((-accum) as u32);
                self.frow[x_out] = sum.wrapping_mul(self.x_sub as u32).wrapping_sub(frac_part);
                sum = mult_fix(frac_part, self.fx_scale) as u32;
                x_out += x_stride;
            }
        }
    }

    /// `WebPRescalerExport`: write every output row currently ready.
    fn export(&mut self) {
        while self.has_pending_output() {
            self.export_row();
        }
    }

    /// `WebPRescalerExportRow`: emit one output row, then advance the vertical phase.
    fn export_row(&mut self) {
        if self.y_accum <= 0 {
            if self.y_expand {
                self.export_row_expand();
            } else if self.fxy_scale != 0 {
                self.export_row_shrink();
            } else {
                self.export_row_special();
            }
            self.y_accum += self.y_add;
            self.dst_y += 1;
        }
    }

    /// `WebPRescalerExportRowExpand_C`: vertical bilinear blend of `irow`/`frow`.
    fn export_row_expand(&mut self) {
        let x_out_max = self.dst_width as usize * self.num_channels;
        let base = self.dst_y as usize * self.dst_stride;
        if self.y_accum == 0 {
            for x_out in 0..x_out_max {
                let j = self.frow[x_out];
                let v = mult_fix(j, self.fy_scale) as i32;
                self.dst[base + x_out] = if v > 255 { 255 } else { v as u8 };
            }
        } else {
            let b = frac((-self.y_accum) as u32, self.y_sub);
            let a = (ONE - u64::from(b)) as u32;
            for x_out in 0..x_out_max {
                let i = u64::from(a) * u64::from(self.frow[x_out])
                    + u64::from(b) * u64::from(self.irow[x_out]);
                let j = ((i + ROUNDER) >> RFIX) as u32;
                let v = mult_fix(j, self.fy_scale) as i32;
                self.dst[base + x_out] = if v > 255 { 255 } else { v as u8 };
            }
        }
    }

    /// `WebPRescalerExportRowShrink_C`: vertical area-average, carrying the leftover
    /// fraction of each column back into `irow` for the next output row.
    fn export_row_shrink(&mut self) {
        let x_out_max = self.dst_width as usize * self.num_channels;
        let base = self.dst_y as usize * self.dst_stride;
        let yscale = self.fy_scale.wrapping_mul((-self.y_accum) as u32);
        if yscale != 0 {
            for x_out in 0..x_out_max {
                let f = mult_fix_floor(self.frow[x_out], yscale) as u32;
                let v = mult_fix(self.irow[x_out].wrapping_sub(f), self.fxy_scale) as i32;
                self.dst[base + x_out] = if v > 255 { 255 } else { v as u8 };
                self.irow[x_out] = f;
            }
        } else {
            for x_out in 0..x_out_max {
                let v = mult_fix(self.irow[x_out], self.fxy_scale) as i32;
                self.dst[base + x_out] = if v > 255 { 255 } else { v as u8 };
                self.irow[x_out] = 0;
            }
        }
    }

    /// The `fxy_scale == 0` special case: `src_height == dst_height`, a 1-wide source
    /// expanded to at most 2 columns — the fraction cannot be represented, so `irow`
    /// already holds the exact bytes.
    fn export_row_special(&mut self) {
        let n = self.num_channels * self.dst_width as usize;
        let base = self.dst_y as usize * self.dst_stride;
        for i in 0..n {
            self.dst[base + i] = self.irow[i] as u8;
            self.irow[i] = 0;
        }
    }
}

/// Rescale one `num_channels`-interleaved `u8` plane from `src` (packed,
/// `src_width * num_channels` bytes per row) to a fresh `dst_width x dst_height`
/// plane, driving the [`Rescaler`] exactly as libwebp's `RescalePlane`.
#[allow(
    clippy::cast_sign_loss,
    reason = "the driver's dimensions are positive (validated by the caller), so the \
              i32 -> usize row-offset casts never lose a sign"
)]
fn rescale_plane(
    src: &[u8],
    src_width: i32,
    src_height: i32,
    num_channels: usize,
    dst_width: i32,
    dst_height: i32,
) -> Vec<u8> {
    let mut rescaler = Rescaler::new(src_width, src_height, dst_width, dst_height, num_channels);
    let src_stride = src_width as usize * num_channels;
    let mut y = 0;
    while y < src_height {
        let consumed = rescaler.import(src_height - y, &src[y as usize * src_stride..], src_stride);
        y += consumed;
        rescaler.export();
    }
    rescaler.dst
}

//------------------------------------------------------------------------------
// Public geometry operations

/// A crop rectangle in source-pixel coordinates: the `width x height` window whose
/// top-left corner sits at `(x, y)`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Rect {
    x: u32,
    y: u32,
    width: u32,
    height: u32,
}

impl Rect {
    /// A crop window of `width x height` pixels with its top-left corner at
    /// `(x, y)`. The window is validated against the source only at [`Image::crop`].
    #[must_use]
    pub const fn new(x: u32, y: u32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// The left edge, in pixels from the source's left.
    #[must_use]
    pub const fn x(self) -> u32 {
        self.x
    }

    /// The top edge, in pixels from the source's top.
    #[must_use]
    pub const fn y(self) -> u32 {
        self.y
    }

    /// The window width in pixels.
    #[must_use]
    pub const fn width(self) -> u32 {
        self.width
    }

    /// The window height in pixels.
    #[must_use]
    pub const fn height(self) -> u32 {
        self.height
    }
}

/// Copy the exact `rect` sub-window out of `image`, preserving its layout and
/// metadata and recomputing `has_alpha` over the window.
///
/// # Errors
///
/// [`Error::InvalidDimensions`] if the rectangle is empty, or
/// [`Error::CropOutOfBounds`] if it does not lie fully inside `image`.
pub(crate) fn crop(image: &Image, rect: Rect) -> Result<Image> {
    if rect.width == 0 || rect.height == 0 {
        return Err(Error::InvalidDimensions);
    }
    let right = u64::from(rect.x) + u64::from(rect.width);
    let bottom = u64::from(rect.y) + u64::from(rect.height);
    if right > u64::from(image.width()) || bottom > u64::from(image.height()) {
        return Err(Error::CropOutOfBounds);
    }
    let out_dims = Dimensions::new(rect.width, rect.height)?;

    let src_w = image.width() as usize;
    let (x, y, w, h) = (
        rect.x as usize,
        rect.y as usize,
        rect.width as usize,
        rect.height as usize,
    );
    let row_bytes = w * 4;
    let src = image.as_bytes();
    let mut out = Vec::with_capacity(row_bytes * h);
    for ry in y..y + h {
        let start = (ry * src_w + x) * 4;
        out.extend_from_slice(&src[start..start + row_bytes]);
    }

    let off = image.layout().alpha_byte_offset();
    let has_alpha = out.chunks_exact(4).any(|px| px[off] != 0xff);
    Ok(Image::from_parts(
        out_dims,
        image.layout(),
        out,
        has_alpha,
        image.metadata().clone(),
    ))
}

/// Resize `image` to `target` with the bit-exact [`Rescaler`], following
/// `WebPPictureRescale`'s ARGB path: premultiply the color channels by alpha, rescale
/// all four channels, then remove the premultiplication. Layout and metadata carry
/// through; `has_alpha` is recomputed. Opaque images are unaffected by the matting, so
/// their color channels rescale directly.
#[allow(
    clippy::cast_possible_wrap,
    reason = "validated Dimensions are in 1..=16384, well within i32, so the u32 -> i32 \
              casts handed to the rescaler never wrap"
)]
#[must_use]
pub(crate) fn resize(image: &Image, target: Dimensions) -> Image {
    let layout = image.layout();
    let src_w = image.width();
    let src_h = image.height();

    // Native ARGB, black-matted so translucent colors interpolate as libwebp's do.
    let mut argb = unpack_pixels(layout, image.as_bytes());
    for pixel in &mut argb {
        *pixel = mult_argb(*pixel, false);
    }
    // Little-endian bytes of 0xAARRGGBB are [B, G, R, A] — the exact interleaving
    // libwebp rescales when it casts its argb plane to bytes.
    let mut src_bytes = Vec::with_capacity(argb.len() * 4);
    for pixel in &argb {
        src_bytes.extend_from_slice(&pixel.to_le_bytes());
    }

    let dst_bytes = rescale_plane(
        &src_bytes,
        src_w as i32,
        src_h as i32,
        4,
        target.width() as i32,
        target.height() as i32,
    );

    let mut out_argb: Vec<u32> = dst_bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    for pixel in &mut out_argb {
        *pixel = mult_argb(*pixel, true);
    }
    let has_alpha = argb_has_alpha(&out_argb);
    let out_bytes = pack_pixels(layout, &out_argb);
    Image::from_parts(
        target,
        layout,
        out_bytes,
        has_alpha,
        image.metadata().clone(),
    )
}

#[cfg(test)]
#[allow(
    clippy::cast_possible_truncation,
    clippy::unwrap_used,
    reason = "tests: fixed small dimensions and hand-verified reference values"
)]
mod tests {
    use super::{Rect, crop, mult_argb, rescale_plane, resize};
    use crate::image::{Dimensions, Image, PixelLayout};

    fn dims(w: u32, h: u32) -> Dimensions {
        Dimensions::new(w, h).unwrap()
    }

    fn opaque(w: u32, h: u32, rgba: &[u8]) -> Image {
        Image::new(dims(w, h), PixelLayout::Rgba8, rgba.to_vec()).unwrap()
    }

    #[test]
    fn plane_identity_at_scale_one() {
        // A 3x2 single-channel plane must survive a 1:1 rescale byte-for-byte.
        let src = vec![10u8, 20, 30, 40, 50, 60];
        let out = rescale_plane(&src, 3, 2, 1, 3, 2);
        assert_eq!(out, src);
    }

    #[test]
    fn plane_downscale_two_to_one_is_the_average() {
        // [100, 200] -> one pixel = round-down average 150 (hand-computed against
        // the reference's fixed-point path).
        let out = rescale_plane(&[100, 200], 2, 1, 1, 1, 1);
        assert_eq!(out, vec![150]);
    }

    #[test]
    fn plane_downscale_matches_hand_computed_column() {
        // 1x2 -> 1x1 vertical average of [80, 200] = 140.
        let out = rescale_plane(&[80, 200], 1, 2, 1, 1, 1);
        assert_eq!(out, vec![140]);
    }

    #[test]
    fn resize_is_deterministic() {
        let img = opaque(4, 4, &(0..64u8).collect::<Vec<_>>());
        let a = resize(&img, dims(2, 2));
        let b = resize(&img, dims(2, 2));
        assert_eq!(a.as_bytes(), b.as_bytes());
        assert_eq!((a.width(), a.height()), (2, 2));
    }

    #[test]
    fn resize_identity_preserves_opaque_pixels() {
        // Opaque pixels skip the alpha matting, so a 1:1 resize is the identity.
        let src: Vec<u8> = (0..16u8).map(|v| v | 0x03).collect();
        let mut pixels = Vec::new();
        for c in src.chunks_exact(4) {
            pixels.extend_from_slice(&[c[0], c[1], c[2], 0xff]);
        }
        let img = opaque(2, 2, &pixels);
        let out = resize(&img, dims(2, 2));
        assert_eq!(out.as_bytes(), img.as_bytes());
    }

    #[test]
    fn resize_downscale_averages_each_channel() {
        // 2x1 opaque, channels [100,110,120] and [200,210,220] -> averages.
        let img = opaque(2, 1, &[100, 110, 120, 255, 200, 210, 220, 255]);
        let out = resize(&img, dims(1, 1));
        assert_eq!(out.as_bytes(), &[150, 160, 170, 255]);
        assert!(!out.has_alpha());
    }

    #[test]
    fn crop_selects_the_window() {
        // 4x4 with a per-pixel first byte = index; crop the inner 2x2 at (1,1).
        let mut pixels = Vec::new();
        for i in 0..16u8 {
            pixels.extend_from_slice(&[i, 0, 0, 255]);
        }
        let img = opaque(4, 4, &pixels);
        let out = crop(&img, Rect::new(1, 1, 2, 2)).unwrap();
        assert_eq!((out.width(), out.height()), (2, 2));
        let firsts: Vec<u8> = out.as_bytes().chunks_exact(4).map(|c| c[0]).collect();
        assert_eq!(firsts, vec![5, 6, 9, 10]);
    }

    #[test]
    fn crop_rejects_out_of_bounds_and_empty() {
        let img = opaque(4, 4, &[0u8; 64]);
        assert_eq!(
            crop(&img, Rect::new(3, 0, 2, 1)).unwrap_err(),
            crate::error::Error::CropOutOfBounds
        );
        assert_eq!(
            crop(&img, Rect::new(0, 0, 0, 1)).unwrap_err(),
            crate::error::Error::InvalidDimensions
        );
    }

    #[test]
    fn mult_argb_opaque_and_transparent_are_fixed_points() {
        // Opaque passes through; fully transparent collapses to 0; the round trip of a
        // translucent pixel is stable (premultiply then un-premultiply).
        assert_eq!(mult_argb(0xff11_2233, false), 0xff11_2233);
        assert_eq!(mult_argb(0x0012_3456, false), 0);
        let translucent = 0x8011_2233;
        let matted = mult_argb(translucent, false);
        let restored = mult_argb(matted, true);
        // Alpha lane is preserved exactly across the round trip.
        assert_eq!(restored >> 24, 0x80);
    }

    #[test]
    fn crop_recomputes_alpha_over_the_window() {
        // A 2x1 image: opaque pixel + translucent pixel. Cropping to just the opaque
        // one must clear has_alpha.
        let img = Image::new(
            dims(2, 1),
            PixelLayout::Rgba8,
            vec![1, 2, 3, 255, 4, 5, 6, 0x40],
        )
        .unwrap();
        assert!(img.has_alpha());
        let opaque_only = crop(&img, Rect::new(0, 0, 1, 1)).unwrap();
        assert!(!opaque_only.has_alpha());
    }
}
