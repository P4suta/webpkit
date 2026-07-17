//! Near-lossless preprocessing — a lossy encode-side filter that snaps the low
//! bits of pixels in busy regions to a coarser grid, trading a bounded per-channel
//! error for a smaller VP8L payload. Ports libwebp `near_lossless.c`.
//!
//! It is a pure ARGB-to-ARGB pass applied before the transform search; the
//! bitstream is untouched, so the result still round-trips exactly (the decoder
//! reproduces the quantized pixels). Only pixels whose 4-connected neighborhood is
//! *not* already smooth are quantized, so flat gradients survive verbatim while
//! noisy detail collapses onto multiples of `1 << bits`.

use crate::lossless::prelude::*;

/// Images with both dimensions below this get no near-lossless pass: on a small
/// icon the neighborhood test has too little context to tell detail from an edge,
/// so quantizing it only adds error. Matches libwebp `MIN_DIM_FOR_NEAR_LOSSLESS`.
const MIN_DIM_FOR_NEAR_LOSSLESS: usize = 64;

/// Map a near-lossless level (`0..=100`, higher = weaker) to the number of low
/// bits the strongest pass may shave: `100` (or above) disables it, `80..=99`
/// shaves 1, down to `0..=19` shaving 5. Ports libwebp `VP8LNearLosslessBits`.
const fn near_lossless_bits(level: u8) -> u32 {
    let level = level as u32;
    if level >= 100 { 0 } else { 5 - level / 20 }
}

/// Quantize one 8-bit channel value to the closest multiple of `1 << bits` (or to
/// `255`), biasing ties toward the value with more trailing zeros. Ports libwebp
/// `FindClosestDiscretized`; `bits` is always `>= 1` here.
const fn find_closest_discretized(value: u32, bits: u32) -> u32 {
    let mask = (1u32 << bits) - 1;
    let biased = value + (mask >> 1) + ((value >> bits) & 1);
    if biased > 0xff { 0xff } else { biased & !mask }
}

/// Apply [`find_closest_discretized`] to all four ARGB channels of `pixel`. Ports
/// libwebp `ClosestDiscretizedArgb`.
const fn closest_discretized_argb(pixel: u32, bits: u32) -> u32 {
    let mut out = 0u32;
    let mut i = 0;
    while i < 4 {
        let channel = (pixel >> (i * 8)) & 0xff;
        out |= find_closest_discretized(channel, bits) << (i * 8);
        i += 1;
    }
    out
}

/// Whether every channel of `a` and `b` differs by less than `limit`. Ports
/// libwebp `IsNear` (`|diff| < limit`, expressed here as an unsigned `abs_diff`).
fn is_near(a: u32, b: u32, limit: u32) -> bool {
    (0..4).all(|i| {
        let ca = (a >> (i * 8)) & 0xff;
        let cb = (b >> (i * 8)) & 0xff;
        ca.abs_diff(cb) < limit
    })
}

/// Whether the pixel at flat index `i` (an interior pixel, so all four neighbors
/// exist) is smooth: near each of its left/right/up/down neighbors. Ports libwebp
/// `IsSmooth`.
fn is_smooth(src: &[u32], width: usize, i: usize, limit: u32) -> bool {
    is_near(src[i], src[i - 1], limit)
        && is_near(src[i], src[i + 1], limit)
        && is_near(src[i], src[i - width], limit)
        && is_near(src[i], src[i + width], limit)
}

/// One near-lossless pass at `bits`: copy `src`, then quantize every interior,
/// non-smooth pixel to the discretized grid. Border rows/columns are left exact.
/// Ports libwebp `NearLossless`; reading the smoothness test from the untouched
/// `src` (not the output) is why libwebp's rolling 3-row copy buffer is unneeded.
fn quantize_pass(width: usize, height: usize, src: &[u32], bits: u32) -> Vec<u32> {
    let limit = 1u32 << bits;
    let mut dst = src.to_vec();
    for y in 1..height.saturating_sub(1) {
        for x in 1..width.saturating_sub(1) {
            let i = y * width + x;
            if !is_smooth(src, width, i, limit) {
                dst[i] = closest_discretized_argb(src[i], bits);
            }
        }
    }
    dst
}

/// Apply near-lossless preprocessing to `argb` (row-major, `width * height`) in
/// place at the given `level` (`0..=100`, lower = stronger).
///
/// A no-op when `level >= 100`, or for a small image (both dimensions below
/// [`MIN_DIM_FOR_NEAR_LOSSLESS`], or fewer than three rows). Otherwise it runs one
/// pass at the full bit budget then successively weaker passes (`bits - 1 .. 1`),
/// each reading the previous output — so the per-channel error stays within
/// `(1 << bits) - 1` (the sum of the per-pass `1 << (b - 1)` bounds). Ports libwebp
/// `VP8ApplyNearLossless`.
pub(crate) fn apply(argb: &mut [u32], width: u32, height: u32, level: u8) {
    let bits = near_lossless_bits(level);
    if bits == 0 {
        return;
    }
    let width = width as usize;
    let height = height as usize;
    if (width < MIN_DIM_FOR_NEAR_LOSSLESS && height < MIN_DIM_FOR_NEAR_LOSSLESS) || height < 3 {
        return;
    }
    let mut current = quantize_pass(width, height, argb, bits);
    for pass_bits in (1..bits).rev() {
        current = quantize_pass(width, height, &current, pass_bits);
    }
    argb.copy_from_slice(&current);
}

/// The provable per-channel error bound after [`apply`] at `level` — the value
/// `(1 << bits) - 1`, or zero when the pass is disabled. Exposed for the round-trip
/// tests that assert the quantization never exceeds it.
#[cfg(test)]
pub(crate) const fn error_bound(level: u8) -> u32 {
    (1u32 << near_lossless_bits(level)) - 1
}

#[cfg(test)]
mod tests {
    use super::{
        apply, closest_discretized_argb, error_bound, find_closest_discretized, is_near,
        near_lossless_bits,
    };
    use proptest::prelude::*;

    #[test]
    fn bits_follow_the_quality_ramp() {
        // The libwebp mapping: 100 disables, then 20-wide bands shave one more bit.
        assert_eq!(near_lossless_bits(100), 0);
        assert_eq!(near_lossless_bits(99), 1);
        assert_eq!(near_lossless_bits(80), 1);
        assert_eq!(near_lossless_bits(79), 2);
        assert_eq!(near_lossless_bits(40), 3);
        assert_eq!(near_lossless_bits(20), 4);
        assert_eq!(near_lossless_bits(0), 5);
    }

    #[test]
    fn discretized_snaps_to_the_grid_and_clamps_255() {
        // bits = 3 -> grid of 8. 250 rounds to 248 (nearest multiple), and a value
        // that would round to 256 clamps to 255.
        assert_eq!(find_closest_discretized(250, 3), 248);
        assert_eq!(find_closest_discretized(255, 3), 255);
        // Opaque alpha (255) must survive every bit budget so opaque stays opaque.
        for bits in 1..=5 {
            assert_eq!(find_closest_discretized(255, bits), 255);
        }
    }

    #[test]
    fn discretized_argb_processes_every_lane() {
        // Each lane is snapped independently; bits = 4 -> grid of 16.
        // 0x37 -> 0x30, 0x88 -> 0x80, 0xC1 -> 0xC0, 0xFF -> 0xFF.
        assert_eq!(closest_discretized_argb(0xFFC1_8837, 4), 0xFFC0_8030);
    }

    #[test]
    fn is_near_is_per_channel_abs_diff() {
        // Differences of exactly `limit` are not near; strictly-less-than are.
        assert!(is_near(0x0000_0000, 0x0000_0003, 4));
        assert!(!is_near(0x0000_0000, 0x0000_0004, 4));
        // A single out-of-band channel makes the whole pixel not near.
        assert!(!is_near(0x0000_0000, 0x0400_0000, 4));
    }

    #[test]
    fn small_image_is_left_untouched() {
        // Both dimensions below 64 -> no pass, even at the strongest level.
        let orig: Vec<u32> = (0..16u32)
            .map(|i| 0xFF00_0000 | (i * 0x0011_1111))
            .collect();
        let mut px = orig.clone();
        apply(&mut px, 4, 4, 0);
        assert_eq!(px, orig);
    }

    #[test]
    fn level_100_is_a_no_op_on_a_large_image() {
        let orig: Vec<u32> = (0..64 * 4u32)
            .map(|i| 0xFF00_0000 | i.wrapping_mul(2_654_435_761))
            .collect();
        let mut px = orig.clone();
        apply(&mut px, 64, 4, 100);
        assert_eq!(px, orig);
    }

    /// Deterministic LCG ARGB stream (numerical-recipes constants) for sweeps, with
    /// alpha forced opaque so the corpus stays a realistic still image.
    fn lcg_argb(seed: u32, count: usize) -> Vec<u32> {
        let mut state = seed;
        (0..count)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                0xFF00_0000 | (state & 0x00FF_FFFF)
            })
            .collect()
    }

    proptest! {
        #[test]
        fn per_channel_error_within_bound(seed in any::<u32>(), level in 0u8..=100) {
            // A 64-wide image clears the small-image guard, so the pass actually
            // runs; every channel must stay within the theoretical bound.
            let (width, height) = (64u32, 4u32);
            let orig = lcg_argb(seed, (width * height) as usize);
            let mut px = orig.clone();
            apply(&mut px, width, height, level);
            let bound = error_bound(level);
            for (o, q) in orig.iter().zip(&px) {
                for i in 0..4 {
                    let co = (o >> (i * 8)) & 0xff;
                    let cq = (q >> (i * 8)) & 0xff;
                    prop_assert!(
                        co.abs_diff(cq) <= bound,
                        "channel {i}: |{co} - {cq}| exceeds bound {bound} at level {level}"
                    );
                }
            }
        }

        #[test]
        fn opaque_alpha_is_preserved(seed in any::<u32>(), level in 0u8..=100) {
            // Near-lossless quantizes all four lanes, but opaque alpha (255) maps to
            // itself under every bit budget, so an opaque image stays opaque.
            let (width, height) = (64u32, 4u32);
            let mut px = lcg_argb(seed, (width * height) as usize);
            apply(&mut px, width, height, level);
            prop_assert!(px.iter().all(|&p| p >> 24 == 0xff));
        }
    }
}
