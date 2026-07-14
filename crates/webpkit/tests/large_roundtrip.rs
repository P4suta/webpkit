//! Heavy, `#[ignore]`d round-trip tests on realistic large (1024x1024) images.
//!
//! The proptest sweeps top out at 64px, so nothing there crosses the multi-row
//! streaming boundaries or the transform tile grid at scale. These tests encode
//! three structured megapixel archetypes (a smooth gradient, an 8x8 repeat, and a
//! 12-color scatter) and assert an exact `decode(encode(img)) == img` round-trip,
//! plus a small-chunk incremental decode that advances `packed_base` over a
//! thousand-plus rows. Run with `--ignored` (they are slow by design).
#![forbid(unsafe_code)]

use webpkit::lossless::{
    Dimensions, Effort, EncoderConfig, ImageRef, IncrementalDecoder, PixelLayout, Progress,
    decode_rgba,
};

/// Square edge for the heavy images. Deliberately > 1024 rows worth of pixels so
/// the incremental decoder finalizes rows in many bursts.
const EDGE: u32 = 1024;

/// Low 8 bits of `v`.
const fn lo(v: u32) -> u8 {
    (v & 0xff) as u8
}

/// Two-axis linear ramp: a predictor-friendly smooth gradient.
fn gradient(edge: u32) -> Vec<u8> {
    let mut rgba = Vec::with_capacity((edge * edge * 4) as usize);
    for y in 0..edge {
        for x in 0..edge {
            rgba.extend_from_slice(&[lo(4 * x), lo(4 * y), lo(2 * (x + y)), 255]);
        }
    }
    rgba
}

/// An 8x8 repeating tile over six colors — long-range LZ77 / cache bait.
fn tiled(edge: u32) -> Vec<u8> {
    const COLORS: [[u8; 4]; 6] = [
        [0x00, 0x00, 0x00, 255],
        [0xff, 0x00, 0x00, 255],
        [0x00, 0xff, 0x00, 255],
        [0x00, 0x00, 0xff, 255],
        [0xff, 0xff, 0x00, 255],
        [0xff, 0x00, 0xff, 255],
    ];
    let mut rgba = Vec::with_capacity((edge * edge * 4) as usize);
    for y in 0..edge {
        for x in 0..edge {
            let band = ((x / 8 + y / 8) % 6) as usize;
            rgba.extend_from_slice(&COLORS[band]);
        }
    }
    rgba
}

/// A 12-color scatter driven by a deterministic LCG — palette-transform bait
/// with no spatial structure the predictor can exploit.
fn scatter(edge: u32) -> Vec<u8> {
    const COLORS: [[u8; 4]; 12] = [
        [0x10, 0x20, 0x30, 255],
        [0x20, 0x40, 0x30, 255],
        [0x70, 0xa0, 0xc0, 255],
        [0xa0, 0x30, 0x60, 255],
        [0x30, 0xc0, 0x90, 255],
        [0xf0, 0xf0, 0x10, 255],
        [0x01, 0x02, 0x03, 255],
        [0xfe, 0xdc, 0xba, 255],
        [0x44, 0x55, 0x66, 255],
        [0x99, 0x88, 0x77, 255],
        [0xc0, 0x60, 0x30, 255],
        [0x7f, 0x7f, 0x7f, 255],
    ];
    let mut state: u64 = 0x5AFE_5EED;
    let count = (edge * edge) as usize;
    let mut rgba = Vec::with_capacity(count * 4);
    for _ in 0..count {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let idx = ((state >> 40) % 12) as usize;
        rgba.extend_from_slice(&COLORS[idx]);
    }
    rgba
}

/// Encode raw RGBA with an explicit method. Fallible via `?` so the `expect`
/// stays inside the `#[test]` fns — the boundary clippy allows it in.
fn encode(edge: u32, rgba: &[u8], method: Effort) -> webpkit::lossless::Result<Vec<u8>> {
    let dims = Dimensions::new(edge, edge)?;
    let image = ImageRef::new(dims, PixelLayout::Rgba8, rgba)?;
    let config = EncoderConfig::default().with_effort(method);
    webpkit::lossless::encode(image, &config)
}

/// `decode(encode(img)) == img`, byte-for-byte, at megapixel scale.
#[test]
#[ignore = "heavy: large-image roundtrip"]
fn large_images_round_trip_exactly() {
    let dims = Dimensions::new(EDGE, EDGE).unwrap();
    for (name, rgba) in [
        ("gradient", gradient(EDGE)),
        ("tiled", tiled(EDGE)),
        ("scatter", scatter(EDGE)),
    ] {
        let webp =
            encode(EDGE, &rgba, Effort::Balanced).expect("encode is infallible for valid input");
        let decoded = decode_rgba(&webp).expect("must decode our own output");
        assert_eq!(decoded.0, dims, "{name}: dimensions changed");
        assert!(decoded.1 == rgba, "{name}: round-trip was not lossless");
    }
}

/// Pushing a large encoded image in 4 KiB chunks finalizes rows in many bursts;
/// the drained rows reassemble to the source (exercising `packed_base` advancing
/// across the whole image), and `into_image` still returns the complete image.
#[test]
#[ignore = "heavy: large-image streaming roundtrip"]
fn large_image_streams_row_by_row() {
    let rgba = gradient(EDGE);
    let webp = encode(EDGE, &rgba, Effort::Balanced).expect("encode is infallible for valid input");

    let mut decoder = IncrementalDecoder::new();
    let mut drained: Vec<u8> = Vec::with_capacity(rgba.len());
    let mut next_row = 0u32;
    let mut finished = false;
    let mut bursts = 0u32;

    for chunk in webp.chunks(4096) {
        let progress = decoder
            .push(chunk)
            .expect("push must not fail on valid input");
        if let Some(rows) = decoder.drain_rows() {
            assert_eq!(rows.first_row, next_row, "drained rows are not contiguous");
            assert_eq!(rows.width, EDGE, "drained row width mismatch");
            next_row += rows.rows;
            drained.extend_from_slice(rows.as_bytes());
            bursts += 1;
        }
        if progress == Progress::Finished {
            finished = true;
        }
    }

    assert!(finished, "streaming never reached Finished");
    // The gradient decodes over many pushes, so rows arrive in more than one burst.
    assert!(bursts > 1, "expected multiple row bursts, got {bursts}");
    assert_eq!(next_row, EDGE, "not every row was drained");
    assert!(
        drained == rgba,
        "streamed rows differ from the source image"
    );

    // `into_image` reconstructs the full image even though drained rows were freed.
    let image = decoder.into_image().expect("into_image after Finished");
    assert_eq!(image.dimensions(), Dimensions::new(EDGE, EDGE).unwrap());
    assert!(
        image.as_bytes() == rgba,
        "into_image differs from the source"
    );
}
