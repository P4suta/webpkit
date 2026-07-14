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
```

For metadata preservation, effort tiers, non-RGBA layouts, or streaming, reach for
the [`decode`] function and the type-state [`Encoder`] builder:

```rust
use webpkit::{decode, Effort, Encoder};

fn reencode(bytes: &[u8]) -> webpkit::Result<()> {
    let img = decode(bytes)?; // -> Image (keeps ICC/Exif/XMP metadata)
    let _lossless = Encoder::lossless().effort(Effort::Best).encode(&img)?;
    let _lossy = Encoder::lossy().quality(90).encode(&img)?;
    Ok(())
}
```

See [`examples/roundtrip.rs`](examples/roundtrip.rs) for a complete, runnable
encode → decode round-trip.

[`decode`]: https://docs.rs/webpkit/latest/webpkit/fn.decode.html
[`Encoder`]: https://docs.rs/webpkit/latest/webpkit/struct.Encoder.html

## License

Dual-licensed under MIT OR Apache-2.0.
