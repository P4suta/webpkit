//! Fuzz target: feed arbitrary bytes to the webpkit-lossy VP8 (lossy) decoder.
//!
//! Run: `cargo +nightly fuzz run decode --fuzz-dir crates/webpkit-lossy-fuzz --features fuzzing`.
//! Gated on the `fuzzing` feature (which pulls in `libfuzzer-sys`); a normal
//! `cargo build --workspace` compiles this to an inert binary so the libFuzzer
//! runtime is never linked outside a fuzzing build.
#![cfg_attr(feature = "fuzzing", no_main)]

#[cfg(feature = "fuzzing")]
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    // The decoder must never panic on hostile input.
    let _ = webpkit::lossy::decode(data);
});

#[cfg(not(feature = "fuzzing"))]
fn main() {
    // Inert entry point for non-fuzzing builds.
}
