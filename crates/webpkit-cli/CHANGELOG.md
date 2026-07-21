# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/P4suta/webpkit/compare/webpkit-cli-v0.1.0...webpkit-cli-v0.2.0) - 2026-07-21

### Added

- [**breaking**] consume pre-publish follow-ups #32/#33/#34 — lossy RD default tune + fidelity refinements + mutation harness ([#36](https://github.com/P4suta/webpkit/pull/36))
- [**breaking**] full-surpass campaign P1-P9 — auto Effort, lossy RD, bit-exact resize, animation ([#35](https://github.com/P4suta/webpkit/pull/35))
- [**breaking**] mux, YUV/BMP/TIFF output, near-lossless, API cleanup, GIF fidelity ([#20](https://github.com/P4suta/webpkit/pull/20))
- [**breaking**] pre-publish remediation - internals gate, lossy-anim, config wiring, facade chunk API ([#19](https://github.com/P4suta/webpkit/pull/19))
- [**breaking**] non_exhaustive Metadata/FrameMeta, modernize allow→expect, gate the CLI ([#18](https://github.com/P4suta/webpkit/pull/18))
- *(cli)* [**breaking**] ground-up CLI overhaul — color, completions, config, diagnostics, formats, safe I/O, preprocessing ([#16](https://github.com/P4suta/webpkit/pull/16))

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
