//! Subtract-green transform.
//!
//! `forward` ports libwebp `SubtractGreenFromBlueAndRed_C`; `inverse` ports
//! `AddGreenToBlueAndRed_C`.

/// Subtract-green forward: subtract the green channel from red and blue.
///
/// Green and alpha are left unchanged. Each channel is subtracted independently
/// because a combined-lane `wrapping_sub` over `0x00ff_00ff` would let a borrow
/// out of blue propagate up into red and corrupt it.
pub(crate) fn forward(pixels: &mut [u32]) {
    for pixel in pixels.iter_mut() {
        let green = (*pixel >> 8) & 0xff;
        let red = ((*pixel >> 16) & 0xff).wrapping_sub(green) & 0xff;
        let blue = (*pixel & 0xff).wrapping_sub(green) & 0xff;
        *pixel = (*pixel & 0xff00_ff00) | (red << 16) | blue;
    }
}

/// Subtract-green inverse: add the green channel back into red and blue.
///
/// Pointwise, so the whole-buffer inverse is exactly [`inverse_row`] applied to
/// the entire buffer; it delegates to it so the batch and row-streaming forms
/// can never drift.
pub(crate) fn inverse(pixels: &mut [u32]) {
    inverse_row(pixels);
}

/// Subtract-green inverse over a single row — the row-streaming counterpart of
/// the whole-buffer [`inverse`]. Because the transform is pointwise, looping
/// this over every row reproduces [`inverse`] byte-for-byte (proven in tests).
pub(crate) fn inverse_row(row: &mut [u32]) {
    for pixel in row.iter_mut() {
        let green = (*pixel >> 8) & 0xff;
        let red_blue = (*pixel & 0x00ff_00ff).wrapping_add((green << 16) | green) & 0x00ff_00ff;
        *pixel = (*pixel & 0xff00_ff00) | red_blue;
    }
}

#[cfg(test)]
mod tests {
    use super::{forward, inverse, inverse_row};
    use proptest::prelude::*;

    #[test]
    fn inverse_adds_green_to_red_and_blue_with_wrap() {
        // Pixel 0: A=0x00 R=0x10 G=0x02 B=0x20 -> R=0x12, B=0x22, green untouched.
        // Pixel 1: A=0xff R=0xff G=0x03 B=0xfe -> R=0x02 (0xff+3 wrap), B=0x01.
        let mut px = [0x0010_0220, 0xffff_03fe];
        inverse(&mut px);
        assert_eq!(px, [0x0012_0222, 0xff02_0301]);
    }

    #[test]
    fn forward_matches_known_vector() {
        // Visible ARGB A=255, R=110, G=100, B=120 -> stored R=10, G=100, B=20.
        let mut px = [0xff6e_6478];
        forward(&mut px);
        assert_eq!(px, [0xff0a_6414]);
    }

    #[test]
    fn forward_does_not_propagate_borrow_from_blue_to_red() {
        // Visible A=0, R=5, G=1, B=0. Per-channel subtract gives R=4, B=0xff; a
        // combined-lane subtract would leak the blue borrow up and give R=3.
        let mut px = [0x0005_0100];
        forward(&mut px);
        assert_eq!(px, [0x0004_01ff]);
    }

    /// Deterministic LCG pixel stream (numerical-recipes constants) for sweeps.
    fn lcg_pixels(seed: u32) -> Vec<u32> {
        let mut state = seed;
        (0..1024)
            .map(|_| {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                state
            })
            .collect()
    }

    #[test]
    fn forward_then_inverse_is_identity_over_lcg() {
        let original = lcg_pixels(0x1234_5678);
        let mut px = original.clone();
        forward(&mut px);
        inverse(&mut px);
        assert_eq!(px, original);
    }

    #[test]
    fn whole_buffer_inverse_equals_looping_inverse_row() {
        // The whole-buffer inverse must equal feeding it one row at a time
        // through `inverse_row` (the row-streaming decoder's per-row payout).
        let coded = lcg_pixels(0x0bad_cafe);
        let width = 7usize;
        let usable = coded.len() / width * width;
        let coded = &coded[..usable];

        let mut batch = coded.to_vec();
        inverse(&mut batch);

        let mut rows = coded.to_vec();
        for row in rows.chunks_mut(width) {
            inverse_row(row);
        }
        assert_eq!(batch, rows);
    }

    #[test]
    fn inverse_then_forward_is_identity_over_lcg() {
        let original = lcg_pixels(0x9e37_79b9);
        let mut px = original.clone();
        inverse(&mut px);
        forward(&mut px);
        assert_eq!(px, original);
    }

    proptest! {
        #[test]
        fn forward_then_inverse_is_identity(
            pixels in prop::collection::vec(any::<u32>(), 0..256)
        ) {
            let mut px = pixels.clone();
            forward(&mut px);
            inverse(&mut px);
            prop_assert_eq!(px, pixels);
        }
    }
}
