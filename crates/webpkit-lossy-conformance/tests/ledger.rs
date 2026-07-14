//! Drift gate for the committed `conformance-results-lossy.json` ledger.
//!
//! Recomputes a [`CaseResult`] for every `fixtures/decode/<case>/` (decode-only,
//! tool-free — it never links libwebp), serializes it with the crate's
//! [`results_to_json`], and asserts the bytes equal the committed ledger at the
//! crate root. This pins the machine-readable conformance record so it cannot
//! silently drift from what the decoder actually does.
//!
//! The ledger and fixtures are supplied by the integrator (the ledger is
//! regenerated from the webpkit-lossy `oracle` harness). Until the ledger exists the
//! gate skips with a note rather than failing.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use webpkit_lossy_conformance::{CaseResult, load_meta, results_to_json};

/// Recompute the decode ledger from the committed fixtures, visiting cases in
/// sorted order so the serialized bytes are deterministic.
///
/// A case is any `fixtures/decode/<case>/` holding `input.vp8`, `expected.rgba`,
/// and `meta.toml`; `passed` records whether `webpkit::lossy::decode` reproduced the
/// golden byte-for-byte. This is intentionally tool-free (no libwebp link), so
/// the gate stays reproducible on CI.
fn compute_results(decode_dir: &Path) -> Result<Vec<CaseResult>> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(decode_dir)
        .with_context(|| format!("reading {}", decode_dir.display()))?
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();

    let mut results = Vec::with_capacity(dirs.len());
    for case_dir in dirs {
        let input = case_dir.join("input.vp8");
        let golden = case_dir.join("expected.rgba");
        let meta_path = case_dir.join("meta.toml");
        if !input.exists() || !golden.exists() || !meta_path.exists() {
            continue;
        }
        let meta = load_meta(&meta_path)?;
        let case = case_dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("<unknown>")
            .to_owned();
        let payload =
            std::fs::read(&input).with_context(|| format!("reading {}", input.display()))?;
        let expected =
            std::fs::read(&golden).with_context(|| format!("reading {}", golden.display()))?;
        let passed = webpkit::lossy::decode(&payload)
            .is_ok_and(|image| image.as_bytes() == expected.as_slice());
        results.push(CaseResult {
            case,
            feature: meta.feature,
            level: meta.level,
            passed,
        });
    }
    Ok(results)
}

#[test]
fn committed_ledger_is_up_to_date() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let ledger_path = root.join("conformance-results-lossy.json");

    let committed = match std::fs::read_to_string(&ledger_path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!(
                "skipping: no ledger at {} (the integrator commits it from the oracle harness)",
                ledger_path.display()
            );
            return;
        },
        Err(e) => panic!("reading {}: {e}", ledger_path.display()),
    };

    let decode_dir = root.join("fixtures/decode");
    let results = compute_results(&decode_dir).expect("recompute conformance ledger");
    let fresh = results_to_json(&results).expect("serialize conformance ledger");

    assert_eq!(
        fresh,
        committed,
        "conformance-results-lossy.json at {} has drifted from a fresh decode run. \
         Regenerate it from the webpkit::lossy oracle harness and commit the updated file.",
        ledger_path.display()
    );
}

/// Regenerate the committed ledger from the current fixtures. Tool-free (decode
/// only), so it runs without the `oracle` feature. Run explicitly:
/// `cargo test -p webpkit-lossy-conformance --test ledger -- --ignored gen_ledger`.
#[test]
#[ignore = "regenerates the committed ledger; run explicitly"]
fn gen_ledger() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let results = compute_results(&root.join("fixtures/decode")).expect("recompute ledger");
    let json = results_to_json(&results).expect("serialize ledger");
    std::fs::write(root.join("conformance-results-lossy.json"), json).expect("write ledger");
}
