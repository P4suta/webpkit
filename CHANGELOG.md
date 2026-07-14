# Changelog

All notable changes to this project are documented here. This project adheres to
[Semantic Versioning](https://semver.org/), and the changelog is maintained by
[release-please](https://github.com/googleapis/release-please) from
[Conventional Commits](https://www.conventionalcommits.org/).

## [Unreleased]

First public release. A pure-Rust WebP codec — `#![forbid(unsafe_code)]`, zero
required runtime dependencies, `no_std`-friendly with `alloc`.

### Added

- **VP8L (lossless).** Bit-exact decoder (all transforms, meta-Huffman, color
  cache, LZ77) and a three-tier encoder (`Fast` / `Balanced` / `Best`), matching
  libwebp `dwebp` byte-for-byte on the conformance goldens.
- **VP8 (lossy).** Baseline key-frame decoder and encoder, byte-exact against
  libwebp on the decode conformance set.
- **Container & metadata.** Extended `VP8X` container with ICC / Exif / XMP
  metadata (preserved by default), alpha (`ALPH`), and animation.
- **Streaming API.** Incremental / suspend-resume row-streaming decode.
- **CLI.** `cwebp`, `dwebp`, and `webp` binaries, tracking libwebp semantics.
- **Verification harness.** Golden fixtures, conformance ledger, proptest
  strategies, cargo-fuzz targets, and a libwebp differential oracle, driven by
  `xtask` automation (`gen-fixtures`, `conformance`, `drift-gate`,
  `corpus-sweep`) and a full CI / lint / hook toolchain.
