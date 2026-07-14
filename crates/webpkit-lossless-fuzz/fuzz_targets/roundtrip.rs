//! Fuzz target: an exact lossless round-trip through the webpkit-lossless encoder/decoder.
//!
//! The first two bytes pick a width and height (each `1..=64`); the following
//! bytes are the RGBA pixels. Encoding then decoding that image must reproduce
//! it byte-for-byte — the core lossless invariant, checked over the whole input
//! space rather than the handful of proptest images. Inert under a normal build.
#![cfg_attr(feature = "fuzzing", no_main)]

#[cfg(feature = "fuzzing")]
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    // Decoding arbitrary bytes must never panic.
    let _ = webpkit::lossless::decode(data);

    // Derive an arbitrary small image from the input and round-trip it.
    if data.len() < 2 {
        return;
    }
    let width = u32::from(data[0]) % 64 + 1;
    let height = u32::from(data[1]) % 64 + 1;
    let needed = (width * height * 4) as usize;
    let body = &data[2..];
    if body.len() < needed {
        return;
    }
    let rgba = &body[..needed];

    let dims =
        webpkit::lossless::Dimensions::new(width, height).expect("1..=64 is a valid dimension");
    let image = webpkit::lossless::ImageRef::new(dims, webpkit::lossless::PixelLayout::Rgba8, rgba)
        .expect("buffer length matches width*height*4");
    let webp = webpkit::lossless::encode(image, &webpkit::lossless::EncoderConfig::default())
        .expect("encode is infallible");

    // Our own encoder output must decode back to exactly the source image.
    let (out_dims, out_rgba) = webpkit::lossless::decode_rgba(&webp)
        .expect("webpkit::lossless must decode its own output");
    assert_eq!(out_dims, dims, "round-trip changed the dimensions");
    assert!(out_rgba == rgba, "round-trip was not lossless");

    // Also round-trip through Effort::Best (Tier 3: predictor / cross-color /
    // palette / meta-Huffman), so the whole encode surface — not just Balanced —
    // is fuzzed over the full input space.
    let best_config =
        webpkit::lossless::EncoderConfig::default().with_effort(webpkit::lossless::Effort::Best);
    let best = webpkit::lossless::encode(image, &best_config).expect("encode is infallible");
    let (best_dims, best_rgba) = webpkit::lossless::decode_rgba(&best)
        .expect("webpkit::lossless must decode its own Best output");
    assert_eq!(best_dims, dims, "Best round-trip changed the dimensions");
    assert!(best_rgba == rgba, "Best round-trip was not lossless");

    // And through Effort::Fast (literal + subtract-green only, LZ77 / color-cache
    // search skipped) so the fast path is fuzzed over the full input space too.
    let fast_config =
        webpkit::lossless::EncoderConfig::default().with_effort(webpkit::lossless::Effort::Fast);
    let fast = webpkit::lossless::encode(image, &fast_config).expect("encode is infallible");
    let (fast_dims, fast_rgba) = webpkit::lossless::decode_rgba(&fast)
        .expect("webpkit::lossless must decode its own Fast output");
    assert_eq!(fast_dims, dims, "Fast round-trip changed the dimensions");
    assert!(fast_rgba == rgba, "Fast round-trip was not lossless");
});

#[cfg(not(feature = "fuzzing"))]
fn main() {
    // Inert entry point for non-fuzzing builds.
}
