//! Reversible VP8L transforms as pure pixel math (no bitstream coupling). Each
//! submodule co-locates the forward (encode, later) and inverse (decode) of one
//! transform; layer-1 primitives depending only on `crate::lossless::constants`.

pub(crate) mod cross_color;
pub(crate) mod near_lossless;
pub(crate) mod palette;
pub(crate) mod predictor;
pub(crate) mod subtract_green;

/// Per-channel wrapping ARGB add (libwebp `AddPixelsEq`): each 8-bit lane adds
/// mod 256. Shared by predictor residual reconstruction and palette color-map
/// expansion.
#[must_use]
pub(crate) const fn add_pixels(a: u32, b: u32) -> u32 {
    let alpha_green = (a & 0xff00_ff00).wrapping_add(b & 0xff00_ff00);
    let red_blue = (a & 0x00ff_00ff).wrapping_add(b & 0x00ff_00ff);
    (alpha_green & 0xff00_ff00) | (red_blue & 0x00ff_00ff)
}

/// Per-channel wrapping ARGB subtract: the exact inverse of [`add_pixels`], with
/// each 8-bit lane subtracted mod 256. Used by palette color-map deltas and
/// predictor residual formation.
///
/// Implemented as an independent per-byte `wrapping_sub` rather than the
/// split-mask SWAR of [`add_pixels`]: a subtraction *borrow* propagates upward
/// across the zeroed guard byte into the next lane (a green underflow would
/// corrupt alpha), so the two-lane mask trick is not reversible here. Operating
/// on each byte in isolation makes this the provably exact inverse.
#[must_use]
pub(crate) const fn sub_pixels(a: u32, b: u32) -> u32 {
    let [a0, a1, a2, a3] = a.to_le_bytes();
    let [b0, b1, b2, b3] = b.to_le_bytes();
    u32::from_le_bytes([
        a0.wrapping_sub(b0),
        a1.wrapping_sub(b1),
        a2.wrapping_sub(b2),
        a3.wrapping_sub(b3),
    ])
}

#[cfg(test)]
mod tests {
    use super::{add_pixels, sub_pixels};
    use proptest::prelude::*;

    #[test]
    fn add_pixels_adds_each_lane() {
        assert_eq!(add_pixels(0x0102_0304, 0x1020_3040), 0x1122_3344);
    }

    #[test]
    fn add_pixels_wraps_per_lane() {
        // Each lane 0xff + 0xff = 0x1fe -> 0xfe (mod 256); must not panic under
        // overflow-checks.
        assert_eq!(add_pixels(0xff00_ff00, 0xff00_ff00), 0xfe00_fe00);
    }

    #[test]
    fn sub_pixels_subtracts_each_lane() {
        assert_eq!(sub_pixels(0x1122_3344, 0x1020_3040), 0x0102_0304);
    }

    #[test]
    fn sub_pixels_wraps_per_lane_without_borrow_leak() {
        // A=0xff R=0x10 G=0x02 B=0x20 minus a b whose green (0x05) exceeds a's
        // green (0x02): the green lane underflows to 0xfd. A per-byte subtract
        // keeps the borrow inside the green byte, so alpha stays 0xff. A naive
        // split-mask SWAR would let the borrow leak up and clobber alpha.
        let a = 0xff10_0220;
        let b = 0x0000_0500;
        let sub = sub_pixels(a, b);
        assert_eq!(sub, 0xff10_fd20);
        // Alpha byte survives the green underflow untouched.
        assert_eq!(sub >> 24, 0xff);
        // ...and add_pixels reverses it exactly, recovering the original alpha.
        assert_eq!(add_pixels(sub, b), a);
    }

    #[test]
    fn add_pixels_recovers_representative_vectors() {
        // Includes the green-underflow-with-opaque-alpha case: green < b.green,
        // alpha = 0xff, which must round-trip without disturbing alpha.
        let cases = [
            (0x0000_0000_u32, 0x0000_0000_u32),
            (0xffff_ffff, 0xffff_ffff),
            (0x8040_2010, 0x0102_0408),
            (0xff10_0220, 0x0000_0500), // green underflow, opaque alpha
            (0xff00_00ff, 0x00ff_ff00), // every lane crosses a byte boundary
        ];
        for (a, b) in cases {
            assert_eq!(
                add_pixels(sub_pixels(a, b), b),
                a,
                "a={a:#010x} b={b:#010x}"
            );
        }
    }

    proptest! {
        #[test]
        fn sub_pixels_is_inverse_of_add_pixels(a in any::<u32>(), b in any::<u32>()) {
            prop_assert_eq!(add_pixels(sub_pixels(a, b), b), a);
        }
    }
}
