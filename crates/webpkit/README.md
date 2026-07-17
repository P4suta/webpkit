# webpkit

A pure-Rust WebP codec — lossless (VP8L) and lossy (VP8), decode and encode, behind one API. This is the recommended entry point.

Part of the [webpkit](https://github.com/P4suta/webpkit) workspace.

One-call helpers cover the common case — raw RGBA8 in, WebP bytes out (and back):

```rust
use webpkit::{decode_rgba, encode_lossless_rgba, encode_lossy_rgba};

fn convert(width: u32, height: u32, rgba: &[u8]) -> webpkit::Result<()> {
    // Encode raw RGBA8 pixels — lossless (byte-exact) or lossy at a quality.
    let lossless = encode_lossless_rgba(width, height, rgba)?;
    let _lossy = encode_lossy_rgba(width, height, rgba, 90)?;

    // Decode any WebP (lossless or lossy) straight back to pixels.
    let (_dims, _pixels) = decode_rgba(&lossless)?;
    Ok(())
}
# convert(4, 4, &vec![0u8; 4 * 4 * 4]).unwrap();
```

For metadata preservation, effort tiers, non-RGBA layouts, or streaming, reach for
the [`decode`] function and the type-state [`Encoder`] builder:

```rust,no_run
use webpkit::{decode, Effort, Encoder};

fn reencode(bytes: &[u8]) -> webpkit::Result<()> {
    let img = decode(bytes)?; // -> Image (keeps ICC/Exif/XMP metadata)
    let _lossless = Encoder::lossless().effort(Effort::level(9)).encode(&img)?;
    let _lossy = Encoder::lossy().quality(90).encode(&img)?;
    Ok(())
}
# let webp = webpkit::encode_lossless_rgba(1, 1, &[0u8; 4]).unwrap();
# reencode(&webp).unwrap();
```

See [`examples/roundtrip.rs`](examples/roundtrip.rs) for a complete, runnable
encode → decode round-trip.

`decode` is **safe on untrusted input by default**: it caps the canvas at
`DEFAULT_MAX_PIXELS` before allocating, so a hostile header cannot exhaust memory.
Use `decode_with` + `DecodeOptions::max_pixels` for a different cap, or
`DecodeOptions::unbounded` to lift it for trusted input.

## Optional features

Off by default, so a plain dependency stays zero-dependency:

- `rayon` — encoder data-parallelism (byte-identical output, only faster).
- `image` — `TryFrom` conversions between `image::DynamicImage`/`RgbaImage` and the
  codec's `Image`.

[`decode`]: https://docs.rs/webpkit/latest/webpkit/fn.decode.html
[`Encoder`]: https://docs.rs/webpkit/latest/webpkit/struct.Encoder.html

## License

Dual-licensed under MIT OR Apache-2.0.
