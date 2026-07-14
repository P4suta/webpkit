//! Spatial predictor transform — `inverse` ports libwebp
//! `PredictorInverseTransform_C`; `forward` is its exact encode-side mirror.
//!
//! Every pixel is reconstructed as the per-lane wrapping sum of its transmitted
//! residual and a prediction derived from already-decoded neighbors (left, top,
//! top-left, top-right). One of 14 prediction modes is selected per tile and
//! stored in the GREEN byte of the `tile_data` sub-image; tiles are `1 << bits`
//! pixels square. The border rules (first row, first column) mirror libwebp
//! exactly so decode stays bit-identical. `forward` runs the same prediction
//! against the ORIGINAL neighbors, emitting residuals into a separate buffer, so
//! that `inverse` inverts it losslessly.

use crate::lossless::constants::{ARGB_BLACK, subsample_size};
use crate::lossless::histogram::shannon_bits;
use crate::lossless::prelude::*;
use crate::lossless::transform::{add_pixels, sub_pixels};
use crate::lossless::work::work;

/// Spatial predictor inverse: reconstruct each pixel from its residual plus a
/// neighbor-based prediction.
pub(crate) fn inverse(argb: &mut [u32], width: u32, bits: u32, tile_data: &[u32]) {
    if width == 0 || argb.is_empty() {
        return;
    }
    let tiles_per_row = subsample_size(width, bits) as usize;
    let width = width as usize;
    let height = argb.len() / width;
    reconstruct_first_row(argb, width);
    for y in 1..height {
        reconstruct_row(argb, width, bits, tile_data, tiles_per_row, y);
    }
}

/// Reconstruct row 0: the origin seeds from opaque black (mode 0), every other
/// pixel predicts from its left neighbor (mode 1).
fn reconstruct_first_row(argb: &mut [u32], width: usize) {
    argb[0] = add_pixels(argb[0], ARGB_BLACK);
    for x in 1..width {
        argb[x] = add_pixels(argb[x], argb[x - 1]);
    }
}

/// Reconstruct one interior row `y`: column 0 predicts from the pixel above
/// (mode 2); each remaining pixel uses the mode of its covering tile.
fn reconstruct_row(
    argb: &mut [u32],
    width: usize,
    bits: u32,
    tile_data: &[u32],
    tiles_per_row: usize,
    y: usize,
) {
    let row = y * width;
    argb[row] = add_pixels(argb[row], argb[row - width]);
    let tile_row = (y >> bits) * tiles_per_row;
    for x in 1..width {
        let i = row + x;
        let mode = (tile_data[tile_row + (x >> bits)] >> 8) & 0x0f;
        // All four neighbors sit at indices < i; copy them out (u32 is Copy) so
        // the write to argb[i] never aliases an outstanding read. At the last
        // column, top_right naturally reads this row's already-rebuilt column 0.
        let left = argb[i - 1];
        let top = argb[i - width];
        let top_left = argb[i - width - 1];
        let top_right = argb[i - width + 1];
        argb[i] = add_pixels(argb[i], predict(mode, left, top_left, top, top_right));
    }
}

/// Reconstruct one output row into `out` from its `residual` row and the
/// previous output row `prev_out` — the row-streaming counterpart of the
/// whole-buffer [`inverse`].
///
/// Because the predictor reads its OWN stage's previous-row output (top /
/// top-left / top-right) and within-row left, it is the only inverse stage with
/// a cross-row dependency; a streaming pipeline therefore feeds it one `prev_out`
/// row. `out` is a SEPARATE buffer from `residual` (it must be — the whole-buffer
/// [`inverse`] reconstructs in place, but here `prev_out` is the *reconstructed*
/// previous row, not the buffer being overwritten). Row 0 uses the first-row rule
/// of [`reconstruct_first_row`] (origin against `ARGB_BLACK`, then left);
/// interior rows mirror [`reconstruct_row`] exactly, including the last column's
/// `top_right` wrap to this row's already-rebuilt column 0 (`out[0]`, matching
/// `argb[i - width + 1]`). Feeding every residual row through this in order, with
/// `prev_out` set to the row just reconstructed, reproduces [`inverse`]
/// byte-for-byte (proven in tests).
///
/// Consumed by the streaming decoder's `InverseChain` ([`crate::lossless::vp8l::decode_incr`]).
pub(crate) fn reconstruct_row_into(
    out: &mut [u32],
    residual: &[u32],
    prev_out: &[u32],
    y: usize,
    bits: u32,
    tile_data: &[u32],
) {
    let width = out.len();
    if width == 0 {
        return;
    }
    if y == 0 {
        // Row 0: origin against opaque black (mode 0), then left (mode 1).
        out[0] = add_pixels(residual[0], ARGB_BLACK);
        for x in 1..width {
            out[x] = add_pixels(residual[x], out[x - 1]);
        }
        return;
    }
    // Interior row: column 0 predicts from the pixel above (mode 2); the rest use
    // their covering tile's mode, reading top/top-left/top-right from `prev_out`
    // and left from this row's already-written `out`.
    let tiles_per_row = subsample_size(u32::try_from(width).unwrap_or(0), bits) as usize;
    out[0] = add_pixels(residual[0], prev_out[0]);
    let tile_row = (y >> bits) * tiles_per_row;
    for x in 1..width {
        let mode = (tile_data[tile_row + (x >> bits)] >> 8) & 0x0f;
        let left = out[x - 1];
        let top = prev_out[x];
        let top_left = prev_out[x - 1];
        // Last column: top_right wraps to this row's column 0, matching the
        // whole-buffer inverse's `argb[i - width + 1]`.
        let top_right = if x + 1 < width {
            prev_out[x + 1]
        } else {
            out[0]
        };
        out[x] = add_pixels(residual[x], predict(mode, left, top_left, top, top_right));
    }
}

/// Spatial predictor forward: the exact mirror of [`inverse`]. Each tile's mode
/// is chosen to minimize the residual's coding cost, then every pixel's
/// `value ⊖ prediction` is written to a SEPARATE `residual` buffer — forward
/// reads the ORIGINAL (un-residualized) neighbors, so unlike subtract-green it
/// cannot work in place. The border rules (origin seeds from `ARGB_BLACK`, row 0
/// predicts left, interior column 0 predicts top) and the last-column
/// `top_right` wrap mirror [`reconstruct_row`] byte-for-byte, so `inverse`
/// inverts `forward` exactly.
///
/// `tile_data` carries one pixel per tile with the mode packed as
/// `(mode & 0x0f) << 8`, exactly where [`reconstruct_row`] reads it back.
///
/// Reused per-`forward` scratch for the predictor's per-tile entropy scoring: a
/// residual-byte histogram per channel (in `residual.to_le_bytes()` order —
/// blue, green, red, alpha), each paired with a touched-bin list so a mode's
/// accumulate + Shannon-finalize + reset is O(interior pixels), never a 256-bin
/// scan, plus a shared `used_counts` staging buffer. Allocated once in
/// [`forward`] and threaded through every `best_mode` call so mode scoring does
/// zero per-mode/per-tile heap work — every buffer is cleared and reused.
struct EntropyScratch {
    counts: [[u32; 256]; 4],
    touched: [Vec<u8>; 4],
    used_counts: Vec<u32>,
    /// The interior pixels of the tile currently being scored, each as
    /// `(value, left, top_left, top, top_right)`. Gathered once per tile so all
    /// 14 modes reuse the same neighbor loads instead of re-reading `argb`.
    neighbors: Vec<(u32, u32, u32, u32, u32)>,
}

impl EntropyScratch {
    fn new() -> Self {
        Self {
            counts: [[0u32; 256]; 4],
            touched: core::array::from_fn(|_| Vec::new()),
            used_counts: Vec::new(),
            neighbors: Vec::new(),
        }
    }
}

/// Pick the min-cost mode for the tile at `(tx, ty)`, scoring only the
/// interior pixels (`x >= 1 && y >= 1`) it actually governs — border pixels
/// use fixed modes 0/1/2 and are excluded, exactly as [`reconstruct_row`]
/// decodes them.
///
/// Cost is the ENTROPY of the mode's residual over the tile interior:
/// `Σ_channel shannon_bits(total, used_counts)`, where `total` is the interior
/// pixel count (constant across modes) and `used_counts` are the residual-byte
/// histogram's nonzero bins. This rewards residuals that reduce to FEW distinct
/// symbols — a constant residual costs exactly 0 bits (a single symbol), so a
/// flat / periodic tile picks the LZ77-friendly mode instead of the one with
/// the tightest magnitude. The magnitude proxy this replaced misjudged those,
/// picking clip255-spiky gradient modes that break row-to-row back-references.
///
/// There is NO per-tile header term: a constant channel must score exactly 0
/// so flat tiles tie. Ties resolve to the lowest mode index (strict `<` keeps
/// the first, ascending, winner), so a 0-entropy tile resolves to mode 1
/// (mode 0 predicts opaque black, never constant on a non-black tile).
///
/// `scratch` is reused across every call so this allocates nothing; each mode
/// only touches, then resets, the histogram bins it actually hit. The four
/// neighbors are the same for every mode at a given pixel, so they are gathered
/// once into `scratch.neighbors` and reused across all 14 modes.
fn best_mode(
    argb: &[u32],
    width: usize,
    height: usize,
    bits: u32,
    tx: usize,
    ty: usize,
    scratch: &mut EntropyScratch,
) -> u32 {
    let tile = 1usize << bits;
    let x0 = (tx << bits).max(1);
    let y0 = (ty << bits).max(1);
    let x1 = ((tx << bits) + tile).min(width);
    let y1 = ((ty << bits) + tile).min(height);
    let total = (y1.saturating_sub(y0) * x1.saturating_sub(x0)) as u64;
    // Gather each interior pixel's value and its four neighbors once. They are
    // identical across all 14 modes, so this replaces 14 re-reads of `argb` per
    // pixel with a single pass — the scored order (row-major over the interior)
    // is unchanged, so the histograms and argmin are byte-for-byte identical.
    scratch.neighbors.clear();
    for y in y0..y1 {
        let row = y * width;
        for x in x0..x1 {
            let i = row + x;
            scratch.neighbors.push((
                argb[i],
                argb[i - 1],
                argb[i - width - 1],
                argb[i - width],
                argb[i - width + 1],
            ));
        }
    }
    let mut best = 0u32;
    let mut best_cost = u64::MAX;
    for mode in 0..=13u32 {
        work!(PredictorModeEval);
        // Accumulate the per-channel residual-byte histogram over the interior,
        // recording each first-touched bin so the reset stays O(used).
        for &(cur, left, top_left, top, top_right) in &scratch.neighbors {
            let residual = sub_pixels(cur, predict(mode, left, top_left, top, top_right));
            for (c, &byte) in residual.to_le_bytes().iter().enumerate() {
                let idx = usize::from(byte);
                if scratch.counts[c][idx] == 0 {
                    scratch.touched[c].push(byte);
                }
                scratch.counts[c][idx] += 1;
            }
        }
        // Finalize the Shannon cost per channel, then reset only touched bins.
        let mut cost = 0u64;
        for c in 0..4 {
            scratch.used_counts.clear();
            let n = scratch.touched[c].len();
            for k in 0..n {
                let byte = scratch.touched[c][k];
                scratch
                    .used_counts
                    .push(scratch.counts[c][usize::from(byte)]);
            }
            cost += shannon_bits(total, &scratch.used_counts);
            for k in 0..n {
                let byte = scratch.touched[c][k];
                scratch.counts[c][usize::from(byte)] = 0;
            }
            scratch.touched[c].clear();
        }
        if cost < best_cost {
            best_cost = cost;
            best = mode;
        }
    }
    best
}

#[must_use]
pub(crate) fn forward(argb: &[u32], width: u32, height: u32, bits: u32) -> (Vec<u32>, Vec<u32>) {
    let tiles_per_row = subsample_size(width, bits) as usize;
    let tiles_per_col = subsample_size(height, bits) as usize;
    let mut tile_data = vec![0u32; tiles_per_row * tiles_per_col];
    let mut residual = vec![0u32; argb.len()];
    if width == 0 || argb.is_empty() {
        return (residual, tile_data);
    }
    let width = width as usize;
    let height = height as usize;

    // Select every tile's mode first: residual formation below re-reads it from
    // `tile_data` through the same index math the decoder uses. One scratch,
    // reused across every tile, keeps mode scoring allocation-free.
    let mut scratch = EntropyScratch::new();
    for ty in 0..tiles_per_col {
        for tx in 0..tiles_per_row {
            let mode = best_mode(argb, width, height, bits, tx, ty, &mut scratch);
            tile_data[ty * tiles_per_row + tx] = (mode & 0x0f) << 8;
        }
    }

    // Row 0: origin against black (mode 0), then left (mode 1).
    residual[0] = sub_pixels(argb[0], ARGB_BLACK);
    for x in 1..width {
        residual[x] = sub_pixels(argb[x], argb[x - 1]);
    }
    // Interior rows: column 0 against top (mode 2), the rest against their tile's
    // predicted value, all neighbors read from the ORIGINAL `argb`.
    for y in 1..height {
        let row = y * width;
        residual[row] = sub_pixels(argb[row], argb[row - width]);
        let tile_row = (y >> bits) * tiles_per_row;
        for x in 1..width {
            let i = row + x;
            let mode = (tile_data[tile_row + (x >> bits)] >> 8) & 0x0f;
            let left = argb[i - 1];
            let top = argb[i - width];
            let top_left = argb[i - width - 1];
            let top_right = argb[i - width + 1];
            residual[i] = sub_pixels(argb[i], predict(mode, left, top_left, top, top_right));
        }
    }
    (residual, tile_data)
}

/// Dispatch a prediction mode to its neighbor combination. Modes 0, 14 and 15
/// fall back to opaque black, matching libwebp's default arm.
#[must_use]
fn predict(mode: u32, left: u32, tl: u32, t: u32, tr: u32) -> u32 {
    match mode {
        1 => left,
        2 => t,
        3 => tr,
        4 => tl,
        5 => average3(left, t, tr),
        6 => average2(left, tl),
        7 => average2(left, t),
        8 => average2(tl, t),
        9 => average2(t, tr),
        10 => average4(left, tl, t, tr),
        11 => select(t, left, tl),
        12 => clamped_add_subtract_full(left, t, tl),
        13 => clamped_add_subtract_half(left, t, tl),
        _ => ARGB_BLACK,
    }
}

/// Per-lane floor average of two ARGB pixels (libwebp `Average2`). The
/// `0xfefe_fefe` mask clears each lane's low bit before the shift so no carry
/// crosses a byte boundary.
#[must_use]
const fn average2(a: u32, b: u32) -> u32 {
    (((a ^ b) & 0xfefe_fefe) >> 1).wrapping_add(a & b)
}

/// Three-way average (libwebp `Average3`). `a` and `c` are averaged first; the
/// evaluation order is load-bearing for bit-exactness.
#[must_use]
const fn average3(a: u32, b: u32, c: u32) -> u32 {
    average2(average2(a, c), b)
}

/// Four-way average (libwebp `Average4`): average the two pairs, then average
/// the results.
#[must_use]
const fn average4(a: u32, b: u32, c: u32, d: u32) -> u32 {
    average2(average2(a, b), average2(c, d))
}

/// Clamp a signed channel intermediate to `0..=255`. Bit-identical to libwebp's
/// `Clip255` (`a < 256 ? a : ~a >> 24`) over the reachable domain `[-255, 510]`.
#[must_use]
const fn clip255(v: i32) -> u8 {
    if v < 0 {
        0
    } else if v > 255 {
        255
    } else {
        // Cast-free narrowing: for 0..=255 the low little-endian byte is the
        // value itself, and it trips no truncation/sign-loss lints.
        v.to_le_bytes()[0]
    }
}

/// Clamped full add-subtract of a single channel: `clip255(a + b - c)`.
#[must_use]
fn add_sub_full(a: i32, b: i32, c: i32) -> i32 {
    i32::from(clip255(a + b - c))
}

/// Clamped half add-subtract of a single channel: `clip255(a + (a - b) / 2)`.
/// The `/ 2` truncates toward zero — it is NOT an arithmetic `>> 1`.
#[must_use]
fn add_sub_half(a: i32, b: i32) -> i32 {
    i32::from(clip255(a + (a - b) / 2))
}

/// Gradient predictor (libwebp `ClampedAddSubtractFull`): per channel,
/// `clip255(c0 + c1 - c2)`.
#[must_use]
fn clamped_add_subtract_full(c0: u32, c1: u32, c2: u32) -> u32 {
    let a = c0.to_le_bytes();
    let b = c1.to_le_bytes();
    let c = c2.to_le_bytes();
    let out: [u8; 4] = core::array::from_fn(|k| {
        add_sub_full(i32::from(a[k]), i32::from(b[k]), i32::from(c[k])).to_le_bytes()[0]
    });
    u32::from_le_bytes(out)
}

/// Half-gradient predictor (libwebp `ClampedAddSubtractHalf`): let
/// `ave = average2(c0, c1)`; per channel `clip255(ave + (ave - c2) / 2)`.
#[must_use]
fn clamped_add_subtract_half(c0: u32, c1: u32, c2: u32) -> u32 {
    let ave = average2(c0, c1).to_le_bytes();
    let c = c2.to_le_bytes();
    let out: [u8; 4] =
        core::array::from_fn(|k| add_sub_half(i32::from(ave[k]), i32::from(c[k])).to_le_bytes()[0]);
    u32::from_le_bytes(out)
}

/// libwebp `Select`, called as `select(t, left, tl)` (`a = T`, `b = Left`,
/// `c = TL`): pick `a` when the accumulated gradient
/// `Σ(|b - c| - |a - c|) <= 0`, otherwise `b`.
#[must_use]
fn select(a: u32, b: u32, c: u32) -> u32 {
    let ba = a.to_le_bytes();
    let bb = b.to_le_bytes();
    let bc = c.to_le_bytes();
    let sum: i32 = ba
        .iter()
        .zip(&bb)
        .zip(&bc)
        .map(|((&av, &bv), &cv)| {
            let cc = i32::from(cv);
            (i32::from(bv) - cc).abs() - (i32::from(av) - cc).abs()
        })
        .sum();
    if sum <= 0 { a } else { b }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::cast_sign_loss,
        clippy::cast_possible_truncation,
        reason = "test fixtures build ARGB pixels from small in-range signed offsets; the \
                  casts fit their targets by construction"
    )]

    use super::{
        EntropyScratch, add_sub_full, add_sub_half, average2, average3, average4, best_mode,
        clamped_add_subtract_full, clamped_add_subtract_half, clip255, forward, inverse, predict,
        reconstruct_row_into, select, shannon_bits, sub_pixels,
    };
    use crate::lossless::constants::subsample_size;
    use proptest::prelude::*;

    /// Pre-hoist reference for [`best_mode`]: re-reads the four neighbors from
    /// `argb` inside every mode's loop (the shape before the neighbor gather was
    /// hoisted out). Kept only so the proptest can assert byte-identical modes.
    fn best_mode_reference(
        argb: &[u32],
        width: usize,
        height: usize,
        bits: u32,
        tx: usize,
        ty: usize,
        scratch: &mut EntropyScratch,
    ) -> u32 {
        let tile = 1usize << bits;
        let x0 = (tx << bits).max(1);
        let y0 = (ty << bits).max(1);
        let x1 = ((tx << bits) + tile).min(width);
        let y1 = ((ty << bits) + tile).min(height);
        let total = (y1.saturating_sub(y0) * x1.saturating_sub(x0)) as u64;
        let mut best = 0u32;
        let mut best_cost = u64::MAX;
        for mode in 0..=13u32 {
            for y in y0..y1 {
                let row = y * width;
                for x in x0..x1 {
                    let i = row + x;
                    let left = argb[i - 1];
                    let top = argb[i - width];
                    let top_left = argb[i - width - 1];
                    let top_right = argb[i - width + 1];
                    let residual =
                        sub_pixels(argb[i], predict(mode, left, top_left, top, top_right));
                    for (c, &byte) in residual.to_le_bytes().iter().enumerate() {
                        let idx = usize::from(byte);
                        if scratch.counts[c][idx] == 0 {
                            scratch.touched[c].push(byte);
                        }
                        scratch.counts[c][idx] += 1;
                    }
                }
            }
            let mut cost = 0u64;
            for c in 0..4 {
                scratch.used_counts.clear();
                let n = scratch.touched[c].len();
                for k in 0..n {
                    let byte = scratch.touched[c][k];
                    scratch
                        .used_counts
                        .push(scratch.counts[c][usize::from(byte)]);
                }
                cost += shannon_bits(total, &scratch.used_counts);
                for k in 0..n {
                    let byte = scratch.touched[c][k];
                    scratch.counts[c][usize::from(byte)] = 0;
                }
                scratch.touched[c].clear();
            }
            if cost < best_cost {
                best_cost = cost;
                best = mode;
            }
        }
        best
    }

    /// The whole-buffer [`inverse`] must equal feeding each residual row through
    /// [`reconstruct_row_into`] in order, `prev_out` fed from the row just
    /// reconstructed — the row-streaming inverse.
    fn assert_rows_equal_batch(
        coded: &[u32],
        width: u32,
        height: u32,
        bits: u32,
        tile_data: &[u32],
    ) {
        let mut batch = coded.to_vec();
        inverse(&mut batch, width, bits, tile_data);

        let w = width as usize;
        let mut rows = Vec::with_capacity(coded.len());
        let mut prev: Vec<u32> = vec![0u32; w];
        for y in 0..height as usize {
            let mut out = vec![0u32; w];
            reconstruct_row_into(
                &mut out,
                &coded[y * w..(y + 1) * w],
                &prev,
                y,
                bits,
                tile_data,
            );
            rows.extend_from_slice(&out);
            prev = out;
        }
        assert_eq!(batch, rows);
    }

    #[test]
    fn average2_floors_each_lane_independently() {
        // Even sums halve exactly; no carry crosses byte lanes.
        assert_eq!(average2(0x1020_3040, 0x3040_5060), 0x2030_4050);
        // Odd sum floors: (0xff + 0x00) / 2 = 0x7f.
        assert_eq!(average2(0x0000_00ff, 0x0000_0000), 0x0000_007f);
    }

    #[test]
    fn average3_averages_a_and_c_first() {
        // average2(average2(0, 2), 255) = average2(1, 255) = 128 = 0x80.
        // The wrong order average2(average2(0,255),2) would give 0x40.
        assert_eq!(average3(0x0000_0000, 0x0000_00ff, 0x0000_0002), 0x0000_0080);
    }

    #[test]
    fn average4_averages_pairs_then_combines() {
        // average2(average2(0,4), average2(8,12)) = average2(2, 10) = 6.
        assert_eq!(
            average4(0x0000_0000, 0x0000_0004, 0x0000_0008, 0x0000_000c),
            0x0000_0006
        );
    }

    #[test]
    #[allow(
        clippy::cast_sign_loss,
        reason = "test reproduces libwebp's raw i32->u32 !a>>24 bit trick to prove equivalence"
    )]
    fn clip255_matches_libwebp_bit_trick_over_full_domain() {
        for v in -255..=510 {
            let got = u32::from(clip255(v));
            let vu = v as u32; // wrapping cast mirrors the C uint32_t reinterpret
            let trick = if vu < 256 { vu } else { !vu >> 24 };
            assert_eq!(got, trick, "clip255 diverged from trick at v={v}");
        }
    }

    #[test]
    fn add_sub_full_clamps_both_ends() {
        assert_eq!(add_sub_full(10, 20, 5), 25);
        assert_eq!(add_sub_full(0, 0, 255), 0); // -255 clamps to 0
        assert_eq!(add_sub_full(255, 255, 0), 255); // 510 clamps to 255
    }

    #[test]
    fn add_sub_half_truncates_toward_zero_not_arithmetic_shift() {
        // a=10, b=13: 10 + (-3)/2 = 10 + (-1) = 9. An arithmetic >>1 would floor
        // (-3)>>1 = -2 and give 8 — the trap this asserts against.
        assert_eq!(add_sub_half(10, 13), 9);
        assert_eq!(add_sub_half(0, 255), 0); // 0 + (-127) clamps to 0
        assert_eq!(add_sub_half(255, 0), 255); // 255 + 127 clamps to 255
    }

    #[test]
    fn select_returns_a_on_tie_and_b_when_positive() {
        // Symmetric about c (|b-c| == |a-c|): sum == 0 -> a (the <= branch).
        assert_eq!(select(0x0000_005a, 0x0000_006e, 0x0000_0064), 0x0000_005a);
        // |b-c| (30) > |a-c| (10): sum == 20 > 0 -> b.
        assert_eq!(select(0x0000_005a, 0x0000_0082, 0x0000_0064), 0x0000_0082);
    }

    #[test]
    fn clamped_add_subtract_full_and_half_known_values() {
        assert_eq!(
            clamped_add_subtract_full(0x1020_3040, 0x0101_0101, 0x0202_0202),
            0x0f1f_2f3f
        );
        assert_eq!(
            clamped_add_subtract_full(0x0000_00ff, 0x0000_00ff, 0x0000_0000),
            0x0000_00ff // 255 + 255 - 0 = 510 -> 255
        );
        // ave = average2(20, 20) = 20; 20 + (20 - 10) / 2 = 25 = 0x19.
        assert_eq!(
            clamped_add_subtract_half(0x0000_0014, 0x0000_0014, 0x0000_000a),
            0x0000_0019
        );
    }

    #[test]
    fn inverse_reconstructs_3x2_with_mode7_average() {
        // width=3, height=2. bits=2 -> tile_width=4 covers the image in a single
        // tile; GREEN byte 0x07 selects mode 7 = average2(left, top).
        let tile_data = [0x0000_0700_u32];
        let mut argb = [
            0x0010_2030, // (0,0) residual
            0x0001_0203, // (1,0) residual (Left)
            0x0001_0101, // (2,0) residual (Left)
            0x0002_0202, // (0,1) residual (Top)
            0x0000_0000, // (1,1) residual (mode 7)
            0x0000_0000, // (2,1) residual (mode 7)
        ];
        inverse(&mut argb, 3, 2, &tile_data);
        assert_eq!(
            argb,
            [
                0xff10_2030, // black seed lifts alpha to 0xff
                0xff11_2233, // + left
                0xff12_2334, // + left
                0xff12_2232, // + top (argb[0])
                0xff11_2232, // + average2(argb[3], argb[1])
                0xff11_2233, // + average2(argb[4], argb[2])
            ]
        );
    }

    #[test]
    fn inverse_last_column_top_right_wraps_to_current_row_col0() {
        // width=2, height=2, bits=1 -> single tile. GREEN byte 0x03 selects mode
        // 3 (top_right). For the last column, top_right = argb[i-width+1] reads
        // THIS row's column 0, which was just reconstructed via the Top rule.
        let tile_data = [0x0000_0300_u32];
        let mut argb = [0x0000_0005, 0x0000_0003, 0x0000_0002, 0x0000_0001];
        inverse(&mut argb, 2, 1, &tile_data);
        // argb[2] (row1 col0) = 5 + 2 = 7 via Top; argb[3] = 1 + top_right(=7) = 8.
        // Had top_right wrongly used the residual 0x2, argb[3] would be 3.
        assert_eq!(argb, [0xff00_0005, 0xff00_0008, 0xff00_0007, 0xff00_0008]);
    }

    #[test]
    fn forward_reverses_the_mode7_inverse_fixture() {
        // The reconstructed image from `inverse_reconstructs_3x2_with_mode7_average`
        // is fed back as the ORIGINAL: forward picks its own per-tile mode by cost
        // (not necessarily 7), so we assert the forward∘inverse round-trip rather
        // than an exact residual. Because `inverse` operates in place, clone the
        // residual first.
        let original = [
            0xff10_2030u32,
            0xff11_2233,
            0xff12_2334,
            0xff12_2232,
            0xff11_2232,
            0xff11_2233,
        ];
        let (residual, tile_data) = forward(&original, 3, 2, 2);
        let mut round = residual; // owned buffer for the in-place inverse
        inverse(&mut round, 3, 2, &tile_data);
        assert_eq!(round, original);
    }

    #[test]
    fn forward_reverses_the_last_column_wrap_fixture() {
        // The reconstructed image from
        // `inverse_last_column_top_right_wraps_to_current_row_col0` round-trips.
        // With width=2 the last column's top_right = argb[i-width+1] indexes this
        // row's already-formed column 0, exercising the wrap in `forward` under
        // the same index rule the inverse uses.
        let original = [0xff00_0005u32, 0xff00_0008, 0xff00_0007, 0xff00_0008];
        let (residual, tile_data) = forward(&original, 2, 2, 1);
        let mut round = residual; // owned buffer for the in-place inverse
        inverse(&mut round, 2, 1, &tile_data);
        assert_eq!(round, original);
    }

    /// On a mod-256-wrapping linear gradient the entropy metric must pick the
    /// LZ77-friendly mode 1 (a constant, row-identical residual → 0 entropy) and
    /// never the clip255-spiky gradient predictor mode 12, whose residual has
    /// moving per-row wrap spikes that break row-to-row back-references. The old
    /// magnitude proxy scored mode 12 cheapest and blew up the encoded size.
    #[test]
    fn best_mode_prefers_lz77_friendly_mode_on_wrapping_gradient() {
        // Two-axis ramp that wraps inside the image: R = lo(4x) wraps at x = 64,
        // B = lo(2(x+y)) wraps at x + y = 128, mirroring the corpus gradient.
        let ramp = |wide: u32, tall: u32| -> Vec<u32> {
            (0..tall)
                .flat_map(|row| {
                    (0..wide).map(move |col| {
                        let red = (4 * col) & 0xff;
                        let green = (4 * row) & 0xff;
                        let blue = (2 * (col + row)) & 0xff;
                        (255u32 << 24) | (red << 16) | (green << 8) | blue
                    })
                })
                .collect()
        };
        let (wide, tall) = (96u32, 8u32);
        let argb = ramp(wide, tall);
        let (_residual, tile_data) = forward(&argb, wide, tall, 2);
        let modes: Vec<u32> = tile_data.iter().map(|&td| (td >> 8) & 0x0f).collect();
        assert!(
            modes.iter().all(|&m| m != 12),
            "entropy metric must not pick clip255-spiky mode 12: {modes:?}"
        );
        let ones = modes.iter().filter(|&&m| m == 1).count();
        assert!(
            ones * 2 > modes.len(),
            "mode 1 should dominate the interior tiles, got {ones}/{} : {modes:?}",
            modes.len()
        );

        // A single tile whose ramp wraps inside it still resolves to mode 1: its
        // left-difference residual stays constant across the wrap (0 entropy), so
        // the lowest-index zero-entropy mode wins (mode 0 predicts black, never
        // constant on this tile). A step of 40 wraps inside the 8-wide tile.
        let wrap_tile: Vec<u32> = (0..8u32)
            .flat_map(|row| {
                (0..8u32).map(move |col| {
                    let red = (40 * col) & 0xff;
                    let green = (40 * row) & 0xff;
                    let blue = (40 * (col + row)) & 0xff;
                    (255u32 << 24) | (red << 16) | (green << 8) | blue
                })
            })
            .collect();
        let (_r2, td2) = forward(&wrap_tile, 8, 8, 3); // bits=3 -> one 8x8 tile
        assert_eq!(td2.len(), 1);
        assert_eq!(
            (td2[0] >> 8) & 0x0f,
            1,
            "single wrap tile must resolve to mode 1"
        );
    }

    #[test]
    fn inverse_tile_row_offset_is_multiplied_not_divided() {
        // Kill `tile_row = (y >> bits) * tiles_per_row` -> `/`. bits=1 (2x2 tiles),
        // width=4 -> tiles_per_row=2, height=3 -> tiles_per_col=2, so 4 tiles laid
        // out [row0: t0 t1][row1: t2 t3]. Row 2 (y=2) has y>>1 = 1, so the real
        // offset is 1*2 = 2: interior pixels read tile_data[2] / tile_data[3]
        // (mode 2 = Top). The mutant computes 1/2 = 0, reading tile_data[0] /
        // tile_data[1] (mode 1 = Left) instead — a different prediction on row 2.
        let tile_data = [
            0x0000_0100_u32, // t0: mode 1
            0x0000_0100,     // t1: mode 1
            0x0000_0200,     // t2: mode 2 (Top)
            0x0000_0200,     // t3: mode 2 (Top)
        ];
        // Blue-only residuals (alpha/red/green 0 -> alpha lifts to 0xff at the
        // origin and propagates, other lanes stay 0).
        let mut argb = [
            0x0000_000a_u32,
            0x0000_0003,
            0x0000_0005,
            0x0000_0002, // row 0
            0x0000_0005,
            0x0000_0002,
            0x0000_0002,
            0x0000_0002, // row 1
            0x0000_0004,
            0x0000_0003,
            0x0000_0001,
            0x0000_0001, // row 2
        ];
        inverse(&mut argb, 4, 1, &tile_data);
        // Row 2 predicts from Top (row 1): the mutant would predict from Left and
        // diverge at columns 1..3 (e.g. p9 = 20 vs the mutant's 22).
        assert_eq!(
            argb,
            [
                0xff00_000a,
                0xff00_000d,
                0xff00_0012,
                0xff00_0014, // row 0
                0xff00_000f,
                0xff00_0011,
                0xff00_0013,
                0xff00_0015, // row 1
                0xff00_0013,
                0xff00_0014,
                0xff00_0014,
                0xff00_0016, // row 2
            ]
        );
    }

    #[test]
    fn reconstruct_row_into_equals_whole_buffer_inverse_on_fixtures() {
        // The two hand-built inverse fixtures must reconstruct identically row by
        // row and whole-buffer. First: the mode-7 average fixture (3x2).
        assert_rows_equal_batch(
            &[0x0010_2030, 0x0001_0203, 0x0001_0101, 0x0002_0202, 0, 0],
            3,
            2,
            2,
            &[0x0000_0700],
        );
        // Second: the last-column top-right wrap (mode 3, width 2).
        assert_rows_equal_batch(&[5, 3, 2, 1], 2, 2, 1, &[0x0000_0300]);
    }

    #[test]
    fn inverse_returns_untouched_on_zero_width() {
        // The `width == 0 || argb.is_empty()` guard must SHORT-CIRCUIT on a
        // zero-width (but non-empty) buffer: `inverse` returns leaving `argb`
        // untouched. Flipping `||` to `&&` makes the guard false here, so the
        // body runs `argb.len() / width` with width 0 and divides by zero.
        let mut argb = [0x1122_3344u32, 0x5566_7788];
        inverse(&mut argb, 0, 2, &[]);
        assert_eq!(argb, [0x1122_3344u32, 0x5566_7788]);
    }

    #[test]
    fn forward_returns_zero_residual_on_zero_width() {
        // Same short-circuit on the encode side: a zero-width input returns the
        // freshly zeroed residual buffer untouched. With `||` -> `&&` the guard
        // is false, the body runs, and residual[0] is overwritten with
        // sub_pixels(argb[0], ARGB_BLACK) = 0x1222_3344 (non-zero), diverging.
        let (residual, tile_data) = forward(&[0x1122_3344u32], 0, 1, 2);
        assert_eq!(residual, vec![0u32]);
        assert!(tile_data.is_empty());
    }

    #[test]
    fn best_mode_uses_shifted_tile_origin_for_interior_row() {
        // Kill `y0 = (ty << bits).max(1)` -> `>>`. Tile (tx=0, ty=1), bits=1,
        // width=2, height=4. The real y-origin is (1 << 1)=2, so only rows 2..4
        // (x=1) are scored; both have value 15, so mode 0's residual is constant
        // (0 bits) and wins -> mode 0. Under `>>`, y0=(1>>1).max(1)=1 pulls in
        // row 1 (value 20): mode 0 is no longer constant, but mode 1 (left) has a
        // constant residual of 5 across rows 1..4 -> the mutant returns mode 1.
        let argb = [0u32, 0, 15, 20, 10, 15, 10, 15];
        let mut scratch = EntropyScratch::new();
        assert_eq!(best_mode(&argb, 2, 4, 1, 0, 1, &mut scratch), 0);
    }

    #[test]
    fn best_mode_reads_top_right_by_subtraction_not_division() {
        // Kill the `top_right = argb[i - width + 1]` gather -> `argb[i / width + 1]`.
        // For every interior pixel `i = y*width + x` (0 < x < width), integer
        // `i / width` collapses to the row index `y`, so the mutant feeds
        // `argb[y + 1]` into the cost histogram instead of the true up-right
        // neighbor `argb[(y-1)*width + x + 1]` — a different pixel, silently (no
        // panic), which shifts only the modes that read top_right (3, 5, 9, 10).
        //
        // width=4, height=3, bits=2 -> one tile covering the whole image; interior
        // is y in {1,2}, x in {1,2,3}. The blue lane is hand-built so mode 3
        // (top_right) reproduces EVERY interior pixel exactly (all-zero residual ->
        // 0 entropy, the unique zero-cost mode, so it wins outright). alpha stays
        // 0xff and red/green stay 0 across the tile, so those lanes cost 0 for every
        // mode and only the blue lane decides the winner.
        //
        //   blue grid      row0:  68  32 130  60
        //                  row1: 253 130  60 253
        //                  row2: 230  60 253 230
        //
        // Each interior pixel equals its true top-right neighbor (e.g. (1,1)=130 =
        // argb[2], (3,1)=253 = argb[4] the next row's col 0), so correct top_right
        // gives mode 3 a flat zero residual -> best_mode returns 3. Under the
        // mutant, top_right becomes argb[y+1] (130 for row 1, 60 for row 2): mode
        // 3's residual is no longer constant (cost jumps to 13 bits) and mode 0
        // (opaque black, cost 11) wins instead. The exact chosen mode diverges
        // 3 -> 0, deterministically, at any case count.
        let argb = [
            0xff00_0044_u32,
            0xff00_0020,
            0xff00_0082,
            0xff00_003c, // row 0
            0xff00_00fd,
            0xff00_0082,
            0xff00_003c,
            0xff00_00fd, // row 1
            0xff00_00e6,
            0xff00_003c,
            0xff00_00fd,
            0xff00_00e6, // row 2
        ];
        let mut scratch = EntropyScratch::new();
        assert_eq!(best_mode(&argb, 4, 3, 2, 0, 0, &mut scratch), 3);
    }

    #[test]
    fn predict_dispatches_each_mode_to_its_distinct_combo() {
        // Every flagged match arm, exercised with four distinct non-black
        // neighbors so each mode's true output differs from the `_ => ARGB_BLACK`
        // fallback a deleted arm would collapse into.
        let left = 0xff10_2030u32;
        let tl = 0xff40_5060u32;
        let t = 0xff70_8090u32;
        let tr = 0xffa0_b0c0u32;
        assert_eq!(predict(2, left, tl, t, tr), t);
        assert_eq!(predict(4, left, tl, t, tr), tl);
        assert_eq!(predict(5, left, tl, t, tr), average3(left, t, tr));
        assert_eq!(predict(6, left, tl, t, tr), average2(left, tl));
        assert_eq!(predict(8, left, tl, t, tr), average2(tl, t));
        assert_eq!(predict(9, left, tl, t, tr), average2(t, tr));
        assert_eq!(predict(10, left, tl, t, tr), average4(left, tl, t, tr));
        assert_eq!(predict(11, left, tl, t, tr), select(t, left, tl));
        assert_eq!(
            predict(12, left, tl, t, tr),
            clamped_add_subtract_full(left, t, tl)
        );
        assert_eq!(
            predict(13, left, tl, t, tr),
            clamped_add_subtract_half(left, t, tl)
        );
        // The outputs are all non-black, so a deleted arm (-> ARGB_BLACK) diverges.
        for &v in &[
            t,
            tl,
            average3(left, t, tr),
            average2(left, tl),
            average2(tl, t),
            average2(t, tr),
            average4(left, tl, t, tr),
            select(t, left, tl),
            clamped_add_subtract_full(left, t, tl),
            clamped_add_subtract_half(left, t, tl),
        ] {
            assert_ne!(v, super::ARGB_BLACK);
        }
    }

    proptest! {
        #[test]
        fn forward_then_inverse_is_identity(
            (width, height, bits, argb) in (1u32..=8, 1u32..=8, 2u32..=5u32).prop_flat_map(
                |(w, h, bits)| {
                    prop::collection::vec(any::<u32>(), (w as usize) * (h as usize))
                        .prop_map(move |argb| (w, h, bits, argb))
                },
            )
        ) {
            let (residual, tile_data) = forward(&argb, width, height, bits);
            // `inverse` reconstructs in place, so move the residual into an owned
            // buffer and compare back to the untouched original.
            let mut round = residual;
            inverse(&mut round, width, bits, &tile_data);
            prop_assert_eq!(round, argb);
        }

        /// The whole-buffer inverse equals looping [`reconstruct_row_into`] over
        /// every residual row (with `prev_out` chained from the reconstructed
        /// previous row) across random residuals, dims, tile sizes, and modes.
        #[test]
        fn reconstruct_row_into_matches_whole_buffer_inverse(
            (width, height, bits, coded, modes) in (1u32..=8, 1u32..=8, 2u32..=5u32)
                .prop_flat_map(|(w, h, bits)| {
                    let n = (w as usize) * (h as usize);
                    let tiles =
                        (subsample_size(w, bits) as usize) * (subsample_size(h, bits) as usize);
                    (
                        Just(w),
                        Just(h),
                        Just(bits),
                        prop::collection::vec(any::<u32>(), n),
                        prop::collection::vec(0u32..=13, tiles),
                    )
                })
        ) {
            let tile_data: Vec<u32> = modes.iter().map(|&m| (m & 0x0f) << 8).collect();
            assert_rows_equal_batch(&coded, width, height, bits, &tile_data);
        }

        /// The hoisted-neighbor [`best_mode`] must pick the byte-identical mode of
        /// the pre-hoist [`best_mode_reference`] for every tile, across noise,
        /// solid/palette-like tiles (small alphabet, alpha present and absent),
        /// 1x1 and tiny dims, and every tile size.
        #[test]
        fn best_mode_matches_reference(
            (width, height, bits, argb) in (1u32..=8, 1u32..=8, 2u32..=5u32).prop_flat_map(
                |(w, h, bits)| {
                    let n = (w as usize) * (h as usize);
                    prop_oneof![
                        prop::collection::vec(any::<u32>(), n),
                        prop::collection::vec(
                            prop::sample::select(vec![
                                0u32, 0xffff_ffff, 0xff00_0000, 0x00ff_ffff, 0xff80_8080,
                            ]),
                            n,
                        ),
                    ]
                    .prop_map(move |argb| (w, h, bits, argb))
                },
            )
        ) {
            let w = width as usize;
            let h = height as usize;
            let tiles_per_row = subsample_size(width, bits) as usize;
            let tiles_per_col = subsample_size(height, bits) as usize;
            let mut fast = EntropyScratch::new();
            let mut slow = EntropyScratch::new();
            for ty in 0..tiles_per_col {
                for tx in 0..tiles_per_row {
                    let got = best_mode(&argb, w, h, bits, tx, ty, &mut fast);
                    let want = best_mode_reference(&argb, w, h, bits, tx, ty, &mut slow);
                    prop_assert_eq!(got, want);
                }
            }
        }
    }

    // ---- Crafted single-tile images whose min-entropy mode is a KNOWN single
    // predictor, so a mutation that mis-gathers that predictor's neighbor (or
    // mis-shifts the tile origin) flips the chosen mode deterministically. Each
    // is a ramp where exactly ONE difference is constant across the interior
    // (0 entropy) while the competing modes vary, plus flat alpha/red/green so
    // only the blue lane decides. Winners verified by direct computation.

    /// Horizontal ramp (blue = 20·x, rows identical): the left-difference is a
    /// constant 20, so mode 1 (Left) reaches 0 entropy. Mode 2 (Top) is also 0
    /// (identical rows), so the tie resolves to the lower index — mode 1.
    fn img_left_winner() -> Vec<u32> {
        (0..4)
            .flat_map(|_y| (0..4u32).map(|x| 0xff00_0000 | (20 * x)))
            .collect()
    }

    /// Vertical ramp with a nonlinear per-column offset (blue = 10·y + H[x]):
    /// the top-difference is a constant 10 (mode 2 → 0 entropy) while the
    /// left-difference H[x]−H[x−1] varies, so mode 2 is the unique winner.
    fn img_top_winner() -> Vec<u32> {
        let h = [0u32, 7, 15, 26];
        (0..4u32)
            .flat_map(|y| (0..4usize).map(move |x| 0xff00_0000 | (10 * y + h[x])))
            .collect()
    }

    /// Diagonal ramp (blue = 10·y + H[x−y]) whose top-LEFT difference is the
    /// constant 10 (mode 4 → 0 entropy) while left/top/top-right differences all
    /// vary with the nonlinear H, so mode 4 (TL) is the unique winner.
    fn img_top_left_winner() -> Vec<u32> {
        let h = [0i32, 5, 9, 20, 26, 35, 41]; // indexed by (x - y + 3), x,y in 0..4
        (0..4i32)
            .flat_map(|y| {
                (0..4i32).map(move |x| 0xff00_0000 | ((10 * y + h[(x - y + 3) as usize]) as u32))
            })
            .collect()
    }

    #[test]
    fn best_mode_tile_x_origin_shifts_left_not_right() {
        // Tile (tx=1, ty=1), bits=1, over a 4x4 image whose blue is [0,100,110,120]
        // per (identical) row. Mode 1's left-difference is constant 10 over the tile
        // interior columns {2,3} but breaks if column 1 is pulled in (100−0), and
        // mode 2's top-difference is 0 everywhere; so the CORRECT origin scores
        // {2,3}×{2,3} and returns mode 1. Mutating `tx << bits` -> `tx >> bits`
        // (line 203) pulls x0 back to column 1, breaking mode 1 so mode 2 wins;
        // mutating the x1/y1 origins (`<<`->`>>` or `+ tile`->`- tile`, lines 205/206)
        // collapses the interior to empty and returns mode 0. All diverge from 1.
        let blue = [0u32, 100, 110, 120];
        let argb: Vec<u32> = (0..4)
            .flat_map(|_y| (0..4usize).map(move |x| 0xff00_0000 | blue[x]))
            .collect();
        let mut scratch = EntropyScratch::new();
        assert_eq!(best_mode(&argb, 4, 4, 1, 1, 1, &mut scratch), 1);
    }

    #[test]
    fn best_mode_top_left_neighbor_is_load_bearing() {
        // The diagonal ramp's unique zero-entropy mode is 4 (top-left). Any
        // mutation that corrupts the top_left gather (`argb[i - width - 1]`, line
        // 220) makes mode 4's residual non-constant so a different mode wins, and
        // the left-self mutation (`argb[i - 1]` -> `argb[i]`, line 219) makes
        // mode 1 a spurious 0-entropy winner — both diverge from 4.
        let argb = img_top_left_winner();
        let mut scratch = EntropyScratch::new();
        assert_eq!(best_mode(&argb, 4, 4, 2, 0, 0, &mut scratch), 4);
    }

    #[test]
    fn best_mode_top_neighbor_is_load_bearing() {
        // The vertical ramp's unique zero-entropy mode is 2 (top). Corrupting the
        // top gather (`argb[i - width]`, line 221) breaks mode 2, and the row
        // stride / left-self mutations flip the winner too — all diverge from 2.
        let argb = img_top_winner();
        let mut scratch = EntropyScratch::new();
        assert_eq!(best_mode(&argb, 4, 4, 2, 0, 0, &mut scratch), 2);
    }

    #[test]
    fn best_mode_left_ramp_resolves_to_mode_one() {
        // Sanity anchor for the left/tie logic: the horizontal ramp resolves to
        // mode 1, and `< -> <=` on the argmin (line 260) would resolve the mode1/
        // mode2 tie to the HIGHER index instead, diverging from 1.
        let argb = img_left_winner();
        let mut scratch = EntropyScratch::new();
        assert_eq!(best_mode(&argb, 4, 4, 2, 0, 0, &mut scratch), 1);
    }

    #[test]
    fn reconstruct_row_into_interior_modes_and_wrap_are_pinned() {
        // width=4, bits=1 -> tiles_per_row=2; reconstruct interior row y=2, whose
        // tile_row = (2>>1)*2 = 2. tile_data is laid so the CORRECT indices
        // [tile_row + (x>>1)] read mode 4 (top_left) at x=1 and mode 3 (top_right)
        // at x=2 (a NON-last interior column) and x=3 (the last column, whose
        // top_right wraps to out[0]). This pins, deterministically:
        //   * line 113 `(y>>bits) * tiles_per_row`: `*`->`/` or `+` re-bases
        //     tile_row and reads a different mode.
        //   * line 115 `tile_row + (x>>bits)`: `+`->`-` reads tile_data[1] (mode 1)
        //     for x>=2 — the known flaky survivor.
        //   * line 118 top_left `prev_out[x-1]`: `-`->`/` (== prev_out[x]) changes
        //     the mode-4 pixel at x=1.
        //   * line 121 `x + 1 < width`: `<`->`>` sends the non-last mode-3 column to
        //     out[0] instead of prev_out[x+1].
        //   * line 122 top_right `prev_out[x+1]`: `+`->`-`/`*` reads the wrong row-0
        //     neighbor for the mode-3 columns.
        // Panic-covered index blowups (`x<<bits`, out[x+1]/prev_out[x+1] at the last
        // column) also fail here.
        let prev_out = [0x0000_000a_u32, 0x0000_0014, 0x0000_001e, 0x0000_0028];
        let residual = [0x0000_0002_u32, 0x0000_0001, 0x0000_0003, 0x0000_0005];
        let tile_data = [0x0000_0200_u32, 0x0000_0100, 0x0000_0400, 0x0000_0300];
        let mut out = [0u32; 4];
        reconstruct_row_into(&mut out, &residual, &prev_out, 2, 1, &tile_data);
        // out[0]=2+10=12; x1 mode4 tl=prev[0]=10 ->1+10=11; x2 mode3 tr=prev[3]=40
        // ->3+40=43; x3 mode3 tr wraps to out[0]=12 ->5+12=17.
        assert_eq!(out, [0x0000_000c, 0x0000_000b, 0x0000_002b, 0x0000_0011]);
    }

    #[test]
    fn inverse_mode4_reads_top_left_neighbor() {
        // A whole-buffer inverse with mode 4 (top_left) pins reconstruct_row's
        // top_left gather (`argb[i - width - 1]`, line 64): flipping either `-`
        // makes the interior read a different pixel. width=3, height=2, bits=2
        // (single tile). Blue-only residuals; alpha lifts to 0xff at the origin.
        let tile_data = [0x0000_0400_u32]; // mode 4
        let mut argb = [
            0x0000_0005_u32,
            0x0000_0003,
            0x0000_0002, // row 0 residuals
            0x0000_0001,
            0x0000_0000,
            0x0000_0000, // row 1 residuals
        ];
        inverse(&mut argb, 3, 2, &tile_data);
        // row0: 5, 5+3=8, 8+2=10 (alpha 0xff). row1 col0 (Top): 1+5=6.
        // x=1 mode4 tl=argb[0]=5 -> 0+5=5; x=2 mode4 tl=argb[1]=8 -> 0+8=8.
        assert_eq!(
            argb,
            [
                0xff00_0005,
                0xff00_0008,
                0xff00_000a, // row 0
                0xff00_0006,
                0xff00_0005,
                0xff00_0008, // row 1
            ]
        );
    }

    #[test]
    fn forward_roundtrips_each_single_predictor_mode() {
        // Each crafted image drives `forward` to a single known mode, so its
        // residual loop exercises that predictor's neighbor gather; a corrupt
        // gather (or a mis-read tile mode) makes the residual inconsistent with
        // the untouched inverse and breaks the round-trip. mode1=Left, mode2=Top,
        // mode4=TL, plus the top-right fixture (mode 3) from the best_mode test.
        let mode3_img = [
            0xff00_0044_u32,
            0xff00_0020,
            0xff00_0082,
            0xff00_003c, // row 0
            0xff00_00fd,
            0xff00_0082,
            0xff00_003c,
            0xff00_00fd, // row 1
            0xff00_00e6,
            0xff00_003c,
            0xff00_00fd,
            0xff00_00e6, // row 2
        ];
        for (img, w, h, bits) in [
            (img_left_winner(), 4u32, 4u32, 2u32),
            (img_top_winner(), 4, 4, 2),
            (img_top_left_winner(), 4, 4, 2),
            (mode3_img.to_vec(), 4, 3, 2),
        ] {
            let (residual, tile_data) = forward(&img, w, h, bits);
            let mut round = residual;
            inverse(&mut round, w, bits, &tile_data);
            assert_eq!(round, img, "round-trip failed for {w}x{h}");
        }
    }

    #[test]
    fn forward_roundtrips_multi_tile_with_mixed_modes() {
        // An 8x8 image at bits=2 is a 2x2 tile grid; the top rows form a
        // horizontal gradient (tile-row 0 -> modes 1,1) and the bottom rows a
        // per-column ramp (tile-row 1 -> modes 2,12), so tile_row is non-zero and
        // adjacent tiles differ. This pins forward's residual-loop tile index
        // (`tile_row + (x>>bits)`, line 304) and stride (`(y>>bits)*tiles_per_row`,
        // line 301): any `+`/`*`/`/` there reads a different tile's mode than the
        // untouched inverse, so the round-trip breaks.
        let h = [0i32, 5, 9, 20, 26, 35, 41];
        let img: Vec<u32> = (0..8i32)
            .flat_map(|y| {
                (0..8i32).map(move |x| {
                    let b = if y < 4 {
                        (13 * x) & 0xff
                    } else {
                        (7 * y + h[(x % 7) as usize]) & 0xff
                    };
                    0xff00_0000 | (b as u32)
                })
            })
            .collect();
        let (residual, tile_data) = forward(&img, 8, 8, 2);
        // Guard the premise: tile-row 0 and tile-row 1 must carry different modes.
        let modes: Vec<u32> = tile_data.iter().map(|&d| (d >> 8) & 0x0f).collect();
        assert_eq!(modes, vec![1, 1, 2, 12]);
        let mut round = residual;
        inverse(&mut round, 8, 2, &tile_data);
        assert_eq!(round, img);
    }
}
