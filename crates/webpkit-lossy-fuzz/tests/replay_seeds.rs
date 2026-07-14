//! Corpus-replay regression for the VP8 (lossy) decode + streaming targets.
//!
//! Feeds every committed seed through the same decode entry points the libFuzzer
//! targets call, on **stable** under a normal `cargo test` (overflow-checks on).
//! See `crates/webpkit-fuzz/tests/replay_seeds.rs` for the rationale.

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
fn lossy_decode_never_panics_on_any_seed() -> std::io::Result<()> {
    // Mirrors fuzz_targets/decode.rs: the VP8 key-frame decoder must never panic on
    // hostile input. The encode corpus is also decode-safe to replay as a smoke.
    for target in ["decode", "encode"] {
        replay(target, |data| {
            let _ = webpkit::lossy::decode(data);
        })?;
    }
    Ok(())
}

#[test]
fn lossy_stream_never_panics_on_any_seed() -> std::io::Result<()> {
    // Mirrors fuzz_targets/stream.rs: push each seed in data-derived chunk sizes so
    // many suspend/resume boundaries are crossed; no split may panic, and draining
    // rows / finishing must stay sound.
    replay("stream", |data| {
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
    })
}
