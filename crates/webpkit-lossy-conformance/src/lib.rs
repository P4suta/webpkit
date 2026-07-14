//! Conformance fixture harness for the `webpkit-lossy` WebP VP8 (lossy) **decoder**.
//!
//! Fixtures live under `fixtures/decode/<case>/`, each with a `meta.toml`
//! manifest, an `input.vp8` (the raw contents of a WebP `VP8 ` chunk), and an
//! `expected.rgba` golden produced by libwebp's `WebPDecodeRGBA` (never
//! hand-edited). The decoder is DECODE-ONLY, so there is no encode / round-trip
//! angle: the load-bearing property is that [`webpkit::lossy::decode`] reproduces the
//! libwebp golden byte-for-byte.
//!
//! Unlike the sibling `webpkit-lossless-conformance` (driven by the shared `xtask`), this
//! crate is self-contained. The auto-discovering runner lives in
//! `tests/decode.rs`, and the drift gate that pins the machine-readable
//! `conformance-results-lossy.json` ledger lives in `tests/ledger.rs`. Both
//! build their case records with the helpers below.
//!
//! [`webpkit::lossy::decode`]: https://docs.rs/webpkit-lossy
#![forbid(unsafe_code)]

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Severity tier for a conformance case: how a failure is gated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// A required case: any failure fails the whole conformance run.
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
    /// The decoder feature this case exercises (e.g. `intra-4x4`, `loop-filter`,
    /// `chroma`).
    pub feature: String,
    /// The severity tier used to gate the conformance run (defaults to `must`).
    #[serde(default)]
    pub level: Level,
    /// Source image width in pixels. Optional provenance metadata; defaults to
    /// `0` for cases whose metas omit it.
    #[serde(default)]
    pub width: u32,
    /// Source image height in pixels. Optional provenance metadata; defaults to
    /// `0` for cases whose metas omit it.
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
    /// The decoder feature the case exercises (copied from its `meta.toml`).
    pub feature: String,
    /// The severity tier this case is gated at.
    pub level: Level,
    /// Whether the decoded RGBA matched the golden byte-for-byte.
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
/// The output is pretty-printed and terminated with a single trailing newline,
/// so it is written to `conformance-results-lossy.json` verbatim (no caller-side
/// fix-up) and the drift gate can compare bytes directly. The [`CaseResult`]
/// field order is load-bearing — it fixes the JSON key order the committed
/// ledger is pinned to.
///
/// # Errors
///
/// Returns an error if serialization fails.
pub fn results_to_json(results: &[CaseResult]) -> Result<String> {
    let mut json =
        serde_json::to_string_pretty(results).context("serializing conformance results")?;
    json.push('\n');
    Ok(json)
}
