//! Fuzz target: feed arbitrary bytes to the umbrella `webp` decoder.
//!
//! Run: `cargo +nightly fuzz run decode --fuzz-dir crates/webpkit-fuzz --features fuzzing`.
//! Gated on the `fuzzing` feature (which pulls in `libfuzzer-sys`); a normal
//! `cargo build --workspace` compiles this to an inert binary so the libFuzzer
//! runtime is never linked outside a fuzzing build.
//!
//! `webpkit::decode` dispatches the whole container -> lossy (VP8) -> ALPH
//! -> lossless (VP8L) pipeline, so this one target exercises every layer
//! reachable from hostile input.
#![cfg_attr(feature = "fuzzing", no_main)]

#[cfg(feature = "fuzzing")]
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    // The decoder must never panic on hostile input.
    let _ = webpkit::decode(data);
});

#[cfg(not(feature = "fuzzing"))]
fn main() {
    // Inert entry point for non-fuzzing builds.
}
