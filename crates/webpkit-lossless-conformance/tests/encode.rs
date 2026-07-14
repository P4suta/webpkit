//! Committed encode conformance fixtures (self round-trip, tool-free).
//!
//! Each `fixtures/encode/<case>/` holds `input.rgba` (the raw, row-major RGBA
//! source, `R,G,B,A` per pixel) and a `meta.toml` recording its dimensions.
//! `webpkit::lossless::encode` must produce a valid VP8L file that `webpkit::lossless::decode` restores
//! byte-for-byte back to the original `(width, height, rgba)`. New cases are
//! auto-discovered — drop a directory in and it runs.
//!
//! This gate is deliberately tool-free: the *independent* libwebp `dwebp`
//! cross-check runs once, at fixture-generation time (`cargo xtask
//! gen-fixtures`), so encode goldens are not committed (they are version
//! dependent) and this test stays reproducible everywhere.

use std::path::Path;

use webpkit_lossless_conformance::load_meta;

#[test]
fn encode_fixtures_round_trip() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/encode");
    // The encode fixtures are optional: skip cleanly when the directory is absent.
    if !dir.exists() {
        return;
    }
    let entries = std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));

    let mut checked = 0;
    for entry in entries {
        let case = entry.unwrap().path();
        let input = case.join("input.rgba");
        let meta_path = case.join("meta.toml");
        if !input.exists() || !meta_path.exists() {
            continue;
        }

        let meta = load_meta(&meta_path)
            .unwrap_or_else(|e| panic!("{}: reading meta: {e}", case.display()));
        let (width, height) = (meta.width, meta.height);
        assert!(
            width > 0 && height > 0,
            "{}: meta.toml must record positive dimensions (got {width}x{height})",
            case.display()
        );

        let rgba = std::fs::read(&input).unwrap();
        let expected_len = usize::try_from(width).unwrap() * usize::try_from(height).unwrap() * 4;
        assert_eq!(
            rgba.len(),
            expected_len,
            "{}: input.rgba is {} bytes but {width}x{height}*4 = {expected_len}",
            case.display(),
            rgba.len()
        );

        let dims = webpkit::lossless::Dimensions::new(width, height)
            .unwrap_or_else(|e| panic!("{}: bad dimensions: {e}", case.display()));
        let image =
            webpkit::lossless::ImageRef::new(dims, webpkit::lossless::PixelLayout::Rgba8, &rgba)
                .unwrap_or_else(|e| panic!("{}: bad image: {e}", case.display()));
        let webp = webpkit::lossless::encode(image, &webpkit::lossless::EncoderConfig::default())
            .unwrap_or_else(|e| panic!("{}: webpkit::lossless encode failed: {e}", case.display()));
        let decoded = webpkit::lossless::decode_rgba(&webp)
            .unwrap_or_else(|e| panic!("{}: webpkit::lossless decode failed: {e}", case.display()));
        assert_eq!(
            decoded,
            (dims, rgba),
            "{}: encode->decode did not round-trip",
            case.display()
        );
        checked += 1;
    }
    assert!(
        checked > 0,
        "no encode fixtures found under {}",
        dir.display()
    );
}
