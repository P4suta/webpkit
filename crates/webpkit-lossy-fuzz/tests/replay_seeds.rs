//! Corpus-replay regression for the VP8 (lossy) fuzz targets.
//!
//! Feeds every committed seed through the targets themselves — the functions in
//! this crate's lib that `fuzz_targets/*.rs` wrap — on **stable** under a normal
//! `cargo test`, with overflow-checks on. While the libFuzzer job is disabled this
//! is the only continuous coverage these invariants get, so it calls the target
//! rather than re-typing it: `encode`'s hand-written mirror ran `decode`, so the
//! encoder invariant never ran at all.

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
    // Both corpora are decode-safe to replay; the encode seeds are a smoke here.
    for target in ["decode", "encode"] {
        replay(target, webpkit_lossy_fuzz::decode)?;
    }
    Ok(())
}

/// The encoder invariant itself — encode, decode our own output, compare
/// dimensions — not merely "decoding the seeds does not panic".
#[test]
fn encode_survives_every_seed() -> std::io::Result<()> {
    replay("encode", webpkit_lossy_fuzz::encode)
}

#[test]
fn stream_survives_every_seed() -> std::io::Result<()> {
    replay("stream", webpkit_lossy_fuzz::stream)
}

/// At least one seed must actually reach the encode assertions, so
/// `encode_survives_every_seed` cannot pass by asserting nothing.
#[test]
fn encode_seeds_are_not_vacuous() -> std::io::Result<()> {
    let reached = seeds("encode")?
        .iter()
        .filter(|(_, data)| webpkit_lossy_fuzz::encode_reached(data))
        .count();
    assert!(
        reached > 0,
        "no encode seed reaches the encode/decode invariant"
    );
    Ok(())
}
