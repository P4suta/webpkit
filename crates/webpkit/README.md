# webpkit

A pure-Rust WebP codec — lossless (VP8L) and lossy (VP8), decode and encode, behind one API. This is the recommended entry point.

Part of the [webpkit](https://github.com/P4suta/webpkit) workspace.

```rust
use webpkit::{decode, Encoder, Effort};

// Decode any WebP (lossless or lossy) into an image.
let img = decode(bytes)?;

// Re-encode: lossless at the highest effort, or lossy at a quality.
let lossless = Encoder::lossless().effort(Effort::Best).encode(&img)?;
let lossy = Encoder::lossy().quality(90).encode(&img)?;
```

## License

Dual-licensed under MIT OR Apache-2.0.
