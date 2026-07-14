//! Property tests over the public decoder surface.
//!
//! The load-bearing robustness property for a decoder is that it **never
//! panics** on hostile input — it must return an [`webpkit::lossy::Error`] instead.
//! These run in the default build (no libwebp); the differential `oracle`
//! properties (our decode == libwebp over random real frames) live in
//! `tests/oracle.rs` behind the `oracle` feature.

use proptest::prelude::*;
use webpkit::lossy::{Dimensions, Effort, ImageRef, LossyConfig, PixelLayout, decode, encode_vp8};
use webpkit_lossy_proptest::{arbitrary_bytes, arbitrary_lossy_rgb, psnr_rgb};

/// Expand interleaved RGB (`w*h*3`) to opaque RGBA for the encoder.
fn rgb_to_rgba(rgb: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rgb.len() / 3 * 4);
    for px in rgb.chunks_exact(3) {
        out.extend_from_slice(&[px[0], px[1], px[2], 0xff]);
    }
    out
}

/// A `w`×`h` RGBA frame from a per-pixel function.
fn frame(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            out.extend_from_slice(&f(x, y));
        }
    }
    out
}

/// A frame `(w, h, rgba, floor)` drawn from an adversarial pixel class, paired with
/// a conservative round-trip PSNR floor that must hold at any quality/method.
///
/// The floors are set well below the empirically measured worst case for each class
/// (a decode that merely preserved the shape sits far above them), so the property
/// catches gross corruption — a garbage decode scores near 0 dB — without flaking on
/// the coarse quantization of low quality. Small frames round-trip *better* (a 1×1
/// frame is near-lossless), so a flat per-class floor already covers every size.
fn adversarial_frame() -> impl Strategy<Value = (u32, u32, Vec<u8>, f64)> {
    let dims = (1u32..=48, 1u32..=48);
    prop_oneof![
        // Coherent gradient blended with noise — the oracle's non-degenerate shape.
        arbitrary_lossy_rgb().prop_map(|(w, h, rgb)| (w, h, rgb_to_rgba(&rgb), 6.0)),
        // A single solid color: a DC-only residual, opaque.
        (dims.clone(), any::<[u8; 3]>())
            .prop_map(|((w, h), [r, g, b])| { (w, h, frame(w, h, |_, _| [r, g, b, 0xff]), 10.0) }),
        // Solid color, fully transparent: alpha is ignored by VP8, so RGB fidelity
        // must match the opaque case (a guard that alpha=0 does not corrupt luma).
        (dims.clone(), any::<[u8; 3]>())
            .prop_map(|((w, h), [r, g, b])| { (w, h, frame(w, h, |_, _| [r, g, b, 0x00]), 10.0) }),
        // Extreme-contrast 2×2 checkerboard of a color and its channel-wise
        // complement — the highest-frequency, highest-contrast content.
        (dims, any::<[u8; 3]>()).prop_map(|((w, h), [r, g, b])| {
            let rgba = frame(w, h, |x, y| {
                if (x / 2 + y / 2) % 2 == 0 {
                    [r, g, b, 0xff]
                } else {
                    [255 - r, 255 - g, 255 - b, 0xff]
                }
            });
            (w, h, rgba, 8.0)
        }),
    ]
}

proptest! {
    /// `decode` returns an error — never panics, aborts, or hangs — on any
    /// arbitrary byte buffer.
    #[test]
    fn decode_never_panics_on_arbitrary_bytes(bytes in arbitrary_bytes(512)) {
        let _ = webpkit::lossy::decode(&bytes);
    }

    /// `peek_dimensions` is likewise total over arbitrary input.
    #[test]
    fn peek_dimensions_never_panics_on_arbitrary_bytes(bytes in arbitrary_bytes(64)) {
        let _ = webpkit::lossy::peek_dimensions(&bytes);
    }

    /// Encoding an arbitrary small frame at any quality never panics, and its
    /// output decodes back to a picture of the original dimensions.
    #[test]
    fn encode_then_decode_round_trips_dimensions(
        (w, h, rgb) in arbitrary_lossy_rgb(),
        quality in 0u8..=100,
    ) {
        let rgba = rgb_to_rgba(&rgb);
        let dims = Dimensions::new(w, h).unwrap();
        let img = ImageRef::new(dims, PixelLayout::Rgba8, &rgba).unwrap();
        let (_dims, payload) = encode_vp8(img, &LossyConfig::new().with_quality(quality)).unwrap();
        let decoded = decode(&payload).unwrap();
        prop_assert_eq!((decoded.width(), decoded.height()), (w, h));
    }

    /// Encoding is deterministic: the same frame and quality yield identical bytes.
    #[test]
    fn encode_is_deterministic(
        (w, h, rgb) in arbitrary_lossy_rgb(),
        quality in 0u8..=100,
    ) {
        let rgba = rgb_to_rgba(&rgb);
        let dims = Dimensions::new(w, h).unwrap();
        let cfg = LossyConfig::new().with_quality(quality);
        let img = ImageRef::new(dims, PixelLayout::Rgba8, &rgba).unwrap();
        let a = encode_vp8(img, &cfg).unwrap().1;
        let b = encode_vp8(img, &cfg).unwrap().1;
        prop_assert_eq!(a, b);
    }
}

proptest! {
    // Encode is heavy (trellis/mode search at Best), so this fidelity property runs
    // a small case budget; each case picks one quality and one method rather than
    // looping the whole grid.
    #![proptest_config(ProptestConfig { cases: 48, ..ProptestConfig::default() })]

    /// Across adversarial pixel classes — solid, fully transparent, extreme-contrast
    /// checkerboard, and gradient+noise — every quality/method round-trips to a frame
    /// of the source dimensions whose PSNR clears the class floor. This guards against
    /// a gross fidelity regression (a garbage decode scores near 0 dB); the tight,
    /// content-specific floors live in the fixed-image `tests/encode.rs`.
    #[test]
    fn encode_holds_a_fidelity_floor_on_adversarial_frames(
        (w, h, rgba, floor) in adversarial_frame(),
        quality in 0u8..=100,
        method_idx in 0usize..3,
    ) {
        let method = [Effort::Fast, Effort::Balanced, Effort::Best][method_idx];
        let dims = Dimensions::new(w, h).unwrap();
        let img = ImageRef::new(dims, PixelLayout::Rgba8, &rgba).unwrap();
        let cfg = LossyConfig::new().with_quality(quality).with_effort(method);
        let (_dims, payload) = encode_vp8(img, &cfg).unwrap();
        let decoded = decode(&payload).unwrap();
        prop_assert_eq!((decoded.width(), decoded.height()), (w, h));
        let out = decoded.into_pixels();
        let psnr = psnr_rgb(&rgba, &out);
        prop_assert!(
            psnr >= floor,
            "{}x{} q{} {:?}: PSNR {:.2} dB below floor {:.1}",
            w, h, quality, method, psnr, floor
        );
    }
}
