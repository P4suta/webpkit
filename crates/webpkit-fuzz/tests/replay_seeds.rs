//! Corpus-replay regression: feed every committed fuzz seed through the umbrella
//! decode entry points and assert none panics — on **stable**, in a normal
//! `cargo test`, with `overflow-checks` on (the dev/test profile).
//!
//! This is the continuous-fuzzing safety net that does *not* depend on the
//! libFuzzer / cargo-fuzz toolchain (currently blocked upstream by a sancov link
//! bug, so the `fuzz` CI job is manual-only). Integer overflow is input-determined,
//! not optimization-determined, so any wrap a release build could hit is reachable
//! here too and trips the overflow trap. Every past crash reproducer added under
//! `seeds/` is replayed on every CI run, pinning the fix against regression.
//!
//! The closures below mirror `fuzz_targets/{decode,decode_frames}.rs` exactly (the
//! same public calls), so the replayed code cannot drift from the fuzzed code.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;

/// Read every committed seed under `seeds/<target>/`. An I/O error propagates (the
/// test fails); an empty directory trips the assertion, so a mis-path can never make
/// a replay test vacuous.
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

/// Run `body` over every seed; a panic on any seed fails the test and names it.
fn replay(target: &str, body: impl Fn(&[u8])) -> std::io::Result<()> {
    for (name, data) in seeds(target)? {
        let outcome = catch_unwind(AssertUnwindSafe(|| body(&data)));
        assert!(outcome.is_ok(), "panicked replaying seed {name}");
    }
    Ok(())
}

#[test]
fn decode_never_panics_on_any_seed() -> std::io::Result<()> {
    // Mirrors fuzz_targets/decode.rs: the whole container -> VP8/ALPH -> VP8L
    // pipeline must never panic on hostile input.
    replay("decode", |data| {
        let _ = webpkit::decode(data);
    })
}

#[test]
fn decode_frames_never_panics_on_any_seed() -> std::io::Result<()> {
    // Mirrors fuzz_targets/decode_frames.rs: same 1M px/frame cap and 64-frame
    // bound, so the animation walk + compositor are exercised identically.
    let options = webpkit::DecodeOptions::default().max_pixels(1 << 20);
    replay("decode_frames", |data| {
        if let Ok(frames) = webpkit::decode_frames_with(data, &options) {
            for frame in frames.composited().take(64) {
                let _ = frame;
            }
        }
    })
}
