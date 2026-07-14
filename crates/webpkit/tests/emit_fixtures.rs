//! Opt-in emitter (`#[ignore]`d) for hand-crafted VP8L conformance inputs.
//!
//! Regenerates the committed `input.webp` files under the conformance crate so a
//! reviewer can reproduce them; `dwebp` then produces the golden `expected.rgba`.
//! These crafted streams use 1-symbol Huffman codes so the pixel data is empty,
//! exercising the container + header + Huffman + main-loop + RGBA path with a
//! tiny, self-contained input.
//!
//! ```text
//! WEBPKIT_EMIT=crates/webpkit-lossless-conformance/fixtures/decode/solid_rgba_8x8/input.webp \
//!   cargo test -p webpkit --test emit_fixtures -- --ignored --exact emit_solid_fixture
//! ```
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "opt-in fixture emitter (ignored test tooling); panicking on misuse is acceptable"
)]

/// Minimal LSB-first bit writer.
#[derive(Default)]
struct BitBuf {
    bytes: Vec<u8>,
    acc: u32,
    n: u32,
}
impl BitBuf {
    fn put(&mut self, value: u32, bits: u32) {
        self.acc |= value << self.n;
        self.n += bits;
        while self.n >= 8 {
            self.bytes.push((self.acc & 0xff) as u8);
            self.acc >>= 8;
            self.n -= 8;
        }
    }
    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.bytes.push((self.acc & 0xff) as u8);
        }
        self.bytes
    }
}

fn put_simple_code(b: &mut BitBuf, symbol: u32) {
    b.put(1, 1);
    b.put(0, 1);
    if symbol <= 1 {
        b.put(0, 1);
        b.put(symbol, 1);
    } else {
        b.put(1, 1);
        b.put(symbol, 8);
    }
}

/// A solid-color VP8L payload (`0x2f`-prefixed bitstream).
fn solid_payload(width: u32, height: u32, r: u32, g: u32, b: u32, a: u32) -> Vec<u8> {
    let mut buf = BitBuf::default();
    buf.put(0x2f, 8);
    buf.put(width - 1, 14);
    buf.put(height - 1, 14);
    buf.put(u32::from(a != 255), 1); // alpha_is_used
    buf.put(0, 3); // version
    buf.put(0, 1); // no transform
    buf.put(0, 1); // no color cache
    buf.put(0, 1); // no meta-huffman
    put_simple_code(&mut buf, g);
    put_simple_code(&mut buf, r);
    put_simple_code(&mut buf, b);
    put_simple_code(&mut buf, a);
    put_simple_code(&mut buf, 0);
    buf.finish()
}

/// Wrap a VP8L payload in a `RIFF....WEBP` + `VP8L` chunk envelope.
fn riff_wrap(payload: &[u8]) -> Vec<u8> {
    let mut chunk = Vec::new();
    chunk.extend_from_slice(b"VP8L");
    chunk.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_le_bytes());
    chunk.extend_from_slice(payload);
    if payload.len() % 2 == 1 {
        chunk.push(0);
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&u32::try_from(4 + chunk.len()).unwrap().to_le_bytes());
    out.extend_from_slice(b"WEBP");
    out.extend_from_slice(&chunk);
    out
}

#[test]
#[ignore = "opt-in: set WEBPKIT_EMIT to the output input.webp path"]
fn emit_solid_fixture() {
    let out = std::env::var("WEBPKIT_EMIT").expect("WEBPKIT_EMIT must be set");
    let webp = riff_wrap(&solid_payload(8, 8, 10, 20, 30, 255));
    // Sanity: our own decoder round-trips the crafted stream before we commit it.
    let (dims, _rgba) = webpkit::lossless::decode_rgba(&webp)
        .expect("webpkit::lossless decodes the crafted stream");
    assert_eq!((dims.width(), dims.height()), (8, 8));
    std::fs::write(&out, &webp).expect("write input.webp");
}
