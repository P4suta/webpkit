//! VP8 in-loop deblocking filter (RFC 6386 §15, transcribed from libwebp
//! `dsp/dec.c` and driven exactly as `dec/frame_dec.c` `DoFilter`).
//!
//! Ported functions from `dsp/dec.c`: `DoFilter2_C`, `DoFilter4_C`,
//! `DoFilter6_C`, `Hev`, `NeedsFilter_C`, `NeedsFilter2_C`, `FilterLoop26_C`,
//! `FilterLoop24_C`, `SimpleVFilter16_C`, `SimpleHFilter16_C`,
//! `SimpleVFilter16i_C`, `SimpleHFilter16i_C`, `VFilter16_C`, `HFilter16_C`,
//! `VFilter16i_C`, `HFilter16i_C`, `VFilter8_C`, `HFilter8_C`, `VFilter8i_C`,
//! `HFilter8i_C`. The per-macroblock dispatch and limit expressions come from
//! `dec/frame_dec.c` `DoFilter`.
//!
//! The clip tables (`VP8ksclip1`/`VP8ksclip2`/`VP8kclip1`/`VP8kabs0`) are
//! computed inline as plain integer clamps: `sclip1` = clamp to `[-128, 127]`,
//! `sclip2` = clamp to `[-16, 15]`, `clip8` = clamp to `[0, 255]`, and the
//! `abs0` lookup is `i32::abs` (its inputs are byte differences in
//! `[-255, 255]`, so the absolute value is exact).
//!
//! Buffer model: each plane is a row-major `&mut [u8]` with row stride
//! `stride`; a block sample at column `x`, row `y` is `plane[off + x + y *
//! stride]`. Neighbor reads across an edge (the C `p[-k*step]`) map to
//! `plane[pos - k * step]` with `step` = `1` for a vertical edge (columns) or
//! `stride` for a horizontal edge (rows). Callers guarantee the top border rows
//! and left/right border columns exist, so these indices never underflow.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "reproduces the C uint8_t clip semantics of the reference decoder: \
              clip8 stores an already-clamped [0,255] value into a u8 exactly as \
              VP8kclip1 does"
)]

use crate::lossy::work::work;

/// Per-macroblock filter parameters (libwebp `VP8FInfo`), precomputed from the
/// segment filter level and sharpness.
#[derive(Clone, Copy, Default)]
pub(crate) struct FInfo {
    /// Outer filter limit `2 * level + ilevel` (`0` disables all filtering).
    pub(crate) f_limit: i32,
    /// Inner (interior) limit, in `1..=9 - sharpness`.
    pub(crate) f_ilevel: i32,
    /// Whether the three interior 4-pixel-spaced sub-edges are filtered.
    pub(crate) f_inner: bool,
    /// High-edge-variance threshold selecting the 2-tap vs 4/6-tap kernel.
    pub(crate) hev_thresh: i32,
}

/// The three threshold parameters carried through a normal filter pass,
/// bundled to keep the loop's argument count small.
#[derive(Clone, Copy)]
struct Limits {
    /// Outer difference threshold (`thresh2 = 2 * thresh + 1` is derived).
    thresh: i32,
    /// Interior smoothness threshold (`ithresh`).
    ithresh: i32,
    /// High-edge-variance threshold (`hev_thresh`).
    hev_thresh: i32,
}

/// Clamp to `[0, 255]` and store as `u8` — the `VP8kclip1` / `clip_8b` pattern.
fn clip8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// Clamp to `[-128, 127]` — the `VP8ksclip1` table.
fn sclip1(v: i32) -> i32 {
    v.clamp(-128, 127)
}

/// Clamp to `[-16, 15]` — the `VP8ksclip2` table.
fn sclip2(v: i32) -> i32 {
    v.clamp(-16, 15)
}

// -----------------------------------------------------------------------------
// Filter primitives (dsp/dec.c)

// These are pure arithmetic cores operating on the already-loaded edge samples.
// Each caller (`filter_loop`, the simple filters) loads the straddling window
// once and threads those `i32` locals through the activation tests, the HEV
// select and the chosen kernel — the samples are never re-read from the plane
// between the tests and the write-back, which was the dominant redundant-load
// cost of the per-primitive `plane[pos ± k*step]` scheme.

/// `DoFilter2_C` arithmetic: from the four straddling samples, the new
/// `(p0, q0)`. Filters `p0`/`q0` across the edge.
fn filter2(p1: i32, p0: i32, q0: i32, q1: i32) -> (u8, u8) {
    let a = 3 * (q0 - p0) + sclip1(p1 - q1);
    let a1 = sclip2((a + 4) >> 3);
    let a2 = sclip2((a + 3) >> 3);
    (clip8(p0 + a2), clip8(q0 - a1))
}

/// `DoFilter4_C` arithmetic: the interior HEV-passing kernel; new
/// `(p1, p0, q0, q1)`.
fn filter4(p1: i32, p0: i32, q0: i32, q1: i32) -> (u8, u8, u8, u8) {
    let a = 3 * (q0 - p0);
    let a1 = sclip2((a + 4) >> 3);
    let a2 = sclip2((a + 3) >> 3);
    let a3 = (a1 + 1) >> 1;
    (
        clip8(p1 + a3),
        clip8(p0 + a2),
        clip8(q0 - a1),
        clip8(q1 - a3),
    )
}

/// `DoFilter6_C` arithmetic: the macroblock-edge / non-HEV kernel; new
/// `(p2, p1, p0, q0, q1, q2)`.
fn filter6(p2: i32, p1: i32, p0: i32, q0: i32, q1: i32, q2: i32) -> (u8, u8, u8, u8, u8, u8) {
    let a = sclip1(3 * (q0 - p0) + sclip1(p1 - q1));
    let a1 = (27 * a + 63) >> 7; // in [-27,27]
    let a2 = (18 * a + 63) >> 7; // in [-18,18]
    let a3 = (9 * a + 63) >> 7; //  in [-9,9]
    (
        clip8(p2 + a3),
        clip8(p1 + a2),
        clip8(p0 + a1),
        clip8(q0 - a1),
        clip8(q1 - a2),
        clip8(q2 - a3),
    )
}

/// `Hev`: true when the edge is a high-variance (sharp) boundary.
const fn hev(p1: i32, p0: i32, q0: i32, q1: i32, thresh: i32) -> bool {
    (p1 - p0).abs() > thresh || (q1 - q0).abs() > thresh
}

/// `NeedsFilter_C`: the simple-filter activation test.
const fn needs_filter(p1: i32, p0: i32, q0: i32, q1: i32, t: i32) -> bool {
    4 * (p0 - q0).abs() + (p1 - q1).abs() <= t
}

/// `NeedsFilter2_C`: the normal-filter activation test (outer `t` + interior `it`).
#[allow(
    clippy::too_many_arguments,
    reason = "the eight straddling samples are loaded once by the caller and \
              passed through as locals; taking a slice would reintroduce the \
              bounds-checked re-reads this refactor removes"
)]
const fn needs_filter2(
    p3: i32,
    p2: i32,
    p1: i32,
    p0: i32,
    q0: i32,
    q1: i32,
    q2: i32,
    q3: i32,
    t: i32,
    it: i32,
) -> bool {
    if 4 * (p0 - q0).abs() + (p1 - q1).abs() > t {
        return false;
    }
    (p3 - p2).abs() <= it
        && (p2 - p1).abs() <= it
        && (p1 - p0).abs() <= it
        && (q3 - q2).abs() <= it
        && (q2 - q1).abs() <= it
        && (q1 - q0).abs() <= it
}

// -----------------------------------------------------------------------------
// Simple in-loop filter (RFC §15.2)

/// `SimpleVFilter16_C`: filter the top horizontal edge across 16 columns.
fn simple_v_filter16(plane: &mut [u8], off: usize, stride: usize, thresh: i32) {
    let thresh2 = 2 * thresh + 1;
    for i in 0..16 {
        simple_filter_edge(plane, off + i, stride, thresh2);
    }
}

/// `SimpleHFilter16_C`: filter the left vertical edge across 16 rows.
fn simple_h_filter16(plane: &mut [u8], off: usize, stride: usize, thresh: i32) {
    let thresh2 = 2 * thresh + 1;
    for i in 0..16 {
        simple_filter_edge(plane, off + i * stride, 1, thresh2);
    }
}

/// One simple-filter edge at `pos` with cross-edge `step`: load the four
/// straddling samples once, gate on `NeedsFilter`, apply the 2-tap kernel.
fn simple_filter_edge(plane: &mut [u8], pos: usize, step: usize, thresh2: i32) {
    let p1 = i32::from(plane[pos - 2 * step]);
    let p0 = i32::from(plane[pos - step]);
    let q0 = i32::from(plane[pos]);
    let q1 = i32::from(plane[pos + step]);
    if needs_filter(p1, p0, q0, q1, thresh2) {
        work!(LoopFilterEdge);
        let (np0, nq0) = filter2(p1, p0, q0, q1);
        plane[pos - step] = np0;
        plane[pos] = nq0;
    }
}

/// `SimpleVFilter16i_C`: the three interior horizontal edges (rows 4, 8, 12).
fn simple_v_filter16i(plane: &mut [u8], off: usize, stride: usize, thresh: i32) {
    let mut pos = off;
    for _ in 0..3 {
        pos += 4 * stride;
        simple_v_filter16(plane, pos, stride, thresh);
    }
}

/// `SimpleHFilter16i_C`: the three interior vertical edges (columns 4, 8, 12).
fn simple_h_filter16i(plane: &mut [u8], off: usize, stride: usize, thresh: i32) {
    let mut pos = off;
    for _ in 0..3 {
        pos += 4;
        simple_h_filter16(plane, pos, stride, thresh);
    }
}

// -----------------------------------------------------------------------------
// Normal (complex) in-loop filter (RFC §15.3)

/// `FilterLoop26_C` (`use_filter6 = true`) / `FilterLoop24_C` (`false`): walk
/// `size` positions of stride `vstride`, filtering each with the edge step
/// `hstride`, choosing the 2-tap kernel on HEV or the 6/4-tap kernel otherwise.
fn filter_loop(
    plane: &mut [u8],
    off: usize,
    hstride: usize,
    vstride: usize,
    size: usize,
    lim: Limits,
    use_filter6: bool,
) {
    let thresh2 = 2 * lim.thresh + 1;
    let step = hstride;
    let mut pos = off;
    for _ in 0..size {
        // Load the p3..q3 window once; the activation test, HEV select and the
        // chosen kernel all work off these locals instead of re-reading the plane.
        let p3 = i32::from(plane[pos - 4 * step]);
        let p2 = i32::from(plane[pos - 3 * step]);
        let p1 = i32::from(plane[pos - 2 * step]);
        let p0 = i32::from(plane[pos - step]);
        let q0 = i32::from(plane[pos]);
        let q1 = i32::from(plane[pos + step]);
        let q2 = i32::from(plane[pos + 2 * step]);
        let q3 = i32::from(plane[pos + 3 * step]);
        if needs_filter2(p3, p2, p1, p0, q0, q1, q2, q3, thresh2, lim.ithresh) {
            work!(LoopFilterEdge);
            if hev(p1, p0, q0, q1, lim.hev_thresh) {
                let (np0, nq0) = filter2(p1, p0, q0, q1);
                plane[pos - step] = np0;
                plane[pos] = nq0;
            } else if use_filter6 {
                let (np2, np1, np0, nq0, nq1, nq2) = filter6(p2, p1, p0, q0, q1, q2);
                plane[pos - 3 * step] = np2;
                plane[pos - 2 * step] = np1;
                plane[pos - step] = np0;
                plane[pos] = nq0;
                plane[pos + step] = nq1;
                plane[pos + 2 * step] = nq2;
            } else {
                let (np1, np0, nq0, nq1) = filter4(p1, p0, q0, q1);
                plane[pos - 2 * step] = np1;
                plane[pos - step] = np0;
                plane[pos] = nq0;
                plane[pos + step] = nq1;
            }
        }
        pos += vstride;
    }
}

/// `VFilter16_C`: luma top macroblock edge (horizontal edge, 16 columns).
fn v_filter16(plane: &mut [u8], off: usize, stride: usize, lim: Limits) {
    filter_loop(plane, off, stride, 1, 16, lim, true);
}

/// `HFilter16_C`: luma left macroblock edge (vertical edge, 16 rows).
fn h_filter16(plane: &mut [u8], off: usize, stride: usize, lim: Limits) {
    filter_loop(plane, off, 1, stride, 16, lim, true);
}

/// `VFilter16i_C`: the three interior luma horizontal edges.
fn v_filter16i(plane: &mut [u8], off: usize, stride: usize, lim: Limits) {
    let mut pos = off;
    for _ in 0..3 {
        pos += 4 * stride;
        filter_loop(plane, pos, stride, 1, 16, lim, false);
    }
}

/// `HFilter16i_C`: the three interior luma vertical edges.
fn h_filter16i(plane: &mut [u8], off: usize, stride: usize, lim: Limits) {
    let mut pos = off;
    for _ in 0..3 {
        pos += 4;
        filter_loop(plane, pos, 1, stride, 16, lim, false);
    }
}

/// `VFilter8_C`: chroma top macroblock edge on both U and V (8 columns each).
fn v_filter8(u: &mut [u8], v: &mut [u8], off: usize, stride: usize, lim: Limits) {
    filter_loop(u, off, stride, 1, 8, lim, true);
    filter_loop(v, off, stride, 1, 8, lim, true);
}

/// `HFilter8_C`: chroma left macroblock edge on both U and V (8 rows each).
fn h_filter8(u: &mut [u8], v: &mut [u8], off: usize, stride: usize, lim: Limits) {
    filter_loop(u, off, 1, stride, 8, lim, true);
    filter_loop(v, off, 1, stride, 8, lim, true);
}

/// `VFilter8i_C`: the single interior chroma horizontal edge (row 4) on U and V.
fn v_filter8i(u: &mut [u8], v: &mut [u8], off: usize, stride: usize, lim: Limits) {
    filter_loop(u, off + 4 * stride, stride, 1, 8, lim, false);
    filter_loop(v, off + 4 * stride, stride, 1, 8, lim, false);
}

/// `HFilter8i_C`: the single interior chroma vertical edge (column 4) on U and V.
fn h_filter8i(u: &mut [u8], v: &mut [u8], off: usize, stride: usize, lim: Limits) {
    filter_loop(u, off + 4, 1, stride, 8, lim, false);
    filter_loop(v, off + 4, 1, stride, 8, lim, false);
}

// -----------------------------------------------------------------------------
// Per-macroblock entry points (dec/frame_dec.c DoFilter)

/// Apply the *simple* in-loop filter to one luma macroblock at `off`.
///
/// Mirrors `DoFilter` for `filter_type == 1`: the left macroblock edge (when
/// `do_left_edge`), the interior vertical edges (when `info.f_inner`), the top
/// macroblock edge (when `do_top_edge`), then the interior horizontal edges.
/// Macroblock edges use limit `f_limit + 4`; interior edges use `f_limit`.
pub(crate) fn filter_mb_simple(
    y: &mut [u8],
    off: usize,
    stride: usize,
    do_left_edge: bool,
    do_top_edge: bool,
    info: FInfo,
) {
    if info.f_limit == 0 {
        return;
    }
    let limit = info.f_limit;
    if do_left_edge {
        simple_h_filter16(y, off, stride, limit + 4);
    }
    if info.f_inner {
        simple_h_filter16i(y, off, stride, limit);
    }
    if do_top_edge {
        simple_v_filter16(y, off, stride, limit + 4);
    }
    if info.f_inner {
        simple_v_filter16i(y, off, stride, limit);
    }
}

/// Apply the *normal* (complex) in-loop filter to one macroblock: luma at
/// `y_off` (stride `y_stride`) plus the co-located chroma at `uv_off` in both
/// `u` and `v` (stride `uv_stride`).
///
/// Mirrors `DoFilter` for `filter_type == 2`: left macroblock edges (luma +
/// chroma) when `do_left_edge`, interior vertical edges when `info.f_inner`,
/// top macroblock edges when `do_top_edge`, then interior horizontal edges.
/// Macroblock edges pass `thresh = f_limit + 4`; interior edges pass `thresh =
/// f_limit`; both pass `ithresh = f_ilevel` and `hev_thresh`.
#[allow(
    clippy::too_many_arguments,
    reason = "mirrors the reference VP8 DoFilter interface: three planes with \
              independent offsets/strides plus the two edge flags and the \
              filter-info the orchestration supplies per macroblock"
)]
pub(crate) fn filter_mb_normal(
    y: &mut [u8],
    y_off: usize,
    y_stride: usize,
    u: &mut [u8],
    v: &mut [u8],
    uv_off: usize,
    uv_stride: usize,
    do_left_edge: bool,
    do_top_edge: bool,
    info: FInfo,
) {
    if info.f_limit == 0 {
        return;
    }
    let outer = Limits {
        thresh: info.f_limit + 4,
        ithresh: info.f_ilevel,
        hev_thresh: info.hev_thresh,
    };
    let inner = Limits {
        thresh: info.f_limit,
        ithresh: info.f_ilevel,
        hev_thresh: info.hev_thresh,
    };
    if do_left_edge {
        h_filter16(y, y_off, y_stride, outer);
        h_filter8(u, v, uv_off, uv_stride, outer);
    }
    if info.f_inner {
        h_filter16i(y, y_off, y_stride, inner);
        h_filter8i(u, v, uv_off, uv_stride, inner);
    }
    if do_top_edge {
        v_filter16(y, y_off, y_stride, outer);
        v_filter8(u, v, uv_off, uv_stride, outer);
    }
    if info.f_inner {
        v_filter16i(y, y_off, y_stride, inner);
        v_filter8i(u, v, uv_off, uv_stride, inner);
    }
}

#[cfg(test)]
#[allow(
    clippy::match_same_arms,
    reason = "the fixture builders match each plane row/col explicitly to document the \
              filter's row layout; collapsing equal-valued rows would obscure that"
)]
mod tests {
    use super::{
        FInfo, filter_mb_normal, filter_mb_simple, filter2, filter4, filter6, hev, needs_filter,
        needs_filter2,
    };

    // Plane geometry for the integration KATs: a 48x48 luma/chroma plane with a
    // macroblock placed at (row 16, col 16) so every neighbor read (p3..q3, up
    // to 4 samples across an edge) stays inside the buffer.
    const S: usize = 48;
    const N: usize = S * S;
    const R: usize = 16;
    const C: usize = 16;

    // The reference clip semantics reproduced by the module under test:
    //   sclip1(v) = clamp(v, -128, 127)   (VP8ksclip1)
    //   sclip2(v) = clamp(v,  -16,  15)   (VP8ksclip2)
    //   clip8(v)  = clamp(v,    0, 255)   (VP8kclip1, stored as u8)
    // Right shifts `>> k` are arithmetic floor-divisions by 2^k; all the shifted
    // operands in these KATs are non-negative, so `n >> 7` = floor(n / 128).

    /// Fill every cell with a value that depends only on its row (a horizontal
    /// stripe pattern → a step in the *vertical* direction for top-edge KATs).
    fn fill_by_row(plane: &mut [u8], f: impl Fn(usize) -> u8) {
        for (r, row) in plane.chunks_exact_mut(S).enumerate() {
            row.fill(f(r));
        }
    }

    /// Fill every cell with a value that depends only on its column (a vertical
    /// stripe pattern → a step in the *horizontal* direction for left/interior
    /// vertical-edge KATs; also stays vertically flat so horizontal edges are
    /// no-ops).
    fn fill_by_col(plane: &mut [u8], f: impl Fn(usize) -> u8) {
        for row in plane.chunks_exact_mut(S) {
            for (c, px) in row.iter_mut().enumerate() {
                *px = f(c);
            }
        }
    }

    // -------------------------------------------------------------------------
    // Direct primitive KATs (DoFilter2/4/6, Hev, NeedsFilter/2). Distinct,
    // non-constant ramp inputs so a swapped tap, wrong coefficient, or off-by-one
    // shift changes the result.

    #[test]
    fn filter6_matches_hand_computed_6tap_kernel() {
        //   p2=100 p1=104 p0=108  q0=120 q1=124 q2=128
        //   a  = sclip1(3*(120-108) + sclip1(104-124))
        //      = sclip1(36 + sclip1(-20)) = sclip1(36 - 20) = 16
        //   a1 = (27*16+63)>>7 = 495>>7 = 3   (495 = 3*128 + 111)
        //   a2 = (18*16+63)>>7 = 351>>7 = 2   (351 = 2*128 + 95)
        //   a3 = ( 9*16+63)>>7 = 207>>7 = 1   (207 = 1*128 + 79)
        //   p2'=100+1=101  p1'=104+2=106  p0'=108+3=111
        //   q0'=120-3=117  q1'=124-2=122  q2'=128-1=127
        assert_eq!(
            filter6(100, 104, 108, 120, 124, 128),
            (101, 106, 111, 117, 122, 127)
        );
    }

    #[test]
    fn filter6_saturates_via_sclip1_and_clip8() {
        // Drive the outer sclip1 into saturation and both byte clips to the ends.
        //   p2=250 p1=100 p0=100  q0=200 q1=100 q2=8
        //   a  = sclip1(3*(200-100) + sclip1(100-100)) = sclip1(300) = 127
        //   a1 = (27*127+63)>>7 = 3492>>7 = 27  (3492 = 27*128 + 36)
        //   a2 = (18*127+63)>>7 = 2349>>7 = 18  (2349 = 18*128 + 45)
        //   a3 = ( 9*127+63)>>7 = 1206>>7 =  9  (1206 =  9*128 + 54)
        //   p2'=clip8(250+9)=clip8(259)=255   p1'=clip8(100+18)=118
        //   p0'=clip8(100+27)=127             q0'=clip8(200-27)=173
        //   q1'=clip8(100-18)=82              q2'=clip8(8-9)=clip8(-1)=0
        assert_eq!(
            filter6(250, 100, 100, 200, 100, 8),
            (255, 118, 127, 173, 82, 0)
        );
    }

    #[test]
    fn filter4_matches_hand_computed_4tap_kernel() {
        //   p1=90 p0=100 q0=130 q1=145   (DoFilter4's `a` ignores p1,q1)
        //   a  = 3*(130-100) = 90
        //   a1 = sclip2((90+4)>>3) = sclip2(11) = 11
        //   a2 = sclip2((90+3)>>3) = sclip2(11) = 11
        //   a3 = (11+1)>>1 = 6
        //   p1'=clip8(90+6)=96    p0'=clip8(100+11)=111
        //   q0'=clip8(130-11)=119 q1'=clip8(145-6)=139
        assert_eq!(filter4(90, 100, 130, 145), (96, 111, 119, 139));
    }

    #[test]
    fn filter2_matches_hand_computed_2tap_kernel_with_sclip2_saturation() {
        //   p1=60 p0=70 q0=150 q1=160
        //   a  = 3*(150-70) + sclip1(60-160) = 240 + (-100) = 140
        //   a1 = sclip2((140+4)>>3) = sclip2(18) = 15   (saturates)
        //   a2 = sclip2((140+3)>>3) = sclip2(17) = 15   (saturates)
        //   p0'=clip8(70+15)=85   q0'=clip8(150-15)=135
        assert_eq!(filter2(60, 70, 150, 160), (85, 135));
    }

    #[test]
    fn hev_uses_strict_greater_than_on_both_pairs() {
        // p-side dominates: p1=100 p0=105 q0=200 q1=203 → |p1-p0|=5, |q1-q0|=3
        assert!(!hev(100, 105, 200, 203, 5), "5 !> 5 and 3 !> 5 → false");
        assert!(hev(100, 105, 200, 203, 4), "5 > 4 on the p-side → true");
        // q-side dominates (p pair flat): p1=p0=120 ; q0=100 q1=112 → |q1-q0|=12
        assert!(!hev(120, 120, 100, 112, 12), "12 !> 12 → false");
        assert!(hev(120, 120, 100, 112, 11), "12 > 11 on the q-side → true");
    }

    #[test]
    fn needs_filter_uses_le_on_the_activation_sum() {
        // p1=100 p0=110 q0=120 q1=118 → 4*|p0-q0| + |p1-q1| = 4*10 + 18 = 58
        assert!(needs_filter(100, 110, 120, 118, 58), "58 <= 58 → true");
        assert!(!needs_filter(100, 110, 120, 118, 57), "58 > 57 → false");
    }

    #[test]
    fn needs_filter2_checks_main_sum_then_every_interior_step() {
        //   p3=90 p2=100 p1=108 p0=110 q0=120 q1=118 q2=130 q3=132
        //   main = 4*|110-120| + |108-118| = 40 + 10 = 50
        //   interior |diffs|: |90-100|=10 |100-108|=8 |108-110|=2
        //                     |132-130|=2 |130-118|=12 |118-120|=2 → max 12
        assert!(
            needs_filter2(90, 100, 108, 110, 120, 118, 130, 132, 50, 12),
            "main 50<=50 and interior all <=12"
        );
        assert!(
            !needs_filter2(90, 100, 108, 110, 120, 118, 130, 132, 49, 12),
            "main 50 > 49 → reject"
        );
        assert!(
            !needs_filter2(90, 100, 108, 110, 120, 118, 130, 132, 50, 11),
            "interior 12 > 11 → reject"
        );
    }

    // -------------------------------------------------------------------------
    // Integration KATs through the public entry points, exercising the full
    // dispatch (NeedsFilter2 gate → HEV select → 6/4/2-tap kernel).

    #[test]
    fn normal_top_edge_applies_do_filter6_to_luma_and_both_chroma() {
        // do_top_edge, f_inner=false → v_filter16 (luma) / v_filter8 (chroma) →
        // FilterLoop26 → NeedsFilter2 pass, HEV false → DoFilter6 (6-tap).
        //
        // A vertical step (constant across the filtered columns), p3..q3 on rows
        // 12..19 with the edge between row 15 (p0) and row 16 (q0).
        //
        // LUMA rows: 12:96 13:100 14:104 15:108 | 16:120 17:124 18:128 19:132
        //   NeedsFilter2 (thresh=f_limit+4=54, thresh2=109, ithresh=9):
        //     4*|108-120| + |104-124| = 48 + 20 = 68 <= 109        main ok
        //     every interior |diff| = 4 <= 9                        ok
        //   Hev(thresh=6): |p1-p0|=4 !>6 and |q1-q0|=4 !>6 → false
        //   DoFilter6: a=sclip1(3*(120-108)+sclip1(104-124))=sclip1(16)=16
        //     a1=495>>7=3  a2=351>>7=2  a3=207>>7=1
        //     rows 13..18 → 101 106 111 117 122 127 ; rows 12,19 untouched.
        //
        // CHROMA V rows: 12:60 13:66 14:72 15:78 | 16:96 17:102 18:108 19:114
        //   main = 4*|78-96| + |72-102| = 72 + 30 = 102 <= 109 ; interior 6 <= 9
        //   Hev(6): |p1-p0|=6 !>6, |q1-q0|=6 !>6 → false
        //   DoFilter6: a=sclip1(3*(96-78)+sclip1(72-102))=sclip1(24)=24
        //     a1=711>>7=5  a2=495>>7=3  a3=279>>7=2
        //     rows 13..18 → 68 75 83 91 99 106.
        //   CHROMA U reuses the luma stripe (→ 101..127). Asserting U and V with
        //   different data catches a skipped-plane or U/V-swap bug.
        let mut y = [0u8; N];
        let mut u = [0u8; N];
        let mut v = [0u8; N];
        fill_by_row(&mut y, |r| match r {
            12 => 96,
            13 => 100,
            14 => 104,
            15 => 108,
            16 => 120,
            17 => 124,
            18 => 128,
            19 => 132,
            _ => 0,
        });
        fill_by_row(&mut u, |r| match r {
            12 => 96,
            13 => 100,
            14 => 104,
            15 => 108,
            16 => 120,
            17 => 124,
            18 => 128,
            19 => 132,
            _ => 0,
        });
        fill_by_row(&mut v, |r| match r {
            12 => 60,
            13 => 66,
            14 => 72,
            15 => 78,
            16 => 96,
            17 => 102,
            18 => 108,
            19 => 114,
            _ => 0,
        });
        let off = R * S + C;
        let info = FInfo {
            f_limit: 50,
            f_ilevel: 9,
            f_inner: false,
            hev_thresh: 6,
        };
        filter_mb_normal(&mut y, off, S, &mut u, &mut v, off, S, false, true, info);

        for c in [C, C + 8] {
            // luma columns 16 and 24 both lie in the filtered span 16..=31.
            assert_eq!(y[12 * S + c], 96, "luma p3 untouched, col {c}");
            assert_eq!(y[13 * S + c], 101, "luma p2, col {c}");
            assert_eq!(y[14 * S + c], 106, "luma p1, col {c}");
            assert_eq!(y[15 * S + c], 111, "luma p0, col {c}");
            assert_eq!(y[16 * S + c], 117, "luma q0, col {c}");
            assert_eq!(y[17 * S + c], 122, "luma q1, col {c}");
            assert_eq!(y[18 * S + c], 127, "luma q2, col {c}");
            assert_eq!(y[19 * S + c], 132, "luma q3 untouched, col {c}");
        }
        for c in [C, C + 4] {
            // chroma columns 16 and 20 both lie in the 8-wide span 16..=23.
            assert_eq!(u[13 * S + c], 101, "U p2, col {c}");
            assert_eq!(u[14 * S + c], 106, "U p1, col {c}");
            assert_eq!(u[15 * S + c], 111, "U p0, col {c}");
            assert_eq!(u[16 * S + c], 117, "U q0, col {c}");
            assert_eq!(u[17 * S + c], 122, "U q1, col {c}");
            assert_eq!(u[18 * S + c], 127, "U q2, col {c}");
            assert_eq!(v[13 * S + c], 68, "V p2, col {c}");
            assert_eq!(v[14 * S + c], 75, "V p1, col {c}");
            assert_eq!(v[15 * S + c], 83, "V p0, col {c}");
            assert_eq!(v[16 * S + c], 91, "V q0, col {c}");
            assert_eq!(v[17 * S + c], 99, "V q1, col {c}");
            assert_eq!(v[18 * S + c], 106, "V q2, col {c}");
        }
    }

    #[test]
    fn normal_left_edge_applies_do_filter6() {
        // do_left_edge, f_inner=false → h_filter16 → FilterLoop26 (step=1) →
        // DoFilter6. Same neighborhood as the top-edge case but laid out along
        // columns, so this catches a horizontal/vertical (step vs stride) mixup.
        //   cols: 12:96 13:100 14:104 15:108 | 16:120 17:124 18:128 19:132
        //   (edge between col 15 (p0) and col 16 (q0); gating identical to the
        //    luma top-edge case: main=68<=109, HEV false → a=16)
        //   cols 13..18 → 101 106 111 117 122 127 ; cols 12,19 untouched.
        let mut y = [0u8; N];
        let mut u = [128u8; N];
        let mut v = [128u8; N];
        fill_by_col(&mut y, |c| match c {
            12 => 96,
            13 => 100,
            14 => 104,
            15 => 108,
            16 => 120,
            17 => 124,
            18 => 128,
            19 => 132,
            _ => 0,
        });
        let off = R * S + C;
        let info = FInfo {
            f_limit: 50,
            f_ilevel: 9,
            f_inner: false,
            hev_thresh: 6,
        };
        filter_mb_normal(&mut y, off, S, &mut u, &mut v, off, S, true, false, info);

        for r in [R, R + 8] {
            // rows 16 and 24 both lie in the 16-tall filtered span 16..=31.
            assert_eq!(y[r * S + 12], 96, "p3 untouched, row {r}");
            assert_eq!(y[r * S + 13], 101, "p2, row {r}");
            assert_eq!(y[r * S + 14], 106, "p1, row {r}");
            assert_eq!(y[r * S + 15], 111, "p0, row {r}");
            assert_eq!(y[r * S + 16], 117, "q0, row {r}");
            assert_eq!(y[r * S + 17], 122, "q1, row {r}");
            assert_eq!(y[r * S + 18], 127, "q2, row {r}");
            assert_eq!(y[r * S + 19], 132, "q3 untouched, row {r}");
        }
        // Flat chroma has nothing to filter.
        assert!(u.iter().all(|&p| p == 128));
        assert!(v.iter().all(|&p| p == 128));
    }

    #[test]
    fn normal_interior_hev_edge_applies_do_filter2() {
        // f_inner=true, edges off → h_filter16i → FilterLoop24; a HEV-*true*
        // interior edge takes the 2-tap DoFilter2 branch (NOT DoFilter4). The
        // stripe is column-only so the interior *horizontal* edges stay flat
        // (no-ops) and only the C+4=20 vertical edge trips NeedsFilter2.
        //   cols ..=19:100 | 20:120 21:128 | 22..=:130
        //   edge at col 20: p1=col18=100 p0=col19=100 q0=col20=120 q1=col21=128
        //   NeedsFilter2 (interior thresh=f_limit=60, thresh2=121, ithresh=9):
        //     4*|100-120| + |100-128| = 80 + 28 = 108 <= 121           main ok
        //     interior |diffs| = 0,0,0,0,|130-128|=2,|128-120|=8 <= 9  ok
        //   Hev(thresh=1): |p1-p0|=0, |q1-q0|=8 → 8>1 TRUE → DoFilter2
        //     a  = 3*(120-100) + sclip1(100-128) = 60 + (-28) = 32
        //     a1 = sclip2((32+4)>>3) = sclip2(4) = 4
        //     a2 = sclip2((32+3)>>3) = sclip2(4) = 4
        //     p0'=clip8(100+4)=104 (col19)   q0'=clip8(120-4)=116 (col20)
        //   col18/col21 untouched; the col24 edge is killed by its interior test
        //   (|116-128|=12 > 9) and the col28 edge sees a=0 → both leave 130s.
        let mut y = [0u8; N];
        let mut u = [128u8; N];
        let mut v = [128u8; N];
        fill_by_col(&mut y, |c| match c {
            0..=19 => 100,
            20 => 120,
            21 => 128,
            _ => 130,
        });
        let off = R * S + C;
        let info = FInfo {
            f_limit: 60,
            f_ilevel: 9,
            f_inner: true,
            hev_thresh: 1,
        };
        filter_mb_normal(&mut y, off, S, &mut u, &mut v, off, S, false, false, info);

        for r in [R, R + 8] {
            assert_eq!(y[r * S + 18], 100, "p1 untouched, row {r}");
            assert_eq!(y[r * S + 19], 104, "p0 → 104, row {r}");
            assert_eq!(y[r * S + 20], 116, "q0 → 116, row {r}");
            assert_eq!(y[r * S + 21], 128, "q1 untouched, row {r}");
            assert_eq!(y[r * S + 23], 130, "col24 edge region flat, row {r}");
            assert_eq!(y[r * S + 24], 130, "col24 edge no-op, row {r}");
        }
        // Flat chroma unchanged by the interior chroma edges.
        assert!(u.iter().all(|&p| p == 128));
        assert!(v.iter().all(|&p| p == 128));
    }

    #[test]
    fn normal_interior_nonhev_edge_applies_do_filter4() {
        // f_inner=true, edges off → interior edge with HEV *false* → the 4-tap
        // DoFilter4 branch. A pure single-boundary step gives |p1-p0|=|q1-q0|=0.
        //   cols ..=19:100 | 20..=:112   (edge at col 20)
        //   p1=p0=100  q0=q1=112
        //   NeedsFilter2 (thresh=f_limit=40, thresh2=81, ithresh=9):
        //     4*|100-112| + |100-112| = 48 + 12 = 60 <= 81      main ok
        //     interior |diffs| all 0 <= 9                        ok
        //   Hev(thresh=1): |p1-p0|=0, |q1-q0|=0 → false → DoFilter4
        //     a  = 3*(112-100) = 36
        //     a1 = sclip2((36+4)>>3) = sclip2(5) = 5
        //     a2 = sclip2((36+3)>>3) = sclip2(4) = 4
        //     a3 = (5+1)>>1 = 3
        //     p1'=clip8(100+3)=103 (col18)  p0'=clip8(100+4)=104 (col19)
        //     q0'=clip8(112-5)=107 (col20)  q1'=clip8(112-3)=109 (col21)
        //   col17/col22 untouched; the col24/col28 edges see a=0 → no change.
        let mut y = [0u8; N];
        let mut u = [128u8; N];
        let mut v = [128u8; N];
        fill_by_col(&mut y, |c| if c < 20 { 100 } else { 112 });
        let off = R * S + C;
        let info = FInfo {
            f_limit: 40,
            f_ilevel: 9,
            f_inner: true,
            hev_thresh: 1,
        };
        filter_mb_normal(&mut y, off, S, &mut u, &mut v, off, S, false, false, info);

        for r in [R, R + 8] {
            assert_eq!(y[r * S + 17], 100, "p2 untouched, row {r}");
            assert_eq!(y[r * S + 18], 103, "p1 → 103, row {r}");
            assert_eq!(y[r * S + 19], 104, "p0 → 104, row {r}");
            assert_eq!(y[r * S + 20], 107, "q0 → 107, row {r}");
            assert_eq!(y[r * S + 21], 109, "q1 → 109, row {r}");
            assert_eq!(y[r * S + 22], 112, "q2 untouched, row {r}");
        }
    }

    // -------------------------------------------------------------------------
    // Simple in-loop filter (kept + strengthened) and the f_limit==0 guards.

    #[test]
    fn simple_left_edge_smooths_a_step() {
        // A vertical step 128 | 134 at the left macroblock edge. DoFilter2 with
        // thresh = f_limit + 4 = 24 (thresh2 = 49 >= 30) gives:
        //   a  = 3*(134-128) + sclip1(128-134) = 18 + (-6) = 12
        //   a1 = sclip2((12+4)>>3 = 2) = 2 ;  a2 = sclip2((12+3)>>3 = 1) = 1
        //   p0(col 15) = 128 + 1 = 129 ;      q0(col 16) = 134 - 2 = 132
        let off_col = 16usize;
        let off_row = 16usize;
        let mut y = [0u8; N];
        for (idx, px) in y.iter_mut().enumerate() {
            *px = if idx % S < off_col { 128 } else { 134 };
        }
        let info = FInfo {
            f_limit: 20,
            f_ilevel: 9,
            f_inner: false,
            hev_thresh: 0,
        };
        filter_mb_simple(&mut y, off_row * S + off_col, S, true, false, info);
        for r in off_row..off_row + 16 {
            assert_eq!(y[r * S + (off_col - 2)], 128, "p1 row {r}");
            assert_eq!(y[r * S + (off_col - 1)], 129, "p0 row {r}");
            assert_eq!(y[r * S + off_col], 132, "q0 row {r}");
            assert_eq!(y[r * S + (off_col + 1)], 134, "q1 row {r}");
        }
    }

    #[test]
    fn zero_f_limit_short_circuits_the_normal_filter() {
        // Same stepped plane, same left edge: with f_limit=50 DoFilter6 rewrites
        // columns 13..18 (see normal_left_edge_applies_do_filter6); with
        // f_limit=0 the guard returns before touching a single byte. Proving both
        // on one input shows the early-return — not an accidental no-op — is what
        // suppresses the filter.
        let mut base = [0u8; N];
        fill_by_col(&mut base, |c| match c {
            12 => 96,
            13 => 100,
            14 => 104,
            15 => 108,
            16 => 120,
            17 => 124,
            18 => 128,
            19 => 132,
            _ => 140,
        });
        let mut u = [128u8; N];
        let mut v = [128u8; N];
        let off = R * S + C;

        let mut active = base;
        let info_on = FInfo {
            f_limit: 50,
            f_ilevel: 9,
            f_inner: false,
            hev_thresh: 6,
        };
        filter_mb_normal(
            &mut active,
            off,
            S,
            &mut u,
            &mut v,
            off,
            S,
            true,
            false,
            info_on,
        );
        assert_ne!(active, base, "sanity: this plane is actually filterable");

        let mut idle = base;
        let info_off = FInfo {
            f_limit: 0,
            f_ilevel: 9,
            f_inner: true,
            hev_thresh: 6,
        };
        filter_mb_normal(
            &mut idle, off, S, &mut u, &mut v, off, S, true, true, info_off,
        );
        assert_eq!(idle, base, "f_limit==0 must return before filtering");
    }

    #[test]
    fn zero_f_limit_short_circuits_the_simple_filter() {
        // 128 | 134 step at the left edge. f_limit=20 → DoFilter2 rewrites cols
        // 15,16 (see simple_left_edge_smooths_a_step); f_limit=0 → untouched.
        let mut base = [0u8; N];
        fill_by_col(&mut base, |c| if c < C { 128 } else { 134 });
        let off = R * S + C;

        let mut active = base;
        filter_mb_simple(
            &mut active,
            off,
            S,
            true,
            false,
            FInfo {
                f_limit: 20,
                f_ilevel: 9,
                f_inner: false,
                hev_thresh: 0,
            },
        );
        assert_ne!(active, base, "sanity: this step is actually filterable");

        let mut idle = base;
        filter_mb_simple(
            &mut idle,
            off,
            S,
            true,
            false,
            FInfo {
                f_limit: 0,
                f_ilevel: 9,
                f_inner: false,
                hev_thresh: 0,
            },
        );
        assert_eq!(idle, base, "f_limit==0 must return before filtering");
    }

    // -------------------------------------------------------------------------
    // Simple-filter NeedsFilter boundary: the activation sum is engineered to
    // equal thresh2 = 2*(limit+4)+1 *exactly*, so the edge filters on real code
    // but is rejected by any mutation that shifts thresh2 down — the `2*thresh+1`
    // in simple_v/h_filter16 or the `limit+4` in filter_mb_simple.

    #[test]
    fn simple_top_edge_needs_filter_threshold_is_exactly_2t_plus_1() {
        // do_top_edge, f_inner=false → simple_v_filter16 on the row-16 edge.
        //   rows: 14:103 15:100 | 16:102 17:100   (edge between rows 15 and 16)
        //   f_limit=1 → thresh=5, thresh2 = 2*5+1 = 11
        //   NeedsFilter: 4*|100-102| + |103-100| = 8 + 3 = 11 <= 11   (passes;
        //     any thresh2 in {9,10} from a mutated `2*thresh+1` or `limit+4`
        //     rejects it)
        //   DoFilter2: a = 3*(102-100) + sclip1(103-100) = 6 + 3 = 9
        //     a1 = sclip2((9+4)>>3) = 1 ; a2 = sclip2((9+3)>>3) = 1
        //     p0(row15) = 100 + 1 = 101 ; q0(row16) = 102 - 1 = 101
        let mut y = [0u8; N];
        fill_by_row(&mut y, |r| match r {
            14 => 103,
            15 => 100,
            16 => 102,
            17 => 100,
            _ => 0,
        });
        let info = FInfo {
            f_limit: 1,
            f_ilevel: 9,
            f_inner: false,
            hev_thresh: 0,
        };
        filter_mb_simple(&mut y, R * S + C, S, false, true, info);
        for c in [C, C + 15] {
            assert_eq!(y[14 * S + c], 103, "p1 untouched, col {c}");
            assert_eq!(y[15 * S + c], 101, "p0 → 101, col {c}");
            assert_eq!(y[16 * S + c], 101, "q0 → 101, col {c}");
            assert_eq!(y[17 * S + c], 100, "q1 untouched, col {c}");
        }
    }

    #[test]
    fn simple_left_edge_needs_filter_threshold_is_exactly_2t_plus_1() {
        // Mirror of the top-edge boundary case along columns: do_left_edge →
        // simple_h_filter16 on the col-16 edge (identical arithmetic → col15→101,
        // col16→101). Kills the `2*thresh+1` in simple_h_filter16 and the
        // `limit+4` in filter_mb_simple's left-edge call.
        let mut y = [0u8; N];
        fill_by_col(&mut y, |c| match c {
            14 => 103,
            15 => 100,
            16 => 102,
            17 => 100,
            _ => 0,
        });
        let info = FInfo {
            f_limit: 1,
            f_ilevel: 9,
            f_inner: false,
            hev_thresh: 0,
        };
        filter_mb_simple(&mut y, R * S + C, S, true, false, info);
        for r in [R, R + 15] {
            assert_eq!(y[r * S + 14], 103, "p1 untouched, row {r}");
            assert_eq!(y[r * S + 15], 101, "p0 → 101, row {r}");
            assert_eq!(y[r * S + 16], 101, "q0 → 101, row {r}");
            assert_eq!(y[r * S + 17], 100, "q1 untouched, row {r}");
        }
    }

    // -------------------------------------------------------------------------
    // Simple-filter interior edges: a filterable step at each of the three
    // interior sub-edges pins the `pos += 4*stride` / `pos += 4` walk. A dropped
    // body, a negated/rescaled step, or a `4/stride`=0 step all leave at least one
    // sub-edge unfiltered (or index out of bounds), which the exact-value
    // assertions catch.

    #[test]
    fn simple_interior_horizontal_edges_filter_rows_4_8_12() {
        // f_inner=true, edges off → simple_v_filter16i, interior horizontal edges
        // at rows off+4/8/12 (= 20/24/28). Each step: p1=p0=100 | q0=q1=120 →
        //   NeedsFilter 4*20+20 = 100 <= 2*60+1 = 121
        //   DoFilter2: a = 3*(120-100)+sclip1(-20) = 40 ; a1=a2=sclip2(5)=5
        //     p0 → 105 ; q0 → 115
        let mut y = [0u8; N];
        fill_by_row(&mut y, |r| match r {
            20 | 21 | 24 | 25 | 28 | 29 => 120,
            18 | 19 | 22 | 23 | 26 | 27 => 100,
            _ => 0,
        });
        let info = FInfo {
            f_limit: 60,
            f_ilevel: 9,
            f_inner: true,
            hev_thresh: 0,
        };
        filter_mb_simple(&mut y, R * S + C, S, false, false, info);
        for c in [C, C + 15] {
            for (p0_row, q0_row) in [(19, 20), (23, 24), (27, 28)] {
                assert_eq!(y[p0_row * S + c], 105, "p0 row {p0_row}, col {c}");
                assert_eq!(y[q0_row * S + c], 115, "q0 row {q0_row}, col {c}");
            }
            // Rows outside the three interior edges stay put (a `4/stride`=0 step
            // would filter the row-16 edge instead, a `-=` step rows 4/8/12).
            assert_eq!(y[16 * S + c], 0, "top-edge row untouched, col {c}");
            assert_eq!(y[18 * S + c], 100, "row 18 untouched, col {c}");
        }
    }

    #[test]
    fn simple_interior_vertical_edges_filter_cols_4_8_12() {
        // f_inner=true → simple_h_filter16i, interior vertical edges at cols
        // off+4/8/12 (= 20/24/28). Mirror of the horizontal case along columns.
        let mut y = [0u8; N];
        fill_by_col(&mut y, |c| match c {
            20 | 21 | 24 | 25 | 28 | 29 => 120,
            18 | 19 | 22 | 23 | 26 | 27 => 100,
            _ => 0,
        });
        let info = FInfo {
            f_limit: 60,
            f_ilevel: 9,
            f_inner: true,
            hev_thresh: 0,
        };
        filter_mb_simple(&mut y, R * S + C, S, false, false, info);
        for r in [R, R + 15] {
            for (p0_col, q0_col) in [(19, 20), (23, 24), (27, 28)] {
                assert_eq!(y[r * S + p0_col], 105, "p0 col {p0_col}, row {r}");
                assert_eq!(y[r * S + q0_col], 115, "q0 col {q0_col}, row {r}");
            }
            assert_eq!(y[r * S + 16], 0, "left-edge col untouched, row {r}");
            assert_eq!(y[r * S + 18], 100, "col 18 untouched, row {r}");
        }
    }

    // -------------------------------------------------------------------------
    // Normal-filter coverage the earlier KATs miss: a luma interior *horizontal*
    // edge, a chroma-V interior edge, and the FilterLoop NeedsFilter2 boundary.

    #[test]
    fn normal_interior_horizontal_nonhev_edge_applies_do_filter4() {
        // f_inner=true, edges off, a *row* step → v_filter16i drives FilterLoop24
        // on the interior horizontal edge at row off+4 (= 20). HEV false → the
        // 4-tap DoFilter4 writes p1,p0,q0,q1. This is the only test that runs a
        // luma interior horizontal edge, so it kills both a dropped v_filter16i
        // body and the `pos - 2*step` p1 write-back (step = stride here, so a
        // `2/step`=0 mis-targets p1 onto q0 and leaves the real p1 unwritten).
        //   rows ..=19:100 | 20..:112 (edge at row 20) ; p1=p0=100 q0=q1=112
        //   main 4*12+12 = 60 <= 2*40+1 = 81 ; interior diffs 0 ; HEV false
        //   DoFilter4: a=3*12=36 ; a1=sclip2(5)=5 a2=sclip2(4)=4 a3=(5+1)>>1=3
        //     p1(row18)→103 p0(row19)→104 q0(row20)→107 q1(row21)→109
        let mut y = [0u8; N];
        let mut u = [128u8; N];
        let mut v = [128u8; N];
        fill_by_row(&mut y, |r| if r < 20 { 100 } else { 112 });
        let off = R * S + C;
        let info = FInfo {
            f_limit: 40,
            f_ilevel: 9,
            f_inner: true,
            hev_thresh: 1,
        };
        filter_mb_normal(&mut y, off, S, &mut u, &mut v, off, S, false, false, info);
        for c in [C, C + 15] {
            assert_eq!(y[17 * S + c], 100, "p2 untouched, col {c}");
            assert_eq!(y[18 * S + c], 103, "p1 → 103, col {c}");
            assert_eq!(y[19 * S + c], 104, "p0 → 104, col {c}");
            assert_eq!(y[20 * S + c], 107, "q0 → 107, col {c}");
            assert_eq!(y[21 * S + c], 109, "q1 → 109, col {c}");
            assert_eq!(y[22 * S + c], 112, "q2 untouched, col {c}");
        }
        assert!(u.iter().all(|&p| p == 128), "flat chroma untouched");
        assert!(v.iter().all(|&p| p == 128), "flat chroma untouched");
    }

    #[test]
    fn normal_interior_chroma_v_edge_filters_column_4_of_the_v_plane() {
        // f_inner=true → h_filter8i filters the interior vertical chroma edge at
        // col uv_off+4 (= 20) on U then V. A column step on the V plane only pins
        // that offset: negating `off + 4` to `off - 4` filters a flat column and
        // leaves col 20 unfiltered.
        //   V cols ..=19:100 | 20..:112 → HEV false → DoFilter4:
        //     col18→103 col19→104 col20→107 col21→109
        let mut y = [100u8; N];
        let mut u = [100u8; N];
        let mut v = [0u8; N];
        fill_by_col(&mut v, |c| if c < 20 { 100 } else { 112 });
        let off = R * S + C;
        let info = FInfo {
            f_limit: 40,
            f_ilevel: 9,
            f_inner: true,
            hev_thresh: 1,
        };
        filter_mb_normal(&mut y, off, S, &mut u, &mut v, off, S, false, false, info);
        for r in [R, R + 7] {
            // the chroma edge spans 8 rows: 16..=23.
            assert_eq!(v[r * S + 17], 100, "V p2 untouched, row {r}");
            assert_eq!(v[r * S + 18], 103, "V p1 → 103, row {r}");
            assert_eq!(v[r * S + 19], 104, "V p0 → 104, row {r}");
            assert_eq!(v[r * S + 20], 107, "V q0 → 107, row {r}");
            assert_eq!(v[r * S + 21], 109, "V q1 → 109, row {r}");
            assert_eq!(v[r * S + 22], 112, "V q2 untouched, row {r}");
        }
    }

    #[test]
    fn normal_filter_loop_threshold_is_exactly_2t_plus_1() {
        // do_top_edge, f_inner=false → v_filter16 → FilterLoop26. The main
        // activation sum equals thresh2 = 2*(f_limit+4)+1 exactly, so the edge
        // filters on real code but any mutation of that `2*thresh+1` (→ 2t-1 or
        // 2t) rejects it.
        //   rows 12..15:100 | 16:125 17:109 18:109 19:109  (edge rows 15|16)
        //   f_limit=50 → thresh=54, thresh2 = 109
        //   NeedsFilter2 main = 4*|100-125| + |100-109| = 100 + 9 = 109 <= 109
        //     interior max |109-125| = 16 <= f_ilevel 20
        //   HEV(6): |q1-q0| = 16 > 6 → DoFilter2
        //     a = 3*(125-100) + sclip1(100-109) = 75 - 9 = 66 ; a1=a2=sclip2(8)=8
        //     p0(row15) → 108 ; q0(row16) → 117
        let mut y = [0u8; N];
        let mut u = [128u8; N];
        let mut v = [128u8; N];
        fill_by_row(&mut y, |r| match r {
            12..=15 => 100,
            16 => 125,
            17..=19 => 109,
            _ => 0,
        });
        let off = R * S + C;
        let info = FInfo {
            f_limit: 50,
            f_ilevel: 20,
            f_inner: false,
            hev_thresh: 6,
        };
        filter_mb_normal(&mut y, off, S, &mut u, &mut v, off, S, false, true, info);
        for c in [C, C + 15] {
            assert_eq!(y[14 * S + c], 100, "p1 untouched, col {c}");
            assert_eq!(y[15 * S + c], 108, "p0 → 108, col {c}");
            assert_eq!(y[16 * S + c], 117, "q0 → 117, col {c}");
            assert_eq!(y[17 * S + c], 109, "q1 untouched, col {c}");
        }
    }
}
