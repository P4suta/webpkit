//! YUV 4:2:0 → RGBA conversion with fancy (bilinear) chroma upsampling,
//! byte-for-byte identical to libwebp's `WebPDecodeRGBA` default path.
//!
//! Ported functions:
//! * `dsp/yuv.h` — `MultHi`, `VP8Clip8`, `VP8YUVToR`/`VP8YUVToG`/`VP8YUVToB`,
//!   `VP8YuvToRgba` (the 19077/26149/6419/13320/8708/33050 fixed-point
//!   coefficients, the `>>8` `mulhi` and `>>6` descale, and the `[0,255]` clip).
//! * `dsp/upsampling.c` — `UpsampleRgbaLinePair_C` and its `LOAD_UV` packing plus
//!   the diagonal-weighted 9/3/3/1 bilinear filter (`diag_12`/`diag_03`).
//! * `dec/io_dec.c` — `EmitFancyRGB`, the two-line driver that mirrors the U/V
//!   samples on the first and last output rows (rows `0` and, for even heights,
//!   `height-1`, are not vertically interpolated).
//!
//! The reference upsampler emits a *pair* of output rows per call, sharing the
//! `diag_12`/`diag_03` invariants. Because every output pixel is written exactly
//! once and each `diag` value is a deterministic function of its four chroma
//! neighbors, we specialize it to a single-output-row helper
//! ([`upsample_one_row`]) parameterised by the vertically-nearer chroma row
//! (`near`, weight 3) and the farther row (`far`, weight 1). For the top output
//! row of a pair `near` is the upper chroma row and `far` the lower; for the
//! bottom row they swap. This reproduces `diag_12`/`diag_03` and the corner
//! blends bit-for-bit (verified per lane: every packed `u16` sum stays `< 2^16`,
//! so the `u`/`v` halves never contaminate each other).
//!
//! U and V are processed together stashed into a `u32` (`u` in the low 16 bits,
//! `v` in the high 16 bits) exactly as the C `LOAD_UV` macro does.
#![expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "reproduces the C uint8_t/int16_t wrapping and clip semantics of \
              the reference decoder: VP8Clip8 stores an already-clamped value \
              into a byte, and the packed-UV halves are truncated to u8 exactly \
              as libwebp's VP8YuvToRgba does"
)]
#![allow(
    clippy::similar_names,
    reason = "near_u/near_v/far_u/far_v mirror the reference sample roles \
              (top/cur, tl/l) and keep the interpolation traceable to \
              UpsampleRgbaLinePair_C"
)]

use crate::lossy::prelude::*;
use crate::lossy::work::work;

/// `_mm_mulhi_epu16` emulation (libwebp `MultHi`): keep 8 fractional bits before
/// the final descale. `v` is a byte-range sample and `coeff` a fixed constant, so
/// the product never overflows `i32`.
const fn mult_hi(v: i32, coeff: i32) -> i32 {
    (v * coeff) >> 8
}

/// libwebp `VP8Clip8`: descale by `YUV_FIX2` (`>>6`) with saturation to
/// `[0,255]`. The C tests `(v & ~YUV_MASK2) == 0` (i.e. `0 <= v <= 16383`) then
/// returns `v >> 6`, else `0`/`255`; clamping to `[0, 16383]` before the shift is
/// the identical mapping.
fn vp8_clip8(v: i32) -> u8 {
    (v.clamp(0, (256 << 6) - 1) >> 6) as u8
}

/// libwebp `VP8YUVToR`: red channel from luma `y` and chroma `v`.
fn vp8_yuv_to_r(y: i32, v: i32) -> u8 {
    vp8_clip8(mult_hi(y, 19077) + mult_hi(v, 26149) - 14234)
}

/// libwebp `VP8YUVToG`: green channel from luma `y` and chroma `u`, `v`.
fn vp8_yuv_to_g(y: i32, u: i32, v: i32) -> u8 {
    vp8_clip8(mult_hi(y, 19077) - mult_hi(u, 6419) - mult_hi(v, 13320) + 8708)
}

/// libwebp `VP8YUVToB`: blue channel from luma `y` and chroma `u`.
fn vp8_yuv_to_b(y: i32, u: i32) -> u8 {
    vp8_clip8(mult_hi(y, 19077) + mult_hi(u, 33050) - 17685)
}

/// libwebp `VP8YuvToRgba`: write one opaque RGBA pixel into `dst[0..4]`.
fn yuv_to_rgba(y: i32, u: i32, v: i32, dst: &mut [u8]) {
    dst[0] = vp8_yuv_to_r(y, v);
    dst[1] = vp8_yuv_to_g(y, u, v);
    dst[2] = vp8_yuv_to_b(y, u);
    dst[3] = 0xff;
}

/// libwebp `LOAD_UV`: pack a `u`/`v` sample pair into one `u32` (`u` low 16 bits,
/// `v` high 16 bits) so both channels share the interpolation arithmetic.
fn load_uv(u: u8, v: u8) -> u32 {
    u32::from(u) | (u32::from(v) << 16)
}

/// Unpack a packed-UV `u32` and store the resulting RGBA pixel at column `col`.
/// Mirrors the C `FUNC(y, uv & 0xff, uv >> 16, dst + col * XSTEP)`.
fn emit_pixel(y: u8, uv: u32, dst: &mut [u8], col: usize) {
    let u = i32::from((uv & 0xff) as u8);
    let v = i32::from((uv >> 16) as u8);
    yuv_to_rgba(i32::from(y), u, v, &mut dst[col * 4..col * 4 + 4]);
}

/// Produce one fully-upsampled RGBA output row of `len` pixels from the luma row
/// `y_row` and the two chroma rows straddling it vertically: `near`/`far` carry
/// vertical weights 3/1. This is the per-row specialization of the top (or, with
/// `near`/`far` swapped, bottom) half of `UpsampleRgbaLinePair_C`.
///
/// The first and last columns mirror the horizontal boundary
/// (`(3*near + far + 2) >> 2`); interior pixel pairs use the diagonal-weighted
/// blend (`diag_12`/`diag_03` here reconstructed as `d_l`/`d_r`).
fn upsample_one_row(
    y_row: &[u8],
    dst: &mut [u8],
    near_u: &[u8],
    near_v: &[u8],
    far_u: &[u8],
    far_v: &[u8],
    len: usize,
) {
    work!(UpsampleRow);
    let last_pixel_pair = (len - 1) >> 1;
    // The packed near/far UV of the pair's left column, carried across iterations:
    // each pixel pair's right column becomes the next pair's left, so both planes
    // are packed once per chroma column instead of twice.
    let mut near_l = load_uv(near_u[0], near_v[0]);
    let mut far_l = load_uv(far_u[0], far_v[0]);
    // Column 0: mirror the left edge, then blend vertically 3:1.
    let first = (3 * near_l + far_l + 0x0002_0002) >> 2;
    emit_pixel(y_row[0], first, dst, 0);
    // Interior pixel pairs (output columns 2x-1 and 2x).
    for x in 1..=last_pixel_pair {
        let near_r = load_uv(near_u[x], near_v[x]);
        let far_r = load_uv(far_u[x], far_v[x]);
        let d_l = (near_l + 3 * near_r + 3 * far_l + far_r + 0x0008_0008) >> 3;
        let d_r = (3 * near_l + near_r + far_l + 3 * far_r + 0x0008_0008) >> 3;
        emit_pixel(y_row[2 * x - 1], (d_l + near_l) >> 1, dst, 2 * x - 1);
        emit_pixel(y_row[2 * x], (d_r + near_r) >> 1, dst, 2 * x);
        near_l = near_r;
        far_l = far_r;
    }
    // Even width: mirror the right edge for the final column. After the loop
    // `near_l`/`far_l` already hold the packed UV of column `last_pixel_pair`.
    if len & 1 == 0 {
        let last = (3 * near_l + far_l + 0x0002_0002) >> 2;
        emit_pixel(y_row[len - 1], last, dst, len - 1);
    }
}

/// Borrowed plane origins and pitches of a padded YUV 4:2:0 frame, bundling the
/// arguments shared by the whole-frame and row-streaming upsamplers. `y` points
/// at the luma origin (`y_stride`-pitched, one row per picture row); `u`/`v`
/// point at the chroma origins (`uv_stride`-pitched, `(height + 1) / 2` rows,
/// each row holding at least `(width + 1) / 2` samples).
pub(crate) struct Yuv420Ref<'a> {
    /// Luma plane origin.
    pub y: &'a [u8],
    /// Luma row pitch in bytes.
    pub y_stride: usize,
    /// U chroma plane origin.
    pub u: &'a [u8],
    /// V chroma plane origin.
    pub v: &'a [u8],
    /// Chroma row pitch in bytes.
    pub uv_stride: usize,
}

/// Produce output row `out_y` (`width` RGBA pixels into `out`, `width * 4` bytes)
/// of a fancy-upsampled YUV 4:2:0 frame `src`. This is the single-output-row
/// specialization of [`yuv420_to_rgba`]'s `EmitFancyRGB` schedule: it picks the
/// two chroma rows straddling `out_y` exactly as the reference two-line driver
/// does. Output row `0` and, for an even height, the final row mirror a single
/// chroma row (`near == far`); every interior row straddles a chroma pair with
/// `near` (weight 3) the vertically-closer row. Each output row depends only on
/// the planes (no inter-row state), so a row-streaming decoder can call this the
/// instant a row's chroma is finalized and get bytes identical to the whole-frame
/// pass. The caller guarantees `src` spans `out_y`'s luma row and its near/far
/// chroma rows.
pub(crate) fn upsample_output_row(
    src: &Yuv420Ref<'_>,
    width: usize,
    height: usize,
    out_y: usize,
    out: &mut [u8],
) {
    let chroma_rows = height.div_ceil(2);
    // Chroma rows feeding this output row (`near` = weight 3, `far` = weight 1),
    // matching the whole-frame driver's row-0 / interior-pair / last-row cases.
    let (near, far) = if out_y == 0 {
        (0, 0)
    } else if out_y & 1 == 1 {
        let near = (out_y - 1) / 2;
        let far = out_y.div_ceil(2);
        // Even-height final row: `far` runs off the bottom, so mirror `near`.
        (near, if far < chroma_rows { far } else { near })
    } else {
        (out_y / 2, out_y / 2 - 1)
    };
    let (near_off, far_off) = (near * src.uv_stride, far * src.uv_stride);
    upsample_one_row(
        &src.y[out_y * src.y_stride..],
        out,
        &src.u[near_off..],
        &src.v[near_off..],
        &src.u[far_off..],
        &src.v[far_off..],
        width,
    );
}

/// Convert a full YUV 4:2:0 frame `src` to a `width * height * 4` RGBA buffer
/// (alpha = `255`), byte-identical to `WebPDecodeRGBA` with fancy upsampling.
///
/// Reproduces the whole-frame `EmitFancyRGB` schedule (single call, `mb_y = 0`,
/// `mb_h = height`, no crop, no scaling) by emitting each output row through
/// [`upsample_output_row`].
pub(crate) fn yuv420_to_rgba(src: &Yuv420Ref<'_>, width: usize, height: usize) -> Vec<u8> {
    let out_stride = width * 4;
    let mut out = vec![0u8; out_stride * height];
    if width == 0 || height == 0 {
        return out;
    }
    emit_rows(src, width, height, out_stride, &mut out);
    out
}

/// Emit every output row serially. Each row is a self-contained function of the
/// planes (see [`upsample_output_row`]), so this is also the byte-for-byte
/// reference the `rayon` path must reproduce.
fn emit_rows_serial(
    src: &Yuv420Ref<'_>,
    width: usize,
    height: usize,
    out_stride: usize,
    out: &mut [u8],
) {
    for out_y in 0..height {
        let (row_start, row_end) = (out_y * out_stride, (out_y + 1) * out_stride);
        upsample_output_row(src, width, height, out_y, &mut out[row_start..row_end]);
    }
}

/// Emit all output rows (serial build).
#[cfg(not(feature = "rayon"))]
fn emit_rows(src: &Yuv420Ref<'_>, width: usize, height: usize, out_stride: usize, out: &mut [u8]) {
    emit_rows_serial(src, width, height, out_stride, out);
}

/// Emit all output rows, upsampling independent rows across the rayon pool. Output
/// rows share only the immutable planes and write disjoint `out_stride` chunks, so
/// the result is identical to [`emit_rows_serial`] regardless of scheduling. Small
/// frames stay serial to skip thread-pool overhead that would dwarf the work.
#[cfg(feature = "rayon")]
fn emit_rows(src: &Yuv420Ref<'_>, width: usize, height: usize, out_stride: usize, out: &mut [u8]) {
    use rayon::prelude::*;
    // ~256×256; below this the fork/join cost outweighs the per-row upsample.
    const PAR_MIN_PIXELS: usize = 1 << 16;
    if width.saturating_mul(height) < PAR_MIN_PIXELS {
        emit_rows_serial(src, width, height, out_stride, out);
        return;
    }
    out.par_chunks_mut(out_stride)
        .enumerate()
        .for_each(|(out_y, row)| upsample_output_row(src, width, height, out_y, row));
}

#[cfg(test)]
mod tests {
    use super::{Yuv420Ref, yuv420_to_rgba};
    use crate::lossy::prelude::*;

    /// Build constant single-value YUV planes for a `width`×`height` frame.
    fn constant_frame(
        width: usize,
        height: usize,
        yv: u8,
        uv: u8,
        vv: u8,
    ) -> (Vec<u8>, Vec<u8>, Vec<u8>, usize, usize) {
        let uv_w = width.div_ceil(2);
        let uv_h = height.div_ceil(2);
        let y = vec![yv; width * height];
        let u = vec![uv; uv_w * uv_h];
        let v = vec![vv; uv_w * uv_h];
        (y, u, v, width, uv_w)
    }

    /// Assert every pixel of a constant-input frame equals `expected` RGBA.
    fn assert_constant(width: usize, height: usize, yv: u8, uv: u8, vv: u8, expected: [u8; 4]) {
        let (y, u, v, w, uv_w) = constant_frame(width, height, yv, uv, vv);
        let out = yuv420_to_rgba(
            &Yuv420Ref {
                y: &y,
                y_stride: w,
                u: &u,
                v: &v,
                uv_stride: uv_w,
            },
            width,
            height,
        );
        assert_eq!(out.len(), width * height * 4);
        for (i, px) in out.chunks_exact(4).enumerate() {
            assert_eq!(px, expected, "pixel {i} of {width}x{height}");
        }
    }

    /// Fill `n` bytes with a cast-free wrapping pseudo-random ramp (distinct seed
    /// and multiplier per plane) so a row/plane mix-up in the parallel emitter
    /// changes the bytes.
    #[cfg(feature = "rayon")]
    fn ramp(n: usize, seed: u8, mul: u8) -> Vec<u8> {
        let mut s = seed;
        (0..n)
            .map(|_| {
                s = s.wrapping_mul(mul).wrapping_add(0x2b);
                s
            })
            .collect()
    }

    // The rayon row-parallel emitter must be byte-identical to the serial
    // reference. Uses a frame above `PAR_MIN_PIXELS` so `yuv420_to_rgba` actually
    // takes the parallel branch, and varied planes so a scheduling or row-index
    // bug would diverge from the serial fallback.
    #[cfg(feature = "rayon")]
    #[test]
    fn rayon_row_parallel_matches_serial_byte_for_byte() {
        use super::emit_rows_serial;
        let (width, height) = (260usize, 258usize);
        let uv_w = width.div_ceil(2);
        let uv_h = height.div_ceil(2);
        let y = ramp(width * height, 1, 7);
        let u = ramp(uv_w * uv_h, 99, 5);
        let v = ramp(uv_w * uv_h, 200, 11);
        let src = Yuv420Ref {
            y: &y,
            y_stride: width,
            u: &u,
            v: &v,
            uv_stride: uv_w,
        };
        let parallel = yuv420_to_rgba(&src, width, height);
        let mut serial = vec![0u8; width * 4 * height];
        emit_rows_serial(&src, width, height, width * 4, &mut serial);
        assert_eq!(
            parallel, serial,
            "rayon emitter must match serial byte-for-byte"
        );
    }

    #[test]
    fn zero_width_nonzero_height_returns_empty_without_panic() {
        // The empty-frame guard is `width == 0 || height == 0`. A zero-width but
        // nonzero-height frame must still early-return the (empty) buffer. Flipping
        // the `||` to `&&` lets this case fall through to the row emitter, which
        // indexes empty chroma planes and underflows `len - 1` in `upsample_one_row`
        // -> panic. Asserting the clean empty return kills the `|| -> &&` mutant.
        let (y, u, v, w, uv_w) = constant_frame(0, 2, 0, 0, 0);
        let out = yuv420_to_rgba(
            &Yuv420Ref {
                y: &y,
                y_stride: w,
                u: &u,
                v: &v,
                uv_stride: uv_w,
            },
            0,
            2,
        );
        assert!(out.is_empty(), "zero-width frame must produce no bytes");
    }

    #[test]
    fn all_zero_yuv_is_green() {
        // Y=U=V=0: R=clip8(-14234)=0, G=clip8(8708)=136, B=clip8(-17685)=0.
        for &(w, h) in &[(1, 1), (2, 2), (3, 3), (4, 4), (5, 5), (7, 3)] {
            assert_constant(w, h, 0, 0, 0, [0, 136, 0, 255]);
        }
    }

    #[test]
    fn neutral_gray_is_130() {
        // Y=128, U=V=128 (neutral chroma) -> studio-swing gray 1.164*112 = 130.
        for &(w, h) in &[(1, 1), (2, 2), (3, 3), (4, 4), (5, 5), (6, 4), (3, 6)] {
            assert_constant(w, h, 128, 128, 128, [130, 130, 130, 255]);
        }
    }

    #[test]
    fn full_luma_neutral_chroma_is_white() {
        // Y=255, U=V=128: every channel saturates to 255.
        assert_constant(4, 4, 255, 128, 128, [255, 255, 255, 255]);
    }

    #[test]
    fn vertical_gradient_orients_near_far_correctly() {
        // width=2 (one chroma column), height=4 (two chroma rows) with a chroma
        // step U: row0=0, row1=100 and neutral V=128, full luma Y=255.
        // Fancy vertical upsampling yields per-row U = [0, 25, 75, 100]:
        //   row0 = mirror(C0)            = (3*0   + 0   + 2) >> 2 = 0
        //   row1 = (3*C0 + C1 + 2) >> 2  = (0     + 100 + 2) >> 2 = 25
        //   row2 = (3*C1 + C0 + 2) >> 2  = (300   + 0   + 2) >> 2 = 75
        //   row3 = mirror(C1)            = (3*100 + 100 + 2) >> 2 = 100
        // With Y=255, V=128, R and G saturate to 255; only B = VP8YUVToB(255, U)
        // varies: [20, 71, 171, 222]. A swapped near/far would flip rows 1<->2.
        let width = 2;
        let height = 4;
        let y = vec![255u8; width * height];
        let u = vec![0u8, 100u8]; // uv_w=1, uv_h=2: row0=0, row1=100
        let v = vec![128u8, 128u8];
        let out = yuv420_to_rgba(
            &Yuv420Ref {
                y: &y,
                y_stride: width,
                u: &u,
                v: &v,
                uv_stride: 1,
            },
            width,
            height,
        );
        let expected_b = [20u8, 71u8, 171u8, 222u8];
        for (row, &b) in expected_b.iter().enumerate() {
            for col in 0..width {
                let px = &out[row * width * 4 + col * 4..row * width * 4 + col * 4 + 4];
                assert_eq!(px, [255, 255, b, 255], "row {row} col {col}");
            }
        }
    }

    #[test]
    fn horizontal_gradient_blends_interior_pairs_and_mirrors_the_edge() {
        // width=4 (two chroma columns), height=1 (one chroma row): a horizontal
        // chroma step U col0=0, col1=100 with neutral V=128 and full luma Y=255.
        // With near==far (single row) the horizontal fancy filter reduces to
        // (per U channel, C0=0, C1=100):
        //   col0 = mirror(C0)             = (3*0   + 0   + 2) >> 2 = 0
        //   diag = (4*C0 + 4*C1 + 8) >> 3 = (0 + 400 + 8) >> 3     = 51
        //   col1 = (diag + C0) >> 1       = (51 + 0)   >> 1        = 25
        //   col2 = (diag + C1) >> 1       = (51 + 100) >> 1        = 75
        //   col3 = mirror(C1) (even width)= (3*100 + 100 + 2) >> 2 = 100
        // giving U = [0, 25, 75, 100]. R,G saturate (Y=255, V=128); only
        // B = VP8YUVToB(255, U) varies: B = [20, 71, 171, 222] (same mapping the
        // vertical test verified). A broken interior diagonal, a swapped
        // col1/col2 near-sample, or a missing even-column mirror changes cols 1..3.
        let width = 4;
        let height = 1;
        let y = vec![255u8; width * height];
        let u = vec![0u8, 100u8]; // uv_w = 2, uv_h = 1
        let v = vec![128u8, 128u8];
        let out = yuv420_to_rgba(
            &Yuv420Ref {
                y: &y,
                y_stride: width,
                u: &u,
                v: &v,
                uv_stride: 2,
            },
            width,
            height,
        );
        let expected_b = [20u8, 71u8, 171u8, 222u8];
        for (col, &b) in expected_b.iter().enumerate() {
            let px = &out[col * 4..col * 4 + 4];
            assert_eq!(px, [255, 255, b, 255], "even-width col {col}");
        }

        // Odd width (3) takes the no-mirror branch: the last column comes from the
        // interior pair, so U = [0, 25, 75] and B = [20, 71, 171].
        let y3 = vec![255u8; 3];
        let out3 = yuv420_to_rgba(
            &Yuv420Ref {
                y: &y3,
                y_stride: 3,
                u: &u,
                v: &v,
                uv_stride: 2,
            },
            3,
            1,
        );
        let expected_b3 = [20u8, 71u8, 171u8];
        for (col, &b) in expected_b3.iter().enumerate() {
            let px = &out3[col * 4..col * 4 + 4];
            assert_eq!(px, [255, 255, b, 255], "odd-width col {col}");
        }
    }
}
