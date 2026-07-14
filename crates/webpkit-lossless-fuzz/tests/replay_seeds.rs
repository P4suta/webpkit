//! Corpus-replay regression for the VP8L (lossless) decode + animation targets.
//!
//! Feeds every committed seed through the same decode entry points the libFuzzer
//! targets call, on **stable** under a normal `cargo test` (overflow-checks on).
//! See `crates/webpkit-fuzz/tests/replay_seeds.rs` for the rationale — this is the
//! toolchain-independent continuous-fuzzing net that replays every past crash
//! reproducer on every CI run.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;

fn seeds(target: &str) -> std::io::Result<Vec<(String, Vec<u8>)>> {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("seeds")
        .join(target);
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let path = entry?.path();
        if path.is_file() {
            out.push((path.to_string_lossy().into_owned(), std::fs::read(&path)?));
        }
    }
    assert!(!out.is_empty(), "no seeds under {}", dir.display());
    Ok(out)
}

fn replay(target: &str, body: impl Fn(&[u8])) -> std::io::Result<()> {
    for (name, data) in seeds(target)? {
        let outcome = catch_unwind(AssertUnwindSafe(|| body(&data)));
        assert!(outcome.is_ok(), "panicked replaying seed {name}");
    }
    Ok(())
}

#[test]
fn lossless_decode_never_panics_on_any_seed() -> std::io::Result<()> {
    // Mirrors fuzz_targets/decode.rs (and the first line of roundtrip.rs): decoding
    // arbitrary bytes must never panic. Both corpora feed the same decoder.
    for target in ["decode", "roundtrip"] {
        replay(target, |data| {
            let _ = webpkit::lossless::decode(data);
        })?;
    }
    Ok(())
}

#[test]
fn lossless_animation_never_panics_on_any_seed() -> std::io::Result<()> {
    // Mirrors fuzz_targets/animation.rs: the lazy per-frame walk and the
    // compositing pass over the same input, each pulled to completion or first err.
    replay("animation", |data| {
        if let Ok(frames) = webpkit::lossless::decode_frames(data) {
            for frame in frames {
                if frame.is_err() {
                    break;
                }
            }
        }
        if let Ok(frames) = webpkit::lossless::decode_frames(data) {
            for composited in frames.composited() {
                if composited.is_err() {
                    break;
                }
            }
        }
    })
}
