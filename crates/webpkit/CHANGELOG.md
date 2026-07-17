# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- near-lossless lossless preprocessing: `Encoder::lossless().near_lossless(level)` and `EncoderConfig::with_near_lossless(level)` (`0..=100`, lower = stronger; `100` is a no-op). A pure ARGB filter that snaps the low bits of pixels in busy regions to a coarser grid, trading a bounded per-channel error for a smaller VP8L payload while keeping the bitstream exact.
- `read_metadata` / `write_metadata` — the `webpmux` half: read a file's ICC/Exif/XMP sidecar chunks, or set/replace/strip them, without decoding or re-encoding the image bitstream. Stills and animations alike; the `VP8 `/`VP8L`/`ALPH`/`ANIM`/`ANMF` chunks are copied through byte-for-byte.
- `decode_yuv` and the `YuvImage` type — decode a lossy still straight to its native YUV 4:2:0 planes (Y at full resolution, U/V subsampled). Lossless and animated inputs return `Error::UnsupportedFeature`.
- `Error::InvalidFrame` variant, distinguishing an animation-frame placement failure (off-canvas, odd offset, or over-long duration) from the general `InvalidDimensions`.
- `impl Display for Codec`, `Hash` for `Dimensions`, and `impl TryFrom<(u32, u32)> for Dimensions`.

### Changed

- [**breaking**] `AnimationEncoder` builder setters drop the `with_` prefix, matching `Encoder`: `with_loop_count`/`with_background`/`with_effort`/`with_metadata` are now `loop_count`/`background`/`effort`/`metadata`.
- [**breaking**] `AnimationEncoder::lossless()` / `lossy(q)` are replaced by a single `codec(AnimCodec)` setter, aligning with the `add_frame_with` per-frame codec argument.
- [**breaking**] The custom frame-decoder seam (`FrameDecoder`, `FramePayload`, `DecodedFrame`, `WebpFrameDecoder`, `decode_frames_with_decoder`) is now internal; `decode_frames` / `decode_frames_with` and the `Frames` / `CompositedFrames` aliases are the public animation surface.
- [**breaking**] The type-state markers `Empty` / `HasFrames` / `Lossless` / `Lossy` move from the crate root to `webpkit::encoder`, freeing the root namespace (no more `webpkit::Lossless` vs `webpkit::Codec::Lossless` clash).
- [**breaking**] `PixelLayout` and `RowDrain` are now `#[non_exhaustive]`.

## [0.1.0](https://github.com/P4suta/webpkit/releases/tag/webpkit-v0.1.0) - 2026-07-14

### Added

- [**breaking**] pre-publish hardening — safe-default decode, image interop, corpus-replay net ([#11](https://github.com/P4suta/webpkit/pull/11))

### Other

- crates.io publication readiness (licenses, one-call API, hardening docs) ([#9](https://github.com/P4suta/webpkit/pull/9))
- sort Cargo.toml tables (cargo sort)
- fix en-US spellings flagged by typos 1.48
- initial public release
