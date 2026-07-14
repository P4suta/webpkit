//! Committed lossy conformance fixtures.
//!
//! Each case decodes a real libwebp-authored `VP8 ` payload and asserts the
//! result is byte-for-byte identical to the committed `WebPDecodeRGBA` golden.
//! Unlike the `oracle` differential these run in the **default** build (no
//! libwebp linked), so the whole decode pipeline — intra-4×4 and 16×16
//! reconstruction, the loop filter, chroma, and YUV→RGB — is exercised and
//! covered by the standard test suite over real, varied streams.
//!
//! Regenerate the fixtures with:
//! `cargo test -p webpkit --features oracle --test oracle_lossy -- --ignored gen_fixtures`.

/// Decode `fixtures/<name>.vp8` and assert it equals `fixtures/<name>.rgba`.
macro_rules! fixture_case {
    ($test:ident, $name:literal) => {
        #[test]
        fn $test() {
            let payload = include_bytes!(concat!("fixtures/", $name, ".vp8"));
            let golden = include_bytes!(concat!("fixtures/", $name, ".rgba"));
            let image = webpkit::lossy::decode(payload).expect(concat!($name, ": decode failed"));
            assert_eq!(
                image.as_bytes(),
                &golden[..],
                concat!($name, ": RGBA differs from the committed libwebp golden")
            );
        }
    };
}

// A low-quality photographic-noise frame (intra-4×4 macroblocks + normal filter).
fixture_case!(noise_32x24_q30, "noise_32x24_q30");
// A sharp 4×4 checkerboard (strong deblocking, vertical/horizontal predictors).
fixture_case!(checker_16x16_q20, "checker_16x16_q20");
// A smooth gradient at a non-macroblock-aligned odd size (16×16 predictors).
fixture_case!(gradient_17x13_q80, "gradient_17x13_q80");
// A tiny odd frame (chroma edges, single partial macroblock).
fixture_case!(noise_5x9_q50, "noise_5x9_q50");

/// Wrap `fixtures/<name>.vp8` in a RIFF/WEBP container, stream it through the
/// public [`webpkit::lossy::IncrementalDecoder`] at several chunk granularities, and
/// assert the assembled image equals the same golden — proving the row-streaming
/// path is byte-identical to the one-shot decode across arbitrary push splits, in
/// the default (libwebp-free) build.
macro_rules! streamed_case {
    ($test:ident, $name:literal) => {
        #[test]
        fn $test() {
            use webpkit::container::fourcc::FourCc;
            use webpkit::container::writer::{push_chunk, riff_envelope};

            let payload = include_bytes!(concat!("fixtures/", $name, ".vp8"));
            let golden = include_bytes!(concat!("fixtures/", $name, ".rgba"));
            let mut body = Vec::new();
            push_chunk(&mut body, FourCc::VP8, payload);
            let file = riff_envelope(&body);

            for chunk in [1usize, 5, file.len().max(1)] {
                let mut dec = webpkit::lossy::IncrementalDecoder::new();
                for slice in file.chunks(chunk) {
                    dec.push(slice).expect(concat!($name, ": push failed"));
                }
                let image = dec
                    .into_image()
                    .expect(concat!($name, ": into_image failed"));
                assert_eq!(
                    image.as_bytes(),
                    &golden[..],
                    concat!($name, ": streamed RGBA differs from the committed golden")
                );
            }
        }
    };
}

streamed_case!(stream_noise_32x24_q30, "noise_32x24_q30");
streamed_case!(stream_checker_16x16_q20, "checker_16x16_q20");
streamed_case!(stream_gradient_17x13_q80, "gradient_17x13_q80");
streamed_case!(stream_noise_5x9_q50, "noise_5x9_q50");
