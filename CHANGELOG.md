# Changelog

All notable changes to this project are documented here. This project adheres to
[Semantic Versioning](https://semver.org/), and the changelog is maintained by
[release-plz](https://release-plz.dev) from
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
- **One-call helpers.** `encode_lossless_rgba` / `encode_lossy_rgba` and
  `decode_rgba` / `decode_reader` cover the raw-RGBA common case in a single call,
  alongside the full-control [`Encoder`] builder and `decode`.
- **Safe-by-default decoding.** `decode` / `decode_frames` now cap the canvas at
  `DEFAULT_MAX_PIXELS` (100 Mpx) before any allocation, so a hostile header cannot
  exhaust memory out of the box. `DecodeOptions::unbounded` opts out for trusted
  input; `DecodeOptions::max_pixels` sets a custom cap.
- **`image` crate interop (optional `image` feature).** `TryFrom` conversions
  between `image::DynamicImage` / `RgbaImage` and the codec's `Image`. Off by
  default — the baseline build stays zero-dependency.
- **Streaming API.** Incremental / suspend-resume row-streaming decode.
- **CLI.** `cwebp`, `dwebp`, and `webp` binaries, tracking libwebp semantics.
- **Verification harness.** Golden fixtures, conformance ledger, proptest
  strategies, cargo-fuzz targets (with a toolchain-independent stable corpus-replay
  net that runs every committed seed on each CI run), and a libwebp differential
  oracle, driven by `xtask` automation (`gen-fixtures`, `conformance`,
  `drift-gate`, `corpus-sweep`) and a full CI / lint / hook toolchain.
