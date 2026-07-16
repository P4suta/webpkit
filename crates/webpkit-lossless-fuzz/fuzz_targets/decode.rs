//! Fuzz target: an arbitrary-bytes decode that must never panic.
//!
//! The body is [`webpkit_lossless_fuzz::decode`], so `tests/replay_seeds.rs` runs
//! this target rather than an imitation of it. Inert under a normal build: the
//! `fuzzing` feature is what pulls in libFuzzer.
#![cfg_attr(feature = "fuzzing", no_main)]

#[cfg(feature = "fuzzing")]
libfuzzer_sys::fuzz_target!(|data: &[u8]| webpkit_lossless_fuzz::decode(data));

#[cfg(not(feature = "fuzzing"))]
fn main() {
    // Inert entry point for non-fuzzing builds.
}
