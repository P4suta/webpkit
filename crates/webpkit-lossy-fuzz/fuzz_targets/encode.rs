//! Fuzz target: encode an arbitrary small RGBA frame with the webpkit-lossy VP8
//! encoder, then decode our own output.
//!
//! The first three bytes pick a width, height (each `1..=64`) and quality; the
//! following bytes are the RGBA pixels. Encoding must never panic, and decoding
//! that output must reproduce a frame of the original dimensions — the encoder's
//! total-and-self-consistent invariant, checked over the whole input space rather
//! than the handful of proptest images.
//!
//! Run: `cargo +nightly fuzz run encode --fuzz-dir crates/webpkit-lossy-fuzz --features fuzzing`.
//! Gated on the `fuzzing` feature (which pulls in `libfuzzer-sys`); a normal
//! `cargo build --workspace` compiles this to an inert binary so the libFuzzer
//! runtime is never linked outside a fuzzing build.
#![cfg_attr(feature = "fuzzing", no_main)]

#[cfg(feature = "fuzzing")]
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    // Derive an arbitrary small image and quality from the input.
    if data.len() < 3 {
        return;
    }
    let width = u32::from(data[0]) % 64 + 1;
    let height = u32::from(data[1]) % 64 + 1;
    let quality = data[2] % 101;
    let needed = (width * height * 4) as usize;
    let body = &data[3..];
    if body.len() < needed {
        return;
    }
    let rgba = &body[..needed];

    let dims = webpkit::lossy::Dimensions::new(width, height).expect("1..=64 is a valid dimension");
    let img = webpkit::lossy::ImageRef::new(dims, webpkit::lossy::PixelLayout::Rgba8, rgba)
        .expect("buffer length matches width*height*4");
    let cfg = webpkit::lossy::LossyConfig::new().with_quality(quality);
    let (_dims, payload) = webpkit::lossy::encode_vp8(img, &cfg).expect("encode is infallible");

    // Our own encoder output must decode back to a frame of the source dimensions.
    let decoded =
        webpkit::lossy::decode(&payload).expect("webpkit-lossy must decode its own output");
    assert_eq!(
        (decoded.width(), decoded.height()),
        (width, height),
        "round-trip changed the dimensions"
    );
});

#[cfg(not(feature = "fuzzing"))]
fn main() {
    // Inert entry point for non-fuzzing builds.
}
