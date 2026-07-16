//! A minimal round-trip through the unified `webpkit` API: build an image, encode
//! it with each codec via the type-state [`Encoder`], and decode both back through
//! the one container-dispatching [`decode`].
//!
//! Run with:
//!
//! ```text
//! cargo run -p webpkit --example roundtrip
//! ```
#![expect(
    clippy::print_stdout,
    reason = "a runnable example reports its round-trip results to stdout"
)]

use webpkit::{Dimensions, Effort, Encoder, ImageRef, PixelLayout, Result, decode};

fn main() -> Result<()> {
    // A tiny 4x4 RGBA gradient to encode.
    let (w, h) = (4u32, 4u32);
    let mut rgba = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let r = u8::try_from(x * 60).unwrap_or(255);
            let g = u8::try_from(y * 60).unwrap_or(255);
            rgba.extend_from_slice(&[r, g, 128, 255]);
        }
    }
    let dims = Dimensions::new(w, h)?;
    let img = ImageRef::new(dims, PixelLayout::Rgba8, &rgba)?;

    // Lossless (VP8L): a byte-exact round-trip at the highest effort.
    let lossless = Encoder::lossless().effort(Effort::Best).encode_ref(img)?;
    let back = decode(&lossless)?;
    assert_eq!(back.as_bytes(), &rgba[..], "lossless must be byte-exact");
    println!("lossless: {} bytes, decoded byte-exact", lossless.len());

    // Lossy (VP8): smaller, close but not identical — only shape/opacity are pinned.
    let lossy = Encoder::lossy().quality(90).encode_ref(img)?;
    let back = decode(&lossy)?;
    assert_eq!(back.dimensions(), dims, "lossy must preserve dimensions");
    println!("lossy(q90): {} bytes, decoded {w}x{h}", lossy.len());

    // `decode` dispatched each file to the right codec purely from its container.
    Ok(())
}
