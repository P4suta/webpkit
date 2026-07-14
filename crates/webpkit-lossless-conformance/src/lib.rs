//! Conformance fixture harness for the `webpkit-lossless` WebP VP8L codec.
//!
//! Fixtures live under `fixtures/{decode,encode}/<case>/`, each with a
//! `meta.toml` manifest, an `input.*`, and an `expected.*` golden produced by
//! libwebp's `cwebp` / `dwebp` (never hand-edited). The fixture runner walks
//! the tree, drives each case through `webpkit-lossless`, and records the outcome in the
//! machine-readable `conformance-results.json` ledger (see `cargo xtask
//! conformance`).
#![forbid(unsafe_code)]

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Severity tier for a conformance case: how a failure is gated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// A required case: any failure fails the whole conformance run (non-zero exit).
    #[default]
    Must,
    /// A recommended case: a failure is reported as a warning only.
    Should,
    /// An optional case: a failure is reported as a warning only.
    May,
}

/// The `meta.toml` manifest describing a single conformance case.
#[derive(Debug, Clone, Deserialize)]
pub struct Meta {
    /// The codec feature this case exercises (e.g. `palette`, `predictor`).
    pub feature: String,
    /// The severity tier used to gate the conformance run (defaults to `must`).
    #[serde(default)]
    pub level: Level,
    /// Source image width in pixels. Required by encode cases (which round-trip
    /// raw RGBA); defaults to `0` for decode cases, whose metas omit it.
    #[serde(default)]
    pub width: u32,
    /// Source image height in pixels. Required by encode cases (which round-trip
    /// raw RGBA); defaults to `0` for decode cases, whose metas omit it.
    #[serde(default)]
    pub height: u32,
    /// Short provenance note describing how the golden was produced.
    #[serde(default)]
    pub note: String,
}

/// The outcome of running a single conformance case.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseResult {
    /// The case identifier (its directory name).
    pub case: String,
    /// The codec feature the case exercises (copied from its `meta.toml`).
    pub feature: String,
    /// The severity tier this case is gated at.
    pub level: Level,
    /// Whether the produced output matched the golden.
    pub passed: bool,
}

/// Load and parse a case's `meta.toml`.
///
/// # Errors
///
/// Returns an error if the file cannot be read or is not valid TOML.
pub fn load_meta(path: &Path) -> Result<Meta> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let meta: Meta =
        toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    Ok(meta)
}

/// Serialize a set of case results to the machine-readable ledger JSON.
///
/// # Errors
///
/// Returns an error if serialization fails.
pub fn results_to_json(results: &[CaseResult]) -> Result<String> {
    serde_json::to_string_pretty(results).context("serializing conformance results")
}
