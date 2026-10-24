# Webpkit guide

webpkit is a pure-Rust library and set of command-line tools for WebP encoding and decoding.

The project is pre-1.0 and remains under active development.

## Status

The library reads and writes lossless VP8L and baseline lossy VP8 images.
It also reads and writes WebP animation through frame-based APIs.
The core forbids unsafe Rust and has no required runtime dependencies.
Encoder tuning and broader real-image benchmarking remain in progress.

## Install from current source

Current `main` requires Rust 1.88 or newer.

```sh
git clone https://github.com/P4suta/webpkit.git
cd webpkit
cargo install --path crates/webpkit-cli --locked
```

This builds and installs the `webp`, `cwebp`, and `dwebp` commands from the current checkout.
The published v0.1.0 source tag needs Rust 1.96, while the current-main install needs Rust 1.88.

## Library example

The following example uses the one-call pixel helpers documented by the `webpkit` crate.

```rust
use webpkit::{decode_rgba, encode_lossless_rgba};

fn convert(width: u32, height: u32, rgba: &[u8]) -> webpkit::Result<()> {
    let encoded = encode_lossless_rgba(width, height, rgba)?;
    let (_dimensions, _pixels) = decode_rgba(&encoded)?;
    Ok(())
}
```

`decode_rgba` accepts lossless or lossy still WebP and returns image dimensions with four-channel pixel data.
For advanced layouts, streaming, effort settings, and metadata, use the `decode` API and `Encoder` builder.

## Command-line examples

```sh
webp photo.png
webp photo.webp
cwebp photo.png -o photo-lossy.webp -q 80
cwebp photo.png -o photo-lossless.webp -lossless
dwebp photo-lossy.webp -o decoded.png
webp info photo-lossy.webp
webp meta show photo-lossy.webp
```

`webp photo.png` writes a lossless `photo.webp` by default.
`webp photo.webp` decodes to Portable Network Graphics (PNG) and refuses to overwrite an existing output unless forced.
`cwebp` encodes lossy VP8 by default, while `-lossless` selects VP8L.
`dwebp` decodes either still-image codec.
The default `formats` feature adds more input formats for common photo and animation files.
The CLI can convert `.gif` animations to animated WebP files and write selected frames.
The `cwebp` and `dwebp` names may collide with libwebp tools on `PATH`.

## Codec and safety limits

Lossy VP8 support covers baseline still-image key frames.
WebP still-image inter frames are outside the codec's scope.
The default `decode` path caps the image canvas at `DEFAULT_MAX_PIXELS` before allocating pixel storage.
Use `decode_with` and `DecodeOptions::max_pixels` to choose a different limit for untrusted input.
Use `DecodeOptions::unbounded` only for input you trust.
`webp` keeps color profiles and photo metadata by default during supported still-image operations.
Animated `.gif` output doesn't carry metadata because the animation encoder doesn't model it.
CLI crop dimensions match libwebp, but webpkit's own resampler may produce different pixels when resizing.
CLI tools report an error for unsupported preprocessing options rather than ignore them.

## Further documentation

- [Library crate guide](../crates/webpkit/README.md)
- [Command-line crate guide](../crates/webpkit-cli/README.md)
- [Contribution guide](../CONTRIBUTING.md)
- [Benchmark details](benchmarking.md)
- [Library API on docs.rs](https://docs.rs/webpkit)
- [Full historical README at commit b4c6319](https://github.com/P4suta/webpkit/blob/b4c631948a6a66c549c37e4583a2887d528f1489/README.md)

## License

Users may choose the Massachusetts Institute of Technology (MIT) License or Apache License 2.0.

See the [MIT License](../LICENSE-MIT) and [Apache License 2.0](../LICENSE-APACHE).

Copyright 2026 webpkit contributors
