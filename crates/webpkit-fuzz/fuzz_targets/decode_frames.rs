//! Fuzz target: feed arbitrary bytes to the umbrella `webp` animation walk.
//!
//! Run: `cargo +nightly fuzz run decode_frames --fuzz-dir crates/webpkit-fuzz --features fuzzing`.
//! Gated on the `fuzzing` feature (which pulls in `libfuzzer-sys`); a normal
//! `cargo build --workspace` compiles this to an inert binary so the libFuzzer
//! runtime is never linked outside a fuzzing build.
//!
//! `webpkit::decode_frames_with` drives the `ANIM`/`ANMF` parser, the per-frame
//! VP8L/VP8-lossy decode (with sibling `ALPH` alpha), and the canvas
//! compositor. A hostile animation header can declare an enormous canvas, so we
//! cap every frame at 1M pixels via `DecodeOptions::max_pixels` before it is
//! materialized, and `.take(64)` bounds the frame count so a hostile loop can't
//! spin forever — together they keep the target OOM- and hang-free while still
//! asserting the whole pipeline never panics on hostile input.
#![cfg_attr(feature = "fuzzing", no_main)]

#[cfg(feature = "fuzzing")]
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    // 1M px/frame cap: bound per-frame allocation against a hostile header.
    let options = webpkit::DecodeOptions::default().max_pixels(1 << 20);
    if let Ok(frames) = webpkit::decode_frames_with(data, &options) {
        // `.take(64)` bounds the frame count under a hostile loop; each item is
        // a `Result` (errors are fine, panics are not) that we explicitly drop.
        for frame in frames.composited().take(64) {
            let _ = frame;
        }
    }
});

#[cfg(not(feature = "fuzzing"))]
fn main() {
    // Inert entry point for non-fuzzing builds.
}
