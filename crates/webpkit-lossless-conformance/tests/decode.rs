//! Committed decode conformance fixtures.
//!
//! Each `fixtures/decode/<case>/` holds `input.webp` and `expected.rgba` (the
//! golden RGBA produced by libwebp `dwebp`, never hand-edited). `webpkit::lossless::decode`
//! must reproduce the golden byte-for-byte. New cases are auto-discovered — drop
//! a directory in and it runs.

use std::path::Path;

#[test]
fn decode_fixtures_match_golden() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/decode");
    let entries = std::fs::read_dir(&dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));

    let mut checked = 0;
    for entry in entries {
        let case = entry.unwrap().path();
        let input = case.join("input.webp");
        let golden = case.join("expected.rgba");
        if !input.exists() || !golden.exists() {
            continue;
        }

        let webp = std::fs::read(&input).unwrap();
        let (dims, rgba) = webpkit::lossless::decode_rgba(&webp)
            .unwrap_or_else(|e| panic!("{}: webpkit::lossless decode failed: {e}", case.display()));
        let expected = std::fs::read(&golden).unwrap();

        assert_eq!(
            rgba.len(),
            expected.len(),
            "{}: {}x{} size mismatch vs golden",
            case.display(),
            dims.width(),
            dims.height()
        );
        assert_eq!(
            rgba,
            expected,
            "{}: pixels differ from dwebp golden",
            case.display()
        );
        checked += 1;
    }
    assert!(
        checked > 0,
        "no decode fixtures found under {}",
        dir.display()
    );
}
