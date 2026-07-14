//! Committed decode conformance fixtures for the `webpkit-lossy` VP8 decoder.
//!
//! Each `fixtures/decode/<case>/` holds `input.vp8` (the raw `VP8 ` chunk
//! payload), `expected.rgba` (the golden RGBA produced by libwebp's
//! `WebPDecodeRGBA`, never hand-edited), and a `meta.toml` manifest.
//! `webpkit::lossy::decode` must reproduce the golden byte-for-byte. New cases are
//! auto-discovered — drop a directory in and it runs.
//!
//! The fixtures are supplied by the integrator (regenerated from the webpkit-lossy
//! `oracle` harness). Until then the directory is absent and this test skips
//! with a note rather than failing; once fixtures exist it refuses to pass
//! vacuously (`checked > 0`).

use std::path::Path;

#[test]
fn decode_fixtures_match_golden() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/decode");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        eprintln!(
            "skipping: no decode fixtures directory at {} (the integrator supplies fixtures)",
            dir.display()
        );
        return;
    };

    let mut checked = 0u32;
    for entry in entries {
        let case = entry.expect("read fixtures/decode entry").path();
        let input = case.join("input.vp8");
        let golden = case.join("expected.rgba");
        if !input.exists() || !golden.exists() {
            continue;
        }

        let payload = std::fs::read(&input).expect("read input.vp8");
        let expected = std::fs::read(&golden).expect("read expected.rgba");
        let image = webpkit::lossy::decode(&payload)
            .unwrap_or_else(|e| panic!("{}: webpkit::lossy decode failed: {e:?}", case.display()));
        let actual = image.as_bytes();

        assert_eq!(
            actual.len(),
            expected.len(),
            "{}: {}x{} byte-length mismatch vs libwebp golden",
            case.display(),
            image.width(),
            image.height()
        );
        assert_eq!(
            actual,
            expected.as_slice(),
            "{}: pixels differ from the libwebp golden",
            case.display()
        );
        checked += 1;
    }

    assert!(
        checked > 0,
        "no decode fixtures found under {} (each case needs input.vp8 + expected.rgba)",
        dir.display()
    );
}
