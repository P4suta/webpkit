# webpkit-cli

The `cwebp` / `dwebp` / `webp` command-line tools — libwebp-compatible WebP
encoding and decoding, built on the pure-Rust [`webpkit`](https://crates.io/crates/webpkit)
codec. `cwebp` encodes images to WebP (lossy by default, `-lossless` for VP8L),
`dwebp` decodes WebP back to PNG, and `webp` is the unified brand tool.

This crate is part of the [webpkit](https://github.com/P4suta/webpkit) workspace.
If you want the codec itself — `decode`, `encode`, and the `Encoder` API — rather
than the command-line tools, depend on the [`webpkit`](https://crates.io/crates/webpkit)
umbrella crate, which is the recommended entry point for library use.

## License

Dual-licensed under MIT OR Apache-2.0.
