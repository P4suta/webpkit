//! Corpus-replay regression for the VP8L (lossless) fuzz targets.
//!
//! Feeds every committed seed through the targets themselves — the functions in
//! this crate's lib that `fuzz_targets/*.rs` wrap — on **stable** under a normal
//! `cargo test`, with overflow-checks on. See `crates/webpkit-fuzz/tests/replay_seeds.rs`
//! for the rationale: this is the toolchain-independent net that replays every past
//! crash reproducer on every CI run, and while the libFuzzer job is disabled it is
//! the only continuous coverage these invariants get.
//!
//! Calling the target rather than re-typing it is the point. The previous version
//! "mirrored" each target by hand, and `roundtrip`'s mirror ran `decode` alone — so
//! the lossless round-trip assertion, the entire reason that target exists, ran in
//! CI never, under a comment promising "the exact same entry points".

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
fn decode_survives_every_seed() -> std::io::Result<()> {
    // Both corpora feed the same decoder, and `roundtrip` decodes raw input first.
    for target in ["decode", "roundtrip"] {
        replay(target, webpkit_lossless_fuzz::decode)?;
    }
    Ok(())
}

/// The round-trip invariant itself — encode at every effort, decode, compare —
/// not merely "decoding the seeds does not panic".
#[test]
fn roundtrip_survives_every_seed() -> std::io::Result<()> {
    replay("roundtrip", webpkit_lossless_fuzz::roundtrip)
}

#[test]
fn animation_survives_every_seed() -> std::io::Result<()> {
    replay("animation", webpkit_lossless_fuzz::animation)
}

/// At least one seed must actually reach the round-trip assertions.
///
/// Without this the whole `roundtrip_survives_every_seed` test is vacuous — and it
/// was: the corpus held WebP files, whose first two bytes the target reads as
/// dimensions it can never fill, so every seed returned early and asserted nothing.
#[test]
fn roundtrip_seeds_are_not_vacuous() -> std::io::Result<()> {
    let reached = seeds("roundtrip")?
        .iter()
        .filter(|(_, data)| webpkit_lossless_fuzz::roundtrip_reached(data))
        .count();
    assert!(
        reached > 0,
        "no roundtrip seed reaches the encode/decode invariant"
    );
    Ok(())
}
