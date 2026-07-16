//! The fuzz targets' bodies, callable from a normal build.
//!
//! Each `fuzz_targets/*.rs` is one `fuzz_target!` around a function here, and
//! `tests/replay_seeds.rs` calls the same functions over the committed seeds — so
//! the replay is the target, not an imitation of it.
//!
//! The libFuzzer job is `workflow_dispatch`-only while an upstream regression is
//! fixed, which makes the replay the only continuous coverage these invariants
//! get. `encode`'s replay used to run `decode` instead, so the encoder invariant
//! it exists to check ran in CI never.
#![forbid(unsafe_code)]
#![allow(
    clippy::expect_used,
    clippy::panic,
    reason = "fuzz bodies: a violated invariant must abort loudly — that is the finding"
)]

use webpkit::lossy::{Dimensions, ImageRef, LossyConfig, PixelLayout};

/// The VP8 key-frame decoder must never panic on hostile input.
pub fn decode(data: &[u8]) {
    let _ = webpkit::lossy::decode(data);
}

/// Encode an arbitrary small frame, then decode our own output.
///
/// The first three bytes pick a width, height (each `1..=64`) and quality; the
/// rest are RGBA pixels. Encoding must never panic, and the output must decode
/// back to a frame of the source dimensions.
pub fn encode(data: &[u8]) {
    let _ = encode_reached(data);
}

/// The encode body, returning whether it reached the encode/decode assertions.
/// The `bool` lets a replay test prove its seeds are not vacuous.
///
/// # Panics
///
/// If the encoder's output does not decode to the source dimensions — the finding.
#[must_use]
pub fn encode_reached(data: &[u8]) -> bool {
    let [w, h, q, body @ ..] = data else {
        return false;
    };
    let width = u32::from(*w) % 64 + 1;
    let height = u32::from(*h) % 64 + 1;
    let quality = *q % 101;
    let needed = (width * height * 4) as usize;
    let Some(rgba) = body.get(..needed) else {
        return false;
    };

    let dims = Dimensions::new(width, height).expect("1..=64 is a valid dimension");
    let img =
        ImageRef::new(dims, PixelLayout::Rgba8, rgba).expect("buffer length matches the frame");
    let cfg = LossyConfig::new().with_quality(quality);
    let (_dims, payload) = webpkit::lossy::encode_vp8(img, &cfg).expect("encode is infallible");

    let decoded = webpkit::lossy::decode(&payload).expect("must decode its own output");
    assert_eq!(
        (decoded.width(), decoded.height()),
        (width, height),
        "round-trip changed the dimensions"
    );
    true
}

/// Push the input in data-derived chunk sizes, crossing many suspend/resume
/// boundaries. No split may panic, and draining rows / finishing must stay sound.
pub fn stream(data: &[u8]) {
    let mut dec = webpkit::lossy::IncrementalDecoder::new();
    let mut off = 0;
    while off < data.len() {
        let step = 1 + (data[off] as usize & 0x0f);
        let end = (off + step).min(data.len());
        if dec.push(&data[off..end]).is_err() {
            break;
        }
        let _ = dec.drain_rows();
        off = end;
    }
    let _ = dec.into_image();
}
