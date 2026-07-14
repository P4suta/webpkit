//! Fuzz target: decoding arbitrary bytes as an animation must never panic.
//!
//! Drives [`webpkit::lossless::decode_frames`] over hostile input, iterating every lazy
//! frame and then compositing the whole sequence — exercising the `ANIM`/`ANMF`
//! parser, the per-frame VP8L decode, and the canvas compositor. Inert under a
//! normal build.
#![cfg_attr(feature = "fuzzing", no_main)]

#[cfg(feature = "fuzzing")]
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    // Lazy per-frame decode: pull every frame (errors are fine, panics are not).
    if let Ok(frames) = webpkit::lossless::decode_frames(data) {
        for frame in frames {
            if frame.is_err() {
                break;
            }
        }
    }
    // Compositing pass over the same input.
    if let Ok(frames) = webpkit::lossless::decode_frames(data) {
        for composited in frames.composited() {
            if composited.is_err() {
                break;
            }
        }
    }
});

#[cfg(not(feature = "fuzzing"))]
fn main() {
    // Inert entry point for non-fuzzing builds.
}
