# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- *(cli)* near-lossless preprocessing: `cwebp -near_lossless N` and `webp [encode|convert] --near-lossless N` (`0..=100`, lower = stronger). Implies lossless, matching libwebp's cwebp; replaces the former rejection of `-near_lossless`.
- *(cli)* `webp meta` — the `webpmux` half: `webp meta show <f>` prints a file's ICC/Exif/XMP, `webp meta set <f> -o OUT --icc/--exif/--xmp FILE` replaces kinds from files, and `webp meta strip <f> -o OUT` removes all sidecar metadata. The image bitstream is never decoded or re-encoded; writes are atomic.
- *(cli)* `dwebp -yuv` / `-pgm` now emit a lossy still's native YUV 4:2:0 planes (planar binary, or PGM layout) instead of rejecting; lossless input is a clear error.
- *(cli)* `dwebp -bmp` / `-tiff` now write BMP/TIFF via the `image` crate (`formats` feature) instead of rejecting; still rejected under `--no-default-features`.

### Fixed

- *(cli)* GIF → animated WebP now preserves the source GIF's loop count instead of hard-coding an infinite loop.
- *(cli)* `webp <gif> --lossy [--quality N]` now encodes the animation's frames as `VP8 ` instead of dropping the request with a warning and staying lossless.

## [0.1.0](https://github.com/P4suta/webpkit/releases/tag/webpkit-cli-v0.1.0) - 2026-07-14

### Fixed

- *(cli)* migrate PNG decode to png 0.18 reader API

### Other

- crates.io publication readiness (licenses, one-call API, hardening docs) ([#9](https://github.com/P4suta/webpkit/pull/9))
- sort Cargo.toml tables (cargo sort)
- fix en-US spellings flagged by typos 1.48
- initial public release
