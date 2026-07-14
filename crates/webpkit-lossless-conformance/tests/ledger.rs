//! Drift gate for the committed `conformance-results-lossless.json` ledger.
//!
//! Recomputes a [`CaseResult`] for every `fixtures/decode/<case>/` and
//! `fixtures/encode/<case>/` (tool-free — it never links libwebp), serializes it
//! with the crate's [`results_to_json`], and asserts the bytes equal the committed
//! ledger. This is the in-crate drift gate, symmetric with the `webpkit-lossy-conformance`
//! and `webpkit-conformance` ledgers — so all three codecs pin their machine-readable
//! conformance record the same way (no separate `xtask` mechanism for lossless).
//!
//! Decode cases decode `input.webp` and compare against `expected.rgba`; encode
//! cases encode `input.rgba` and decode it back, asserting a lossless round trip.
//! Encode cases are namespaced `encode/<name>` so the two halves share one ledger
//! without colliding.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use webpkit_lossless_conformance::{CaseResult, load_meta, results_to_json};

/// Sorted `<case>` subdirectories of `dir` (empty if `dir` is absent).
fn case_dirs(dir: &Path) -> Result<Vec<PathBuf>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    Ok(dirs)
}

/// `case_dir`'s directory name (its case identifier).
fn case_name(case_dir: &Path) -> String {
    case_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("<unknown>")
        .to_owned()
}

/// Decode ledger: `webpkit::lossless::decode_rgba(input.webp) == expected.rgba`.
fn compute_decode_results(decode_dir: &Path) -> Result<Vec<CaseResult>> {
    let mut results = Vec::new();
    for case_dir in case_dirs(decode_dir)? {
        let (input, golden, meta_path) = (
            case_dir.join("input.webp"),
            case_dir.join("expected.rgba"),
            case_dir.join("meta.toml"),
        );
        if !input.exists() || !golden.exists() || !meta_path.exists() {
            continue;
        }
        let meta = load_meta(&meta_path)?;
        let webp = std::fs::read(&input).with_context(|| format!("reading {}", input.display()))?;
        let expected =
            std::fs::read(&golden).with_context(|| format!("reading {}", golden.display()))?;
        let passed =
            matches!(webpkit::lossless::decode_rgba(&webp), Ok((_, rgba)) if rgba == expected);
        results.push(CaseResult {
            case: case_name(&case_dir),
            feature: meta.feature,
            level: meta.level,
            passed,
        });
    }
    Ok(results)
}

/// Encode ledger: encoding `input.rgba` and decoding it back reproduces the source
/// byte-for-byte (a lossless round trip). Tool-free — no libwebp link — so the gate
/// stays reproducible on CI; the independent `dwebp` cross-check happens once, at
/// fixture-generation time. Cases are namespaced `encode/<name>`.
fn compute_encode_results(encode_dir: &Path) -> Result<Vec<CaseResult>> {
    let mut results = Vec::new();
    for case_dir in case_dirs(encode_dir)? {
        let (input, meta_path) = (case_dir.join("input.rgba"), case_dir.join("meta.toml"));
        if !input.exists() || !meta_path.exists() {
            continue;
        }
        let meta = load_meta(&meta_path)?;
        let rgba = std::fs::read(&input).with_context(|| format!("reading {}", input.display()))?;
        let passed = encode_round_trips(&rgba, meta.width, meta.height);
        results.push(CaseResult {
            case: format!("encode/{}", case_name(&case_dir)),
            feature: meta.feature,
            level: meta.level,
            passed,
        });
    }
    Ok(results)
}

/// Whether encoding `rgba` to a lossless WebP and decoding it back reproduces the
/// source byte-for-byte at the declared dimensions.
fn encode_round_trips(rgba: &[u8], width: u32, height: u32) -> bool {
    let Ok(dims) = webpkit::lossless::Dimensions::new(width, height) else {
        return false;
    };
    let Ok(image) =
        webpkit::lossless::ImageRef::new(dims, webpkit::lossless::PixelLayout::Rgba8, rgba)
    else {
        return false;
    };
    let Ok(webp) = webpkit::lossless::encode(image, &webpkit::lossless::EncoderConfig::default())
    else {
        return false;
    };
    matches!(
        webpkit::lossless::decode_rgba(&webp),
        Ok((out_dims, out)) if out_dims == dims && out == rgba
    )
}

/// The full ledger: decode cases followed by `encode/<name>` cases.
fn compute_results(root: &Path) -> Result<Vec<CaseResult>> {
    let mut results = compute_decode_results(&root.join("fixtures/decode"))?;
    results.extend(compute_encode_results(&root.join("fixtures/encode"))?);
    Ok(results)
}

#[test]
fn committed_ledger_is_up_to_date() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let ledger_path = root.join("conformance-results-lossless.json");

    let committed = match std::fs::read_to_string(&ledger_path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!(
                "skipping: no ledger at {} (run the ignored `gen_ledger` to create it)",
                ledger_path.display()
            );
            return;
        },
        Err(e) => panic!("reading {}: {e}", ledger_path.display()),
    };

    let results = compute_results(root).expect("recompute conformance ledger");
    let fresh = format!("{}\n", results_to_json(&results).expect("serialize ledger"));

    assert_eq!(
        fresh,
        committed,
        "conformance-results-lossless.json at {} has drifted from a fresh run. \
         Regenerate it (`cargo test -p webpkit-lossless-conformance --test ledger -- \
         --ignored gen_ledger`) and commit the updated file.",
        ledger_path.display()
    );
}

/// Regenerate the committed ledger from the current fixtures. Tool-free (no
/// libwebp link). Run explicitly:
/// `cargo test -p webpkit-lossless-conformance --test ledger -- --ignored gen_ledger`.
#[test]
#[ignore = "regenerates the committed ledger; run explicitly"]
fn gen_ledger() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let results = compute_results(root).expect("recompute ledger");
    let json = format!("{}\n", results_to_json(&results).expect("serialize ledger"));
    std::fs::write(root.join("conformance-results-lossless.json"), json).expect("write ledger");
}
