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

/// Encode raw RGBA through the public API (default `Balanced`, no metadata).
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

/// Every encoder method, in ledger order (cheapest first). Measured at every size.
pub(crate) const ALL_METHODS: [webpkit::lossless::Effort; 3] = [
    webpkit::lossless::Effort::Fast,
    webpkit::lossless::Effort::Balanced,
    webpkit::lossless::Effort::Best,
];

/// The stable ledger label for an encoder method.
///
/// `webpkit::lossless::Effort` is deliberately not `#[non_exhaustive]`, so this
/// match is exhaustive: a new variant would fail to compile here until it is given
/// a label, which is exactly the reminder this tool's method matrix wants.
pub(crate) const fn method_name(m: webpkit::lossless::Effort) -> &'static str {
    match m {
        webpkit::lossless::Effort::Fast => "fast",
        webpkit::lossless::Effort::Balanced => "balanced",
        webpkit::lossless::Effort::Best => "best",
    }
}

/// Every lossy effort method, cheapest first, in printed-table order.
pub(crate) const LOSSY_METHODS: [webpkit::lossy::Effort; 3] = [
    webpkit::lossy::Effort::Fast,
    webpkit::lossy::Effort::Balanced,
    webpkit::lossy::Effort::Best,
];

/// The stable label for a lossy effort method.
///
/// `webpkit::lossy::Effort` is deliberately not `#[non_exhaustive]`, so this match is
/// exhaustive: a new variant would fail to compile here until it is given a label,
/// which is exactly the reminder this tool's method matrix wants.
pub(crate) const fn lossy_method_name(m: webpkit::lossy::Effort) -> &'static str {
    match m {
        webpkit::lossy::Effort::Fast => "fast",
        webpkit::lossy::Effort::Balanced => "balanced",
        webpkit::lossy::Effort::Best => "best",
    }
}
