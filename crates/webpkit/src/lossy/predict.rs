//! VP8 intra prediction (RFC 6386 §12.2–12.3, transcribed from libwebp
//! `dsp/dec.c`).
//!
//! Ports every reference predictor: the 16×16 luma set (`DC16_C`,
//! `DC16NoTop_C`, `DC16NoLeft_C`, `DC16NoTopLeft_C`, `VE16_C`, `HE16_C`,
//! `TM16_C`), the 8×8 chroma set (`DC8uv_C`, `DC8uvNoTop_C`, `DC8uvNoLeft_C`,
//! `DC8uvNoTopLeft_C`, `VE8uv_C`, `HE8uv_C`, `TM8uv_C`) and all ten 4×4 modes
//! (`DC4_C`, `TM4_C`, `VE4_C`, `HE4_C`, `RD4_C`, `VR4_C`, `LD4_C`, `VL4_C`,
//! `HD4_C`, `HU4_C`) built on the `AVG2`/`AVG3` macros and the shared
//! `TrueMotion` helper.
//!
//! # Buffer model
//!
//! libwebp's `uint8_t* dst` with a fixed `BPS` stride becomes a row-major
//! `plane: &mut [u8]` with an explicit `stride`. The block's pixel at column
//! `x`, row `y` is `plane[off + x + y * stride]`; the C's negative neighbor
//! reads (`dst[-1 + y*BPS]`, `dst[x - BPS]`, `dst[-1 - BPS]`, `dst[4 + x - BPS]`)
//! translate to `plane[off + y*stride - 1]`, `plane[off - stride + x]`,
//! `plane[off - stride - 1]` and `plane[off - stride + 4 + x]`. The caller
//! guarantees a top border row and left/right border columns, so these indices
//! never underflow. Predictors *write* the predicted samples into the block.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "reproduces the C uint8_t/int16_t wrapping and clip semantics of the \
              reference decoder; every AVG/DC value is provably in 0..=255 before the cast"
)]
#![allow(
    clippy::many_single_char_names,
    reason = "A..L and X are the canonical RFC 6386 / libwebp neighbor-sample labels; \
              preserving them keeps this transcription auditable against dsp/dec.c"
)]

use crate::lossy::constants::{
    B_DC_PRED, B_HD_PRED, B_HE_PRED, B_HU_PRED, B_LD_PRED, B_RD_PRED, B_TM_PRED, B_VE_PRED,
    B_VL_PRED, B_VR_PRED, DC_PRED, H_PRED, TM_PRED, V_PRED,
};
use crate::lossy::work::work;

// -----------------------------------------------------------------------------
// Shared helpers
// -----------------------------------------------------------------------------

/// Clip an `i32` sample to `0..=255` and narrow to `u8` — libwebp's `VP8kclip1`
/// / `clip_8b` store pattern.
pub(crate) fn clip8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// libwebp `AVG3(a, b, c) = (uint8_t)((a + 2*b + c + 2) >> 2)`: the 3-tap
/// rounded average used by the diagonal 4×4 predictors.
pub(crate) const fn avg3(a: i32, b: i32, c: i32) -> u8 {
    ((a + 2 * b + c + 2) >> 2) as u8
}

/// libwebp `AVG2(a, b) = (a + b + 1) >> 1`: the 2-tap rounded average.
pub(crate) const fn avg2(a: i32, b: i32) -> u8 {
    ((a + b + 1) >> 1) as u8
}

/// Store `v` at block coordinate `(x, y)` — the C `DST(x, y) = dst[x + y*BPS]`.
pub(crate) fn put(plane: &mut [u8], off: usize, stride: usize, x: usize, y: usize, v: u8) {
    plane[off + x + y * stride] = v;
}

/// Fill a `size`×`size` block at `off` with the constant `v` — the `Put16` /
/// `Put8x8uv` / `DC4` memset pattern.
pub(crate) fn fill_square(plane: &mut [u8], off: usize, stride: usize, size: usize, v: u8) {
    for j in 0..size {
        let base = off + j * stride;
        plane[base..base + size].fill(v);
    }
}

/// libwebp `TrueMotion`: `dst[x,y] = clip8(top[x] + left[y] - top_left)` over a
/// `size`×`size` block. Shared by `TM4_C`, `TM8uv_C` and `TM16_C`.
pub(crate) fn true_motion(plane: &mut [u8], off: usize, stride: usize, size: usize) {
    let top_left = i32::from(plane[off - stride - 1]);
    // Gather the `size`-wide top row into a local before writing any block pixel.
    // `plane` is a single `&mut [u8]` that is both read (the top row at
    // `off - stride + x`) and written (the block at `off + y*stride + x`); for an
    // unknown runtime `stride` the compiler cannot prove those never overlap, so
    // against the live slice it must reload the top sample every iteration (256
    // reloads for a 16×16 block). Copying the top row into `top` (which the block
    // writes provably cannot alias) breaks that dependency, so each row is a
    // straight-line map — ~7× faster at 16×16 (`just bench-kernels true_motion`),
    // still scalar (no packed op; the clamp+narrow does not vectorize on baseline
    // SSE2, `--emit asm`). The top row sits above the block and no write ever touches
    // it, so the gather is byte-identical to reading `plane[off - stride + x]` in
    // place. `size <= 16`, so the fixed `[u8; 16]` holds it; only `top[..size]` is
    // used.
    let mut top = [0u8; 16];
    top[..size].copy_from_slice(&plane[off - stride..off - stride + size]);
    for y in 0..size {
        // `left` (the border column at `base - 1`) is never written by this block, so
        // reading it fresh per row is unaffected by earlier rows' stores.
        let left = i32::from(plane[off + y * stride - 1]);
        let base = off + y * stride;
        for (out, &t) in plane[base..base + size].iter_mut().zip(&top[..size]) {
            *out = clip8(i32::from(t) + left - top_left);
        }
    }
}

/// The pre-optimization [`true_motion`] verbatim: it reads each top sample straight
/// from the aliasable `plane` slice inside the inner loop. The gathered-top
/// [`true_motion`] must reconstruct the block byte for byte (the top row is never
/// written, so the gather cannot observe a different value). Compiled only for the
/// equivalence proptest (`test`) and the `kernels` microbench (`bench` feature).
#[cfg(any(test, feature = "bench"))]
pub(crate) fn true_motion_reference(plane: &mut [u8], off: usize, stride: usize, size: usize) {
    let top_left = i32::from(plane[off - stride - 1]);
    for y in 0..size {
        let left = i32::from(plane[off + y * stride - 1]);
        let base = off + y * stride;
        for x in 0..size {
            let top = i32::from(plane[off - stride + x]);
            plane[base + x] = clip8(top + left - top_left);
        }
    }
}

// -----------------------------------------------------------------------------
// 16×16 luma
// -----------------------------------------------------------------------------

/// libwebp `VE16_C`: copy the 16-sample top row into every row.
pub(crate) fn ve16(plane: &mut [u8], off: usize, stride: usize) {
    let mut top = [0u8; 16];
    top.copy_from_slice(&plane[off - stride..off - stride + 16]);
    for j in 0..16 {
        let base = off + j * stride;
        plane[base..base + 16].copy_from_slice(&top);
    }
}

/// libwebp `HE16_C`: fill each row with that row's left neighbor.
pub(crate) fn he16(plane: &mut [u8], off: usize, stride: usize) {
    for j in 0..16 {
        let base = off + j * stride;
        let v = plane[base - 1];
        plane[base..base + 16].fill(v);
    }
}

/// libwebp `DC16_C`: average of the 16 top and 16 left samples (rounder 16, >>5).
pub(crate) fn dc16_both(plane: &[u8], off: usize, stride: usize) -> u8 {
    let mut dc = 16i32;
    for j in 0..16 {
        dc += i32::from(plane[off - stride + j]) + i32::from(plane[off + j * stride - 1]);
    }
    (dc >> 5) as u8
}

/// libwebp `DC16NoLeft_C`: average of the 16 top samples only (rounder 8, >>4).
pub(crate) fn dc16_top(plane: &[u8], off: usize, stride: usize) -> u8 {
    let mut dc = 8i32;
    for &t in &plane[off - stride..off - stride + 16] {
        dc += i32::from(t);
    }
    (dc >> 4) as u8
}

/// libwebp `DC16NoTop_C`: average of the 16 left samples only (rounder 8, >>4).
pub(crate) fn dc16_left(plane: &[u8], off: usize, stride: usize) -> u8 {
    let mut dc = 8i32;
    for j in 0..16 {
        dc += i32::from(plane[off + j * stride - 1]);
    }
    (dc >> 4) as u8
}

/// The 16×16 DC predictor, selecting the `DC16`/`NoTop`/`NoLeft`/`NoTopLeft`
/// variant from sample availability exactly as libwebp remaps `DC_PRED` at the
/// frame edges (no top → left-only, no left → top-only, neither → `0x80`).
pub(crate) fn dc16(plane: &mut [u8], off: usize, stride: usize, has_top: bool, has_left: bool) {
    let v = match (has_top, has_left) {
        (true, true) => dc16_both(plane, off, stride),
        (true, false) => dc16_top(plane, off, stride),
        (false, true) => dc16_left(plane, off, stride),
        (false, false) => 0x80,
    };
    fill_square(plane, off, stride, 16, v);
}

// -----------------------------------------------------------------------------
// 8×8 chroma
// -----------------------------------------------------------------------------

/// libwebp `VE8uv_C`: copy the 8-sample top row into every row.
pub(crate) fn ve8(plane: &mut [u8], off: usize, stride: usize) {
    let mut top = [0u8; 8];
    top.copy_from_slice(&plane[off - stride..off - stride + 8]);
    for j in 0..8 {
        let base = off + j * stride;
        plane[base..base + 8].copy_from_slice(&top);
    }
}

/// libwebp `HE8uv_C`: fill each row with that row's left neighbor.
pub(crate) fn he8(plane: &mut [u8], off: usize, stride: usize) {
    for j in 0..8 {
        let base = off + j * stride;
        let v = plane[base - 1];
        plane[base..base + 8].fill(v);
    }
}

/// libwebp `DC8uv_C`: average of the 8 top and 8 left samples (rounder 8, >>4).
pub(crate) fn dc8_both(plane: &[u8], off: usize, stride: usize) -> u8 {
    let mut dc = 8i32;
    for j in 0..8 {
        dc += i32::from(plane[off - stride + j]) + i32::from(plane[off + j * stride - 1]);
    }
    (dc >> 4) as u8
}

/// libwebp `DC8uvNoLeft_C`: average of the 8 top samples only (rounder 4, >>3).
pub(crate) fn dc8_top(plane: &[u8], off: usize, stride: usize) -> u8 {
    let mut dc = 4i32;
    for &t in &plane[off - stride..off - stride + 8] {
        dc += i32::from(t);
    }
    (dc >> 3) as u8
}

/// libwebp `DC8uvNoTop_C`: average of the 8 left samples only (rounder 4, >>3).
pub(crate) fn dc8_left(plane: &[u8], off: usize, stride: usize) -> u8 {
    let mut dc = 4i32;
    for j in 0..8 {
        dc += i32::from(plane[off + j * stride - 1]);
    }
    (dc >> 3) as u8
}

/// The 8×8 chroma DC predictor, selecting the `DC8uv`/`NoTop`/`NoLeft`/
/// `NoTopLeft` variant from sample availability (mirrors [`dc16`]).
pub(crate) fn dc8(plane: &mut [u8], off: usize, stride: usize, has_top: bool, has_left: bool) {
    let v = match (has_top, has_left) {
        (true, true) => dc8_both(plane, off, stride),
        (true, false) => dc8_top(plane, off, stride),
        (false, true) => dc8_left(plane, off, stride),
        (false, false) => 0x80,
    };
    fill_square(plane, off, stride, 8, v);
}

// -----------------------------------------------------------------------------
// 4×4 luma
// -----------------------------------------------------------------------------

/// libwebp `VE4_C`: vertical, each column an `AVG3` of the three top samples
/// above and around it (reads one top-left and one top-right sample).
pub(crate) fn ve4(plane: &mut [u8], off: usize, stride: usize) {
    let tl = i32::from(plane[off - stride - 1]);
    let t0 = i32::from(plane[off - stride]);
    let t1 = i32::from(plane[off - stride + 1]);
    let t2 = i32::from(plane[off - stride + 2]);
    let t3 = i32::from(plane[off - stride + 3]);
    let t4 = i32::from(plane[off - stride + 4]);
    let vals = [
        avg3(tl, t0, t1),
        avg3(t0, t1, t2),
        avg3(t1, t2, t3),
        avg3(t2, t3, t4),
    ];
    for j in 0..4 {
        let base = off + j * stride;
        plane[base..base + 4].copy_from_slice(&vals);
    }
}

/// libwebp `HE4_C`: horizontal, each row a constant `AVG3` of neighboring left
/// samples (top-left `A` through left `E`).
pub(crate) fn he4(plane: &mut [u8], off: usize, stride: usize) {
    let a = i32::from(plane[off - stride - 1]);
    let b = i32::from(plane[off - 1]);
    let c = i32::from(plane[off + stride - 1]);
    let d = i32::from(plane[off + 2 * stride - 1]);
    let e = i32::from(plane[off + 3 * stride - 1]);
    fill_square4_row(plane, off, avg3(a, b, c));
    fill_square4_row(plane, off + stride, avg3(b, c, d));
    fill_square4_row(plane, off + 2 * stride, avg3(c, d, e));
    fill_square4_row(plane, off + 3 * stride, avg3(d, e, e));
}

/// Fill four consecutive samples at `base` with `v` (the `HE4` per-row store).
fn fill_square4_row(plane: &mut [u8], base: usize, v: u8) {
    plane[base..base + 4].fill(v);
}

/// libwebp `DC4_C`: average of the 4 top and 4 left samples (rounder 4, >>3),
/// always the both-available form.
pub(crate) fn dc4(plane: &mut [u8], off: usize, stride: usize) {
    let mut dc = 4i32;
    for j in 0..4 {
        dc += i32::from(plane[off - stride + j]) + i32::from(plane[off + j * stride - 1]);
    }
    fill_square(plane, off, stride, 4, (dc >> 3) as u8);
}

/// libwebp `RD4_C`: down-right diagonal.
pub(crate) fn rd4(plane: &mut [u8], off: usize, stride: usize) {
    let i = i32::from(plane[off - 1]);
    let j = i32::from(plane[off + stride - 1]);
    let k = i32::from(plane[off + 2 * stride - 1]);
    let l = i32::from(plane[off + 3 * stride - 1]);
    let x = i32::from(plane[off - stride - 1]);
    let a = i32::from(plane[off - stride]);
    let b = i32::from(plane[off - stride + 1]);
    let c = i32::from(plane[off - stride + 2]);
    let d = i32::from(plane[off - stride + 3]);
    put(plane, off, stride, 0, 3, avg3(j, k, l));
    let v = avg3(i, j, k);
    put(plane, off, stride, 1, 3, v);
    put(plane, off, stride, 0, 2, v);
    let v = avg3(x, i, j);
    put(plane, off, stride, 2, 3, v);
    put(plane, off, stride, 1, 2, v);
    put(plane, off, stride, 0, 1, v);
    let v = avg3(a, x, i);
    put(plane, off, stride, 3, 3, v);
    put(plane, off, stride, 2, 2, v);
    put(plane, off, stride, 1, 1, v);
    put(plane, off, stride, 0, 0, v);
    let v = avg3(b, a, x);
    put(plane, off, stride, 3, 2, v);
    put(plane, off, stride, 2, 1, v);
    put(plane, off, stride, 1, 0, v);
    let v = avg3(c, b, a);
    put(plane, off, stride, 3, 1, v);
    put(plane, off, stride, 2, 0, v);
    put(plane, off, stride, 3, 0, avg3(d, c, b));
}

/// libwebp `LD4_C`: down-left diagonal (reads the four top-right samples).
pub(crate) fn ld4(plane: &mut [u8], off: usize, stride: usize) {
    let a = i32::from(plane[off - stride]);
    let b = i32::from(plane[off - stride + 1]);
    let c = i32::from(plane[off - stride + 2]);
    let d = i32::from(plane[off - stride + 3]);
    let e = i32::from(plane[off - stride + 4]);
    let f = i32::from(plane[off - stride + 5]);
    let g = i32::from(plane[off - stride + 6]);
    let h = i32::from(plane[off - stride + 7]);
    put(plane, off, stride, 0, 0, avg3(a, b, c));
    let v = avg3(b, c, d);
    put(plane, off, stride, 1, 0, v);
    put(plane, off, stride, 0, 1, v);
    let v = avg3(c, d, e);
    put(plane, off, stride, 2, 0, v);
    put(plane, off, stride, 1, 1, v);
    put(plane, off, stride, 0, 2, v);
    let v = avg3(d, e, f);
    put(plane, off, stride, 3, 0, v);
    put(plane, off, stride, 2, 1, v);
    put(plane, off, stride, 1, 2, v);
    put(plane, off, stride, 0, 3, v);
    let v = avg3(e, f, g);
    put(plane, off, stride, 3, 1, v);
    put(plane, off, stride, 2, 2, v);
    put(plane, off, stride, 1, 3, v);
    let v = avg3(f, g, h);
    put(plane, off, stride, 3, 2, v);
    put(plane, off, stride, 2, 3, v);
    put(plane, off, stride, 3, 3, avg3(g, h, h));
}

/// libwebp `VR4_C`: vertical-right diagonal.
pub(crate) fn vr4(plane: &mut [u8], off: usize, stride: usize) {
    let i = i32::from(plane[off - 1]);
    let j = i32::from(plane[off + stride - 1]);
    let k = i32::from(plane[off + 2 * stride - 1]);
    let x = i32::from(plane[off - stride - 1]);
    let a = i32::from(plane[off - stride]);
    let b = i32::from(plane[off - stride + 1]);
    let c = i32::from(plane[off - stride + 2]);
    let d = i32::from(plane[off - stride + 3]);
    let v = avg2(x, a);
    put(plane, off, stride, 0, 0, v);
    put(plane, off, stride, 1, 2, v);
    let v = avg2(a, b);
    put(plane, off, stride, 1, 0, v);
    put(plane, off, stride, 2, 2, v);
    let v = avg2(b, c);
    put(plane, off, stride, 2, 0, v);
    put(plane, off, stride, 3, 2, v);
    put(plane, off, stride, 3, 0, avg2(c, d));
    put(plane, off, stride, 0, 3, avg3(k, j, i));
    put(plane, off, stride, 0, 2, avg3(j, i, x));
    let v = avg3(i, x, a);
    put(plane, off, stride, 0, 1, v);
    put(plane, off, stride, 1, 3, v);
    let v = avg3(x, a, b);
    put(plane, off, stride, 1, 1, v);
    put(plane, off, stride, 2, 3, v);
    let v = avg3(a, b, c);
    put(plane, off, stride, 2, 1, v);
    put(plane, off, stride, 3, 3, v);
    put(plane, off, stride, 3, 1, avg3(b, c, d));
}

/// libwebp `VL4_C`: vertical-left diagonal (reads the four top-right samples).
pub(crate) fn vl4(plane: &mut [u8], off: usize, stride: usize) {
    let a = i32::from(plane[off - stride]);
    let b = i32::from(plane[off - stride + 1]);
    let c = i32::from(plane[off - stride + 2]);
    let d = i32::from(plane[off - stride + 3]);
    let e = i32::from(plane[off - stride + 4]);
    let f = i32::from(plane[off - stride + 5]);
    let g = i32::from(plane[off - stride + 6]);
    let h = i32::from(plane[off - stride + 7]);
    put(plane, off, stride, 0, 0, avg2(a, b));
    let v = avg2(b, c);
    put(plane, off, stride, 1, 0, v);
    put(plane, off, stride, 0, 2, v);
    let v = avg2(c, d);
    put(plane, off, stride, 2, 0, v);
    put(plane, off, stride, 1, 2, v);
    let v = avg2(d, e);
    put(plane, off, stride, 3, 0, v);
    put(plane, off, stride, 2, 2, v);
    put(plane, off, stride, 0, 1, avg3(a, b, c));
    let v = avg3(b, c, d);
    put(plane, off, stride, 1, 1, v);
    put(plane, off, stride, 0, 3, v);
    let v = avg3(c, d, e);
    put(plane, off, stride, 2, 1, v);
    put(plane, off, stride, 1, 3, v);
    let v = avg3(d, e, f);
    put(plane, off, stride, 3, 1, v);
    put(plane, off, stride, 2, 3, v);
    put(plane, off, stride, 3, 2, avg3(e, f, g));
    put(plane, off, stride, 3, 3, avg3(f, g, h));
}

/// libwebp `HU4_C`: horizontal-up diagonal.
pub(crate) fn hu4(plane: &mut [u8], off: usize, stride: usize) {
    let i = i32::from(plane[off - 1]);
    let j = i32::from(plane[off + stride - 1]);
    let k = i32::from(plane[off + 2 * stride - 1]);
    let l = i32::from(plane[off + 3 * stride - 1]);
    put(plane, off, stride, 0, 0, avg2(i, j));
    let v = avg2(j, k);
    put(plane, off, stride, 2, 0, v);
    put(plane, off, stride, 0, 1, v);
    let v = avg2(k, l);
    put(plane, off, stride, 2, 1, v);
    put(plane, off, stride, 0, 2, v);
    put(plane, off, stride, 1, 0, avg3(i, j, k));
    let v = avg3(j, k, l);
    put(plane, off, stride, 3, 0, v);
    put(plane, off, stride, 1, 1, v);
    let v = avg3(k, l, l);
    put(plane, off, stride, 3, 1, v);
    put(plane, off, stride, 1, 2, v);
    let lv = l as u8;
    put(plane, off, stride, 3, 2, lv);
    put(plane, off, stride, 2, 2, lv);
    put(plane, off, stride, 0, 3, lv);
    put(plane, off, stride, 1, 3, lv);
    put(plane, off, stride, 2, 3, lv);
    put(plane, off, stride, 3, 3, lv);
}

/// libwebp `HD4_C`: horizontal-down diagonal.
pub(crate) fn hd4(plane: &mut [u8], off: usize, stride: usize) {
    let i = i32::from(plane[off - 1]);
    let j = i32::from(plane[off + stride - 1]);
    let k = i32::from(plane[off + 2 * stride - 1]);
    let l = i32::from(plane[off + 3 * stride - 1]);
    let x = i32::from(plane[off - stride - 1]);
    let a = i32::from(plane[off - stride]);
    let b = i32::from(plane[off - stride + 1]);
    let c = i32::from(plane[off - stride + 2]);
    let v = avg2(i, x);
    put(plane, off, stride, 0, 0, v);
    put(plane, off, stride, 2, 1, v);
    let v = avg2(j, i);
    put(plane, off, stride, 0, 1, v);
    put(plane, off, stride, 2, 2, v);
    let v = avg2(k, j);
    put(plane, off, stride, 0, 2, v);
    put(plane, off, stride, 2, 3, v);
    put(plane, off, stride, 0, 3, avg2(l, k));
    put(plane, off, stride, 3, 0, avg3(a, b, c));
    put(plane, off, stride, 2, 0, avg3(x, a, b));
    let v = avg3(i, x, a);
    put(plane, off, stride, 1, 0, v);
    put(plane, off, stride, 3, 1, v);
    let v = avg3(j, i, x);
    put(plane, off, stride, 1, 1, v);
    put(plane, off, stride, 3, 2, v);
    let v = avg3(k, j, i);
    put(plane, off, stride, 1, 2, v);
    put(plane, off, stride, 3, 3, v);
    put(plane, off, stride, 1, 3, avg3(l, k, j));
}

// -----------------------------------------------------------------------------
// Dispatchers
// -----------------------------------------------------------------------------

/// Predict a 16×16 luma block in `mode` at `off`. `DC_PRED` selects the
/// availability-remapped DC variant from `has_top`/`has_left`; `V`/`H`/`TM` are
/// only ever requested when their samples exist.
pub(crate) fn predict_luma16(
    plane: &mut [u8],
    off: usize,
    stride: usize,
    mode: u8,
    has_top: bool,
    has_left: bool,
) {
    work!(PredictLuma);
    match mode {
        DC_PRED => dc16(plane, off, stride, has_top, has_left),
        TM_PRED => true_motion(plane, off, stride, 16),
        V_PRED => ve16(plane, off, stride),
        H_PRED => he16(plane, off, stride),
        _ => {},
    }
}

/// Predict an 8×8 chroma block in `mode` at `off` (same DC remap as
/// [`predict_luma16`]).
pub(crate) fn predict_chroma8(
    plane: &mut [u8],
    off: usize,
    stride: usize,
    mode: u8,
    has_top: bool,
    has_left: bool,
) {
    work!(PredictChroma);
    match mode {
        DC_PRED => dc8(plane, off, stride, has_top, has_left),
        TM_PRED => true_motion(plane, off, stride, 8),
        V_PRED => ve8(plane, off, stride),
        H_PRED => he8(plane, off, stride),
        _ => {},
    }
}

/// Predict a 4×4 luma sub-block in `mode` at `off`. The caller has filled the
/// four top-right samples (`plane[off - stride + 4 + x]`); `DC4` always uses the
/// 4-top + 4-left form.
pub(crate) fn predict_luma4(plane: &mut [u8], off: usize, stride: usize, mode: u8) {
    work!(PredictLuma);
    match mode {
        B_DC_PRED => dc4(plane, off, stride),
        B_TM_PRED => true_motion(plane, off, stride, 4),
        B_VE_PRED => ve4(plane, off, stride),
        B_HE_PRED => he4(plane, off, stride),
        B_RD_PRED => rd4(plane, off, stride),
        B_VR_PRED => vr4(plane, off, stride),
        B_LD_PRED => ld4(plane, off, stride),
        B_VL_PRED => vl4(plane, off, stride),
        B_HD_PRED => hd4(plane, off, stride),
        B_HU_PRED => hu4(plane, off, stride),
        _ => {},
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::needless_range_loop,
        clippy::doc_markdown,
        reason = "these KATs compare a predictor's output against an expected \
                  [y][x] grid, where indexing by (x, y) reads clearest, and cite \
                  informal RFC 6386 / libwebp neighbor labels in comments"
    )]

    use super::{predict_chroma8, predict_luma4, predict_luma16};
    use crate::lossy::constants::{
        B_DC_PRED, B_HD_PRED, B_HE_PRED, B_HU_PRED, B_LD_PRED, B_RD_PRED, B_TM_PRED, B_VE_PRED,
        B_VL_PRED, B_VR_PRED, DC_PRED, H_PRED, TM_PRED, V_PRED,
    };

    // A plane wide/tall enough to hold one border row + column and a 16x16 block.
    const STRIDE: usize = 24;
    const ROWS: usize = 20;
    // One top border row and one left border column precede the block origin.
    const OFF: usize = STRIDE + 1;

    // ---- Distinct linear-ramp border --------------------------------------
    //
    // A flat/constant border cannot catch a swapped/mis-kerneled/off-by-one
    // predictor, so every KAT below drives a *distinct* border:
    //   top[x]  = 10 + 8*x   (rising ramp, x in 0..=19  ->  10..=162)
    //   left[y] = 200 - 9*y  (falling ramp, y in 0..=15 -> 200..=65)
    //   top-left corner = TL = 27  (on neither ramp)
    // Each directional predictor therefore yields a distinct, hand/inline
    // verifiable result. Neighbor values feed both the plane index *and* the
    // expected computation, so a wrongly wired output is observable.

    const TL: i32 = 27;

    /// `top(x) = 10 + 8*x` as a `u8` (independent of the predictor code).
    fn top_val(x: usize) -> u8 {
        u8::try_from(10 + 8 * x).unwrap()
    }

    /// `left(y) = 200 - 9*y` as a `u8` (y <= 15 keeps it >= 65).
    fn left_val(y: usize) -> u8 {
        u8::try_from(200 - 9 * y).unwrap()
    }

    fn top_i(x: usize) -> i32 {
        i32::from(top_val(x))
    }

    fn left_i(y: usize) -> i32 {
        i32::from(left_val(y))
    }

    // ---- Independent reference kernels (re-derived from RFC 6386) ----------
    //
    // These reproduce the AVG2/AVG3/clip *formulas* from the spec; they do NOT
    // call the module's own avg2/avg3/clip8, so a KAT comparing against them is
    // a genuine cross-check rather than a tautology.

    /// `AVG2(a, b) = (a + b + 1) >> 1`.
    fn r_avg2(a: i32, b: i32) -> u8 {
        u8::try_from((a + b + 1) >> 1).unwrap()
    }

    /// `AVG3(a, b, c) = (a + 2*b + c + 2) >> 2`.
    fn r_avg3(a: i32, b: i32, c: i32) -> u8 {
        u8::try_from((a + 2 * b + c + 2) >> 2).unwrap()
    }

    /// `clip8(v) = clamp(v, 0, 255)` — the TrueMotion store clip.
    fn r_clip(v: i32) -> u8 {
        u8::try_from(v.clamp(0, 255)).unwrap()
    }

    /// Build a plane whose borders carry the ramps above and whose interior is
    /// left at 0 (so a correct predictor is observed to *write*).
    fn ramp_plane() -> [u8; STRIDE * ROWS] {
        let mut p = [0u8; STRIDE * ROWS];
        // top-left corner: plane[off - stride - 1]
        p[OFF - STRIDE - 1] = u8::try_from(TL).unwrap();
        // top row (incl. the four top-right samples LD4/VL4 read): plane[off - stride + x]
        for x in 0..20 {
            p[OFF - STRIDE + x] = top_val(x);
        }
        // left column: plane[off + y*stride - 1]
        for y in 0..16 {
            p[OFF + y * STRIDE - 1] = left_val(y);
        }
        p
    }

    /// Zero the `size`x`size` interior so equality with a non-zero expected also
    /// proves the predictor wrote every covered sample.
    fn zero_block(plane: &mut [u8; STRIDE * ROWS], size: usize) {
        for y in 0..size {
            let base = OFF + y * STRIDE;
            plane[base..base + size].fill(0);
        }
    }

    /// Read block sample (x, y).
    fn at(p: &[u8], x: usize, y: usize) -> u8 {
        p[OFF + x + y * STRIDE]
    }

    fn run16(mode: u8, has_top: bool, has_left: bool) -> [u8; STRIDE * ROWS] {
        let mut p = ramp_plane();
        zero_block(&mut p, 16);
        predict_luma16(&mut p, OFF, STRIDE, mode, has_top, has_left);
        p
    }

    fn run8(mode: u8, has_top: bool, has_left: bool) -> [u8; STRIDE * ROWS] {
        let mut p = ramp_plane();
        zero_block(&mut p, 8);
        predict_chroma8(&mut p, OFF, STRIDE, mode, has_top, has_left);
        p
    }

    fn run4(mode: u8) -> [u8; STRIDE * ROWS] {
        let mut p = ramp_plane();
        zero_block(&mut p, 4);
        predict_luma4(&mut p, OFF, STRIDE, mode);
        p
    }

    fn assert_uniform(p: &[u8], size: usize, expected: u8) {
        for y in 0..size {
            for x in 0..size {
                assert_eq!(at(p, x, y), expected, "({x},{y})");
            }
        }
    }

    fn assert_grid4(p: &[u8], expected: [[u8; 4]; 4], name: &str) {
        for y in 0..4 {
            for x in 0..4 {
                assert_eq!(at(p, x, y), expected[y][x], "{name} at ({x},{y})");
            }
        }
    }

    // =====================================================================
    // 16x16 luma
    // =====================================================================

    #[test]
    fn luma16_vertical_copies_top_ramp() {
        // VE16_C: every output row is a verbatim copy of the top ramp, so
        // column x reads top(x) = 10 + 8*x for every y (never constant across x).
        let p = run16(V_PRED, true, true);
        for y in 0..16 {
            for x in 0..16 {
                assert_eq!(at(&p, x, y), top_val(x), "({x},{y})");
            }
        }
    }

    #[test]
    fn luma16_horizontal_fills_left_ramp() {
        // HE16_C: every output row is filled with that row's left sample
        // left(y) = 200 - 9*y (never constant across y).
        let p = run16(H_PRED, true, true);
        for y in 0..16 {
            for x in 0..16 {
                assert_eq!(at(&p, x, y), left_val(y), "({x},{y})");
            }
        }
    }

    #[test]
    fn luma16_dc_both_averages_all_32() {
        // DC16_C: (16 + Sum top(0..15) + Sum left(0..15)) >> 5.
        //   Sum(top(k)+left(k)) = Sum(210 - k) = 3360 - 120 = 3240
        //   (16 + 3240) >> 5 = 3256 >> 5 = 101.
        let p = run16(DC_PRED, true, true);
        let mut sum = 16i32;
        for k in 0..16 {
            sum += top_i(k) + left_i(k);
        }
        let dc = u8::try_from(sum >> 5).unwrap();
        assert_eq!(dc, 101);
        assert_uniform(&p, 16, dc);
    }

    #[test]
    fn luma16_dc_top_only_averages_16_top() {
        // DC16NoLeft_C (has_top, !has_left): (8 + Sum top(0..15)) >> 4.
        //   Sum top = 160 + 8*120 = 1120; (8 + 1120) >> 4 = 1128 >> 4 = 70.
        let p = run16(DC_PRED, true, false);
        let mut sum = 8i32;
        for k in 0..16 {
            sum += top_i(k);
        }
        let dc = u8::try_from(sum >> 4).unwrap();
        assert_eq!(dc, 70);
        assert_uniform(&p, 16, dc);
    }

    #[test]
    fn luma16_dc_left_only_averages_16_left() {
        // DC16NoTop_C (!has_top, has_left): (8 + Sum left(0..15)) >> 4.
        //   Sum left = 3200 - 1080 = 2120; (8 + 2120) >> 4 = 2128 >> 4 = 133.
        let p = run16(DC_PRED, false, true);
        let mut sum = 8i32;
        for k in 0..16 {
            sum += left_i(k);
        }
        let dc = u8::try_from(sum >> 4).unwrap();
        assert_eq!(dc, 133);
        assert_uniform(&p, 16, dc);
    }

    #[test]
    fn luma16_dc_no_top_no_left_is_128() {
        // DC16NoTopLeft_C: constant 0x80 fallback (preserved KAT).
        let p = run16(DC_PRED, false, false);
        assert_uniform(&p, 16, 0x80);
    }

    #[test]
    fn luma16_true_motion_matches_clip_formula() {
        // TM16_C: clip(top(x) + left(y) - top_left), top_left = TL = 27.
        let p = run16(TM_PRED, true, true);
        for y in 0..16 {
            for x in 0..16 {
                let e = r_clip(top_i(x) + left_i(y) - TL);
                assert_eq!(at(&p, x, y), e, "({x},{y})");
            }
        }
        // Hand-computed corners (top(15)=130, left(15)=65):
        //   (0,0)=10+200-27=183; (15,0)=130+200-27=303 -> clip 255;
        //   (0,15)=10+65-27=48; (15,15)=130+65-27=168.
        assert_eq!(at(&p, 0, 0), 183);
        assert_eq!(at(&p, 15, 0), 255);
        assert_eq!(at(&p, 0, 15), 48);
        assert_eq!(at(&p, 15, 15), 168);
    }

    // =====================================================================
    // 8x8 chroma
    // =====================================================================

    #[test]
    fn chroma8_vertical_copies_top_ramp() {
        let p = run8(V_PRED, true, true);
        for y in 0..8 {
            for x in 0..8 {
                assert_eq!(at(&p, x, y), top_val(x), "({x},{y})");
            }
        }
    }

    #[test]
    fn chroma8_horizontal_fills_left_ramp() {
        let p = run8(H_PRED, true, true);
        for y in 0..8 {
            for x in 0..8 {
                assert_eq!(at(&p, x, y), left_val(y), "({x},{y})");
            }
        }
    }

    #[test]
    fn chroma8_dc_both_averages_all_16() {
        // DC8uv_C: (8 + Sum top(0..7) + Sum left(0..7)) >> 4.
        //   Sum top = 304, Sum left = 1348; (8 + 304 + 1348) >> 4 = 1660 >> 4 = 103.
        let p = run8(DC_PRED, true, true);
        let mut sum = 8i32;
        for k in 0..8 {
            sum += top_i(k) + left_i(k);
        }
        let dc = u8::try_from(sum >> 4).unwrap();
        assert_eq!(dc, 103);
        assert_uniform(&p, 8, dc);
    }

    #[test]
    fn chroma8_dc_top_only_averages_8_top() {
        // DC8uvNoLeft_C (has_top, !has_left): (4 + Sum top(0..7)) >> 3.
        //   (4 + 304) >> 3 = 308 >> 3 = 38.
        let p = run8(DC_PRED, true, false);
        let mut sum = 4i32;
        for k in 0..8 {
            sum += top_i(k);
        }
        let dc = u8::try_from(sum >> 3).unwrap();
        assert_eq!(dc, 38);
        assert_uniform(&p, 8, dc);
    }

    #[test]
    fn chroma8_dc_left_only_averages_8_left() {
        // DC8uvNoTop_C (!has_top, has_left): (4 + Sum left(0..7)) >> 3.
        //   (4 + 1348) >> 3 = 1352 >> 3 = 169.
        let p = run8(DC_PRED, false, true);
        let mut sum = 4i32;
        for k in 0..8 {
            sum += left_i(k);
        }
        let dc = u8::try_from(sum >> 3).unwrap();
        assert_eq!(dc, 169);
        assert_uniform(&p, 8, dc);
    }

    #[test]
    fn chroma8_dc_no_top_no_left_is_128() {
        // DC8uvNoTopLeft_C: constant 0x80 fallback (preserved KAT).
        let p = run8(DC_PRED, false, false);
        assert_uniform(&p, 8, 0x80);
    }

    #[test]
    fn chroma8_true_motion_matches_clip_formula() {
        // TM8uv_C: clip(top(x) + left(y) - 27).
        let p = run8(TM_PRED, true, true);
        for y in 0..8 {
            for x in 0..8 {
                let e = r_clip(top_i(x) + left_i(y) - TL);
                assert_eq!(at(&p, x, y), e, "({x},{y})");
            }
        }
        // Corners (top(7)=66, left(7)=137):
        //   (0,0)=183; (7,0)=66+200-27=239; (0,7)=10+137-27=120; (7,7)=66+137-27=176.
        assert_eq!(at(&p, 0, 0), 183);
        assert_eq!(at(&p, 7, 0), 239);
        assert_eq!(at(&p, 0, 7), 120);
        assert_eq!(at(&p, 7, 7), 176);
    }

    // =====================================================================
    // 4x4 luma — all ten B_ modes.
    //
    // Named ramp samples (RFC 6386 labels):
    //   top   A=10  B=18  C=26  D=34  E=42  F=50  G=58  H=66   (top(0..7))
    //   left  I=200 J=191 K=182 L=173                          (left(0..3))
    //   top-left X = 27
    // =====================================================================

    #[test]
    fn luma4_dc_averages_4_top_4_left() {
        // DC4_C: (4 + Sum top(0..3) + Sum left(0..3)) >> 3.
        //   Sum top = 88, Sum left = 746; (4 + 88 + 746) >> 3 = 838 >> 3 = 104.
        let p = run4(B_DC_PRED);
        let mut sum = 4i32;
        for k in 0..4 {
            sum += top_i(k) + left_i(k);
        }
        let dc = u8::try_from(sum >> 3).unwrap();
        assert_eq!(dc, 104);
        assert_uniform(&p, 4, dc);
    }

    #[test]
    fn luma4_vertical_avg3_of_top_triples() {
        // VE4_C: each column c = AVG3 of the three top samples around it;
        // all four rows are identical. vals = [AVG3(X,A,B),AVG3(A,B,C),
        // AVG3(B,C,D),AVG3(C,D,E)] = [16, 18, 26, 34].
        let p = run4(B_VE_PRED);
        let col = [
            r_avg3(TL, top_i(0), top_i(1)),
            r_avg3(top_i(0), top_i(1), top_i(2)),
            r_avg3(top_i(1), top_i(2), top_i(3)),
            r_avg3(top_i(2), top_i(3), top_i(4)),
        ];
        assert_eq!(col, [16, 18, 26, 34]);
        for y in 0..4 {
            for x in 0..4 {
                assert_eq!(at(&p, x, y), col[x], "({x},{y})");
            }
        }
    }

    #[test]
    fn luma4_horizontal_avg3_of_left_triples() {
        // HE4_C: each row r = constant AVG3 of neighboring left samples
        // (A=top-left, B..E=left(0..3)). rows = [AVG3(X,I,J),AVG3(I,J,K),
        // AVG3(J,K,L),AVG3(K,L,L)] = [155, 191, 182, 175].
        let p = run4(B_HE_PRED);
        let row = [
            r_avg3(TL, left_i(0), left_i(1)),
            r_avg3(left_i(0), left_i(1), left_i(2)),
            r_avg3(left_i(1), left_i(2), left_i(3)),
            r_avg3(left_i(2), left_i(3), left_i(3)),
        ];
        assert_eq!(row, [155, 191, 182, 175]);
        for y in 0..4 {
            for x in 0..4 {
                assert_eq!(at(&p, x, y), row[y], "({x},{y})");
            }
        }
    }

    #[test]
    fn luma4_true_motion_matches_clip_formula() {
        // TM4_C: clip(top(x) + left(y) - 27).
        let p = run4(B_TM_PRED);
        for y in 0..4 {
            for x in 0..4 {
                let e = r_clip(top_i(x) + left_i(y) - TL);
                assert_eq!(at(&p, x, y), e, "({x},{y})");
            }
        }
        // Corners: (0,0)=10+200-27=183; (3,0)=34+200-27=207;
        //          (0,3)=10+173-27=156; (3,3)=34+173-27=180.
        assert_eq!(at(&p, 0, 0), 183);
        assert_eq!(at(&p, 3, 0), 207);
        assert_eq!(at(&p, 0, 3), 156);
        assert_eq!(at(&p, 3, 3), 180);
    }

    #[test]
    fn luma4_down_right_diagonal() {
        // RD4_C wiring transcribed from dsp/dec.c: each anti-diagonal is one
        // constant AVG3 triple; a wrongly wired cell moves off its diagonal.
        let p = run4(B_RD_PRED);
        let x = TL;
        let a = top_i(0);
        let b = top_i(1);
        let c = top_i(2);
        let d = top_i(3);
        let i = left_i(0);
        let j = left_i(1);
        let k = left_i(2);
        let l = left_i(3);
        let expected = [
            [
                r_avg3(a, x, i),
                r_avg3(b, a, x),
                r_avg3(c, b, a),
                r_avg3(d, c, b),
            ],
            [
                r_avg3(x, i, j),
                r_avg3(a, x, i),
                r_avg3(b, a, x),
                r_avg3(c, b, a),
            ],
            [
                r_avg3(i, j, k),
                r_avg3(x, i, j),
                r_avg3(a, x, i),
                r_avg3(b, a, x),
            ],
            [
                r_avg3(j, k, l),
                r_avg3(i, j, k),
                r_avg3(x, i, j),
                r_avg3(a, x, i),
            ],
        ];
        assert_grid4(&p, expected, "RD4");
        // Spot literals: (0,0)=AVG3(A,X,I)=(10+54+200+2)>>2=66;
        //   (3,0)=AVG3(D,C,B)=106>>2=26; (0,2)=AVG3(I,J,K)=766>>2=191;
        //   (0,3)=AVG3(J,K,L)=730>>2=182.
        assert_eq!(at(&p, 0, 0), 66);
        assert_eq!(at(&p, 3, 0), 26);
        assert_eq!(at(&p, 0, 2), 191);
        assert_eq!(at(&p, 0, 3), 182);
    }

    #[test]
    fn luma4_down_left_diagonal() {
        // LD4_C: reads the four top-right samples E..H; anti-diagonals constant.
        let p = run4(B_LD_PRED);
        let a = top_i(0);
        let b = top_i(1);
        let c = top_i(2);
        let d = top_i(3);
        let e = top_i(4);
        let f = top_i(5);
        let g = top_i(6);
        let h = top_i(7);
        let expected = [
            [
                r_avg3(a, b, c),
                r_avg3(b, c, d),
                r_avg3(c, d, e),
                r_avg3(d, e, f),
            ],
            [
                r_avg3(b, c, d),
                r_avg3(c, d, e),
                r_avg3(d, e, f),
                r_avg3(e, f, g),
            ],
            [
                r_avg3(c, d, e),
                r_avg3(d, e, f),
                r_avg3(e, f, g),
                r_avg3(f, g, h),
            ],
            [
                r_avg3(d, e, f),
                r_avg3(e, f, g),
                r_avg3(f, g, h),
                r_avg3(g, h, h),
            ],
        ];
        assert_grid4(&p, expected, "LD4");
        // Spot: (0,0)=AVG3(A,B,C)=74>>2=18; (3,0)=AVG3(D,E,F)=170>>2=42;
        //   (0,3)=AVG3(D,E,F)=42; (3,3)=AVG3(G,H,H)=258>>2=64.
        assert_eq!(at(&p, 0, 0), 18);
        assert_eq!(at(&p, 3, 0), 42);
        assert_eq!(at(&p, 0, 3), 42);
        assert_eq!(at(&p, 3, 3), 64);
    }

    #[test]
    fn luma4_vertical_right_diagonal() {
        // VR4_C: mixes AVG2 (row 0 and the (x,2) tail) with AVG3 wiring.
        let p = run4(B_VR_PRED);
        let x = TL;
        let a = top_i(0);
        let b = top_i(1);
        let c = top_i(2);
        let d = top_i(3);
        let i = left_i(0);
        let j = left_i(1);
        let k = left_i(2);
        let expected = [
            [r_avg2(x, a), r_avg2(a, b), r_avg2(b, c), r_avg2(c, d)],
            [
                r_avg3(i, x, a),
                r_avg3(x, a, b),
                r_avg3(a, b, c),
                r_avg3(b, c, d),
            ],
            [r_avg3(j, i, x), r_avg2(x, a), r_avg2(a, b), r_avg2(b, c)],
            [
                r_avg3(k, j, i),
                r_avg3(i, x, a),
                r_avg3(x, a, b),
                r_avg3(a, b, c),
            ],
        ];
        assert_grid4(&p, expected, "VR4");
        // Spot: (0,0)=AVG2(X,A)=(27+10+1)>>1=19; (3,0)=AVG2(C,D)=61>>1=30;
        //   (0,2)=AVG3(J,I,X)=620>>2=155; (0,3)=AVG3(K,J,I)=766>>2=191;
        //   (1,1)=AVG3(X,A,B)=67>>2=16.
        assert_eq!(at(&p, 0, 0), 19);
        assert_eq!(at(&p, 3, 0), 30);
        assert_eq!(at(&p, 0, 2), 155);
        assert_eq!(at(&p, 0, 3), 191);
        assert_eq!(at(&p, 1, 1), 16);
    }

    #[test]
    fn luma4_vertical_left_diagonal() {
        // VL4_C: reads the four top-right samples E..H; mixes AVG2/AVG3.
        let p = run4(B_VL_PRED);
        let a = top_i(0);
        let b = top_i(1);
        let c = top_i(2);
        let d = top_i(3);
        let e = top_i(4);
        let f = top_i(5);
        let g = top_i(6);
        let h = top_i(7);
        let expected = [
            [r_avg2(a, b), r_avg2(b, c), r_avg2(c, d), r_avg2(d, e)],
            [
                r_avg3(a, b, c),
                r_avg3(b, c, d),
                r_avg3(c, d, e),
                r_avg3(d, e, f),
            ],
            [r_avg2(b, c), r_avg2(c, d), r_avg2(d, e), r_avg3(e, f, g)],
            [
                r_avg3(b, c, d),
                r_avg3(c, d, e),
                r_avg3(d, e, f),
                r_avg3(f, g, h),
            ],
        ];
        assert_grid4(&p, expected, "VL4");
        // Spot: (0,0)=AVG2(A,B)=29>>1=14; (3,0)=AVG2(D,E)=77>>1=38;
        //   (3,2)=AVG3(E,F,G)=202>>2=50; (3,3)=AVG3(F,G,H)=234>>2=58;
        //   (0,3)=AVG3(B,C,D)=106>>2=26.
        assert_eq!(at(&p, 0, 0), 14);
        assert_eq!(at(&p, 3, 0), 38);
        assert_eq!(at(&p, 3, 2), 50);
        assert_eq!(at(&p, 3, 3), 58);
        assert_eq!(at(&p, 0, 3), 26);
    }

    #[test]
    fn luma4_horizontal_up_diagonal() {
        // HU4_C: left-only; the bottom-right wedge collapses to L.
        let p = run4(B_HU_PRED);
        let i = left_i(0);
        let j = left_i(1);
        let k = left_i(2);
        let l = left_i(3);
        let l8 = u8::try_from(l).unwrap();
        let expected = [
            [r_avg2(i, j), r_avg3(i, j, k), r_avg2(j, k), r_avg3(j, k, l)],
            [r_avg2(j, k), r_avg3(j, k, l), r_avg2(k, l), r_avg3(k, l, l)],
            [r_avg2(k, l), r_avg3(k, l, l), l8, l8],
            [l8, l8, l8, l8],
        ];
        assert_grid4(&p, expected, "HU4");
        // Spot: (0,0)=AVG2(I,J)=392>>1=196; (1,0)=AVG3(I,J,K)=766>>2=191;
        //   (3,0)=AVG3(J,K,L)=730>>2=182; (0,2)=AVG2(K,L)=356>>1=178; (2,2)=L=173.
        assert_eq!(at(&p, 0, 0), 196);
        assert_eq!(at(&p, 1, 0), 191);
        assert_eq!(at(&p, 3, 0), 182);
        assert_eq!(at(&p, 0, 2), 178);
        assert_eq!(at(&p, 2, 2), 173);
    }

    #[test]
    fn luma4_horizontal_down_diagonal() {
        // HD4_C: mixes AVG2 (left column + its (2,y) echo) with AVG3 wiring.
        let p = run4(B_HD_PRED);
        let x = TL;
        let a = top_i(0);
        let b = top_i(1);
        let c = top_i(2);
        let i = left_i(0);
        let j = left_i(1);
        let k = left_i(2);
        let l = left_i(3);
        let expected = [
            [
                r_avg2(i, x),
                r_avg3(i, x, a),
                r_avg3(x, a, b),
                r_avg3(a, b, c),
            ],
            [r_avg2(j, i), r_avg3(j, i, x), r_avg2(i, x), r_avg3(i, x, a)],
            [r_avg2(k, j), r_avg3(k, j, i), r_avg2(j, i), r_avg3(j, i, x)],
            [r_avg2(l, k), r_avg3(l, k, j), r_avg2(k, j), r_avg3(k, j, i)],
        ];
        assert_grid4(&p, expected, "HD4");
        // Spot: (0,0)=AVG2(I,X)=228>>1=114; (2,0)=AVG3(X,A,B)=67>>2=16;
        //   (3,0)=AVG3(A,B,C)=74>>2=18; (0,1)=AVG2(J,I)=392>>1=196;
        //   (0,3)=AVG2(L,K)=356>>1=178.
        assert_eq!(at(&p, 0, 0), 114);
        assert_eq!(at(&p, 2, 0), 16);
        assert_eq!(at(&p, 3, 0), 18);
        assert_eq!(at(&p, 0, 1), 196);
        assert_eq!(at(&p, 0, 3), 178);
    }

    #[test]
    fn true_motion_reference_tracks_true_motion_off_stride_geometry() {
        // The gathered-top `true_motion` and the in-place `true_motion_reference`
        // must agree for ANY block position, not only the canonical off = stride+1
        // the equivalence proptest uses. At that special offset off / stride == 1
        // == off - stride, so corrupting a neighbor index from `-` to `/` (the
        // top-left at `off - stride - 1`, the top sample at `off - stride + x`)
        // reads the SAME sample and hides. Placing the block at row 3, col 5 makes
        // off / stride (= 3) differ from off - stride (= 53), so any such `-`->`/`
        // makes the reference read a different neighbor and diverge from the
        // (unmutated, correct) gathered-top kernel.
        const STRIDE: usize = 24;
        const ROWS: usize = 20;
        const OFF: usize = 3 * STRIDE + 5; // 77: off/stride=3 != off-stride=53
        let base: Vec<u8> = (0..STRIDE * ROWS)
            .map(|i| u8::try_from((i * 97 + 13) % 251).unwrap())
            .collect();
        for size in [4usize, 8, 16] {
            let mut opt = base.clone();
            let mut reference = base.clone();
            super::true_motion(&mut opt, OFF, STRIDE, size);
            super::true_motion_reference(&mut reference, OFF, STRIDE, size);
            assert_eq!(opt, reference, "size {size}");
        }
    }

    #[test]
    fn dc16_both_averages_the_true_top_row_off_stride_geometry() {
        // dc16_both sums the 16 top samples at `off - stride + j`. The canonical
        // DC KAT above cannot catch a `-`->`/` on that index because at off =
        // stride + 1, off / stride == 1 == off - stride reads the same row. Place
        // the block at row 3 so off / stride (= 3) != off - stride (= 49): a
        // corrupted top index then sums a different row and the DC value changes.
        // Compared against an independent rounder-16 / >>5 reference computed from
        // the neighbor indices directly.
        const STRIDE: usize = 24;
        const ROWS: usize = 24;
        const OFF: usize = 3 * STRIDE + 1; // 73: off/stride=3 != off-stride=49
        let plane: Vec<u8> = (0..STRIDE * ROWS)
            .map(|i| u8::try_from((i * 53 + 7) % 241).unwrap())
            .collect();
        let mut sum = 16i32;
        for j in 0..16 {
            sum += i32::from(plane[OFF - STRIDE + j]) + i32::from(plane[OFF + j * STRIDE - 1]);
        }
        let expected = u8::try_from(sum >> 5).unwrap();
        assert_eq!(super::dc16_both(&plane, OFF, STRIDE), expected);
    }

    proptest::proptest! {
        /// The gathered-top [`super::true_motion`] reconstructs every block byte
        /// identically to the in-place [`super::true_motion_reference`] over random
        /// planes and all three block sizes (4/8/16) — the mechanical proof that
        /// breaking the read/write alias (top row copied to a local) did not change a
        /// single output byte. Runs both on clones of one random plane and compares.
        #[test]
        fn true_motion_matches_reference(
            size_sel in 0usize..3,
            seed in proptest::prelude::any::<u64>(),
        ) {
            // One top border row + one left border column precede the block origin,
            // and the plane is tall enough for a 16-row block: STRIDE=24, 20 rows.
            const STRIDE: usize = 24;
            const OFF: usize = STRIDE + 1;
            let size = [4usize, 8, 16][size_sel];
            let mut st = seed;
            let mut next = || {
                st = st.wrapping_add(0x9E37_79B9_7F4A_7C15);
                let mut z = st;
                z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
                (z ^ (z >> 31)) as u8
            };
            let base: Vec<u8> = (0..STRIDE * 20).map(|_| next()).collect();
            let mut opt = base.clone();
            let mut reference = base;
            super::true_motion(&mut opt, OFF, STRIDE, size);
            super::true_motion_reference(&mut reference, OFF, STRIDE, size);
            proptest::prop_assert_eq!(opt, reference);
        }
    }
}
