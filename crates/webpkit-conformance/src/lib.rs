//! Conformance fixture harness for the pure-Rust `webp` decoder's lossy paths:
//! the still `ALPH` (transparent-lossy) path and the animated-lossy path.
//!
//! Still fixtures live under `fixtures/alpha/<case>/`, each with a `meta.toml`
//! manifest, an `input.webp` (a full `VP8 ` + `ALPH` container file), and an
//! `expected.rgba` golden produced by libwebp's `WebPDecodeRGBA` (never
//! hand-edited). Animated fixtures live under `fixtures/anim/<case>/`, each with
//! a `meta.toml`, an `input.webp` (an animated-lossy `VP8X` file) and a
//! `frames.rgba` golden: the per-frame *composited* RGBA that libwebp's
//! `WebPAnimDecoder` produces, concatenated in frame order. Lossy decode is not a
//! round-trip identity — the golden is libwebp's *decode* of the committed file —
//! so, exactly like the sibling `webpkit-lossy-conformance`, the load-bearing property
//! is that `webpkit::decode` (still) / `webpkit::decode_frames(...).composited()`
//! (animated) reproduces the libwebp golden byte-for-byte.
//!
//! The auto-discovering decode gates live in `tests/decode.rs`; the drift gates
//! that pin the machine-readable `conformance-results-alpha.json` and
//! `conformance-results-anim.json` ledgers live in `tests/ledger.rs`, alongside
//! the `#[ignore]` + `oracle`-gated generators that (re)produce the fixtures and
//! ledgers from libwebp. The default build and CI gate link no reference library.
//! All gates build their case records with the helpers below.
#![forbid(unsafe_code)]

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// How a fixture's `ALPH` alpha plane was compressed by the libwebp encoder —
/// the `alpha_compression` knob the case pins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AlphaCompression {
    /// Uncompressed alpha (`WebPConfig.alpha_compression == 0`).
    #[default]
    None,
    /// Lossless `VP8L` alpha (`WebPConfig.alpha_compression == 1`).
    Lossless,
}

/// The `meta.toml` manifest describing a single `ALPH` conformance case.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    /// Source image width in pixels.
    pub width: u32,
    /// Source image height in pixels.
    pub height: u32,
    /// The `alpha_compression` method the fixture's `ALPH` chunk was encoded with.
    pub alpha_compression: AlphaCompression,
    /// The libwebp `alpha_filtering` knob (0 none / 1 fast / 2 best) the fixture
    /// was encoded with. This steers the encoder's spatial-filter *search*, not
    /// the concrete filter stored in the chunk.
    pub alpha_filtering: u8,
    /// The lossy VP8 quality the RGB was encoded at.
    pub quality: f32,
    /// Short provenance note describing how the golden was produced.
    #[serde(default)]
    pub note: String,
}

/// The outcome of running a single `ALPH` conformance case.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseResult {
    /// The case identifier (its directory name).
    pub case: String,
    /// The alpha-compression method the case pins (copied from its `meta.toml`).
    pub alpha_compression: AlphaCompression,
    /// The libwebp `alpha_filtering` knob the case pins (copied from its
    /// `meta.toml`).
    pub alpha_filtering: u8,
    /// Whether the decoded RGBA matched the libwebp golden byte-for-byte.
    pub passed: bool,
}

/// The `meta.toml` manifest describing a single animated-lossy conformance case.
///
/// The golden for an animated case is the per-frame *composited* RGBA that
/// libwebp's `WebPAnimDecoder` produces (never hand-edited), concatenated in
/// frame order into `frames.rgba`; the load-bearing property is that
/// `webpkit::decode_frames(...).composited()` reproduces it byte-for-byte.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnimMeta {
    /// Canvas width in pixels (every frame composites onto this canvas).
    pub width: u32,
    /// Canvas height in pixels.
    pub height: u32,
    /// Number of frames the animation composites to.
    pub frame_count: u32,
    /// The lossy VP8 quality the frames' RGB was encoded at.
    pub quality: f32,
    /// Short provenance note describing how the golden was produced.
    #[serde(default)]
    pub note: String,
}

/// The outcome of running a single animated-lossy conformance case.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnimCaseResult {
    /// The case identifier (its directory name).
    pub case: String,
    /// The frame count the case pins (copied from its `meta.toml`).
    pub frame_count: u32,
    /// Whether every decoded, composited frame matched the libwebp golden
    /// byte-for-byte (compared as one concatenated buffer).
    pub passed: bool,
}

/// Load and parse a case's `meta.toml` into `T` (a [`Meta`] or an [`AnimMeta`]).
fn load_toml<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

/// Load and parse a still `ALPH` case's `meta.toml`.
///
/// # Errors
///
/// Returns an error if the file cannot be read or is not valid TOML.
pub fn load_meta(path: &Path) -> Result<Meta> {
    load_toml(path)
}

/// Load and parse an animated-lossy case's `meta.toml`.
///
/// # Errors
///
/// Returns an error if the file cannot be read or is not valid TOML.
pub fn load_anim_meta(path: &Path) -> Result<AnimMeta> {
    load_toml(path)
}

/// Serialize a set of ledger records to the machine-readable JSON.
///
/// The output is pretty-printed and terminated with a single trailing newline,
/// so it is written to the committed `conformance-results-*.json` verbatim (no
/// caller-side fix-up) and the drift gate can compare bytes directly. The record
/// field order is load-bearing — it fixes the JSON key order the committed ledger
/// is pinned to — and the records carry no floats so the serialization is
/// byte-stable across platforms.
fn ledger_json<T: Serialize>(records: &[T]) -> Result<String> {
    let mut json =
        serde_json::to_string_pretty(records).context("serializing conformance results")?;
    json.push('\n');
    Ok(json)
}

/// Serialize the still `ALPH` [`CaseResult`] ledger (see `ledger_json` for the
/// byte-stability contract the committed ledger relies on).
///
/// # Errors
///
/// Returns an error if serialization fails.
pub fn results_to_json(results: &[CaseResult]) -> Result<String> {
    ledger_json(results)
}

/// Serialize the animated-lossy [`AnimCaseResult`] ledger (see `ledger_json` for
/// the byte-stability contract the committed ledger relies on).
///
/// # Errors
///
/// Returns an error if serialization fails.
pub fn anim_results_to_json(results: &[AnimCaseResult]) -> Result<String> {
    ledger_json(results)
}
