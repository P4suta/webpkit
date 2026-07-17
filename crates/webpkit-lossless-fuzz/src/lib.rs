//! The fuzz targets' bodies, callable from a normal build.
//!
//! Each `fuzz_targets/*.rs` is one `fuzz_target!` around a function here, and
//! `tests/replay_seeds.rs` calls the same functions over the committed seeds. The
//! replay is therefore the target, not a copy of it.
//!
//! That matters more than it sounds. The libFuzzer job is `workflow_dispatch`-only
//! while an upstream regression is fixed, so the replay is the *only* continuous
//! coverage these invariants get — and it used to be a hand-written imitation.
//! `roundtrip`'s replay ran `decode` alone, so the lossless round-trip assertion,
//! the whole point of the target, ran in CI exactly never while a comment promised
//! "the exact same entry points".
#![forbid(unsafe_code)]
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "fuzz bodies: a violated invariant must abort loudly — that is the finding"
)]

use webpkit::lossless::{Dimensions, Effort, EncoderConfig, ImageRef, PixelLayout};

/// Decoding arbitrary bytes must never panic.
pub fn decode(data: &[u8]) {
    let _ = webpkit::lossless::decode(data);
}

/// Carve an arbitrary small RGBA image out of `data`.
///
/// The first two bytes pick a width and height (each `1..=64`); the rest are
/// pixels. `None` when there are not enough bytes to fill the frame.
fn carve(data: &[u8]) -> Option<(Dimensions, &[u8])> {
    let [w, h, body @ ..] = data else {
        return None;
    };
    let width = u32::from(*w) % 64 + 1;
    let height = u32::from(*h) % 64 + 1;
    let needed = (width * height * 4) as usize;
    let rgba = body.get(..needed)?;
    let dims = Dimensions::new(width, height).expect("1..=64 is a valid dimension");
    Some((dims, rgba))
}

/// An exact lossless round-trip through the encoder and decoder, at every effort.
///
/// Encoding then decoding must reproduce the source byte-for-byte — the core
/// lossless invariant, over the whole input space rather than the handful of
/// proptest images.
pub fn roundtrip(data: &[u8]) {
    let _ = roundtrip_reached(data);
}

/// The round-trip body, returning whether it reached the encode/decode assertions
/// rather than the early-out.
///
/// The `bool` exists so a replay test can prove its seeds are not vacuous. Seeds
/// here are raw `[w][h][rgba..]` for the target's `carve`, not WebP files: a WebP
/// seed's first two bytes (`R`, `I`) parse as a 20x74 frame it never has the bytes
/// to fill, so every such seed returns early and asserts nothing. That is exactly
/// how this corpus sat, testing the round-trip invariant on zero inputs.
///
/// # Panics
///
/// If the round-trip is not lossless — which is the finding.
#[must_use]
pub fn roundtrip_reached(data: &[u8]) -> bool {
    // Decoding arbitrary bytes must never panic.
    let _ = webpkit::lossless::decode(data);

    let Some((dims, rgba)) = carve(data) else {
        return false;
    };
    let image =
        ImageRef::new(dims, PixelLayout::Rgba8, rgba).expect("buffer length matches the frame");

    // The fastest fixed level, the adaptive default, and the deepest search, so the
    // whole encode surface — including the always-on forward transforms — is fuzzed.
    for effort in [Effort::level(0), Effort::AUTO, Effort::level(9)] {
        let config = EncoderConfig::default().with_effort(effort);
        let webp = webpkit::lossless::encode(image, &config).expect("encode is infallible");
        let (out_dims, out_rgba) =
            webpkit::lossless::decode_rgba(&webp).expect("must decode its own output");
        assert_eq!(
            out_dims, dims,
            "{effort:?} round-trip changed the dimensions"
        );
        assert!(out_rgba == rgba, "{effort:?} round-trip was not lossless");
    }
    true
}

/// The lazy per-frame walk and the compositing pass over the same input, each
/// pulled to completion or to the first error.
pub fn animation(data: &[u8]) {
    if let Ok(frames) = webpkit::lossless::decode_frames(data) {
        for frame in frames {
            if frame.is_err() {
                break;
            }
        }
    }
    if let Ok(frames) = webpkit::lossless::decode_frames(data) {
        for frame in frames.composited() {
            if frame.is_err() {
                break;
            }
        }
    }
}
