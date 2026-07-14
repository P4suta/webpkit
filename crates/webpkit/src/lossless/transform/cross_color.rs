//! Cross-color transform — `inverse` ports libwebp `TransformColorInverse_C`;
//! `forward` is its exact per-pixel encode-side mirror.
//!
//! The encoder decorrelates the color channels per tile: it subtracts a scaled
//! amount of green from red, of green from blue, and of the (already
//! reconstructed) red from blue. This inverse adds those contributions back.
//! Tiling mirrors the predictor transform: the image is split into
//! `2^bits`-sized square tiles and every pixel inside a tile shares the single
//! 24-bit multiplier code stored in `tile_data`. `forward` subtracts exactly the
//! deltas `inverse` adds — using the ORIGINAL red as the `red_to_blue` input,
//! which equals the red `inverse` reconstructs — so the pair round-trips
//! losslessly. Per tile it picks the three multipliers by a deterministic
//! integer greedy sweep (no float, no hashing), keeping the residual magnitudes
//! small so the stored channels compress well.

use crate::lossless::constants::subsample_size;
use crate::lossless::prelude::*;
use crate::lossless::work::work;

/// Coordinate-descent rounds for the coupled (`green_to_blue`, `red_to_blue`) pair.
/// `1` reproduces the historical axis-separated greedy byte-for-byte; higher values
/// re-minimize each blue-axis multiplier against the other's current value, so the
/// summed blue residual is monotonically non-increasing.
const MAX_CC_REFINE_ROUNDS: u32 = 3;

/// Per-tile signed color-mixing multipliers unpacked from a 24-bit tile code.
struct Multipliers {
    /// Scaled green added back into red.
    green_to_red: i8,
    /// Scaled green added back into blue.
    green_to_blue: i8,
    /// Scaled (reconstructed) red added back into blue.
    red_to_blue: i8,
}

/// Unpack the low 24 bits of a tile code into its three signed multipliers.
///
/// The bytes are little-endian `[green_to_red, green_to_blue, red_to_blue, _]`;
/// each is reinterpreted as `i8`, so byte values `>= 128` become negative.
#[must_use]
const fn code_to_multipliers(code: u32) -> Multipliers {
    let [g2r, g2b, r2b, _] = code.to_le_bytes();
    Multipliers {
        green_to_red: i8::from_le_bytes([g2r]),
        green_to_blue: i8::from_le_bytes([g2b]),
        red_to_blue: i8::from_le_bytes([r2b]),
    }
}

/// Signed multiply then arithmetic shift right by 5 (libwebp `ColorTransformDelta`).
///
/// The shift is on an `i32`, which is arithmetic in Rust, so negative products
/// round toward negative infinity exactly as the C reference does.
#[must_use]
fn color_transform_delta(pred: i8, channel: i8) -> i32 {
    (i32::from(pred) * i32::from(channel)) >> 5
}

/// Low 8 bits of an `i32` as `u8` (least-significant byte, sign-agnostic).
#[must_use]
const fn low_u8(v: i32) -> u8 {
    v.to_le_bytes()[0]
}

/// Low byte reinterpreted as `i8` (libwebp `(int8_t)` cast).
#[must_use]
const fn low_i8(v: i32) -> i8 {
    i8::from_le_bytes([low_u8(v)])
}

/// Reconstruct one pixel: green and alpha pass through unchanged; red gets its
/// green contribution back, then blue gets both its green contribution and the
/// contribution from the *updated, masked* red.
#[must_use]
fn transform_pixel(argb: u32, mul: &Multipliers) -> u32 {
    let [b, g, r, a] = argb.to_le_bytes();
    let green = i8::from_le_bytes([g]);
    let mut new_red = i32::from(r);
    let mut new_blue = i32::from(b);
    new_red += color_transform_delta(mul.green_to_red, green);
    new_red &= 0xff;
    new_blue += color_transform_delta(mul.green_to_blue, green);
    // red_to_blue feeds on the reconstructed red, masked to 8 bits first.
    new_blue += color_transform_delta(mul.red_to_blue, low_i8(new_red));
    new_blue &= 0xff;
    u32::from_le_bytes([low_u8(new_blue), g, low_u8(new_red), a])
}

/// Cross-color inverse: undo the green->red / green->blue / red->blue
/// decorrelation, per tile.
///
/// The transform is pointwise within a row (tile selection only depends on the
/// row index `y`), so this delegates to [`inverse_row`] — the batch and
/// row-streaming forms share one implementation and can never drift.
pub(crate) fn inverse(argb: &mut [u32], width: u32, bits: u32, tile_data: &[u32]) {
    if width == 0 {
        return;
    }
    let width = width as usize;
    for (y, row) in argb.chunks_mut(width).enumerate() {
        inverse_row(row, y, bits, tile_data);
    }
}

/// Cross-color inverse over a single row at image row `y` — the row-streaming
/// counterpart of the whole-buffer [`inverse`]. The row's own width picks the
/// tile column (`subsample_size(row.len(), bits)` tiles per row); `y >> bits`
/// picks the tile row. Looping this over every row reproduces [`inverse`]
/// exactly (proven in tests).
pub(crate) fn inverse_row(row: &mut [u32], y: usize, bits: u32, tile_data: &[u32]) {
    // A row is one image row, so its length is a validated 14-bit VP8L width that
    // always fits `u32`; the cast is lossless. `width == 0` then means a genuinely
    // empty row (nothing to transform) rather than a silently truncated length.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "row length is a validated 14-bit image width, so it fits u32"
    )]
    let width = row.len() as u32;
    if width == 0 {
        return;
    }
    let tiles_per_row = subsample_size(width, bits) as usize;
    let tile_row = (y >> bits) * tiles_per_row;
    // All pixels in `[tx<<bits, (tx+1)<<bits)` share one tile code, so walk the row
    // in `2^bits`-wide runs and unpack the multipliers ONCE per run instead of
    // re-fetching `tile_data` and re-splitting the code on every pixel. The `n`th
    // `chunks_mut` chunk holds exactly the pixels whose `x >> bits == n`, so the tile
    // code selected is identical to the per-pixel form — byte-invariant.
    let run = 1usize << bits;
    for (tx, chunk) in row.chunks_mut(run).enumerate() {
        let code = tile_data.get(tile_row + tx).copied().unwrap_or(0);
        let mul = code_to_multipliers(code);
        for pixel in chunk.iter_mut() {
            *pixel = transform_pixel(*pixel, &mul);
        }
    }
}

/// The pre-hoist [`inverse_row`] verbatim: it re-fetches `tile_data` and re-splits
/// the tile code on every pixel. The run-chunked [`inverse_row`] must reproduce the
/// row byte for byte (each `2^bits`-wide chunk selects the same code the per-pixel
/// `x >> bits` did), pinned by `inverse_row_matches_reference`. Compiled only for
/// that proptest (`test`) and the `kernels` microbench (`bench` feature).
#[cfg(any(test, feature = "bench"))]
pub(crate) fn inverse_row_reference(row: &mut [u32], y: usize, bits: u32, tile_data: &[u32]) {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "row length is a validated 14-bit image width, so it fits u32"
    )]
    let width = row.len() as u32;
    if width == 0 {
        return;
    }
    let tiles_per_row = subsample_size(width, bits) as usize;
    let tile_row = (y >> bits) * tiles_per_row;
    for (x, pixel) in row.iter_mut().enumerate() {
        let code = tile_data.get(tile_row + (x >> bits)).copied().unwrap_or(0);
        *pixel = transform_pixel(*pixel, &code_to_multipliers(code));
    }
}

/// One tile pixel unpacked once per tile for the encode sweeps: `green` is the
/// green channel as `i8` (the multiplier input), `r`/`g`/`b`/`a` are the raw
/// bytes the sweeps and emit consume, and `idx` is the pixel's offset in the
/// stored buffer. Gathering these into a contiguous scratch lets every
/// 256-value multiplier sweep and the emit read the tile sequentially instead of
/// re-striding `argb` (stride `width`) and re-unpacking on each pass.
struct TilePixel {
    idx: usize,
    green: i8,
    r: u8,
    g: u8,
    b: u8,
    a: u8,
}

/// Signed residual magnitude of a stored byte: `min(v, 256 - v)`, i.e. the
/// distance from zero of the byte read as an `i8` (`|-56| == 200`'s complement
/// `56`). Smaller means the residual clusters tighter around zero, coding cheaper.
#[must_use]
fn residual_magnitude(byte: u8) -> u32 {
    let v = u32::from(byte);
    v.min(256 - v)
}

/// Sweep every `i8` multiplier and return the cost-minimizing one, breaking ties
/// toward zero then toward the lower value. The key `(cost, |m|, m)` is unique per
/// `m`, so the minimum is well-defined.
#[must_use]
fn best_multiplier(cost: impl Fn(i8) -> u64) -> i8 {
    let mut best = 0i8;
    let mut best_key = (u64::MAX, u32::MAX, i32::MAX);
    for m in i8::MIN..=i8::MAX {
        work!(CrossColorEval);
        let key = (cost(m), i32::from(m).unsigned_abs(), i32::from(m));
        if key < best_key {
            best_key = key;
            best = m;
        }
    }
    best
}

/// Fill `dst` with each tile pixel's stored-blue base: original blue minus the
/// HELD blue multiplier's delta. A subsequent 256-value sweep of the OTHER blue
/// axis then adds only its own per-pixel delta, so the held axis's delta — loop-
/// invariant across the sweep — is computed once here instead of on every eval.
/// `hold_red` selects the held axis: `red_to_blue` (feeding on red) when true,
/// else `green_to_blue` (feeding on green).
fn fill_blue_base(dst: &mut Vec<i32>, tile_px: &[TilePixel], hold_red: bool, held: i8) {
    dst.clear();
    for px in tile_px {
        let held_delta = if hold_red {
            color_transform_delta(held, i8::from_le_bytes([px.r]))
        } else {
            color_transform_delta(held, px.green)
        };
        dst.push(i32::from(px.b) - held_delta);
    }
}

/// Pick the blue multiplier minimizing the summed stored-blue residual, given
/// `blue_base` already holding the fixed axis's contribution (see
/// [`fill_blue_base`]). `sweep_red` selects the swept axis's channel input: red
/// (for `red_to_blue`) when true, else green (for `green_to_blue`). Adding the
/// swept delta to the pre-subtracted base and masking reproduces `stored_blue`
/// exactly (i32 subtraction commutes; only the low byte reaches the residual), so
/// the choice is byte-identical to the two-delta form.
#[must_use]
fn sweep_blue(
    tile_px: &[TilePixel],
    blue_base: &[i32],
    sweep_red: bool,
    channel: &mut Vec<i8>,
) -> i8 {
    // The swept channel is invariant across all 256 multiplier evals, so gather it
    // once into a contiguous `channel` scratch instead of re-reading the strided AoS
    // `TilePixel` field (and re-branching on `sweep_red`) inside every eval — the
    // same "gather once, sweep many" shape as [`fill_blue_base`], and the reason the
    // caller threads a reused buffer through. `residual_magnitude <= 128` and a tile
    // holds at most `2^bits × 2^bits <= 1024` pixels, so the per-eval sum stays far
    // below `u32::MAX`; folding in `u32` (widened once to the `u64` the cost key
    // wants) is bit-identical to a flat `u64` fold.
    channel.clear();
    channel.extend(tile_px.iter().map(|px| {
        if sweep_red {
            i8::from_le_bytes([px.r])
        } else {
            px.green
        }
    }));
    best_multiplier(|m| {
        let mut sum = 0u32;
        for (&ch, &base) in channel.iter().zip(blue_base) {
            let sb = low_u8(base - color_transform_delta(m, ch));
            sum += residual_magnitude(sb);
        }
        u64::from(sum)
    })
}

/// The pre-scratch [`sweep_blue`] verbatim: it re-reads the swept channel from the
/// strided array-of-structs `TilePixel` field (and re-branches on `sweep_red`) inside every one
/// of the 256 multiplier evals, folding into a `u64`. The channel-gathering
/// [`sweep_blue`] must return the identical `i8` (the gather cannot observe a
/// different channel value, and the `u32` fold is exact), pinned end-to-end by
/// `forward_matches_reference` and directly by `sweep_blue_matches_reference`.
/// Compiled only for those proptests (`test`) and the `kernels` microbench
/// (`bench` feature).
#[cfg(any(test, feature = "bench"))]
#[must_use]
fn sweep_blue_reference(tile_px: &[TilePixel], blue_base: &[i32], sweep_red: bool) -> i8 {
    best_multiplier(|m| {
        let mut sum = 0u64;
        for (px, &base) in tile_px.iter().zip(blue_base) {
            let ch = if sweep_red {
                i8::from_le_bytes([px.r])
            } else {
                px.green
            };
            let sb = low_u8(base - color_transform_delta(m, ch));
            sum += u64::from(residual_magnitude(sb));
        }
        sum
    })
}

/// Bench-only driver for the `kernels` microbench: build a tile from parallel
/// `green`/`red` channels (with a zero blue/alpha — `sweep_blue` reads only the
/// swept channel and `blue_base`) and run one full [`sweep_blue`]. The tile build
/// is `O(len)` and identical to [`bench_sweep_blue_reference`]'s, so it cancels in
/// the opt-vs-ref delta the microbench reads; the `O(256·len)` sweep dominates.
#[cfg(feature = "bench")]
#[must_use]
pub(crate) fn bench_sweep_blue(
    green: &[i8],
    red: &[u8],
    blue_base: &[i32],
    sweep_red: bool,
    channel: &mut Vec<i8>,
) -> i8 {
    let tile: Vec<TilePixel> = green
        .iter()
        .zip(red)
        .map(|(&green, &r)| TilePixel {
            idx: 0,
            green,
            r,
            g: 0,
            b: 0,
            a: 0,
        })
        .collect();
    sweep_blue(&tile, blue_base, sweep_red, channel)
}

/// Bench-only driver: the [`bench_sweep_blue`] twin running the pre-scratch
/// [`sweep_blue_reference`], for the back-to-back A/B.
#[cfg(feature = "bench")]
#[must_use]
pub(crate) fn bench_sweep_blue_reference(
    green: &[i8],
    red: &[u8],
    blue_base: &[i32],
    sweep_red: bool,
) -> i8 {
    let tile: Vec<TilePixel> = green
        .iter()
        .zip(red)
        .map(|(&green, &r)| TilePixel {
            idx: 0,
            green,
            r,
            g: 0,
            b: 0,
            a: 0,
        })
        .collect();
    sweep_blue_reference(&tile, blue_base, sweep_red)
}

/// Cross-color forward: the exact inverse of [`transform_pixel`]. Green and alpha
/// pass through untouched; the stored red/blue are the original channels with the
/// same green->red / green->blue / red->blue deltas *subtracted* that
/// [`transform_pixel`] adds back. Because the reconstruction is exact, the
/// `red_to_blue` term feeds on the ORIGINAL red (as `i8`), which is precisely the
/// red [`transform_pixel`] rebuilds, so `inverse ∘ forward` is the identity for
/// any chosen multipliers.
///
/// `green_to_red` is selected by a single deterministic greedy sweep over the
/// full `i8` range, minimizing the summed red residual magnitude `min(v, 256 - v)`.
/// The blue axis is coupled — both `green_to_blue` and `red_to_blue` feed the same
/// stored-blue residual — so it is minimized by a bounded coordinate descent: the
/// first round reproduces the historical axis-separated passes (`green_to_blue`
/// with `red_to_blue = 0`, then `red_to_blue`), and up to
/// `MAX_CC_REFINE_ROUNDS - 1` further rounds re-minimize each blue multiplier
/// against the other's current value, stopping early once the pair is stable. Each
/// sweep resolves ties to the multiplier closest to zero, then to the lower (more
/// negative) value — integer-only, so the choice is bit-reproducible across
/// platforms and the summed blue residual is monotonically non-increasing.
///
/// Returns `(stored, tile_data)` where `tile_data` holds one pixel per tile whose
/// little-endian bytes are `[green_to_red, green_to_blue, red_to_blue, 0]` — the
/// exact packing [`code_to_multipliers`] unpacks.
#[must_use]
pub(crate) fn forward(argb: &[u32], width: u32, height: u32, bits: u32) -> (Vec<u32>, Vec<u32>) {
    /// Stored red = `(red - delta(green_to_red, green)) & 0xff` — the exact term
    /// [`transform_pixel`] adds back.
    fn stored_red(red: u8, green: i8, green_to_red: i8) -> u8 {
        low_u8(i32::from(red) - color_transform_delta(green_to_red, green))
    }

    /// Stored blue = `(blue - delta(green_to_blue, green) - delta(red_to_blue,
    /// (i8)red)) & 0xff`. `red` is the ORIGINAL red, matching the reconstructed
    /// red the inverse feeds to its `red_to_blue` term.
    fn stored_blue(blue: u8, green: i8, red: u8, green_to_blue: i8, red_to_blue: i8) -> u8 {
        let red_i8 = i8::from_le_bytes([red]);
        low_u8(
            i32::from(blue)
                - color_transform_delta(green_to_blue, green)
                - color_transform_delta(red_to_blue, red_i8),
        )
    }

    let tiles_per_row = subsample_size(width, bits) as usize;
    let tiles_per_col = subsample_size(height, bits) as usize;
    let mut tile_data = vec![0u32; tiles_per_row * tiles_per_col];
    let mut stored = vec![0u32; argb.len()];
    if width == 0 || argb.is_empty() {
        return (stored, tile_data);
    }
    let width = width as usize;
    let height = height as usize;
    let tile = 1usize << bits;

    // Reused across tiles — `clear` keeps the capacity, so no per-tile realloc.
    let mut tile_px: Vec<TilePixel> = Vec::new();
    // Reused fixed-axis blue base for the coordinate-descent sweeps (see below).
    let mut blue_base: Vec<i32> = Vec::new();
    // Reused swept-channel gather for `sweep_blue` (invariant across its 256 evals).
    let mut channel: Vec<i8> = Vec::new();

    for ty in 0..tiles_per_col {
        for tx in 0..tiles_per_row {
            let x0 = tx << bits;
            let y0 = ty << bits;
            let x1 = (x0 + tile).min(width);
            let y1 = (y0 + tile).min(height);

            // Gather this tile's pixels once; every sweep and the emit read them.
            tile_px.clear();
            for y in y0..y1 {
                for x in x0..x1 {
                    let idx = y * width + x;
                    let [b, g, r, a] = argb[idx].to_le_bytes();
                    tile_px.push(TilePixel {
                        idx,
                        green: i8::from_le_bytes([g]),
                        r,
                        g,
                        b,
                        a,
                    });
                }
            }

            // Greedy per-axis selection over the tile the multiplier governs.
            let green_to_red = best_multiplier(|m| {
                let mut sum = 0u64;
                for px in &tile_px {
                    sum += u64::from(residual_magnitude(stored_red(px.r, px.green, m)));
                }
                sum
            });
            // Blue depends on BOTH multipliers, so descend on the coupled pair; each
            // sweep holds one axis fixed, hoisting its per-pixel delta into
            // `blue_base` so the 256-value sweep adds only the swept axis's delta.
            // Round 1 reproduces the historical passes: g2b with r2b=0, then r2b.
            fill_blue_base(&mut blue_base, &tile_px, true, 0); // hold red_to_blue = 0
            let mut green_to_blue = sweep_blue(&tile_px, &blue_base, false, &mut channel);
            fill_blue_base(&mut blue_base, &tile_px, false, green_to_blue); // hold g2b
            let mut red_to_blue = sweep_blue(&tile_px, &blue_base, true, &mut channel);
            for _ in 1..MAX_CC_REFINE_ROUNDS {
                fill_blue_base(&mut blue_base, &tile_px, true, red_to_blue); // hold r2b
                let g2b = sweep_blue(&tile_px, &blue_base, false, &mut channel);
                fill_blue_base(&mut blue_base, &tile_px, false, g2b); // hold g2b
                let r2b = sweep_blue(&tile_px, &blue_base, true, &mut channel);
                if g2b == green_to_blue && r2b == red_to_blue {
                    break;
                }
                green_to_blue = g2b;
                red_to_blue = r2b;
            }

            // Emit the chosen deltas for every pixel in the tile.
            for px in &tile_px {
                let sr = stored_red(px.r, px.green, green_to_red);
                let sb = stored_blue(px.b, px.green, px.r, green_to_blue, red_to_blue);
                stored[px.idx] = u32::from_le_bytes([sb, px.g, sr, px.a]);
            }

            // Pack [green_to_red, green_to_blue, red_to_blue, 0] exactly as
            // `code_to_multipliers` unpacks it (i8 -> u8 reinterpret).
            tile_data[ty * tiles_per_row + tx] = u32::from_le_bytes([
                green_to_red.to_le_bytes()[0],
                green_to_blue.to_le_bytes()[0],
                red_to_blue.to_le_bytes()[0],
                0,
            ]);
        }
    }
    (stored, tile_data)
}

#[cfg(test)]
mod tests {
    use super::{
        TilePixel, code_to_multipliers, color_transform_delta, forward, inverse, inverse_row,
        inverse_row_reference, sweep_blue, sweep_blue_reference, transform_pixel,
    };
    use proptest::prelude::*;

    #[test]
    fn color_transform_delta_uses_arithmetic_shift() {
        // (-1 * 100) >> 5 == (-100) >> 5 == -4 (floor toward -inf), proving the
        // shift is arithmetic rather than logical.
        assert_eq!(color_transform_delta(-1, 100), -4);
        // (-2 * 100) >> 5 == (-200) >> 5 == -7 (floor(-6.25)).
        assert_eq!(color_transform_delta(-2, 100), -7);
        // Positive control: (2 * 100) >> 5 == 200 >> 5 == 6.
        assert_eq!(color_transform_delta(2, 100), 6);
    }

    #[test]
    fn code_to_multipliers_sign_extends_high_bytes() {
        // Bytes (LE) [0xB0, 0xA0, 0x90, _] -> -80, -96, -112 as i8.
        let m = code_to_multipliers(0x0090_A0B0);
        assert_eq!(m.green_to_red, -80);
        assert_eq!(m.green_to_blue, -96);
        assert_eq!(m.red_to_blue, -112);
    }

    #[test]
    fn transform_pixel_masks_red_before_feeding_red_to_blue() {
        // multipliers: green_to_red=32, green_to_blue=0, red_to_blue=16.
        let m = code_to_multipliers(0x0010_0020);
        // Pixel [b,g,r,a] = [0x00, 0x40, 0xF0, 0xFF]; green = 64.
        //   new_red  = 0xF0 + ((32*64)>>5)=240+64 = 304 -> &0xff = 48 (0x30).
        //   red_to_blue uses the UPDATED, masked red: (int8_t)48 = 48.
        //   new_blue = 0 + 0 + ((16*48)>>5) = 24 (0x18).
        // Had the original red (240 -> (int8_t) = -16) been used instead, blue
        // would be (16*-16)>>5 = -8 -> 0xF8, so this value pins the ordering.
        assert_eq!(transform_pixel(0xFFF0_4000, &m), 0xFF30_4018);
    }

    #[test]
    fn inverse_applies_per_tile_multipliers() {
        // width=4, bits=1 -> 2 tiles per row; tile 0 = identity (code 0),
        // tile 1 = code 0x0010_0020 (green_to_red=32, red_to_blue=16).
        let mut argb = [0x1234_5678, 0x89AB_CDEF, 0xFFF0_4000, 0xFF10_1000];
        let tile_data = [0x0000_0000, 0x0010_0020];
        inverse(&mut argb, 4, 1, &tile_data);
        assert_eq!(
            argb,
            // Pixels 0,1 (tile 0) untouched; pixels 2,3 (tile 1) transformed.
            [0x1234_5678, 0x89AB_CDEF, 0xFF30_4018, 0xFF20_1010]
        );
    }

    #[test]
    fn whole_buffer_inverse_equals_looping_inverse_row() {
        // 4x3 image, bits=1 -> a 2x2 tile grid; a distinct multiplier code per
        // tile so both the tile-row (`y >> bits`) and tile-column selection are
        // exercised. Feeding rows one at a time through `inverse_row` must match
        // the whole-buffer inverse.
        let width = 4u32;
        let bits = 1u32;
        let tile_data = [0x0000_0000u32, 0x0010_0020, 0x0030_0040, 0x0005_0006];
        let coded: Vec<u32> = (0..12u32).map(|i| i.wrapping_mul(0x0102_0305)).collect();

        let mut batch = coded.clone();
        inverse(&mut batch, width, bits, &tile_data);

        let w = width as usize;
        let mut rows = Vec::with_capacity(coded.len());
        for (y, row) in coded.chunks(w).enumerate() {
            let mut r = row.to_vec();
            inverse_row(&mut r, y, bits, &tile_data);
            rows.extend_from_slice(&r);
        }
        assert_eq!(batch, rows);
    }

    #[test]
    fn forward_picks_cancelling_multipliers_known_vector() {
        // One 1x1 tile. Pixel [b,g,r,a] = [0x14, 0x20, 0x0a, 0xff]:
        //   green = 32 == 2^5, so delta(m, 32) = (m*32)>>5 = m exactly for any m.
        // green_to_red sweep: stored_red = (10 - m) & 0xff, uniquely zero at m=10.
        // green_to_blue (with red_to_blue=0): stored_blue = (20 - m) & 0xff, zero
        //   uniquely at m=20.
        // red_to_blue given green_to_blue=20: stored_blue = (-(m*10>>5)) & 0xff;
        //   the delta is 0 for m in {0,1,2,3} (cost 0) -> tie broken to m=0.
        // So stored red/blue both cancel to 0; green/alpha pass through.
        let (stored, tile_data) = forward(&[0xff0a_2014], 1, 1, 0);
        assert_eq!(stored, [0xff00_2000]);
        // tile_data bytes (LE) = [g2r=10, g2b=20, r2b=0, 0] = 0x0000_140a.
        assert_eq!(tile_data, [0x0000_140a]);
        // And the chosen multipliers reconstruct the original exactly.
        let mut round = stored;
        inverse(&mut round, 1, 0, &tile_data);
        assert_eq!(round, [0xff0a_2014]);
    }

    /// The pre-scratch `forward` verbatim: it re-reads `argb` (re-striding by
    /// `width`) and re-unpacks every pixel on every multiplier sweep and the
    /// emit. The scratch-gathering [`super::forward`] must return byte-identical
    /// `stored` and `tile_data`; `forward_matches_reference` pins that
    /// mechanically. The `work!(CrossColorEval)` bump is omitted — it is test-only
    /// here and does not affect the returned bytes.
    fn forward_reference(argb: &[u32], width: u32, height: u32, bits: u32) -> (Vec<u32>, Vec<u32>) {
        use super::{color_transform_delta, low_u8};
        use crate::lossless::constants::subsample_size;

        fn residual_magnitude(byte: u8) -> u32 {
            let v = u32::from(byte);
            v.min(256 - v)
        }
        fn stored_red(red: u8, green: i8, green_to_red: i8) -> u8 {
            low_u8(i32::from(red) - color_transform_delta(green_to_red, green))
        }
        fn stored_blue(blue: u8, green: i8, red: u8, green_to_blue: i8, red_to_blue: i8) -> u8 {
            let red_i8 = i8::from_le_bytes([red]);
            low_u8(
                i32::from(blue)
                    - color_transform_delta(green_to_blue, green)
                    - color_transform_delta(red_to_blue, red_i8),
            )
        }
        fn best_multiplier(cost: impl Fn(i8) -> u64) -> i8 {
            let mut best = 0i8;
            let mut best_key = (u64::MAX, u32::MAX, i32::MAX);
            for m in i8::MIN..=i8::MAX {
                let key = (cost(m), i32::from(m).unsigned_abs(), i32::from(m));
                if key < best_key {
                    best_key = key;
                    best = m;
                }
            }
            best
        }

        let tiles_per_row = subsample_size(width, bits) as usize;
        let tiles_per_col = subsample_size(height, bits) as usize;
        let mut tile_data = vec![0u32; tiles_per_row * tiles_per_col];
        let mut stored = vec![0u32; argb.len()];
        if width == 0 || argb.is_empty() {
            return (stored, tile_data);
        }
        let width = width as usize;
        let height = height as usize;
        let tile = 1usize << bits;

        for ty in 0..tiles_per_col {
            for tx in 0..tiles_per_row {
                let x0 = tx << bits;
                let y0 = ty << bits;
                let x1 = (x0 + tile).min(width);
                let y1 = (y0 + tile).min(height);

                let green_to_red = best_multiplier(|m| {
                    let mut sum = 0u64;
                    for y in y0..y1 {
                        for x in x0..x1 {
                            let [_, g, r, _] = argb[y * width + x].to_le_bytes();
                            let green = i8::from_le_bytes([g]);
                            sum += u64::from(residual_magnitude(stored_red(r, green, m)));
                        }
                    }
                    sum
                });
                let cost_blue = |green_to_blue: i8, red_to_blue: i8| -> u64 {
                    let mut sum = 0u64;
                    for y in y0..y1 {
                        for x in x0..x1 {
                            let [b, g, r, _] = argb[y * width + x].to_le_bytes();
                            let green = i8::from_le_bytes([g]);
                            let sb = stored_blue(b, green, r, green_to_blue, red_to_blue);
                            sum += u64::from(residual_magnitude(sb));
                        }
                    }
                    sum
                };
                let mut green_to_blue = best_multiplier(|m| cost_blue(m, 0));
                let mut red_to_blue = best_multiplier(|m| cost_blue(green_to_blue, m));
                for _ in 1..super::MAX_CC_REFINE_ROUNDS {
                    let g2b = best_multiplier(|m| cost_blue(m, red_to_blue));
                    let r2b = best_multiplier(|m| cost_blue(g2b, m));
                    if g2b == green_to_blue && r2b == red_to_blue {
                        break;
                    }
                    green_to_blue = g2b;
                    red_to_blue = r2b;
                }

                for y in y0..y1 {
                    for x in x0..x1 {
                        let idx = y * width + x;
                        let [b, g, r, a] = argb[idx].to_le_bytes();
                        let green = i8::from_le_bytes([g]);
                        let sr = stored_red(r, green, green_to_red);
                        let sb = stored_blue(b, green, r, green_to_blue, red_to_blue);
                        stored[idx] = u32::from_le_bytes([sb, g, sr, a]);
                    }
                }

                tile_data[ty * tiles_per_row + tx] = u32::from_le_bytes([
                    green_to_red.to_le_bytes()[0],
                    green_to_blue.to_le_bytes()[0],
                    red_to_blue.to_le_bytes()[0],
                    0,
                ]);
            }
        }
        (stored, tile_data)
    }

    #[test]
    fn forward_matches_reference_edge_cases() {
        // Enumerated edges: empty, 1x1, tiny widths, solid, palette-like, noise,
        // and alpha present/absent — each must match the reference byte-for-byte.
        let solid = vec![0xff20_2020u32; 64];
        let palette: Vec<u32> = (0..64u32)
            .map(|i| [0xff00_0000, 0xff40_8010, 0x0011_2233][i as usize % 3])
            .collect();
        let noise: Vec<u32> = (0..64u32).map(|i| i.wrapping_mul(0x9E37_79B9)).collect();
        let cases: &[(&[u32], u32, u32)] = &[
            (&[], 0, 0),
            (&[0xff12_3456], 1, 1),
            (&solid[..3], 3, 1),
            (&solid, 8, 8),
            (&palette, 8, 8),
            (&noise, 8, 8),
        ];
        for &(argb, w, h) in cases {
            for bits in 0..=4u32 {
                assert_eq!(
                    forward(argb, w, h, bits),
                    forward_reference(argb, w, h, bits),
                    "mismatch at {w}x{h} bits={bits}"
                );
            }
        }
    }

    proptest! {
        /// The scratch-gathering `forward` returns byte-identical `stored` and
        /// `tile_data` to the pre-scratch [`forward_reference`] across arbitrary
        /// sizes, tile bits, and pixel data — the mechanical proof the perf change
        /// is byte-invariant. Edge cases (1x1, tiny widths, solid, palette-like,
        /// noise, alpha present/absent) fall inside the generated space.
        #[test]
        fn forward_matches_reference(
            (width, height, bits, argb) in (1u32..=8, 1u32..=8, 0u32..=5u32).prop_flat_map(
                |(w, h, bits)| {
                    prop::collection::vec(any::<u32>(), (w as usize) * (h as usize))
                        .prop_map(move |argb| (w, h, bits, argb))
                },
            )
        ) {
            prop_assert_eq!(forward(&argb, width, height, bits), forward_reference(&argb, width, height, bits));
        }

        #[test]
        fn forward_then_inverse_is_identity(
            (width, height, bits, argb) in (1u32..=8, 1u32..=8, 2u32..=5u32).prop_flat_map(
                |(w, h, bits)| {
                    prop::collection::vec(any::<u32>(), (w as usize) * (h as usize))
                        .prop_map(move |argb| (w, h, bits, argb))
                },
            )
        ) {
            let (stored, tile_data) = forward(&argb, width, height, bits);
            // `inverse` reconstructs in place, so round-trip an owned copy of the
            // stored channels back to the untouched original.
            let mut round = stored;
            inverse(&mut round, width, bits, &tile_data);
            prop_assert_eq!(round, argb);
        }

        /// The channel-gathering `sweep_blue` returns the identical multiplier to the
        /// pre-scratch [`sweep_blue_reference`] over random tiles, both swept axes,
        /// and a realistic `blue_base` range — a direct localized proof to complement
        /// the end-to-end `forward_matches_reference`. `blue_base` is bounded to the
        /// `i32::from(px.b) - held_delta` range the caller actually passes (roughly
        /// `-512..=767`), well clear of the `base - delta` debug-overflow both forms
        /// would share.
        #[test]
        fn sweep_blue_matches_reference(
            (green, red, blue_base) in (1usize..=64).prop_flat_map(|n| {
                (
                    prop::collection::vec(any::<i8>(), n),
                    prop::collection::vec(any::<u8>(), n),
                    prop::collection::vec(-2048i32..=2048, n),
                )
            }),
            sweep_red in any::<bool>(),
        ) {
            let tile: Vec<TilePixel> = green
                .iter()
                .zip(&red)
                .map(|(&green, &r)| TilePixel { idx: 0, green, r, g: 0, b: 0, a: 0 })
                .collect();
            let mut channel = Vec::new();
            prop_assert_eq!(
                sweep_blue(&tile, &blue_base, sweep_red, &mut channel),
                sweep_blue_reference(&tile, &blue_base, sweep_red),
            );
        }

        /// The run-chunked `inverse_row` reproduces the pre-hoist
        /// [`inverse_row_reference`] byte for byte over random rows, tile-bits, row
        /// indices and tile-code tables (both forms read `tile_data` via the same
        /// `.get().unwrap_or(0)`, so a short table is handled identically). A single
        /// differing pixel would break the decoded image.
        #[test]
        fn inverse_row_matches_reference(
            row in prop::collection::vec(any::<u32>(), 1usize..=64),
            y in 0usize..=64,
            bits in 0u32..=5,
            tile_data in prop::collection::vec(any::<u32>(), 0usize..=32),
        ) {
            let mut opt = row.clone();
            let mut reference = row;
            inverse_row(&mut opt, y, bits, &tile_data);
            inverse_row_reference(&mut reference, y, bits, &tile_data);
            prop_assert_eq!(opt, reference);
        }
    }

    #[test]
    fn forward_early_return_needs_or_not_and() {
        // Inconsistent request: `argb` is empty while width/height are non-zero.
        // The guard is `width == 0 || argb.is_empty()`, so the empty buffer must
        // short-circuit the early return and yield zeroed outputs. If the `||`
        // became `&&`, the guard would be false (width != 0), the tile loop would
        // run and index `argb[0]` on the empty slice, panicking. Pin the exact
        // early-return outputs so the `|| -> &&` mutant is caught by that panic.
        let (stored, tile_data) = forward(&[], 1, 1, 0);
        assert_eq!(stored, Vec::<u32>::new());
        // subsample_size(1, 0) == 1 tile in each axis -> one zeroed tile code.
        assert_eq!(tile_data, vec![0u32; 1]);
    }

    #[cfg(feature = "bench")]
    #[test]
    fn bench_sweep_blue_wrappers_return_swept_multiplier() {
        use super::{bench_sweep_blue, bench_sweep_blue_reference};
        // green == 32 makes delta(m, 32) == (m*32)>>5 == m exactly, so with a
        // single pixel and blue_base == 40 the stored-blue residual is |40 - m|,
        // uniquely minimized to 0 at m == 40. sweep_red == false selects the green
        // channel (so `red` is unused). The bench drivers must return that 40,
        // which is distinct from the 0 / -1 / 1 the "-> i8 with N" mutants inject.
        let green = [32i8];
        let red = [0u8];
        let blue_base = [40i32];
        let mut channel = Vec::new();
        assert_eq!(
            bench_sweep_blue(&green, &red, &blue_base, false, &mut channel),
            40
        );
        assert_eq!(
            bench_sweep_blue_reference(&green, &red, &blue_base, false),
            40
        );
    }
}
