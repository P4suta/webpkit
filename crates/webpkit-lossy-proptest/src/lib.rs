//! Shared `proptest` strategies for exercising the `webpkit-lossy` WebP VP8 (lossy)
//! decoder.
//!
//! These strategies generate decoder *inputs* — the lossy codec has an encoder
//! (`webpkit::lossy::encode_vp8`), but these properties feed the decode and
//! differential paths, not an encode round-trip. Two shapes cover the
//! load-bearing properties:
//!
//! * [`arbitrary_bytes`] — hostile buffers for the "decode never panics"
//!   property (the decoder must return an error, never panic, on any input).
//! * [`arbitrary_lossy_rgb`] — small raw RGB frames for the differential
//!   `oracle` (a consumer encodes them with the reference library and checks
//!   that `webpkit::lossy::decode` agrees with libwebp).
//!
//! The consumer test crates own the decode and oracle assertions; this crate
//! only produces the data and stays dependency-light (`proptest` alone, with no
//! dependency on `webpkit-lossy` or the C reference).
#![forbid(unsafe_code)]

use proptest::collection::vec;
use proptest::prelude::{Just, Strategy, any};

/// Largest side length, in pixels, of a frame from [`arbitrary_lossy_rgb`].
const MAX_SIDE: u32 = 48;

/// Peak signal-to-noise ratio (dB) over the RGB channels of two equal-length
/// RGBA buffers — the fidelity metric shared by the encoder's integration and
/// property tests.
///
/// Lives here (not in `webpkit-lossy`, which forbids floating point) so both the
/// fixed-image `tests/encode.rs` floors and the `tests/proptest.rs` fidelity
/// property measure fidelity through one implementation. Returns `99.0` for
/// byte-identical input (the integer squared-error sum is then below one).
#[must_use]
pub fn psnr_rgb(a: &[u8], b: &[u8]) -> f64 {
    let mut se = 0.0f64;
    let mut n = 0.0f64;
    for (pa, pb) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
        for c in 0..3 {
            let d = f64::from(pa[c]) - f64::from(pb[c]);
            se = d.mul_add(d, se);
            n += 1.0;
        }
    }
    if se < 1.0 {
        return 99.0; // identical (the squared-error sum is integer-valued)
    }
    10.0 * (255.0 * 255.0 / (se / n)).log10()
}

/// A strategy producing arbitrary byte buffers of up to `max_len` bytes,
/// suitable as hostile decoder input.
///
/// The decoder's load-bearing safety property is that it returns an error —
/// never panics — on any such buffer; consumer tests feed these straight to
/// `webpkit::lossy::decode`.
pub fn arbitrary_bytes(max_len: usize) -> impl Strategy<Value = Vec<u8>> {
    vec(any::<u8>(), 0..=max_len)
}

/// A strategy producing a small raw RGB frame `(width, height, rgb)` where both
/// dimensions lie in `1..=48` and `rgb.len() == width * height * 3`.
///
/// The pixel content is a per-channel positional gradient blended with drawn
/// noise, so every frame mixes the large
/// low-frequency regions a lossy encoder favors with high-frequency detail —
/// never a degenerate flat color. The differential oracle encodes these frames
/// with the reference library and checks that `webpkit::lossy::decode` agrees with
/// libwebp.
pub fn arbitrary_lossy_rgb() -> impl Strategy<Value = (u32, u32, Vec<u8>)> {
    (1u32..=MAX_SIDE, 1u32..=MAX_SIDE).prop_flat_map(|(width, height)| {
        // width, height <= 48, so width * height * 3 <= 6912 and neither the u32
        // product nor its usize form can overflow; the conversion never fails.
        let len = usize::try_from(width * height * 3).unwrap_or_default();
        (Just(width), Just(height), vec(any::<u8>(), len..=len)).prop_map(
            |(width, height, noise)| {
                // width, height <= 48 fit in u8; the saturating fallback is
                // unreachable and only satisfies the fallible conversion.
                let cols = u8::try_from(width).unwrap_or(u8::MAX);
                let rows = u8::try_from(height).unwrap_or(u8::MAX);
                (width, height, gradient_noise_mix(cols, rows, &noise))
            },
        )
    })
}

/// Blend a smooth positional gradient with `noise` (three bytes per pixel, in
/// row-major order) into a `cols`×`rows` RGB frame that is neither a flat color
/// nor pure white noise. Integer-only and free of widening casts, so the output
/// is bit-deterministic across platforms.
fn gradient_noise_mix(cols: u8, rows: u8, noise: &[u8]) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(noise.len());
    let mut pixels = noise.chunks_exact(3);
    for y in 0..rows {
        for x in 0..cols {
            // Coherent ramps: red across x, green across y, blue across both.
            let base = [
                x.wrapping_mul(5),
                y.wrapping_mul(5),
                x.wrapping_add(y).wrapping_mul(3),
            ];
            // Exactly `cols * rows` chunks exist, so `next` is always `Some`;
            // the empty fallback is unreachable and keeps the code panic-free.
            let drawn = pixels.next().unwrap_or(&[0, 0, 0]);
            for (&base, &drawn) in base.iter().zip(drawn) {
                // Overflow-safe floor average of two bytes, no widening cast:
                // (a & b) + ((a ^ b) >> 1) == (a + b) / 2 without overflow.
                rgb.push((base & drawn) + ((base ^ drawn) >> 1));
            }
        }
    }
    rgb
}

#[cfg(test)]
mod tests {
    use proptest::{prop_assert, prop_assert_eq, proptest};

    use super::{arbitrary_bytes, arbitrary_lossy_rgb};

    proptest! {
        /// Hostile buffers never exceed the requested cap.
        #[test]
        fn arbitrary_bytes_respects_max_len(bytes in arbitrary_bytes(64)) {
            prop_assert!(bytes.len() <= 64);
        }

        /// The generated RGB buffer is exactly `width * height * 3` bytes, and
        /// the dimensions stay within the documented `1..=48` window.
        #[test]
        fn lossy_rgb_length_matches_dimensions((width, height, rgb) in arbitrary_lossy_rgb()) {
            prop_assert!((1..=48).contains(&width));
            prop_assert!((1..=48).contains(&height));
            let expected = usize::try_from(width * height * 3).unwrap();
            prop_assert_eq!(rgb.len(), expected);
        }
    }
}
