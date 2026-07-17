//! Cross-cutting helpers shared by the xtask subcommands: workspace/path
//! resolution, the byte-reproducible FNV hash, the codec round-trip wrappers, and
//! the shared encoder-method vocabulary.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Resolve the workspace root via `cargo metadata`.
pub(crate) fn workspace_root() -> Result<PathBuf> {
    let metadata = cargo_metadata::MetadataCommand::new()
        .no_deps()
        .exec()
        .context("running `cargo metadata`")?;
    Ok(metadata.workspace_root.into_std_path_buf())
}

/// Directory holding the decode conformance fixtures.
pub(crate) fn decode_fixtures_dir(root: &Path) -> PathBuf {
    root.join("crates/webpkit-lossless-conformance/fixtures/decode")
}

/// Directory holding the encode conformance fixtures.
pub(crate) fn encode_fixtures_dir(root: &Path) -> PathBuf {
    root.join("crates/webpkit-lossless-conformance/fixtures/encode")
}

/// Absolute path to the committed corpus directory (repo root `corpus/`).
pub(crate) fn corpus_dir(root: &Path) -> PathBuf {
    root.join("corpus")
}

/// FNV-1a-64 hash: integer-only and deterministic (no float, no `HashMap`), so
/// the committed golden is byte-reproducible across platforms.
#[must_use]
pub(crate) fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// Encode raw RGBA through the public API (default `AUTO` effort, no metadata).
pub(crate) fn webpkit_encode(
    rgba: &[u8],
    width: u32,
    height: u32,
) -> webpkit::lossless::Result<Vec<u8>> {
    let dims = webpkit::lossless::Dimensions::new(width, height)?;
    let image =
        webpkit::lossless::ImageRef::new(dims, webpkit::lossless::PixelLayout::Rgba8, rgba)?;
    webpkit::lossless::encode(image, &webpkit::lossless::EncoderConfig::default())
}

/// The three representative encoder efforts sampled for the ledger, cheapest first:
/// the fastest fixed level, the adaptive default, and the deepest search.
pub(crate) const ALL_METHODS: [webpkit::lossless::Effort; 3] = [
    webpkit::lossless::Effort::level(0),
    webpkit::lossless::Effort::AUTO,
    webpkit::lossless::Effort::level(9),
];

/// The stable ledger label for a sampled encoder effort. `Effort` exposes no level
/// getter, so the three sampled points are recovered by probing the constructors.
pub(crate) fn method_name(m: webpkit::lossless::Effort) -> &'static str {
    if m == webpkit::lossless::Effort::AUTO {
        "auto"
    } else if m == webpkit::lossless::Effort::level(0) {
        "l0"
    } else {
        "l9"
    }
}

/// The three representative lossy efforts sampled for the ledger, cheapest first.
pub(crate) const LOSSY_METHODS: [webpkit::lossy::Effort; 3] = [
    webpkit::lossy::Effort::level(0),
    webpkit::lossy::Effort::AUTO,
    webpkit::lossy::Effort::level(9),
];

/// The stable ledger label for a sampled lossy effort (see [`method_name`]).
pub(crate) fn lossy_method_name(m: webpkit::lossy::Effort) -> &'static str {
    if m == webpkit::lossy::Effort::AUTO {
        "auto"
    } else if m == webpkit::lossy::Effort::level(0) {
        "l0"
    } else {
        "l9"
    }
}
