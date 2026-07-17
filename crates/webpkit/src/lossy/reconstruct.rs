//! VP8 key-frame reconstruction and in-loop filtering orchestration.
//!
//! Drives the pixel pipeline once a macroblock's syntax is parsed: predict each
//! block from already-reconstructed neighbors (`predict`), add the inverse-DCT
//! residual (`idct`), then — after the whole frame is reconstructed — run one
//! in-place raster loop-filter pass (`loop_filter`) and convert the cropped YUV
//! planes to RGBA (`yuv`).
//!
//! Bit-exact per the libwebp `dec/frame_dec.c` contract: reconstruction reads
//! only *unfiltered* neighbors, so reconstruct-whole-frame-then-filter is
//! byte-identical to libwebp's rolling per-row scheme. Full padded planes carry
//! a 1-pixel top border (127), a 1-pixel left border (129) and a 4-pixel right
//! margin for the intra-4×4 top-right lane.
#![allow(
    clippy::cast_possible_truncation,
    reason = "border/dimension values are provably in range; casts reproduce the \
              reference decoder's byte handling"
)]

use crate::{Codec, Dimensions, Error, Image, Metadata, PixelLayout, Result};

use crate::lossy::decode::{FilterHeader, MbData, SegmentHeader};
use crate::lossy::loop_filter::{self, FInfo};
use crate::lossy::predict;
use crate::lossy::prelude::*;
use crate::lossy::{idct, yuv};

/// The padded Y/U/V reconstruction planes and their strides.
pub(crate) struct Planes {
    /// Luma plane (`y_stride` × `1 + mb_h*16` rows).
    pub(crate) y: Vec<u8>,
    /// U chroma plane (`uv_stride` × `1 + mb_h*8` rows).
    pub(crate) u: Vec<u8>,
    /// V chroma plane.
    pub(crate) v: Vec<u8>,
    /// Luma row stride: `1 + mb_w*16 + 4` (left border + width + top-right margin).
    pub(crate) y_stride: usize,
    /// Chroma row stride: `1 + mb_w*8`.
    pub(crate) uv_stride: usize,
}

impl Planes {
    /// Allocate the padded planes and fill the constant borders: the top row is
    /// `127`, the left column `129` (the frame's top-left corner stays `127`).
    pub(crate) fn new(mb_w: usize, mb_h: usize) -> Self {
        let y_stride = 1 + mb_w * 16 + 4;
        let uv_stride = 1 + mb_w * 8;
        let mut y = vec![0u8; y_stride * (1 + mb_h * 16)];
        let mut u = vec![0u8; uv_stride * (1 + mb_h * 8)];
        let mut v = vec![0u8; uv_stride * (1 + mb_h * 8)];
        fill_borders(&mut y, y_stride, 1 + mb_h * 16);
        fill_borders(&mut u, uv_stride, 1 + mb_h * 8);
        fill_borders(&mut v, uv_stride, 1 + mb_h * 8);
        Self {
            y,
            u,
            v,
            y_stride,
            uv_stride,
        }
    }
}

/// Fill a plane's top border row with `127` and its left border column with
/// `129` (the corner keeps the top value `127`).
fn fill_borders(plane: &mut [u8], stride: usize, rows: usize) {
    plane[0..stride].fill(127);
    for row in 1..rows {
        plane[row * stride] = 129;
    }
}

/// Copy the real `w × h` top-left region of a padded `plane` (whose real `(0,0)`
/// sits at `[stride + 1]`, after the 1-pixel top/left borders) into a packed,
/// row-contiguous buffer — the plane shape `decode_yuv` returns and the Level-A
/// YUV oracle compares against libwebp.
fn crop(plane: &[u8], stride: usize, w: usize, h: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(w * h);
    for y in 0..h {
        let base = (y + 1) * stride + 1;
        out.extend_from_slice(&plane[base..base + w]);
    }
    out
}

impl Planes {
    /// The reconstructed luma plane cropped to `w × h`.
    pub(crate) fn crop_y(&self, w: usize, h: usize) -> Vec<u8> {
        crop(&self.y, self.y_stride, w, h)
    }

    /// The reconstructed U plane cropped to `w × h`.
    pub(crate) fn crop_u(&self, w: usize, h: usize) -> Vec<u8> {
        crop(&self.u, self.uv_stride, w, h)
    }

    /// The reconstructed V plane cropped to `w × h`.
    pub(crate) fn crop_v(&self, w: usize, h: usize) -> Vec<u8> {
        crop(&self.v, self.uv_stride, w, h)
    }
}

/// Reconstruct one macroblock into `planes` from its parsed `block` data. Thin
/// wrapper computing the plane offsets and the three edge flags from the grid
/// position, then delegating to the coordinate-free [`reconstruct_mb_at`].
pub(crate) fn reconstruct_mb(
    planes: &mut Planes,
    block: &MbData,
    mb_x: usize,
    mb_y: usize,
    mb_w: usize,
) {
    let y_off = (mb_y * 16 + 1) * planes.y_stride + (mb_x * 16 + 1);
    let uv_off = (mb_y * 8 + 1) * planes.uv_stride + (mb_x * 8 + 1);
    reconstruct_mb_at(
        planes,
        block,
        y_off,
        uv_off,
        mb_y > 0,
        mb_x > 0,
        mb_x == mb_w - 1,
    );
}

/// Reconstruct one macroblock at explicit plane offsets and edge flags. This is
/// the coordinate-free core: it derives nothing from a grid position, so the
/// encoder's wavefront scheduler can drive it against a per-macroblock scratch
/// buffer (where the macroblock sits at local `(0, 0)`) while still supplying the
/// true `has_top`/`has_left`/`is_rightmost` geometry the intra-4×4 top-right lane
/// depends on.
pub(crate) fn reconstruct_mb_at(
    planes: &mut Planes,
    block: &MbData,
    y_off: usize,
    uv_off: usize,
    has_top: bool,
    has_left: bool,
    is_rightmost: bool,
) {
    if block.is_i4x4 {
        reconstruct_luma_i4x4(planes, block, y_off, has_top, is_rightmost);
    } else {
        reconstruct_luma_16(planes, block, y_off, has_top, has_left);
    }
    reconstruct_chroma(planes, block, uv_off, has_top, has_left);
}

/// Add the inverse-DCT residual of coefficient block `idx` (16 coeffs) onto the
/// 4×4 at `sub`, dispatching on the block's content: an all-zero block is a no-op,
/// a block whose only non-zero coefficient is its DC term takes the DC-only fast
/// path ([`idct::transform_dc`], byte-identical to the full transform but skipping
/// both butterfly passes), and any block carrying AC coefficients runs the full
/// inverse DCT. Deciding straight from the coefficients keeps this correct on both
/// the decode and the encoder-reconstruction path (the latter does not carry the
/// decoder's packed per-block non-zero codes).
fn add_residual(plane: &mut [u8], coeffs: &[i16; 384], idx: usize, sub: usize, stride: usize) {
    let block = &coeffs[idx * 16..idx * 16 + 16];
    if block[1..].iter().any(|&c| c != 0) {
        idct::transform_one(block, plane, sub, stride);
    } else if block[0] != 0 {
        idct::transform_dc(block[0], plane, sub, stride);
    }
}

/// 16×16-predicted luma: predict the whole block, then add the 16 sub-block
/// residuals in raster (`kScan`) order.
fn reconstruct_luma_16(
    planes: &mut Planes,
    block: &MbData,
    y_off: usize,
    has_top: bool,
    has_left: bool,
) {
    let stride = planes.y_stride;
    predict::predict_luma16(
        &mut planes.y,
        y_off,
        stride,
        block.imodes[0],
        has_top,
        has_left,
    );
    for n in 0..16 {
        let sub = y_off + (n % 4) * 4 + (n / 4) * 4 * stride;
        add_residual(&mut planes.y, &block.coeffs, n, sub, stride);
    }
}

/// Intra-4×4 luma: set up the top-right lane, then predict-and-reconstruct each
/// 4×4 in raster order so later sub-blocks read reconstructed siblings.
fn reconstruct_luma_i4x4(
    planes: &mut Planes,
    block: &MbData,
    y_off: usize,
    has_top: bool,
    is_rightmost: bool,
) {
    let stride = planes.y_stride;
    fill_top_right_lane(&mut planes.y, y_off, stride, has_top, is_rightmost);
    for n in 0..16 {
        let sub = y_off + (n % 4) * 4 + (n / 4) * 4 * stride;
        predict::predict_luma4(&mut planes.y, sub, stride, block.imodes[n]);
        add_residual(&mut planes.y, &block.coeffs, n, sub, stride);
    }
}

/// Fill the four intra-4×4 top-right samples of an MB and replicate them down to
/// rows 3, 7, 11 so the right-column sub-blocks read the MB's top-right, not the
/// (unreconstructed) macroblock to the right. Port of `ReconstructRow`'s lane
/// setup (`dec/frame_dec.c`). Also called by the encoder's per-sub-block i4x4
/// mode search so its prediction reads the exact same top-right lane the decoder
/// will reconstruct from.
pub(crate) fn fill_top_right_lane(
    y: &mut [u8],
    y_off: usize,
    stride: usize,
    has_top: bool,
    is_rightmost: bool,
) {
    let tr = y_off - stride + 16;
    if !has_top {
        // The top border row (including the right margin) is already 127.
    } else if is_rightmost {
        // Rightmost column: no neighbor, replicate the MB's own last top sample.
        let last = y[y_off - stride + 15];
        y[tr..tr + 4].fill(last);
    }
    // Otherwise the above-right MB's bottom-left samples are already in place.
    let lane = [y[tr], y[tr + 1], y[tr + 2], y[tr + 3]];
    for r in [3usize, 7, 11] {
        let dst = y_off + r * stride + 16;
        y[dst..dst + 4].copy_from_slice(&lane);
    }
}

/// Predict and reconstruct the 8×8 U and V chroma blocks (4 sub-blocks each).
fn reconstruct_chroma(
    planes: &mut Planes,
    block: &MbData,
    uv_off: usize,
    has_top: bool,
    has_left: bool,
) {
    let stride = planes.uv_stride;
    predict::predict_chroma8(
        &mut planes.u,
        uv_off,
        stride,
        block.uvmode,
        has_top,
        has_left,
    );
    predict::predict_chroma8(
        &mut planes.v,
        uv_off,
        stride,
        block.uvmode,
        has_top,
        has_left,
    );
    for n in 0..4 {
        let sub = uv_off + (n % 2) * 4 + (n / 2) * 4 * stride;
        add_residual(&mut planes.u, &block.coeffs, 16 + n, sub, stride);
        add_residual(&mut planes.v, &block.coeffs, 20 + n, sub, stride);
    }
}

/// Precompute the `[segment][is_i4x4]` filter strengths from the frame's filter
/// and segment headers (libwebp `PrecomputeFilterStrengths`). Taking the two
/// headers directly (rather than the whole `Frame`) keeps this a pure function
/// of its inputs, so it is directly unit-testable.
pub(crate) fn compute_fstrengths(
    segment: &SegmentHeader,
    filter: &FilterHeader,
) -> [[FInfo; 2]; 4] {
    let mut table = [[FInfo::default(); 2]; 4];
    for (s, seg) in table.iter_mut().enumerate() {
        let base = if segment.use_segment {
            let mut b = segment.filter_strength[s];
            if !segment.absolute_delta {
                b += filter.level;
            }
            b
        } else {
            filter.level
        };
        for (i4x4, info) in seg.iter_mut().enumerate() {
            *info = strength_for(filter, base, i4x4 == 1);
        }
    }
    table
}

/// One `FInfo` from a base level plus the optional loop-filter deltas.
fn strength_for(filter: &FilterHeader, base: i32, i4x4: bool) -> FInfo {
    let mut level = base;
    if filter.use_lf_delta {
        level += filter.ref_lf_delta[0]; // intra-frame reference delta
        if i4x4 {
            level += filter.mode_lf_delta[0]; // B_PRED mode delta
        }
    }
    let level = level.clamp(0, 63);
    if level == 0 {
        return FInfo::default();
    }
    let mut inner = level;
    if filter.sharpness > 0 {
        inner >>= if filter.sharpness > 4 { 2 } else { 1 };
        let cap = 9 - filter.sharpness;
        if inner > cap {
            inner = cap;
        }
    }
    if inner < 1 {
        inner = 1;
    }
    FInfo {
        f_limit: 2 * level + inner,
        f_ilevel: inner,
        f_inner: i4x4,
        hev_thresh: if level >= 40 {
            2
        } else {
            i32::from(level >= 15)
        },
    }
}

/// Resolve one macroblock's final `FInfo`: pick `fstrengths[segment][is_i4x4]`
/// then set `f_inner = is_i4x4 || has-residual` (libwebp `f_inner |= !skip`).
pub(crate) fn resolve_finfo(table: [[FInfo; 2]; 4], block: &MbData, use_skip: bool) -> FInfo {
    let mut skip = use_skip && block.skip;
    if !skip {
        skip = (block.non_zero_y | block.non_zero_uv) == 0;
    }
    let mut info = table[usize::from(block.segment)][usize::from(block.is_i4x4)];
    info.f_inner = block.is_i4x4 || !skip;
    info
}

/// Apply the in-loop filter to a single macroblock row `mb_y`. `finfo_row` is
/// that row's `mb_w` resolved [`FInfo`] entries. A no-op when `filter_type == 0`.
/// Reads/writes only rows within this MB row plus the few top-edge samples of the
/// row above, so filtering a row is independent of whether lower rows exist yet —
/// the byte-identity guarantee the streaming decoder relies on.
pub(crate) fn filter_mb_row(
    planes: &mut Planes,
    finfo_row: &[FInfo],
    mb_y: usize,
    filter_type: u8,
) {
    if filter_type == 0 {
        return;
    }
    for (mb_x, &info) in finfo_row.iter().enumerate() {
        let (do_left, do_top) = (mb_x > 0, mb_y > 0);
        let y_off = (mb_y * 16 + 1) * planes.y_stride + (mb_x * 16 + 1);
        let uv_off = (mb_y * 8 + 1) * planes.uv_stride + (mb_x * 8 + 1);
        if filter_type == 1 {
            loop_filter::filter_mb_simple(
                &mut planes.y,
                y_off,
                planes.y_stride,
                do_left,
                do_top,
                info,
            );
        } else {
            loop_filter::filter_mb_normal(
                &mut planes.y,
                y_off,
                planes.y_stride,
                &mut planes.u,
                &mut planes.v,
                uv_off,
                planes.uv_stride,
                do_left,
                do_top,
                info,
            );
        }
    }
}

/// Run one whole-frame in-place raster loop-filter pass (skipped if filtering is
/// off), matching libwebp's per-macroblock `DoFilter` sequence and guards.
pub(crate) fn filter_frame(
    planes: &mut Planes,
    finfo: &[FInfo],
    mb_w: usize,
    mb_h: usize,
    filter_type: u8,
) {
    for mb_y in 0..mb_h {
        filter_mb_row(
            planes,
            &finfo[mb_y * mb_w..(mb_y + 1) * mb_w],
            mb_y,
            filter_type,
        );
    }
}

/// Crop the padded planes to `width`×`height` and convert to an RGBA [`Image`]
/// (opaque; baseline lossy carries no alpha).
pub(crate) fn to_image(planes: &Planes, width: usize, height: usize) -> Result<Image> {
    let y0 = planes.y_stride + 1;
    let uv0 = planes.uv_stride + 1;
    let rgba = yuv::yuv420_to_rgba(
        &yuv::Yuv420Ref {
            y: &planes.y[y0..],
            y_stride: planes.y_stride,
            u: &planes.u[uv0..],
            v: &planes.v[uv0..],
            uv_stride: planes.uv_stride,
        },
        width,
        height,
    );
    let dims = Dimensions::new(
        u32::try_from(width).unwrap_or(0),
        u32::try_from(height).unwrap_or(0),
    )
    .map_err(|_| Error::InvalidBitstream {
        codec: Codec::Lossy,
    })?;
    Ok(Image::from_parts(
        dims,
        PixelLayout::Rgba8,
        rgba,
        false,
        Metadata::none(),
    ))
}

#[cfg(test)]
mod tests {
    #![expect(
        clippy::doc_markdown,
        clippy::inconsistent_struct_constructor,
        reason = "these KATs construct FInfo / header literals and cite informal \
                  RFC 6386 / libwebp identifiers in comments; field order and \
                  bare names mirror the reference for readability"
    )]

    use super::{Planes, compute_fstrengths, fill_top_right_lane, filter_frame, resolve_finfo};
    use crate::lossy::decode::{FilterHeader, MbData, SegmentHeader};
    use crate::lossy::loop_filter::FInfo;

    // -------------------------------------------------------------------------
    // Helpers

    /// Build a `SegmentHeader` with only the fields `compute_fstrengths` reads.
    fn segment(
        use_segment: bool,
        absolute_delta: bool,
        filter_strength: [i32; 4],
    ) -> SegmentHeader {
        SegmentHeader {
            use_segment,
            update_map: false,
            absolute_delta,
            quantizer: [0; 4],
            filter_strength,
        }
    }

    /// Build an `MbData` carrying only the fields `resolve_finfo` reads.
    fn mb(segment: u8, is_i4x4: bool, skip: bool, non_zero_y: u32, non_zero_uv: u32) -> MbData {
        MbData {
            segment,
            is_i4x4,
            skip,
            non_zero_y,
            non_zero_uv,
            ..MbData::default()
        }
    }

    /// Compare an `FInfo` field-by-field (`FInfo` has no `PartialEq`).
    fn assert_finfo(info: FInfo, f_limit: i32, f_ilevel: i32, f_inner: bool, hev_thresh: i32) {
        assert_eq!(info.f_limit, f_limit, "f_limit");
        assert_eq!(info.f_ilevel, f_ilevel, "f_ilevel");
        assert_eq!(info.f_inner, f_inner, "f_inner");
        assert_eq!(info.hev_thresh, hev_thresh, "hev_thresh");
    }

    // =========================================================================
    // compute_fstrengths — port of libwebp PrecomputeFilterStrengths
    // (dec/frame_dec.c:276). For each [segment][i4x4]:
    //   base   = use_segment ? filter_strength[s] (+level if !absolute_delta)
    //                        : level
    //   level  = base (+ref_lf_delta[0], +mode_lf_delta[0] if i4x4) when use_lf_delta
    //   level  = clamp(level, 0, 63); level==0 => f_limit 0 (no filtering)
    //   ilevel = level >> (sharpness>4 ? 2 : 1) when sharpness>0, capped at 9-sharpness,
    //            floored at 1; ilevel = level when sharpness==0
    //   f_limit = 2*level + ilevel ; f_ilevel = ilevel
    //   hev_thresh = level>=40 ? 2 : level>=15 ? 1 : 0 ; f_inner = i4x4
    // =========================================================================

    #[test]
    fn fstrengths_plain_level_no_sharpness_no_deltas() {
        // level=20, sharpness=0, no lf-delta, no segments.
        // ilevel = 20 (sharpness 0) => f_ilevel 20, f_limit = 2*20+20 = 60,
        // hev_thresh: 20>=15 (not >=40) => 1. f_inner = i4x4.
        let seg = segment(false, false, [0; 4]);
        let filter = FilterHeader {
            level: 20,
            ..Default::default()
        };
        let table = compute_fstrengths(&seg, &filter);
        for lane in &table {
            assert_finfo(lane[0], 60, 20, false, 1); // 16x16 lane: f_inner=i4x4=false
            assert_finfo(lane[1], 60, 20, true, 1); // B_PRED lane: f_inner=i4x4=true
        }
    }

    #[test]
    fn fstrengths_segment_absolute_uses_filter_strength_directly() {
        // absolute_delta=true => base = filter_strength[s]; filter.level is IGNORED.
        // filter.level=10 must NOT be added. Distinct per-segment strengths verify
        // the [segment] index. sharpness=0 so f_ilevel == level.
        //   s0: level 7  -> f_limit 21,  f_ilevel 7,  hev 0  (7<15)
        //   s1: level 20 -> f_limit 60,  f_ilevel 20, hev 1  (20>=15)
        //   s2: level 40 -> f_limit 120, f_ilevel 40, hev 2  (40>=40)
        //   s3: level 3  -> f_limit 9,   f_ilevel 3,  hev 0
        let seg = segment(true, true, [7, 20, 40, 3]);
        let filter = FilterHeader {
            level: 10,
            ..Default::default()
        };
        let table = compute_fstrengths(&seg, &filter);
        let expect = [(21, 7, 0), (60, 20, 1), (120, 40, 2), (9, 3, 0)];
        for (s, &(fl, il, hev)) in expect.iter().enumerate() {
            assert_finfo(table[s][0], fl, il, false, hev);
            assert_finfo(table[s][1], fl, il, true, hev);
        }
    }

    #[test]
    fn fstrengths_segment_delta_adds_base_level() {
        // absolute_delta=false => base = filter_strength[s] + filter.level(=10).
        //   s0: 7+10=17  -> f_limit 51,  f_ilevel 17, hev 1
        //   s1: 20+10=30 -> f_limit 90,  f_ilevel 30, hev 1
        //   s2: 40+10=50 -> f_limit 150, f_ilevel 50, hev 2
        //   s3: 3+10=13  -> f_limit 39,  f_ilevel 13, hev 0  (13<15)
        let seg = segment(true, false, [7, 20, 40, 3]);
        let filter = FilterHeader {
            level: 10,
            ..Default::default()
        };
        let table = compute_fstrengths(&seg, &filter);
        let expect = [(51, 17, 1), (90, 30, 1), (150, 50, 2), (39, 13, 0)];
        for (s, &(fl, il, hev)) in expect.iter().enumerate() {
            assert_finfo(table[s][0], fl, il, false, hev);
            assert_finfo(table[s][1], fl, il, true, hev);
        }
    }

    #[test]
    fn fstrengths_lf_delta_adds_ref_then_mode_on_i4x4() {
        // use_lf_delta: +ref_lf_delta[0] always, +mode_lf_delta[0] only on the
        // i4x4 (B_PRED) lane. level=20, ref=5, mode=8, sharpness=0.
        //   16x16 lane: 20+5      = 25 -> f_limit 75, f_ilevel 25, hev 1
        //   B_PRED lane: 20+5+8   = 33 -> f_limit 99, f_ilevel 33, hev 1
        let seg = segment(false, false, [0; 4]);
        let filter = FilterHeader {
            level: 20,
            use_lf_delta: true,
            ref_lf_delta: [5, 0, 0, 0],
            mode_lf_delta: [8, 0, 0, 0],
            ..Default::default()
        };
        let table = compute_fstrengths(&seg, &filter);
        for lane in &table {
            assert_finfo(lane[0], 75, 25, false, 1);
            assert_finfo(lane[1], 99, 33, true, 1);
        }
    }

    #[test]
    fn fstrengths_sharpness_shifts_and_caps_inner_level() {
        // With use_segment=false and no lf-delta, level == filter.level, so each
        // call probes one (level, sharpness) point via the 16x16 lane [0][0].
        let inner = |level: i32, sharpness: i32| {
            let seg = segment(false, false, [0; 4]);
            let filter = FilterHeader {
                level,
                sharpness,
                ..Default::default()
            };
            compute_fstrengths(&seg, &filter)[0][0]
        };
        // sharpness 1 (<=4 => >>1): ilevel = 40>>1 = 20, cap = 9-1 = 8 => 8.
        //   f_limit = 2*40+8 = 88, hev 2.
        assert_finfo(inner(40, 1), 88, 8, false, 2);
        // sharpness 5 (>4 => >>2): ilevel = 40>>2 = 10, cap = 9-5 = 4 => 4.
        //   f_limit = 2*40+4 = 84, hev 2.
        assert_finfo(inner(40, 5), 84, 4, false, 2);
        // sharpness 3 (>>1) NO cap: ilevel = 6>>1 = 3, cap = 9-3 = 6, 3<=6 keeps 3.
        //   f_limit = 2*6+3 = 15, hev 0 (6<15).
        assert_finfo(inner(6, 3), 15, 3, false, 0);
        // floor: ilevel = 1>>1 = 0, cap 8, then floored to 1.
        //   f_limit = 2*1+1 = 3, hev 0.
        assert_finfo(inner(1, 1), 3, 1, false, 0);
        // sharpness 7 (>>2) hits the tightest cap: ilevel = 63>>2 = 15, cap = 2 => 2.
        //   f_limit = 2*63+2 = 128, hev 2.
        assert_finfo(inner(63, 7), 128, 2, false, 2);
    }

    #[test]
    fn fstrengths_level_clamped_to_zero_disables_filtering() {
        // ref_lf_delta[0] = -10 drives the 16x16 lane's level negative -> clamp 0
        // -> FInfo::default (f_limit 0). mode_lf_delta[0] = +20 lifts the B_PRED
        // lane back positive (3-10+20 = 13), proving the two lanes diverge and
        // that a zeroed level yields the all-zero disabled FInfo.
        let seg = segment(false, false, [0; 4]);
        let filter = FilterHeader {
            level: 3,
            use_lf_delta: true,
            ref_lf_delta: [-10, 0, 0, 0],
            mode_lf_delta: [20, 0, 0, 0],
            ..Default::default()
        };
        let table = compute_fstrengths(&seg, &filter);
        for lane in &table {
            // 16x16 lane: level 3-10 = -7 -> 0 -> disabled (all zero).
            assert_finfo(lane[0], 0, 0, false, 0);
            // B_PRED lane: level 3-10+20 = 13 -> f_limit 39, f_ilevel 13, hev 0.
            assert_finfo(lane[1], 39, 13, true, 0);
        }
    }

    // =========================================================================
    // resolve_finfo — port of vp8_dec.c:642-643:
    //   finfo = fstrengths[block.segment][block.is_i4x4]
    //   finfo.f_inner |= !skip
    // where skip = (use_skip && block.skip) || (non_zero_y | non_zero_uv == 0).
    // f_inner therefore = is_i4x4 || !skip; everything else passes through the
    // [segment][is_i4x4]-indexed table entry unchanged.
    // =========================================================================

    /// Table whose `f_limit` encodes its indices as `100 + 10*segment + i4x4`, so
    /// a swapped or wrong index is observable. `f_ilevel`/`hev_thresh` are fixed
    /// witnesses that resolve_finfo passes them through untouched.
    fn indexed_table() -> [[FInfo; 2]; 4] {
        let mut table = [[FInfo::default(); 2]; 4];
        for (s, lane) in table.iter_mut().enumerate() {
            for (i, cell) in lane.iter_mut().enumerate() {
                *cell = FInfo {
                    f_limit: 100 + 10 * i32::try_from(s).unwrap() + i32::try_from(i).unwrap(),
                    f_ilevel: 7,
                    f_inner: false,
                    hev_thresh: 1,
                };
            }
        }
        table
    }

    #[test]
    fn resolve_finfo_residual_present_sets_inner_and_indexes_segment() {
        // segment 2, 16x16, use_skip on but block.skip=false, luma residual present
        // (non_zero_y != 0) => skip=false => f_inner = false || !false = true.
        // Picks table[2][0] => f_limit 120; f_ilevel/hev pass through.
        let table = indexed_table();
        let block = mb(2, false, false, 0x5, 0);
        let info = resolve_finfo(table, &block, true);
        assert_finfo(info, 120, 7, true, 1);
    }

    #[test]
    fn resolve_finfo_explicit_skip_clears_inner() {
        // segment 1, 16x16, use_skip on, block.skip=true, no residual => skip=true
        // => f_inner = false || !true = false. Picks table[1][0] => f_limit 110.
        let table = indexed_table();
        let block = mb(1, false, true, 0, 0);
        let info = resolve_finfo(table, &block, true);
        assert_finfo(info, 110, 7, false, 1);
    }

    #[test]
    fn resolve_finfo_i4x4_forces_inner_even_when_skipped() {
        // segment 3, i4x4, block.skip=true, no residual => skip=true, yet the
        // i4x4 disjunct keeps f_inner = true. Also proves the is_i4x4=1 lane is
        // selected: table[3][1] => f_limit 131 (not 130).
        let table = indexed_table();
        let block = mb(3, true, true, 0, 0);
        let info = resolve_finfo(table, &block, true);
        assert_finfo(info, 131, 7, true, 1);
    }

    #[test]
    fn resolve_finfo_use_skip_false_ignores_block_skip_but_checks_residual() {
        // use_skip=false makes block.skip irrelevant: skip starts false, then the
        // residual test runs. With no residual (nz all zero) skip becomes true =>
        // f_inner = false. table[0][0] => f_limit 100.
        let table = indexed_table();
        let block = mb(0, false, true, 0, 0);
        let info = resolve_finfo(table, &block, false);
        assert_finfo(info, 100, 7, false, 1);
    }

    #[test]
    fn resolve_finfo_chroma_residual_alone_sets_inner() {
        // use_skip=false, block.skip=true (ignored). Only chroma is non-zero
        // (non_zero_uv != 0) => the (nz_y | nz_uv)==0 test fails => skip=false =>
        // f_inner true. Confirms non_zero_uv participates in the OR. table[1][0].
        let table = indexed_table();
        let block = mb(1, false, true, 0, 0x2);
        let info = resolve_finfo(table, &block, false);
        assert_finfo(info, 110, 7, true, 1);
    }

    #[test]
    fn resolve_finfo_i4x4_no_residual_indexes_i4x4_lane() {
        // segment 2, i4x4, use_skip on, block.skip=false, no residual => skip=true
        // but is_i4x4 keeps f_inner=true. Selects table[2][1] => f_limit 121,
        // distinguishing the i4x4 lane (121) from the 16x16 lane (120).
        let table = indexed_table();
        let block = mb(2, true, false, 0, 0);
        let info = resolve_finfo(table, &block, true);
        assert_finfo(info, 121, 7, true, 1);
    }

    // =========================================================================
    // filter_frame — dispatch + per-MB geometry over Planes.
    // filter_type 0 => no-op; 1 => simple luma filter; f_limit==0 => per-MB no-op.
    // =========================================================================

    /// Two macroblocks side by side (mb_w=2, mb_h=1). Interior rows carry a
    /// vertical step 128|134 whose boundary sits exactly on mb_x=1's left edge
    /// (plane column 17 = mb_x*16+1). y_stride = 1 + 2*16 + 4 = 37.
    fn stepped_left_edge_planes() -> Planes {
        let mut planes = Planes::new(2, 1);
        let stride = planes.y_stride; // 37
        for row in 1..17 {
            for col in 1..stride {
                planes.y[row * stride + col] = if col < 17 { 128 } else { 134 };
            }
        }
        planes
    }

    #[test]
    fn filter_frame_simple_smooths_left_mb_edge() {
        // Simple filter on mb_x=1's left macroblock edge (do_left=true). DoFilter2
        // with thresh = f_limit+4 = 24 (thresh2 = 49 >= NeedsFilter's 4*6+6 = 30):
        //   a  = 3*(134-128) + sclip1(128-134) = 18 - 6 = 12
        //   a1 = sclip2((12+4)>>3) = 2 ; a2 = sclip2((12+3)>>3) = 1
        //   p0 (col16) = 128 + 1 = 129 ; q0 (col17) = 134 - 2 = 132
        // mb_x=0 does nothing (do_left/do_top false, f_inner false).
        let mut planes = stepped_left_edge_planes();
        let stride = planes.y_stride;
        let info = FInfo {
            f_limit: 20,
            f_ilevel: 9,
            f_inner: false,
            hev_thresh: 0,
        };
        let finfo = vec![info; 2];
        filter_frame(&mut planes, &finfo, 2, 1, 1);
        for r in 1..17 {
            assert_eq!(planes.y[r * stride + 15], 128, "p1 untouched, row {r}");
            assert_eq!(planes.y[r * stride + 16], 129, "p0 filtered, row {r}");
            assert_eq!(planes.y[r * stride + 17], 132, "q0 filtered, row {r}");
            assert_eq!(planes.y[r * stride + 18], 134, "q1 untouched, row {r}");
            // Samples away from the single filtered edge are untouched.
            assert_eq!(planes.y[r * stride + 5], 128, "mb_x=0 interior, row {r}");
            assert_eq!(planes.y[r * stride + 30], 134, "mb_x=1 interior, row {r}");
        }
    }

    #[test]
    fn filter_frame_simple_smooths_top_mb_edge() {
        // Single column of two macroblocks (mb_w=1, mb_h=2). A horizontal step
        // 128|134 straddles mb_y=1's top edge (plane row 17 = mb_y*16+1). This
        // exercises the y_off row arithmetic and do_top. Same DoFilter2 kernel:
        //   p0 (row16) = 129 ; q0 (row17) = 132. y_stride = 1 + 16 + 4 = 21.
        let mut planes = Planes::new(1, 2);
        let stride = planes.y_stride; // 21
        for row in 1..33 {
            for col in 1..stride {
                planes.y[row * stride + col] = if row < 17 { 128 } else { 134 };
            }
        }
        let info = FInfo {
            f_limit: 20,
            f_ilevel: 9,
            f_inner: false,
            hev_thresh: 0,
        };
        let finfo = vec![info; 2];
        filter_frame(&mut planes, &finfo, 1, 2, 1);
        for c in 1..17 {
            assert_eq!(planes.y[15 * stride + c], 128, "p1 untouched, col {c}");
            assert_eq!(planes.y[16 * stride + c], 129, "p0 filtered, col {c}");
            assert_eq!(planes.y[17 * stride + c], 132, "q0 filtered, col {c}");
            assert_eq!(planes.y[18 * stride + c], 134, "q1 untouched, col {c}");
        }
    }

    #[test]
    fn filter_frame_type0_leaves_plane_untouched() {
        // filter_type 0 returns immediately — the identical stepped plane is
        // byte-for-byte unchanged despite an edge that type 1 would smooth.
        let mut planes = stepped_left_edge_planes();
        let before = planes.y.clone();
        let info = FInfo {
            f_limit: 20,
            f_ilevel: 9,
            f_inner: false,
            hev_thresh: 0,
        };
        let finfo = vec![info; 2];
        filter_frame(&mut planes, &finfo, 2, 1, 0);
        assert_eq!(planes.y, before, "filter_type 0 must be a no-op");
    }

    #[test]
    fn filter_frame_zero_flimit_is_a_per_mb_noop() {
        // filter_type 1 but every FInfo has f_limit 0: filter_mb_simple's guard
        // fires per macroblock, so the step across mb_x=1's edge survives intact.
        let mut planes = stepped_left_edge_planes();
        let before = planes.y.clone();
        let finfo = vec![FInfo::default(); 2]; // f_limit == 0
        filter_frame(&mut planes, &finfo, 2, 1, 1);
        assert_eq!(planes.y, before, "f_limit==0 must disable filtering");
    }

    // =========================================================================
    // resolve_finfo — residual detection uses OR, not XOR.
    // =========================================================================

    #[test]
    fn resolve_finfo_equal_nonzero_luma_and_chroma_uses_or_not_xor() {
        // non_zero_y == non_zero_uv (both 0x5): their OR is non-zero, so the block
        // carries residual => skip=false => f_inner = false || !false = true. A
        // `|`->`^` mutation would XOR the two equal masks to zero, wrongly declaring
        // the block skippable (skip=true => f_inner=false). 16x16 lane so f_inner
        // mirrors !skip directly; table[0][0] passes through f_limit 100.
        let table = indexed_table();
        let block = mb(0, false, false, 0x5, 0x5);
        let info = resolve_finfo(table, &block, true);
        assert_finfo(info, 100, 7, true, 1);
    }

    // =========================================================================
    // strength_for — the inner-level shift selector branches on `>`, not `==`/`>=`.
    // =========================================================================

    #[test]
    fn fstrengths_sharpness_shift_selector_branches_strictly_above_four() {
        // The shift `inner >>= sharpness > 4 ? 2 : 1` must use a strict `>`. At the
        // boundary sharpness == 4 the true branch ( `> 4` false => >>1 ) diverges
        // from both `== 4` and `>= 4` ( which take >>2 ), and with level=12 the shift
        // result stays under the cap so the difference is observable:
        //   >>1 => 6, cap = 9-4 = 5, capped to 5 => f_ilevel 5, f_limit 2*12+5 = 29.
        // (A >>2 mutant would give inner 3 => f_ilevel 3, f_limit 27.) hev: 12<15 => 0.
        let seg = segment(false, false, [0; 4]);
        let filter = FilterHeader {
            level: 12,
            sharpness: 4,
            ..Default::default()
        };
        let info = compute_fstrengths(&seg, &filter)[0][0];
        assert_finfo(info, 29, 5, false, 0);
    }

    // =========================================================================
    // fill_top_right_lane — port of ReconstructRow's intra-4x4 top-right lane setup.
    // Reads the 4 samples one row above `y_off` at column offset +16 (`tr`) and
    // replicates them down to rows 3, 7, 11 (same +16 offset). On the rightmost
    // column with a top neighbor it first fills `tr..tr+4` with the MB's own last
    // top sample (`y_off - stride + 15`).
    // =========================================================================

    #[test]
    fn fill_top_right_lane_replicates_above_right_samples() {
        // has_top=true, is_rightmost=false: the above-right MB's bottom-left samples
        // already sit at tr..tr+4, so the lane is read verbatim and copied to rows
        // 3/7/11. Distinct values at tr-3..tr+3 make any index-arithmetic slip in the
        // `[y[tr], y[tr+1], y[tr+2], y[tr+3]]` reads observable. mb at (0,1) in a
        // 2x2-MB plane: y_stride 37, y_off 630, tr = 630-37+16 = 609.
        let mut planes = Planes::new(2, 2);
        let stride = planes.y_stride; // 37
        let y_off = 630usize;
        let tr = y_off - stride + 16; // 609
        for (i, v) in [90u8, 91, 92, 100, 101, 102, 103].into_iter().enumerate() {
            planes.y[tr - 3 + i] = v; // tr-3..=tr+3
        }
        fill_top_right_lane(&mut planes.y, y_off, stride, true, false);
        let lane = [100u8, 101, 102, 103]; // y[tr..tr+4]
        for r in [3usize, 7, 11] {
            let dst = y_off + r * stride + 16;
            assert_eq!(&planes.y[dst..dst + 4], &lane, "row {r} replica");
        }
    }

    #[test]
    fn fill_top_right_lane_rightmost_fills_from_own_last_top_sample() {
        // has_top=true, is_rightmost=true: no above-right neighbor, so tr..tr+4 must
        // be filled with the MB's own last top sample at `y_off - stride + 15` before
        // replication. Seed that source (index 608) with 150 and tr..tr+4 with other
        // values; correct code overwrites the lane to [150;4] and replicates it.
        // Deleting the `!` on `if !has_top` skips the fill (lane stays seeded); a `+`
        // or `-` slip in the source index reads a 0/border byte instead of 150.
        let mut planes = Planes::new(2, 2);
        let stride = planes.y_stride; // 37
        let y_off = 630usize;
        let tr = y_off - stride + 16; // 609
        planes.y[y_off - stride + 15] = 150; // the unique last-top-sample source
        for (i, v) in [10u8, 11, 12, 13].into_iter().enumerate() {
            planes.y[tr + i] = v; // pre-seed the lane with non-150 values
        }
        fill_top_right_lane(&mut planes.y, y_off, stride, true, true);
        assert_eq!(
            &planes.y[tr..tr + 4],
            &[150u8; 4],
            "lane filled from last top"
        );
        for r in [3usize, 7, 11] {
            let dst = y_off + r * stride + 16;
            assert_eq!(&planes.y[dst..dst + 4], &[150u8; 4], "row {r} replica");
        }
    }

    // =========================================================================
    // crop / crop_{y,u,v} (oracle Level-A) — pack the real w×h top-left region.
    // =========================================================================

    #[cfg(feature = "oracle")]
    #[test]
    fn crop_extracts_exact_top_left_region_from_each_plane() {
        // crop reads the real region starting at [stride + 1] (past the 1-px top/left
        // borders), row by row: row y at `(y+1)*stride + 1 .. +w`. Distinct per-cell
        // bytes make any index arithmetic (stride multiply/add, row/col offset, slice
        // end) or a stubbed body (vec![]/vec![0]/vec![1]) observable as a byte
        // mismatch. Different values per plane pin the y/u/v selection.
        let mut planes = Planes::new(1, 1);
        let ys = planes.y_stride; // 21
        let us = planes.uv_stride; // 9
        // Luma: w=3, h=2 => rows 1,2 at cols 1..4.
        for (r, base) in [10u8, 20].into_iter().enumerate() {
            let row = (r + 1) * ys;
            planes.y[row + 1] = base;
            planes.y[row + 2] = base + 1;
            planes.y[row + 3] = base + 2;
        }
        assert_eq!(planes.crop_y(3, 2), vec![10, 11, 12, 20, 21, 22]);
        // U and V share uv_stride; distinct values distinguish the two planes.
        for (r, base) in [30u8, 40].into_iter().enumerate() {
            let row = (r + 1) * us;
            planes.u[row + 1] = base;
            planes.u[row + 2] = base + 1;
        }
        assert_eq!(planes.crop_u(2, 2), vec![30, 31, 40, 41]);
        for (r, base) in [50u8, 60].into_iter().enumerate() {
            let row = (r + 1) * us;
            planes.v[row + 1] = base;
            planes.v[row + 2] = base + 1;
        }
        assert_eq!(planes.crop_v(2, 2), vec![50, 51, 60, 61]);
    }
}
