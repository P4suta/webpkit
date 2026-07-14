//! Committed decode conformance fixtures for the `webp` lossy paths: the still
//! `ALPH` (transparent-lossy) path and the animated-lossy path.
//!
//! Each `fixtures/alpha/<case>/` holds `input.webp` (a full `VP8 ` + `ALPH`
//! container file), `expected.rgba` (the golden RGBA produced by libwebp's
//! `WebPDecodeRGBA`, never hand-edited), and a `meta.toml` manifest.
//! `webpkit::decode` must reproduce the golden byte-for-byte. Each
//! `fixtures/anim/<case>/` holds `input.webp` (an animated-lossy `VP8X` file),
//! `frames.rgba` (the per-frame composited RGBA from libwebp's `WebPAnimDecoder`,
//! concatenated in frame order), and a `meta.toml`;
//! `webpkit::decode_frames(...).composited()` must reproduce it. New cases are
//! auto-discovered — drop a directory in and it runs.
//!
//! The fixtures are supplied by the integrator (regenerated from the
//! `#[ignore]` + `oracle`-gated generators in `tests/ledger.rs`, which link
//! libwebp). Until they exist the directory is absent and these tests skip with a
//! note rather than failing; once fixtures exist each refuses to pass vacuously
//! (`checked > 0`).

use std::path::Path;

#[test]
fn decode_fixtures_match_golden() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/alpha");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        eprintln!(
            "skipping: no alpha fixtures directory at {} (the integrator supplies fixtures)",
            dir.display()
        );
        return;
    };

    let mut checked = 0u32;
    for entry in entries {
        let case = entry.expect("read fixtures/alpha entry").path();
        let input = case.join("input.webp");
        let golden = case.join("expected.rgba");
        if !input.exists() || !golden.exists() {
            continue;
        }

        let payload = std::fs::read(&input).expect("read input.webp");
        let expected = std::fs::read(&golden).expect("read expected.rgba");
        let image = webpkit::decode(&payload)
            .unwrap_or_else(|e| panic!("{}: webp decode failed: {e:?}", case.display()));
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
        "no alpha fixtures found under {} (each case needs input.webp + expected.rgba)",
        dir.display()
    );
}

#[test]
fn decode_anim_fixtures_match_golden() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/anim");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        eprintln!(
            "skipping: no anim fixtures directory at {} (the integrator supplies fixtures)",
            dir.display()
        );
        return;
    };

    let mut checked = 0u32;
    for entry in entries {
        let case = entry.expect("read fixtures/anim entry").path();
        let input = case.join("input.webp");
        let golden = case.join("frames.rgba");
        if !input.exists() || !golden.exists() {
            continue;
        }

        let payload = std::fs::read(&input).expect("read input.webp");
        let expected = std::fs::read(&golden).expect("read frames.rgba");

        // Decode every frame and composite it onto the canvas, concatenating the
        // canvas-sized RGBA in frame order — the exact shape of `frames.rgba`.
        let frames = webpkit::decode_frames(&payload)
            .unwrap_or_else(|e| panic!("{}: webp decode_frames failed: {e:?}", case.display()));
        let canvas = frames.anim_info().canvas;
        let mut actual = Vec::with_capacity(expected.len());
        let mut frame_count = 0u32;
        for frame in frames.composited() {
            let frame = frame
                .unwrap_or_else(|e| panic!("{}: frame composite failed: {e:?}", case.display()));
            actual.extend_from_slice(frame.image().as_bytes());
            frame_count += 1;
        }

        assert_eq!(
            actual.len(),
            expected.len(),
            "{}: {}x{} concatenated byte-length mismatch vs libwebp golden ({frame_count} frames)",
            case.display(),
            canvas.width(),
            canvas.height()
        );
        assert_eq!(
            actual,
            expected,
            "{}: composited frames differ from the libwebp golden",
            case.display()
        );
        checked += 1;
    }

    assert!(
        checked > 0,
        "no anim fixtures found under {} (each case needs input.webp + frames.rgba)",
        dir.display()
    );
}
